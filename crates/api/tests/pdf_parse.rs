//! Explicit real-service test: CI starts the pinned Java parser and supplies
//! GEO_TEST_PDF_PARSER_URL. No canned parser text can satisfy this assertion.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, PDF_PARSER_PROFILE, PdfParserClient, dispatch_pdf_parse_job};
use geo_api::{CSRF_HEADER, EventBus, MemoryIdempotencyStore, MemoryOperationStore, router};
use geo_domain::{
    ChunkLocator, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ImportStatus, KnowledgePurpose,
    KnowledgeRepository, KnowledgeSearchRequest, Membership, MemoryAuthRepository,
    MemoryKnowledgeRepository, MemoryProjectRepository, ProjectCreate, ProjectRepository,
    ProjectSettings, Role, TenantScope, UploadSessionCommand, User, sha256_hex,
};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

fn two_page_pdf() -> Vec<u8> {
    let first = "BT /F1 12 Tf 72 710 Td (Almond on first page) Tj ET\n";
    let second = "BT /F1 12 Tf 72 710 Td (Walnut on second page) Tj ET\n";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{first}endstream", first.len()),
        format!("<< /Length {} >>\nstream\n{second}endstream", second.len()),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    bytes
}

async fn upload(
    repository: &dyn KnowledgeRepository,
    scope: &TenantScope,
    bytes: &[u8],
) -> Result<geo_domain::ImportAcceptance, geo_domain::AppError> {
    let session = repository
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "sample.pdf".into(),
                declared_media_type: "application/pdf".into(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await?;
    repository
        .put_upload_content(scope, session.upload_session_id, bytes.to_vec())
        .await?;
    repository
        .complete_upload(scope, session.upload_session_id, "pdf-test-upload")
        .await
}

async fn login(app: &Router, login_name: &str, password: &str) -> (String, String) {
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
                    serde_json::json!({"login_name":login_name,"password":password}).to_string(),
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
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_owned())
}

async fn retry_http(
    app: &Router,
    project_id: Uuid,
    job_id: Uuid,
    cookie: &str,
    csrf: Option<&str>,
) -> StatusCode {
    let mut request = Request::builder().method("POST")
        .uri(format!("/api/v1/knowledge/import-jobs/{job_id}/retry?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}"))
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie);
    if let Some(csrf) = csrf {
        request = request.header(CSRF_HEADER, csrf);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn retry_requires_project_writer_csrf_and_original_scope() {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "test-password",
    ));
    let viewer = User::new(
        Uuid::new_v4().into(),
        DEVELOPMENT_OPERATOR_ID,
        "reader@localhost",
        "Reader",
        "reader-password",
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
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let first = projects
        .create(
            &base,
            ProjectCreate {
                slug: Some("pdf-scope-first".into()),
                display_name: "First".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let other = projects
        .create(
            &base,
            ProjectCreate {
                slug: Some("pdf-scope-second".into()),
                display_name: "Second".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(first.id),
    );
    let knowledge: Arc<dyn KnowledgeRepository> = Arc::new(
        MemoryKnowledgeRepository::with_pdf_parser_profile(PDF_PARSER_PROFILE.into()),
    );
    let accepted = upload(knowledge.as_ref(), &scope, &two_page_pdf())
        .await
        .unwrap();
    let job_id = accepted.import_job.unwrap().import_job_id;
    let lease = knowledge
        .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    knowledge
        .fail_pdf_parse(&scope, &lease, "invalid_pdf")
        .await
        .unwrap();
    let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        projects,
        knowledge.clone(),
        EventBus::default(),
        false,
    );
    let app = router(state);
    let (writer_cookie, writer_csrf) = login(&app, "demo@localhost", "test-password").await;
    let (reader_cookie, reader_csrf) = login(&app, "reader@localhost", "reader-password").await;
    assert_eq!(
        retry_http(
            &app,
            first.id.into(),
            job_id,
            &reader_cookie,
            Some(&reader_csrf)
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        retry_http(
            &app,
            other.id.into(),
            job_id,
            &writer_cookie,
            Some(&writer_csrf)
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        retry_http(&app, first.id.into(), job_id, &writer_cookie, None).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        retry_http(
            &app,
            first.id.into(),
            job_id,
            &writer_cookie,
            Some(&writer_csrf)
        )
        .await,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
#[ignore = "requires the pinned Java parser running at GEO_TEST_PDF_PARSER_URL"]
async fn actual_pdf_pages_are_imported_as_searchable_source_evidence() {
    let endpoint = std::env::var("GEO_TEST_PDF_PARSER_URL")
        .expect("set GEO_TEST_PDF_PARSER_URL to the real local Java parser");
    let parser = PdfParserClient::new(&endpoint).expect("trusted parser endpoint");
    parser.check_ready().await.expect("pinned parser health");
    let state = AppState::development_with_pdf_parser_profile(
        "test-password",
        PDF_PARSER_PROFILE.to_owned(),
    );
    let repository = state.knowledge_repository();
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let bytes = two_page_pdf();
    let sha = sha256_hex(&bytes);

    // Default configuration cannot pretend a PDF has been parsed.
    let disabled = AppState::development_with_password("test-password");
    assert!(
        !disabled
            .knowledge_repository()
            .capabilities(&scope)
            .await
            .unwrap()
            .pdf_parser
    );
    let disabled_acceptance = upload(disabled.knowledge_repository().as_ref(), &scope, &bytes)
        .await
        .expect("verified original bytes remain available without parser");
    assert_eq!(disabled_acceptance.status, ImportStatus::Failed);
    assert!(disabled_acceptance.release.is_none());
    assert!(disabled_acceptance.source_version.is_none());
    assert_eq!(
        disabled_acceptance.error.unwrap().code,
        geo_domain::ErrorCode::CapabilityMissing
    );

    let acceptance = upload(repository.as_ref(), &scope, &bytes)
        .await
        .expect("queue verified original bytes");
    assert_eq!(acceptance.status, ImportStatus::Queued);
    assert!(acceptance.release.is_none());
    let source_id = acceptance.source.as_ref().unwrap().source_id;
    let candidates = repository
        .pdf_parse_candidates(None, 10)
        .await
        .expect("queued jobs");
    assert_eq!(candidates.len(), 1);
    dispatch_pdf_parse_job(state, parser, candidates[0].clone())
        .await
        .expect("real parser dispatch");

    let detail = repository
        .get_source_detail(&scope, source_id)
        .await
        .expect("source read")
        .expect("source exists");
    assert_eq!(detail.import_jobs[0].status, ImportStatus::Succeeded);
    assert_eq!(detail.versions[0].content_sha256, sha);
    assert_eq!(detail.versions[0].parser_version, PDF_PARSER_PROFILE);
    assert!(detail.chunks.iter().any(|chunk| {
        matches!(chunk.locator, ChunkLocator::Pdf { page: 2, .. })
            && chunk.text.contains("Walnut on second page")
    }));
    let search = repository
        .search(
            &scope,
            KnowledgeSearchRequest {
                query: "Walnut".into(),
                purpose: KnowledgePurpose::Public,
                limit: 10,
                knowledge_release_id: None,
            },
        )
        .await
        .expect("search generated evidence");
    assert!(search.evidence.iter().any(|evidence| {
        matches!(evidence.locator, ChunkLocator::Pdf { page: 2, .. })
            && evidence.text.contains("Walnut")
    }));
}
