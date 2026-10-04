use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use geo_api::{
    AppState, ChannelService, ContentService, EventBus, MemoryIdempotencyStore,
    MemoryOperationStore, ModelProviderBridge, RepositoryHostOps,
    distribution::DistributionService,
};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelRepository, ChannelStatus,
    ContentItemStatus, ContentRepository, DistributionScope, DistributionScopeMode,
    DistributionTargetStatus, DocumentManifestPlanRequest, DocumentScope, ErrorCode, ImportItem,
    InitialSource, InitialSourceKind, InitialSourceVisibility, KnowledgePurpose,
    KnowledgeRepository, MemoryAuthRepository, MemoryChannelRepository, MemoryContentRepository,
    MemoryDistributionRepository, MemoryKnowledgeRepository, MemoryProjectRepository,
    PlatformPlacement, ProjectCreate, ProjectPatch, ProjectRepository, ProjectSettings,
    ProjectStartCommand, SourceKind, TenantScope, hash_idempotency_key, settings_hash,
    start_request_hash,
};
use geo_worker::{
    DistributionReadRequest, DistributionResumeRequest, DistributionStartRequest,
    DistributionTargetsReadRequest, HostOpError, HostOpErrorCode, HostOps, ModelCompletion,
    ModelCompletionRequest,
};
use uuid::Uuid;

struct GroundedModel;
#[async_trait]
impl ModelProviderBridge for GroundedModel {
    async fn complete(
        &self,
        _scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let input: serde_json::Value = serde_json::from_str(&request.prompt).unwrap();
        let citation = input["evidence"][0]["chunk_id"].as_str().unwrap();
        let text = if request
            .system
            .as_deref()
            .unwrap_or_default()
            .contains("checker")
        {
            let block = input["document"]["blocks"][0]["block_id"].as_str().unwrap();
            let title = input["title_check_id"].as_str().unwrap();
            serde_json::json!({"checks":[
                {"block_id":title,"verdict":"supported","citation_ids":[citation],
                 "detail":"Verified from the public source"},
                {"block_id":block,"verdict":"supported","citation_ids":[citation],
                 "detail":"Verified from the public source"}]})
            .to_string()
        } else {
            serde_json::json!({"title":"A documented answer","blocks":[{
                "kind":"paragraph","text":"Public description",
                "citation_ids":[citation],"items":[]}]})
            .to_string()
        };
        Ok(ModelCompletion {
            text,
            tool_calls: vec![],
            model: "injected".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop".into(),
        })
    }
}

fn placement(name: &str, formats: &[&str], unavailable: Option<&str>) -> PlatformPlacement {
    PlatformPlacement {
        platform_id: name.into(),
        placement_slot: "primary".into(),
        capability_version: "fixture-capabilities-v1".into(),
        supported_formats: formats.iter().map(|value| (*value).to_owned()).collect(),
        unavailable_reason: unavailable.map(str::to_owned),
        fixture: true,
    }
}

struct Fixture {
    scope: TenantScope,
    cycle_id: Uuid,
    projects: Arc<MemoryProjectRepository>,
    knowledge: Arc<MemoryKnowledgeRepository>,
    content: Arc<MemoryContentRepository>,
    channels: Arc<MemoryChannelRepository>,
    distribution: Arc<MemoryDistributionRepository>,
    service: DistributionService,
}

async fn setup(second_blocked: bool) -> Fixture {
    let tenant = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
    let projects = Arc::new(MemoryProjectRepository::default());
    let project = projects
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Generic fixture".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Public description".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    document_scope: DocumentScope {
                        content_types: vec!["faq".into(), "company_profile".into()],
                        ..Default::default()
                    },
                    distribution_scope: DistributionScope {
                        mode: DistributionScopeMode::Explicit,
                        included_platform_ids: vec!["a".into(), "b".into(), "c".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    let hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let started = projects
        .start(
            &tenant,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("distribution-fixture-start"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public".into(),
                kind: SourceKind::Text,
                name: "generic public input".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Public description".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let release_id = imported.items[0]
        .release
        .as_ref()
        .unwrap()
        .knowledge_release_id;
    let planned = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: started.document_manifest.manifest_id,
                knowledge_release_id: release_id,
            },
            project.settings.document_scope,
        )
        .await
        .unwrap();
    assert_eq!(planned.items.len(), 2);
    let content = Arc::new(MemoryContentRepository::default());
    let first_stage = ContentService::new(content.clone(), knowledge.clone(), projects.clone())
        .with_model_provider(Arc::new(GroundedModel));
    let execution = first_stage.start(&scope, started.cycle_id).await.unwrap();
    let items = content
        .list_items(&scope, execution.execution_id)
        .await
        .unwrap();
    for (index, item) in items.iter().enumerate() {
        if index == 1 && second_blocked {
            content
                .classify(
                    &scope,
                    execution.execution_id,
                    item.item_id,
                    ContentItemStatus::Blocked,
                    "fact_conflict",
                )
                .await
                .unwrap();
            continue;
        }
        first_stage
            .prepare(&scope, execution.execution_id, item.item_id)
            .await
            .unwrap();
        first_stage
            .generate(&scope, execution.execution_id, item.item_id)
            .await
            .unwrap();
        assert_eq!(
            first_stage
                .check(&scope, execution.execution_id, item.item_id)
                .await
                .unwrap()
                .status,
            ContentItemStatus::Ready
        );
    }
    first_stage
        .close(&scope, execution.execution_id)
        .await
        .unwrap();
    let channels = Arc::new(MemoryChannelRepository::default());
    let distribution = Arc::new(MemoryDistributionRepository::new());
    let service = DistributionService::new(
        distribution.clone(),
        content.clone(),
        knowledge.clone(),
        projects.clone(),
        channels.clone(),
    )
    .with_capability_snapshot(vec![
        placement("a", &["faq", "company_profile"], None),
        placement("b", &["faq", "company_profile"], Some("runner_unavailable")),
        placement("c", &["unknown_format"], None),
    ]);
    Fixture {
        scope,
        cycle_id: started.cycle_id,
        projects,
        knowledge,
        content,
        channels,
        distribution,
        service,
    }
}

async fn add_account(fixture: &Fixture) {
    let now = Utc::now();
    fixture
        .channels
        .save_account(
            &fixture.scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id: Uuid::new_v4(),
                    project_id: fixture.scope.project_id.unwrap(),
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "a".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: Some("generic account".into()),
                    platform_account_id: Some("fixture-identity".into()),
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: false,
                    proxy_server: None,
                    created_at: now,
                    updated_at: now,
                },
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn agent_distribution_tools_use_service_with_current_cycle_and_reference_only_pages() {
    let fixture = setup(true).await;
    add_account(&fixture).await;
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password("unused")),
        fixture.projects.clone(),
        fixture.knowledge.clone(),
        EventBus::default(),
        false,
    )
    .with_content_repository(fixture.content.clone())
    .with_distribution_repository(fixture.distribution.clone())
    .with_channel_service(ChannelService::unconfigured(fixture.channels.clone()));
    let host = RepositoryHostOps::new(fixture.knowledge.clone()).with_content(state);
    let other_project = TenantScope::new(
        fixture.scope.operator_id,
        fixture.scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        host.distribution_start(&other_project, DistributionStartRequest { cycle_id: None })
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.distribution_read(
            &fixture.scope,
            DistributionReadRequest {
                cycle_id: None,
                manifest_id: None,
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    let started = host
        .distribution_start(&fixture.scope, DistributionStartRequest { cycle_id: None })
        .await
        .unwrap();
    assert_eq!(started.cycle_id, fixture.cycle_id);
    assert_eq!(started.expected_count, 6);
    assert!(started.complete);
    assert_eq!(started.expansion_cursor, 6);
    assert_eq!(
        started,
        host.distribution_start(
            &fixture.scope,
            DistributionStartRequest {
                cycle_id: Some(fixture.cycle_id)
            }
        )
        .await
        .unwrap()
    );
    assert_eq!(
        started,
        host.distribution_read(
            &fixture.scope,
            DistributionReadRequest {
                cycle_id: None,
                manifest_id: None,
            }
        )
        .await
        .unwrap()
    );
    let first = host
        .distribution_targets_read(
            &fixture.scope,
            DistributionTargetsReadRequest {
                manifest_id: started.manifest_id,
                after_ordinal: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.expected_count, 6);
    assert_eq!(first.items.len(), 2);
    assert!(first.next_ordinal.is_some());
    let second = host
        .distribution_targets_read(
            &fixture.scope,
            DistributionTargetsReadRequest {
                manifest_id: started.manifest_id,
                after_ordinal: first.next_ordinal,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 2);
    assert!(second.items[0].ordinal > first.items[1].ordinal);
    let view = serde_json::to_value(&first).unwrap();
    let serialized = view.to_string();
    for forbidden in [
        "account_id",
        "markdown",
        "payload_hash",
        "supported_formats",
        "capability_version",
        "platform_account_id",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }
    assert_eq!(
        host.distribution_resume(
            &fixture.scope,
            DistributionResumeRequest {
                manifest_id: started.manifest_id,
                after_ordinal: None,
            }
        )
        .await
        .unwrap(),
        started
    );
    assert_eq!(
        host.distribution_targets_read(
            &other_project,
            DistributionTargetsReadRequest {
                manifest_id: started.manifest_id,
                after_ordinal: None,
                limit: None,
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    // In-memory channel count and IDs cannot claim real connector support.
    // The unverified server snapshot leaves all targets non-publishable.
    assert!(
        first
            .items
            .iter()
            .all(|item| item.status != DistributionTargetStatus::Ready)
    );
}

#[tokio::test]
async fn two_documents_three_platforms_preserve_all_cells_and_only_queue_checked_revisions() {
    let fixture = setup(true).await;
    add_account(&fixture).await;
    let frozen = fixture
        .service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    assert_eq!(frozen.expected_count, 6);
    assert_eq!(frozen.expansion_cursor, 0);
    let result = fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    assert!(result.complete);
    let page = fixture
        .service
        .targets(&fixture.scope, frozen.manifest_id, None, 10)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 6);
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::Ready)
            .count(),
        1
    );
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::Deferred)
            .count(),
        1
    );
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::NotApplicable)
            .count(),
        1
    );
    assert_eq!(
        page.rows
            .iter()
            .filter(|target| target.status == DistributionTargetStatus::Blocked)
            .count(),
        3
    );
    let first = &page.rows[0];
    assert!(first.variant_id.is_some());
    assert!(first.publication_intent_id.is_some());
    assert_eq!(
        fixture
            .service
            .target(&fixture.scope, frozen.manifest_id, first.target_id)
            .await
            .unwrap(),
        *first
    );
    let commands = fixture
        .distribution
        .publication_commands(&fixture.scope)
        .await;
    assert_eq!(
        commands.len(),
        1,
        "outbox means intent, not a successful publication"
    );
    assert!(
        commands[0].fixture,
        "fixture connector must stay marked simulated"
    );
    fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .distribution
            .publication_commands(&fixture.scope)
            .await
            .len(),
        1
    );
    let foreign = TenantScope::new(
        Uuid::new_v4().into(),
        fixture.scope.tenant_id,
        fixture.scope.project_id,
    );
    assert_eq!(
        fixture
            .service
            .get(&foreign, frozen.manifest_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn frozen_scope_does_not_drift_and_delayed_account_uses_sealed_checked_revision() {
    let fixture = setup(false).await;
    let frozen = fixture
        .service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    let first = fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    assert!(first.complete);
    assert_eq!(first.expected_count, 6);
    let before = fixture
        .service
        .targets(&fixture.scope, frozen.manifest_id, None, 10)
        .await
        .unwrap();
    assert_eq!(before.rows[0].reason.as_deref(), Some("account_unassigned"));
    let project_id = fixture.scope.project_id.unwrap();
    let current = fixture
        .projects
        .get(&fixture.scope, project_id)
        .await
        .unwrap()
        .unwrap();
    fixture
        .projects
        .update(
            &fixture.scope,
            project_id,
            current.revision,
            ProjectPatch {
                distribution_scope: Some(DistributionScope {
                    mode: DistributionScopeMode::Explicit,
                    included_platform_ids: vec!["different".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // A post-freeze edit creates a new current draft but must not mutate the
    // immutable checked revision or the earlier distribution handoff.
    let source_row = &before.rows[0];
    let execution = fixture
        .content
        .list_executions(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap()
        .remove(0);
    let item = fixture
        .content
        .get_item(
            &fixture.scope,
            execution.execution_id,
            source_row.document_item_id,
        )
        .await
        .unwrap()
        .unwrap();
    let old_revision = fixture
        .content
        .list_revisions(&fixture.scope, item.asset_id.unwrap())
        .await
        .unwrap()
        .remove(0);
    let mut edited = old_revision.document.clone();
    edited.title = "Later draft awaiting check".into();
    fixture
        .content
        .edit(
            &fixture.scope,
            item.asset_id.unwrap(),
            old_revision.revision_id,
            edited,
        )
        .await
        .unwrap();
    add_account(&fixture).await;
    let replay = fixture
        .service
        .freeze(&fixture.scope, fixture.cycle_id)
        .await
        .unwrap();
    assert_eq!(replay.input_hash, frozen.input_hash);
    assert_eq!(replay.platform_scope, frozen.platform_scope);
    fixture
        .service
        .resume(&fixture.scope, frozen.manifest_id, 1)
        .await
        .unwrap();
    let after = fixture
        .service
        .targets(&fixture.scope, frozen.manifest_id, None, 10)
        .await
        .unwrap();
    assert_eq!(
        after.rows[0].content_revision_id,
        Some(old_revision.revision_id)
    );
    assert_eq!(after.rows[0].status, DistributionTargetStatus::Ready);
    assert_eq!(
        fixture
            .distribution
            .publication_commands(&fixture.scope)
            .await
            .len(),
        2
    );
}
