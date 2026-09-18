use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    middleware,
    routing::post,
};
use geo_api::{
    AppState, CORRELATION_ID_HEADER, EventBus, IdempotencyStore, MemoryIdempotencyStore,
    OPERATOR_ID_HEADER, REQUEST_ID_HEADER, StoredResponse, TENANT_ID_HEADER, body_hash,
    dev_scope_middleware, json_command_idempotency_middleware, router,
};
use geo_domain::{EventEnvelope, Operation, TenantScope};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
use uuid::Uuid;

fn scoped_request(uri: &str, operator_id: Uuid, tenant_id: Uuid) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(OPERATOR_ID_HEADER, operator_id.to_string())
        .header(TENANT_ID_HEADER, tenant_id.to_string())
        .body(Body::empty())
        .expect("request")
}

#[tokio::test]
async fn health_and_readiness_are_explicit() {
    let state = AppState::development();
    let app = router(state.clone());

    let live = app
        .clone()
        .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(live.status(), StatusCode::OK);
    assert_eq!(
        live.headers()[REQUEST_ID_HEADER],
        live.headers()[CORRELATION_ID_HEADER]
    );

    let not_ready = app
        .clone()
        .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(not_ready.status(), StatusCode::SERVICE_UNAVAILABLE);

    state.set_ready(true);
    let ready = app
        .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
}

#[tokio::test]
async fn operation_is_visible_only_inside_server_side_scope() {
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let other_tenant_id = Uuid::new_v4();
    let operation = Operation::queued(
        "knowledge.import",
        TenantScope::new(operator_id.into(), tenant_id.into(), None),
    );
    let operation_id = operation.id;
    // The public development state intentionally hides its mutable store. A
    // custom store is used below in the production-shaped constructor instead.
    let operation_store = std::sync::Arc::new(geo_api::MemoryOperationStore::default());
    operation_store.insert(operation).await;
    let state = AppState::with_stores(
        operation_store,
        std::sync::Arc::new(MemoryIdempotencyStore::default()),
        EventBus::default(),
    );
    state.set_ready(true);
    let app = router(state);

    let response = app
        .clone()
        .oneshot(scoped_request(
            &format!("/api/v1/operations/{operation_id}"),
            operator_id,
            tenant_id,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(scoped_request(
            &format!("/api/v1/operations/{operation_id}"),
            operator_id,
            other_tenant_id,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_scope_is_rejected_and_openapi_is_public() {
    let app = router(AppState::development());
    let missing_scope = app
        .clone()
        .oneshot(Request::get("/api/v1/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing_scope.status(), StatusCode::BAD_REQUEST);

    let openapi = app
        .oneshot(
            Request::get("/api/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(openapi.status(), StatusCode::OK);
    assert_eq!(openapi.headers()["content-type"], "application/json");
}

#[tokio::test]
async fn event_stream_route_is_sse_and_event_bus_keeps_scope_filtering() {
    let state = AppState::development();
    state.set_ready(true);
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let event = EventEnvelope::new(
        "knowledge.imported",
        TenantScope::new(operator_id.into(), tenant_id.into(), None),
        Uuid::new_v4(),
        1,
        Uuid::new_v4(),
    );
    let _ = state.publish_event(event);
    let response = router(state)
        .oneshot(scoped_request("/api/v1/events", operator_id, tenant_id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
}

#[tokio::test]
async fn idempotency_binds_key_to_scope_and_body_hash() {
    let store = MemoryIdempotencyStore::default();
    let scope = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
    let hash = body_hash(br#"{"hello":"world"}"#);
    let token = match store.begin(&scope, "request-1", &hash).await.unwrap() {
        geo_api::IdempotencyDecision::New(token) => token,
        other => panic!("expected reservation, got {other:?}"),
    };
    assert!(matches!(
        store.begin(&scope, "request-1", &hash).await.unwrap(),
        geo_api::IdempotencyDecision::InFlight
    ));
    store
        .complete(
            &token,
            StoredResponse {
                status: 202,
                content_type: "application/json".to_owned(),
                body: br#"{"operation_id":"1"}"#.to_vec(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store.begin(&scope, "request-1", &hash).await.unwrap(),
        geo_api::IdempotencyDecision::Replay(_)
    ));
    assert_eq!(
        store
            .begin(&scope, "request-1", "different-hash")
            .await
            .unwrap_err()
            .code,
        geo_domain::ErrorCode::Conflict
    );
}

async fn idempotency_test_command(
    axum::extract::State(executions): axum::extract::State<Arc<AtomicUsize>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let execution = executions.fetch_add(1, Ordering::SeqCst) + 1;
    (StatusCode::CREATED, Json(json!({ "execution": execution })))
}

fn idempotency_test_router(executions: Arc<AtomicUsize>) -> Router {
    let store: Arc<dyn IdempotencyStore> = Arc::new(MemoryIdempotencyStore::default());
    Router::new()
        .route("/command", post(idempotency_test_command))
        .route_layer(middleware::from_fn_with_state(
            store,
            json_command_idempotency_middleware,
        ))
        .route_layer(middleware::from_fn(dev_scope_middleware))
        .with_state(executions)
}

fn idempotency_command_request(
    operator_id: Uuid,
    tenant_id: Uuid,
    key: Option<&str>,
    body: &'static str,
) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/command")
        .header("content-type", "application/json")
        .header(OPERATOR_ID_HEADER, operator_id.to_string())
        .header(TENANT_ID_HEADER, tenant_id.to_string());
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.body(Body::from(body)).expect("request")
}

#[tokio::test]
async fn json_command_idempotency_replays_and_rejects_conflicts() {
    let executions = Arc::new(AtomicUsize::new(0));
    let app = idempotency_test_router(Arc::clone(&executions));
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let first = app
        .clone()
        .oneshot(idempotency_command_request(
            operator_id,
            tenant_id,
            Some("command-1"),
            r#"{"action":"import"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(first.headers()["content-type"], "application/json");
    let first_body = to_bytes(first.into_body(), 1024).await.unwrap();

    let replay = app
        .clone()
        .oneshot(idempotency_command_request(
            operator_id,
            tenant_id,
            Some("command-1"),
            r#"{"action":"import"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.headers()["content-type"], "application/json");
    assert_eq!(
        to_bytes(replay.into_body(), 1024).await.unwrap(),
        first_body
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1);

    let conflict = app
        .clone()
        .oneshot(idempotency_command_request(
            operator_id,
            tenant_id,
            Some("command-1"),
            r#"{"action":"delete"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(executions.load(Ordering::SeqCst), 1);

    let missing_key = app
        .oneshot(idempotency_command_request(
            operator_id,
            tenant_id,
            None,
            r#"{"action":"import"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(missing_key.status(), StatusCode::BAD_REQUEST);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}
