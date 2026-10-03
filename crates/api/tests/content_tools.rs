use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use geo_api::{
    AppState, EventBus, MemoryIdempotencyStore, MemoryOperationStore, ModelProviderBridge,
    RepositoryHostOps, content_runtime::ContentWorkflowExecutor,
};
use geo_domain::{
    ContentItemStatus, DocumentManifestPlanRequest, DocumentScope, ImportItem, InitialSource,
    InitialSourceKind, InitialSourceVisibility, KnowledgePurpose, KnowledgeRepository,
    MemoryAuthRepository, MemoryContentRepository, MemoryKnowledgeRepository,
    MemoryProjectRepository, ProjectCreate, ProjectRepository, ProjectSettings,
    ProjectStartCommand, SourceKind, TenantScope, hash_idempotency_key, settings_hash,
    start_request_hash,
};
use geo_worker::{
    ContentCloseRequest, ContentExecutionReadRequest, ContentItemsReadRequest, ContentStartRequest,
    ContentStepRequest, HostOpError, HostOpErrorCode, HostOps, ModelCompletion,
    ModelCompletionRequest,
};
use uuid::Uuid;

struct AcceptedExecutor(AtomicUsize);
impl ContentWorkflowExecutor for AcceptedExecutor {
    fn dispatch(
        &self,
        _scope: TenantScope,
        _execution_id: Uuid,
    ) -> Result<(), geo_domain::AppError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct GroundedModel(AtomicUsize);
#[async_trait]
impl ModelProviderBridge for GroundedModel {
    async fn complete(
        &self,
        _scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.0.fetch_add(1, Ordering::SeqCst);
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
                    "detail":"Source quote supports title"},
                {"block_id":block,"verdict":"supported","citation_ids":[citation],
                    "detail":"Source quote supports statement"}]})
            .to_string()
        } else {
            serde_json::json!({"title":"A documented answer","blocks":[{"kind":"paragraph",
                "text":"Public description","citation_ids":[citation],"items":[]}]})
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

#[tokio::test]
async fn host_ops_use_scoped_references_and_reject_foreign_or_tampered_cursor() {
    let tenant = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
    let projects = Arc::new(MemoryProjectRepository::default());
    let project = projects
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Host content".into(),
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
                        ..DocumentScope::default()
                    },
                    ..ProjectSettings::default()
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
                idempotency_key_hash: hash_idempotency_key("host-start"),
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
                name: "public".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Public description".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: started.document_manifest.manifest_id,
                knowledge_release_id: imported.items[0]
                    .release
                    .as_ref()
                    .unwrap()
                    .knowledge_release_id,
            },
            project.settings.document_scope.clone(),
        )
        .await
        .unwrap();
    let executor = Arc::new(AcceptedExecutor(AtomicUsize::new(0)));
    let model = Arc::new(GroundedModel(AtomicUsize::new(0)));
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password("unused")),
        projects,
        knowledge.clone(),
        EventBus::default(),
        false,
    )
    .with_content_repository(Arc::new(MemoryContentRepository::default()));
    state.configure_content_executor(executor.clone());
    let host = RepositoryHostOps::new(knowledge).with_content(state.clone());
    assert_eq!(
        host.content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(started.cycle_id)
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::CapabilityMissing
    );
    state.configure_content_model(model.clone());
    let started_ref = host
        .content_start(
            &scope,
            ContentStartRequest {
                cycle_id: Some(started.cycle_id),
            },
        )
        .await
        .unwrap();
    assert_eq!(started_ref.coverage.total, 2);
    assert_eq!(executor.0.load(Ordering::SeqCst), 1);
    let read = host
        .content_execution_read(
            &scope,
            ContentExecutionReadRequest {
                execution_id: started_ref.execution_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(read, started_ref);
    let first = host
        .content_items_read(
            &scope,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.total, 2);
    assert_eq!(first.items.len(), 1);
    let json = serde_json::to_string(&first).unwrap();
    for forbidden in [
        "quotes", "evidence", "brief", "document", "markdown", "provider", "token",
    ] {
        assert!(!json.contains(forbidden), "host page leaked {forbidden}");
    }
    let cursor = first.next_cursor.clone().unwrap();
    let second = host
        .content_items_read(
            &scope,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: Some(cursor.clone()),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_ne!(first.items[0].item_id, second.items[0].item_id);
    assert!(second.next_cursor.is_none());
    let tampered = format!("{cursor}x");
    assert_eq!(
        host.content_items_read(
            &scope,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: Some(tampered),
                limit: Some(1)
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::InvalidRequest
    );
    let foreign = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert_eq!(
        host.content_items_read(
            &foreign,
            ContentItemsReadRequest {
                execution_id: started_ref.execution_id,
                cursor: Some(cursor),
                limit: Some(1)
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.content_execution_read(
            &scope,
            ContentExecutionReadRequest {
                execution_id: Uuid::new_v4()
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(
        host.content_prepare(
            &scope,
            ContentStepRequest {
                execution_id: started_ref.execution_id,
                item_id: Uuid::new_v4()
            }
        )
        .await
        .unwrap_err()
        .code,
        HostOpErrorCode::NotFound
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 0);

    let item_id = first.items[0].item_id;
    let step = ContentStepRequest {
        execution_id: started_ref.execution_id,
        item_id,
    };
    let prepared = host.content_prepare(&scope, step.clone()).await.unwrap();
    assert_eq!(prepared.status, ContentItemStatus::Prepared);
    let projected = serde_json::to_string(&prepared).unwrap();
    assert!(!projected.contains("Public description"));
    let drafted = host.content_generate(&scope, step.clone()).await.unwrap();
    assert_eq!(drafted.status, ContentItemStatus::Drafted);
    let ready = host.content_check(&scope, step).await.unwrap();
    assert_eq!(ready.status, ContentItemStatus::Ready);
    let second_step = ContentStepRequest {
        execution_id: started_ref.execution_id,
        item_id: second.items[0].item_id,
    };
    host.content_prepare(&scope, second_step.clone())
        .await
        .unwrap();
    host.content_generate(&scope, second_step.clone())
        .await
        .unwrap();
    host.content_check(&scope, second_step).await.unwrap();
    assert_eq!(model.0.load(Ordering::SeqCst), 4);
    let handoff = host
        .content_close(
            &scope,
            ContentCloseRequest {
                execution_id: started_ref.execution_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(handoff.total, 2);
}
