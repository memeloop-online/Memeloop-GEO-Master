use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, CSRF_HEADER, router};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, Membership, MemoryAuthRepository, ProjectCreate, ProjectSettings,
    Role, TenantScope, UserId,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    path: String,
    cookie: String,
    csrf: String,
    tenant: String,
}
impl Fixture {
    fn request(&self, method: &str, suffix: &str, body: Value, csrf: bool) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("{}{}", self.path, suffix))
            .header("host", "localhost:8080")
            .header("origin", "http://localhost:5173")
            .header("cookie", &self.cookie)
            .header("x-tenant-selector", &self.tenant)
            .header("content-type", "application/json");
        if csrf {
            request = request.header(CSRF_HEADER, &self.csrf);
        }
        request.body(Body::from(body.to_string())).unwrap()
    }
}
async fn fixture(role: Role) -> Fixture {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "synthetic-password",
    ));
    let tenant = Uuid::new_v4();
    auth.insert_membership(Membership::new(
        UserId::new(Uuid::from_u128(0x00000000000040008000000000000004)),
        DEVELOPMENT_OPERATOR_ID,
        tenant.into(),
        role,
    ))
    .await
    .unwrap();
    let state =
        AppState::development_with_password("synthetic-password").with_auth_repository(auth);
    let project = state
        .project_repository()
        .create(
            &TenantScope::new(DEVELOPMENT_OPERATOR_ID, tenant.into(), None),
            ProjectCreate {
                slug: None,
                display_name: "Synthetic settings".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let app = router(state);
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
                    json!({"login_name":"demo@localhost","password":"synthetic-password"})
                        .to_string(),
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
        .into();
    let body = body(response).await;
    Fixture {
        app,
        path: format!("/api/v1/projects/{}/ai-settings", project.id),
        cookie,
        csrf: body["csrf_token"].as_str().unwrap().into(),
        tenant: tenant.to_string(),
    }
}
async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}
fn custom() -> Value {
    json!({"expected_revision":0,"mode":"custom","model":"synthetic-model","base_url":"http://127.0.0.1:1/v1","api_key":"synthetic-key"})
}

#[tokio::test]
async fn settings_reads_redact_keys_and_writes_require_admin_csrf_and_revision() {
    let f = fixture(Role::CustomerAdmin).await;
    let response = f
        .app
        .clone()
        .oneshot(f.request("GET", "", json!(null), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let initial = body(response).await;
    assert_eq!(initial["items"].as_array().unwrap().len(), 2);
    assert_eq!(initial["items"][0]["effective"]["configured"], false);
    let response = f
        .app
        .clone()
        .oneshot(f.request("PUT", "/workbench_content", custom(), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = f
        .app
        .clone()
        .oneshot(f.request("PUT", "/workbench_content", custom(), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let saved = body(response).await;
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["key_present"], true);
    assert!(!saved.to_string().contains("synthetic-key"));
    let response = f
        .app
        .clone()
        .oneshot(f.request("PUT", "/workbench_content", custom(), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = f
        .app
        .clone()
        .oneshot(f.request(
            "POST",
            "/workbench_content/test",
            json!({"expected_revision":0}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = f
        .app
        .clone()
        .oneshot(f.request(
            "POST",
            "/workbench_content/test",
            json!({"expected_revision":1}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body(response).await.to_string().contains("synthetic-key"));
    let mut req = f.request("GET", "", json!(null), false);
    *req.uri_mut() = format!("/api/v1/projects/{}/ai-settings", Uuid::new_v4())
        .parse()
        .unwrap();
    assert_eq!(
        f.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn read_only_and_member_can_read_but_cannot_save_or_probe() {
    for role in [Role::CustomerReadOnly, Role::CustomerMember] {
        let f = fixture(role).await;
        assert_eq!(
            f.app
                .clone()
                .oneshot(f.request("GET", "", json!(null), false))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            f.app
                .clone()
                .oneshot(f.request("PUT", "/workbench_content", custom(), true))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        for suffix in ["/workbench_content/test", "/workbench_content/models"] {
            assert_eq!(
                f.app
                    .clone()
                    .oneshot(f.request("POST", suffix, json!({"expected_revision":0}), true))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
    }
}

#[tokio::test]
async fn tests_and_model_discovery_use_saved_credentials_and_provider_requests() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers =
                        String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic-key")
            );
            let response=if index==0 {
                assert!(request.starts_with("POST /v1/chat/completions "));
                assert!(request.contains("synthetic-model"));
                json!({"id":"synthetic-response","model":"synthetic-model","choices":[{"message":{"content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":1,"total_tokens":5}})
            } else {
                assert!(request.starts_with("GET /v1/models "));
                json!({"data":[{"id":"synthetic-model"},{"id":"synthetic-other"}]})
            }.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).as_bytes()).await.unwrap();
        }
    });
    let f = fixture(Role::CustomerAdmin).await;
    let mut input = custom();
    input["base_url"] = json!(endpoint);
    assert_eq!(
        f.app
            .clone()
            .oneshot(f.request("PUT", "/workbench_content", input, true))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = f
        .app
        .clone()
        .oneshot(f.request(
            "POST",
            "/workbench_content/test",
            json!({"expected_revision":1}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, json!({"success":true}));
    let response = f
        .app
        .clone()
        .oneshot(f.request(
            "POST",
            "/workbench_content/models",
            json!({"expected_revision":1}),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["items"].as_array().unwrap().len(), 2);
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
