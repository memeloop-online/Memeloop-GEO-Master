use std::sync::Arc;
use std::time::Duration;

use geo_domain::{
    AgentRepository, AppendMessage, AttachmentId, AttachmentReference, CreateConversation,
    DocumentManifestPlanRequest, ErrorCode, ImportItem, InitialSource, InitialSourceKind,
    InitialSourceVisibility, KnowledgePurpose, KnowledgeRepository, MessageRole, ObjectRef,
    ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand, RecordToolCall,
    RunCompletion, RunStatus, RuntimeCapability, SourceKind, StoreCheckpoint, TenantScope,
    ToolCallDecision, ToolCallIdentity, ToolCallOutcome, TurnStatus, UploadSessionCommand,
    hash_idempotency_key, settings_hash, sha256_hex, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgAgentRepository, PgKnowledgeRepository, PgProjectRepository,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn embedded_migrations_apply_to_postgres_when_configured() {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");

    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("connect and apply embedded migrations");

    // Running the embedded migration set again must be safe.
    database.migrate().await.expect("re-run migration set");

    let required_tables = [
        "operators",
        "tenants",
        "projects",
        "operations",
        "idempotency_records",
        "outbox_events",
        "project_config_revisions",
        "project_start_records",
        "optimization_cycles",
        "document_manifests",
        "distribution_manifests",
        "workflow_runs",
        "agent_conversations",
        "agent_messages",
        "agent_message_attachments",
        "agent_turns",
        "agent_runs",
        "agent_checkpoints",
        "agent_tool_call_ledger",
        "agent_conversation_events",
        "agent_submissions",
    ];

    for table in required_tables {
        let relation: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(format!("public.{table}"))
            .fetch_one(database.pool())
            .await
            .expect("query migration result");
        assert_eq!(relation.as_deref(), Some(table));
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn atomic_start_and_scope_visibility_hold_when_postgres_is_configured() {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");
    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let other_tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id, slug, display_name) VALUES ($1,$2,$3)")
        .bind(operator_id)
        .bind(format!("atomic-{operator_id}"))
        .bind("Atomic test operator")
        .execute(database.pool())
        .await
        .expect("operator");
    for tenant_id in [tenant_id, other_tenant_id] {
        sqlx::query(
            "INSERT INTO tenants (tenant_id, operator_id, slug, display_name) VALUES ($1,$2,$3,$4)",
        )
        .bind(tenant_id)
        .bind(operator_id)
        .bind(format!("tenant-{tenant_id}"))
        .bind("Atomic test tenant")
        .execute(database.pool())
        .await
        .expect("tenant");
    }
    let scope = TenantScope::new(operator_id.into(), tenant_id.into(), None);
    let repository = PgProjectRepository::from_database(&database);
    let project = repository
        .create(
            &scope,
            ProjectCreate {
                slug: Some(format!("atomic-{tenant_id}")),
                display_name: "Atomic start".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Acme".to_owned(),
                    market: "US".to_owned(),
                    language: "en".to_owned(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.com".to_owned(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .expect("draft");
    let frozen = project
        .settings
        .clone()
        .validate_start()
        .expect("startable");
    let frozen_hash = settings_hash(&frozen).expect("settings hash");
    let command = ProjectStartCommand {
        expected_revision: project.revision,
        idempotency_key_hash: hash_idempotency_key("atomic-start"),
        request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
        settings_hash: frozen_hash,
        operation_id: Uuid::new_v4(),
    };
    let acceptance = repository
        .start(&scope, project.id, command.clone())
        .await
        .expect("atomic start");
    assert!(!acceptance.document_manifest.sealed);
    assert_eq!(acceptance.document_manifest.expected_count, None);
    assert_eq!(acceptance.document_manifest.state, "awaiting_knowledge");
    assert_eq!(acceptance.distribution_manifest.state, "awaiting_documents");
    let project_scope = TenantScope::new(scope.operator_id, scope.tenant_id, Some(project.id));
    let knowledge = PgKnowledgeRepository::from_database(&database);
    assert_eq!(
        knowledge
            .get_document_manifest(&project_scope, acceptance.document_manifest.manifest_id)
            .await
            .expect("unsealed skeleton is not a readable snapshot"),
        None
    );
    let imported = knowledge
        .import_batch(
            &project_scope,
            vec![ImportItem {
                client_item_id: format!("manifest-public-{tenant_id}"),
                kind: SourceKind::Text,
                name: "Public source".to_owned(),
                purpose: KnowledgePurpose::Public,
                text: Some("Public company description".to_owned()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .expect("import");
    let request = DocumentManifestPlanRequest {
        manifest_id: acceptance.document_manifest.manifest_id,
        knowledge_release_id: imported.items[0]
            .release
            .as_ref()
            .expect("release")
            .knowledge_release_id,
    };
    let mut document_scope = frozen.document_scope.clone();
    document_scope.markets = frozen.effective_markets();
    document_scope.languages = frozen.effective_languages();
    let manifest = knowledge
        .plan_document_manifest(&project_scope, request.clone(), document_scope.clone())
        .await
        .expect("seal finite document manifest");
    assert_eq!(manifest.expected_count, Some(1));
    assert_eq!(manifest.coverage.planned, 1);
    assert_eq!(manifest.items[0].source_version_refs.len(), 1);
    assert_eq!(
        knowledge
            .get_document_manifest(&project_scope, request.manifest_id)
            .await
            .expect("read stored manifest"),
        Some(manifest.clone())
    );
    assert_eq!(
        knowledge
            .get_document_manifest(&project_scope, Uuid::new_v4())
            .await
            .expect("missing manifest"),
        None
    );
    assert_eq!(
        knowledge
            .get_document_manifest(
                &TenantScope::new(scope.operator_id, other_tenant_id.into(), Some(project.id)),
                request.manifest_id
            )
            .await
            .expect("tenant-scoped manifest"),
        None
    );
    knowledge
        .import_batch(
            &project_scope,
            vec![ImportItem {
                client_item_id: format!("manifest-later-{tenant_id}"),
                kind: SourceKind::Text,
                name: "Later public source".to_owned(),
                purpose: KnowledgePurpose::Public,
                text: Some("A later public description".to_owned()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .expect("newer release");
    assert_eq!(
        knowledge
            .get_document_manifest(&project_scope, request.manifest_id)
            .await
            .expect("historical manifest"),
        Some(manifest.clone())
    );
    assert_eq!(
        manifest,
        knowledge
            .plan_document_manifest(&project_scope, request.clone(), document_scope.clone())
            .await
            .expect("read sealed manifest")
    );
    let stored_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM document_manifest_items WHERE manifest_id=$1")
            .bind(request.manifest_id)
            .fetch_one(database.pool())
            .await
            .expect("count persisted items");
    assert_eq!(stored_count, 1);
    assert!(
        knowledge
            .plan_document_manifest(
                &TenantScope::new(scope.operator_id, other_tenant_id.into(), Some(project.id)),
                request.clone(),
                document_scope.clone()
            )
            .await
            .is_err()
    );
    assert!(
        knowledge
            .plan_document_manifest(
                &project_scope,
                request,
                geo_domain::DocumentScope {
                    markets: vec!["GB".to_owned()],
                    ..document_scope
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        repository
            .start(&scope, project.id, command)
            .await
            .expect("same-key replay"),
        acceptance
    );
    let other_scope = TenantScope::new(operator_id.into(), other_tenant_id.into(), None);
    assert!(
        repository
            .get_start(&other_scope, project.id)
            .await
            .expect("scoped lookup")
            .is_none()
    );
}

async fn connect() -> Database {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");
    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    Database::connect_and_migrate(&config)
        .await
        .expect("connect and apply embedded migrations")
}

/// Creates an operator, a tenant and one project, and returns the project scope.
async fn seed_scope(pool: &PgPool, label: &str) -> TenantScope {
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id, slug, display_name) VALUES ($1,$2,$3)")
        .bind(operator_id)
        .bind(format!("agent-{label}-{operator_id}"))
        .bind("Agent test operator")
        .execute(pool)
        .await
        .expect("operator");
    sqlx::query(
        "INSERT INTO tenants (tenant_id, operator_id, slug, display_name) VALUES ($1,$2,$3,$4)",
    )
    .bind(tenant_id)
    .bind(operator_id)
    .bind(format!("agent-{label}-{tenant_id}"))
    .bind("Agent test tenant")
    .execute(pool)
    .await
    .expect("tenant");
    TenantScope::new(
        operator_id.into(),
        tenant_id.into(),
        Some(
            seed_project(pool, operator_id, tenant_id, label)
                .await
                .into(),
        ),
    )
}

/// Adds a second project inside the same operator/tenant as `scope`.
async fn seed_sibling_scope(pool: &PgPool, scope: &TenantScope, label: &str) -> TenantScope {
    TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(
            seed_project(
                pool,
                scope.operator_id.as_uuid(),
                scope.tenant_id.as_uuid(),
                label,
            )
            .await
            .into(),
        ),
    )
}

async fn seed_project(pool: &PgPool, operator_id: Uuid, tenant_id: Uuid, label: &str) -> Uuid {
    let project_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO projects (project_id, operator_id, tenant_id, slug, display_name, status)
         VALUES ($1,$2,$3,$4,$5,'active')",
    )
    .bind(project_id)
    .bind(operator_id)
    .bind(tenant_id)
    .bind(format!("agent-{label}-{project_id}"))
    .bind("Agent test project")
    .execute(pool)
    .await
    .expect("project");
    project_id
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn csv_generation_slices_persist_without_duplicating_full_record_search() {
    let database = connect().await;
    let repository = PgKnowledgeRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "csv-slices").await;
    let sibling = seed_sibling_scope(database.pool(), &scope, "csv-slices-other").await;
    let value = "unique-slice-marker ".repeat(180);
    let bytes = format!("Product,Details\nWidget,{value}\n").into_bytes();
    let session = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "long-record.csv".to_owned(),
                declared_media_type: "text/csv".to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(&bytes),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(&scope, session.upload_session_id, bytes)
        .await
        .unwrap();
    let imported = repository
        .complete_upload(&scope, session.upload_session_id, "csv-slices")
        .await
        .unwrap();
    let version = imported.source_version.as_ref().unwrap();
    assert_eq!(version.parser_version, "deterministic-csv-v2");
    let restarted = PgKnowledgeRepository::from_database(&database);
    let detail = restarted
        .get_source_detail(&scope, version.source_id)
        .await
        .unwrap()
        .unwrap();
    let full = detail
        .chunks
        .iter()
        .find(|chunk| chunk.extraction_method == "deterministic_csv_v1")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&full.text).unwrap()["values"][1],
        value
    );
    let slices = detail
        .chunks
        .iter()
        .filter(|chunk| chunk.extraction_method == "deterministic_csv_evidence_v1")
        .collect::<Vec<_>>();
    assert!(slices.len() > 1);
    for chunk in slices {
        assert!(chunk.text.chars().count() <= 1600);
        assert_ne!(chunk.chunk_id, full.chunk_id);
        assert_eq!(chunk.text_hash, sha256_hex(chunk.text.as_bytes()));
        assert_eq!(
            serde_json::to_value(&chunk.locator).unwrap()["start_row"],
            2
        );
    }
    let result = restarted
        .search(
            &scope,
            geo_domain::KnowledgeSearchRequest {
                query: "unique-slice-marker".to_owned(),
                knowledge_release_id: Some(imported.release.unwrap().knowledge_release_id),
                purpose: KnowledgePurpose::Public,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.evidence.len(), 1);
    assert_eq!(result.evidence[0].chunk_id, full.chunk_id);
    assert!(
        restarted
            .get_source_detail(&sibling, version.source_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn csv_upload_and_attachment_import_preserve_evidence_and_reject_partial_parses() {
    let database = connect().await;
    let repository = PgKnowledgeRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "csv-import").await;
    let sibling = seed_sibling_scope(database.pool(), &scope, "csv-other").await;
    let bytes = "\u{feff}Product,Price (USD),Notes\r\nWidget,12.50,\"first\r\nsecond\"\r\nGadget,9,\"quoted \"\"text\"\"\"\r\n".as_bytes();

    for attachment in [false, true] {
        let session = repository
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "catalog.csv".to_owned(),
                    declared_media_type: "Text/CSV; charset=utf-8".to_owned(),
                    expected_size: bytes.len() as u64,
                    expected_sha256: sha256_hex(bytes),
                    purpose: KnowledgePurpose::Internal,
                },
            )
            .await
            .expect("CSV upload session");
        repository
            .put_upload_content(&scope, session.upload_session_id, bytes.to_vec())
            .await
            .expect("CSV upload bytes");
        let imported = if attachment {
            let (object, filename) = repository
                .complete_attachment_upload(&scope, session.upload_session_id, "csv-attachment")
                .await
                .expect("store CSV attachment");
            let item = ImportItem {
                client_item_id: format!("csv:{}", object.object_id),
                kind: SourceKind::Object,
                name: filename,
                purpose: KnowledgePurpose::Internal,
                text: None,
                url: None,
                object_id: Some(object.object_id),
                knowledge_release_id: None,
            };
            let denied = repository
                .import_batch(&sibling, vec![item.clone()])
                .await
                .expect("scoped import");
            assert_eq!(
                denied.items[0].error.as_ref().unwrap().code,
                ErrorCode::NotFound
            );
            let imported = repository
                .import_batch(&scope, vec![item.clone()])
                .await
                .expect("import CSV attachment")
                .items
                .remove(0);
            assert_eq!(
                repository
                    .import_batch(&scope, vec![item])
                    .await
                    .unwrap()
                    .items[0],
                imported
            );
            imported
        } else {
            let imported = repository
                .complete_upload(&scope, session.upload_session_id, "csv-upload")
                .await
                .expect("import ordinary CSV upload");
            assert_eq!(
                repository
                    .complete_upload(&scope, session.upload_session_id, "csv-upload")
                    .await
                    .unwrap(),
                imported
            );
            imported
        };
        assert_eq!(imported.status, geo_domain::ImportStatus::Succeeded);
        let version = imported.source_version.as_ref().unwrap();
        assert_eq!(version.content_sha256, sha256_hex(bytes));
        assert_eq!(version.parser_version, "deterministic-csv-v2");
        assert_eq!(
            imported.release.as_ref().unwrap().pipeline_versions["parsers"],
            json!(["deterministic-csv-v2"])
        );
        let detail = repository
            .get_source_detail(&scope, version.source_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.chunks.len(), 2);
        for (index, chunk) in detail.chunks.iter().enumerate() {
            assert_eq!(chunk.kind, geo_domain::ChunkKind::Table);
            assert_eq!(chunk.text_hash, sha256_hex(chunk.text.as_bytes()));
            let locator = serde_json::to_value(&chunk.locator).unwrap();
            assert_eq!(locator["kind"], "csv");
            assert_eq!(locator["start_row"], index + 2);
            assert_eq!(locator["end_row"], index + 2);
            assert_eq!(locator["start_column"], 1);
            assert_eq!(locator["end_column"], 3);
            assert_eq!(locator["header_row"], 1);
            let table: Value = serde_json::from_str(&chunk.text).unwrap();
            assert_eq!(table["headers"], json!(["Product", "Price (USD)", "Notes"]));
        }
        assert_eq!(
            serde_json::from_str::<Value>(&detail.chunks[0].text).unwrap()["values"],
            json!(["Widget", "12.50", "first\r\nsecond"])
        );
        assert_eq!(
            serde_json::from_str::<Value>(&detail.chunks[1].text).unwrap()["values"],
            json!(["Gadget", "9", "quoted \"text\""])
        );
        let search = repository
            .search(
                &scope,
                geo_domain::KnowledgeSearchRequest {
                    query: "Widget".to_owned(),
                    knowledge_release_id: imported
                        .release
                        .as_ref()
                        .map(|release| release.knowledge_release_id),
                    purpose: KnowledgePurpose::Internal,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert!(
            search
                .evidence
                .iter()
                .any(|evidence| evidence.locator == detail.chunks[0].locator)
        );
        assert!(
            repository
                .get_source_detail(&sibling, version.source_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    let before = repository.overview(&scope).await.unwrap();
    // A valid first record followed by a malformed record must never leave a
    // partially imported source or advance the usable release, even on retry.
    let malformed = b"Product,Price\r\nWidget,12\r\nGadget,\"unterminated";
    for attachment in [false, true] {
        let session = repository
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "malformed.csv".to_owned(),
                    declared_media_type: "text/csv".to_owned(),
                    expected_size: malformed.len() as u64,
                    expected_sha256: sha256_hex(malformed),
                    purpose: KnowledgePurpose::Internal,
                },
            )
            .await
            .unwrap();
        repository
            .put_upload_content(&scope, session.upload_session_id, malformed.to_vec())
            .await
            .unwrap();
        if attachment {
            let (object, filename) = repository
                .complete_attachment_upload(&scope, session.upload_session_id, "malformed-object")
                .await
                .unwrap();
            let item = ImportItem {
                client_item_id: format!("malformed:{}", object.object_id),
                kind: SourceKind::Object,
                name: filename,
                purpose: KnowledgePurpose::Internal,
                text: None,
                url: None,
                object_id: Some(object.object_id),
                knowledge_release_id: None,
            };
            for _ in 0..2 {
                let result = repository
                    .import_batch(&scope, vec![item.clone()])
                    .await
                    .unwrap()
                    .items
                    .remove(0);
                assert_eq!(result.status, geo_domain::ImportStatus::Failed);
                assert_eq!(result.error.unwrap().code, ErrorCode::InvalidRequest);
                assert!(result.source.is_none());
                assert!(result.release.is_none());
            }
        } else {
            for _ in 0..2 {
                assert_eq!(
                    repository
                        .complete_upload(&scope, session.upload_session_id, "malformed-upload")
                        .await
                        .unwrap_err()
                        .code,
                    ErrorCode::InvalidRequest
                );
            }
        }
        assert_eq!(repository.overview(&scope).await.unwrap(), before);
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn attachment_upload_commits_object_without_knowledge_import_and_ordinary_upload_imports() {
    let database = connect().await;
    let repository = PgKnowledgeRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "attachment-upload").await;
    let sibling = seed_sibling_scope(database.pool(), &scope, "attachment-other").await;
    let bytes = b"Attachment-only notes";
    let session = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "notes.txt".to_owned(),
                declared_media_type: "text/plain".to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .expect("create attachment upload session");
    assert_eq!(
        repository
            .put_upload_content(&scope, session.upload_session_id, bytes.to_vec())
            .await
            .expect("upload attachment bytes")
            .state,
        geo_domain::UploadSessionState::Uploaded
    );
    assert!(
        repository
            .complete_attachment_upload(&sibling, session.upload_session_id, "other")
            .await
            .is_err(),
        "another project cannot complete this upload"
    );
    let (object, filename) = repository
        .complete_attachment_upload(&scope, session.upload_session_id, "attachment-key")
        .await
        .expect("complete attachment-only upload");
    assert_eq!(filename, "notes.txt");
    assert_eq!(object.actual_size, bytes.len() as u64);
    assert_eq!(object.sha256, sha256_hex(bytes));
    assert_eq!(object.state, geo_domain::StoredObjectState::Committed);
    assert_eq!(
        repository
            .complete_attachment_upload(&scope, session.upload_session_id, "attachment-key")
            .await
            .expect("idempotent completion"),
        (object.clone(), filename.clone())
    );
    assert!(
        repository
            .complete_attachment_upload(&scope, session.upload_session_id, "different-key")
            .await
            .is_err()
    );
    assert_eq!(
        repository
            .get_attachment_object(&scope, object.object_id)
            .await
            .expect("read attachment"),
        Some((object.clone(), filename))
    );
    assert!(
        repository
            .get_attachment_object(&sibling, object.object_id)
            .await
            .expect("scoped attachment read")
            .is_none()
    );
    assert!(
        repository
            .list_sources(&scope)
            .await
            .expect("source listing")
            .is_empty(),
        "attaching must not create a knowledge source"
    );
    let item = ImportItem {
        client_item_id: format!("agent-attachment:{}", object.object_id),
        kind: SourceKind::Object,
        name: "notes.txt".to_owned(),
        purpose: KnowledgePurpose::Internal,
        text: None,
        url: None,
        object_id: Some(object.object_id),
        knowledge_release_id: None,
    };
    let cross_scope = repository
        .import_batch(&sibling, vec![item.clone()])
        .await
        .expect("cross-project import is an item failure");
    assert_eq!(
        cross_scope.items[0]
            .error
            .as_ref()
            .expect("scoped error")
            .code,
        ErrorCode::NotFound
    );
    let (first, replay) = tokio::join!(
        repository.import_batch(&scope, vec![item.clone()]),
        repository.import_batch(&scope, vec![item.clone()])
    );
    let first = first.expect("first import").items.remove(0);
    assert_eq!(first, replay.expect("concurrent replay").items.remove(0));
    let version = first
        .source_version
        .as_ref()
        .expect("object source version");
    assert_eq!(version.object_id, Some(object.object_id));
    assert_eq!(version.object_version, Some(object.object_version));
    assert_eq!(version.content_sha256, object.sha256);
    assert_eq!(
        first.source.as_ref().expect("object source").locator["object_id"],
        object.object_id.to_string()
    );
    assert_eq!(
        repository
            .list_sources(&scope)
            .await
            .expect("one source")
            .len(),
        1
    );
    let changed = ImportItem {
        purpose: KnowledgePurpose::Public,
        ..item.clone()
    };
    assert_eq!(
        repository
            .import_batch(&scope, vec![changed])
            .await
            .expect("purpose conflict")
            .items[0]
            .error
            .as_ref()
            .expect("conflict")
            .code,
        ErrorCode::Conflict
    );
    let unsupported_bytes = b"opaque bytes";
    let unsupported_session = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "unsupported.pdf".to_owned(),
                declared_media_type: "application/pdf".to_owned(),
                expected_size: unsupported_bytes.len() as u64,
                expected_sha256: sha256_hex(unsupported_bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .expect("unsupported upload session");
    repository
        .put_upload_content(
            &scope,
            unsupported_session.upload_session_id,
            unsupported_bytes.to_vec(),
        )
        .await
        .expect("unsupported upload bytes");
    let (unsupported_object, _) = repository
        .complete_attachment_upload(&scope, unsupported_session.upload_session_id, "unsupported")
        .await
        .expect("unsupported bytes still stored");
    let partial = repository
        .import_batch(
            &scope,
            vec![
                item.clone(),
                ImportItem {
                    client_item_id: format!("agent-attachment:{}", unsupported_object.object_id),
                    kind: SourceKind::Object,
                    name: "unsupported.pdf".to_owned(),
                    purpose: KnowledgePurpose::Internal,
                    text: None,
                    url: None,
                    object_id: Some(unsupported_object.object_id),
                    knowledge_release_id: None,
                },
            ],
        )
        .await
        .expect("per-item import result");
    assert_eq!(partial.items[0], first);
    assert_eq!(
        partial.items[1].error.as_ref().expect("adapter error").code,
        ErrorCode::CapabilityMissing
    );
    assert_eq!(
        repository
            .list_sources(&scope)
            .await
            .expect("no failed source")
            .len(),
        1
    );

    // The existing knowledge completion path has the same nullable-blob join;
    // exercise its row lock too and verify that it still imports normally.
    let import_bytes = b"Knowledge import text";
    let import_session = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "knowledge.txt".to_owned(),
                declared_media_type: "text/plain".to_owned(),
                expected_size: import_bytes.len() as u64,
                expected_sha256: sha256_hex(import_bytes),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .expect("create knowledge upload");
    repository
        .put_upload_content(
            &scope,
            import_session.upload_session_id,
            import_bytes.to_vec(),
        )
        .await
        .expect("upload knowledge bytes");
    let imported = repository
        .complete_upload(&scope, import_session.upload_session_id, "knowledge-key")
        .await
        .expect("complete ordinary knowledge import");
    assert!(imported.source.is_some());
    assert!(imported.release.is_some());
    assert_eq!(
        repository
            .complete_upload(&scope, import_session.upload_session_id, "knowledge-key")
            .await
            .expect("idempotent knowledge replay"),
        imported
    );
    assert_eq!(
        repository
            .list_sources(&scope)
            .await
            .expect("sources after import")
            .len(),
        2
    );
    let imported_object_id = imported
        .source_version
        .as_ref()
        .and_then(|version| version.object_id)
        .expect("imported object ID");
    assert!(
        repository
            .get_attachment_object(&scope, imported_object_id)
            .await
            .expect("knowledge object is not an agent attachment")
            .is_none()
    );
}

fn message(content: &str) -> AppendMessage {
    AppendMessage {
        content: content.to_owned(),
        attachments: Vec::new(),
        metadata: Value::Null,
    }
}

async fn create_conversation(
    repository: &PgAgentRepository,
    scope: &TenantScope,
) -> geo_domain::Conversation {
    repository
        .create_conversation(scope, None, CreateConversation::default())
        .await
        .expect("create conversation")
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_conversations_are_isolated_by_tenant_and_project() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "isolation").await;
    let conversation = create_conversation(&repository, &scope).await;

    assert_eq!(
        repository
            .list_conversations(&scope)
            .await
            .expect("scoped list")
            .len(),
        1
    );
    assert!(
        repository
            .get_conversation(&scope, conversation.id)
            .await
            .expect("scoped read")
            .is_some()
    );

    let sibling = seed_sibling_scope(database.pool(), &scope, "isolation-sibling").await;
    assert!(
        repository
            .list_conversations(&sibling)
            .await
            .expect("sibling list")
            .is_empty()
    );
    assert!(
        repository
            .get_conversation(&sibling, conversation.id)
            .await
            .expect("sibling read")
            .is_none()
    );
    assert_eq!(
        repository
            .replay_events(&sibling, conversation.id, Some(0))
            .await
            .expect_err("sibling replay")
            .code,
        ErrorCode::NotFound
    );

    let other_tenant = seed_scope(database.pool(), "isolation-tenant").await;
    assert!(
        repository
            .list_conversations(&other_tenant)
            .await
            .expect("other tenant list")
            .is_empty()
    );
    assert!(
        repository
            .get_conversation(&other_tenant, conversation.id)
            .await
            .expect("other tenant read")
            .is_none()
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_message_submission_is_idempotent_and_rejects_a_changed_request() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "idempotency").await;
    let conversation = create_conversation(&repository, &scope).await;
    let capability = RuntimeCapability::missing("runtime missing");

    let first = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "same-key".to_owned(),
            "body-a".to_owned(),
            capability.clone(),
        )
        .await
        .expect("first submission");
    let replay = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "same-key".to_owned(),
            "body-a".to_owned(),
            capability.clone(),
        )
        .await
        .expect("same-key replay");
    assert_eq!(first, replay);
    assert_eq!(replay.message.id, first.message.id);

    let conflict = repository
        .append_message(
            &scope,
            conversation.id,
            message("different"),
            "same-key".to_owned(),
            "body-b".to_owned(),
            capability,
        )
        .await
        .expect_err("same key with a different request");
    assert_eq!(conflict.code, ErrorCode::Conflict);

    // The replay returned the stored acceptance instead of writing new rows.
    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    assert_eq!(detail.messages.len(), 1);
    assert_eq!(detail.turns.len(), 1);
    assert_eq!(detail.runs.len(), 1);
    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(events.len(), 4);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_run_state_machine_fails_without_a_runtime_and_cancels_with_one() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "state-machine").await;
    let conversation = create_conversation(&repository, &scope).await;

    let failed = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "failed-key".to_owned(),
            "failed-body".to_owned(),
            RuntimeCapability::missing("runtime missing"),
        )
        .await
        .expect("submission without a runtime");
    assert_eq!(failed.turn.status, TurnStatus::Failed);
    assert_eq!(failed.run.status, RunStatus::Failed);
    assert_eq!(
        failed.run.error.as_ref().map(|error| error.code),
        Some(ErrorCode::CapabilityMissing)
    );
    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert!(events.iter().any(|event| event.event_type == "run.failed"));
    assert!(
        events
            .windows(2)
            .all(|window| window[0].sequence < window[1].sequence)
    );

    // A failed run releases the conversation, so the next turn is accepted.
    let queued = repository
        .append_message(
            &scope,
            conversation.id,
            message("again"),
            "queued-key".to_owned(),
            "queued-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("submission with a runtime");
    assert_eq!(queued.turn.status, TurnStatus::Queued);
    assert_eq!(queued.run.status, RunStatus::Queued);

    // Only one turn may be active per conversation.
    let concurrent = repository
        .append_message(
            &scope,
            conversation.id,
            message("too soon"),
            "concurrent-key".to_owned(),
            "concurrent-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect_err("active turn");
    assert_eq!(concurrent.code, ErrorCode::Conflict);

    let cancelled = repository
        .cancel_turn(&scope, queued.turn.id)
        .await
        .expect("cancel");
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(cancelled.cancel_version, 1);
    let cancelled_again = repository
        .cancel_turn(&scope, queued.turn.id)
        .await
        .expect("cancel is idempotent");
    assert_eq!(cancelled_again.cancel_version, 1);
    assert_eq!(cancelled_again.status, RunStatus::Cancelled);

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    assert_eq!(detail.turns.len(), 2);
    assert_eq!(detail.turns[1].status, TurnStatus::Cancelled);
    assert_eq!(detail.runs[1].status, RunStatus::Cancelled);
    assert_eq!(
        detail
            .turns
            .iter()
            .filter(|turn| matches!(turn.status, TurnStatus::Queued | TurnStatus::Running))
            .count(),
        0
    );

    let sibling = seed_sibling_scope(database.pool(), &scope, "state-machine-sibling").await;
    assert_eq!(
        repository
            .cancel_turn(&sibling, queued.turn.id)
            .await
            .expect_err("cross-project cancel")
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repository
            .cancel_turn(&scope, geo_domain::TurnId::from(Uuid::new_v4()))
            .await
            .expect_err("unknown turn")
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_event_sequence_is_monotonic_under_concurrent_writers() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "concurrent-events").await;
    let conversation = create_conversation(&repository, &scope).await;

    let concurrency = 5usize;
    let mut handles = Vec::with_capacity(concurrency);
    for index in 0..concurrency {
        let repository = Arc::clone(&repository);
        let scope = scope.clone();
        let conversation_id = conversation.id;
        handles.push(tokio::spawn(async move {
            repository
                .append_message(
                    &scope,
                    conversation_id,
                    message(&format!("message {index}")),
                    format!("concurrent-key-{index}"),
                    format!("concurrent-body-{index}"),
                    RuntimeCapability::missing("runtime missing"),
                )
                .await
        }));
    }
    for handle in handles {
        handle
            .await
            .expect("join append")
            .expect("concurrent append");
    }

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    let expected_events = 1 + 3 * u64::try_from(concurrency).expect("small concurrency");
    assert_eq!(events.len() as u64, expected_events);
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (1..=expected_events).collect::<Vec<_>>()
    );

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    let expected_messages = u64::try_from(concurrency).expect("small concurrency");
    assert_eq!(detail.messages.len(), concurrency);
    assert_eq!(
        detail
            .messages
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        (1..=expected_messages).collect::<Vec<_>>()
    );
    assert_eq!(detail.turns.len(), concurrency);
    assert_eq!(
        detail
            .turns
            .iter()
            .filter(|turn| turn.previous_turn_id.is_none())
            .count(),
        1
    );
    for turn in &detail.turns {
        if let Some(previous) = turn.previous_turn_id {
            assert_ne!(previous, turn.id);
            assert!(
                detail
                    .turns
                    .iter()
                    .any(|candidate| candidate.id == previous)
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_replay_after_restart_returns_the_durable_events() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "replay").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "replay-key".to_owned(),
            "replay-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let before_restart = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events before restart");

    // A new process only has the pool; nothing is carried over in memory.
    let restarted = PgAgentRepository::new(pool.clone());
    let after_restart = restarted
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events after restart");
    assert_eq!(after_restart, before_restart);
    assert_eq!(after_restart.len(), 3);
    assert_eq!(after_restart[0].event_type, "conversation.created");
    assert_eq!(after_restart[2].event_type, "turn.accepted");

    let cursor = after_restart[0].sequence;
    let incremental = restarted
        .replay_events(&scope, conversation.id, Some(cursor))
        .await
        .expect("incremental replay");
    assert_eq!(incremental, after_restart[1..].to_vec());

    let replayed_submission = restarted
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "replay-key".to_owned(),
            "replay-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("idempotent replay after restart");
    assert_eq!(replayed_submission, acceptance);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_history_restores_completed_pairs_after_repository_restart() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "history").await;
    let conversation = create_conversation(&repository, &scope).await;
    let first = repository
        .append_message(
            &scope,
            conversation.id,
            message("Remember the warranty"),
            "history-first".into(),
            "history-first-body".into(),
            RuntimeCapability::available("deno_core", None),
        )
        .await
        .unwrap();
    repository
        .begin_run(&scope, first.run.id)
        .await
        .unwrap()
        .unwrap();
    repository
        .finish_run(
            &scope,
            first.run.id,
            RunCompletion::Succeeded {
                content: "The warranty is two years.".into(),
                metadata: json!({"internal": "not model history"}),
            },
        )
        .await
        .unwrap()
        .unwrap();
    let second = repository
        .append_message(
            &scope,
            conversation.id,
            message("What did you say?"),
            "history-second".into(),
            "history-second-body".into(),
            RuntimeCapability::available("deno_core", None),
        )
        .await
        .unwrap();
    let restarted = PgAgentRepository::new(pool.clone());
    let detail = restarted
        .get_conversation(&scope, conversation.id)
        .await
        .unwrap()
        .unwrap();
    let restored = detail.turn_input(second.run.id).unwrap();
    assert_eq!(
        restored,
        restarted
            .load_turn_input(&scope, conversation.id, second.run.id)
            .await
            .unwrap()
    );
    assert_eq!(restored.history.len(), 2);
    assert_eq!(restored.history[0].message_id, first.message.id);
    assert_eq!(restored.history[0].role, MessageRole::User);
    assert_eq!(restored.history[1].role, MessageRole::Assistant);
    assert_eq!(restored.history[1].root_message_id, first.message.id);
    assert_eq!(restored.history[1].content, "The warranty is two years.");
    assert_eq!(restored.history_omitted_turns, 0);
    assert!(detail.turn_input(first.run.id).unwrap().history.is_empty());
    let again = repository
        .get_conversation(&scope, conversation.id)
        .await
        .unwrap()
        .unwrap()
        .turn_input(second.run.id)
        .unwrap();
    assert_eq!(restored, again);
    let sibling = seed_sibling_scope(&pool, &scope, "history-sibling").await;
    assert!(
        restarted
            .get_conversation(&sibling, conversation.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        restarted
            .load_turn_input(&sibling, conversation.id, second.run.id)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_bounded_history_preserves_cutoffs_success_and_exact_omissions() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "bounded-history").await;
    let conversation = create_conversation(&repository, &scope).await;
    let mut first_run = None;
    let mut latest_turn = None;
    for index in 0..28 {
        let accepted = repository
            .append_message(
                &scope,
                conversation.id,
                message(&format!("question {index}")),
                format!("bounded-question-{index}"),
                format!("bounded-body-{index}"),
                RuntimeCapability::available("deno_core", None),
            )
            .await
            .unwrap();
        repository
            .begin_run(&scope, accepted.run.id)
            .await
            .unwrap()
            .unwrap();
        repository
            .finish_run(
                &scope,
                accepted.run.id,
                RunCompletion::Succeeded {
                    content: format!("answer {index}"),
                    metadata: Value::Null,
                },
            )
            .await
            .unwrap()
            .unwrap();
        if index == 0 {
            first_run = Some(accepted.run.id);
        }
        latest_turn = Some(accepted.turn.id);
    }
    let failed = repository
        .append_message(
            &scope,
            conversation.id,
            message("failed prompt"),
            "bounded-failed".into(),
            "bounded-failed-body".into(),
            RuntimeCapability::available("deno_core", None),
        )
        .await
        .unwrap();
    repository.begin_run(&scope, failed.run.id).await.unwrap();
    repository
        .finish_run(
            &scope,
            failed.run.id,
            RunCompletion::Failed {
                error: geo_domain::AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "model unavailable",
                ),
            },
        )
        .await
        .unwrap();
    let current = repository
        .append_message(
            &scope,
            conversation.id,
            message("current prompt"),
            "bounded-current".into(),
            "bounded-current-body".into(),
            RuntimeCapability::available("deno_core", None),
        )
        .await
        .unwrap();
    let restarted = PgAgentRepository::new(pool);
    let loaded = restarted
        .load_turn_input(&scope, conversation.id, current.run.id)
        .await
        .unwrap();
    let detail = restarted
        .get_conversation(&scope, conversation.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded, detail.turn_input(current.run.id).unwrap());
    assert_eq!(loaded.prompt, "current prompt");
    assert_eq!(loaded.history.len(), 40);
    assert_eq!(loaded.history_omitted_turns, 8);
    assert_eq!(loaded.history[0].content, "question 8");
    assert_eq!(loaded.history[39].content, "answer 27");
    assert!(
        restarted
            .load_turn_input(&scope, conversation.id, first_run.unwrap())
            .await
            .unwrap()
            .history
            .is_empty()
    );

    // A legacy/corrupt whitespace-only answer is not a valid pair even when
    // the whitespace is non-ASCII; the scoped bounded read must agree with
    // domain reconstruction on both selection and omission count.
    sqlx::query("UPDATE agent_messages SET content = $1 WHERE turn_id = $2 AND role = 'assistant'")
        .bind("\u{00a0}\u{2003}")
        .bind(latest_turn.unwrap().as_uuid())
        .execute(restarted.pool())
        .await
        .unwrap();
    let loaded = restarted
        .load_turn_input(&scope, conversation.id, current.run.id)
        .await
        .unwrap();
    assert_eq!(
        loaded,
        restarted
            .get_conversation(&scope, conversation.id)
            .await
            .unwrap()
            .unwrap()
            .turn_input(current.run.id)
            .unwrap()
    );
    assert_eq!(loaded.history_omitted_turns, 7);
    assert_eq!(loaded.history[0].content, "question 7");
    assert_eq!(loaded.history[39].content, "answer 26");

    // A second answer recorded after the current prompt still invalidates
    // the old turn: the cutoff applies to valid pairs, not the answer count.
    let duplicate_turn: Uuid = sqlx::query_scalar(
        "SELECT turn_id FROM agent_messages WHERE conversation_id = $1 AND content = 'answer 26'",
    )
    .bind(conversation.id.as_uuid())
    .fetch_one(restarted.pool())
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO agent_messages
                (message_id, conversation_id, operator_id, tenant_id, project_id,
                 turn_id, role, content, sequence)
            SELECT $1, conversation_id, operator_id, tenant_id, project_id,
                   turn_id, role, 'duplicate answer',
                   (SELECT MAX(sequence) + 1 FROM agent_messages WHERE conversation_id = $2)
              FROM agent_messages
             WHERE turn_id = $3 AND role = 'assistant'"#,
    )
    .bind(Uuid::new_v4())
    .bind(conversation.id.as_uuid())
    .bind(duplicate_turn)
    .execute(restarted.pool())
    .await
    .unwrap();
    let loaded = restarted
        .load_turn_input(&scope, conversation.id, current.run.id)
        .await
        .unwrap();
    assert_eq!(
        loaded,
        restarted
            .get_conversation(&scope, conversation.id)
            .await
            .unwrap()
            .unwrap()
            .turn_input(current.run.id)
            .unwrap()
    );
    assert_eq!(loaded.history_omitted_turns, 6);
    assert_eq!(loaded.history[0].content, "question 6");
    assert_eq!(loaded.history[39].content, "answer 25");
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_bounded_history_stops_at_oversized_recent_pair() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "bounded-history-bytes").await;
    let conversation = create_conversation(&repository, &scope).await;
    for index in 0..25 {
        let accepted = repository
            .append_message(
                &scope,
                conversation.id,
                message(&format!("question {index}")),
                format!("bounded-bytes-{index}"),
                format!("bounded-bytes-body-{index}"),
                RuntimeCapability::available("deno_core", None),
            )
            .await
            .unwrap();
        repository.begin_run(&scope, accepted.run.id).await.unwrap();
        repository
            .finish_run(
                &scope,
                accepted.run.id,
                RunCompletion::Succeeded {
                    content: if index == 23 {
                        "界".repeat(45_000)
                    } else {
                        format!("answer {index}")
                    },
                    metadata: Value::Null,
                },
            )
            .await
            .unwrap();
    }
    let current = repository
        .append_message(
            &scope,
            conversation.id,
            message("current prompt"),
            "bounded-bytes-current".into(),
            "bounded-bytes-current-body".into(),
            RuntimeCapability::available("deno_core", None),
        )
        .await
        .unwrap();
    let loaded = PgAgentRepository::new(pool)
        .load_turn_input(&scope, conversation.id, current.run.id)
        .await
        .unwrap();
    assert_eq!(loaded.history.len(), 2);
    assert_eq!(loaded.history[0].content, "question 24");
    assert_eq!(loaded.history_omitted_turns, 24);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_checkpoints_survive_a_restart_and_reject_a_changed_input() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "checkpoint").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "checkpoint-key".to_owned(),
            "checkpoint-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;
    let checkpoint = |input_hash: &str, cursor: i64| StoreCheckpoint {
        checkpoint_scope: "loop".to_owned(),
        step_key: "collect".to_owned(),
        input_hash: input_hash.to_owned(),
        result_ref: Some(ObjectRef {
            object_id: "object-1".to_owned(),
            version: Some("3".to_owned()),
        }),
        state: json!({"cursor": cursor}),
    };

    let stored = repository
        .store_checkpoint(&scope, run_id, checkpoint("digest-a", 3))
        .await
        .expect("store checkpoint");
    assert_eq!(stored.version, 1);
    assert_eq!(stored.run_id, run_id);
    assert_eq!(stored.conversation_id, conversation.id);

    let restarted = PgAgentRepository::new(pool.clone());
    let restored = restarted
        .load_checkpoint(&scope, run_id, "loop", "collect")
        .await
        .expect("load checkpoint")
        .expect("checkpoint is durable");
    assert_eq!(restored, stored);

    let refreshed = restarted
        .store_checkpoint(&scope, run_id, checkpoint("digest-a", 4))
        .await
        .expect("refresh checkpoint");
    assert_eq!(refreshed.id, stored.id);
    assert_eq!(refreshed.version, 2);
    assert_eq!(refreshed.state, json!({"cursor": 4}));

    let conflict = restarted
        .store_checkpoint(&scope, run_id, checkpoint("digest-b", 5))
        .await
        .expect_err("changed input digest");
    assert_eq!(conflict.code, ErrorCode::Conflict);

    assert!(
        restarted
            .load_checkpoint(&scope, run_id, "loop", "missing-step")
            .await
            .expect("missing step")
            .is_none()
    );
    let sibling = seed_sibling_scope(&pool, &scope, "checkpoint-sibling").await;
    assert_eq!(
        restarted
            .load_checkpoint(&sibling, run_id, "loop", "collect")
            .await
            .expect_err("cross-project checkpoint")
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_tool_call_ledger_appends_idempotently_per_tool_call() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "ledger").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "ledger-key".to_owned(),
            "ledger-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;
    let record = |tool_call_id: &str, arguments_hash: &str| RecordToolCall {
        run_id,
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "geo.publish".to_owned(),
        arguments_hash: arguments_hash.to_owned(),
        idempotency_key_hash: "ledger-idempotency".to_owned(),
        permission: ToolCallDecision::Allowed,
        budget: ToolCallDecision::Allowed,
        intent: json!({"document_id": "doc-1"}),
        attempt_count: 0,
        result_ref: Some(ObjectRef {
            object_id: "object-2".to_owned(),
            version: None,
        }),
        outcome: ToolCallOutcome::Intent,
        cost_minor: Some(12),
        currency: Some("CNY".to_owned()),
    };

    let appended = repository
        .append_tool_call(&scope, record("call-1", "args-a"))
        .await
        .expect("append tool call");
    assert_eq!(appended.run_id, run_id);
    assert_eq!(appended.turn_id, acceptance.turn.id);
    assert_eq!(appended.conversation_id, conversation.id);
    assert_eq!(appended.idempotency_key_hash, "ledger-idempotency");

    let replayed = repository
        .append_tool_call(&scope, record("call-1", "args-a"))
        .await
        .expect("replay tool call");
    assert_eq!(replayed, appended);

    let conflict = repository
        .append_tool_call(&scope, record("call-1", "args-b"))
        .await
        .expect_err("different arguments");
    assert_eq!(conflict.code, ErrorCode::Conflict);

    let listed = PgAgentRepository::new(pool.clone())
        .list_tool_calls(&scope, run_id)
        .await
        .expect("list tool calls");
    assert_eq!(listed, vec![appended.clone()]);
    assert_eq!(listed[0].attempt_count, 0);
    assert_eq!(listed[0].cost_minor, Some(12));
    assert_eq!(listed[0].currency.as_deref(), Some("CNY"));

    let sibling = seed_sibling_scope(&pool, &scope, "ledger-sibling").await;
    assert_eq!(
        repository
            .list_tool_calls(&sibling, run_id)
            .await
            .expect_err("cross-project list")
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repository
            .append_tool_call(&sibling, record("call-2", "args-a"))
            .await
            .expect_err("cross-project append")
            .code,
        ErrorCode::NotFound
    );
}

fn rust_tool_intent(run_id: geo_domain::RunId, tool_call_id: &str) -> RecordToolCall {
    RecordToolCall {
        run_id,
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "geo.knowledge.search".to_owned(),
        arguments_hash: "arguments-hash".to_owned(),
        idempotency_key_hash: "stable-key-hash".to_owned(),
        permission: ToolCallDecision::Allowed,
        budget: ToolCallDecision::Allowed,
        intent: json!({"tool": "search"}),
        attempt_count: 0,
        result_ref: None,
        outcome: ToolCallOutcome::Intent,
        cost_minor: None,
        currency: None,
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_rust_tool_lifecycle_serializes_replays_attempt_and_late_outcome() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "rust-tool-ledger").await;
    let sibling = seed_sibling_scope(&pool, &scope, "rust-tool-ledger-sibling").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "rust-ledger-key".to_owned(),
            "rust-ledger-body".to_owned(),
            RuntimeCapability::available("test", None),
        )
        .await
        .expect("accept");
    let run_id = acceptance.run.id;
    let input = rust_tool_intent(run_id, "call-1");
    assert_eq!(
        repository
            .begin_tool_call(&scope, input.clone())
            .await
            .expect_err("queued")
            .code,
        ErrorCode::Conflict
    );
    repository
        .begin_run(&scope, run_id)
        .await
        .expect("claim run");
    let writes = (0..8)
        .map(|_| {
            let repository = repository.clone();
            let scope = scope.clone();
            let input = input.clone();
            tokio::spawn(async move {
                repository
                    .begin_tool_call(&scope, input)
                    .await
                    .expect("race intent")
            })
        })
        .collect::<Vec<_>>();
    let mut initial = None;
    for write in writes {
        let entry = write.await.expect("join");
        assert_eq!(entry.attempt_count, 0);
        assert_eq!(entry.outcome, ToolCallOutcome::Intent);
        if let Some(previous) = &initial {
            assert_eq!(previous, &entry);
        } else {
            initial = Some(entry);
        }
    }
    let original = initial.expect("intent");
    let identity = ToolCallIdentity::from_record(&input);
    let collision = RecordToolCall {
        tool_name: "geo.other".to_owned(),
        ..input.clone()
    };
    assert_eq!(
        repository
            .begin_tool_call(&scope, collision)
            .await
            .expect_err("tool name")
            .code,
        ErrorCode::Conflict
    );
    let collision = RecordToolCall {
        budget: ToolCallDecision::Denied,
        ..input.clone()
    };
    assert_eq!(
        repository
            .begin_tool_call(&scope, collision)
            .await
            .expect_err("decision")
            .code,
        ErrorCode::Conflict
    );
    let collision = RecordToolCall {
        intent: json!({"tool": "different"}),
        ..input.clone()
    };
    assert_eq!(
        repository
            .begin_tool_call(&scope, collision)
            .await
            .expect_err("intent")
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .attempt_tool_call(&sibling, identity.clone())
            .await
            .expect_err("other project")
            .code,
        ErrorCode::NotFound
    );
    let claims = (0..8)
        .map(|_| {
            let repository = repository.clone();
            let scope = scope.clone();
            let identity = identity.clone();
            tokio::spawn(async move {
                repository
                    .attempt_tool_call(&scope, identity)
                    .await
                    .expect("attempt race")
            })
        })
        .collect::<Vec<_>>();
    let mut won = 0;
    for claim in claims {
        won += usize::from(claim.await.expect("join"));
    }
    assert_eq!(won, 1);
    let entry = repository
        .list_tool_calls(&scope, run_id)
        .await
        .expect("list");
    assert_eq!(entry.len(), 1);
    assert_eq!(entry[0].id, original.id);
    assert_eq!(entry[0].attempt_count, 1);
    let changed_identity = ToolCallIdentity {
        idempotency_key_hash: "changed".to_owned(),
        ..identity.clone()
    };
    assert_eq!(
        repository
            .finish_tool_call(&scope, changed_identity, ToolCallOutcome::Unknown)
            .await
            .expect_err("identity mismatch")
            .code,
        ErrorCode::Conflict
    );
    repository
        .cancel_turn(&scope, acceptance.turn.id)
        .await
        .expect("cancel");
    let result = repository
        .finish_tool_call(&scope, identity.clone(), ToolCallOutcome::Unknown)
        .await
        .expect("late result");
    assert_eq!(result.outcome, ToolCallOutcome::Unknown);
    assert_eq!(result.attempt_count, 1);
    assert_eq!(
        PgAgentRepository::new(pool.clone())
            .finish_tool_call(&scope, identity.clone(), ToolCallOutcome::Unknown)
            .await
            .expect("restart replay"),
        result
    );
    assert!(
        !repository
            .attempt_tool_call(&scope, identity.clone())
            .await
            .expect("no second attempt")
    );
    assert_eq!(
        repository
            .finish_tool_call(&scope, identity.clone(), ToolCallOutcome::Succeeded)
            .await
            .expect_err("unknown must remain unknown")
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .begin_tool_call(&scope, input)
            .await
            .expect("replay after cancel"),
        result
    );
    assert_eq!(
        repository
            .finish_tool_call(&sibling, identity, ToolCallOutcome::Unknown)
            .await
            .expect_err("cross scope")
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_rust_tool_cancellation_before_attempt_rejects_new_side_effects() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "rust-tool-cancel").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "rust-cancel-key".to_owned(),
            "rust-cancel-body".to_owned(),
            RuntimeCapability::available("test", None),
        )
        .await
        .expect("accept");
    let run_id = acceptance.run.id;
    repository
        .begin_run(&scope, run_id)
        .await
        .expect("claim run");
    let input = rust_tool_intent(run_id, "call-before-cancel");
    repository
        .begin_tool_call(&scope, input.clone())
        .await
        .expect("intent");
    repository
        .cancel_turn(&scope, acceptance.turn.id)
        .await
        .expect("cancel");
    assert_eq!(
        repository
            .attempt_tool_call(&scope, ToolCallIdentity::from_record(&input))
            .await
            .expect_err("cannot start after cancel")
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .begin_tool_call(&scope, rust_tool_intent(run_id, "call-after-cancel"))
            .await
            .expect_err("no new intent")
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .finish_tool_call(
                &scope,
                ToolCallIdentity::from_record(&input),
                ToolCallOutcome::Failed
            )
            .await
            .expect_err("cannot finish unattempted call")
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .list_tool_calls(&scope, run_id)
            .await
            .expect("unchanged")
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_rust_tool_attempt_respects_stored_denied_decisions_and_legacy_unknown() {
    let database = connect().await;
    let repository = PgAgentRepository::from_database(&database);
    let scope = seed_scope(database.pool(), "rust-tool-denied").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "rust-denied-key".to_owned(),
            "rust-denied-body".to_owned(),
            RuntimeCapability::available("test", None),
        )
        .await
        .expect("accept");
    let run_id = acceptance.run.id;
    repository.begin_run(&scope, run_id).await.expect("running");
    for (tool_id, permission, budget) in [
        (
            "permission-denied",
            ToolCallDecision::Denied,
            ToolCallDecision::Allowed,
        ),
        (
            "budget-denied",
            ToolCallDecision::Allowed,
            ToolCallDecision::Denied,
        ),
    ] {
        let input = RecordToolCall {
            permission,
            budget,
            ..rust_tool_intent(run_id, tool_id)
        };
        repository
            .begin_tool_call(&scope, input.clone())
            .await
            .expect("denied intent");
        assert_eq!(
            repository
                .attempt_tool_call(&scope, ToolCallIdentity::from_record(&input))
                .await
                .expect_err("must not claim a denied attempt")
                .code,
            ErrorCode::Forbidden
        );
    }
    let legacy_denied = RecordToolCall {
        permission: ToolCallDecision::Denied,
        cost_minor: Some(4),
        ..rust_tool_intent(run_id, "legacy-denied")
    };
    repository
        .append_tool_call(&scope, legacy_denied.clone())
        .await
        .expect("legacy denied");
    assert_eq!(
        repository
            .attempt_tool_call(&scope, ToolCallIdentity::from_record(&legacy_denied))
            .await
            .expect_err("legacy denied attempt")
            .code,
        ErrorCode::Forbidden
    );
    let legacy_unknown = RecordToolCall {
        attempt_count: 1,
        outcome: ToolCallOutcome::Unknown,
        ..rust_tool_intent(run_id, "legacy-unknown")
    };
    repository
        .append_tool_call(&scope, legacy_unknown.clone())
        .await
        .expect("legacy unknown");
    assert!(
        !repository
            .attempt_tool_call(&scope, ToolCallIdentity::from_record(&legacy_unknown))
            .await
            .expect("unknown is not claimable")
    );
    assert!(
        repository
            .list_tool_calls(&scope, run_id)
            .await
            .expect("ledger")
            .iter()
            .all(|entry| entry.outcome != ToolCallOutcome::Attempted)
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_attachment_only_message_round_trips_and_links_the_previous_turn() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "attachments").await;
    let conversation = create_conversation(&repository, &scope).await;
    let first = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "attachment-first".to_owned(),
            "attachment-first-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("first message");

    let attachment = AttachmentReference {
        attachment_id: AttachmentId::from(Uuid::new_v4()),
        object_id: "object-attachment-1".to_owned(),
        filename: "report.pdf".to_owned(),
        media_type: Some("application/pdf".to_owned()),
        size_bytes: Some(2048),
        sha256: Some("a".repeat(64)),
        object_version: Some("2".to_owned()),
    };
    // An attachment-only message would be accepted, but the first turn is still
    // active, so the conflict has to win before the attachment rules are tested.
    let active = repository
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: "  ".to_owned(),
                attachments: vec![attachment.clone()],
                metadata: Value::Null,
            },
            "attachment-second".to_owned(),
            "attachment-second-body".to_owned(),
            RuntimeCapability::missing("runtime missing"),
        )
        .await
        .expect_err("active turn");
    assert_eq!(active.code, ErrorCode::Conflict);

    repository
        .cancel_turn(&scope, first.turn.id)
        .await
        .expect("cancel first turn");
    let second = repository
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: "  ".to_owned(),
                attachments: vec![attachment.clone()],
                metadata: json!({"source": "upload"}),
            },
            "attachment-second".to_owned(),
            "attachment-second-body".to_owned(),
            RuntimeCapability::missing("runtime missing"),
        )
        .await
        .expect("attachment-only message");
    assert_eq!(second.message.content, "");
    assert_eq!(second.message.attachments, vec![attachment.clone()]);
    assert_eq!(second.message.sequence, 2);
    assert_eq!(second.turn.previous_turn_id, Some(first.turn.id));
    assert_eq!(second.turn.root_message_id, second.message.id);

    let restarted = PgAgentRepository::new(pool.clone());
    let detail = restarted
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    assert_eq!(detail.messages.len(), 2);
    let restored = &detail.messages[1];
    assert_eq!(restored.content, "");
    assert_eq!(restored.attachments, vec![attachment]);
    assert_eq!(restored.turn_id, Some(second.turn.id));
    assert_eq!(restored.metadata, json!({"source": "upload"}));
    assert_eq!(restored.sequence, 2);
    assert_eq!(detail.turns.len(), 2);
    assert_eq!(detail.turns[1].previous_turn_id, Some(first.turn.id));
    assert_eq!(detail.runs.len(), 2);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_cancel_racing_completion_keeps_a_single_terminal_state() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "cancel-race").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "cancel-race-key".to_owned(),
            "cancel-race-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;
    let turn_id = acceptance.turn.id;

    // A worker holding the run row finishes only while the run is still open;
    // the same lock is what cancel takes, so exactly one of them can decide.
    let completion = tokio::spawn(async move {
        let mut transaction = pool.begin().await.expect("completion transaction");
        let status: String =
            sqlx::query_scalar("SELECT status FROM agent_runs WHERE run_id = $1 FOR UPDATE")
                .bind(run_id.as_uuid())
                .fetch_one(&mut *transaction)
                .await
                .expect("lock run");
        if status != "queued" && status != "running" {
            transaction.rollback().await.expect("rollback completion");
            return;
        }
        sqlx::query(
            "UPDATE agent_runs SET status = 'succeeded', updated_at = now() WHERE run_id = $1",
        )
        .bind(run_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .expect("complete run");
        sqlx::query(
            "UPDATE agent_turns SET status = 'succeeded', updated_at = now() WHERE turn_id = $1",
        )
        .bind(turn_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .expect("complete turn");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        transaction.commit().await.expect("commit completion");
    });

    let cancelled = repository
        .cancel_turn(&scope, turn_id)
        .await
        .expect("cancel turn");
    completion.await.expect("join completion");

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    let run = detail
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .expect("run");
    let turn = detail
        .turns
        .iter()
        .find(|turn| turn.id == turn_id)
        .expect("turn");
    assert!(matches!(
        run.status,
        RunStatus::Succeeded | RunStatus::Cancelled
    ));
    assert_eq!(run.status, cancelled.status);
    assert_eq!(
        turn.status == TurnStatus::Succeeded,
        run.status == RunStatus::Succeeded
    );

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    let cancelled_events = events
        .iter()
        .filter(|event| event.event_type == "run.cancelled")
        .count();
    assert_eq!(
        cancelled_events,
        usize::from(run.status == RunStatus::Cancelled)
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_concurrent_cancels_emit_one_cancellation() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "cancel-concurrent").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "cancel-concurrent-key".to_owned(),
            "cancel-concurrent-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");

    let mut handles = Vec::new();
    for _ in 0..3 {
        let repository = Arc::clone(&repository);
        let scope = scope.clone();
        let turn_id = acceptance.turn.id;
        handles.push(tokio::spawn(async move {
            repository.cancel_turn(&scope, turn_id).await
        }));
    }
    for handle in handles {
        let run = handle.await.expect("join cancel").expect("cancel turn");
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(run.cancel_version, 1);
    }

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.cancelled")
            .count(),
        1
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_begin_run_claims_a_queued_run_exactly_once() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "claim").await;
    let conversation = create_conversation(&repository, &scope).await;
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("hello"),
            "claim-key".to_owned(),
            "claim-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    let run_id = acceptance.run.id;

    // Several workers race for the same run, the way a replayed dispatch would.
    let mut handles = Vec::new();
    for _ in 0..4 {
        let repository = Arc::clone(&repository);
        let scope = scope.clone();
        handles.push(tokio::spawn(async move {
            repository.begin_run(&scope, run_id).await
        }));
    }
    let mut claims = Vec::new();
    for handle in handles {
        claims.push(handle.await.expect("join claim").expect("begin run"));
    }
    let claimed: Vec<_> = claims.iter().flatten().collect();
    assert_eq!(
        claimed.len(),
        1,
        "exactly one caller may own the run: {claims:?}"
    );
    assert_eq!(claimed[0].status, RunStatus::Running);

    // A run is not claimable twice, and not from outside its own scope.
    assert!(
        repository
            .begin_run(&scope, run_id)
            .await
            .expect("second claim")
            .is_none(),
        "a claimed run is not claimable again"
    );
    let sibling = seed_sibling_scope(&pool, &scope, "claim").await;
    assert!(
        repository
            .begin_run(&sibling, run_id)
            .await
            .expect("cross-scope claim")
            .is_none(),
        "a run must not be claimable from a sibling project"
    );

    // A cancelled run is not claimable either: the cancellation outranks a
    // worker that has not started yet.  It needs a conversation of its own,
    // because the first one still has an active turn.
    let second = create_conversation(&repository, &scope).await;
    let cancelling = repository
        .append_message(
            &scope,
            second.id,
            message("second"),
            "claim-key-2".to_owned(),
            "claim-body-2".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append second message");
    repository
        .cancel_turn(&scope, cancelling.turn.id)
        .await
        .expect("cancel second turn");
    assert!(
        repository
            .begin_run(&scope, cancelling.run.id)
            .await
            .expect("claim after cancel")
            .is_none(),
        "a cancelled run must not be claimed for execution"
    );

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.running")
            .count(),
        1,
        "only the winning claim may be recorded"
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_queued_scan_pages_across_scopes_without_claiming_or_resurrecting_runs() {
    // Global discovery is intentionally isolated from concurrently running
    // tests; otherwise their queued runs can appear in these exact pages.
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("test database");
    let admin = PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("admin pool");
    let schema = format!("queued_scan_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("connection options")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .connect_with(options)
        .await
        .expect("isolated pool");
    geo_persistence::MIGRATOR
        .run(&pool)
        .await
        .expect("isolated migrations");
    let first_repo = PgAgentRepository::new(pool.clone());
    let first_scope = seed_scope(&pool, "queued-first").await;
    let sibling_scope = seed_sibling_scope(&pool, &first_scope, "queued-sibling").await;
    let other_tenant = seed_scope(&pool, "queued-other-tenant").await;
    let mut queued = Vec::new();
    for (index, scope) in [&first_scope, &sibling_scope, &other_tenant]
        .into_iter()
        .enumerate()
    {
        let conversation = create_conversation(&first_repo, scope).await;
        let acceptance = first_repo
            .append_message(
                scope,
                conversation.id,
                message("queued"),
                format!("queued-key-{index}"),
                format!("queued-body-{index}"),
                RuntimeCapability::available("deno_core", None),
            )
            .await
            .expect("queued acceptance");
        queued.push((scope.clone(), acceptance.run.id));
    }
    let stamp = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("fixed timestamp")
        .with_timezone(&chrono::Utc);
    for (index, (_, run_id)) in queued.iter().enumerate() {
        sqlx::query("UPDATE agent_runs SET created_at = $1 WHERE run_id = $2")
            .bind(stamp + chrono::Duration::seconds(if index == 2 { 1 } else { 0 }))
            .bind(run_id.as_uuid())
            .execute(&pool)
            .await
            .expect("set deterministic cursor");
    }
    // An unavailable capability is durably failed on acceptance, not queued.
    let failed_conversation = create_conversation(&first_repo, &first_scope).await;
    let failed = first_repo
        .append_message(
            &first_scope,
            failed_conversation.id,
            message("unavailable"),
            "failed-key".to_owned(),
            "failed-body".to_owned(),
            RuntimeCapability::missing("runtime unavailable"),
        )
        .await
        .expect("failed acceptance");
    assert_eq!(failed.run.status, RunStatus::Failed);
    let cancelled_conversation = create_conversation(&first_repo, &first_scope).await;
    let cancelled = first_repo
        .append_message(
            &first_scope,
            cancelled_conversation.id,
            message("cancel"),
            "cancelled-key".to_owned(),
            "cancelled-body".to_owned(),
            RuntimeCapability::available("deno_core", None),
        )
        .await
        .expect("cancelled acceptance");
    first_repo
        .cancel_turn(&first_scope, cancelled.turn.id)
        .await
        .expect("cancel queued turn");
    let running_conversation = create_conversation(&first_repo, &first_scope).await;
    let running = first_repo
        .append_message(
            &first_scope,
            running_conversation.id,
            message("running"),
            "running-key".to_owned(),
            "running-body".to_owned(),
            RuntimeCapability::available("deno_core", None),
        )
        .await
        .expect("running acceptance");
    first_repo
        .begin_run(&first_scope, running.run.id)
        .await
        .expect("begin running")
        .expect("running claim");

    let restarted = PgAgentRepository::new(pool.clone());
    assert_eq!(
        restarted
            .scan_queued_after(None, 0)
            .await
            .expect_err("zero page size")
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        restarted
            .scan_queued_after(None, 101)
            .await
            .expect_err("oversized page")
            .code,
        ErrorCode::InvalidRequest
    );
    let mut discovered = Vec::new();
    let mut cursor = None;
    loop {
        let page = restarted
            .scan_queued_after(cursor, 1)
            .await
            .expect("keyset page");
        if page.is_empty() {
            break;
        }
        let row = page.into_iter().next().expect("page row");
        cursor = Some((row.created_at, row.run_id));
        discovered.push(row);
    }
    assert_eq!(discovered.len(), 3);
    queued[..2].sort_by_key(|(_, run_id)| run_id.as_uuid());
    for (row, (scope, run_id)) in discovered.iter().zip(&queued) {
        assert_eq!(&row.scope, scope);
        assert_eq!(row.run_id, *run_id);
    }
    assert_eq!(discovered[0].created_at, stamp);
    assert_eq!(discovered[1].created_at, stamp);
    assert_eq!(
        discovered[2].created_at,
        stamp + chrono::Duration::seconds(1)
    );
    assert_eq!(
        first_repo
            .run_status(&discovered[0].scope, discovered[0].run_id)
            .await
            .expect("scan does not claim"),
        Some(RunStatus::Queued)
    );
    let foreign_scope = if discovered[0].scope == first_scope {
        &sibling_scope
    } else {
        &first_scope
    };
    assert!(
        restarted
            .begin_run(foreign_scope, discovered[0].run_id)
            .await
            .expect("sibling cannot claim")
            .is_none()
    );
    let mut claims = Vec::new();
    for repository in [first_repo.clone(), restarted.clone()] {
        let row = discovered[0].clone();
        claims.push(tokio::spawn(async move {
            repository.begin_run(&row.scope, row.run_id).await
        }));
    }
    let mut winners = 0;
    for claim in claims {
        if claim
            .await
            .expect("join claim")
            .expect("claim transaction")
            .is_some()
        {
            winners += 1;
        }
    }
    assert_eq!(winners, 1, "only one repository may begin a queued run");
    let claimed_conversation: Uuid =
        sqlx::query_scalar("SELECT conversation_id FROM agent_runs WHERE run_id = $1")
            .bind(discovered[0].run_id.as_uuid())
            .fetch_one(&pool)
            .await
            .expect("claimed conversation");
    let events = restarted
        .replay_events(&discovered[0].scope, claimed_conversation.into(), Some(0))
        .await
        .expect("running events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.running")
            .count(),
        1
    );
    assert_eq!(
        restarted
            .scan_queued_after(None, 100)
            .await
            .expect("scan excludes claimed run")
            .len(),
        2
    );
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("remove isolated schema");
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_single_process_startup_reconciles_only_running_runs() {
    // This operation intentionally scans all running rows in a single-process
    // deployment. Isolate its schema from parallel repository tests rather
    // than weakening the production query or disrupting their active runs.
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("test database");
    let admin = PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("admin pool");
    let schema = format!("reconcile_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .expect("connection options")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .connect_with(options)
        .await
        .expect("isolated pool");
    geo_persistence::MIGRATOR
        .run(&pool)
        .await
        .expect("isolated migrations");
    let repository = PgAgentRepository::new(pool.clone());
    let scope = seed_scope(&pool, "reconcile").await;
    let conversation = create_conversation(&repository, &scope).await;
    let running = repository
        .append_message(
            &scope,
            conversation.id,
            message("resume"),
            "reconcile-key".to_owned(),
            "reconcile-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    repository
        .begin_run(&scope, running.run.id)
        .await
        .expect("begin run")
        .expect("claim");

    let queued_conversation = create_conversation(&repository, &scope).await;
    let queued = repository
        .append_message(
            &scope,
            queued_conversation.id,
            message("not started"),
            "reconcile-queued-key".to_owned(),
            "reconcile-queued-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append queued message");
    let restarted = PgAgentRepository::new(pool.clone());
    assert_eq!(
        restarted.reconcile_running_runs().await.expect("reconcile"),
        1
    );
    assert_eq!(
        restarted
            .reconcile_running_runs()
            .await
            .expect("repeated reconcile"),
        0
    );

    let detail = restarted
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("conversation");
    assert_eq!(detail.runs[0].status, RunStatus::Failed);
    assert_eq!(detail.turns[0].status, TurnStatus::Failed);
    assert_eq!(
        detail.runs[0].error.as_ref().map(|error| error.code),
        Some(ErrorCode::DependencyUnavailable)
    );
    assert_eq!(detail.messages.len(), 1, "no answer was fabricated");
    let events = restarted
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.failed")
            .count(),
        1
    );
    assert!(events.iter().any(|event| {
        event.event_type == "run.failed" && event.payload["reason"] == "process_restart"
    }));
    let queued_detail = restarted
        .get_conversation(&scope, queued_conversation.id)
        .await
        .expect("queued detail")
        .expect("queued conversation");
    assert_eq!(queued_detail.runs[0].id, queued.run.id);
    assert_eq!(queued_detail.runs[0].status, RunStatus::Queued);
    assert_eq!(queued_detail.turns[0].status, TurnStatus::Queued);
    pool.close().await;
    // Only the UUID-named schema created by this test is removed.
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("remove isolated schema");
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn agent_finish_run_records_the_answer_once_and_never_over_a_cancellation() {
    let database = connect().await;
    let pool = database.pool().clone();
    let repository = Arc::new(PgAgentRepository::new(pool.clone()));
    let scope = seed_scope(&pool, "finish").await;
    let conversation = create_conversation(&repository, &scope).await;

    // The happy path, driven through the real claim rather than around it.
    let acceptance = repository
        .append_message(
            &scope,
            conversation.id,
            message("how long is the warranty?"),
            "finish-key".to_owned(),
            "finish-body".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    repository
        .begin_run(&scope, acceptance.run.id)
        .await
        .expect("begin run")
        .expect("the run must be claimable");
    let transition = repository
        .finish_run(
            &scope,
            acceptance.run.id,
            RunCompletion::Succeeded {
                content: "twenty-four months".to_owned(),
                metadata: json!({"source": "test"}),
            },
        )
        .await
        .expect("finish run")
        .expect("the run must have been running");
    assert_eq!(transition.run.status, RunStatus::Succeeded);
    assert_eq!(transition.turn.status, TurnStatus::Succeeded);
    let answer = transition
        .message
        .expect("a succeeded run carries its answer");
    assert_eq!(answer.role, MessageRole::Assistant);
    assert_eq!(answer.content, "twenty-four months");

    // Finishing twice must not add a second answer.
    assert!(
        repository
            .finish_run(
                &scope,
                acceptance.run.id,
                RunCompletion::Succeeded {
                    content: "a second answer".to_owned(),
                    metadata: Value::Null,
                },
            )
            .await
            .expect("second finish")
            .is_none(),
        "a terminal run must not be finished again"
    );

    // An answer that cannot be stored fails the run instead of being truncated.
    let unstorable = repository
        .append_message(
            &scope,
            conversation.id,
            message("empty answer please"),
            "finish-key-empty".to_owned(),
            "finish-body-empty".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    repository
        .begin_run(&scope, unstorable.run.id)
        .await
        .expect("begin run")
        .expect("claimable");
    let failed = repository
        .finish_run(
            &scope,
            unstorable.run.id,
            RunCompletion::Succeeded {
                content: "   ".to_owned(),
                metadata: Value::Null,
            },
        )
        .await
        .expect("finish run")
        .expect("running");
    assert_eq!(failed.run.status, RunStatus::Failed);
    assert!(
        failed.message.is_none(),
        "an unstorable answer is not stored"
    );
    assert_eq!(
        failed.run.error.as_ref().map(|error| error.code),
        Some(ErrorCode::InvalidRequest)
    );

    // Cancellation wins over a completion, and the answer is discarded.
    let raced = repository
        .append_message(
            &scope,
            conversation.id,
            message("cancel me"),
            "finish-key-race".to_owned(),
            "finish-body-race".to_owned(),
            RuntimeCapability::available("deno_core", Some("0.412.0".to_owned())),
        )
        .await
        .expect("append message");
    repository
        .begin_run(&scope, raced.run.id)
        .await
        .expect("begin run")
        .expect("claimable");
    repository
        .cancel_turn(&scope, raced.turn.id)
        .await
        .expect("cancel turn");
    assert!(
        repository
            .finish_run(
                &scope,
                raced.run.id,
                RunCompletion::Succeeded {
                    content: "too late".to_owned(),
                    metadata: Value::Null,
                },
            )
            .await
            .expect("late finish")
            .is_none(),
        "a cancelled run must not accept a completion"
    );

    let detail = repository
        .get_conversation(&scope, conversation.id)
        .await
        .expect("detail")
        .expect("present");
    let answers: Vec<_> = detail
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .collect();
    assert_eq!(
        answers.len(),
        1,
        "exactly one answer must be durable: {answers:?}"
    );
    assert_eq!(answers[0].content, "twenty-four months");
    let raced_run = detail
        .runs
        .iter()
        .find(|run| run.id == raced.run.id)
        .expect("raced run");
    assert_eq!(raced_run.status, RunStatus::Cancelled);

    let events = repository
        .replay_events(&scope, conversation.id, Some(0))
        .await
        .expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.succeeded")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.failed")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "run.cancelled")
            .count(),
        1
    );
}

#[tokio::test]
async fn agent_unreachable_database_fails_closed() {
    // No credentials or database are required here: the pool targets a closed
    // local port, so the repository has to report a dependency failure instead
    // of silently serving process memory.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(250))
        .connect_lazy("postgres://geo:geo@127.0.0.1:1/geo_test")
        .expect("valid lazy pool");
    let repository = PgAgentRepository::new(pool);
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );

    for error in [
        repository
            .list_conversations(&scope)
            .await
            .expect_err("list"),
        repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .expect_err("create"),
        repository
            .replay_events(&scope, Uuid::new_v4().into(), Some(0))
            .await
            .expect_err("replay"),
        repository
            .get_conversation(&scope, Uuid::new_v4().into())
            .await
            .expect_err("read"),
    ] {
        assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    }
}
