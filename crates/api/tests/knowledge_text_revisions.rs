use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, CSRF_HEADER, router};
use geo_domain::{DEVELOPMENT_TENANT_ID, DEVELOPMENT_USER_EMAIL};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn response_json(response: Response<Body>) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

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
                    json!({"login_name":DEVELOPMENT_USER_EMAIL,"password":"test-password"})
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
        .to_owned();
    let csrf = response_json(response).await["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    (cookie, csrf)
}

#[allow(clippy::too_many_arguments)] // Keep independent auth and revision headers explicit in request tests.
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    revision: Option<&str>,
    key: Option<&str>,
    body: Option<Value>,
) -> Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie);
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    if let Some(revision) = revision {
        builder = builder.header("if-match", revision);
    }
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    app.clone()
        .oneshot(
            builder
                .body(Body::from(body.map_or_else(String::new, |v| v.to_string())))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn project(app: &Router, cookie: &str, csrf: &str, key: &str) -> String {
    let response = call(
        app,
        "POST",
        &format!("/api/v1/projects?tenant_id={DEVELOPMENT_TENANT_ID}"),
        cookie,
        Some(csrf),
        None,
        Some(key),
        Some(json!({"display_name":"Revision test","settings":{}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn source(app: &Router, cookie: &str, csrf: &str, selector: &str, key: &str) -> Value {
    let response = call(
        app,
        "POST",
        &format!("/api/v1/knowledge/imports?{selector}"),
        cookie,
        Some(csrf),
        None,
        None,
        Some(json!({"items":[{
            "client_item_id":key,"kind":"text","name":"Reference",
            "purpose":"public","text":"Original source evidence."
        }]})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    response_json(response).await["items"][0].clone()
}

#[tokio::test]
async fn authored_text_revision_replays_across_later_edits_and_keeps_original_readable() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf) = login(&app).await;
    let project_id = project(&app, &cookie, &csrf, "revision-project").await;
    let selector = format!("tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}");
    let imported = source(&app, &cookie, &csrf, &selector, "first-source").await;
    let source_id = imported["source"]["source_id"].as_str().unwrap();
    let original_id = imported["source_version"]["source_version_id"]
        .as_str()
        .unwrap();
    let revision = imported["source"]["revision"].as_i64().unwrap();
    let uri = format!("/api/v1/knowledge/sources/{source_id}/versions?{selector}");
    let original_uri =
        format!("/api/v1/knowledge/sources/{source_id}/versions/{original_id}/content?{selector}");
    let original = call(&app, "GET", &original_uri, &cookie, None, None, None, None).await;
    assert_eq!(original.status(), StatusCode::OK);
    let original = response_json(original).await;
    assert_eq!(original["representation"], "original");
    assert_eq!(original["text"], "Original source evidence.");

    let first_body = json!({
        "base_version_id":original_id, "media_type":"text/markdown",
        "text":"# 修订\n\n**保留**原文。"
    });
    let first = call(
        &app,
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some(&revision.to_string()),
        Some("revision-1"),
        Some(first_body.clone()),
    )
    .await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let receipt = response_json(first).await;
    assert_eq!(receipt["source_version"]["representation"], "authored_text");
    assert_eq!(receipt["source_version"]["parent_version_id"], original_id);
    assert_eq!(receipt["source"]["source_id"], source_id);
    assert!(receipt["knowledge_release"]["knowledge_release_id"].is_string());
    let first_id = receipt["source_version"]["source_version_id"]
        .as_str()
        .unwrap();
    let authored_uri =
        format!("/api/v1/knowledge/sources/{source_id}/versions/{first_id}/content?{selector}");
    let authored = call(&app, "GET", &authored_uri, &cookie, None, None, None, None).await;
    assert_eq!(authored.status(), StatusCode::OK);
    let authored = response_json(authored).await;
    assert_eq!(authored["text_basis"], "exact");
    assert_eq!(authored["media_type"], "text/markdown");
    assert_eq!(authored["text"], first_body["text"]);
    let second_body = json!({
        "base_version_id":first_id, "media_type":"text/plain", "text":"Second authored revision."
    });
    let second = call(
        &app,
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some(&receipt["source"]["revision"].as_i64().unwrap().to_string()),
        Some("revision-2"),
        Some(second_body),
    )
    .await;
    assert_eq!(second.status(), StatusCode::CREATED);
    let second = response_json(second).await;
    assert_eq!(second["source_version"]["parent_version_id"], first_id);
    let replay = call(
        &app,
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some(&revision.to_string()),
        Some("revision-1"),
        Some(first_body.clone()),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(response_json(replay).await, receipt);
    let changed_request = call(
        &app,
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some(&revision.to_string()),
        Some("revision-1"),
        Some(json!({
            "base_version_id":original_id,"media_type":"text/markdown",
            "text":"Changed request"
        })),
    )
    .await;
    assert_eq!(changed_request.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(changed_request).await["details"]["reason"],
        "idempotency_conflict"
    );
    let stale = call(
        &app,
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some(&revision.to_string()),
        Some("revision-3"),
        Some(first_body),
    )
    .await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        response_json(stale).await["details"]["reason"],
        "source_revision_conflict"
    );
    let old_read = call(&app, "GET", &original_uri, &cookie, None, None, None, None).await;
    assert_eq!(response_json(old_read).await, original);
}

#[tokio::test]
async fn revision_http_rejects_bad_headers_body_and_cross_scope_references() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf) = login(&app).await;
    let first_project = project(&app, &cookie, &csrf, "revision-scope-a").await;
    let second_project = project(&app, &cookie, &csrf, "revision-scope-b").await;
    let first_selector = format!("tenant_id={DEVELOPMENT_TENANT_ID}&project_id={first_project}");
    let second_selector = format!("tenant_id={DEVELOPMENT_TENANT_ID}&project_id={second_project}");
    let first = source(&app, &cookie, &csrf, &first_selector, "scope-source-a").await;
    let another = source(&app, &cookie, &csrf, &first_selector, "scope-source-b").await;
    let source_id = first["source"]["source_id"].as_str().unwrap();
    let version_id = first["source_version"]["source_version_id"]
        .as_str()
        .unwrap();
    let other_version_id = another["source_version"]["source_version_id"]
        .as_str()
        .unwrap();
    let revision = first["source"]["revision"].as_i64().unwrap().to_string();
    let uri = format!("/api/v1/knowledge/sources/{source_id}/versions?{first_selector}");
    let body = json!({
        "base_version_id":version_id,"media_type":"text/plain","text":"Valid text"
    });
    let missing_csrf = call(
        &app,
        "POST",
        &uri,
        &cookie,
        None,
        Some(&revision),
        Some("missing-csrf"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);
    for (match_header, key) in [
        (None, Some("missing-revision")),
        (Some("not-a-number"), Some("bad-revision")),
        (Some(revision.as_str()), None),
    ] {
        let response = call(
            &app,
            "POST",
            &uri,
            &cookie,
            Some(&csrf),
            match_header,
            key,
            Some(body.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    for (suffix, rejected, status) in [
        (
            "extra",
            json!({"base_version_id":version_id,"media_type":"text/plain","text":"Sensitive body","extra":true}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "blank",
            json!({"base_version_id":version_id,"media_type":"text/plain","text":" \n "}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "media",
            json!({"base_version_id":version_id,"media_type":"text/html","text":"Sensitive body"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "large",
            json!({"base_version_id":version_id,"media_type":"text/plain","text":"x".repeat(256 * 1024 + 1)}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = call(
            &app,
            "POST",
            &uri,
            &cookie,
            Some(&csrf),
            Some(&revision),
            Some(&format!("invalid-{suffix}")),
            Some(rejected),
        )
        .await;
        assert_eq!(response.status(), status);
    }
    let other_source_version = call(
        &app,
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some(&revision),
        Some("foreign-base"),
        Some(json!({"base_version_id":other_version_id,"media_type":"text/plain","text":"Valid text"})),
    )
    .await;
    assert_eq!(other_source_version.status(), StatusCode::NOT_FOUND);
    let cross_source_read = call(
        &app,
        "GET",
        &format!(
            "/api/v1/knowledge/sources/{source_id}/versions/{other_version_id}/content?{first_selector}"
        ),
        &cookie,
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(cross_source_read.status(), StatusCode::NOT_FOUND);
    // A key is reserved per source within its project: two independent
    // sources may use the same caller-generated key without aliasing receipts.
    let shared_key = "shared-source-revision-key";
    let valid_a = call(
        &app,
        "POST",
        &uri,
        &cookie,
        Some(&csrf),
        Some(&revision),
        Some(shared_key),
        Some(body.clone()),
    )
    .await;
    assert_eq!(valid_a.status(), StatusCode::CREATED);
    let other_source_id = another["source"]["source_id"].as_str().unwrap();
    let other_revision = another["source"]["revision"].as_i64().unwrap().to_string();
    let valid_b = call(
        &app,
        "POST",
        &format!("/api/v1/knowledge/sources/{other_source_id}/versions?{first_selector}"),
        &cookie,
        Some(&csrf),
        Some(&other_revision),
        Some(shared_key),
        Some(json!({"base_version_id":other_version_id,"media_type":"text/plain","text":"Different source text"})),
    )
    .await;
    assert_eq!(valid_b.status(), StatusCode::CREATED);
    assert_ne!(
        response_json(valid_a).await["source_version"]["source_version_id"],
        response_json(valid_b).await["source_version"]["source_version_id"]
    );
    let cross_project_read = call(
        &app,
        "GET",
        &format!(
            "/api/v1/knowledge/sources/{source_id}/versions/{version_id}/content?{second_selector}"
        ),
        &cookie,
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(cross_project_read.status(), StatusCode::NOT_FOUND);
    let cross_project_write = call(
        &app,
        "POST",
        &format!("/api/v1/knowledge/sources/{source_id}/versions?{second_selector}"),
        &cookie,
        Some(&csrf),
        Some(&revision),
        Some("foreign-project"),
        Some(body),
    )
    .await;
    assert_eq!(cross_project_write.status(), StatusCode::NOT_FOUND);
    let missing_read = call(
        &app,
        "GET",
        &format!(
            "/api/v1/knowledge/sources/{source_id}/versions/{}/content?{first_selector}",
            Uuid::new_v4()
        ),
        &cookie,
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(missing_read.status(), StatusCode::NOT_FOUND);
}
