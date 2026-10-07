//! Explicit integration cases; only run with an isolated disposable database.
use chrono::Utc;
use geo_domain::{
    ContentBlock, ContentBlockKind, ContentBrief, ContentMediaRepository, ContentRepository,
    ContentStep, DocumentManifestPlanRequest, ErrorCode, ImportItem, InitialSource,
    InitialSourceKind, InitialSourceVisibility, KnowledgePurpose, KnowledgeRepository,
    MediaReference, ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand,
    RICH_CHECK_POLICY_VERSION, RICH_GENERATION_POLICY_VERSION, RichContent, RichNode, SourceKind,
    StructuredDocument, TenantScope, hash_idempotency_key, settings_hash, sha256_hex,
    start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentMediaRepository, PgContentRepository, PgKnowledgeRepository,
    PgProjectRepository,
};
use uuid::Uuid;

fn document(citation: Uuid, image: Option<(Uuid, String)>) -> StructuredDocument {
    let node = match image {
        Some((object_id, sha256)) => RichNode::Media {
            attrs: MediaReference {
                object_id,
                object_version: 1,
                sha256,
                alt: "Product illustration".into(),
                caption: String::new(),
            },
        },
        None => RichNode::Paragraph {
            content: vec![RichNode::Text {
                text: "Illustration removed".into(),
                marks: vec![],
            }],
        },
    };
    StructuredDocument {
        title: "Verified document".into(),
        schema_version: Some(2),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Rich,
            text: String::new(),
            citation_ids: vec![citation],
            items: vec![],
            rich: Some(RichContent { version: 1, node }),
        }],
    }
}

struct Fixture {
    database: Database,
    scope: TenantScope,
    repository: PgContentRepository,
    media: PgContentMediaRepository,
    execution_id: Uuid,
    item_id: Uuid,
    citation: Uuid,
    object_id: Uuid,
    session_id: Uuid,
    hash: String,
}

async fn fixture() -> Fixture {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Test')")
        .bind(operator)
        .bind(format!("content-media-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("content-media-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(&database);
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic media project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "global".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Synthetic public reference".into(),
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
    let frozen = project.settings.clone().validate_start().unwrap();
    let frozen_hash = settings_hash(&frozen).unwrap();
    let start = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("media-content-start"),
                request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
                settings_hash: frozen_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: format!("content-media-{tenant}"),
                kind: SourceKind::Text,
                name: "Synthetic source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Documented product capability.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let source = imported.items[0].source.as_ref().unwrap().source_id;
    let release = imported.items[0]
        .release
        .as_ref()
        .unwrap()
        .knowledge_release_id;
    let detail = knowledge
        .get_source_detail(&scope, source)
        .await
        .unwrap()
        .unwrap();
    let chunk = &detail.chunks[0];
    let mut document_scope = frozen.document_scope.clone();
    document_scope.markets = frozen.effective_markets();
    document_scope.languages = frozen.effective_languages();
    let manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: start.document_manifest.manifest_id,
                knowledge_release_id: release,
            },
            document_scope,
        )
        .await
        .unwrap();
    let item_id = manifest
        .items
        .iter()
        .find(|item| item.source_version_refs.contains(&chunk.source_version_id))
        .unwrap()
        .document_manifest_item_id;
    let repository = PgContentRepository::from_database(&database);
    let execution = repository
        .start(
            &scope,
            start.cycle_id,
            manifest,
            RICH_GENERATION_POLICY_VERSION,
        )
        .await
        .unwrap();
    let prepare = repository
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "worker",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    repository
        .complete_prepare(
            &scope,
            &prepare,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Document".into(),
                objective: "Describe".into(),
                evidence: vec![geo_domain::EvidenceRef {
                    source_version_id: chunk.source_version_id,
                    chunk_id: Some(chunk.chunk_id),
                    locator: chunk.locator.clone(),
                }],
                quotes: vec![],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let session = Uuid::new_v4();
    let object_id = Uuid::new_v4();
    let bytes = b"synthetic immutable attachment bytes";
    let hash = sha256_hex(bytes);
    sqlx::query(
        "INSERT INTO knowledge_upload_sessions
         (upload_session_id,operator_id,tenant_id,project_id,revision,filename,
          declared_media_type,expected_size,expected_sha256,purpose,state,expires_at,
          committed_object_id,staging_object_ref)
         VALUES ($1,$2,$3,$4,2,'attachment.bin','application/octet-stream',$5,$6,'internal',
                 'committed',now()+interval '1 day',$7,'agent-attachment')",
    )
    .bind(session)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(bytes.len() as i64)
    .bind(&hash)
    .bind(object_id)
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO knowledge_upload_blobs (upload_session_id,content,actual_size,sha256)
         VALUES ($1,$2,$3,$4)",
    )
    .bind(session)
    .bind(bytes.as_slice())
    .bind(bytes.len() as i64)
    .bind(&hash)
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO knowledge_stored_objects
         (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,
          actual_size,detected_media_type,sha256,state)
         VALUES ($1,$2,$3,$4,1,'postgres_blob',$5,$6,'application/octet-stream',$7,'committed')",
    )
    .bind(object_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(format!("upload/{session}"))
    .bind(bytes.len() as i64)
    .bind(&hash)
    .execute(database.pool())
    .await
    .unwrap();
    let media = PgContentMediaRepository::from_database(&database);
    Fixture {
        database,
        scope,
        repository,
        media,
        execution_id: execution.execution_id,
        item_id,
        citation: chunk.chunk_id,
        object_id,
        session_id: session,
        hash,
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn media_write_rolls_back_without_binding_and_removal_succeeds_after_withdrawal() {
    let f = fixture().await;
    let generated = f
        .repository
        .claim(
            &f.scope,
            f.execution_id,
            f.item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let media_doc = document(f.citation, Some((f.object_id, f.hash.clone())));
    assert_eq!(
        f.repository
            .complete_generate(&f.scope, &generated, media_doc.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        f.repository
            .get_item(&f.scope, f.execution_id, f.item_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        geo_domain::ContentItemStatus::Prepared
    );
    let image = geo_domain::VerifiedImage {
        key: geo_domain::MediaObjectKey {
            object_id: f.object_id,
            object_version: 1,
            sha256: f.hash.clone(),
        },
        media_type: "image/png".into(),
        byte_len: b"synthetic immutable attachment bytes".len() as u64,
        width: 8,
        height: 8,
    };
    let binding = f.media.create_binding(&f.scope, image).await.unwrap();
    let revision = f
        .repository
        .complete_generate(&f.scope, &generated, media_doc)
        .await
        .unwrap();
    f.media
        .withdraw_binding(&f.scope, binding.binding_id)
        .await
        .unwrap();
    let check = f
        .repository
        .claim(
            &f.scope,
            f.execution_id,
            f.item_id,
            ContentStep::Check,
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    assert_eq!(
        f.repository
            .complete_check(&f.scope, &check, vec![])
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let edited = f
        .repository
        .edit(
            &f.scope,
            revision.asset_id,
            revision.revision_id,
            document(f.citation, None),
        )
        .await
        .unwrap();
    assert!(edited.document.media_references().is_empty());
    assert_eq!(
        f.repository
            .list_revisions(&f.scope, revision.asset_id)
            .await
            .unwrap()
            .len(),
        2
    );
    // Keep the database alive through all repository operations.
    let _ = f.database.pool();
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn corrupt_bytes_prevent_a_new_revision_even_with_an_active_binding() {
    let f = fixture().await;
    let image = geo_domain::VerifiedImage {
        key: geo_domain::MediaObjectKey {
            object_id: f.object_id,
            object_version: 1,
            sha256: f.hash.clone(),
        },
        media_type: "image/png".into(),
        byte_len: b"synthetic immutable attachment bytes".len() as u64,
        width: 8,
        height: 8,
    };
    f.media.create_binding(&f.scope, image).await.unwrap();
    let lease = f
        .repository
        .claim(
            &f.scope,
            f.execution_id,
            f.item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let revision = f
        .repository
        .complete_generate(&f.scope, &lease, document(f.citation, None))
        .await
        .unwrap();
    // Simulate corruption outside the supported immutable upload API.
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$1 WHERE upload_session_id=$2")
        .bind(b"changed byte content".as_slice())
        .bind(f.session_id)
        .execute(f.database.pool())
        .await
        .unwrap();
    let error = f
        .repository
        .edit(
            &f.scope,
            revision.asset_id,
            revision.revision_id,
            document(f.citation, Some((f.object_id, f.hash))),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        f.repository
            .list_revisions(&f.scope, revision.asset_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn ready_transition_and_withdrawal_serialize_on_the_same_project_row() {
    let f = fixture().await;
    let image = geo_domain::VerifiedImage {
        key: geo_domain::MediaObjectKey {
            object_id: f.object_id,
            object_version: 1,
            sha256: f.hash.clone(),
        },
        media_type: "image/png".into(),
        byte_len: b"synthetic immutable attachment bytes".len() as u64,
        width: 8,
        height: 8,
    };
    let binding = f.media.create_binding(&f.scope, image).await.unwrap();
    let generate = f
        .repository
        .claim(
            &f.scope,
            f.execution_id,
            f.item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    f.repository
        .complete_generate(
            &f.scope,
            &generate,
            document(f.citation, Some((f.object_id, f.hash))),
        )
        .await
        .unwrap();
    let check = f
        .repository
        .claim(
            &f.scope,
            f.execution_id,
            f.item_id,
            ContentStep::Check,
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let (checked, withdrawn) = tokio::join!(
        f.repository.complete_check(&f.scope, &check, vec![]),
        f.media.withdraw_binding(&f.scope, binding.binding_id)
    );
    withdrawn.unwrap();
    match checked {
        Err(error) => {
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(
                f.repository
                    .get_item(&f.scope, f.execution_id, f.item_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                geo_domain::ContentItemStatus::Drafted
            );
        }
        Ok(item) => assert_eq!(item.status, geo_domain::ContentItemStatus::Ready),
    }
}
