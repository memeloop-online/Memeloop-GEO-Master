use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use geo_api::{AppState, BrowserBridge, CSRF_HEADER, ChannelService, router};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, Membership, MemoryAuthRepository, Role, User,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
use uuid::Uuid;

const ENDPOINT: &str = "/api/v1/pdf/font-subset";
const FONT: &[u8] = b"OTTO\0\xff\x80subset font";

struct Fixture {
    app: Router,
    cookie: String,
    csrf: String,
}

async fn fixture(bridge: Option<BrowserBridge>) -> Fixture {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "test-password",
    ));
    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "font-viewer@localhost",
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
    let mut service = ChannelService::development();
    service.browser = bridge;
    let app = router(
        AppState::development_with_password("test-password")
            .with_auth_repository(auth)
            .with_channel_service(service),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "login_name": "font-viewer@localhost", "password": "viewer-password"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let bytes = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    Fixture {
        app,
        cookie,
        csrf: body["csrf_token"].as_str().unwrap().to_owned(),
    }
}

fn request(fixture: &Fixture, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(ENDPOINT)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &fixture.cookie)
        .header(CSRF_HEADER, &fixture.csrf)
        // Font generation must not enter the generic idempotency cache.
        .header("idempotency-key", "font-fixture")
        .body(body.into())
        .unwrap()
}

async fn runner(app: Router) -> (BrowserBridge, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        BrowserBridge::new(format!("http://{address}"), "font-fixture-token".into()).unwrap(),
        server,
    )
}

#[tokio::test]
async fn readonly_session_without_project_gets_unchanged_binary_for_each_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (bridge, server) = runner(Router::new().route(
        "/v1/font-subset",
        post(move |request: Request<Body>| {
            let calls = observed.clone();
            async move {
                assert_eq!(
                    request.headers()[header::AUTHORIZATION],
                    "Bearer font-fixture-token"
                );
                let bytes = to_bytes(request.into_body(), 1024).await.unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let expected = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    65
                } else {
                    66
                };
                assert_eq!(body, json!({"codepoints": [expected, 20013, 128512]}));
                ([(header::CONTENT_TYPE, "font/otf")], FONT).into_response()
            }
        }),
    ))
    .await;
    let fixture = fixture(Some(bridge)).await;
    for codepoint in [65, 66] {
        let response = fixture
            .app
            .clone()
            .oneshot(request(
                &fixture,
                json!({"codepoints": [codepoint, 20013, 128512]}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "font/otf");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            FONT
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn authentication_csrf_and_body_limit_reject_before_runner() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (bridge, server) = runner(Router::new().route(
        "/v1/font-subset",
        post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async { StatusCode::INTERNAL_SERVER_ERROR }
        }),
    ))
    .await;
    let fixture = fixture(Some(bridge)).await;
    for (kind, status) in [
        ("session", StatusCode::UNAUTHORIZED),
        ("csrf", StatusCode::FORBIDDEN),
        ("origin", StatusCode::FORBIDDEN),
        ("json", StatusCode::BAD_REQUEST),
        ("structure", StatusCode::UNPROCESSABLE_ENTITY),
        ("limit", StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let body = match kind {
            "json" => "{".to_owned(),
            "structure" => json!({"codepoints": ["A"]}).to_string(),
            "limit" => format!("{{\"codepoints\":[{}]}}", "1,".repeat(4 * 1024 * 1024)),
            _ => json!({"codepoints": [65]}).to_string(),
        };
        let mut request = request(&fixture, body);
        match kind {
            "session" => {
                request.headers_mut().remove(header::COOKIE);
            }
            "csrf" => {
                request.headers_mut().remove(CSRF_HEADER);
            }
            "origin" => {
                request
                    .headers_mut()
                    .insert(header::ORIGIN, "https://other.invalid".parse().unwrap());
            }
            _ => {}
        }
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), status, "{kind}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn upstream_scalar_rejection_and_failures_use_sanitized_errors() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let redirects = Arc::new(AtomicUsize::new(0));
    let redirected = redirects.clone();
    let (bridge, server) = runner(
        Router::new()
            .route(
                "/v1/font-subset",
                post(move |Json(body): Json<Value>| {
                    let attempt = observed.fetch_add(1, Ordering::SeqCst);
                    async move {
                        assert_eq!(body, json!({"codepoints": [1.5, 55296, 1114112]}));
                        match attempt {
                            0 => (StatusCode::BAD_REQUEST, "private upstream body").into_response(),
                            1 => (StatusCode::INTERNAL_SERVER_ERROR, "private upstream body")
                                .into_response(),
                            2 => (
                                StatusCode::TEMPORARY_REDIRECT,
                                [(header::LOCATION, "/redirected")],
                            )
                                .into_response(),
                            3 => {
                                ([(header::CONTENT_TYPE, "application/json")], "{}").into_response()
                            }
                            _ => ([(header::CONTENT_TYPE, "font/otf")], "").into_response(),
                        }
                    }
                }),
            )
            .route(
                "/redirected",
                post(move || {
                    redirected.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::OK }
                }),
            ),
    )
    .await;
    let fixture = fixture(Some(bridge)).await;
    for attempt in 0..5 {
        let response = fixture
            .app
            .clone()
            .oneshot(request(
                &fixture,
                json!({"codepoints": [1.5, 55296, 1114112]}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if attempt == 0 {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        );
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["message"],
            if attempt == 0 {
                "invalid PDF font codepoints"
            } else {
                "PDF font unavailable"
            }
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    assert_eq!(redirects.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn unconfigured_or_oversized_font_is_unavailable() {
    let fixture_without_runner = fixture(None).await;
    let response = fixture_without_runner
        .app
        .clone()
        .oneshot(request(
            &fixture_without_runner,
            json!({"codepoints": [65]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let (bridge, server) = runner(Router::new().route(
        "/v1/font-subset",
        post(|| async {
            let stream = futures_util::stream::iter(
                (0..33).map(|_| Ok::<_, std::convert::Infallible>(vec![0_u8; 1024 * 1024])),
            );
            (
                [(header::CONTENT_TYPE, "font/otf")],
                Body::from_stream(stream),
            )
                .into_response()
        }),
    ))
    .await;
    let fixture = fixture(Some(bridge)).await;
    let response = fixture
        .app
        .clone()
        .oneshot(request(&fixture, json!({"codepoints": [65]}).to_string()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    server.abort();
}
