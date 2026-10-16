use chrono::Utc;
use geo_domain::{
    DocumentManifestPlanRequest, InitialSource, InitialSourceKind, InitialSourceVisibility,
    KnowledgePurpose, KnowledgeRepository, PartnerAdvance, PartnerPrincipal, PartnerRepository,
    PartnerSyncAction, PartnerSyncItem, PartnerSyncRequest, PartnerSyncWork, ProjectCreate,
    ProjectRepository, ProjectSettings, ProjectStartCommand, SourceState, TenantScope,
    hash_idempotency_key, settings_hash, sha256_hex, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgKnowledgeRepository, PgPartnerRepository, PgProjectRepository,
};
use uuid::Uuid;

async fn sync(
    repo: &PgPartnerRepository,
    principal: &PartnerPrincipal,
    sequence: i64,
    action: PartnerSyncAction,
) -> geo_domain::PartnerKnowledgeRefs {
    let receipt = accept(repo, principal, sequence, action).await;
    advance(repo, principal, receipt).await
}

async fn accept(
    repo: &PgPartnerRepository,
    principal: &PartnerPrincipal,
    sequence: i64,
    action: PartnerSyncAction,
) -> geo_domain::PartnerSyncReceipt {
    repo.accept(
        principal,
        &format!("item-{sequence}"),
        PartnerSyncRequest {
            schema_version: 1,
            items: vec![PartnerSyncItem {
                external_source_id: "synthetic-source".into(),
                external_version: format!("revision-{sequence}"),
                source_sequence: sequence,
                effective_at: Utc::now(),
                action,
            }],
        },
    )
    .await
    .unwrap()
}

async fn advance(
    repo: &PgPartnerRepository,
    principal: &PartnerPrincipal,
    receipt: geo_domain::PartnerSyncReceipt,
) -> geo_domain::PartnerKnowledgeRefs {
    assert!(matches!(
        repo.advance_one(&PartnerSyncWork {
            scope: principal.scope.clone(),
            connection_id: principal.connection_id,
            operation_id: receipt.operation_id,
        })
        .await
        .unwrap(),
        PartnerAdvance::ItemCommitted { .. }
    ));
    let page = repo
        .results(principal, receipt.operation_id, None, 10)
        .await
        .unwrap();
    assert_eq!(page.results.len(), 1);
    assert_eq!(
        page.results[0].disposition,
        geo_domain::PartnerDisposition::Accepted
    );
    page.results[0].resources.clone()
}

fn text(purpose: KnowledgePurpose, body: &str) -> PartnerSyncAction {
    PartnerSyncAction::Upsert {
        name: "Synthetic source".into(),
        purpose,
        media_type: "text/markdown".into(),
        text: body.into(),
        content_sha256: sha256_hex(body.as_bytes()),
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn partner_versions_revoke_restore_and_old_release_authorization() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("sync-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("sync-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let project = PgProjectRepository::from_database(&database)
        .create(
            &TenantScope::new(operator.into(), tenant.into(), None),
            ProjectCreate {
                slug: Some(format!("sync-{tenant}")),
                display_name: "Synthetic".into(),
                settings: ProjectSettings {
                    brand_name: "Synthetic".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.com".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let frozen = project.settings.clone().validate_start().unwrap();
    let frozen_hash = settings_hash(&frozen).unwrap();
    let started = PgProjectRepository::from_database(&database)
        .start(
            &TenantScope::new(operator.into(), tenant.into(), None),
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("synthetic-start"),
                request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
                settings_hash: frozen_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let repo = PgPartnerRepository::from_database(&database);
    let connection = repo.create_connection(&scope).await.unwrap();
    let key = repo.issue_key(&scope, connection, None).await.unwrap();
    let principal = repo
        .authenticate(connection, key.key_id, &key.secret)
        .await
        .unwrap();
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let first = sync(
        &repo,
        &principal,
        1,
        text(KnowledgePurpose::Public, "# Synthetic\n\nFirst content."),
    )
    .await;
    let source_id = first.source_id.unwrap();
    let old_release = first.knowledge_release_id;
    let original_job = first.import_job_id.unwrap();
    assert!(
        knowledge
            .get_import_progress(&scope, original_job, KnowledgePurpose::Public)
            .await
            .unwrap()
            .is_some()
    );
    let pending_internal = accept(
        &repo,
        &principal,
        2,
        text(KnowledgePurpose::Internal, "# Synthetic\n\nSecond content."),
    )
    .await;
    assert!(
        knowledge
            .get_import_progress(&scope, original_job, KnowledgePurpose::Public)
            .await
            .unwrap()
            .is_none()
    );
    let before_parse = knowledge
        .get_source_detail(&scope, source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before_parse.versions.len(), 1);
    assert_eq!(before_parse.import_jobs.len(), 1);
    assert_eq!(before_parse.source.purpose, KnowledgePurpose::Internal);
    let second = advance(&repo, &principal, pending_internal).await;
    assert_eq!(second.source_id, first.source_id);
    assert_ne!(second.source_version_id, first.source_version_id);
    let version = knowledge
        .get_source_version(&scope, source_id, second.source_version_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.version, 2);
    assert_eq!(version.parent_version_id, first.source_version_id);
    assert!(
        knowledge
            .get_import_progress(&scope, original_job, KnowledgePurpose::Public)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        knowledge
            .get_import_progress(&scope, original_job, KnowledgePurpose::Internal)
            .await
            .unwrap()
            .is_some()
    );
    let third = sync(
        &repo,
        &principal,
        3,
        text(KnowledgePurpose::Public, "# Synthetic\n\nThird content."),
    )
    .await;
    let pending_revoke = accept(&repo, &principal, 4, PartnerSyncAction::Revoke {}).await;
    assert_eq!(
        knowledge
            .get_source(&scope, source_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        SourceState::Removed
    );
    assert!(
        knowledge
            .get_import_progress(&scope, original_job, KnowledgePurpose::Internal)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        knowledge
            .get_import_progress(&scope, original_job, KnowledgePurpose::Public)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        knowledge
            .get_source_version(&scope, source_id, first.source_version_id.unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let mut document_scope = frozen.document_scope.clone();
    document_scope.markets = frozen.effective_markets();
    document_scope.languages = frozen.effective_languages();
    let manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: started.document_manifest.manifest_id,
                knowledge_release_id: old_release.unwrap(),
            },
            document_scope,
        )
        .await
        .unwrap();
    assert!(!manifest.items.is_empty());
    assert!(
        manifest
            .items
            .iter()
            .all(|item| item.source_version_refs.is_empty())
    );
    let revoked = advance(&repo, &principal, pending_revoke).await;
    assert_eq!(revoked.source_version_id, third.source_version_id);
    assert!(revoked.import_job_id.is_none());
    let restored = sync(
        &repo,
        &principal,
        5,
        text(KnowledgePurpose::Public, "# Synthetic\n\nRestored content."),
    )
    .await;
    assert_eq!(restored.source_id, first.source_id);
    let version = knowledge
        .get_source_version(&scope, source_id, restored.source_version_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.version, 4);
    assert_eq!(version.parent_version_id, third.source_version_id);
    assert_eq!(
        knowledge
            .get_source(&scope, source_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        SourceState::Active
    );
    let detail = knowledge
        .get_source_detail(&scope, source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.versions.len(), 4);
    assert_eq!(detail.import_jobs.len(), 4);
    assert!(
        detail
            .import_jobs
            .iter()
            .all(|job| job.status == geo_domain::ImportStatus::Succeeded)
    );
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
         AND aggregate_id=$4 AND event_type='knowledge.source.revoked'",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(source_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(events, 1);
}
