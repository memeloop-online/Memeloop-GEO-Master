use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{
    AppState, CSRF_HEADER, ChannelService, EventBus, MemoryIdempotencyStore, MemoryOperationStore,
    router,
};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, Membership, MemoryAuthRepository,
    MemoryProjectRepository, ProjectCreate, ProjectSettings, Role, TenantScope, UserId,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

async fn fixture() -> (Router, String, String, String) {
    let state = AppState::development_with_password("channel-test");
    let scope = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &scope,
            ProjectCreate {
                slug: Some("channel-test".into()),
                display_name: "Channel test".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":"demo@localhost","password":"channel-test"}).to_string(),
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
        serde_json::from_slice(&to_bytes(response.into_body(), 16 * 1024).await.unwrap()).unwrap();
    (
        app,
        project.id.to_string(),
        cookie,
        body["csrf_token"].as_str().unwrap().to_owned(),
    )
}

async fn pool_fixture(
    role: Role,
    membership_matches_configured_pool: bool,
) -> (Router, String, String, String) {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "channel-test",
    ));
    let pool_tenant = Uuid::new_v4();
    auth.insert_membership(Membership::new(
        UserId::new(Uuid::from_u128(0x00000000000040008000000000000004)),
        DEVELOPMENT_OPERATOR_ID,
        pool_tenant.into(),
        role,
    ))
    .await
    .unwrap();
    let configured_pool = if membership_matches_configured_pool {
        pool_tenant
    } else {
        Uuid::new_v4()
    };
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        Arc::new(MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    )
    .with_channel_service(
        ChannelService::development().with_operator_pool_tenant_id(configured_pool.into()),
    );
    let customer = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &customer,
            ProjectCreate {
                slug: Some("pool-test".into()),
                display_name: "Pool test".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":"demo@localhost","password":"channel-test"}).to_string(),
        ))
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
    let body = json_body(login).await;
    (
        app,
        project.id.to_string(),
        cookie,
        body["csrf_token"].as_str().unwrap().to_owned(),
    )
}

fn request(
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: String,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if !path.starts_with("/api/v1/operator/") && path != "/api/v1/channel-platforms" {
        builder = builder.header("x-tenant-selector", DEVELOPMENT_TENANT_ID.to_string());
    }
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    builder.body(Body::from(body)).unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn connector_settings_are_resource_admin_only_and_fail_closed() {
    let (app, project, cookie, csrf) = pool_fixture(Role::ResourceAdmin, true).await;
    let operator_url = "/api/v1/operator/connector-capabilities";
    let list = app
        .clone()
        .oneshot(request(
            "GET",
            operator_url,
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list = json_body(list).await;
    assert!(list["items"].as_array().unwrap().iter().any(|entry| {
        entry["platform_id"] == "zhihu" && entry["availability"] == "unavailable"
    }));

    let patch = format!("{operator_url}/zhihu/primary");
    let enabled = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":true,"content_types":["article"]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::BAD_REQUEST);
    let unknown = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":false,"content_types":[],"verification":true})
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let disabled = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":false,"content_types":[]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(json_body(disabled).await["revision"], 1);
    let stale = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":false,"content_types":[]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let project_list = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/projects/{project}/connector-capabilities"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(project_list.status(), StatusCode::OK);
    let project_list = json_body(project_list).await;
    let row = project_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["platform_id"] == "zhihu")
        .unwrap();
    assert!(row.get("verified_content_types").is_none());
    assert!(row.get("deployed_version").is_none());
    assert_eq!(row["content_types"], json!([]));

    for (role, matches_pool) in [(Role::CustomerAdmin, true), (Role::ResourceAdmin, false)] {
        let (app, _, cookie, csrf) = pool_fixture(role, matches_pool).await;
        let list = app
            .clone()
            .oneshot(request(
                "GET",
                operator_url,
                Some(&cookie),
                None,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::FORBIDDEN);
        let denied = app
            .clone()
            .oneshot(request(
                "PATCH",
                &patch,
                Some(&cookie),
                Some(&csrf),
                json!({"expected_revision":0,"enabled":false,"content_types":[]}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn account_group_settings_scope_and_no_client_claimed_identity() {
    let (app, project, cookie, csrf) = fixture().await;
    let group = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-groups",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"name":"Editorial"}).to_string(),
        ))
        .await
        .unwrap();
    let group_status = group.status();
    let group = json_body(group).await;
    assert_eq!(group_status, StatusCode::CREATED, "group response: {group}");
    let group_id = group["group_id"].as_str().unwrap();
    let settings = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/api/v1/channel-settings",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"default_group_id":group_id}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(settings.status(), StatusCode::OK);
    let account = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-accounts",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"platform":"zhihu"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::CREATED);
    let account = json_body(account).await;
    assert_eq!(account["group_id"], group_id);
    assert_eq!(account["status"], "needs_login");
    assert!(account["platform_account_id"].is_null());
    let id = account["account_id"].as_str().unwrap();
    let forbidden_claim = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/channel-accounts/{id}?project_id={project}"),
            Some(&cookie),
            Some(&csrf),
            json!({"status":"ready","platform_account_id":"invented"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(forbidden_claim.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let no_browser = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-login-sessions",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"account_id":id}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(no_browser.status(), StatusCode::SERVICE_UNAVAILABLE);
    let foreign = uuid::Uuid::new_v4();
    let list = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={foreign}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pool_admin_assignment_and_customer_mutation_boundary() {
    let (app, project, cookie, csrf) = pool_fixture(Role::ResourceAdmin, true).await;
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/operator/channel-accounts",
            Some(&cookie),
            Some(&csrf),
            json!({"platform":"zhihu"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let id = created["account_id"].as_str().unwrap();
    let before = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={project}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(
        json_body(before).await["items"].as_array().unwrap().len(),
        0
    );
    let assigned = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/operator/channel-accounts/{id}/assignments"),
            Some(&cookie),
            Some(&csrf),
            json!({"tenant_id":DEVELOPMENT_TENANT_ID,"project_id":project}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(assigned.status(), StatusCode::NO_CONTENT);
    let visible = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={project}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    let visible = json_body(visible).await;
    assert_eq!(visible["items"][0]["owner_kind"], "operator_pool");
    assert!(visible["items"][0]["proxy_server"].is_null());
    let customer_patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/channel-accounts/{id}?project_id={project}"),
            Some(&cookie),
            Some(&csrf),
            json!({"enabled":false}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(customer_patch.status(), StatusCode::NOT_FOUND);
    let unassigned = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/operator/channel-accounts/{id}/assignments"),
            Some(&cookie),
            Some(&csrf),
            json!({"tenant_id":DEVELOPMENT_TENANT_ID,"project_id":project}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(unassigned.status(), StatusCode::NO_CONTENT);
    let after = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={project}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert!(
        json_body(after).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn only_pool_tenant_admin_roles_can_manage_pool() {
    for (role, membership_matches_configured_pool, expected) in [
        (Role::Operator, true, StatusCode::FORBIDDEN),
        (Role::OemAdmin, true, StatusCode::CREATED),
        (Role::ResourceAdmin, false, StatusCode::FORBIDDEN),
    ] {
        let (app, _, cookie, csrf) = pool_fixture(role, membership_matches_configured_pool).await;
        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/operator/channel-accounts",
                Some(&cookie),
                Some(&csrf),
                json!({"platform":"zhihu"}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "role {role:?}");
    }
}

#[tokio::test]
async fn platform_catalogue_requires_session_but_no_tenant_selector() {
    let (app, _, cookie, _) = fixture().await;
    let anonymous = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/channel-platforms",
            None,
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let authenticated = app
        .oneshot(request(
            "GET",
            "/api/v1/channel-platforms",
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(authenticated.status(), StatusCode::OK);
    assert_eq!(
        json_body(authenticated).await["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}
