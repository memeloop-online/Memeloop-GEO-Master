use geo_domain::{
    ImportItem, KnowledgePurpose, KnowledgeRepository, ProjectCreate, ProjectRepository,
    ProjectSettings, ReviseSourceTextCommand, SourceKind, SourceTextBasis, TenantScope, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgKnowledgeRepository, PgProjectRepository};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
struct RevisionCounts {
    source_versions: i64,
    authored_text: i64,
    chunks: i64,
    releases: i64,
    release_source_versions: i64,
    release_facts: i64,
    current_releases: i64,
    receipts: i64,
    revision_receipts: i64,
    outbox_events: i64,
    current_release_id: Option<Uuid>,
}

async fn scoped_revision_counts(
    pool: &PgPool,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    source_id: Uuid,
) -> RevisionCounts {
    let row = sqlx::query(
        "SELECT
            (SELECT count(*) FROM knowledge_source_versions
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_id=$4)
                AS source_versions,
            (SELECT count(*) FROM knowledge_authored_text
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_id=$4)
                AS authored_text,
            (SELECT count(*) FROM knowledge_chunks chunk
             WHERE chunk.operator_id=$1 AND chunk.tenant_id=$2 AND chunk.project_id=$3
               AND EXISTS (
                   SELECT 1 FROM knowledge_source_versions version
                   WHERE version.operator_id=chunk.operator_id
                     AND version.tenant_id=chunk.tenant_id
                     AND version.project_id=chunk.project_id
                     AND version.source_version_id=chunk.source_version_id
                     AND version.source_id=$4
               ))
                AS chunks,
            (SELECT count(*) FROM knowledge_releases
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3)
                AS releases,
            (SELECT count(*) FROM knowledge_release_source_versions
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3)
                AS release_source_versions,
            (SELECT count(*) FROM knowledge_release_facts
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3)
                AS release_facts,
            (SELECT count(*) FROM knowledge_current_releases
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3)
                AS current_releases,
            (SELECT count(*) FROM knowledge_import_receipts
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3)
                AS receipts,
            (SELECT count(*) FROM knowledge_import_receipts
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND action='source_text_revision' AND target_id=$4)
                AS revision_receipts,
            (SELECT count(*) FROM outbox_events
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3)
                AS outbox_events,
            (SELECT knowledge_release_id FROM knowledge_current_releases
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3)
                AS current_release_id",
    )
    .bind(operator_id)
    .bind(tenant_id)
    .bind(project_id)
    .bind(source_id)
    .fetch_one(pool)
    .await
    .unwrap();
    RevisionCounts {
        source_versions: row.get("source_versions"),
        authored_text: row.get("authored_text"),
        chunks: row.get("chunks"),
        releases: row.get("releases"),
        release_source_versions: row.get("release_source_versions"),
        release_facts: row.get("release_facts"),
        current_releases: row.get("current_releases"),
        receipts: row.get("receipts"),
        revision_receipts: row.get("revision_receipts"),
        outbox_events: row.get("outbox_events"),
        current_release_id: row.get("current_release_id"),
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn scoped_authored_revision_replays_after_repository_restart() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("revision-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("revision-{tenant}"))
        .execute(database.pool()).await.unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let project = PgProjectRepository::from_database(&database)
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("revision-{tenant}")),
                display_name: "Synthetic".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Synthetic".to_owned(),
                    market: "US".to_owned(),
                    language: "en".to_owned(),
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let repository = PgKnowledgeRepository::from_database(&database);
    let import = repository
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: format!("revision-{}", Uuid::new_v4()),
                kind: SourceKind::Text,
                name: "Synthetic".to_owned(),
                purpose: KnowledgePurpose::Public,
                text: Some("Original evidence".to_owned()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap()
        .items
        .remove(0);
    let source = import.source.unwrap();
    let original = import.source_version.unwrap();
    let command = ReviseSourceTextCommand {
        base_version_id: original.source_version_id,
        media_type: "text/markdown".to_owned(),
        text: "  中文\n\n* evidence  \n".to_owned(),
    };
    let receipt = repository
        .revise_source_text(
            &scope,
            source.source_id,
            source.revision,
            "stable-key",
            command.clone(),
        )
        .await
        .unwrap();
    let restarted = PgKnowledgeRepository::from_database(&database);
    assert_eq!(
        restarted
            .revise_source_text(
                &scope,
                source.source_id,
                source.revision,
                "stable-key",
                command.clone(),
            )
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        restarted
            .get_source_version_content(
                &scope,
                source.source_id,
                receipt.source_version.source_version_id,
            )
            .await
            .unwrap()
            .unwrap()
            .text,
        command.text
    );
    assert!(sqlx::query(
        "UPDATE knowledge_authored_text SET body='changed' WHERE source_version_id=$1",
    )
    .bind(receipt.source_version.source_version_id)
    .execute(database.pool())
    .await
    .is_err());
    assert!(
        sqlx::query(
            "INSERT INTO knowledge_authored_text
         (operator_id,tenant_id,project_id,source_id,source_version_id,media_type,body)
         VALUES ($1,$2,$3,$4,$5,'text/plain','incorrect representation')",
        )
        .bind(operator)
        .bind(tenant)
        .bind(project.id.as_uuid())
        .bind(source.source_id)
        .bind(original.source_version_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    assert_eq!(
        restarted
            .get_source_version_content(&scope, source.source_id, original.source_version_id,)
            .await
            .unwrap()
            .unwrap()
            .text_basis,
        SourceTextBasis::Extracted
    );
    let detail = restarted
        .get_source_detail(&scope, source.source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.versions.len(), 2);
    assert!(
        detail
            .chunks
            .iter()
            .any(|chunk| chunk.source_version_id == original.source_version_id)
    );
    assert!(
        detail
            .chunks
            .iter()
            .any(|chunk| chunk.source_version_id == receipt.source_version.source_version_id)
    );
    assert_eq!(
        restarted
            .revise_source_text(
                &scope,
                source.source_id,
                source.revision,
                "stable-key",
                ReviseSourceTextCommand {
                    text: "Different".to_owned(),
                    ..command.clone()
                }
            )
            .await
            .unwrap_err()
            .details
            .unwrap()["reason"],
        "idempotency_conflict"
    );
    assert_eq!(
        restarted
            .revise_source_text(
                &scope,
                source.source_id,
                source.revision,
                "different-key",
                command,
            )
            .await
            .unwrap_err()
            .details
            .unwrap()["reason"],
        "source_revision_conflict"
    );
    assert!(
        restarted
            .get_source_version_content(&scope, Uuid::new_v4(), original.source_version_id)
            .await
            .unwrap()
            .is_none()
    );
    // An unsuccessful parse can reserve a number above the published pointer.
    let reserved_version_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO knowledge_source_versions
         (source_version_id,operator_id,tenant_id,project_id,source_id,version,
          content_sha256,captured_at,parent_version_id,parser_version,extraction_version)
         VALUES ($1,$2,$3,$4,$5,3,$6,now(),$7,'synthetic-test-v1','none-v1')",
    )
    .bind(reserved_version_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(source.source_id)
    .bind(sha256_hex(b"unpublished"))
    .bind(receipt.source_version.source_version_id)
    .execute(database.pool())
    .await
    .unwrap();
    let next = ReviseSourceTextCommand {
        base_version_id: receipt.source_version.source_version_id,
        media_type: "text/plain".to_owned(),
        text: "A second branch".to_owned(),
    };
    let other = ReviseSourceTextCommand {
        text: "Competing second branch".to_owned(),
        ..next.clone()
    };
    let (left, right) = tokio::join!(
        restarted.revise_source_text(
            &scope,
            source.source_id,
            receipt.source.revision,
            "parallel-left",
            next,
        ),
        repository.revise_source_text(
            &scope,
            source.source_id,
            receipt.source.revision,
            "parallel-right",
            other,
        ),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let losing = left.err().or(right.err()).unwrap();
    assert_eq!(
        losing.details.unwrap()["reason"],
        "source_revision_conflict"
    );
    let latest = restarted
        .get_source_detail(&scope, source.source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.versions.len(), 4);
    assert_eq!(latest.versions.last().unwrap().version, 4);
    assert_eq!(latest.source.revision, source.revision + 2);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn authored_revision_transaction_failure_rolls_back_and_retries_same_request() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("rollback-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant)
        .bind(operator)
        .bind(format!("rollback-{tenant}"))
        .execute(database.pool())
        .await
        .unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let project = PgProjectRepository::from_database(&database)
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("rollback-{tenant}")),
                display_name: "Synthetic".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Synthetic".to_owned(),
                    market: "US".to_owned(),
                    language: "en".to_owned(),
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let project_id = project.id.as_uuid();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project_id.into()));
    let repository = PgKnowledgeRepository::from_database(&database);
    let import = repository
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: format!("rollback-{}", Uuid::new_v4()),
                kind: SourceKind::Text,
                name: "Synthetic".to_owned(),
                purpose: KnowledgePurpose::Public,
                text: Some("Original evidence".to_owned()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap()
        .items
        .remove(0);
    let source = import.source.unwrap();
    let original = import.source_version.unwrap();
    let source_id = source.source_id;
    let command = ReviseSourceTextCommand {
        base_version_id: original.source_version_id,
        media_type: "text/markdown".to_owned(),
        text: "Rollback-safe authored text".to_owned(),
    };
    let idempotency_key = "rollback-once";
    let before_source = repository
        .get_source_detail(&scope, source_id)
        .await
        .unwrap()
        .unwrap()
        .source;
    let before_counts =
        scoped_revision_counts(database.pool(), operator, tenant, project_id, source_id).await;

    // The release outbox row is inserted first; this trigger fails only on
    // the final source-version event so all prior writes must roll back too.
    let suffix = Uuid::new_v4().simple().to_string();
    let function_name = format!("geo_test_revision_rollback_{suffix}");
    let trigger_name = format!("geo_test_revision_rollback_{suffix}");
    sqlx::query(&format!(
        r#"CREATE FUNCTION "{function_name}"() RETURNS trigger
           LANGUAGE plpgsql AS $$
           BEGIN
             IF NEW.event_type = 'knowledge.source.version.ready'
                AND NEW.project_id = '{project_id}'
                AND NEW.aggregate_id = '{source_id}' THEN
               RAISE EXCEPTION 'synthetic knowledge revision rollback';
             END IF;
             RETURN NEW;
           END;
           $$"#
    ))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(&format!(
        r#"CREATE TRIGGER "{trigger_name}"
           BEFORE INSERT ON outbox_events
           FOR EACH ROW EXECUTE FUNCTION "{function_name}"()"#
    ))
    .execute(database.pool())
    .await
    .unwrap();

    let failed = repository
        .revise_source_text(
            &scope,
            source_id,
            source.revision,
            idempotency_key,
            command.clone(),
        )
        .await;
    assert!(failed.is_err());
    let after_failure_source = repository
        .get_source_detail(&scope, source_id)
        .await
        .unwrap()
        .unwrap()
        .source;
    let after_failure_counts =
        scoped_revision_counts(database.pool(), operator, tenant, project_id, source_id).await;
    assert_eq!(after_failure_source, before_source);
    assert_eq!(after_failure_counts, before_counts);

    sqlx::query(&format!(
        r#"DROP TRIGGER IF EXISTS "{trigger_name}" ON outbox_events"#
    ))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(&format!(r#"DROP FUNCTION IF EXISTS "{function_name}"()"#))
        .execute(database.pool())
        .await
        .unwrap();

    let receipt = repository
        .revise_source_text(&scope, source_id, source.revision, idempotency_key, command)
        .await
        .unwrap();
    assert_eq!(
        receipt.source_version.parent_version_id,
        Some(original.source_version_id)
    );
    assert_eq!(receipt.source.revision, source.revision + 1);
    let after_success_counts =
        scoped_revision_counts(database.pool(), operator, tenant, project_id, source_id).await;
    assert_eq!(
        after_success_counts.source_versions,
        before_counts.source_versions + 1
    );
    assert_eq!(
        after_success_counts.authored_text,
        before_counts.authored_text + 1
    );
    assert!(
        after_success_counts.chunks > before_counts.chunks,
        "the authored revision should persist at least one chunk"
    );
    assert_eq!(after_success_counts.releases, before_counts.releases + 1);
    assert_eq!(
        after_success_counts.release_source_versions,
        before_counts.release_source_versions + 1
    );
    assert_eq!(
        after_success_counts.release_facts,
        before_counts.release_facts
    );
    assert_eq!(
        after_success_counts.current_releases,
        before_counts.current_releases
    );
    assert_eq!(after_success_counts.receipts, before_counts.receipts + 1);
    assert_eq!(
        after_success_counts.revision_receipts,
        before_counts.revision_receipts + 1
    );
    assert_eq!(
        after_success_counts.outbox_events,
        before_counts.outbox_events + 2
    );
    assert_ne!(
        after_success_counts.current_release_id,
        before_counts.current_release_id
    );
}
