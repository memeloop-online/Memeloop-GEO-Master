use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, CSRF_HEADER, router};
use geo_domain::{AuthRepository, Membership, MemoryAuthRepository, Operator, Role, User};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn fixture() -> (Router, Arc<MemoryAuthRepository>) {
    let repository = Arc::new(MemoryAuthRepository::new());
    for (host, name, role) in [
        ("one.test", "One Operator", Role::OemAdmin),
        ("two.test", "Two Operator", Role::CustomerAdmin),
    ] {
        let operator = Operator::new(Uuid::new_v4().into(), host, name).unwrap();
        repository
            .insert_operator(operator.clone(), &[host.to_owned()])
            .await
            .unwrap();
        let user = User::new(
            Uuid::new_v4().into(),
            operator.id,
            format!("user@{host}"),
            "Test User",
            "test-password",
        )
        .unwrap();
        repository.insert_user(user.clone()).await.unwrap();
        repository
            .insert_membership(Membership::new(
                user.id,
                operator.id,
                Uuid::new_v4().into(),
                role,
            ))
            .await
            .unwrap();
    }
    let app = router(
        AppState::development()
            .with_auth_repository(repository.clone())
            .with_origin_scheme("http"),
    );
    (app, repository)
}

#[allow(clippy::too_many_arguments)] // Tests vary host, authentication and revision independently.
async fn send(
    app: &Router,
    method: &str,
    host: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    etag: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    if method != "GET" {
        builder = builder.header("origin", format!("http://{host}"));
    }
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    if let Some(etag) = etag {
        builder = builder.header("if-match", etag);
    }
    let request = if let Some(body) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 65_536).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, json)
}

async fn login(app: &Router, host: &str) -> (String, String) {
    let body = json!({"login_name":format!("user@{host}"),"password":"test-password"});
    let (status, headers, data) = send(
        app,
        "POST",
        host,
        "/api/v1/auth/login",
        None,
        None,
        None,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (
        headers
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned(),
        data["csrf_token"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn host_brand_loads_before_login_edits_need_oem_admin_and_revision() {
    let (app, repository) = fixture().await;
    let (status, headers, one) = send(
        &app,
        "GET",
        "one.test",
        "/api/v1/public/appearance",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["display_name"], "One Operator");
    assert_eq!(one["logo_url"], Value::Null);
    assert_eq!(one["revision"], 1);
    assert_eq!(headers.get("etag").unwrap(), "\"1\"");
    let (status, _, _) = send(
        &app,
        "GET",
        "unknown.test",
        "/api/v1/public/appearance",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (cookie, csrf) = login(&app, "one.test").await;
    let update =
        json!({"display_name":"Renamed Operator","primary_color":"#abc123","default_locale":"en"});
    let (status, _, _) = send(
        &app,
        "PUT",
        "one.test",
        "/api/v1/operator/appearance",
        Some(&cookie),
        None,
        Some("\"1\""),
        Some(update.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &app,
        "PUT",
        "one.test",
        "/api/v1/operator/appearance",
        Some(&cookie),
        Some(&csrf),
        None,
        Some(update.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, headers, changed) = send(
        &app,
        "PUT",
        "one.test",
        "/api/v1/operator/appearance",
        Some(&cookie),
        Some(&csrf),
        Some("\"1\""),
        Some(update.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(headers.get("etag").unwrap(), "\"2\"");
    assert_eq!(changed["primary_color"], "#ABC123");
    assert_eq!(changed["default_locale"], "en");
    let (status, _, _) = send(
        &app,
        "PUT",
        "one.test",
        "/api/v1/operator/appearance",
        Some(&cookie),
        Some(&csrf),
        Some("\"1\""),
        Some(update),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _, one) = send(
        &app,
        "GET",
        "one.test",
        "/api/v1/public/appearance",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["display_name"], "Renamed Operator");
    let (status, _, two) = send(
        &app,
        "GET",
        "two.test",
        "/api/v1/public/appearance",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(two["display_name"], "Two Operator");
    assert_eq!(two["revision"], 1);
    let (status, _, _) = send(
        &app,
        "GET",
        "two.test",
        "/api/v1/operator/appearance",
        Some(&cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (two_cookie, two_csrf) = login(&app, "two.test").await;
    let (status, _, _) = send(
        &app,
        "PUT",
        "two.test",
        "/api/v1/operator/appearance",
        Some(&two_cookie),
        Some(&two_csrf),
        Some("\"1\""),
        Some(json!({"display_name":"Cannot edit","primary_color":"#aabbcc","default_locale":"en"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        repository
            .operator_for_host("one.test")
            .await
            .unwrap()
            .is_some()
    );
}
