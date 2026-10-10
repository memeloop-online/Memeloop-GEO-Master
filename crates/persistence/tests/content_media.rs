//! Run only against a disposable PostgreSQL database via GEO_TEST_DATABASE_URL.
use geo_domain::{
    ContentMediaBindingState, ContentMediaRepository, ErrorCode, MediaObjectKey, TenantScope,
    VerifiedImage, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgContentMediaRepository};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    scope: TenantScope,
    image: VerifiedImage,
}

async fn fixture(pool: &PgPool, bytes: &[u8]) -> Fixture {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    let session = Uuid::new_v4();
    let object = Uuid::new_v4();
    let hash = sha256_hex(bytes);
    sqlx::query(
        "INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Media fixture')",
    )
    .bind(operator)
    .bind(format!("media-{operator}"))
    .execute(pool)
    .await
    .expect("operator");
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Media fixture')")
        .bind(tenant).bind(operator).bind(format!("media-{tenant}"))
        .execute(pool).await.expect("tenant");
    sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Media fixture')")
        .bind(project).bind(operator).bind(tenant).bind(format!("media-{project}"))
        .execute(pool).await.expect("project");
    sqlx::query(
        "INSERT INTO knowledge_upload_sessions
         (upload_session_id,operator_id,tenant_id,project_id,revision,filename,
          declared_media_type,expected_size,expected_sha256,purpose,state,expires_at,
          committed_object_id,staging_object_ref)
         VALUES ($1,$2,$3,$4,2,'image.bin','text/plain',$5,$6,'internal','committed',
                 now()+interval '1 day',$7,'agent-attachment')",
    )
    .bind(session)
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(bytes.len() as i64)
    .bind(&hash)
    .bind(object)
    .execute(pool)
    .await
    .expect("upload session");
    sqlx::query(
        "INSERT INTO knowledge_upload_blobs (upload_session_id,content,actual_size,sha256)
         VALUES ($1,$2,$3,$4)",
    )
    .bind(session)
    .bind(bytes)
    .bind(bytes.len() as i64)
    .bind(&hash)
    .execute(pool)
    .await
    .expect("blob");
    sqlx::query(
        "INSERT INTO knowledge_stored_objects
         (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,
          actual_size,detected_media_type,sha256,state)
         VALUES ($1,$2,$3,$4,1,'postgres_blob',$5,$6,'text/plain',$7,'committed')",
    )
    .bind(object)
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(format!("upload/{session}"))
    .bind(bytes.len() as i64)
    .bind(&hash)
    .execute(pool)
    .await
    .expect("stored object");
    Fixture {
        scope: TenantScope::new(operator.into(), tenant.into(), Some(project.into())),
        image: VerifiedImage {
            key: MediaObjectKey {
                object_id: object,
                object_version: 1,
                sha256: hash,
            },
            media_type: "image/png".into(),
            byte_len: bytes.len() as u64,
            width: 1,
            height: 1,
        },
    }
}

async fn database() -> (Database, DatabaseConfig) {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL");
    let config = DatabaseConfig::from_url(url).expect("valid database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrate");
    (database, config)
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn binding_replay_scope_reconnect_and_terminal_withdrawal() {
    let (database, config) = database().await;
    let bytes = b"\x89PNG\r\n\x1a\nfixture media bytes";
    let fixture = fixture(database.pool(), bytes).await;
    let repo = PgContentMediaRepository::from_database(&database);
    let original = repo
        .create_binding(&fixture.scope, fixture.image.clone())
        .await
        .expect("grant");
    assert_eq!(
        original,
        repo.create_binding(&fixture.scope, fixture.image.clone())
            .await
            .expect("replay")
    );
    assert_eq!(
        repo.list_bindings(&fixture.scope, None, 10)
            .await
            .expect("list"),
        vec![original.clone()]
    );
    assert_eq!(
        repo.list_bindings(&fixture.scope, None, 101)
            .await
            .expect("100-item API page plus cursor probe"),
        vec![original.clone()]
    );
    assert_eq!(
        repo.list_bindings(&fixture.scope, None, 102)
            .await
            .expect_err("page beyond internal cursor probe cap")
            .code,
        ErrorCode::InvalidRequest,
    );
    let other_scope = TenantScope::new(
        fixture.scope.operator_id,
        Uuid::new_v4().into(),
        fixture.scope.project_id,
    );
    assert_eq!(
        repo.get_binding(&other_scope, original.binding_id)
            .await
            .expect("foreign read"),
        None
    );
    assert!(repo.list_bindings(&other_scope, None, 10).await.is_err());
    assert!(
        repo.create_binding(&other_scope, fixture.image.clone())
            .await
            .is_err()
    );

    let second_connection = Database::connect_and_migrate(&config)
        .await
        .expect("reconnect");
    let reopened = PgContentMediaRepository::from_database(&second_connection);
    assert_eq!(
        reopened
            .get_binding(&fixture.scope, original.binding_id)
            .await
            .expect("reopen"),
        Some(original.clone())
    );
    let withdrawn = reopened
        .withdraw_binding(&fixture.scope, original.binding_id)
        .await
        .expect("withdraw");
    assert_eq!(withdrawn.state, ContentMediaBindingState::Withdrawn);
    assert!(withdrawn.withdrawn_at.is_some());
    assert_eq!(
        repo.withdraw_binding(&fixture.scope, original.binding_id)
            .await
            .expect("withdraw replay"),
        withdrawn
    );
    assert!(
        repo.list_bindings(&fixture.scope, None, 10)
            .await
            .expect("no active")
            .is_empty()
    );
    assert_eq!(
        repo.create_binding(&fixture.scope, fixture.image.clone())
            .await
            .expect_err("cannot resurrect")
            .code,
        ErrorCode::Conflict,
    );
    let direct_resurrection = sqlx::query(
        "UPDATE content_media_bindings SET state='active',withdrawn_at=NULL WHERE binding_id=$1",
    )
    .bind(original.binding_id)
    .execute(database.pool())
    .await;
    assert!(
        direct_resurrection.is_err(),
        "SQL cannot revive a withdrawn grant"
    );
    let direct_delete = sqlx::query("DELETE FROM content_media_bindings WHERE binding_id=$1")
        .bind(original.binding_id)
        .execute(database.pool())
        .await;
    assert!(
        direct_delete.is_err(),
        "SQL cannot erase withdrawal history"
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn corrupted_bytes_and_uncommitted_agent_tuple_are_rejected() {
    let (database, _) = database().await;
    let fixture = fixture(database.pool(), b"synthetic byte identity").await;
    let repo = PgContentMediaRepository::from_database(&database);
    let original = repo
        .create_binding(&fixture.scope, fixture.image.clone())
        .await
        .expect("initial grant");
    let object = fixture.image.key.object_id;
    let session: Uuid = sqlx::query_scalar(
        "SELECT upload_session_id FROM knowledge_upload_sessions WHERE committed_object_id=$1",
    )
    .bind(object)
    .fetch_one(database.pool())
    .await
    .expect("session");
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$1 WHERE upload_session_id=$2")
        .bind(b"corrupt".as_slice())
        .bind(session)
        .execute(database.pool())
        .await
        .expect("corrupt");
    assert_eq!(
        repo.list_bindings(&fixture.scope, None, 10)
            .await
            .expect_err("corruption cannot appear available")
            .code,
        ErrorCode::Conflict,
    );
    assert_eq!(
        repo.create_binding(&fixture.scope, fixture.image.clone())
            .await
            .expect_err("replay checks bytes")
            .code,
        ErrorCode::Conflict,
    );
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$1 WHERE upload_session_id=$2")
        .bind(b"synthetic byte identity".as_slice())
        .bind(session)
        .execute(database.pool())
        .await
        .expect("restore");
    sqlx::query("UPDATE knowledge_upload_sessions SET staging_object_ref='other' WHERE upload_session_id=$1")
        .bind(session).execute(database.pool()).await.expect("remove agent commitment");
    assert_eq!(
        repo.create_binding(&fixture.scope, fixture.image.clone())
            .await
            .expect_err("requires agent attachment")
            .code,
        ErrorCode::Conflict,
    );
    let forbidden = sqlx::query(
        "UPDATE content_media_bindings SET media_type='image/webp' WHERE binding_id=$1",
    )
    .bind(original.binding_id)
    .execute(database.pool())
    .await;
    assert!(forbidden.is_err(), "SQL cannot rewrite verified metadata");
}
