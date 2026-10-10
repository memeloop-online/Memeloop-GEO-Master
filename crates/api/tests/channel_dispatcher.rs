use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{Duration, Utc};
use geo_api::{
    AppState, BrowserBridge, ChannelDispatchDeferred, ChannelDispatchResult, ChannelService,
    EventBus, MemoryIdempotencyStore, MemoryOperationStore, execute_channel_target,
};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelJobRepository, ChannelOwnerKind, ChannelPlan,
    ChannelSecret, ChannelStatus, ChannelTarget, ChannelTargetInput, ConnectorKey,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ImportItem, KnowledgePurpose,
    MemoryAuthRepository, MemoryChannelRepository, MemoryProjectRepository, ProjectCreate,
    ProjectRepository, ProjectSettings, ProjectStatus, SourceKind, TenantScope,
};
use geo_provider::SecretEnvelope;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone)]
struct Runner {
    sends: Arc<AtomicUsize>,
    completes: Arc<AtomicUsize>,
    failure: RunnerFailure,
}

#[derive(Clone, Copy, Default)]
enum RunnerFailure {
    #[default]
    None,
    LostReceipt,
    FinalIdentity,
}

async fn mock_runner(
    State(runner): State<Runner>,
    method: Method,
    uri: Uri,
    payload: Option<Json<Value>>,
) -> Response {
    if method == Method::POST
        && uri.path() == "/v1/executions"
        && matches!(runner.failure, RunnerFailure::LostReceipt)
    {
        // The runner accepted the one-shot request, then the response transport
        // disconnected. It remains busy and refuses the subsequent close.
        runner.sends.fetch_add(1, Ordering::SeqCst);
        return Response::new(Body::from_stream(futures_util::stream::iter([
            Ok(Bytes::from_static(b"{")),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "synthetic post-accept disconnect",
            )),
        ])));
    }
    if method == Method::POST
        && uri.path().ends_with("/complete")
        && runner.completes.fetch_add(1, Ordering::SeqCst) == 1
        && matches!(runner.failure, RunnerFailure::FinalIdentity)
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"identity unavailable"})),
        )
            .into_response();
    }
    match (method.as_str(), uri.path()) {
        ("GET", "/v1/capabilities") => (
            StatusCode::OK,
            Json(json!({"connectors":[{
                "platform":"zhihu","placement_slot":"primary",
                "connector_version":"test.browser.v1",
                "operations":["publish","lookup"],"verified":false
            }]})),
        ),
        ("POST", "/v1/sessions") => (
            StatusCode::OK,
            Json(json!({
                "session_id":payload.unwrap().0["session_id"]
            })),
        ),
        ("POST", "/v1/executions") => {
            runner.sends.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::OK,
                Json(json!({
                    "execution_id":payload.unwrap().0["execution_id"],
                    "status":"unknown","provenance":"fixture","evidence":[]
                })),
            )
        }
        ("POST", path) if path.ends_with("/complete") => (
            StatusCode::OK,
            Json(json!({
                "identity":{"platform_account_id":"verified","display_name":"Verified"},
                "storage_state":{"cookies":[],"origins":[]}
            })),
        ),
        // Model a runner still busy after its ambiguous execution response.
        // The account reservation must survive this failed cleanup.
        ("DELETE", _) => (StatusCode::CONFLICT, Json(json!({"error":"busy"}))),
        _ => (StatusCode::OK, Json(json!({"closed":true}))),
    }
    .into_response()
}

#[tokio::test]
async fn explicit_operator_disable_defers_legacy_publication_without_an_attempt() {
    let (state, scope, account, sends, server, _) = fixture().await;
    let imported = state
        .knowledge_repository()
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "generic".into(),
                kind: SourceKind::Text,
                name: "Generic public source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("A public text that may be published.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let source = imported.items[0].source.as_ref().unwrap();
    let version = imported.items[0].source_version.as_ref().unwrap();
    let mut record = state
        .channel_service()
        .repository
        .get_account(&scope, account)
        .await
        .unwrap();
    record.account.platform = "zhihu".into();
    state
        .channel_service()
        .repository
        .save_account(&scope, record)
        .await
        .unwrap();
    let target_id = Uuid::new_v4();
    let repo = state.channel_job_repository();
    repo.create_plan(
        &scope,
        ChannelPlan {
            plan_id: Uuid::new_v4(),
            project_id: scope.project_id.unwrap(),
            cycle_id: Uuid::new_v4(),
            input_hash: target_id.to_string(),
            revision: 1,
            created_at: Utc::now(),
            targets: vec![ChannelTarget {
                target_id,
                input: ChannelTargetInput::Publish {
                    source_id: source.source_id,
                    source_version_id: version.source_version_id,
                    platform: "zhihu".into(),
                    account_id: account,
                    title: "Test".into(),
                    body: "Text".into(),
                    body_sha256: geo_domain::sha256_hex(b"Text"),
                },
            }],
        },
    )
    .await
    .unwrap();
    state
        .connector_capability_repository()
        .configure(
            scope.operator_id,
            ConnectorKey {
                platform_id: "zhihu".into(),
                placement_slot: "primary".into(),
            },
            0,
            false,
            vec![],
            "",
        )
        .await
        .unwrap();
    assert!(matches!(
        execute_channel_target(&state, &scope, target_id)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::ConnectorUnavailable)
    ));
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert!(
        repo.get_target(&scope, target_id)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    server.abort();
}

async fn fixture() -> (
    AppState,
    TenantScope,
    Uuid,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
    Arc<MemoryProjectRepository>,
) {
    fixture_with_failure(RunnerFailure::None).await
}

async fn fixture_with_failure(
    failure: RunnerFailure,
) -> (
    AppState,
    TenantScope,
    Uuid,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
    Arc<MemoryProjectRepository>,
) {
    let sends = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let runner = Runner {
        sends: sends.clone(),
        completes: Arc::new(AtomicUsize::new(0)),
        failure,
    };
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(mock_runner)).with_state(runner),
        )
        .await
        .unwrap()
    });
    let bridge = BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap();
    let channels = Arc::new(MemoryChannelRepository::default());
    let key = "a5".repeat(32);
    let service = ChannelService::persistent(channels, &key, Some(bridge)).unwrap();
    let projects = Arc::new(MemoryProjectRepository::default());
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password(
            "dispatcher-fixture",
        )),
        projects.clone(),
        EventBus::default(),
        false,
    )
    .with_channel_service(service);
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Dispatcher fixture".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project.id),
    );
    let account_id = Uuid::new_v4();
    let aad = format!(
        "geo-channel-v1:{}:{}:{}:{}:session",
        scope.operator_id, scope.tenant_id, project.id, account_id
    );
    let encrypted = SecretEnvelope::from_hex_key(&key)
        .unwrap()
        .seal(aad.as_bytes(), br#"{"cookies":[],"origins":[]}"#)
        .unwrap();
    state
        .channel_service()
        .repository
        .save_account(
            &scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id,
                    project_id: project.id,
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "kimi".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: None,
                    platform_account_id: Some("verified".into()),
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
    (state, scope, account_id, sends, server, projects)
}

async fn plan_measure(
    repository: Arc<dyn ChannelJobRepository>,
    scope: &TenantScope,
    account_id: Uuid,
    scheduled_at: chrono::DateTime<Utc>,
) -> Uuid {
    let target_id = Uuid::new_v4();
    repository
        .create_plan(
            scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id: Uuid::new_v4(),
                input_hash: target_id.to_string(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![ChannelTarget {
                    target_id,
                    input: ChannelTargetInput::Measure {
                        account_id,
                        provider: "kimi".into(),
                        model: "fixture-model".into(),
                        surface: "consumer_web".into(),
                        search_mode: "web_search".into(),
                        protocol_version: "v1".into(),
                        question_set_version: "v1".into(),
                        question: "Fixture question".into(),
                        market: "CN".into(),
                        language: "en".into(),
                        scheduled_at,
                        sample_ordinal: 0,
                        question_binding: None,
                    },
                }],
            },
        )
        .await
        .unwrap();
    target_id
}

#[tokio::test]
async fn duplicated_dispatch_sends_once_and_unknown_is_never_retried() {
    let (state, scope, account, sends, server, _) = fixture().await;
    let repo = state.channel_job_repository();
    let target = plan_measure(
        repo.clone(),
        &scope,
        account,
        Utc::now() - Duration::seconds(1),
    )
    .await;
    let (a, b) = tokio::join!(
        execute_channel_target(&state, &scope, target),
        execute_channel_target(&state, &scope, target)
    );
    assert_eq!(
        usize::from(matches!(a, Ok(ChannelDispatchResult::Executed(_))))
            + usize::from(matches!(b, Ok(ChannelDispatchResult::Executed(_)))),
        1
    );
    let view = repo.get_target(&scope, target).await.unwrap();
    assert_eq!(view.attempts.len(), 1);
    assert!(view.attempts[0].outcome.as_ref().unwrap().fixture);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    // Unknown may mean the remote request outlived our response; retain the
    // account reservation for its bounded deadline, without retrying target.
    let now = Utc::now();
    assert!(
        repo.reserve_account(
            &scope,
            account,
            Uuid::new_v4(),
            now,
            now + Duration::minutes(5)
        )
        .await
        .is_err()
    );
    assert!(
        execute_channel_target(&state, &scope, target)
            .await
            .is_err()
    );
    assert!(
        repo.scan_pending(None, Utc::now(), 100)
            .await
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn post_accept_disconnect_and_failed_close_preserve_unknown_and_account_lease() {
    let (state, scope, account, sends, server, _) =
        fixture_with_failure(RunnerFailure::LostReceipt).await;
    let repo = state.channel_job_repository();
    let first = plan_measure(repo.clone(), &scope, account, Utc::now()).await;
    let second = plan_measure(repo.clone(), &scope, account, Utc::now()).await;
    let ChannelDispatchResult::Executed(view) =
        execute_channel_target(&state, &scope, first).await.unwrap()
    else {
        panic!("accepted measurement must retain its attempt")
    };
    assert_eq!(view.attempts.len(), 1);
    assert_eq!(
        view.attempts[0].outcome.as_ref().unwrap().status,
        geo_domain::ChannelOutcomeStatus::Unknown
    );
    assert!(execute_channel_target(&state, &scope, first).await.is_err());
    assert!(matches!(
        execute_channel_target(&state, &scope, second)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::AccountBusy)
    ));
    assert!(
        repo.get_target(&scope, second)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn final_identity_preflight_failure_is_not_an_unknown_send() {
    let (state, scope, account, sends, server, _) =
        fixture_with_failure(RunnerFailure::FinalIdentity).await;
    let repo = state.channel_job_repository();
    let target = plan_measure(repo.clone(), &scope, account, Utc::now()).await;
    let ChannelDispatchResult::Executed(view) = execute_channel_target(&state, &scope, target)
        .await
        .unwrap()
    else {
        panic!("claimed preflight failure must retain its attempt")
    };
    assert_eq!(
        view.attempts[0].outcome.as_ref().unwrap().status,
        geo_domain::ChannelOutcomeStatus::LoginRequired
    );
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    let now = Utc::now();
    repo.reserve_account(
        &scope,
        account,
        Uuid::new_v4(),
        now,
        now + Duration::minutes(5),
    )
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn account_and_schedule_deferrals_leave_target_unattempted() {
    let (state, scope, account, sends, server, _) = fixture().await;
    let repo = state.channel_job_repository();
    let future = plan_measure(
        repo.clone(),
        &scope,
        account,
        Utc::now() + Duration::days(1),
    )
    .await;
    assert!(matches!(
        execute_channel_target(&state, &scope, future)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::ScheduledForLater)
    ));
    assert!(
        repo.get_target(&scope, future)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    let due = plan_measure(
        repo.clone(),
        &scope,
        account,
        Utc::now() - Duration::seconds(1),
    )
    .await;
    let mut record = state
        .channel_service()
        .repository
        .get_account(&scope, account)
        .await
        .unwrap();
    record.account.status = ChannelStatus::NeedsLogin;
    state
        .channel_service()
        .repository
        .save_account(&scope, record)
        .await
        .unwrap();
    assert!(matches!(
        execute_channel_target(&state, &scope, due).await.unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::AccountUnavailable)
    ));
    assert!(
        repo.get_target(&scope, due)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn paused_project_is_not_dispatched() {
    let (state, scope, account, sends, server, projects) = fixture().await;
    let target = plan_measure(
        state.channel_job_repository(),
        &scope,
        account,
        Utc::now() - Duration::seconds(1),
    )
    .await;
    let mut project = projects
        .get(&scope, scope.project_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    project.status = ProjectStatus::Paused;
    projects.insert(project).await.unwrap();
    assert!(matches!(
        execute_channel_target(&state, &scope, target)
            .await
            .unwrap(),
        ChannelDispatchResult::Deferred(ChannelDispatchDeferred::ProjectInactive)
    ));
    assert!(
        state
            .channel_job_repository()
            .get_target(&scope, target)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    server.abort();
}
