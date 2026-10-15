use chrono::Duration;
use geo_domain::{
    ErrorCode, KnowledgePurpose, KnowledgeRepository, PartnerAdvance, PartnerDisposition,
    PartnerPrincipal, PartnerRepository, PartnerSyncAction, PartnerSyncItem, PartnerSyncRequest,
    PartnerSyncStatus, PartnerSyncWork, TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgKnowledgeRepository, PgPartnerRepository, set_local_scope,
};
use sqlx::Row;
use uuid::Uuid;

async fn setup() -> (Database, PgPartnerRepository, PartnerPrincipal) {
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
        .bind(format!("partner-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(operator)
    .bind(tenant)
    .bind(format!("partner-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(operator_id,tenant_id,project_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
        .bind(operator).bind(tenant).bind(project).bind(format!("partner-{project}")).execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let repo = PgPartnerRepository::from_database(&database);
    let connection = repo.create_connection(&scope).await.unwrap();
    let key = repo.issue_key(&scope, connection, None).await.unwrap();
    let principal = repo
        .authenticate(connection, key.key_id, &key.secret)
        .await
        .unwrap();
    (database, repo, principal)
}

fn item(source: &str, version: &str, sequence: i64) -> PartnerSyncItem {
    PartnerSyncItem {
        external_source_id: source.into(),
        external_version: version.into(),
        source_sequence: sequence,
        effective_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        action: PartnerSyncAction::Upsert {
            name: "Synthetic".into(),
            purpose: KnowledgePurpose::Public,
            media_type: "text/plain".into(),
            text: "Synthetic evidence.".into(),
            content_sha256: sha256_hex(b"Synthetic evidence."),
        },
    }
}

fn request(items: Vec<PartnerSyncItem>) -> PartnerSyncRequest {
    PartnerSyncRequest {
        schema_version: 1,
        items,
    }
}

fn work(principal: &PartnerPrincipal, operation_id: Uuid) -> PartnerSyncWork {
    PartnerSyncWork {
        scope: principal.scope.clone(),
        connection_id: principal.connection_id,
        operation_id,
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn durable_acceptance_restart_idempotency_mixed_results_and_cursor() {
    let (database, repo, principal) = setup().await;
    let input = request(vec![
        item("synthetic-a", "v1", 1),
        item("synthetic-b", "v0", 0),
        item("synthetic-c", "v1", 1),
    ]);
    let receipt = repo
        .accept(&principal, "stable", input.clone())
        .await
        .unwrap();
    assert_eq!(receipt.status, PartnerSyncStatus::Pending);
    assert_eq!(receipt.completed_count, 0);
    let replay = repo.accept(&principal, "stable", input).await.unwrap();
    assert_eq!(receipt.operation_id, replay.operation_id);
    assert_eq!(
        repo.accept(
            &principal,
            "stable",
            request(vec![item("different", "v1", 1)])
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let restored = PgPartnerRepository::from_database(&database);
    let pending = work(&principal, receipt.operation_id);
    assert_eq!(
        restored.advance_one(&pending).await.unwrap(),
        PartnerAdvance::ItemCommitted {
            index: 0,
            operation_completed: false
        }
    );
    assert_eq!(
        restored
            .results(&principal, receipt.operation_id, None, 1)
            .await
            .unwrap()
            .operation
            .completed_count,
        1
    );
    restored.advance_one(&pending).await.unwrap();
    restored.advance_one(&pending).await.unwrap();
    assert_eq!(
        restored.advance_one(&pending).await.unwrap(),
        PartnerAdvance::Completed
    );
    let first = restored
        .results(&principal, receipt.operation_id, None, 1)
        .await
        .unwrap();
    assert_eq!(first.operation.status, PartnerSyncStatus::Completed);
    assert_eq!(first.results[0].disposition, PartnerDisposition::Accepted);
    assert_eq!(first.next_index, Some(0));
    let rest = restored
        .results(&principal, receipt.operation_id, first.next_index, 100)
        .await
        .unwrap();
    assert_eq!(rest.results.len(), 2);
    assert_eq!(rest.results[0].disposition, PartnerDisposition::Rejected);
    assert_eq!(
        rest.results[0].reason_code.as_deref(),
        Some("invalid_source_identity")
    );
    assert_eq!(rest.results[1].disposition, PartnerDisposition::Accepted);
    assert!(rest.next_index.is_none());
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn source_identity_replay_conflict_tombstone_and_newer_restore() {
    let (_, repo, principal) = setup().await;
    let mut tombstone = item("synthetic-a", "deleted", 5);
    tombstone.action = PartnerSyncAction::Revoke {};
    let mut collision = item("synthetic-a", "deleted", 5);
    collision.effective_at += Duration::seconds(1);
    let batch = repo
        .accept(
            &principal,
            "versions",
            request(vec![
                tombstone.clone(),
                tombstone,
                item("synthetic-a", "old", 4),
                collision,
                item("synthetic-a", "restored", 6),
                item("synthetic-a", "restored", 6),
                item("synthetic-a", "different-version", 6),
            ]),
        )
        .await
        .unwrap();
    for _ in 0..7 {
        repo.advance_one(&work(&principal, batch.operation_id))
            .await
            .unwrap();
    }
    let page = repo
        .results(&principal, batch.operation_id, None, 100)
        .await
        .unwrap();
    assert_eq!(
        page.results
            .iter()
            .map(|item| item.disposition)
            .collect::<Vec<_>>(),
        vec![
            PartnerDisposition::Accepted,
            PartnerDisposition::Unchanged,
            PartnerDisposition::Rejected,
            PartnerDisposition::Rejected,
            PartnerDisposition::Accepted,
            PartnerDisposition::Unchanged,
            PartnerDisposition::Rejected
        ]
    );
    assert!(page.results[0].resources.source_id.is_none());
    assert_eq!(page.results[4].resources, page.results[5].resources);
    assert!(page.results[4].resources.source_id.is_some());
    assert_eq!(
        page.results[2].reason_code.as_deref(),
        Some("stale_source_sequence")
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn intake_restrictions_are_immediate_and_conflicting_or_stale_items_cannot_restrict() {
    let (database, repo, principal) = setup().await;
    let initial = repo
        .accept(
            &principal,
            "prime",
            request(vec![item("synthetic-a", "v1", 1)]),
        )
        .await
        .unwrap();
    repo.advance_one(&work(&principal, initial.operation_id))
        .await
        .unwrap();
    let resources = repo
        .results(&principal, initial.operation_id, None, 10)
        .await
        .unwrap()
        .results[0]
        .resources
        .clone();
    let source = resources.source_id.unwrap();
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let mut internal = item("synthetic-a", "v2", 2);
    if let PartnerSyncAction::Upsert { purpose, .. } = &mut internal.action {
        *purpose = KnowledgePurpose::Internal;
    }
    let restricted = repo
        .accept(&principal, "restrict", request(vec![internal]))
        .await
        .unwrap();
    let purpose: String =
        sqlx::query_scalar("SELECT purpose FROM knowledge_sources WHERE source_id=$1")
            .bind(source)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        purpose, "internal",
        "authority tightens before worker advances"
    );
    assert!(
        knowledge
            .get_import_progress(
                &principal.scope,
                resources.import_job_id.unwrap(),
                KnowledgePurpose::Public,
            )
            .await
            .unwrap()
            .is_none(),
        "old import receipt cannot bypass intake restriction"
    );
    repo.advance_one(&work(&principal, restricted.operation_id))
        .await
        .unwrap();
    let mut revoke = item("synthetic-a", "v3", 3);
    revoke.action = PartnerSyncAction::Revoke {};
    let revoked = repo
        .accept(&principal, "revoke", request(vec![revoke]))
        .await
        .unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM knowledge_sources WHERE source_id=$1")
            .bind(source)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        state, "removed",
        "revoke authority is part of durable acceptance"
    );
    let restore = repo
        .accept(
            &principal,
            "restore",
            request(vec![item("synthetic-a", "v4", 4)]),
        )
        .await
        .unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM knowledge_sources WHERE source_id=$1")
            .bind(source)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        state, "removed",
        "public intake alone must not relax a revoke fence"
    );
    assert_eq!(
        repo.advance_one(&work(&principal, restore.operation_id))
            .await
            .unwrap(),
        PartnerAdvance::Deferred
    );
    repo.advance_one(&work(&principal, revoked.operation_id))
        .await
        .unwrap();
    sqlx::query("UPDATE partner_sync_operations SET next_attempt_at=now() WHERE operation_id=$1")
        .bind(restore.operation_id)
        .execute(database.pool())
        .await
        .unwrap();
    repo.advance_one(&work(&principal, restore.operation_id))
        .await
        .unwrap();
    let mut conflict = item("synthetic-a", "v4", 5);
    conflict.action = PartnerSyncAction::Revoke {};
    let mut stale = item("synthetic-a", "older-unseen", 2);
    stale.action = PartnerSyncAction::Revoke {};
    let invalid = repo
        .accept(
            &principal,
            "invalid-restrictions",
            request(vec![conflict, stale]),
        )
        .await
        .unwrap();
    let row = sqlx::query("SELECT purpose,state FROM knowledge_sources WHERE source_id=$1")
        .bind(source)
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("purpose"), "public");
    assert_eq!(row.get::<String, _>("state"), "active");
    repo.advance_one(&work(&principal, invalid.operation_id))
        .await
        .unwrap();
    repo.advance_one(&work(&principal, invalid.operation_id))
        .await
        .unwrap();
    let page = repo
        .results(&principal, invalid.operation_id, None, 10)
        .await
        .unwrap();
    assert!(
        page.results
            .iter()
            .all(|result| result.disposition == PartnerDisposition::Rejected)
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn pending_fences_keep_earlier_valid_versions_nonpublic_without_discarding_receipts() {
    let (database, repo, principal) = setup().await;
    let mut revoke = item("synthetic-a", "v2", 2);
    revoke.action = PartnerSyncAction::Revoke {};
    let receipt = repo
        .accept(
            &principal,
            "fenced-history",
            request(vec![
                item("synthetic-a", "v1", 1),
                revoke,
                item("synthetic-a", "v3", 3),
            ]),
        )
        .await
        .unwrap();
    let pending = work(&principal, receipt.operation_id);
    repo.advance_one(&pending).await.unwrap();
    let first = repo
        .results(&principal, receipt.operation_id, None, 10)
        .await
        .unwrap();
    assert_eq!(first.results[0].disposition, PartnerDisposition::Accepted);
    let source = first.results[0].resources.source_id.unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM knowledge_sources WHERE source_id=$1")
            .bind(source)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        state, "removed",
        "first materialization inherits the future revoke fence"
    );
    repo.advance_one(&pending).await.unwrap();
    repo.advance_one(&pending).await.unwrap();
    let page = repo
        .results(&principal, receipt.operation_id, None, 10)
        .await
        .unwrap();
    assert_eq!(page.results.len(), 3);
    assert!(
        page.results
            .iter()
            .all(|result| result.disposition == PartnerDisposition::Accepted)
    );
    assert_ne!(
        page.results[0].resources.source_version_id,
        page.results[2].resources.source_version_id
    );
    let row = sqlx::query("SELECT purpose,state FROM knowledge_sources WHERE source_id=$1")
        .bind(source)
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("purpose"), "public");
    assert_eq!(row.get::<String, _>("state"), "active");
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn rotated_revoked_expired_and_cross_connection_credentials_fail_closed() {
    let (database, repo, principal) = setup().await;
    let receipt = repo
        .accept(
            &principal,
            "revoke-between-items",
            request(vec![
                item("synthetic-a", "v1", 1),
                item("synthetic-b", "v1", 1),
            ]),
        )
        .await
        .unwrap();
    repo.advance_one(&work(&principal, receipt.operation_id))
        .await
        .unwrap();
    let replacement = repo
        .rotate_key(
            &principal.scope,
            principal.connection_id,
            principal.key_id,
            None,
        )
        .await
        .unwrap();
    let current = repo
        .authenticate(
            principal.connection_id,
            replacement.key_id,
            &replacement.secret,
        )
        .await
        .unwrap();
    assert_eq!(
        repo.results(&principal, receipt.operation_id, None, 100)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );
    // Rotation changes credential authority, never the connection's operation identity.
    let replay = repo
        .accept(
            &current,
            "revoke-between-items",
            request(vec![
                item("synthetic-a", "v1", 1),
                item("synthetic-b", "v1", 1),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(replay.operation_id, receipt.operation_id);
    repo.advance_one(&work(&principal, receipt.operation_id))
        .await
        .unwrap();
    let page = repo
        .results(&current, receipt.operation_id, None, 100)
        .await
        .unwrap();
    assert_eq!(page.results[0].disposition, PartnerDisposition::Accepted);
    assert_eq!(
        page.results[1].reason_code.as_deref(),
        Some("authorization_revoked")
    );
    let other = repo.create_connection(&principal.scope).await.unwrap();
    let other_key = repo.issue_key(&principal.scope, other, None).await.unwrap();
    assert!(
        repo.authenticate(other, replacement.key_id, &replacement.secret)
            .await
            .is_err()
    );
    let other_principal = repo
        .authenticate(other, other_key.key_id, &other_key.secret)
        .await
        .unwrap();
    assert_eq!(
        repo.results(&other_principal, receipt.operation_id, None, 100)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    sqlx::query("UPDATE partner_keys SET expires_at=now()-interval '1 second' WHERE key_id=$1")
        .bind(other_key.key_id)
        .execute(database.pool())
        .await
        .unwrap();
    assert!(
        repo.authenticate(other, other_key.key_id, &other_key.secret)
            .await
            .is_err()
    );
    repo.revoke_connection(&principal.scope, principal.connection_id)
        .await
        .unwrap();
    assert!(
        repo.authenticate(
            principal.connection_id,
            replacement.key_id,
            &replacement.secret
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn concurrent_workers_commit_each_item_once_and_preserve_cross_project_scope() {
    let (database, repo, principal) = setup().await;
    let request = request(vec![item("synthetic-a", "v1", 1)]);
    let (first, second) = tokio::join!(
        repo.accept(&principal, "concurrent", request.clone()),
        repo.accept(&principal, "concurrent", request)
    );
    let operation = first.unwrap().operation_id;
    assert_eq!(operation, second.unwrap().operation_id);
    let pending = work(&principal, operation);
    let (first, second) = tokio::join!(repo.advance_one(&pending), repo.advance_one(&pending));
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(
        [first, second]
            .iter()
            .filter(|result| matches!(result, PartnerAdvance::ItemCommitted { .. }))
            .count(),
        1
    );
    let mut forged = principal.clone();
    forged.scope.project_id = Some(Uuid::new_v4().into());
    assert_eq!(
        repo.results(&forged, operation, None, 100)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );
    let violation = sqlx::query("INSERT INTO partner_keys(operator_id,tenant_id,project_id,connection_id,key_id,verification_hash) \
        VALUES($1,$2,$3,$4,$5,'not-a-secret')")
        .bind(principal.scope.operator_id.as_uuid()).bind(principal.scope.tenant_id.as_uuid())
        .bind(Uuid::new_v4()).bind(principal.connection_id).bind(Uuid::new_v4())
        .execute(database.pool()).await.unwrap_err();
    assert_eq!(
        violation
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23503")
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn sql_failure_rolls_back_only_current_item_and_records_durable_backoff() {
    let (database, repo, principal) = setup().await;
    let receipt = repo
        .accept(
            &principal,
            "fault",
            request(vec![
                item("synthetic-a", "v1", 1),
                item("synthetic-b", "v1", 1),
            ]),
        )
        .await
        .unwrap();
    let pending = work(&principal, receipt.operation_id);
    repo.advance_one(&pending).await.unwrap();
    let unique = Uuid::new_v4().simple().to_string();
    let function = format!("partner_fault_{unique}");
    // Narrow, synthetic fault after helper writes but before item commit.
    sqlx::query(&format!(
        "CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.operation_id='{}'::uuid AND NEW.item_index=1 THEN \
         RAISE EXCEPTION 'synthetic storage fault'; END IF; RETURN NEW; END $$",
        receipt.operation_id
    ))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER {function} BEFORE INSERT ON partner_source_events \
        FOR EACH ROW EXECUTE FUNCTION {function}()"
    ))
    .execute(database.pool())
    .await
    .unwrap();
    let failed = repo.advance_one(&pending).await;
    sqlx::query(&format!("DROP TRIGGER {function} ON partner_source_events"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION {function}()"))
        .execute(database.pool())
        .await
        .unwrap();
    let error = failed.unwrap_err();
    assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    assert!(!error.message.contains("synthetic storage fault"));
    let page = repo
        .results(&principal, receipt.operation_id, None, 100)
        .await
        .unwrap();
    assert_eq!(page.operation.completed_count, 1);
    assert_eq!(page.results.len(), 1);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM knowledge_sources WHERE project_id=$1")
            .bind(principal.scope.project_id.unwrap().as_uuid())
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "failed item's source, versions, release and mapping must roll back"
    );
    let row = sqlx::query(
        "SELECT failure_count,reason_code,next_attempt_at>now() AS deferred \
        FROM partner_sync_operations WHERE operation_id=$1",
    )
    .bind(receipt.operation_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<i32, _>("failure_count"), 1);
    assert_eq!(row.get::<String, _>("reason_code"), "storage_unavailable");
    assert!(row.get::<bool, _>("deferred"));
    assert_eq!(
        repo.advance_one(&pending).await.unwrap(),
        PartnerAdvance::Deferred
    );
    sqlx::query("UPDATE partner_sync_operations SET next_attempt_at=now() WHERE operation_id=$1")
        .bind(receipt.operation_id)
        .execute(database.pool())
        .await
        .unwrap();
    let restored = PgPartnerRepository::from_database(&database);
    assert_eq!(
        restored.advance_one(&pending).await.unwrap(),
        PartnerAdvance::ItemCommitted {
            index: 1,
            operation_completed: true
        }
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL database and role-creation authority in GEO_TEST_DATABASE_URL"]
async fn partner_rls_non_owner_role_cannot_read_or_write_other_project() {
    let (database, _, principal) = setup().await;
    let mut tx = database.pool().begin().await.unwrap();
    let role = format!("partner_rls_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(
        "CREATE ROLE {role} NOLOGIN NOSUPERUSER NOBYPASSRLS"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA \"{}\" TO {role}",
        schema.replace('"', "\"\"")
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(&format!(
        "GRANT SELECT,INSERT ON partner_connections TO {role}"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    set_local_scope(&mut tx, &principal.scope).await.unwrap();
    sqlx::query(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let own: i64 = sqlx::query_scalar("SELECT count(*) FROM partner_connections")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(own, 1);
    sqlx::query("SELECT set_config('geo.project_id',$1,true)")
        .bind(Uuid::new_v4().to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let foreign: i64 = sqlx::query_scalar("SELECT count(*) FROM partner_connections")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(foreign, 0);
    let violation = sqlx::query("INSERT INTO partner_connections(operator_id,tenant_id,project_id,connection_id) VALUES($1,$2,$3,$4)")
        .bind(principal.scope.operator_id.as_uuid()).bind(principal.scope.tenant_id.as_uuid())
        .bind(principal.scope.project_id.unwrap().as_uuid()).bind(Uuid::new_v4())
        .execute(&mut *tx).await.unwrap_err();
    assert_eq!(
        violation
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    // Role, grants and transaction-local settings are all rolled back.
    tx.rollback().await.unwrap();
}
