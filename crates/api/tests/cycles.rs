use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{Datelike, Duration, Timelike, Utc, Weekday};
use geo_api::{
    AppState, CSRF_HEADER, EventBus, MemoryIdempotencyStore, MemoryOperationStore, router,
};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, DEVELOPMENT_USER_EMAIL, Membership,
    MemoryAuthRepository, MemoryProjectRepository, ProjectCreate, ProjectId, ProjectSettings,
    ReportSchedule, ReportWeekday, Role, TenantScope, User,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

async fn fixture() -> (Router, Arc<MemoryAuthRepository>, ProjectId) {
    // Place the report later today in a timezone with at least an hour left
    // until 23:59; the prior natural week's Monday cutoff is already due.
    let now = Utc::now();
    let (report_timezone, local_now) = if now.hour() >= 22 {
        ("Pacific/Honolulu", now - Duration::hours(10))
    } else {
        ("UTC", now)
    };
    let report_weekday = match local_now.weekday() {
        Weekday::Mon => ReportWeekday::Monday,
        Weekday::Tue => ReportWeekday::Tuesday,
        Weekday::Wed => ReportWeekday::Wednesday,
        Weekday::Thu => ReportWeekday::Thursday,
        Weekday::Fri => ReportWeekday::Friday,
        Weekday::Sat => ReportWeekday::Saturday,
        Weekday::Sun => ReportWeekday::Sunday,
    };
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "test-password",
    ));
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth.clone(),
        Arc::new(MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    );
    let project = state
        .project_repository()
        .create(
            &TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None),
            ProjectCreate {
                slug: Some("cycle-test".into()),
                display_name: "Cycle test".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "US".into(),
                    language: "en".into(),
                    report_timezone: report_timezone.into(),
                    report_schedule: ReportSchedule {
                        report_weekday,
                        report_local_time: "23:59".into(),
                        ..ReportSchedule::default()
                    },
                    initial_sources: vec![geo_domain::InitialSource {
                        kind: geo_domain::InitialSourceKind::Text,
                        value: "Public product description".into(),
                        visibility: geo_domain::InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    (router(state), auth, project.id)
}

async fn login(app: &Router, name: &str, password: &str) -> (String, String) {
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
                    json!({"login_name": name, "password": password}).to_string(),
                ))
                .unwrap(),
        )
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
        serde_json::from_slice(&to_bytes(response.into_body(), 16 * 1024).await.unwrap()).unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie)
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        request = request.header(CSRF_HEADER, csrf);
    }
    // Project creation and start require stable transport keys.
    if uri.contains("/start?") && method == "POST" {
        request = request.header("idempotency-key", "cycle-test-start");
    } else if uri.starts_with("/api/v1/projects?") && method == "POST" {
        request = request.header("idempotency-key", "cycle-test-other-project");
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn report_advances_current_cycle_without_rewriting_start_and_replays_once() {
    let (app, _, project_id) = fixture().await;
    let (cookie, csrf) = login(&app, DEVELOPMENT_USER_EMAIL, "test-password").await;
    let prefix = format!("/api/v1/projects/{project_id}");
    let query = format!("tenant_id={DEVELOPMENT_TENANT_ID}");
    let current_uri = format!("{prefix}/cycles/current?{query}");
    let start_uri = format!("{prefix}/start?{query}");
    let next_uri = format!("{prefix}/cycles?{query}");
    let (status, _) = send(&app, "GET", &current_uri, &cookie, None, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, started) = send(
        &app,
        "POST",
        &start_uri,
        &cookie,
        Some(&csrf),
        json!({"expected_revision": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let original_id = started["cycle_id"].as_str().unwrap();
    let (status, original_start) = send(&app, "GET", &start_uri, &cookie, None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(original_start["cycle_id"], original_id);
    let (status, first) = send(&app, "GET", &current_uri, &cookie, None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["cycle_id"], original_id);
    assert!(first["report_timezone"] == "UTC" || first["report_timezone"] == "Pacific/Honolulu");
    let request = json!({"predecessor_cycle_id": original_id});
    let (status, _) = send(
        &app,
        "POST",
        &next_uri,
        &cookie,
        Some(&csrf),
        request.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "report must be saved first");

    let reduce_uri =
        format!("/api/v1/cycles/{original_id}/reductions?{query}&project_id={project_id}");
    let (status, report) = send(&app, "POST", &reduce_uri, &cookie, Some(&csrf), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["cycle_id"], original_id);
    assert_eq!(report["revision"], 1);
    let (status, successor) = send(&app, "GET", &current_uri, &cookie, None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(successor["cycle_id"], first["cycle_id"]);
    assert_eq!(
        successor["report_window_start_at"],
        first["report_window_end_at"]
    );
    assert_eq!(successor["report_timezone"], first["report_timezone"]);
    assert_eq!(successor["document_manifest"]["revision"], 1);
    assert_eq!(successor["distribution_manifest"]["revision"], 1);

    let (status, replay_report) =
        send(&app, "POST", &reduce_uri, &cookie, Some(&csrf), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay_report, report);
    let (status, replay_cycle) = send(&app, "POST", &next_uri, &cookie, Some(&csrf), request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay_cycle, successor);
    let (status, current) = send(&app, "GET", &current_uri, &cookie, None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current, successor);
    let (status, start_after) = send(&app, "GET", &start_uri, &cookie, None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(start_after, original_start);
    let (status, reports) = send(
        &app,
        "GET",
        &format!("{prefix}/reports?{query}"),
        &cookie,
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reports["items"].as_array().unwrap().len(), 1);
    let (status, _) = send(
        &app,
        "POST",
        &next_uri,
        &cookie,
        Some(&csrf),
        json!({"predecessor_cycle_id": successor["cycle_id"]}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "successor has no saved report"
    );
}

#[tokio::test]
async fn successor_rejects_synthetic_fields_and_preserves_project_and_tenant_scope() {
    let (app, auth, project_id) = fixture().await;
    let (cookie, csrf) = login(&app, DEVELOPMENT_USER_EMAIL, "test-password").await;
    let query = format!("tenant_id={DEVELOPMENT_TENANT_ID}");
    let (status, start) = send(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/start?{query}"),
        &cookie,
        Some(&csrf),
        json!({"expected_revision": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let cycle_id = start["cycle_id"].as_str().unwrap();
    let next_uri = format!("/api/v1/projects/{project_id}/cycles?{query}");
    let (status, _) = send(
        &app,
        "POST",
        &next_uri,
        &cookie,
        Some(&csrf),
        json!({"predecessor_cycle_id": cycle_id, "measurements": [{"status": "observed"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = send(&app, "POST", &next_uri, &cookie, Some(&csrf), json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "cycle-viewer@localhost",
        "Viewer",
        "viewer-password",
    )
    .unwrap();
    auth.insert_user(viewer.clone()).await.unwrap();
    auth.insert_membership(Membership::new(
        viewer.id,
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Role::CustomerReadOnly,
    ))
    .await
    .unwrap();
    let (viewer_cookie, viewer_csrf) =
        login(&app, "cycle-viewer@localhost", "viewer-password").await;
    let (status, current) = send(
        &app,
        "GET",
        &format!("/api/v1/projects/{project_id}/cycles/current?{query}"),
        &viewer_cookie,
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["cycle_id"], cycle_id);
    let (status, _) = send(
        &app,
        "POST",
        &next_uri,
        &viewer_cookie,
        Some(&viewer_csrf),
        json!({"predecessor_cycle_id": cycle_id}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, report) = send(
        &app,
        "POST",
        &format!("/api/v1/cycles/{cycle_id}/reductions?{query}&project_id={project_id}"),
        &cookie,
        Some(&csrf),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["revision"], 1);

    // A real second project has its own active cycle. Its writer must not
    // schedule the first project's reported cycle through this path.
    let (status, other) = send(
        &app,
        "POST",
        &format!("/api/v1/projects?{query}"),
        &cookie,
        Some(&csrf),
        json!({
            "display_name": "Other project",
            "settings": {
                "brand_name": "Other",
                "market": "US",
                "language": "en",
                "initial_sources": [{"kind": "url", "value": "https://example.org", "visibility": "public"}]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other_project = other["id"].as_str().unwrap();
    let (status, other_start) = send(
        &app,
        "POST",
        &format!("/api/v1/projects/{other_project}/start?{query}"),
        &cookie,
        Some(&csrf),
        json!({"expected_revision": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/v1/projects/{other_project}/cycles/current?{query}"),
        &cookie,
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/projects/{other_project}/cycles?{query}"),
        &cookie,
        Some(&csrf),
        json!({"predecessor_cycle_id": cycle_id}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, other_current) = send(
        &app,
        "GET",
        &format!("/api/v1/projects/{other_project}/cycles/current?{query}"),
        &cookie,
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(other_current["cycle_id"], other_start["cycle_id"]);
    let wrong_tenant = Uuid::new_v4();
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/v1/projects/{project_id}/cycles/current?tenant_id={wrong_tenant}"),
        &cookie,
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/cycles?tenant_id={wrong_tenant}"),
        &cookie,
        Some(&csrf),
        json!({"predecessor_cycle_id": cycle_id}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
