use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
};
use geo_api::{
    AppState, CSRF_HEADER, EventBus, MemoryIdempotencyStore, MemoryOperationStore, router,
};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, InitialSource, InitialSourceKind,
    InitialSourceVisibility, KnowledgePurpose, Membership, MemoryAuthRepository,
    MemoryProjectRepository, ProjectCreate, ProjectId, ProjectSettings, Role, TenantScope,
    UploadSessionCommand, User,
};
use image::{
    ExtendedColorType, ImageEncoder,
    codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

const TENANT: &str = "?tenant_id=";

async fn project(state: &AppState) -> ProjectId {
    state
        .project_repository()
        .create(
            &TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None),
            ProjectCreate {
                slug: None,
                display_name: "Synthetic media project".into(),
                settings: ProjectSettings {
                    brand_name: "Synthetic".into(),
                    market: "US".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Synthetic fixture".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap()
        .id
}

async fn login(app: &Router, name: &str, password: &str) -> (String, String) {
    let result = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"login_name":name,"password":password}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    let cookie = result.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let bytes = to_bytes(result.into_body(), 8192).await.unwrap();
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, response["csrf_token"].as_str().unwrap().to_owned())
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    payload: Value,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    if let Some(csrf) = csrf {
        request = request.header(CSRF_HEADER, csrf);
    }
    let result = app
        .clone()
        .oneshot(request.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap();
    let status = result.status();
    let headers = result.headers().clone();
    let bytes = to_bytes(result.into_body(), 1024 * 1024).await.unwrap();
    (status, headers, bytes.to_vec())
}

fn body(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn base(project_id: ProjectId) -> String {
    format!("/api/v1/projects/{project_id}/content-media/bindings{TENANT}{DEVELOPMENT_TENANT_ID}")
}

fn detail(project_id: ProjectId, binding_id: &str, bytes: bool) -> String {
    let suffix = if bytes { "/bytes" } else { "" };
    format!(
        "/api/v1/projects/{project_id}/content-media/bindings/{binding_id}{suffix}{TENANT}{DEVELOPMENT_TENANT_ID}"
    )
}

fn thumbnail(project_id: ProjectId, binding_id: &str) -> String {
    format!(
        "/api/v1/projects/{project_id}/content-media/bindings/{binding_id}/thumbnail{TENANT}{DEVELOPMENT_TENANT_ID}"
    )
}

fn pixels() -> [u8; 12] {
    [251, 55, 21, 21, 251, 68, 46, 33, 250, 19, 66, 255]
}

fn png() -> Vec<u8> {
    let mut result = Vec::new();
    PngEncoder::new(&mut result)
        .write_image(&pixels(), 2, 2, ExtendedColorType::Rgb8)
        .unwrap();
    result
}

fn alpha_wide_png() -> Vec<u8> {
    let pixels = [251_u8, 55, 21, 96].repeat(640 * 320);
    let mut result = Vec::new();
    PngEncoder::new(&mut result)
        .write_image(&pixels, 640, 320, ExtendedColorType::Rgba8)
        .unwrap();
    result
}

fn apng() -> Vec<u8> {
    let mut result = Vec::new();
    let mut encoder = png::Encoder::new(&mut result, 2, 2);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[255; 16]).unwrap();
    writer.write_image_data(&[0; 16]).unwrap();
    writer.finish().unwrap();
    result
}

fn jpeg() -> Vec<u8> {
    let mut result = Vec::new();
    JpegEncoder::new_with_quality(&mut result, 85)
        .encode(&pixels(), 2, 2, ExtendedColorType::Rgb8)
        .unwrap();
    result
}

fn webp() -> Vec<u8> {
    let mut result = Vec::new();
    let rgba = [
        251, 55, 21, 255, 21, 251, 68, 255, 46, 33, 250, 255, 19, 66, 255, 255,
    ];
    WebPEncoder::new_lossless(&mut result)
        .encode(&rgba, 2, 2, ExtendedColorType::Rgba8)
        .unwrap();
    result
}

async fn attachment(
    state: &AppState,
    project_id: ProjectId,
    bytes: Vec<u8>,
    declared_media_type: &str,
    commit: bool,
) -> Value {
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let hash = hex::encode(Sha256::digest(&bytes));
    let knowledge = state.knowledge_repository();
    let session = knowledge
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "synthetic.test".into(),
                declared_media_type: declared_media_type.into(),
                expected_size: bytes.len() as u64,
                expected_sha256: hash.clone(),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    knowledge
        .put_upload_content(&scope, session.upload_session_id, bytes)
        .await
        .unwrap();
    if commit {
        let (object, _) = knowledge
            .complete_attachment_upload(&scope, session.upload_session_id, "synthetic-complete")
            .await
            .unwrap();
        json!({
            "object_id":object.object_id,
            "object_version":object.object_version,
            "sha256":object.sha256
        })
    } else {
        // A staged object has no committed identity. Guessing its id must not
        // grant content use, even if its bytes have been written.
        json!({
            "object_id":session.upload_session_id,
            "object_version":1,
            "sha256":hash
        })
    }
}

#[tokio::test]
async fn parser_profile_development_shares_attachment_bytes_with_media_snapshots() {
    for (pdf, office) in [
        (None, None),
        (Some("synthetic-pdf".to_owned()), None),
        (None, Some("synthetic-office".to_owned())),
    ] {
        let state = AppState::development_with_parser_profiles("test-password", pdf, office);
        let project_id = project(&state).await;
        let bytes = png();
        let key = attachment(&state, project_id, bytes.clone(), "image/png", true).await;
        let app = router(state.clone());
        let (cookie, csrf) = login(&app, "demo@localhost", "test-password").await;
        let (status, _, result) = call(
            &app,
            "POST",
            &base(project_id),
            Some(&cookie),
            Some(&csrf),
            key.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{}", body(&result));
        let scope = TenantScope::new(
            DEVELOPMENT_OPERATOR_ID,
            DEVELOPMENT_TENANT_ID,
            Some(project_id),
        );
        let key: geo_domain::MediaObjectKey = serde_json::from_value(key).unwrap();
        let snapshots = state
            .content_media_repository()
            .snapshot_authorized_images(&scope, std::slice::from_ref(&key))
            .await
            .unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].image.key, key);
        assert_eq!(snapshots[0].bytes, bytes);
    }
}

#[tokio::test]
async fn image_bindings_validate_bytes_and_are_revocable() {
    let state = AppState::development_with_password("test-password");
    let project_id = project(&state).await;
    let other_project = project(&state).await;
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "test-password").await;
    let first = png();
    let key = attachment(&state, project_id, first.clone(), "text/html", true).await;
    let (status, _, bytes) = call(
        &app,
        "POST",
        &base(project_id),
        Some(&cookie),
        None,
        key.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{}", body(&bytes));
    let (status, _, bytes) = call(
        &app,
        "POST",
        &base(project_id),
        Some(&cookie),
        Some(&csrf),
        key.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body(&bytes));
    let binding = body(&bytes);
    assert_eq!(binding["image"]["media_type"], "image/png");
    assert_eq!(binding["image"]["width"], 2);
    assert_eq!(binding["image"]["height"], 2);
    assert_eq!(binding["image"]["byte_len"], first.len());
    assert!(binding.get("opaque_key").is_none());
    let id = binding["binding_id"].as_str().unwrap();
    let (status, _, bytes) = call(
        &app,
        "POST",
        &base(project_id),
        Some(&cookie),
        Some(&csrf),
        key.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body(&bytes));
    assert_eq!(body(&bytes), binding, "same object must be idempotent");
    let (status, _, bytes) = call(
        &app,
        "GET",
        &base(project_id),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body(&bytes)["items"][0], binding);
    let (status, headers, bytes) = call(
        &app,
        "GET",
        &detail(project_id, id, true),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    // The global middleware replaces the handler's private/no-store with the
    // still stricter effective no-store cache policy.
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(bytes, first);

    for uri in [base(other_project), base(project_id)] {
        let other_key = if uri == base(project_id) {
            json!({"object_id":Uuid::new_v4(),"object_version":1,"sha256":"0".repeat(64)})
        } else {
            key.clone()
        };
        let (status, _, _) = call(&app, "POST", &uri, Some(&cookie), Some(&csrf), other_key).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (status, _, _) = call(
        &app,
        "GET",
        &detail(other_project, id, true),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, bytes) = call(
        &app,
        "DELETE",
        &detail(project_id, id, false),
        Some(&cookie),
        Some(&csrf),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let withdrawn = body(&bytes);
    assert_eq!(withdrawn["state"], "withdrawn");
    let (status, _, bytes) = call(
        &app,
        "DELETE",
        &detail(project_id, id, false),
        Some(&cookie),
        Some(&csrf),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body(&bytes), withdrawn);
    let (status, _, _) = call(
        &app,
        "GET",
        &detail(project_id, id, true),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, bytes) = call(
        &app,
        "POST",
        &base(project_id),
        Some(&cookie),
        Some(&csrf),
        key,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{}", body(&bytes));
    let (status, _, bytes) = call(
        &app,
        "GET",
        &base(project_id),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body(&bytes)["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn thumbnail_reads_are_scoped_and_preserve_original_media_bytes() {
    let state = AppState::development_with_password("test-password");
    let project_id = project(&state).await;
    let other_project = project(&state).await;
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "test-password").await;
    let cases = [
        (alpha_wide_png(), "image/png", (320, 160)),
        (jpeg(), "image/jpeg", (2, 2)),
        (webp(), "image/webp", (2, 2)),
    ];
    let mut first_binding_id = None;

    for (source, media_type, dimensions) in cases {
        let key = attachment(&state, project_id, source.clone(), "text/plain", true).await;
        let (status, _, bytes) = call(
            &app,
            "POST",
            &base(project_id),
            Some(&cookie),
            Some(&csrf),
            key,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{}", body(&bytes));
        let binding_id = body(&bytes)["binding_id"].as_str().unwrap().to_owned();
        first_binding_id.get_or_insert_with(|| binding_id.clone());

        let (status, headers, preview) = call(
            &app,
            "GET",
            &thumbnail(project_id, &binding_id),
            Some(&cookie),
            None,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "image/png");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            image::guess_format(&preview).unwrap(),
            image::ImageFormat::Png
        );
        let preview = image::load_from_memory(&preview).unwrap().into_rgba8();
        assert_eq!(preview.dimensions(), dimensions);
        if media_type == "image/png" {
            assert_eq!(preview.get_pixel(0, 0).0[3], 96);
        }

        let (status, headers, original) = call(
            &app,
            "GET",
            &detail(project_id, &binding_id, true),
            Some(&cookie),
            None,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], media_type);
        assert_eq!(original, source);
    }

    let binding_id = first_binding_id.unwrap();
    let (status, _, _) = call(
        &app,
        "GET",
        &thumbnail(project_id, &binding_id),
        None,
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(
        &app,
        "GET",
        &thumbnail(other_project, &binding_id),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let foreign_tenant = Uuid::new_v4();
    let foreign_tenant_uri = format!(
        "/api/v1/projects/{project_id}/content-media/bindings/{binding_id}/thumbnail?tenant_id={foreign_tenant}"
    );
    let (status, _, _) = call(
        &app,
        "GET",
        &foreign_tenant_uri,
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, bytes) = call(
        &app,
        "DELETE",
        &detail(project_id, &binding_id, false),
        Some(&cookie),
        Some(&csrf),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "GET",
        &thumbnail(project_id, &binding_id),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{}", body(&bytes));
}

#[tokio::test]
async fn staged_and_invalid_images_cannot_be_bound() {
    let state = AppState::development_with_password("test-password");
    let project_id = project(&state).await;
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "test-password").await;
    let staged = attachment(&state, project_id, png(), "image/png", false).await;
    let (status, _, _) = call(
        &app,
        "POST",
        &base(project_id),
        Some(&cookie),
        Some(&csrf),
        staged,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut broken_png = png();
    broken_png.truncate(broken_png.len() - 12);
    let mut broken_jpeg = jpeg();
    broken_jpeg.truncate(broken_jpeg.len() - 2);
    let mut forged_jpeg = jpeg();
    forged_jpeg.truncate(forged_jpeg.len() / 2);
    forged_jpeg.extend_from_slice(&[0xff, 0xd9]);
    let mut broken_webp = webp();
    broken_webp.truncate(broken_webp.len() - 1);
    for (case, invalid) in [
        ("svg", b"<svg xmlns='http://www.w3.org/2000'/>".to_vec()),
        ("html", b"<html>not an image</html>".to_vec()),
        ("truncated png", broken_png),
        ("truncated jpeg", broken_jpeg),
        ("forged jpeg", forged_jpeg),
        ("truncated webp", broken_webp),
        ("animated png", apng()),
    ] {
        let key = attachment(&state, project_id, invalid, "image/png", true).await;
        let (status, _, bytes) = call(
            &app,
            "POST",
            &base(project_id),
            Some(&cookie),
            Some(&csrf),
            key,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {}", body(&bytes));
    }
    for (format, encoded) in [("image/jpeg", jpeg()), ("image/webp", webp())] {
        let key = attachment(&state, project_id, encoded, "text/plain", true).await;
        let (status, _, bytes) = call(
            &app,
            "POST",
            &base(project_id),
            Some(&cookie),
            Some(&csrf),
            key,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{}", body(&bytes));
        assert_eq!(body(&bytes)["image"]["media_type"], format);
    }
}

#[tokio::test]
async fn pagination_and_object_identity_are_scoped() {
    let state = AppState::development_with_password("test-password");
    let project_id = project(&state).await;
    let app = router(state.clone());
    let (cookie, csrf) = login(&app, "demo@localhost", "test-password").await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let key = attachment(&state, project_id, png(), "image/png", true).await;
        let (status, _, bytes) = call(
            &app,
            "POST",
            &base(project_id),
            Some(&cookie),
            Some(&csrf),
            key.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{}", body(&bytes));
        ids.push(body(&bytes)["binding_id"].as_str().unwrap().to_owned());
        let mut invalid_version = key;
        invalid_version["object_version"] = json!(2);
        let (status, _, _) = call(
            &app,
            "POST",
            &base(project_id),
            Some(&cookie),
            Some(&csrf),
            invalid_version,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    ids.sort();
    let page_one = format!("{}&limit=2", base(project_id));
    let (status, _, bytes) = call(&app, "GET", &page_one, Some(&cookie), None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let first = body(&bytes);
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["items"][0]["binding_id"], ids[0]);
    assert_eq!(first["next_cursor"], ids[1]);
    let page_two = format!("{}&limit=2&after={}", base(project_id), ids[1]);
    let (status, _, bytes) = call(&app, "GET", &page_two, Some(&cookie), None, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let second = body(&bytes);
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["items"][0]["binding_id"], ids[2]);
    assert!(second["next_cursor"].is_null());
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("{}&limit=101", base(project_id)),
        Some(&cookie),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn project_reader_can_preview_but_cannot_bind_or_withdraw() {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "owner-password",
    ));
    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "synthetic-viewer@localhost",
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
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        Arc::new(MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    );
    let project_id = project(&state).await;
    let key = attachment(&state, project_id, png(), "image/jpeg", true).await;
    let app = router(state);
    let (writer, writer_csrf) = login(&app, "demo@localhost", "owner-password").await;
    let (reader, reader_csrf) = login(&app, "synthetic-viewer@localhost", "viewer-password").await;
    let (status, _, bytes) = call(
        &app,
        "POST",
        &base(project_id),
        Some(&writer),
        Some(&writer_csrf),
        key.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body(&bytes));
    let id = body(&bytes)["binding_id"].as_str().unwrap().to_owned();
    let (status, _, _) = call(
        &app,
        "GET",
        &base(project_id),
        Some(&reader),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "GET",
        &detail(project_id, &id, true),
        Some(&reader),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "POST",
        &base(project_id),
        Some(&reader),
        Some(&reader_csrf),
        key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(
        &app,
        "DELETE",
        &detail(project_id, &id, false),
        Some(&reader),
        Some(&reader_csrf),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(&app, "GET", &base(project_id), None, None, json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
