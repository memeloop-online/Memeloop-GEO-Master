use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, EventBus, MemoryIdempotencyStore, MemoryOperationStore, router};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, InitialSource, InitialSourceKind,
    InitialSourceVisibility, Membership, MemoryAuthRepository, MemoryProjectRepository,
    ProjectCreate, ProjectRepository, ProjectSettings, Role, TenantScope, User,
};
use tower::ServiceExt;
use uuid::Uuid;

async fn login(app: &axum::Router, name: &str, password: &str) -> (String, String) {
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
                    serde_json::json!({"login_name":name,"password":password}).to_string(),
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
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

fn request(method: &str, uri: &str, cookie: &str, csrf: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("cookie", cookie)
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    builder.body(Body::from("{}")).unwrap()
}

#[tokio::test]
async fn content_reads_are_scoped_and_writes_require_project_writer() {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "owner-password",
    ));
    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "content-viewer@localhost",
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
    let projects = Arc::new(MemoryProjectRepository::default());
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = projects
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Visible project".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Public introduction".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let app = router(AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        projects,
        EventBus::default(),
        false,
    ));
    let (cookie, csrf) = login(&app, "content-viewer@localhost", "viewer-password").await;
    let list = format!(
        "/api/v1/projects/{}/contents?tenant_id={}",
        project.id, DEVELOPMENT_TENANT_ID
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", &list, &cookie, None))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let start = format!(
        "/api/v1/projects/{}/cycles/{}/document-executions?tenant_id={}",
        project.id,
        Uuid::new_v4(),
        DEVELOPMENT_TENANT_ID
    );
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &start, &cookie, Some(&csrf)))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let foreign = format!(
        "/api/v1/projects/{}/contents?tenant_id={}",
        Uuid::new_v4(),
        DEVELOPMENT_TENANT_ID
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", &foreign, &cookie, None))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}
