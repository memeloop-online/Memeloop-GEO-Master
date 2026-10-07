use chrono::Utc;
use geo_api::{AppState, RepositoryHostOps};
use geo_domain::{
    AttachmentId, AttachmentReference, CONTENT_SEMANTIC_DESCRIPTOR_VERSION, ContentBlock,
    ContentBlockKind, ContentBrief, ContentEvidence, ContentReuseDecision, ContentReuseRequest,
    ContentSemanticDescriptor, ContentStep, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID,
    DocumentManifest, DocumentManifestPlanRequest, EvidenceRef, ImportItem, InitialSource,
    InitialSourceKind, InitialSourceVisibility, KnowledgePurpose, ProjectCreate, ProjectSettings,
    ProjectStartCommand, SourceKind, StructuredDocument, TenantScope, UploadSessionCommand,
    hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig};
use geo_worker::{
    ContentDocumentReadRequest, ContentMediaBindRequest, ContentMediaInsertRequest,
    ContentMediaListRequest, HostOpErrorCode, HostOps,
};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use sha2::{Digest, Sha256};
use uuid::Uuid;

async fn project(state: &AppState) -> TenantScope {
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    project_for(state, &tenant).await
}

async fn project_for(state: &AppState, tenant: &TenantScope) -> TenantScope {
    let projects = state.project_repository();
    let project = projects
        .create(
            tenant,
            ProjectCreate {
                slug: None,
                display_name: "Media fixture".into(),
                settings: ProjectSettings {
                    brand_name: "Fixture".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Fixture".into(),
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
    let hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    projects
        .start(
            tenant,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("synthetic-project-start"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id))
}

async fn committed_image(state: &AppState, scope: &TenantScope) -> AttachmentReference {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
            2,
            2,
            ExtendedColorType::Rgb8,
        )
        .unwrap();
    let knowledge = state.knowledge_repository();
    let session = knowledge
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "fixture.test".into(),
                declared_media_type: "text/plain".into(),
                expected_size: bytes.len() as u64,
                expected_sha256: hex::encode(Sha256::digest(&bytes)),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    knowledge
        .put_upload_content(scope, session.upload_session_id, bytes)
        .await
        .unwrap();
    let (object, filename) = knowledge
        .complete_attachment_upload(scope, session.upload_session_id, "image-commit")
        .await
        .unwrap();
    AttachmentReference {
        attachment_id: AttachmentId::new(object.object_id),
        object_id: object.object_id.to_string(),
        filename,
        media_type: Some(object.detected_media_type),
        size_bytes: Some(object.actual_size),
        sha256: Some(object.sha256),
        object_version: Some(object.object_version.to_string()),
    }
}

async fn drafted_content(
    state: &AppState,
    scope: &TenantScope,
) -> (
    Uuid,
    Uuid,
    geo_domain::ContentRevision,
    DocumentManifest,
    ContentSemanticDescriptor,
) {
    let content = state.content_service().repository();
    let project_id = scope.project_id.unwrap();
    let project = state
        .project_repository()
        .get(scope, project_id)
        .await
        .unwrap()
        .unwrap();
    let cycle = state
        .project_repository()
        .get_current_cycle(scope, project_id)
        .await
        .unwrap()
        .unwrap();
    let manifest_id = cycle.document_manifest.unwrap().manifest_id;
    let mut document_scope = project.settings.document_scope.clone();
    document_scope.markets = project.settings.effective_markets();
    document_scope.languages = project.settings.effective_languages();
    let knowledge = state.knowledge_repository();
    let imported = knowledge
        .import_batch(
            scope,
            vec![ImportItem {
                client_item_id: "fixture-source".into(),
                kind: SourceKind::Text,
                name: "Synthetic source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Original text".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let source = imported.items[0].source.as_ref().unwrap();
    let source_version_id = imported.items[0]
        .source_version
        .as_ref()
        .unwrap()
        .source_version_id;
    let release_id = imported.items[0]
        .release
        .as_ref()
        .unwrap()
        .knowledge_release_id;
    let manifest = knowledge
        .plan_document_manifest(
            scope,
            DocumentManifestPlanRequest {
                manifest_id,
                knowledge_release_id: release_id,
            },
            document_scope,
        )
        .await
        .unwrap();
    let branch = manifest.items[0].clone();
    let item_id = branch.document_manifest_item_id;
    let detail = knowledge
        .get_source_detail(scope, source.source_id)
        .await
        .unwrap()
        .unwrap();
    let quotes = detail
        .chunks
        .iter()
        .filter(|chunk| chunk.source_version_id == source_version_id)
        .map(|chunk| ContentEvidence {
            reference: EvidenceRef {
                source_version_id,
                chunk_id: Some(chunk.chunk_id),
                locator: chunk.locator.clone(),
            },
            exact_quote: chunk.text.clone(),
        })
        .collect::<Vec<_>>();
    assert!(!quotes.is_empty());
    let execution = content
        .start(scope, cycle.cycle_id, manifest.clone(), "fixture")
        .await
        .unwrap();
    let descriptor = ContentSemanticDescriptor {
        version: CONTENT_SEMANTIC_DESCRIPTOR_VERSION,
        scope: scope.clone(),
        document_key: branch.document_key,
        content_type: branch.content_type,
        product_id: branch.product_id,
        market: branch.market,
        language: branch.language,
        planner_version: manifest.planner_version.clone(),
        source_version_ids: vec![source_version_id],
        evidence: quotes.clone(),
        brand_name: "Fixture".into(),
        product_name: None,
        target_audience: None,
        objective: None,
        question_clusters: Vec::new(),
        brief_title: "Draft".into(),
        brief_objective: "Preserved source".into(),
        generation_policy_version: "fixture".into(),
        evidence_policy_version: "fixture".into(),
        check_policy_version: "fixture".into(),
        repair_policy_version: "fixture".into(),
        output_schema_version: "fixture".into(),
        generation_policy_revision: "fixture".into(),
    };
    let prepare = match content
        .prepare_or_reuse(
            scope,
            ContentReuseRequest {
                execution_id: execution.execution_id,
                item_id,
                descriptor: descriptor.clone(),
                owner: "fixture".into(),
                now: Utc::now(),
                ttl_seconds: 60,
            },
        )
        .await
        .unwrap()
    {
        ContentReuseDecision::Reserved { lease, .. } => lease,
        other => panic!("expected producer reservation, got {other:?}"),
    };
    content
        .complete_prepare(
            scope,
            &prepare,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Draft".into(),
                objective: "Preserved source".into(),
                evidence: quotes.iter().map(|quote| quote.reference.clone()).collect(),
                quotes,
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let generate = content
        .claim(
            scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "fixture",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let revision = content
        .complete_generate(
            scope,
            &generate,
            StructuredDocument {
                title: "Draft".into(),
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Original text".into(),
                    citation_ids: Vec::new(),
                    items: Vec::new(),
                    rich: None,
                }],
                schema_version: None,
            },
        )
        .await
        .unwrap();
    let check = content
        .claim(
            scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "fixture",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    content
        .complete_check(scope, &check, Vec::new())
        .await
        .unwrap();
    (
        execution.execution_id,
        item_id,
        revision,
        manifest,
        descriptor,
    )
}

#[tokio::test]
async fn host_insert_preserves_history_replays_exact_revision_and_rejects_stale_or_withdrawn() {
    let state = AppState::development();
    let scope = project(&state).await;
    let (execution_id, item_id, base, _, _) = drafted_content(&state, &scope).await;
    let attachment = committed_image(&state, &scope).await;
    let host = RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone());
    let bound = host
        .content_media_bind(
            &scope,
            ContentMediaBindRequest {
                attachment_id: attachment.attachment_id.as_uuid(),
            },
            &[attachment],
        )
        .await
        .unwrap();
    let request = ContentMediaInsertRequest {
        execution_id,
        item_id,
        base_revision_id: base.revision_id,
        binding_id: bound.binding_id,
        after_block_id: Some(base.document.blocks[0].block_id),
        alt: "A synthetic diagram".into(),
        caption: "Synthetic caption".into(),
    };
    let historical = host
        .content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id,
                item_id,
                revision_id: Some(base.revision_id),
            },
        )
        .await
        .unwrap();
    assert_eq!(historical.document, base.document);
    let other_scope = project(&state).await;
    assert_eq!(
        host.content_document_read(
            &other_scope,
            ContentDocumentReadRequest {
                execution_id,
                item_id,
                revision_id: None,
            },
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.content_media_insert(&other_scope, request.clone())
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound
    );
    let (first, duplicate) = tokio::join!(
        host.content_media_insert(&scope, request.clone()),
        host.content_media_insert(&scope, request.clone())
    );
    let first = first.unwrap();
    assert_eq!(duplicate.unwrap(), first);
    let current = host
        .content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id,
                item_id,
                revision_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(current.revision_id, first.revision_id);
    assert_eq!(current.document.blocks[0], base.document.blocks[0]);
    assert_eq!(current.document.blocks[1].block_id, first.block_id);
    assert!(current.document.blocks[1].citation_ids.is_empty());
    let content = state.content_service().repository();
    let other = content
        .edit(
            &scope,
            first.asset_id,
            first.revision_id,
            StructuredDocument {
                title: "Later edit".into(),
                ..current.document.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        host.content_media_insert(&scope, request.clone())
            .await
            .unwrap(),
        first
    );
    let mut stale = request.clone();
    stale.caption.push('!');
    assert_eq!(
        host.content_media_insert(&scope, stale)
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Failed
    );
    assert_eq!(
        content
            .list_revisions(&scope, first.asset_id)
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        content
            .get_asset(&scope, first.asset_id)
            .await
            .unwrap()
            .unwrap()
            .current_revision_id,
        other.revision_id
    );
    state
        .content_media_repository()
        .withdraw_binding(&scope, bound.binding_id)
        .await
        .unwrap();
    let mut new_request = request.clone();
    new_request.base_revision_id = other.revision_id;
    assert_eq!(
        host.content_media_insert(&scope, new_request)
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Failed
    );
    assert_eq!(
        host.content_media_insert(&scope, request).await.unwrap(),
        first,
        "existing exact revision can be read back without a new write"
    );
    let historical_again = host
        .content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id,
                item_id,
                revision_id: Some(base.revision_id),
            },
        )
        .await
        .unwrap();
    assert_eq!(historical_again.document, base.document);
}

#[tokio::test]
async fn reused_destination_forks_and_replays_without_changing_origin_asset() {
    let state = AppState::development();
    let scope = project(&state).await;
    let (_, _, origin, manifest, descriptor) = drafted_content(&state, &scope).await;
    let content = state.content_service().repository();
    let settings = state
        .project_repository()
        .get(&scope, scope.project_id.unwrap())
        .await
        .unwrap()
        .unwrap()
        .settings;
    let mut document_scope = settings.document_scope.clone();
    document_scope.markets = settings.effective_markets();
    document_scope.languages = settings.effective_languages();
    let manifest = state
        .knowledge_repository()
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: Uuid::new_v4(),
                knowledge_release_id: manifest.knowledge_release_id,
            },
            document_scope,
        )
        .await
        .unwrap();
    let dest_item_id = manifest.items[0].document_manifest_item_id;
    let destination = content
        .start(&scope, Uuid::new_v4(), manifest, "fixture")
        .await
        .unwrap();
    let reused = content
        .prepare_or_reuse(
            &scope,
            ContentReuseRequest {
                execution_id: destination.execution_id,
                item_id: dest_item_id,
                descriptor,
                owner: "fixture".into(),
                now: Utc::now(),
                ttl_seconds: 60,
            },
        )
        .await
        .unwrap();
    assert!(matches!(reused, ContentReuseDecision::Ready(_)));
    let host = RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone());
    let reused_read = host
        .content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id: destination.execution_id,
                item_id: dest_item_id,
                revision_id: None,
            },
        )
        .await
        .unwrap();
    assert!(reused_read.is_reused);
    assert_eq!(reused_read.asset_id, origin.asset_id);
    assert_eq!(reused_read.revision_id, origin.revision_id);
    let attachment = committed_image(&state, &scope).await;
    let media = host
        .content_media_bind(
            &scope,
            ContentMediaBindRequest {
                attachment_id: attachment.attachment_id.as_uuid(),
            },
            &[attachment],
        )
        .await
        .unwrap();
    let request = ContentMediaInsertRequest {
        execution_id: destination.execution_id,
        item_id: dest_item_id,
        base_revision_id: origin.revision_id,
        binding_id: media.binding_id,
        after_block_id: None,
        alt: "Reused diagram".into(),
        caption: String::new(),
    };
    let fork = host
        .content_media_insert(&scope, request.clone())
        .await
        .unwrap();
    assert_eq!(fork.revision, 1);
    assert_ne!(fork.asset_id, origin.asset_id);
    let destination_read = host
        .content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id: destination.execution_id,
                item_id: dest_item_id,
                revision_id: None,
            },
        )
        .await
        .unwrap();
    assert!(!destination_read.is_reused);
    assert_eq!(destination_read.revision_id, fork.revision_id);
    assert_eq!(
        destination_read.document.blocks[0],
        origin.document.blocks[0]
    );
    let later = content
        .edit(
            &scope,
            fork.asset_id,
            fork.revision_id,
            StructuredDocument {
                title: "Destination edit".into(),
                ..destination_read.document
            },
        )
        .await
        .unwrap();
    assert_eq!(
        host.content_media_insert(&scope, request).await.unwrap(),
        fork
    );
    assert_eq!(
        content
            .get_asset(&scope, origin.asset_id)
            .await
            .unwrap()
            .unwrap()
            .current_revision_id,
        origin.revision_id
    );
    assert_eq!(
        content
            .list_revisions(&scope, origin.asset_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        content
            .get_asset(&scope, fork.asset_id)
            .await
            .unwrap()
            .unwrap()
            .current_revision_id,
        later.revision_id
    );
}

#[tokio::test]
#[ignore = "requires an isolated disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn postgres_host_media_insert_replay_and_reused_copy_on_write_survive_reconnect() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL required");
    let config = DatabaseConfig::from_url(url).expect("valid database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrated disposable database");
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Fixture')")
        .bind(operator)
        .bind(format!("media-tools-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Fixture')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("media-tools-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let state = AppState::from_database(&database);
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let scope = project_for(&state, &tenant_scope).await;
    let (origin_execution, origin_item, original, manifest, descriptor) =
        drafted_content(&state, &scope).await;
    let project_id = scope.project_id.unwrap();
    let current_cycle = state
        .project_repository()
        .get_current_cycle(&scope, project_id)
        .await
        .unwrap()
        .unwrap();
    let successor = state
        .project_repository()
        .schedule_next_cycle(
            &scope,
            project_id,
            current_cycle.cycle_id,
            current_cycle.cutoff_at,
        )
        .await
        .unwrap();
    let project = state
        .project_repository()
        .get(&scope, project_id)
        .await
        .unwrap()
        .unwrap();
    let mut document_scope = project.settings.document_scope.clone();
    document_scope.markets = project.settings.effective_markets();
    document_scope.languages = project.settings.effective_languages();
    let successor_manifest = state
        .knowledge_repository()
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: successor.document_manifest.unwrap().manifest_id,
                knowledge_release_id: manifest.knowledge_release_id,
            },
            document_scope,
        )
        .await
        .unwrap();
    let destination_item = successor_manifest.items[0].document_manifest_item_id;
    let content = state.content_service().repository();
    let next = content
        .start(&scope, successor.cycle_id, successor_manifest, "fixture")
        .await
        .unwrap();
    assert!(matches!(
        content
            .prepare_or_reuse(
                &scope,
                ContentReuseRequest {
                    execution_id: next.execution_id,
                    item_id: destination_item,
                    descriptor,
                    owner: "fixture-successor".into(),
                    now: Utc::now(),
                    ttl_seconds: 600,
                },
            )
            .await
            .unwrap(),
        ContentReuseDecision::Ready(_)
    ));
    let attachment = committed_image(&state, &scope).await;
    let host = RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone());
    let bound = host
        .content_media_bind(
            &scope,
            ContentMediaBindRequest {
                attachment_id: attachment.attachment_id.as_uuid(),
            },
            &[attachment],
        )
        .await
        .unwrap();
    let fork_request = ContentMediaInsertRequest {
        execution_id: next.execution_id,
        item_id: destination_item,
        base_revision_id: original.revision_id,
        binding_id: bound.binding_id,
        after_block_id: None,
        alt: "A verified synthetic diagram".into(),
        caption: "Reference".into(),
    };
    let fork = host
        .content_media_insert(&scope, fork_request.clone())
        .await
        .unwrap();
    assert_eq!(fork.revision, 1);
    assert_ne!(fork.asset_id, original.asset_id);
    let fork_read = host
        .content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id: next.execution_id,
                item_id: destination_item,
                revision_id: Some(fork.revision_id),
            },
        )
        .await
        .unwrap();
    assert_eq!(fork_read.document.blocks[0], original.document.blocks[0]);
    assert_eq!(fork_read.document.blocks[1].block_id, fork.block_id);
    let destination_later = content
        .edit(
            &scope,
            fork.asset_id,
            fork.revision_id,
            StructuredDocument {
                title: "Later destination edit".into(),
                ..fork_read.document
            },
        )
        .await
        .unwrap();
    let ordinary_request = ContentMediaInsertRequest {
        execution_id: origin_execution,
        item_id: origin_item,
        base_revision_id: original.revision_id,
        binding_id: bound.binding_id,
        after_block_id: Some(original.document.blocks[0].block_id),
        alt: "A different synthetic diagram".into(),
        caption: String::new(),
    };
    let ordinary = host
        .content_media_insert(&scope, ordinary_request.clone())
        .await
        .unwrap();
    assert_eq!(ordinary.revision, 2);
    assert_eq!(ordinary.asset_id, original.asset_id);
    let ordinary_read = host
        .content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id: origin_execution,
                item_id: origin_item,
                revision_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        ordinary_read.document.blocks[0],
        original.document.blocks[0]
    );
    assert_eq!(ordinary_read.document.blocks[1].block_id, ordinary.block_id);
    let ordinary_later = content
        .edit(
            &scope,
            ordinary.asset_id,
            ordinary.revision_id,
            StructuredDocument {
                title: "Later origin edit".into(),
                ..ordinary_read.document
            },
        )
        .await
        .unwrap();
    let reconnected = Database::connect_and_migrate(&config)
        .await
        .expect("independent pool");
    let reopened = AppState::from_database(&reconnected);
    let host =
        RepositoryHostOps::new(reopened.knowledge_repository()).with_content(reopened.clone());
    assert_eq!(
        host.content_media_insert(&scope, fork_request.clone())
            .await
            .unwrap(),
        fork
    );
    assert_eq!(
        host.content_media_insert(&scope, ordinary_request.clone())
            .await
            .unwrap(),
        ordinary
    );
    assert_eq!(
        host.content_document_read(
            &scope,
            ContentDocumentReadRequest {
                execution_id: origin_execution,
                item_id: origin_item,
                revision_id: Some(original.revision_id),
            },
        )
        .await
        .unwrap()
        .document,
        original.document
    );
    let mut stale = ordinary_request.clone();
    stale.caption.push_str("not an exact replay");
    assert_eq!(
        host.content_media_insert(&scope, stale)
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Failed
    );
    let persisted = reopened.content_service().repository();
    assert_eq!(
        persisted
            .get_asset(&scope, fork.asset_id)
            .await
            .unwrap()
            .unwrap()
            .current_revision_id,
        destination_later.revision_id
    );
    assert_eq!(
        persisted
            .get_asset(&scope, ordinary.asset_id)
            .await
            .unwrap()
            .unwrap()
            .current_revision_id,
        ordinary_later.revision_id
    );
    assert_eq!(
        persisted
            .list_revisions(&scope, ordinary.asset_id)
            .await
            .unwrap()
            .len(),
        3
    );
    reopened
        .content_media_repository()
        .withdraw_binding(&scope, bound.binding_id)
        .await
        .unwrap();
    let new_request = ContentMediaInsertRequest {
        base_revision_id: ordinary_later.revision_id,
        ..ordinary_request
    };
    assert_eq!(
        host.content_media_insert(&scope, new_request)
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Failed,
        "withdrawn media cannot authorize another write",
    );
}

#[tokio::test]
async fn host_bind_decodes_real_png_and_lists_scoped_refs() {
    let state = AppState::development();
    let scope = project(&state).await;
    let attachment = committed_image(&state, &scope).await;
    let host = RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone());
    let bound = host
        .content_media_bind(
            &scope,
            ContentMediaBindRequest {
                attachment_id: attachment.attachment_id.as_uuid(),
            },
            &[attachment],
        )
        .await
        .unwrap();
    assert_eq!(bound.media_type, "image/png");
    assert_eq!((bound.width, bound.height), (2, 2));
    let page = host
        .content_media_list(
            &scope,
            ContentMediaListRequest {
                after: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![bound.clone()]);
    assert!(page.next_cursor.is_none());
    let other = project(&state).await;
    assert!(
        host.content_media_list(
            &other,
            ContentMediaListRequest {
                after: None,
                limit: None,
            }
        )
        .await
        .unwrap()
        .items
        .is_empty()
    );
    state
        .content_media_repository()
        .withdraw_binding(&scope, bound.binding_id)
        .await
        .unwrap();
    assert!(
        host.content_media_list(
            &scope,
            ContentMediaListRequest {
                after: None,
                limit: None,
            }
        )
        .await
        .unwrap()
        .items
        .is_empty()
    );
}

#[tokio::test]
async fn host_bind_rejects_unbound_spoofed_and_cross_project_attachments() {
    let state = AppState::development();
    let scope = project(&state).await;
    let attachment = committed_image(&state, &scope).await;
    let request = ContentMediaBindRequest {
        attachment_id: attachment.attachment_id.as_uuid(),
    };
    let host = RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone());
    assert_eq!(
        host.content_media_bind(&scope, request.clone(), &[])
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Denied
    );
    let mut spoofed = attachment.clone();
    spoofed.filename = "different.test".into();
    assert_eq!(
        host.content_media_bind(&scope, request.clone(), &[spoofed])
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::Failed
    );
    let other = project(&state).await;
    assert_eq!(
        host.content_media_bind(&other, request, &[attachment])
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.content_media_list(
            &TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None),
            ContentMediaListRequest::default(),
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::Denied
    );
    // The object was never bound by a spoofed attempt.
    assert_eq!(
        state
            .content_media_repository()
            .list_bindings(&scope, None, 10)
            .await
            .unwrap()
            .len(),
        0
    );
}
