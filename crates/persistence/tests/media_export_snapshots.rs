//! Run only against a disposable PostgreSQL database via GEO_TEST_DATABASE_URL.
use geo_domain::{
    ContentMediaRepository, ErrorCode, MediaObjectKey, TenantScope, VerifiedImage, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgContentMediaRepository};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

async fn database() -> (Database, DatabaseConfig) {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL"),
    )
    .expect("valid database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    (database, config)
}

async fn scope(pool: &PgPool) -> TenantScope {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Snapshot test')",
    )
    .bind(operator)
    .bind(format!("snapshot-{operator}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name)
         VALUES ($1,$2,$3,'Snapshot test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("snapshot-{tenant}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name)
         VALUES ($1,$2,$3,$4,'Snapshot test')",
    )
    .bind(project)
    .bind(operator)
    .bind(tenant)
    .bind(format!("snapshot-{project}"))
    .execute(pool)
    .await
    .unwrap();
    TenantScope::new(operator.into(), tenant.into(), Some(project.into()))
}

async fn image(pool: &PgPool, scope: &TenantScope, bytes: &[u8]) -> (VerifiedImage, Uuid) {
    let session = Uuid::new_v4();
    let object = Uuid::new_v4();
    let hash = sha256_hex(bytes);
    sqlx::query(
        "INSERT INTO knowledge_upload_sessions
         (upload_session_id,operator_id,tenant_id,project_id,revision,filename,
          declared_media_type,expected_size,expected_sha256,purpose,state,expires_at,
          committed_object_id,staging_object_ref)
         VALUES ($1,$2,$3,$4,2,'synthetic.png','image/png',$5,$6,'internal','committed',
                 now()+interval '1 day',$7,'agent-attachment')",
    )
    .bind(session)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(bytes.len() as i64)
    .bind(&hash)
    .bind(object)
    .execute(pool)
    .await
    .unwrap();
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
    .unwrap();
    sqlx::query(
        "INSERT INTO knowledge_stored_objects
         (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,
          actual_size,detected_media_type,sha256,state)
         VALUES ($1,$2,$3,$4,1,'postgres_blob',$5,$6,'image/png',$7,'committed')",
    )
    .bind(object)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .bind(format!("upload/{session}"))
    .bind(bytes.len() as i64)
    .bind(&hash)
    .execute(pool)
    .await
    .unwrap();
    (
        VerifiedImage {
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
        session,
    )
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn batch_snapshot_reconnect_withdrawal_and_corruption() {
    let (database, config) = database().await;
    let scope = scope(database.pool()).await;
    let repo = PgContentMediaRepository::from_database(&database);
    let first_bytes = b"synthetic first media bytes";
    let second_bytes = b"synthetic second media bytes";
    let (first, session) = image(database.pool(), &scope, first_bytes).await;
    let (second, _) = image(database.pool(), &scope, second_bytes).await;
    let grant = repo.create_binding(&scope, first.clone()).await.unwrap();
    repo.create_binding(&scope, second.clone()).await.unwrap();
    let reopened_db = Database::connect_and_migrate(&config)
        .await
        .expect("reconnect");
    let reopened = PgContentMediaRepository::from_database(&reopened_db);
    let batch = reopened
        .snapshot_authorized_images(
            &scope,
            &[second.key.clone(), first.key.clone(), first.key.clone()],
        )
        .await
        .unwrap();
    assert_eq!(batch.len(), 2);
    assert!(
        batch
            .iter()
            .any(|item| item.bytes.as_slice() == first_bytes)
    );
    assert!(
        batch
            .iter()
            .any(|item| item.bytes.as_slice() == second_bytes)
    );
    let mut other_hash = first.key.clone();
    other_hash.sha256 = "a".repeat(64);
    assert_eq!(
        reopened
            .snapshot_authorized_images(&scope, &[first.key.clone(), other_hash])
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
    );
    let sibling = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        reopened
            .snapshot_authorized_images(&sibling, std::slice::from_ref(&first.key))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound,
    );
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$1 WHERE upload_session_id=$2")
        .bind(b"corrupt".as_slice())
        .bind(session)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        reopened
            .snapshot_authorized_images(&scope, std::slice::from_ref(&first.key))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
    );
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$1 WHERE upload_session_id=$2")
        .bind(first_bytes.as_slice())
        .bind(session)
        .execute(database.pool())
        .await
        .unwrap();
    repo.withdraw_binding(&scope, grant.binding_id)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .snapshot_authorized_images(&scope, &[second.key, first.key])
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
    );
    assert!(
        batch
            .iter()
            .any(|item| item.bytes.as_slice() == first_bytes)
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn snapshot_waits_for_project_lock_and_returns_after_transaction_commit() {
    let (database, _) = database().await;
    let scope = scope(database.pool()).await;
    let repo = PgContentMediaRepository::from_database(&database);
    let (image, _) = image(database.pool(), &scope, b"synthetic locked image").await;
    let grant = repo.create_binding(&scope, image.clone()).await.unwrap();
    let mut held = database.pool().begin().await.unwrap();
    sqlx::query("SELECT project_id FROM projects WHERE project_id=$1 FOR UPDATE")
        .bind(scope.project_id.unwrap().as_uuid())
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let contender = repo.clone();
    let scoped = scope.clone();
    let key = image.key.clone();
    let mut pending =
        tokio::spawn(async move { contender.snapshot_authorized_images(&scoped, &[key]).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut pending)
            .await
            .is_err()
    );
    held.commit().await.unwrap();
    let frozen = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(frozen[0].bytes.as_slice(), b"synthetic locked image");
    // Returning a snapshot must release the project/byte locks. Withdrawal
    // after the authorization point cannot alter already-owned bytes.
    tokio::time::timeout(
        Duration::from_secs(5),
        repo.withdraw_binding(&scope, grant.binding_id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(frozen[0].bytes.as_slice(), b"synthetic locked image");
}
