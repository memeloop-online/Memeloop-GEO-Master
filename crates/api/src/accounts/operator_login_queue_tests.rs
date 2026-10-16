use super::*;
use axum::{
    Json,
    extract::State,
    routing::{get, post},
};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, Membership, MemoryAuthRepository, Role,
    TenantScope, UserId,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone, Default)]
struct Runner {
    starts: Arc<AtomicUsize>,
    captures: Arc<AtomicUsize>,
    unavailable: Arc<AtomicBool>,
    generation: Arc<tokio::sync::Mutex<Uuid>>,
}
async fn fixture() -> (
    AppState,
    AuthContext,
    OperatorLoginQueue,
    Runner,
    tokio::task::JoinHandle<()>,
) {
    let runner = Runner::default();
    *runner.generation.lock().await = Uuid::from_u128(555);
    let app = Router::new()
        .route("/v1/lifecycle", get(|State(r): State<Runner>| async move { Json(json!({"generation":*r.generation.lock().await})) }))
        .route("/v1/sessions", post(|State(r): State<Runner>, Json(input): Json<Value>| async move {
            r.starts.fetch_add(1, Ordering::SeqCst);
            Json(json!({"session_id":input["session_id"],"generation":input["expected_generation"]}))
        }))
        .route("/v1/sessions/{id}/status", get(|State(r): State<Runner>| async move {
            if r.unavailable.load(Ordering::SeqCst) { (axum::http::StatusCode::NOT_FOUND, Json(json!({}))) }
            else { (axum::http::StatusCode::OK, Json(json!({"phase":"awaiting_login","identity":null}))) }
        }))
        .route("/v1/sessions/{id}/complete", post(|State(r): State<Runner>| async move {
            r.captures.fetch_add(1, Ordering::SeqCst);
            Json(json!({"identity":{"platform_account_id":"fixture-identity","display_name":"Fixture","avatar_url":null},"storage_state":{}}))
        }))
        .route("/v1/sessions/{id}/dispose", post(|Path(id): Path<Uuid>, Json(input): Json<Value>| async move {
            Json(json!({"session_id":id,"generation":input["expected_generation"],"state":"disposed"}))
        })).with_state(runner.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let auth_repository = Arc::new(MemoryAuthRepository::development_with_password(
        "fixture-password",
    ));
    let pool = Uuid::new_v4().into();
    auth_repository
        .insert_membership(Membership::new(
            UserId::new(Uuid::from_u128(0x00000000000040008000000000000004)),
            DEVELOPMENT_OPERATOR_ID,
            pool,
            Role::ResourceAdmin,
        ))
        .await
        .unwrap();
    let state = AppState::with_stores_and_auth(
        Arc::new(crate::MemoryOperationStore::default()),
        Arc::new(crate::MemoryIdempotencyStore::default()),
        auth_repository,
        crate::EventBus::default(),
        false,
    )
    .with_channel_service(
        channels::ChannelService::development()
            .with_operator_pool_tenant_id(pool)
            .with_browser(
                crate::BrowserBridge::new(format!("http://{address}"), "fixture-token".into())
                    .unwrap(),
            ),
    );
    let identity = state
        .auth_repository()
        .authenticate(
            DEVELOPMENT_OPERATOR_ID,
            "demo@localhost",
            "fixture-password",
        )
        .await
        .unwrap()
        .unwrap();
    let session = state
        .auth_repository()
        .create_session(
            DEVELOPMENT_OPERATOR_ID,
            identity.user.id,
            chrono::Duration::hours(1),
        )
        .await
        .unwrap()
        .session;
    let auth = AuthContext {
        operator: identity.operator,
        user: identity.user,
        memberships: identity.memberships,
        session,
        scope: TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None),
    };
    let context = RequestContext::new(Uuid::new_v4(), Uuid::new_v4());
    let input = serde_json::from_value(json!({"import_key":"fixture","rows":[
        {"row_key":"first","platform":"zhihu","username":"fixture-user","password":"fixture-secret"},
        {"row_key":"second","platform":"zhihu","username":"fixture-user-two","password":"fixture-secret-two"}
    ]})).unwrap();
    let imported = crate::channel_imports::create(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(context),
        Ok(Json(input)),
    )
    .await
    .unwrap();
    let imported = serde_json::to_value(imported.0).unwrap();
    let selections = (0..2)
        .map(|i| OperatorLoginQueueSelection {
            row_key: if i == 0 { "first" } else { "second" }.into(),
            account_id: serde_json::from_value(imported["rows"][i]["account_id"].clone()).unwrap(),
        })
        .collect::<Vec<_>>();
    let queue = state
        .channel_service()
        .operations()
        .unwrap()
        .create_login_queue(
            &queue_scope(&state, &auth).unwrap(),
            Uuid::new_v4(),
            "fixture",
            &selections,
            state.channel_service().repository.as_ref(),
            Utc::now(),
        )
        .await
        .unwrap();
    (state, auth, queue, runner, server)
}

#[tokio::test]
async fn prepared_restart_and_concurrent_drive_dispatch_one_pinned_start() {
    let (state, auth, queue, runner, server) = fixture().await;
    let input = state
        .channel_service()
        .prepare_pool_login_input(&auth, queue.items[0].account_id, Uuid::new_v4())
        .await
        .unwrap();
    let (claimed, operation) = state
        .channel_service()
        .operations()
        .unwrap()
        .begin_login_queue_item(
            &queue.scope,
            queue.queue_id,
            queue.version,
            auth.session.id.as_uuid(),
            &input,
            state.channel_service().repository.as_ref(),
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(runner.starts.load(Ordering::SeqCst), 0);
    let restarted = state.clone();
    let (first, second) = tokio::join!(
        drive_queue(&restarted, &auth, claimed.clone()),
        drive_queue(&restarted, &auth, claimed.clone()),
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(runner.starts.load(Ordering::SeqCst), 1);
    let latest = read_queue(&state, &auth, queue.queue_id).await.unwrap();
    let persisted = current_operation(&state, &latest).await.unwrap().unwrap();
    assert_eq!(persisted.operation_id, operation.operation_id);
    assert_eq!(persisted.runner_generation, operation.runner_generation);
    assert_eq!(persisted.expires_at, operation.expires_at);
    assert_eq!(persisted.network_binding_id, operation.network_binding_id);
    assert_eq!(persisted.state, BrowserOperationState::StartRequested);
    server.abort();
}

#[tokio::test]
async fn start_requested_unknown_never_resends_and_late_error_cannot_pause_successor() {
    let (state, auth, queue, runner, server) = fixture().await;
    let current = drive_queue(&state, &auth, queue).await.unwrap();
    let operation = current_operation(&state, &current).await.unwrap().unwrap();
    runner.unavailable.store(true, Ordering::SeqCst);
    assert!(drive_queue(&state, &auth, current.clone()).await.is_err());
    assert_eq!(runner.starts.load(Ordering::SeqCst), 1);
    let paused = read_queue(&state, &auth, current.queue_id).await.unwrap();
    assert_eq!(paused.status, OperatorLoginQueueStatus::Paused);
    assert_eq!(
        paused.current().unwrap().operation_id,
        Some(operation.operation_id)
    );
    runner.unavailable.store(false, Ordering::SeqCst);
    let recovered = command(
        &state,
        &auth,
        current.queue_id,
        paused.version,
        Command::Complete,
    )
    .await
    .unwrap();
    assert_eq!(recovered.status, OperatorLoginQueueStatus::Paused);
    let resumed = command(
        &state,
        &auth,
        current.queue_id,
        recovered.version,
        Command::Resume,
    )
    .await
    .unwrap();
    assert_eq!(runner.starts.load(Ordering::SeqCst), 2);
    pause_error(
        &state,
        &current,
        Some(operation.operation_id),
        "login_failed",
    )
    .await;
    let latest = read_queue(&state, &auth, current.queue_id).await.unwrap();
    assert_eq!(latest.status, OperatorLoginQueueStatus::Running);
    assert_eq!(latest.version, resumed.version);
    assert_ne!(
        latest.current().unwrap().operation_id,
        Some(operation.operation_id)
    );
    server.abort();
}

#[tokio::test]
async fn concurrent_completion_saved_marker_recovers_without_pausing_success() {
    let (state, auth, queue, runner, server) = fixture().await;
    let current = drive_queue(&state, &auth, queue).await.unwrap();
    let (first, second) = tokio::join!(
        command(
            &state,
            &auth,
            current.queue_id,
            current.version,
            Command::Complete
        ),
        command(
            &state,
            &auth,
            current.queue_id,
            current.version,
            Command::Complete
        ),
    );
    assert!(first.is_ok() || second.is_ok());
    let latest = read_queue(&state, &auth, current.queue_id).await.unwrap();
    assert_eq!(
        latest.items[0].status,
        OperatorLoginQueueItemStatus::Succeeded
    );
    assert_eq!(latest.status, OperatorLoginQueueStatus::Running);
    assert_eq!(runner.starts.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn queue_reads_require_same_creator_operator_and_resource_role() {
    let (state, auth, queue, _, server) = fixture().await;
    let mut other_user = auth.clone();
    other_user.user.id = UserId::new(Uuid::new_v4());
    assert_eq!(
        read_queue(&state, &other_user, queue.queue_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let mut other_operator = auth.clone();
    other_operator.operator.id = Uuid::new_v4().into();
    other_operator.memberships[0].operator_id = other_operator.operator.id;
    assert!(
        read_queue(&state, &other_operator, queue.queue_id)
            .await
            .is_err()
    );
    let mut denied = auth.clone();
    denied.memberships.clear();
    assert_eq!(
        read_queue(&state, &denied, queue.queue_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    server.abort();
}

#[tokio::test]
async fn runner_generation_change_pauses_without_replacing_old_browser() {
    let (state, auth, queue, runner, server) = fixture().await;
    let current = drive_queue(&state, &auth, queue).await.unwrap();
    let operation = current_operation(&state, &current).await.unwrap().unwrap();
    *runner.generation.lock().await = Uuid::from_u128(556);
    assert!(drive_queue(&state, &auth, current.clone()).await.is_err());
    let latest = read_queue(&state, &auth, current.queue_id).await.unwrap();
    assert_eq!(latest.status, OperatorLoginQueueStatus::Paused);
    assert_eq!(
        latest.current().unwrap().operation_id,
        Some(operation.operation_id)
    );
    let view = project(&state, &auth, latest).await.unwrap();
    assert_eq!(view.current.as_ref().unwrap().phase, "recovery_required");
    assert!(view.current.unwrap().session_id.is_none());
    assert_eq!(runner.starts.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn unverified_closing_complete_does_not_capture_or_advance() {
    let (state, auth, queue, runner, server) = fixture().await;
    let current = drive_queue(&state, &auth, queue).await.unwrap();
    let operation = current_operation(&state, &current).await.unwrap().unwrap();
    state
        .channel_service()
        .operations()
        .unwrap()
        .request_browser_close(&operation.scope, operation.operation_id, Utc::now())
        .await
        .unwrap();
    let error = command(
        &state,
        &auth,
        current.queue_id,
        current.version,
        Command::Complete,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(runner.captures.load(Ordering::SeqCst), 0);
    assert_eq!(runner.starts.load(Ordering::SeqCst), 1);
    let latest = read_queue(&state, &auth, current.queue_id).await.unwrap();
    assert_eq!(
        latest.current().unwrap().operation_id,
        Some(operation.operation_id)
    );
    assert_eq!(
        latest.items[0].status,
        OperatorLoginQueueItemStatus::Current
    );
    server.abort();
}
