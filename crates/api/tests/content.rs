use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{
    AppState, EventBus, MemoryIdempotencyStore, MemoryOperationStore, RepositoryHostOps,
    content_runtime::ContentWorkflowExecutor, router,
};
use geo_domain::{
    ContentCreateDocument, ContentCreateRequest, ContentExecution, ContentExecutionOrigin,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ImportItem, InitialSource, InitialSourceKind,
    InitialSourceVisibility, KnowledgePurpose, KnowledgeRepository, Membership,
    MemoryAuthRepository, MemoryKnowledgeRepository, MemoryProjectRepository, ProjectCreate,
    ProjectRepository, ProjectSettings, Role, SourceKind, TenantScope, User,
};
use geo_worker::{ContentItemsReadRequest, HostOps};
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

struct AcceptedContentExecutor(AtomicUsize);
impl ContentWorkflowExecutor for AcceptedContentExecutor {
    fn dispatch(
        &self,
        _scope: TenantScope,
        _execution_id: Uuid,
    ) -> Result<(), geo_domain::AppError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn on_demand_http_creation_enforces_scope_and_writer_and_freezes_two_documents() {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "owner-password",
    ));
    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "manual-content-viewer@localhost",
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
                display_name: "Manual content".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope {
        project_id: Some(project.id),
        ..tenant
    };
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let imported = knowledge
        .import_batch(
            &scope,
            (0..3)
                .map(|index| ImportItem {
                    client_item_id: format!("source-{index}"),
                    kind: SourceKind::Text,
                    name: format!("Source {index}"),
                    purpose: KnowledgePurpose::Public,
                    text: Some(format!("Public description {index}")),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                })
                .collect(),
        )
        .await
        .unwrap();
    let input = ContentCreateRequest {
        request_id: Uuid::new_v4(),
        topic: "Selecting suitable equipment".into(),
        source_version_ids: imported.items[..2]
            .iter()
            .map(|item| item.source_version.as_ref().unwrap().source_version_id)
            .collect(),
        documents: ["article", "faq"]
            .into_iter()
            .map(|kind| ContentCreateDocument {
                key: kind.into(),
                content_type: kind.into(),
                title: None,
                instructions: None,
            })
            .collect(),
        language: "en".into(),
        market: None,
        audience: None,
    };
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        projects.clone(),
        knowledge.clone(),
        EventBus::default(),
        false,
    );
    let executor = Arc::new(AcceptedContentExecutor(AtomicUsize::new(0)));
    state.configure_content_executor(executor.clone());
    let app = router(state.clone());
    let (owner_cookie, owner_csrf) = login(&app, "demo@localhost", "owner-password").await;
    let (viewer_cookie, viewer_csrf) =
        login(&app, "manual-content-viewer@localhost", "viewer-password").await;
    let uri = format!(
        "/api/v1/projects/{}/contents?tenant_id={}",
        project.id, DEVELOPMENT_TENANT_ID
    );
    let make_request = |uri: &str, cookie: &str, csrf: Option<&str>| {
        let mut request = request("POST", uri, cookie, csrf);
        *request.body_mut() = Body::from(serde_json::to_vec(&input).unwrap());
        request
    };
    assert_eq!(
        app.clone()
            .oneshot(make_request(&uri, &viewer_cookie, Some(&viewer_csrf)))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(make_request(&uri, &owner_cookie, None))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let foreign_uri = format!(
        "/api/v1/projects/{}/contents?tenant_id={}",
        Uuid::new_v4(),
        DEVELOPMENT_TENANT_ID
    );
    assert_eq!(
        app.clone()
            .oneshot(make_request(&foreign_uri, &owner_cookie, Some(&owner_csrf)))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(executor.0.load(Ordering::SeqCst), 0);
    let response = app
        .clone()
        .oneshot(make_request(&uri, &owner_cookie, Some(&owner_csrf)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: ContentExecution =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(accepted.expected_count, 2);
    assert_eq!(
        accepted.origin,
        ContentExecutionOrigin::OnDemand {
            request: input.clone()
        }
    );
    assert_eq!(executor.0.load(Ordering::SeqCst), 1);
    let host = RepositoryHostOps::new(knowledge).with_content(state.clone());
    let page = host
        .content_items_read(
            &scope,
            ContentItemsReadRequest {
                execution_id: accepted.execution_id,
                cursor: None,
                limit: Some(10),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.context.brand_name, None);
    assert_eq!(
        page.context.objective.as_deref(),
        Some(input.topic.as_str())
    );
    for item in page.items {
        assert_eq!(item.source_refs.len(), 2);
        assert!(
            item.source_refs
                .iter()
                .all(|source| input.source_version_ids.contains(&source.source_version_id))
        );
    }
    assert!(
        projects
            .get_current_cycle(&scope, project.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .content_service()
            .repository()
            .list_project_assets(&scope)
            .await
            .unwrap()
            .is_empty(),
        "HTTP acceptance alone does not claim generated assets"
    );
}
