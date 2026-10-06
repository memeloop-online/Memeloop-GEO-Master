use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Method, Request, StatusCode, Uri, header::SET_COOKIE},
    routing::any,
};
use chrono::Utc;
use geo_api::{
    AppState, BrowserBridge, CSRF_HEADER, ChannelDispatchDeferred, ChannelDispatchResult,
    ChannelService, EventBus, MemoryIdempotencyStore, MemoryOperationStore, execute_channel_target,
    router,
};
use geo_domain::{
    AppError, ChannelAccount, ChannelAccountRecord, ChannelOutcome, ChannelOutcomeStatus,
    ChannelOwnerKind, ChannelSecret, ChannelStatus, ChannelTarget, ChannelTargetInput,
    ChunkLocator, ConnectorKey, ConnectorVerification, ContentBlock, ContentBlockKind,
    ContentCoverage, ContentExecution, ContentExecutionStatus, ContentHandoff, ContentHandoffItem,
    ContentItemStatus, ContentRevision, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID,
    DocumentManifest, DocumentManifestCoverage, DocumentManifestItem, DocumentManifestItemState,
    DocumentManifestPlanRequest, DocumentManifestState, DocumentScope, EvidenceRef, Fact,
    FreezeDistribution, ImportAcceptance, ImportBatchAcceptance, ImportItem, KnowledgeAskResult,
    KnowledgeCapability, KnowledgeOverview, KnowledgePurpose, KnowledgeRelease,
    KnowledgeRepository, KnowledgeSearchRequest, KnowledgeSearchResult, MemoryAuthRepository,
    MemoryKnowledgeRepository, MemoryProjectRepository, PlatformPlacement, PreparedDistribution,
    Product, ProjectCreate, ProjectSettings, Source, SourceDetail, SourceKind, SourceVersion,
    StoredObject, StructuredDocument, TenantScope, UploadSession, UploadSessionCommand, sha256_hex,
};
use geo_provider::SecretEnvelope;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn public_channel_plan_cannot_forge_a_generated_publication() {
    let state = AppState::development_with_password("generated-test");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Generic test".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"login_name":"demo@localhost","password":"generated-test"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let response = axum::body::to_bytes(login.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let login: serde_json::Value = serde_json::from_slice(&response).unwrap();
    let csrf = login["csrf_token"].as_str().unwrap();
    let forged = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/projects/{}/cycles/{}/channel-plan?tenant_id={DEVELOPMENT_TENANT_ID}",
                    project.id,
                    Uuid::new_v4()
                ))
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .header(CSRF_HEADER, csrf)
                .body(Body::from(
                    json!({"publications":[{
                        "kind":"generated_publish",
                        "content_revision_id":Uuid::new_v4(),
                        "variant_id":Uuid::new_v4(),
                        "publication_intent_id":Uuid::new_v4(),
                        "distribution_target_id":Uuid::new_v4(),
                        "platform":"zhihu",
                        "account_id":Uuid::new_v4(),
                        "title":"forged","body":"forged",
                        "body_sha256":"forged","payload_hash":"forged","evidence":[]
                    }],"measurements":[]})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forged.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn generated_input_without_a_frozen_authoritative_bundle_never_claims() {
    let state = AppState::development_with_password("generated-test");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Generic test".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let command_id = Uuid::new_v4();
    state
        .channel_job_repository()
        .insert_generated_target(
            &scope,
            Uuid::new_v4(),
            command_id,
            ChannelTarget {
                target_id: command_id,
                input: ChannelTargetInput::GeneratedPublish {
                    content_revision_id: Uuid::new_v4(),
                    variant_id: Uuid::new_v4(),
                    publication_intent_id: Uuid::new_v4(),
                    distribution_target_id: Uuid::new_v4(),
                    platform: "zhihu".into(),
                    account_id: Uuid::new_v4(),
                    title: "untrusted".into(),
                    body: "untrusted".into(),
                    body_sha256: "untrusted".into(),
                    payload_hash: "untrusted".into(),
                    evidence: vec![],
                },
            },
        )
        .await
        .unwrap();
    let result = execute_channel_target(&state, &scope, command_id).await;
    assert!(!matches!(result, Ok(ChannelDispatchResult::Executed(_))));
    assert!(
        state
            .channel_job_repository()
            .get_target(&scope, command_id)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert!(
        state
            .channel_job_repository()
            .scan_pending(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.target_id == command_id)
    );
}

#[derive(Default)]
struct RevokingKnowledge {
    inner: MemoryKnowledgeRepository,
    revoked: AtomicBool,
}

#[async_trait]
impl KnowledgeRepository for RevokingKnowledge {
    async fn capabilities(&self, scope: &TenantScope) -> Result<KnowledgeCapability, AppError> {
        self.inner.capabilities(scope).await
    }
    async fn create_upload_session(
        &self,
        scope: &TenantScope,
        command: UploadSessionCommand,
    ) -> Result<UploadSession, AppError> {
        self.inner.create_upload_session(scope, command).await
    }
    async fn put_upload_content(
        &self,
        scope: &TenantScope,
        id: Uuid,
        content: Vec<u8>,
    ) -> Result<UploadSession, AppError> {
        self.inner.put_upload_content(scope, id, content).await
    }
    async fn complete_upload(
        &self,
        scope: &TenantScope,
        id: Uuid,
        key: &str,
    ) -> Result<ImportAcceptance, AppError> {
        self.inner.complete_upload(scope, id, key).await
    }
    async fn complete_attachment_upload(
        &self,
        scope: &TenantScope,
        id: Uuid,
        key: &str,
    ) -> Result<(StoredObject, String), AppError> {
        self.inner.complete_attachment_upload(scope, id, key).await
    }
    async fn get_attachment_object(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<(StoredObject, String)>, AppError> {
        self.inner.get_attachment_object(scope, id).await
    }
    async fn import_batch(
        &self,
        scope: &TenantScope,
        items: Vec<ImportItem>,
    ) -> Result<ImportBatchAcceptance, AppError> {
        self.inner.import_batch(scope, items).await
    }
    async fn list_sources(&self, scope: &TenantScope) -> Result<Vec<Source>, AppError> {
        let mut sources = self.inner.list_sources(scope).await?;
        if self.revoked.load(Ordering::SeqCst) {
            for source in &mut sources {
                source.purpose = KnowledgePurpose::Internal;
            }
        }
        Ok(sources)
    }
    async fn get_source(&self, scope: &TenantScope, id: Uuid) -> Result<Option<Source>, AppError> {
        self.inner.get_source(scope, id).await
    }
    async fn get_source_detail(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SourceDetail>, AppError> {
        self.inner.get_source_detail(scope, id).await
    }
    async fn get_source_version(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<SourceVersion>, AppError> {
        self.inner
            .get_source_version(scope, source_id, version_id)
            .await
    }
    async fn get_source_version_content(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<geo_domain::SourceVersionContent>, AppError> {
        self.inner
            .get_source_version_content(scope, source_id, version_id)
            .await
    }
    async fn revise_source_text(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        command: geo_domain::ReviseSourceTextCommand,
    ) -> Result<geo_domain::SourceTextRevisionReceipt, AppError> {
        self.inner
            .revise_source_text(
                scope,
                source_id,
                expected_revision,
                idempotency_key,
                command,
            )
            .await
    }
    async fn list_products(&self, scope: &TenantScope) -> Result<Vec<Product>, AppError> {
        self.inner.list_products(scope).await
    }
    async fn list_facts(&self, scope: &TenantScope) -> Result<Vec<Fact>, AppError> {
        self.inner.list_facts(scope).await
    }
    async fn current_release(
        &self,
        scope: &TenantScope,
    ) -> Result<geo_domain::CurrentKnowledgeRelease, AppError> {
        self.inner.current_release(scope).await
    }
    async fn get_release(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<KnowledgeRelease>, AppError> {
        self.inner.get_release(scope, id).await
    }
    async fn get_document_manifest(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<DocumentManifest>, AppError> {
        self.inner.get_document_manifest(scope, id).await
    }
    async fn plan_document_manifest(
        &self,
        scope: &TenantScope,
        request: DocumentManifestPlanRequest,
        document_scope: DocumentScope,
    ) -> Result<DocumentManifest, AppError> {
        self.inner
            .plan_document_manifest(scope, request, document_scope)
            .await
    }
    async fn search(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, AppError> {
        self.inner.search(scope, request).await
    }
    async fn ask(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeAskResult, AppError> {
        self.inner.ask(scope, request).await
    }
    async fn overview(&self, scope: &TenantScope) -> Result<KnowledgeOverview, AppError> {
        self.inner.overview(scope).await
    }
}

#[derive(Clone, Default)]
struct RunnerCalls {
    sends: Arc<Mutex<Vec<serde_json::Value>>>,
    revoke_on_complete: Arc<AtomicBool>,
    revoked: Arc<RevokingKnowledge>,
    runner_version: Arc<Mutex<String>>,
    connector_settings: Arc<Mutex<Option<AppState>>>,
}

async fn runner(
    State(calls): State<RunnerCalls>,
    method: Method,
    uri: Uri,
    payload: Option<Json<serde_json::Value>>,
) -> (StatusCode, Json<serde_json::Value>) {
    match (method.as_str(), uri.path()) {
        ("GET", "/v1/capabilities") => {
            let version = calls.runner_version.lock().await.clone();
            (
                StatusCode::OK,
                Json(json!({"connectors":[{"platform":"zhihu",
                "placement_slot":"primary","connector_version":version,
                "operations":["publish"],"verified":false}]})),
            )
        }
        ("POST", "/v1/sessions") => (
            StatusCode::OK,
            Json(json!({"session_id":payload.unwrap().0["session_id"]})),
        ),
        ("POST", "/v1/executions") => {
            let payload = payload.unwrap().0;
            calls.sends.lock().await.push(payload.clone());
            (
                StatusCode::OK,
                Json(json!({
                    "execution_id":payload["execution_id"],
                    "status":"unsupported",
                    "provenance":"fixture",
                    "evidence":[]
                })),
            )
        }
        ("POST", path) if path.ends_with("/complete") => {
            if calls.revoke_on_complete.load(Ordering::SeqCst) {
                calls.revoked.revoked.store(true, Ordering::SeqCst);
            }
            if let Some(state) = calls.connector_settings.lock().await.take() {
                state
                    .connector_capability_repository()
                    .configure(
                        geo_domain::DEVELOPMENT_OPERATOR_ID,
                        ConnectorKey {
                            platform_id: "zhihu".into(),
                            placement_slot: "primary".into(),
                        },
                        1,
                        false,
                        vec![],
                        "",
                    )
                    .await
                    .unwrap();
            }
            (
                StatusCode::OK,
                Json(json!({
                    "identity":{"platform_account_id":"test-identity","display_name":"test"},
                    "storage_state":{"cookies":[],"origins":[]}
                })),
            )
        }
        _ => (StatusCode::OK, Json(json!({"closed":true}))),
    }
}

struct FrozenFixture {
    state: AppState,
    scope: TenantScope,
    command_id: Uuid,
    revision: ContentRevision,
    calls: RunnerCalls,
    knowledge: Arc<RevokingKnowledge>,
    server: tokio::task::JoinHandle<()>,
}

async fn frozen_fixture(fixture: bool) -> FrozenFixture {
    let knowledge = Arc::new(RevokingKnowledge::default());
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password(
            "generated-test",
        )),
        Arc::new(MemoryProjectRepository::default()),
        knowledge.clone(),
        EventBus::default(),
        false,
    );
    let root = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &root,
            ProjectCreate {
                slug: None,
                display_name: "Generic fixture".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(root.operator_id, root.tenant_id, Some(project.id));
    let account_id = Uuid::new_v4();
    let key = "b6".repeat(32);
    let aad = format!(
        "geo-channel-v1:{}:{}:{}:{}:session",
        scope.operator_id, scope.tenant_id, project.id, account_id
    );
    let encrypted = SecretEnvelope::from_hex_key(&key)
        .unwrap()
        .seal(aad.as_bytes(), br#"{"cookies":[],"origins":[]}"#)
        .unwrap();
    let channels = state.channel_service().repository.clone();
    channels
        .save_account(
            &scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id,
                    project_id: project.id,
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "zhihu".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: None,
                    platform_account_id: Some("test-identity".into()),
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: false,
                    proxy_server: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                session: Some(ChannelSecret::new(encrypted)),
                proxy: None,
            },
        )
        .await
        .unwrap();
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "generic".into(),
                kind: SourceKind::Text,
                name: "Generic source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Supported claim.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap()
        .items
        .remove(0);
    let source_version_id = imported.source_version.unwrap().source_version_id;
    let evidence = EvidenceRef {
        source_version_id,
        chunk_id: None,
        locator: ChunkLocator::Manual {},
    };
    let document = StructuredDocument {
        title: "Frozen document".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Supported claim.".into(),
            citation_ids: vec![],
            items: vec![],
        }],
    };
    let revision = ContentRevision {
        revision_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        derived_from_revision_id: None,
        markdown: document.markdown(),
        document,
        evidence: vec![evidence],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    };
    let document_id = Uuid::new_v4();
    let item_id = Uuid::new_v4();
    let execution_id = Uuid::new_v4();
    let handoff_id = Uuid::new_v4();
    let cycle_id = Uuid::new_v4();
    let coverage = ContentCoverage {
        total: 1,
        ready: 1,
        blocked: 0,
        deferred: 0,
        not_applicable: 0,
        cancelled: 0,
        incomplete: 0,
    };
    let repository = state.distribution_repository();
    let manifest = repository
        .freeze(
            &scope,
            FreezeDistribution {
                cycle_id,
                revision: 1,
                document_manifest: DocumentManifest {
                    manifest_id: document_id,
                    operator_id: scope.operator_id,
                    tenant_id: scope.tenant_id,
                    project_id: project.id,
                    revision: 1,
                    knowledge_release_id: imported.release.unwrap().knowledge_release_id,
                    planner_version: "fixture".into(),
                    state: DocumentManifestState::Closed,
                    sealed: true,
                    expected_count: Some(1),
                    scope_hash: "generic".into(),
                    items: vec![DocumentManifestItem {
                        document_manifest_item_id: item_id,
                        manifest_id: document_id,
                        knowledge_release_id: Uuid::new_v4(),
                        document_key: "generic-document".into(),
                        content_type: "faq".into(),
                        product_id: None,
                        market: "CN".into(),
                        language: "en".into(),
                        state: DocumentManifestItemState::Planned,
                        block_reason: None,
                        dependency_hash: "generic".into(),
                        source_version_refs: vec![source_version_id],
                    }],
                    coverage: DocumentManifestCoverage {
                        total: 1,
                        planned: 1,
                        ..Default::default()
                    },
                },
                content_execution: ContentExecution {
                    execution_id,
                    project_id: project.id,
                    cycle_id,
                    manifest_id: document_id,
                    manifest_revision: 1,
                    policy_version: "fixture".into(),
                    input_hash: "generic".into(),
                    status: ContentExecutionStatus::Closed,
                    expected_count: 1,
                    coverage: coverage.clone(),
                    handoff_id: Some(handoff_id),
                },
                content_handoff: ContentHandoff {
                    handoff_id,
                    execution_id,
                    revision: 1,
                    supersedes_handoff_id: None,
                    coverage,
                    items: vec![ContentHandoffItem {
                        item_id,
                        document_key: "generic-document".into(),
                        status: ContentItemStatus::Ready,
                        reason: None,
                        revision_id: Some(revision.revision_id),
                    }],
                    created_at: Utc::now(),
                },
                placements: vec![PlatformPlacement {
                    platform_id: "zhihu".into(),
                    placement_slot: "primary".into(),
                    capability_version: "test-live.v1".into(),
                    supported_formats: vec!["faq".into()],
                    unavailable_reason: None,
                    fixture,
                }],
                sealed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let target = repository
        .expansion_page(&scope, manifest.manifest_id, 0, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    repository
        .commit_expansion_page(&scope, manifest.manifest_id, 0, vec![target.clone()])
        .await
        .unwrap();
    let materialized = repository
        .materialize(
            &scope,
            PreparedDistribution {
                manifest_id: manifest.manifest_id,
                target_id: target.target_id,
                revision: Some(revision.clone()),
                account_id: Some(account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    let command = materialized.publication_commands[0].clone();
    let variant = materialized.variant.unwrap();
    let intent = materialized.intent.unwrap();
    let input = ChannelTargetInput::GeneratedPublish {
        content_revision_id: revision.revision_id,
        variant_id: variant.variant_id,
        publication_intent_id: intent.intent_id,
        distribution_target_id: target.target_id,
        platform: "zhihu".into(),
        account_id,
        title: variant.title.clone(),
        body_sha256: sha256_hex(variant.markdown.as_bytes()),
        body: variant.markdown,
        payload_hash: variant.payload_hash,
        evidence: revision.evidence.clone(),
    };
    state
        .channel_job_repository()
        .insert_generated_target(
            &scope,
            cycle_id,
            command.command_id,
            ChannelTarget {
                target_id: command.command_id,
                input,
            },
        )
        .await
        .unwrap();
    let calls = RunnerCalls {
        sends: Arc::default(),
        revoke_on_complete: Arc::default(),
        revoked: knowledge.clone(),
        runner_version: Arc::new(Mutex::new("test-live.v1".into())),
        connector_settings: Arc::new(Mutex::new(None)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let runner_calls = calls.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(runner)).with_state(runner_calls),
        )
        .await
        .unwrap();
    });
    let bridge = BrowserBridge::new(format!("http://{address}"), "test-token".into()).unwrap();
    if !fixture {
        let repo = state.connector_capability_repository();
        let now = Utc::now();
        let url = "https://example.com/public/readback".to_owned();
        let hash = "a".repeat(64);
        let published = ChannelOutcome {
            status: ChannelOutcomeStatus::Published,
            detail: None,
            occurred_at: now,
            raw_answer: None,
            citations: vec![],
            public_url: Some(url.clone()),
            screenshot_ref: None,
            connector_version: Some("test-live.v1".into()),
            runner_evidence: vec![],
            fixture: false,
        };
        repo.insert_verification(
            scope.operator_id,
            ConnectorVerification {
                verification_id: Uuid::new_v4(),
                key: ConnectorKey {
                    platform_id: "zhihu".into(),
                    placement_slot: "primary".into(),
                },
                connector_version: "test-live.v1".into(),
                content_type: "faq".into(),
                publication_receipt: published.clone(),
                public_readback: ChannelOutcome {
                    status: ChannelOutcomeStatus::Verified,
                    runner_evidence: vec![json!({"kind":"public_readback","url":url,
                    "content_matched":true,"owned_by_account":true,
                    "expected_sha256":hash,"readback_sha256":hash})],
                    ..published
                },
                verified_at: now,
            },
        )
        .await
        .unwrap();
        repo.configure(
            scope.operator_id,
            ConnectorKey {
                platform_id: "zhihu".into(),
                placement_slot: "primary".into(),
            },
            0,
            true,
            vec!["faq".into()],
            "test-live.v1",
        )
        .await
        .unwrap();
    }
    let service = ChannelService::persistent(channels, &key, Some(bridge)).unwrap();
    FrozenFixture {
        state: state.with_channel_service(service),
        scope,
        command_id: command.command_id,
        revision,
        calls,
        knowledge,
        server,
    }
}

#[tokio::test]
async fn fixture_command_never_opens_a_runner_context_or_claims() {
    let fixture = frozen_fixture(true).await;
    assert!(matches!(
        execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::FixtureOnly)
    ));
    assert!(fixture.calls.sends.lock().await.is_empty());
    assert!(
        fixture
            .state
            .channel_job_repository()
            .get_target(&fixture.scope, fixture.command_id)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    fixture.server.abort();
}

#[tokio::test]
async fn generated_publication_uses_frozen_revision_bytes_and_claims_once() {
    let fixture = frozen_fixture(false).await;
    let expected = fixture.revision.markdown.clone();
    let result = execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
        .await
        .unwrap();
    let ChannelDispatchResult::Executed(view) = result else {
        panic!("genuine command should execute");
    };
    assert_eq!(view.attempts.len(), 1);
    let sends = fixture.calls.sends.lock().await;
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["payload"]["title"], "Frozen document");
    assert_eq!(sends[0]["payload"]["body"], expected);
    drop(sends);
    assert!(
        execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
            .await
            .is_err()
    );
    fixture.server.abort();
}

#[tokio::test]
async fn operator_disabling_connector_before_send_preserves_unattempted_frozen_target() {
    let fixture = frozen_fixture(false).await;
    let key = ConnectorKey {
        platform_id: "zhihu".into(),
        placement_slot: "primary".into(),
    };
    fixture
        .state
        .connector_capability_repository()
        .configure(fixture.scope.operator_id, key, 1, false, vec![], "")
        .await
        .unwrap();
    assert!(matches!(
        execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::ConnectorUnavailable)
    ));
    assert!(fixture.calls.sends.lock().await.is_empty());
    assert!(
        fixture
            .state
            .channel_job_repository()
            .get_target(&fixture.scope, fixture.command_id)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    fixture.server.abort();
}

#[tokio::test]
async fn deployed_version_drift_preserves_frozen_coverage_and_blocks_send() {
    let fixture = frozen_fixture(false).await;
    let before = fixture
        .state
        .distribution_repository()
        .get_publication_bundle(
            &fixture.scope,
            match &fixture
                .state
                .channel_job_repository()
                .get_target(&fixture.scope, fixture.command_id)
                .await
                .unwrap()
                .target
                .input
            {
                ChannelTargetInput::GeneratedPublish {
                    publication_intent_id,
                    ..
                } => *publication_intent_id,
                _ => unreachable!(),
            },
        )
        .await
        .unwrap();
    *fixture.calls.runner_version.lock().await = "test-live.v2".into();
    assert!(matches!(
        execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::ConnectorUnavailable)
    ));
    assert_eq!(
        fixture
            .state
            .distribution_repository()
            .get(&fixture.scope, before.target.manifest_id)
            .await
            .unwrap()
            .platform_scope[0]
            .capability_version,
        "test-live.v1"
    );
    assert!(
        fixture
            .state
            .channel_job_repository()
            .get_target(&fixture.scope, fixture.command_id)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert!(fixture.calls.sends.lock().await.is_empty());
    fixture.server.abort();
}

#[tokio::test]
async fn verified_other_format_does_not_authorize_frozen_faq() {
    let fixture = frozen_fixture(false).await;
    let key = ConnectorKey {
        platform_id: "zhihu".into(),
        placement_slot: "primary".into(),
    };
    let repo = fixture.state.connector_capability_repository();
    let mut proof = repo
        .history(fixture.scope.operator_id, &key)
        .await
        .unwrap()
        .remove(0);
    proof.verification_id = Uuid::new_v4();
    proof.content_type = "company_profile".into();
    repo.insert_verification(fixture.scope.operator_id, proof)
        .await
        .unwrap();
    repo.configure(
        fixture.scope.operator_id,
        key,
        1,
        true,
        vec!["company_profile".into()],
        "test-live.v1",
    )
    .await
    .unwrap();
    assert!(matches!(
        execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::ConnectorUnavailable)
    ));
    assert!(fixture.calls.sends.lock().await.is_empty());
    fixture.server.abort();
}

#[tokio::test]
async fn revocation_during_browser_preflight_records_attempt_without_sending() {
    let fixture = frozen_fixture(false).await;
    *fixture.calls.connector_settings.lock().await = Some(fixture.state.clone());
    let result = execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
        .await
        .unwrap();
    let ChannelDispatchResult::Executed(view) = result else {
        panic!("post-claim connector revocation must retain the attempt");
    };
    assert_eq!(view.attempts.len(), 1);
    assert!(fixture.calls.sends.lock().await.is_empty());
    fixture.server.abort();
}

#[tokio::test]
async fn revoked_source_before_claim_consumes_no_attempt_and_does_not_send() {
    let fixture = frozen_fixture(false).await;
    fixture.knowledge.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::SourceUnavailable)
    ));
    assert!(fixture.calls.sends.lock().await.is_empty());
    assert!(
        fixture
            .state
            .channel_job_repository()
            .get_target(&fixture.scope, fixture.command_id)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    fixture.server.abort();
}

#[tokio::test]
async fn revoked_source_during_runner_preflight_claims_but_never_sends() {
    let fixture = frozen_fixture(false).await;
    fixture
        .calls
        .revoke_on_complete
        .store(true, Ordering::SeqCst);
    let result = execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
        .await
        .unwrap();
    let ChannelDispatchResult::Executed(view) = result else {
        panic!("revocation after preflight must record a claimed outcome");
    };
    assert_eq!(view.attempts.len(), 1);
    assert_eq!(
        view.attempts[0].outcome.as_ref().unwrap().status,
        geo_domain::ChannelOutcomeStatus::Unsupported
    );
    assert!(fixture.calls.sends.lock().await.is_empty());
    assert!(
        execute_channel_target(&fixture.state, &fixture.scope, fixture.command_id)
            .await
            .is_err()
    );
    fixture.server.abort();
}
