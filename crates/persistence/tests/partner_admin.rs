use chrono::{Duration, Utc};
use geo_domain::{
    AppError, ErrorCode, PartnerAdminIssuedKey, PartnerAdminRepository, PartnerRepository,
    TenantScope,
};
use geo_persistence::{
    Database, DatabaseConfig, PgPartnerAdminRepository, PgPartnerRepository, set_local_scope,
};
use sqlx::Row;
use uuid::Uuid;

async fn setup() -> (Database, PgPartnerAdminRepository, TenantScope) {
    let database = Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database"),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("partner-admin-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(operator)
    .bind(tenant)
    .bind(format!("partner-admin-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(operator_id,tenant_id,project_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
        .bind(operator).bind(tenant).bind(project).bind(format!("partner-admin-{project}"))
        .execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let repo = PgPartnerAdminRepository::from_database(&database);
    (database, repo, scope)
}

fn error_code<T>(result: Result<T, AppError>) -> ErrorCode {
    match result {
        Ok(_) => panic!("expected operation to fail"),
        Err(error) => error.code,
    }
}

async fn authenticated(
    repo: &PgPartnerRepository,
    connection: Uuid,
    issued: &PartnerAdminIssuedKey,
) -> bool {
    let (id, secret) = issued.bearer.split_once('.').unwrap();
    assert_eq!(id.parse::<Uuid>().unwrap(), issued.key.key_id);
    repo.authenticate(connection, issued.key.key_id, secret)
        .await
        .is_ok()
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn metadata_paginates_in_uuid_order_without_credentials() {
    let (database, repo, scope) = setup().await;
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.push(repo.create_connection(&scope).await.unwrap().connection_id);
    }
    expected.sort();
    let first = repo.list_connections(&scope, None, 2).await.unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.connection_id)
            .collect::<Vec<_>>(),
        expected[..2]
    );
    assert_eq!(first.next_cursor, Some(expected[1]));
    let second = repo
        .list_connections(&scope, first.next_cursor, 2)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].connection_id, expected[2]);
    assert!(second.next_cursor.is_none());
    assert!(
        repo.list_connections(&scope, Some(Uuid::max()), 100)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    for limit in [0, 101] {
        assert_eq!(
            error_code(repo.list_connections(&scope, None, limit).await),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            error_code(repo.list_keys(&scope, expected[0], None, limit).await),
            ErrorCode::InvalidRequest
        );
    }

    let mut keys = Vec::new();
    let mut bearers = Vec::new();
    for _ in 0..3 {
        let issued = repo.issue_key(&scope, expected[0], None).await.unwrap();
        assert!(issued.key.expires_at.is_none());
        keys.push(issued.key.key_id);
        bearers.push(issued.bearer);
    }
    keys.sort();
    let first = repo.list_keys(&scope, expected[0], None, 2).await.unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.key_id)
            .collect::<Vec<_>>(),
        keys[..2]
    );
    assert_eq!(first.next_cursor, Some(keys[1]));
    let second = repo
        .list_keys(&scope, expected[0], first.next_cursor, 2)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].key_id, keys[2]);
    assert!(second.next_cursor.is_none());
    let serialized = serde_json::to_string(&first).unwrap();
    for metadata in &first.items {
        let value = serde_json::to_value(metadata).unwrap();
        let fields = value.as_object().unwrap();
        assert_eq!(fields.len(), 4);
        for field in ["key_id", "created_at", "expires_at", "revoked_at"] {
            assert!(fields.contains_key(field));
        }
    }
    let mut tx = database.pool().begin().await.unwrap();
    set_local_scope(&mut tx, &scope).await.unwrap();
    let hashes = sqlx::query("SELECT verification_hash FROM partner_keys WHERE connection_id=$1")
        .bind(expected[0])
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    for row in hashes {
        assert!(!serialized.contains(&row.get::<String, _>("verification_hash")));
    }
    for bearer in bearers {
        let (_, secret) = bearer.split_once('.').unwrap();
        assert!(!serialized.contains(&bearer));
        assert!(!serialized.contains(secret));
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn foreign_and_missing_resources_are_indistinguishable() {
    let (_, repo, scope) = setup().await;
    let connection = repo.create_connection(&scope).await.unwrap().connection_id;
    let issued = repo.issue_key(&scope, connection, None).await.unwrap();
    let foreign_scopes = [
        TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
        TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
        TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        ),
    ];
    for foreign in foreign_scopes {
        assert!(
            repo.list_connections(&foreign, None, 100)
                .await
                .unwrap()
                .items
                .is_empty()
        );
        assert_eq!(
            error_code(repo.list_keys(&foreign, connection, None, 100).await),
            ErrorCode::NotFound
        );
        assert_eq!(
            error_code(repo.issue_key(&foreign, connection, None).await),
            ErrorCode::NotFound
        );
        assert_eq!(
            error_code(
                repo.rotate_key(&foreign, connection, issued.key.key_id, None)
                    .await
            ),
            ErrorCode::NotFound
        );
        assert_eq!(
            error_code(
                repo.revoke_key(&foreign, connection, issued.key.key_id)
                    .await
            ),
            ErrorCode::NotFound
        );
        assert_eq!(
            error_code(repo.revoke_connection(&foreign, connection).await),
            ErrorCode::NotFound
        );
    }
    let missing = Uuid::new_v4();
    assert_eq!(
        error_code(repo.list_keys(&scope, missing, None, 100).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        error_code(repo.issue_key(&scope, missing, None).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        error_code(
            repo.rotate_key(&scope, missing, issued.key.key_id, None)
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        error_code(repo.rotate_key(&scope, connection, missing, None).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        error_code(repo.revoke_key(&scope, connection, missing).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        error_code(repo.revoke_connection(&scope, missing).await),
        ErrorCode::NotFound
    );
    let other = repo.create_connection(&scope).await.unwrap().connection_id;
    assert_eq!(
        error_code(repo.revoke_key(&scope, other, issued.key.key_id).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        repo.list_keys(&scope, connection, None, 100)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn lifecycle_reuses_audited_mutations_and_persists_metadata() {
    let (database, repo, scope) = setup().await;
    let service = PgPartnerRepository::from_database(&database);
    let connection = repo.create_connection(&scope).await.unwrap();
    assert!(connection.knowledge_write);
    assert!(connection.revoked_at.is_none());
    let id = connection.connection_id;
    let original = repo.issue_key(&scope, id, None).await.unwrap();
    assert!(authenticated(&service, id, &original).await);
    let expiry = Utc::now() + Duration::days(1);
    let rotated = repo
        .rotate_key(&scope, id, original.key.key_id, Some(expiry))
        .await
        .unwrap();
    assert!(rotated.key.expires_at.is_some());
    assert!(!authenticated(&service, id, &original).await);
    assert!(authenticated(&service, id, &rotated).await);
    for expiry in [Utc::now(), Utc::now() - Duration::seconds(1)] {
        assert_eq!(
            error_code(repo.issue_key(&scope, id, Some(expiry)).await),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            error_code(
                repo.rotate_key(&scope, id, rotated.key.key_id, Some(expiry))
                    .await
            ),
            ErrorCode::InvalidRequest
        );
    }
    assert!(authenticated(&service, id, &rotated).await);
    repo.revoke_key(&scope, id, rotated.key.key_id)
        .await
        .unwrap();
    repo.revoke_key(&scope, id, rotated.key.key_id)
        .await
        .unwrap();
    assert!(!authenticated(&service, id, &rotated).await);
    let remaining = repo.issue_key(&scope, id, None).await.unwrap();
    repo.revoke_connection(&scope, id).await.unwrap();
    repo.revoke_connection(&scope, id).await.unwrap();
    assert!(!authenticated(&service, id, &remaining).await);
    assert_eq!(
        error_code(repo.issue_key(&scope, id, None).await),
        ErrorCode::NotFound
    );
    let restored = PgPartnerAdminRepository::from_database(&database);
    let connections = restored.list_connections(&scope, None, 100).await.unwrap();
    assert!(connections.items[0].revoked_at.is_some());
    let keys = restored.list_keys(&scope, id, None, 100).await.unwrap();
    assert_eq!(keys.items.len(), 3);
    assert!(
        keys.items
            .iter()
            .find(|key| key.key_id == original.key.key_id)
            .unwrap()
            .revoked_at
            .is_some()
    );
    assert!(
        keys.items
            .iter()
            .find(|key| key.key_id == rotated.key.key_id)
            .unwrap()
            .revoked_at
            .is_some()
    );
    let mut tx = database.pool().begin().await.unwrap();
    set_local_scope(&mut tx, &scope).await.unwrap();
    let rows = sqlx::query("SELECT event FROM partner_audit_events WHERE connection_id=$1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    let events: Vec<String> = rows.iter().map(|row| row.get("event")).collect();
    for event in [
        "connection_created",
        "key_issued",
        "key_revoked",
        "connection_revoked",
    ] {
        assert!(events.iter().any(|actual| actual == event));
    }
    tx.commit().await.unwrap();
}
