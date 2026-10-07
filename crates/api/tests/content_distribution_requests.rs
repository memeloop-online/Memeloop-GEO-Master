use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::Utc;
use geo_api::{
    AppState, ChannelService, ContentService, EventBus, MemoryIdempotencyStore,
    MemoryOperationStore, ModelProviderBridge, router,
};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOutcome, ChannelOutcomeStatus, ChannelOwnerKind,
    ChannelRepository, ChannelStatus, ConnectorKey, ConnectorVerification, ContentRepository,
    ContentStep, DistributionRepository, DocumentManifestPlanRequest, DocumentScope, ImportItem,
    InitialSource, InitialSourceKind, InitialSourceVisibility, KnowledgePurpose,
    KnowledgeRepository, Membership, MemoryAuthRepository, MemoryChannelRepository,
    MemoryConnectorCapabilityRepository, MemoryContentRepository, MemoryDistributionRepository,
    MemoryKnowledgeRepository, MemoryProjectRepository, ProjectCreate, ProjectRepository,
    ProjectSettings, ProjectStartCommand, ReviseSourceTextCommand, Role, SourceKind, TenantScope,
    User, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_worker::{HostOpError, ModelCompletion, ModelCompletionRequest};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct FixtureModel;

#[async_trait]
impl ModelProviderBridge for FixtureModel {
    async fn complete(
        &self,
        _: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let input: Value = serde_json::from_str(&request.prompt).unwrap();
        let citation = input["evidence"][0]["chunk_id"].as_str().unwrap();
        Ok(ModelCompletion {
            text: json!({"title":"Generic article","blocks":[{
                "kind":"paragraph", "text":"Public example", "citation_ids":[citation], "items":[]
            }]})
            .to_string(),
            tool_calls: vec![],
            model: "fixture".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop".into(),
        })
    }
}

struct Fixture {
    app: axum::Router,
    state: AppState,
    distribution: Arc<MemoryDistributionRepository>,
    knowledge: Arc<MemoryKnowledgeRepository>,
    channels: Arc<MemoryChannelRepository>,
    scope: TenantScope,
    project: String,
    tenant: String,
    cookie: String,
    csrf: String,
    content_asset_id: Uuid,
    content_revision_id: Uuid,
    account_id: Uuid,
    source_id: Uuid,
    source_version_id: Uuid,
}

fn req(
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    key: Option<&str>,
    body: Value,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    if let Some(key) = key {
        builder = builder.header("Idempotency-Key", key);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn call(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 32 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn login(app: &axum::Router, name: &str, password: &str) -> (String, String) {
    let response = app
        .clone()
        .oneshot(req(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            None,
            json!({"login_name":name,"password":password}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

async fn fixture() -> Fixture {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "fixture-pass",
    ));
    let reader = User::new(
        Uuid::new_v4().into(),
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        "reader@localhost",
        "Reader",
        "reader-pass",
    )
    .unwrap();
    auth.insert_user(reader.clone()).await.unwrap();
    auth.insert_membership(Membership::new(
        reader.id,
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        geo_domain::DEVELOPMENT_TENANT_ID,
        Role::CustomerReadOnly,
    ))
    .await
    .unwrap();
    let projects = Arc::new(MemoryProjectRepository::default());
    let tenant = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        geo_domain::DEVELOPMENT_TENANT_ID,
        None,
    );
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
                        value: "Public example".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    document_scope: DocumentScope {
                        content_types: vec!["faq".into()],
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
                idempotency_key_hash: hash_idempotency_key("fixture-start"),
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
                name: "generic source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Public example".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let mut document_scope = project.settings.document_scope.clone();
    document_scope.markets = project.settings.effective_markets();
    document_scope.languages = project.settings.effective_languages();
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
            document_scope,
        )
        .await
        .unwrap();
    let content = Arc::new(MemoryContentRepository::default());
    let service = ContentService::new(content.clone(), knowledge.clone(), projects.clone())
        .with_model_provider(Arc::new(FixtureModel));
    let execution = service.start(&scope, started.cycle_id).await.unwrap();
    let item = content
        .list_items(&scope, execution.execution_id)
        .await
        .unwrap()
        .remove(0);
    service
        .prepare(&scope, execution.execution_id, item.item_id)
        .await
        .unwrap();
    let revision = service
        .generate(&scope, execution.execution_id, item.item_id)
        .await
        .unwrap();
    let lease = content
        .claim(
            &scope,
            execution.execution_id,
            item.item_id,
            ContentStep::Check,
            "request-fixture",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    content
        .complete_check(&scope, &lease, vec![])
        .await
        .unwrap();
    let channels = Arc::new(MemoryChannelRepository::default());
    let now = Utc::now();
    let account_id = Uuid::new_v4();
    channels
        .save_account(
            &scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id,
                    project_id: project.id,
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "generic".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: None,
                    platform_account_id: None,
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
    let distribution = Arc::new(MemoryDistributionRepository::default());
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        projects,
        knowledge.clone(),
        EventBus::default(),
        false,
    )
    .with_content_repository(content)
    .with_channel_service(ChannelService::unconfigured(channels.clone()))
    .with_distribution_repository(distribution.clone());
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "fixture-pass").await;
    Fixture {
        app,
        state,
        distribution,
        knowledge,
        channels,
        scope,
        project: project.id.to_string(),
        tenant: tenant.tenant_id.to_string(),
        cookie,
        csrf,
        content_asset_id: revision.asset_id,
        content_revision_id: revision.revision_id,
        account_id,
        source_id: imported.items[0].source.as_ref().unwrap().source_id,
        source_version_id: imported.items[0]
            .source_version
            .as_ref()
            .unwrap()
            .source_version_id,
    }
}

impl Fixture {
    async fn prove_text_connector(&self) {
        let registry = self.state.connector_capability_repository();
        let key = ConnectorKey {
            platform_id: "generic".into(),
            placement_slot: "primary".into(),
        };
        let at = Utc::now();
        let url = "https://example.org/articles/100".to_owned();
        let receipt = ChannelOutcome {
            status: ChannelOutcomeStatus::Published,
            detail: None,
            occurred_at: at,
            raw_answer: None,
            citations: vec![],
            public_url: Some(url.clone()),
            screenshot_ref: None,
            connector_version: Some("live.v1".into()),
            runner_evidence: vec![],
            fixture: false,
        };
        let readback = ChannelOutcome {
            status: ChannelOutcomeStatus::Verified,
            runner_evidence: vec![json!({
                "kind":"public_readback",
                "url":url,
                "content_matched":true,
                "owned_by_account":true,
                "expected_sha256":"a".repeat(64),
                "readback_sha256":"a".repeat(64),
            })],
            ..receipt.clone()
        };
        registry
            .insert_verification(
                self.scope.operator_id,
                ConnectorVerification {
                    verification_id: Uuid::new_v4(),
                    key: key.clone(),
                    connector_version: "live.v1".into(),
                    content_type: "faq".into(),
                    publication_receipt: receipt,
                    public_readback: readback,
                    verified_at: at,
                },
            )
            .await
            .unwrap();
        registry
            .configure(
                self.scope.operator_id,
                key,
                0,
                true,
                vec!["faq".into()],
                "live.v1",
            )
            .await
            .unwrap();
    }

    fn path(&self) -> String {
        format!(
            "/api/v1/projects/{}/content-distribution-requests?tenant_id={}",
            self.project, self.tenant
        )
    }

    fn body(&self) -> Value {
        json!({
            "content_asset_id":self.content_asset_id,
            "content_revision_id":self.content_revision_id,
            "account_id":self.account_id,
            "placement_slot":"primary",
            "format":"markdown.v1"
        })
    }

    async fn post(&self, key: Option<&str>, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            req(
                "POST",
                &self.path(),
                Some(&self.cookie),
                Some(&self.csrf),
                key,
                body,
            ),
        )
        .await
    }

    async fn get(&self, project: &str, id: &str, suffix: &str) -> (StatusCode, Value) {
        call(
            &self.app,
            req(
                "GET",
                &format!(
                    "/api/v1/projects/{project}/content-distribution-requests/{id}{suffix}?tenant_id={}",
                    self.tenant
                ),
                Some(&self.cookie),
                None,
                None,
                Value::Null,
            ),
        )
        .await
    }
}

#[tokio::test]
async fn accepted_request_freezes_revision_and_account_without_claiming_delivery() {
    let f = fixture().await;
    let (status, first) = f.post(Some("stable"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(
        first["content_revision_id"],
        f.content_revision_id.to_string()
    );
    assert_eq!(first["content_asset_id"], f.content_asset_id.to_string());
    assert_eq!(first["account_id"], f.account_id.to_string());
    assert_eq!(first["platform_id"], "generic");
    assert_eq!(first["account_owner_kind"], "customer");
    assert_eq!(first["publication_intent_id"], Value::Null);
    let id = first["request_id"].as_str().unwrap();
    let (again_status, again) = f.post(Some("stable"), f.body()).await;
    assert_eq!(again_status, StatusCode::ACCEPTED);
    assert_eq!(again["request_id"], first["request_id"]);
    let (status, read) = f.get(&f.project, id, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, first);
    let (status, state) = f.get(&f.project, id, "/publication").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["publication_intent_id"], Value::Null);
    assert_eq!(state["channel_target_id"], Value::Null);
    assert_eq!(state["attempt_id"], Value::Null);
    assert_eq!(state["outcome"], Value::Null);
    assert_eq!(state["fixture"], Value::Null);
    let (status, _) = f.get(&Uuid::new_v4().to_string(), id, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = f.get(&f.project, &Uuid::new_v4().to_string(), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn conflicting_keys_and_strict_or_unavailable_fields_are_rejected() {
    let f = fixture().await;
    assert_eq!(f.post(None, f.body()).await.0, StatusCode::BAD_REQUEST);
    let (status, _) = f.post(Some("stable"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let mut changed = f.body();
    changed["placement_slot"] = json!("secondary");
    assert_eq!(
        f.post(Some("stable"), changed).await.0,
        StatusCode::CONFLICT
    );
    let mut unknown = f.body();
    unknown["cycle_id"] = json!(Uuid::new_v4());
    assert_eq!(
        f.post(Some("unknown"), unknown).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut missing_revision = f.body();
    missing_revision["content_revision_id"] = json!(Uuid::new_v4());
    assert_eq!(
        f.post(Some("missing-revision"), missing_revision).await.0,
        StatusCode::NOT_FOUND
    );
    let mut missing_account = f.body();
    missing_account["account_id"] = json!(Uuid::new_v4());
    assert_eq!(
        f.post(Some("missing-account"), missing_account).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn accepted_key_replays_after_account_revocation_and_conflicts_before_authority_lookup() {
    let f = fixture().await;
    let (status, accepted) = f.post(Some("retry-after-revocation"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    f.channels
        .delete_account(&f.scope, f.account_id)
        .await
        .unwrap();
    let (status, replayed) = f.post(Some("retry-after-revocation"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replayed["request_id"], accepted["request_id"]);
    let mut changed = f.body();
    changed["account_id"] = json!(Uuid::new_v4());
    assert_eq!(
        f.post(Some("retry-after-revocation"), changed).await.0,
        StatusCode::CONFLICT
    );
    let (status, _) = f.post(Some("fresh-key"), f.body()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn checked_single_article_materializes_one_origin_outbox_without_a_cycle() {
    let f = fixture().await;
    f.prove_text_connector().await;
    let (status, first) = f.post(Some("publish-checked"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    let request_id = Uuid::parse_str(first["request_id"].as_str().unwrap()).unwrap();
    let intent_id = Uuid::parse_str(first["publication_intent_id"].as_str().unwrap()).unwrap();
    let commands = f.distribution.publication_commands(&f.scope).await;
    assert_eq!(commands.len(), 1);
    assert!(commands[0].target_id.is_nil());
    assert_eq!(commands[0].intent_id, intent_id);
    let bundle = f
        .distribution
        .get_publication_bundle(&f.scope, intent_id)
        .await
        .unwrap();
    assert!(
        matches!(bundle.origin, geo_domain::PublicationOrigin::ContentRequest { request }
        if request.request_id == request_id)
    );
    assert_eq!(bundle.command.command_id, commands[0].command_id);
    assert_eq!(f.post(Some("publish-checked"), f.body()).await.1, first);
    let (status, distinct) = f.post(Some("another-accepted-request"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{distinct}");
    assert_ne!(distinct["request_id"], first["request_id"]);
    assert_eq!(
        distinct["publication_intent_id"],
        first["publication_intent_id"]
    );
    assert_eq!(f.distribution.publication_commands(&f.scope).await.len(), 1);
    assert!(matches!(
        f.distribution
            .get_publication_bundle(&f.scope, intent_id)
            .await
            .unwrap()
            .origin,
        geo_domain::PublicationOrigin::ContentRequest { request }
            if request.request_id == request_id
    ));
    let other = TenantScope::new(
        f.scope.operator_id,
        f.scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        f.state
            .content_distribution_request_repository()
            .get(&other, request_id)
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::NotFound
    );
}

#[tokio::test]
async fn builder_authority_overrides_are_fresh_without_mutating_sibling_state() {
    let f = fixture().await;
    let (status, accepted) = f.post(Some("pending-builder"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let id = Uuid::parse_str(accepted["request_id"].as_str().unwrap()).unwrap();
    f.prove_text_connector().await;
    // Each fork retains accepted rows, but validates against its own latest
    // channel/content/capability snapshot. The original still publishes.
    let empty_channels = Arc::new(MemoryChannelRepository::default());
    let no_account = f
        .state
        .clone()
        .with_channel_service(ChannelService::unconfigured(empty_channels));
    assert_eq!(
        no_account
            .content_distribution_request_repository()
            .materialize(&f.scope, id)
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::Conflict
    );
    let no_content = f
        .state
        .clone()
        .with_content_repository(Arc::new(MemoryContentRepository::default()));
    assert_eq!(
        no_content
            .content_distribution_request_repository()
            .materialize(&f.scope, id)
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::Conflict
    );
    let no_proof = f
        .state
        .clone()
        .with_connector_capability_repository(Arc::new(
            MemoryConnectorCapabilityRepository::default(),
        ));
    assert_eq!(
        no_proof
            .content_distribution_request_repository()
            .materialize(&f.scope, id)
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::Conflict
    );
    let original_distribution = f.state.distribution_repository();
    let fork_distribution = Arc::new(MemoryDistributionRepository::default());
    let fork = f
        .state
        .clone()
        .with_distribution_repository(fork_distribution.clone());
    assert!(Arc::ptr_eq(
        &original_distribution,
        &f.state.distribution_repository()
    ));
    assert!(!Arc::ptr_eq(
        &original_distribution,
        &fork.distribution_repository()
    ));
    let linked = f
        .state
        .content_distribution_request_repository()
        .materialize(&f.scope, id)
        .await
        .unwrap();
    let intent_id = linked.publication_intent_id.unwrap();
    assert_eq!(f.distribution.publication_commands(&f.scope).await.len(), 1);
    assert!(
        fork_distribution
            .publication_commands(&f.scope)
            .await
            .is_empty()
    );
    assert_eq!(
        fork.distribution_repository()
            .get_publication_bundle(&f.scope, intent_id)
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::NotFound
    );
}

#[tokio::test]
async fn revoked_source_or_account_keeps_accepted_memory_request_unlinked() {
    let f = fixture().await;
    let (status, accepted) = f.post(Some("pending-source"), f.body()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let id = Uuid::parse_str(accepted["request_id"].as_str().unwrap()).unwrap();
    f.prove_text_connector().await;
    let source = f
        .knowledge
        .get_source(&f.scope, f.source_id)
        .await
        .unwrap()
        .unwrap();
    f.knowledge
        .revise_source_text(
            &f.scope,
            f.source_id,
            source.revision,
            "new-source-version",
            ReviseSourceTextCommand {
                base_version_id: f.source_version_id,
                media_type: "text/plain".into(),
                text: "Updated public source".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.state
            .content_distribution_request_repository()
            .materialize(&f.scope, id)
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::Conflict
    );
    assert!(
        f.distribution
            .publication_commands(&f.scope)
            .await
            .is_empty()
    );

    let f = fixture().await;
    let (_, accepted) = f.post(Some("pending-account"), f.body()).await;
    let id = Uuid::parse_str(accepted["request_id"].as_str().unwrap()).unwrap();
    f.prove_text_connector().await;
    f.channels
        .delete_account(&f.scope, f.account_id)
        .await
        .unwrap();
    assert_eq!(
        f.state
            .content_distribution_request_repository()
            .materialize(&f.scope, id)
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::Conflict
    );
    assert!(
        f.distribution
            .publication_commands(&f.scope)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn writer_permission_and_csrf_are_required() {
    let f = fixture().await;
    let (reader_cookie, reader_csrf) = login(&f.app, "reader@localhost", "reader-pass").await;
    let (status, _) = call(
        &f.app,
        req(
            "POST",
            &f.path(),
            Some(&reader_cookie),
            Some(&reader_csrf),
            Some("reader"),
            f.body(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &f.app,
        req(
            "POST",
            &f.path(),
            Some(&f.cookie),
            None,
            Some("no-csrf"),
            f.body(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
