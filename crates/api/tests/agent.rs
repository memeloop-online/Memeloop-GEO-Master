use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, CSRF_HEADER, router};
use geo_domain::DEVELOPMENT_TENANT_ID;
use serde_json::Value;
use tower::ServiceExt;

async fn login(app: &Router) -> (String, String) {
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
                    r#"{"login_name":"demo@localhost","password":"test-password"}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("login response");
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("cookie header")
        .split(';')
        .next()
        .expect("cookie value")
        .to_owned();
    let body = to_bytes(response.into_body(), 16 * 1024)
        .await
        .expect("login body");
    let body: Value = serde_json::from_slice(&body).expect("login json");
    (
        cookie,
        body["csrf_token"].as_str().expect("csrf token").to_owned(),
    )
}

fn request(
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    key: Option<&str>,
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
        builder = builder.header(CSRF_HEADER, csrf);
    }
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

#[tokio::test]
async fn agent_conversation_submission_is_scoped_idempotent_and_honest_about_runtime() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf) = login(&app).await;
    let tenant_id = DEVELOPMENT_TENANT_ID;
    let project_id = uuid::Uuid::new_v4();
    let project_id_text = project_id.to_string();

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/agent/conversations?tenant_id={tenant_id}&project_id={project_id_text}"
            ),
            &cookie,
            Some(&csrf),
            Some("conversation-1"),
            r#"{"title":"P00"}"#,
        ))
        .await
        .expect("create response");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(
        &to_bytes(created.into_body(), 64 * 1024)
            .await
            .expect("create body"),
    )
    .expect("create json");
    let conversation_id = created["id"].as_str().expect("conversation id");

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations?tenant_id={tenant_id}&project_id={project_id_text}"
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("list response");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: Value = serde_json::from_slice(
        &to_bytes(listed.into_body(), 64 * 1024)
            .await
            .expect("list body"),
    )
    .expect("list json");
    assert_eq!(listed["items"].as_array().expect("items").len(), 1);

    let other_project = uuid::Uuid::new_v4();
    let isolated = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations?tenant_id={tenant_id}&project_id={other_project}"
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("isolated response");
    assert_eq!(isolated.status(), StatusCode::OK);
    let isolated: Value = serde_json::from_slice(
        &to_bytes(isolated.into_body(), 64 * 1024)
            .await
            .expect("isolated body"),
    )
    .expect("isolated json");
    assert!(isolated["items"].as_array().expect("items").is_empty());

    let message_uri = format!(
        "/api/v1/agent/conversations/{conversation_id}/messages?tenant_id={tenant_id}&project_id={project_id_text}"
    );
    let submitted = app
        .clone()
        .oneshot(request(
            "POST",
            &message_uri,
            &cookie,
            Some(&csrf),
            Some("message-1"),
            r#"{"content":"hello"}"#,
        ))
        .await
        .expect("submit response");
    assert_eq!(submitted.status(), StatusCode::ACCEPTED);
    let submitted_body = to_bytes(submitted.into_body(), 64 * 1024)
        .await
        .expect("submit body");
    let submitted_json: Value = serde_json::from_slice(&submitted_body).expect("submit json");
    assert_eq!(submitted_json["status"], "accepted");
    assert_eq!(submitted_json["run_status"], "failed");
    assert_eq!(submitted_json["error"]["code"], "capability_missing");

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &message_uri,
            &cookie,
            Some(&csrf),
            Some("message-1"),
            r#"{"content":"hello"}"#,
        ))
        .await
        .expect("replay response");
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(
        to_bytes(replay.into_body(), 64 * 1024)
            .await
            .expect("replay body"),
        submitted_body
    );

    let events = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations/{conversation_id}/events?tenant_id={tenant_id}&project_id={project_id_text}&after=0"
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("events response");
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(
        events
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
}
