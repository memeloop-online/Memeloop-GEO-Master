use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
    middleware,
    routing::post,
};
use geo_api::{
    AppState, CORRELATION_ID_HEADER, EventBus, IdempotencyStore, MemoryIdempotencyStore,
    OPERATOR_ID_HEADER, REQUEST_ID_HEADER, StoredResponse, TENANT_ID_HEADER, body_hash,
    dev_scope_middleware, json_command_idempotency_middleware, router,
};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, DEVELOPMENT_USER_EMAIL, EventEnvelope,
    Membership, MemoryAuthRepository, Operation, Role, TenantScope, User,
};
use serde_json::Value;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
use uuid::Uuid;

async fn login(app: &Router) -> (String, String, Value) {
    login_as(app, DEVELOPMENT_USER_EMAIL, "test-password").await
}

async fn login_as(app: &Router, login_name: &str, password: &str) -> (String, String, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "login_name": login_name,
                        "password": password
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let csrf = body["csrf_token"].as_str().unwrap().to_owned();
    (cookie, csrf, body)
}

fn authenticated_json_request(
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    idempotency_key: Option<&str>,
    body: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie)
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        builder = builder.header(geo_api::CSRF_HEADER, csrf);
    }
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("idempotency-key", idempotency_key);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

#[tokio::test]
async fn project_lifecycle_is_session_scoped_and_idempotent() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf, _) = login(&app).await;
    let tenant_id = DEVELOPMENT_TENANT_ID.to_string();
    let project_input = serde_json::json!({
        "slug": "lifecycle-project",
        "display_name": "Lifecycle project",
        "settings": {
            "brand_name": "Acme",
            "product_name": "Widget",
            "market": "CN",
            "language": "zh-CN",
            "competitors": [],
            "resource_mode": "mixed",
            "monthly_budget_minor": 100000,
            "budget_currency": "CNY",
            "monitoring_reserve_percent": 20,
            "initial_sources": [{
                "kind": "url",
                "value": "https://example.com",
                "visibility": "public"
            }]
        }
    })
    .to_string();

    let create = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("project-create-1"),
            &project_input,
        ))
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let create_body = to_bytes(create.into_body(), 64 * 1024).await.unwrap();
    let project: Value = serde_json::from_slice(&create_body).unwrap();
    let project_id = project["id"].as_str().unwrap().to_owned();
    assert_eq!(project["status"], "draft");
    assert_eq!(project["revision"], 1);

    let replay = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("project-create-1"),
            &project_input,
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(replay.into_body(), 64 * 1024).await.unwrap(),
        create_body
    );

    let list = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects?tenant_id={tenant_id}&limit=1"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body: Value =
        serde_json::from_slice(&to_bytes(list.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(list_body["items"].as_array().unwrap().len(), 1);

    let detail = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{project_id}?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);

    let overview = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{project_id}/overview?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(overview.status(), StatusCode::OK);
    let overview_body: Value =
        serde_json::from_slice(&to_bytes(overview.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(overview_body["knowledge"]["source_count"], 0);
    assert_eq!(overview_body["benchmark"]["effective_samples"], Value::Null);

    let estimate = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/estimate?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            None,
            &project_input,
        ))
        .await
        .unwrap();
    assert_eq!(estimate.status(), StatusCode::OK);
    let estimate_body: Value =
        serde_json::from_slice(&to_bytes(estimate.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(estimate_body["coverage"]["documents"]["state"], "unknown");
    assert_eq!(estimate_body["costs"]["total"]["state"], "unknown");
    assert!(estimate_body["blockers"].as_array().unwrap().len() >= 4);
    assert!(!estimate_body["assumptions"].as_array().unwrap().is_empty());
    assert_eq!(estimate_body["budget"]["measurement_reserve_minor"], 20000);

    let start = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{project_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("project-start-1"),
            r#"{"expected_revision":1}"#,
        ))
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::ACCEPTED);
    let start_body = to_bytes(start.into_body(), 64 * 1024).await.unwrap();
    let acceptance: Value = serde_json::from_slice(&start_body).unwrap();
    assert_eq!(acceptance["status"], "accepted");
    assert_eq!(
        acceptance["document_manifest"]["state"],
        "awaiting_knowledge"
    );
    assert_eq!(
        acceptance["distribution_manifest"]["state"],
        "awaiting_documents"
    );

    let start_replay = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{project_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("project-start-1"),
            r#"{"expected_revision":1}"#,
        ))
        .await
        .unwrap();
    assert_eq!(start_replay.status(), StatusCode::ACCEPTED);
    assert_eq!(
        to_bytes(start_replay.into_body(), 64 * 1024).await.unwrap(),
        start_body
    );

    let started_overview = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{project_id}/overview?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(started_overview.status(), StatusCode::OK);
    let started_overview: Value = serde_json::from_slice(
        &to_bytes(started_overview.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(started_overview["cycle"]["status"], "not_started");
    assert_eq!(started_overview["cycle"]["awaiting_knowledge"], true);
    assert_eq!(started_overview["knowledge"]["status"], "empty");
    assert_eq!(started_overview["benchmark"]["status"], "not_started");

    let stale_patch = app
        .clone()
        .oneshot({
            let mut request = authenticated_json_request(
                "PATCH",
                &format!("/api/v1/projects/{project_id}?tenant_id={tenant_id}"),
                &cookie,
                Some(&csrf),
                Some("project-patch-stale"),
                r#"{"revision":1,"display_name":"stale"}"#,
            );
            request
                .headers_mut()
                .insert("if-match", "1".parse().unwrap());
            request
        })
        .await
        .unwrap();
    assert_eq!(stale_patch.status(), StatusCode::CONFLICT);

    let forged_tenant = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{project_id}?tenant_id={}", Uuid::new_v4()),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(forged_tenant.status(), StatusCode::FORBIDDEN);

    let no_csrf = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/estimate?tenant_id={tenant_id}"),
            &cookie,
            None,
            None,
            &project_input,
        ))
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);

    let wrong_origin = app
        .oneshot({
            let mut request = authenticated_json_request(
                "POST",
                &format!("/api/v1/projects/estimate?tenant_id={tenant_id}"),
                &cookie,
                Some(&csrf),
                None,
                &project_input,
            );
            request
                .headers_mut()
                .insert("origin", "https://attacker.example".parse().unwrap());
            request
        })
        .await
        .unwrap();
    assert_eq!(wrong_origin.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn viewer_can_read_projects_but_cannot_estimate_patch_or_start() {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "test-password",
    ));
    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "viewer@localhost",
        "Viewer",
        "viewer-password",
    )
    .unwrap();
    auth.insert_user(viewer.clone()).await.unwrap();
    let mut membership = Membership::new(
        viewer.id,
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Role::CustomerReadOnly,
    );
    membership.tenant_slug = "demo".to_owned();
    membership.tenant_display_name = "Local Demo Tenant".to_owned();
    auth.insert_membership(membership).await.unwrap();

    let projects = Arc::new(geo_domain::MemoryProjectRepository::default());
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(geo_api::MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        projects,
        EventBus::default(),
        false,
    );
    let app = router(state);
    let (admin_cookie, admin_csrf, _) = login(&app).await;
    let tenant_id = DEVELOPMENT_TENANT_ID.to_string();
    let input = r#"{
        "display_name":"Viewer test",
        "settings":{
            "brand_name":"Acme",
            "product_name":"Widget",
            "market":"CN",
            "language":"zh-CN",
            "competitors":[],
            "resource_mode":"own",
            "monthly_budget_minor":1000,
            "budget_currency":"CNY",
            "monitoring_reserve_percent":20,
            "initial_sources":[{"kind":"url","value":"https://example.com","visibility":"public"}]
        }
    }"#;
    let created = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &admin_cookie,
            Some(&admin_csrf),
            Some("viewer-fixture-create"),
            input,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value =
        serde_json::from_slice(&to_bytes(created.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let project_id = created["id"].as_str().unwrap().to_owned();

    let (viewer_cookie, viewer_csrf, _) =
        login_as(&app, "viewer@localhost", "viewer-password").await;
    let list = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &viewer_cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);

    let estimate = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/estimate?tenant_id={tenant_id}"),
            &viewer_cookie,
            Some(&viewer_csrf),
            None,
            input,
        ))
        .await
        .unwrap();
    assert_eq!(estimate.status(), StatusCode::FORBIDDEN);

    let patch = app
        .clone()
        .oneshot({
            let mut request = authenticated_json_request(
                "PATCH",
                &format!("/api/v1/projects/{project_id}?tenant_id={tenant_id}"),
                &viewer_cookie,
                Some(&viewer_csrf),
                Some("viewer-fixture-patch"),
                r#"{"revision":1,"display_name":"blocked"}"#,
            );
            request
                .headers_mut()
                .insert("if-match", "1".parse().unwrap());
            request
        })
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::FORBIDDEN);

    let start = app
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{project_id}/start?tenant_id={tenant_id}"),
            &viewer_cookie,
            Some(&viewer_csrf),
            Some("viewer-fixture-start"),
            r#"{"expected_revision":1}"#,
        ))
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn start_replay_and_get_return_the_persisted_acceptance() {
    let operation_store = Arc::new(geo_api::MemoryOperationStore::default());
    let state = AppState::with_stores_and_auth_and_projects(
        operation_store.clone(),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password(
            "test-password",
        )),
        Arc::new(geo_domain::MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    );
    let app = router(state);
    let (cookie, csrf, _) = login(&app).await;
    let tenant_id = DEVELOPMENT_TENANT_ID.to_string();
    let input = r#"{
        "display_name":"Recoverable project",
        "settings":{
            "brand_name":"Acme",
            "product_name":"Widget",
            "market":"CN",
            "language":"zh-CN",
            "competitors":[],
            "resource_mode":"own",
            "monthly_budget_minor":1000,
            "budget_currency":"CNY",
            "monitoring_reserve_percent":20,
            "initial_sources":[{"kind":"url","value":"https://example.com","visibility":"public"}]
        }
    }"#;
    let created = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("recoverable-create"),
            input,
        ))
        .await
        .unwrap();
    let created: Value =
        serde_json::from_slice(&to_bytes(created.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let project_id: geo_domain::ProjectId = created["id"].as_str().unwrap().parse().unwrap();
    let key = "recoverable-start";

    let started = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{project_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some(key),
            r#"{"expected_revision":1}"#,
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::ACCEPTED);
    let started = to_bytes(started.into_body(), 64 * 1024).await.unwrap();

    let replay = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{project_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some(key),
            r#"{"expected_revision":1}"#,
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(
        to_bytes(replay.into_body(), 64 * 1024).await.unwrap(),
        started
    );

    let persisted = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{project_id}/start?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(persisted.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(persisted.into_body(), 64 * 1024).await.unwrap(),
        started
    );

    let detail = app
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{project_id}?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    let detail: Value =
        serde_json::from_slice(&to_bytes(detail.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(detail["status"], "active");
}

#[tokio::test]
async fn concurrent_same_key_start_returns_one_deterministic_operation() {
    let operation_store = Arc::new(geo_api::MemoryOperationStore::default());
    let state = AppState::with_stores_and_auth_and_projects(
        operation_store.clone(),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password(
            "test-password",
        )),
        Arc::new(geo_domain::MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    );
    let app = router(state);
    let (cookie, csrf, _) = login(&app).await;
    let tenant_id = DEVELOPMENT_TENANT_ID.to_string();
    let input = r#"{
        "display_name":"Concurrent project",
        "settings":{
            "brand_name":"Acme",
            "product_name":"Widget",
            "market":"CN",
            "language":"zh-CN",
            "competitors":[],
            "resource_mode":"own",
            "monthly_budget_minor":1000,
            "budget_currency":"CNY",
            "monitoring_reserve_percent":20,
            "initial_sources":[{"kind":"url","value":"https://example.com","visibility":"public"}]
        }
    }"#;
    let created = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("concurrent-create"),
            input,
        ))
        .await
        .unwrap();
    let created: Value =
        serde_json::from_slice(&to_bytes(created.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let project_id = created["id"].as_str().unwrap().to_owned();
    let uri = format!("/api/v1/projects/{project_id}/start?tenant_id={tenant_id}");
    let request_a = authenticated_json_request(
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some("concurrent-start"),
        r#"{"expected_revision":1}"#,
    );
    let request_b = authenticated_json_request(
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some("concurrent-start"),
        r#"{"expected_revision":1}"#,
    );
    let (response_a, response_b) =
        tokio::join!(app.clone().oneshot(request_a), app.oneshot(request_b));
    let response_a = response_a.unwrap();
    let response_b = response_b.unwrap();
    assert_eq!(response_a.status(), StatusCode::ACCEPTED);
    assert_eq!(response_b.status(), StatusCode::ACCEPTED);
    let body_a: Value =
        serde_json::from_slice(&to_bytes(response_a.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    let body_b: Value =
        serde_json::from_slice(&to_bytes(response_b.into_body(), 64 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body_a["operation_id"], body_b["operation_id"]);
    assert_eq!(operation_store.len().await, 1);
}

fn authenticated_request(
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie);
    if let Some(csrf) = csrf {
        builder = builder.header(geo_api::CSRF_HEADER, csrf);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn atomic_start_enforces_draft_start_and_business_idempotency_contracts() {
    let state = AppState::development_with_password("test-password");
    let mut events = state.events().subscribe();
    let app = router(state);
    let (cookie, csrf, _) = login(&app).await;
    let tenant_id = DEVELOPMENT_TENANT_ID.to_string();

    let empty = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("empty-draft"),
            r#"{"display_name":"Empty draft","settings":{}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::CREATED);
    let empty: Value =
        serde_json::from_slice(&to_bytes(empty.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let empty_id = empty["id"].as_str().unwrap();
    let never_started = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{empty_id}/start?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(never_started.status(), StatusCode::NOT_FOUND);
    let invalid_start = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{empty_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("empty-start"),
            r#"{"expected_revision":1}"#,
        ))
        .await
        .unwrap();
    assert_eq!(invalid_start.status(), StatusCode::BAD_REQUEST);

    let valid = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("url-only-draft"),
            r#"{
                "display_name":"URL-only source",
                "settings":{
                    "brand_name":"Acme",
                    "product_name":"Widget",
                    "market":"CN",
                    "language":"zh-CN",
                    "target_audience":"Administrators",
                    "objective":"Improve answer coverage",
                    "initial_sources":[{"kind":"url","value":"https://example.com","visibility":"public"}]
                }
            }"#,
        ))
        .await
        .unwrap();
    assert_eq!(valid.status(), StatusCode::CREATED);
    let valid: Value =
        serde_json::from_slice(&to_bytes(valid.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let valid_id = valid["id"].as_str().unwrap();
    let cleared = app
        .clone()
        .oneshot({
            let mut request = authenticated_json_request(
                "PATCH",
                &format!("/api/v1/projects/{valid_id}?tenant_id={tenant_id}"),
                &cookie,
                Some(&csrf),
                Some("clear-optionals"),
                r#"{
                    "revision":1,
                    "settings":{
                        "product_name":null,
                        "target_audience":null,
                        "objective":null
                    }
                }"#,
            );
            request
                .headers_mut()
                .insert("if-match", "1".parse().unwrap());
            request
        })
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared: Value =
        serde_json::from_slice(&to_bytes(cleared.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(cleared["settings"]["product_name"], Value::Null);
    assert_eq!(cleared["settings"]["target_audience"], Value::Null);
    assert_eq!(cleared["settings"]["objective"], Value::Null);

    let stale = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{valid_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("stale-start"),
            r#"{"expected_revision":99}"#,
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let started = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{valid_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("business-key"),
            r#"{"expected_revision":2}"#,
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::ACCEPTED);
    let started = to_bytes(started.into_body(), 64 * 1024).await.unwrap();
    let event = events.recv().await.expect("cycle.created event");
    assert_eq!(event.event_type, "cycle.created");
    assert_eq!(
        event.project_id.map(|value| value.to_string()),
        Some(valid_id.to_owned())
    );
    assert_eq!(event.aggregate_version, 1);

    let same_key_other_body = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{valid_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("business-key"),
            r#"{"expected_revision":3}"#,
        ))
        .await
        .unwrap();
    assert_eq!(same_key_other_body.status(), StatusCode::CONFLICT);
    let other_key = app
        .clone()
        .oneshot(authenticated_json_request(
            "POST",
            &format!("/api/v1/projects/{valid_id}/start?tenant_id={tenant_id}"),
            &cookie,
            Some(&csrf),
            Some("different-business-key"),
            r#"{"expected_revision":2}"#,
        ))
        .await
        .unwrap();
    assert_eq!(other_key.status(), StatusCode::CONFLICT);

    let persisted = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/projects/{valid_id}/start?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(persisted.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(persisted.into_body(), 64 * 1024).await.unwrap(),
        started
    );

    let status_patch = app
        .oneshot({
            let mut request = authenticated_json_request(
                "PATCH",
                &format!("/api/v1/projects/{valid_id}?tenant_id={tenant_id}"),
                &cookie,
                Some(&csrf),
                Some("forged-status"),
                r#"{"revision":3,"status":"active"}"#,
            );
            request
                .headers_mut()
                .insert("if-match", "3".parse().unwrap());
            request
        })
        .await
        .unwrap();
    assert_eq!(status_patch.status(), StatusCode::UNPROCESSABLE_ENTITY);
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
    let operator_id = DEVELOPMENT_OPERATOR_ID.as_uuid();
    let tenant_id = DEVELOPMENT_TENANT_ID.as_uuid();
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
    let state = AppState::with_stores_and_auth(
        operation_store,
        std::sync::Arc::new(MemoryIdempotencyStore::default()),
        std::sync::Arc::new(MemoryAuthRepository::development_with_password(
            "test-password",
        )),
        EventBus::default(),
        false,
    );
    state.set_ready(true);
    let app = router(state);
    let (cookie, _, _) = login(&app).await;

    let response = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/operations/{operation_id}?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/operations/{operation_id}?tenant_id={other_tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn missing_scope_is_rejected_and_openapi_is_public() {
    let app = router(AppState::development());
    let missing_scope = app
        .clone()
        .oneshot(
            Request::get("/api/v1/events")
                .header("host", "localhost:8080")
                .header(OPERATOR_ID_HEADER, Uuid::new_v4().to_string())
                .header(TENANT_ID_HEADER, Uuid::new_v4().to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_scope.status(), StatusCode::UNAUTHORIZED);

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
    let state = AppState::development_with_password("test-password");
    state.set_ready(true);
    let operator_id = DEVELOPMENT_OPERATOR_ID.as_uuid();
    let tenant_id = DEVELOPMENT_TENANT_ID.as_uuid();
    let event = EventEnvelope::new(
        "knowledge.imported",
        TenantScope::new(operator_id.into(), tenant_id.into(), None),
        Uuid::new_v4(),
        1,
        Uuid::new_v4(),
    );
    let _ = state.publish_event(event);
    let app = router(state);
    let (cookie, _, _) = login(&app).await;
    let response = app
        .oneshot(authenticated_request(
            "GET",
            &format!("/api/v1/events?tenant_id={tenant_id}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
}

#[tokio::test]
async fn login_session_logout_and_cookie_scope_are_server_bound() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf, body) = login(&app).await;
    assert_eq!(body["user"].as_object().unwrap().len(), 3);
    assert_eq!(body["user"]["login_name"], DEVELOPMENT_USER_EMAIL);
    assert_eq!(body["operator"]["slug"], "memeloop");
    assert_eq!(
        body["memberships"][0]["tenant_id"],
        DEVELOPMENT_TENANT_ID.to_string()
    );

    let session = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            "/api/v1/auth/session",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);
    assert_eq!(session.headers()["cache-control"], "no-store");

    let logout = app
        .clone()
        .oneshot(authenticated_request(
            "DELETE",
            "/api/v1/auth/session",
            &cookie,
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert_eq!(logout.headers()["cache-control"], "no-store");

    let revoked = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            "/api/v1/auth/session",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

    let cross_host = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/session")
                .header("host", "evil.example")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cross_host.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tenant_selector_and_origin_csrf_fail_closed() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf, _) = login(&app).await;

    let missing_tenant = app
        .clone()
        .oneshot(authenticated_request(
            "GET",
            "/api/v1/events",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing_tenant.status(), StatusCode::BAD_REQUEST);

    let bad_origin = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/auth/session")
                .header("host", "localhost:8080")
                .header("origin", "https://attacker.example")
                .header("cookie", cookie.clone())
                .header(geo_api::CSRF_HEADER, csrf.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);

    let bad_csrf = app
        .oneshot(authenticated_request(
            "DELETE",
            "/api/v1/auth/session",
            &cookie,
            Some("wrong-csrf"),
        ))
        .await
        .unwrap();
    assert_eq!(bad_csrf.status(), StatusCode::FORBIDDEN);
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
