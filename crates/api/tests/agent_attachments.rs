use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, CSRF_HEADER, router};
use geo_domain::{DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, TenantScope};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

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
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

#[allow(clippy::too_many_arguments)] // Test client keeps each request's auth, key, and content explicit.
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    key: Option<&str>,
    body: Vec<u8>,
    binary: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie)
        .header(
            "content-type",
            if binary {
                "application/octet-stream"
            } else {
                "application/json"
            },
        );
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} returned {status}: {error}; body={}",
                String::from_utf8_lossy(&body)
            )
        }),
    )
}

#[tokio::test]
async fn attachment_only_upload_is_verified_scoped_and_not_imported() {
    let state = AppState::development_with_password("test-password");
    let knowledge = state.knowledge_repository();
    let app = router(state);
    let (cookie, csrf) = login(&app).await;
    let project = Uuid::new_v4();
    let other = Uuid::new_v4();
    let base = format!("?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project}");
    let bytes = b"private notes".to_vec();
    let hash = hex::encode(Sha256::digest(&bytes));
    let create = json!({
        "filename": "notes.txt",
        "declared_media_type": "text/plain",
        "expected_size": bytes.len(),
        "expected_sha256": hash
    });
    let (status, session) = call(
        &app,
        "POST",
        &format!("/api/v1/agent/attachments/upload-sessions{base}"),
        &cookie,
        Some(&csrf),
        Some("upload-create"),
        serde_json::to_vec(&create).unwrap(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let session_id = session["upload_session_id"].as_str().unwrap();
    let content_uri =
        format!("/api/v1/agent/attachments/upload-sessions/{session_id}/content{base}");
    let (status, _) = call(
        &app,
        "PUT",
        &content_uri,
        &cookie,
        Some(&csrf),
        None,
        bytes,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let complete_uri =
        format!("/api/v1/agent/attachments/upload-sessions/{session_id}/complete{base}");
    let (status, reference) = call(
        &app,
        "POST",
        &complete_uri,
        &cookie,
        Some(&csrf),
        Some("upload-complete"),
        vec![],
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reference["object_id"], reference["attachment_id"]);
    assert_eq!(reference["filename"], "notes.txt");
    assert_eq!(reference["sha256"], hash);
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project.into()),
    );
    assert!(
        knowledge.list_sources(&scope).await.unwrap().is_empty(),
        "attachment upload must not create a knowledge source"
    );
    let (status, replay) = call(
        &app,
        "POST",
        &complete_uri,
        &cookie,
        Some(&csrf),
        Some("upload-complete"),
        vec![],
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, reference);
    let object_id = reference["attachment_id"].as_str().unwrap();
    let get_uri = format!("/api/v1/agent/attachments/{object_id}{base}");
    let (status, detail) = call(&app, "GET", &get_uri, &cookie, None, None, vec![], false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail, reference);
    let (status, _) = call(&app, "GET",
        &format!("/api/v1/agent/attachments/{object_id}?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={other}"),
        &cookie, None, None, vec![], false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "POST",
        &format!("/api/v1/agent/attachments/upload-sessions/{session_id}/complete?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={other}"),
        &cookie, Some(&csrf), Some("cross-project-complete"), vec![], false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, conversation) = call(
        &app,
        "POST",
        &format!("/api/v1/agent/conversations{base}"),
        &cookie,
        Some(&csrf),
        Some("conversation"),
        serde_json::to_vec(&json!({"title":"test"})).unwrap(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let message_uri = format!(
        "/api/v1/agent/conversations/{}/messages{base}",
        conversation["id"].as_str().unwrap()
    );
    let (status, _) = call(&app, "POST", &message_uri, &cookie, Some(&csrf), Some("foreign-reference"),
        serde_json::to_vec(&json!({"content":"","attachments":[{"attachment_id":Uuid::new_v4(),"object_id":"anything","filename":"fake"}]})).unwrap(), false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut tampered = reference.clone();
    tampered["size_bytes"] = json!(1);
    let (status, _) = call(
        &app,
        "POST",
        &message_uri,
        &cookie,
        Some(&csrf),
        Some("tampered-reference"),
        serde_json::to_vec(&json!({"content":"","attachments":[tampered]})).unwrap(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        "POST",
        &message_uri,
        &cookie,
        Some(&csrf),
        Some("attachment-only"),
        serde_json::to_vec(&json!({"content":"","attachments":[reference]})).unwrap(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn attachment_completion_rejects_size_and_digest_mismatch() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf) = login(&app).await;
    let base = format!(
        "?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={}",
        Uuid::new_v4()
    );
    for (case, expected_size, expected_sha256) in [
        ("size", 4, hex::encode(Sha256::digest(b"real"))),
        ("hash", 3, hex::encode(Sha256::digest(b"not"))),
    ] {
        let command = json!({"filename":"file","declared_media_type":"text/plain",
            "expected_size":expected_size,"expected_sha256":expected_sha256});
        let (status, session) = call(
            &app,
            "POST",
            &format!("/api/v1/agent/attachments/upload-sessions{base}"),
            &cookie,
            Some(&csrf),
            Some(&format!("create-{case}")),
            serde_json::to_vec(&command).unwrap(),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = session["upload_session_id"].as_str().unwrap();
        let (status, _) = call(
            &app,
            "PUT",
            &format!("/api/v1/agent/attachments/upload-sessions/{id}/content{base}"),
            &cookie,
            Some(&csrf),
            None,
            b"bad".to_vec(),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(
            &app,
            "POST",
            &format!("/api/v1/agent/attachments/upload-sessions/{id}/complete{base}"),
            &cookie,
            Some(&csrf),
            Some(&format!("complete-{case}")),
            vec![],
            false,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
