//! End-to-end native first-fanout execution against real Rust repositories.
//! The generated upstream-dependent bundle is intentionally not tracked.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use geo_api::{
    AppState, EventBus, MemoryIdempotencyStore, MemoryOperationStore, ModelProviderBridge,
};
use geo_domain::{
    ContentExecutionStatus, ContentItemStatus, DocumentScope, ImportItem, InitialSource,
    InitialSourceKind, InitialSourceVisibility, KnowledgePurpose, KnowledgeRepository,
    MemoryAuthRepository, MemoryContentRepository, MemoryDistributionRepository,
    MemoryKnowledgeRepository, MemoryProjectRepository, ProjectCreate, ProjectRepository,
    ProjectSettings, ProjectStartCommand, SourceKind, TenantScope, hash_idempotency_key,
    settings_hash, start_request_hash,
};
use geo_worker::{
    ContentStartRequest, HostOpError, HostOps, ModelCompletion, ModelCompletionRequest,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::runtime::assemble_content_workflow;

#[derive(Default)]
struct GroundedModel {
    generated: AtomicUsize,
    checked: AtomicUsize,
}

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
            .contains("independent factual checker")
        {
            self.checked.fetch_add(1, Ordering::SeqCst);
            let quote = input["evidence"][0]["quote"].as_str().unwrap();
            assert_eq!(input["document"]["title"].as_str(), Some(quote));
            let mut ids = vec![input["title_check_id"].as_str().unwrap()];
            ids.extend(
                input["document"]["blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|block| block["block_id"].as_str().unwrap()),
            );
            let checks = ids
                .into_iter()
                .map(|block_id| {
                    serde_json::json!({
                        "block_id":block_id, "verdict":"supported",
                        "citation_ids":[citation],
                        "detail":"The supplied quote supports this exact claim"
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({"checks":checks}).to_string()
        } else {
            self.generated.fetch_add(1, Ordering::SeqCst);
            let quote = input["evidence"][0]["quote"].as_str().unwrap();
            serde_json::json!({
                "title":quote,
                "blocks":[{"kind":"paragraph","text":quote,"citation_ids":[citation],"items":[]}]
            })
            .to_string()
        };
        Ok(ModelCompletion {
            text,
            tool_calls: vec![],
            model: "injected-content-test".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop".into(),
        })
    }
}

fn assembled_state(
    projects: Arc<MemoryProjectRepository>,
    knowledge: Arc<MemoryKnowledgeRepository>,
    content: Arc<MemoryContentRepository>,
    distribution: Arc<MemoryDistributionRepository>,
    model: Option<Arc<GroundedModel>>,
    bundle_path: &str,
    digest: &str,
) -> AppState {
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password(
            &Uuid::new_v4().to_string(),
        )),
        projects,
        knowledge,
        EventBus::default(),
        false,
    )
    .with_content_repository(content)
    .with_distribution_repository(distribution);
    if let Some(model) = model {
        state.configure_content_model(model);
    }
    assemble_content_workflow(&state, bundle_path, digest)
        .expect("approved generated bundle assembles with Rust host capabilities");
    state
}

#[tokio::test]
#[ignore = "requires pnpm agent:bundle; generated MemeLoop ESM is not tracked"]
async fn generated_content_bundle_dispatches_two_branches_and_replays_without_model_calls() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/agent-runtime/dist/memeloop-content-workflow.bundle.mjs");
    let source = std::fs::read(&path).expect("run pnpm agent:bundle before this test");
    let digest = hex::encode(Sha256::digest(&source));
    let path = path.to_str().unwrap();
    let projects = Arc::new(MemoryProjectRepository::default());
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let content = Arc::new(MemoryContentRepository::default());
    let distribution = Arc::new(MemoryDistributionRepository::default());
    let model = Arc::new(GroundedModel::default());
    let state = assembled_state(
        projects.clone(),
        knowledge.clone(),
        content.clone(),
        distribution.clone(),
        Some(model.clone()),
        path,
        &digest,
    );
    let base = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
    let document_scope = DocumentScope {
        content_types: vec!["faq".into(), "company_profile".into()],
        ..Default::default()
    };
    let project = projects
        .create(
            &base,
            ProjectCreate {
                slug: None,
                display_name: "Content integration".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Approved public product description".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    document_scope: document_scope.clone(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(base.operator_id, base.tenant_id, Some(project.id));
    let frozen_settings = project.settings.clone().validate_start().unwrap();
    let frozen_hash = settings_hash(&frozen_settings).unwrap();
    let started = projects
        .start(
            &base,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("content-integration-start"),
                request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
                settings_hash: frozen_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "public-source".into(),
                kind: SourceKind::Text,
                name: "Approved public source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Approved public product description".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    assert!(imported.items[0].release.is_some());
    assert!(
        knowledge
            .get_document_manifest(&scope, started.document_manifest.manifest_id)
            .await
            .unwrap()
            .is_none(),
        "native P00 start must not require a manually planned manifest"
    );
    let host = geo_api::RepositoryHostOps::new(knowledge.clone()).with_content(state.clone());
    let started_ref = host
        .content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(started.cycle_id),
            },
        )
        .await
        .expect("P00 starts planning and native dispatch together");
    let manifest = knowledge
        .get_document_manifest(&scope, started.document_manifest.manifest_id)
        .await
        .unwrap()
        .expect("P00 planned the current cycle");
    assert!(manifest.sealed);
    assert_eq!(manifest.expected_count, Some(2));
    let execution = state
        .content_service()
        .repository()
        .get_execution(&scope, started_ref.execution_id)
        .await
        .unwrap()
        .expect("P00 execution persisted");
    assert_eq!(execution.expected_count, 2);
    let repository = state.content_service().repository();
    let closed = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let current = repository
                .get_execution(&scope, execution.execution_id)
                .await
                .unwrap()
                .unwrap();
            if current.status == ContentExecutionStatus::Closed
                && state
                    .distribution_service()
                    .latest_for_cycle(&scope, started.cycle_id)
                    .await
                    .unwrap()
                    .is_some_and(|manifest| manifest.complete)
            {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("native workflow closes within bounded time");
    assert_eq!(closed.coverage.ready, 2);
    assert_eq!(closed.coverage.incomplete, 0);
    let items = repository
        .list_items(&scope, execution.execution_id)
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item.status == ContentItemStatus::Ready)
    );
    let assets = repository
        .list_assets(&scope, execution.execution_id)
        .await
        .unwrap();
    assert_eq!(assets.len(), 2);
    assert_ne!(assets[0].asset_id, assets[1].asset_id);
    for asset in &assets {
        let revisions = repository
            .list_revisions(&scope, asset.asset_id)
            .await
            .unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(
            revisions[0].document.blocks[0].citation_ids.len(),
            1,
            "each generated branch remains source-grounded"
        );
    }
    assert_eq!(model.generated.load(Ordering::SeqCst), 2);
    assert_eq!(model.checked.load(Ordering::SeqCst), 2);
    let frozen_distribution = state
        .distribution_service()
        .latest_for_cycle(&scope, started.cycle_id)
        .await
        .unwrap()
        .expect("native workflow froze the second-stage manifest");
    assert!(frozen_distribution.complete);
    assert_eq!(
        frozen_distribution.expansion_cursor,
        frozen_distribution.expected_count
    );
    assert_eq!(
        frozen_distribution.content_execution_id,
        execution.execution_id
    );
    assert_eq!(
        frozen_distribution.content_handoff_id,
        closed.handoff_id.unwrap()
    );
    assert_eq!(frozen_distribution.expected_count, 6);
    let target_page = state
        .distribution_service()
        .targets(&scope, frozen_distribution.manifest_id, None, 100)
        .await
        .unwrap();
    assert_eq!(target_page.rows.len(), 6);
    assert!(
        target_page
            .rows
            .iter()
            .all(|target| target.publication_intent_id.is_none())
    );

    let initial_cycle = projects
        .get_report_cycle(&scope, project.id, started.cycle_id)
        .await
        .unwrap()
        .unwrap();
    let successor = projects
        .schedule_next_cycle(
            &scope,
            project.id,
            started.cycle_id,
            initial_cycle.cutoff_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_ne!(successor.cycle_id, started.cycle_id);
    // Reassemble against the same durable state with no model configured and
    // a newer current cycle. Closed replay may only recheck its original
    // immutable manifest; it needs neither generation nor a model provider.
    let restarted = assembled_state(
        projects.clone(),
        knowledge,
        content,
        distribution,
        None,
        path,
        &digest,
    );
    assert!(!restarted.content_model_available());
    assert_eq!(
        projects
            .get_current_cycle(&scope, project.id)
            .await
            .unwrap()
            .unwrap()
            .cycle_id,
        successor.cycle_id
    );
    restarted
        .dispatch_content_execution(scope.clone(), execution.execution_id)
        .expect("re-dispatch the existing reference");
    // Dispatch is asynchronous; a direct bounded native replay confirms the
    // completed handoff and that re-entry cannot regenerate either branch.
    let capabilities = geo_api::RepositoryHostOps::new(restarted.knowledge_repository())
        .with_content(restarted.clone());
    let source: &'static str = Box::leak(
        crate::runtime::load_verified_bundle(path, &digest)
            .unwrap()
            .into_boxed_str(),
    );
    let bundle: &'static [(&'static str, &'static str)] = Box::leak(Box::new([(
        geo_api::content_runtime::CONTENT_WORKFLOW_ENTRY,
        source,
    )]));
    let executor = geo_api::content_runtime::EmbeddedContentWorkflowExecutor::with_bundle(
        bundle,
        geo_api::content_runtime::CONTENT_WORKFLOW_ENTRY,
        Arc::new(capabilities),
    );
    tokio::time::timeout(
        Duration::from_secs(45),
        executor.run(scope.clone(), execution.execution_id),
    )
    .await
    .expect("native replay bounded")
    .expect("native replay succeeds");
    assert_eq!(model.generated.load(Ordering::SeqCst), 2);
    assert_eq!(model.checked.load(Ordering::SeqCst), 2);
    assert!(
        restarted
            .distribution_service()
            .latest_for_cycle(&scope, successor.cycle_id)
            .await
            .unwrap()
            .is_none(),
        "old execution must not freeze the newer current cycle"
    );
    assert_eq!(
        restarted
            .distribution_service()
            .latest_for_cycle(&scope, started.cycle_id)
            .await
            .unwrap()
            .unwrap()
            .manifest_id,
        frozen_distribution.manifest_id,
        "restart must reuse the same frozen distribution manifest"
    );
    assert_eq!(
        restarted
            .content_service()
            .repository()
            .list_assets(&scope, execution.execution_id)
            .await
            .unwrap()
            .len(),
        2
    );

    // A genuinely new cycle must reuse checked content, not merely replay the
    // old execution. No model is configured in this reassembled application.
    let next_execution = restarted
        .content_service()
        .start(&scope, successor.cycle_id)
        .await
        .expect("prepare successor without a model");
    assert_ne!(next_execution.execution_id, execution.execution_id);
    tokio::time::timeout(
        Duration::from_secs(45),
        executor.run(scope.clone(), next_execution.execution_id),
    )
    .await
    .expect("native successor bounded")
    .expect("unchanged successor reuses checked revisions");
    let next_items = restarted
        .content_service()
        .repository()
        .list_items(&scope, next_execution.execution_id)
        .await
        .unwrap();
    assert_eq!(next_items.len(), items.len());
    for next in &next_items {
        let original = items
            .iter()
            .find(|item| item.document_key == next.document_key)
            .unwrap();
        assert_eq!(next.status, ContentItemStatus::Ready);
        assert_eq!(next.ready_revision_id, original.ready_revision_id);
        assert_eq!(next.asset_id, original.asset_id);
        assert_ne!(next.execution_id, original.execution_id);
    }
    assert_eq!(model.generated.load(Ordering::SeqCst), 2);
    assert_eq!(model.checked.load(Ordering::SeqCst), 2);
    assert!(
        restarted
            .content_service()
            .repository()
            .list_assets(&scope, next_execution.execution_id)
            .await
            .unwrap()
            .is_empty(),
        "reuse does not copy origin assets into a new owner"
    );
    let next_distribution = restarted
        .distribution_service()
        .latest_for_cycle(&scope, successor.cycle_id)
        .await
        .unwrap()
        .expect("successor has independent distribution coverage");
    assert_ne!(
        next_distribution.manifest_id,
        frozen_distribution.manifest_id
    );
    assert_eq!(
        next_distribution.expected_count,
        frozen_distribution.expected_count
    );
    assert!(next_distribution.complete);
}
