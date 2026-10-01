use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::Duration;
use geo_api::{
    AppState, CSRF_HEADER, EventBus, MemoryIdempotencyStore, MemoryOperationStore,
    RepositoryHostOps, reduce_cycle_report, router,
};
use geo_domain::{
    DEVELOPMENT_TENANT_ID, DEVELOPMENT_USER_EMAIL, ErrorCode, Membership, MemoryAuthRepository,
    ProjectCreate, ProjectSettings, ProjectStartCommand, ReportAvailability, Role, TenantScope,
    User, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_worker::{HostOps, ReportGetRequest, ReportReduceRequest};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

async fn fixture() -> (
    AppState,
    Arc<MemoryAuthRepository>,
    geo_domain::ProjectId,
    Uuid,
    chrono::DateTime<chrono::Utc>,
) {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "test-password",
    ));
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth.clone(),
        Arc::new(geo_domain::MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    );
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        None,
    );
    let settings = ProjectSettings {
        brand_name: "Example".into(),
        market: "US".into(),
        language: "en".into(),
        report_timezone: "Asia/Shanghai".into(),
        initial_sources: vec![geo_domain::InitialSource {
            kind: geo_domain::InitialSourceKind::Text,
            value: "Public product description".into(),
            visibility: geo_domain::InitialSourceVisibility::Public,
            version_ref: None,
            content_hash: None,
        }],
        ..Default::default()
    };
    let repo = state.project_repository();
    let project = repo
        .create(
            &scope,
            ProjectCreate {
                slug: Some("report-test".into()),
                display_name: "Report test".into(),
                settings,
            },
        )
        .await
        .unwrap();
    let hash = settings_hash(&project.settings).unwrap();
    let acceptance = repo
        .start(
            &scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("report-test"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let started = repo.get_start(&scope, project.id).await.unwrap().unwrap();
    assert_eq!(started.report_timezone, "Asia/Shanghai");
    (
        state,
        auth,
        project.id,
        acceptance.cycle_id,
        started.cutoff_at,
    )
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
                    json!({"login_name":name,"password":password}).to_string(),
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

fn request(method: &str, uri: &str, cookie: &str, csrf: Option<&str>, body: &str) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie)
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        req = req.header(CSRF_HEADER, csrf);
    }
    req.body(Body::from(body.to_owned())).unwrap()
}

#[tokio::test]
async fn due_report_replays_immutable_snapshot_and_keeps_absent_sources_unavailable() {
    let (state, _, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let early = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff - Duration::seconds(1),
    )
    .await
    .unwrap_err();
    assert_eq!(early.code, ErrorCode::NotReady);
    let first = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(first.report_timezone, "Asia/Shanghai");
    assert_eq!(first.documents.availability, ReportAvailability::Unsealed);
    assert_eq!(
        first.publications.availability,
        ReportAvailability::Unsealed
    );
    assert_eq!(
        first.measurements.availability,
        ReportAvailability::Unavailable
    );
    assert_eq!(first.documents.expected_count, None);
    assert!(first.evidence.is_empty());
    let replay = reduce_cycle_report(&state, &scope, cycle_id, None, cutoff + Duration::days(2))
        .await
        .unwrap();
    assert_eq!(replay, first);
    let corrected = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        Some(first.report_id),
        cutoff + Duration::days(2),
    )
    .await
    .unwrap();
    assert_eq!(corrected.revision, 2);
    assert_eq!(corrected.correction_of, Some(first.report_id));
    assert_eq!(
        corrected.report_id,
        reduce_cycle_report(
            &state,
            &scope,
            cycle_id,
            Some(first.report_id),
            cutoff + Duration::days(3)
        )
        .await
        .unwrap()
        .report_id
    );
    assert_eq!(
        state
            .report_repository()
            .list(&scope, project_id)
            .await
            .unwrap()
            .len(),
        2
    );
    let tools =
        RepositoryHostOps::new(state.knowledge_repository()).with_report_state(state.clone());
    assert_eq!(
        tools
            .report_get(&scope, ReportGetRequest { report_id: None })
            .await
            .unwrap()
            .report_id,
        corrected.report_id
    );
    assert_eq!(
        tools
            .report_reduce(
                &scope,
                ReportReduceRequest {
                    cycle_id: None,
                    correction_of: None,
                }
            )
            .await
            .unwrap()
            .report_id,
        first.report_id
    );
    let other_scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        Uuid::new_v4().into(),
        Some(project_id),
    );
    assert_eq!(
        reduce_cycle_report(
            &state,
            &other_scope,
            cycle_id,
            None,
            cutoff + Duration::days(1)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
    assert!(
        tools
            .report_get(
                &other_scope,
                ReportGetRequest {
                    report_id: Some(first.report_id)
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn http_reads_are_scoped_and_viewer_cannot_trigger_reduction() {
    let (state, auth, project_id, cycle_id, cutoff) = fixture().await;
    let scope = TenantScope::new(
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let report = reduce_cycle_report(
        &state,
        &scope,
        cycle_id,
        None,
        cutoff + Duration::seconds(1),
    )
    .await
    .unwrap();
    let viewer = User::new(
        Uuid::new_v4().into(),
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        "report-viewer@localhost",
        "Viewer",
        "viewer-password",
    )
    .unwrap();
    auth.insert_user(viewer.clone()).await.unwrap();
    auth.insert_membership(Membership::new(
        viewer.id,
        geo_domain::DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Role::CustomerReadOnly,
    ))
    .await
    .unwrap();
    let app = router(state);
    let (cookie, csrf) = login(&app, "report-viewer@localhost", "viewer-password").await;
    let selector = format!("tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}");
    let list = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/projects/{project_id}/reports?tenant_id={DEVELOPMENT_TENANT_ID}"),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list: Value =
        serde_json::from_slice(&to_bytes(list.into_body(), 128 * 1024).await.unwrap()).unwrap();
    assert_eq!(list["items"][0]["report_id"], report.report_id.to_string());
    let detail = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/reports/{}?{selector}", report.report_id),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let evidence = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/reports/{}/evidence?{selector}", report.report_id),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(evidence.status(), StatusCode::OK);
    let evidence: Value =
        serde_json::from_slice(&to_bytes(evidence.into_body(), 16 * 1024).await.unwrap()).unwrap();
    assert_eq!(evidence, json!({"items":[]}));
    let forbidden = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/cycles/{cycle_id}/reductions?{selector}"),
            &cookie,
            Some(&csrf),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let (writer_cookie, writer_csrf) = login(&app, DEVELOPMENT_USER_EMAIL, "test-password").await;
    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/cycles/{cycle_id}/reductions?{selector}"),
            &writer_cookie,
            Some(&writer_csrf),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: Value =
        serde_json::from_slice(&to_bytes(replay.into_body(), 128 * 1024).await.unwrap()).unwrap();
    assert_eq!(replay["report_id"], report.report_id.to_string());
    let synthetic = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/cycles/{cycle_id}/reductions?{selector}"),
            &writer_cookie,
            Some(&writer_csrf),
            r#"{"measurements":[{"status":"observed"}]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(synthetic.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let missing = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/reports/{}?{selector}", Uuid::new_v4()),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let wrong_project = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/reports/{}?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={}",
                report.report_id,
                Uuid::new_v4()
            ),
            &cookie,
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(wrong_project.status(), StatusCode::NOT_FOUND);
}
