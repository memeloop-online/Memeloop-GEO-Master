use geo_domain::{
    ErrorCode, ImportItem, ImportStatus, KnowledgePurpose, KnowledgeRepository,
    PDF_PARSE_SCHEMA_VERSION, PdfDocumentManifest, PdfPageResult, SourceKind, TenantScope,
    UploadSessionCommand, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgKnowledgeRepository};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const PROFILE: &str = "test-pdf-parser-v1";
const PDF: &[u8] = b"%PDF-1.4\n%%EOF\n";

async fn database() -> Database {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL required");
    Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap()
}

async fn scope(pool: &PgPool) -> TenantScope {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Import test')",
    )
    .bind(operator)
    .bind(format!("import-{operator}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tenants (operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,'Import test')",
    )
    .bind(operator)
    .bind(tenant)
    .bind(format!("import-{tenant}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO projects (operator_id,tenant_id,project_id,slug,display_name,status)
         VALUES ($1,$2,$3,$4,'Import test','active')",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(format!("import-{project}"))
    .execute(pool)
    .await
    .unwrap();
    TenantScope::new(operator.into(), tenant.into(), Some(project.into()))
}

async fn pdf_attachment(repo: &PgKnowledgeRepository, scope: &TenantScope) -> Uuid {
    let upload = repo
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "manual.pdf".into(),
                declared_media_type: "application/pdf".into(),
                expected_size: PDF.len() as u64,
                expected_sha256: sha256_hex(PDF),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .unwrap();
    repo.put_upload_content(scope, upload.upload_session_id, PDF.to_vec())
        .await
        .unwrap();
    repo.complete_attachment_upload(scope, upload.upload_session_id, "complete")
        .await
        .unwrap()
        .0
        .object_id
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn receipt_tracks_original_pdf_job_without_promoting_queued_version_or_retry() {
    let db = database().await;
    let other = scope(db.pool()).await;
    let scope = scope(db.pool()).await;
    let repo = PgKnowledgeRepository::from_database(&db).with_pdf_parser_profile(PROFILE.into());
    let object_id = pdf_attachment(&repo, &scope).await;
    let item = ImportItem {
        client_item_id: Uuid::new_v4().to_string(),
        kind: SourceKind::Object,
        name: "manual.pdf".into(),
        purpose: KnowledgePurpose::Public,
        text: None,
        url: None,
        object_id: Some(object_id),
        knowledge_release_id: None,
    };
    assert!(
        repo.resolve_import_receipt(&scope, &item)
            .await
            .unwrap()
            .is_none()
    );
    let accepted = repo.import_batch(&scope, vec![item.clone()]).await.unwrap();
    let job = accepted.items[0].import_job.as_ref().unwrap().import_job_id;
    let queued = repo
        .resolve_import_receipt(&scope, &item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queued.status, ImportStatus::Queued);
    assert_eq!(queued.import_job_id, Some(job));
    assert!(queued.source_version_id.is_none());
    assert!(queued.knowledge_release_id.is_none());
    assert!(
        repo.get_import_progress(&other, job, KnowledgePurpose::Public)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.resolve_import_receipt(&other, &item)
            .await
            .unwrap()
            .is_none()
    );
    let mut changed = item.clone();
    changed.name.push_str(".changed");
    assert_eq!(
        repo.resolve_import_receipt(&scope, &changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let lease = repo
        .claim_pdf_parse(&scope, job, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let running = repo
        .resolve_import_receipt(&scope, &item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.status, ImportStatus::Running);
    let input = repo.pdf_parse_input(&scope, &lease).await.unwrap();
    repo.record_pdf_manifest(
        &scope,
        &lease,
        PdfDocumentManifest {
            schema_version: PDF_PARSE_SCHEMA_VERSION.into(),
            input_sha256: input.input_sha256,
            parser_version: PROFILE.into(),
            page_count: 2,
        },
    )
    .await
    .unwrap();
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Success {
            page: 1,
            text: "Visible page one".into(),
        },
    )
    .await
    .unwrap();
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Failure {
            page: 2,
            code: "parse_failed".into(),
        },
    )
    .await
    .unwrap();
    let parsing = repo
        .get_import_progress(&scope, job, KnowledgePurpose::Public)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parsing.failed_units, 1);
    assert_eq!(parsing.error_count, 1);
    assert_eq!(parsing.errors[0].code, "parse_failed");
    assert_eq!(parsing.errors[0].page, Some(2));
    let partial = repo.finish_pdf_parse(&scope, &lease).await.unwrap();
    let release = partial.release.unwrap().knowledge_release_id;
    let read = repo
        .resolve_import_receipt(&scope, &item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.status, ImportStatus::Partial);
    assert_eq!(
        read.source_version_id,
        accepted.items[0]
            .source_version
            .as_ref()
            .map(|v| v.source_version_id)
    );
    assert_eq!(read.knowledge_release_id, Some(release));
    let many_errors = (0..105)
        .map(|_| json!({"code":"unknown_parser_detail","message":"not exposed","page":2}))
        .collect::<Vec<_>>();
    sqlx::query("UPDATE knowledge_import_jobs SET errors=$2 WHERE import_job_id=$1")
        .bind(job)
        .bind(json!(many_errors))
        .execute(db.pool())
        .await
        .unwrap();
    let bounded = repo
        .resolve_import_receipt(&scope, &item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bounded.error_count, 105);
    assert_eq!(bounded.errors.len(), 100);
    assert!(
        bounded
            .errors
            .iter()
            .all(|error| error.code == "import_failed")
    );
    let retry = repo.retry_pdf_parse(&scope, job).await.unwrap();
    let original = repo
        .resolve_import_receipt(&scope, &item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.import_job_id, Some(job));
    assert_eq!(original.knowledge_release_id, Some(release));
    let successor = repo
        .get_import_progress(&scope, retry.import_job_id, KnowledgePurpose::Public)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(successor.status, ImportStatus::Queued);
    assert!(successor.knowledge_release_id.is_none());
    assert!(successor.source_version_id.is_none());
    // A published release must contain exactly this source version.
    sqlx::query("DELETE FROM knowledge_release_source_versions WHERE knowledge_release_id=$1 AND source_version_id=$2")
        .bind(release)
        .bind(read.source_version_id.unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo.resolve_import_receipt(&scope, &item)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn text_and_failed_receipts_are_scoped_and_never_expose_revoked_sources() {
    let db = database().await;
    let scope = scope(db.pool()).await;
    let repo = PgKnowledgeRepository::from_database(&db);
    let text = ImportItem {
        client_item_id: Uuid::new_v4().to_string(),
        kind: SourceKind::Text,
        name: "guide".into(),
        purpose: KnowledgePurpose::Public,
        text: Some("A short, usable test paragraph.".into()),
        url: None,
        object_id: None,
        knowledge_release_id: None,
    };
    let accepted = repo.import_batch(&scope, vec![text.clone()]).await.unwrap();
    let source = accepted.items[0].source.as_ref().unwrap().source_id;
    let progress = repo
        .resolve_import_receipt(&scope, &text)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.status, ImportStatus::Succeeded);
    assert_eq!(
        progress.knowledge_release_id,
        accepted.items[0]
            .release
            .as_ref()
            .map(|r| r.knowledge_release_id)
    );
    assert!(
        repo.get_import_progress(
            &scope,
            progress.import_job_id.unwrap(),
            KnowledgePurpose::Internal
        )
        .await
        .unwrap()
        .is_some()
    );
    let operation = accepted.items[0].operation.as_ref().unwrap();
    sqlx::query("UPDATE operations SET result=$2 WHERE operation_id=$1")
        .bind(operation.id)
        .bind(json!({"source_id": Uuid::new_v4()}))
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo.resolve_import_receipt(&scope, &text)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    sqlx::query("UPDATE operations SET result=$2 WHERE operation_id=$1")
        .bind(operation.id)
        .bind(&operation.result)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE knowledge_sources SET purpose='internal' WHERE source_id=$1")
        .bind(source)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        repo.resolve_import_receipt(&scope, &text)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.get_import_progress(
            &scope,
            progress.import_job_id.unwrap(),
            KnowledgePurpose::Public
        )
        .await
        .unwrap()
        .is_none()
    );
    sqlx::query("UPDATE knowledge_sources SET state='removed' WHERE source_id=$1")
        .bind(source)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        repo.resolve_import_receipt(&scope, &text)
            .await
            .unwrap()
            .is_none()
    );
    let mut private = text.clone();
    private.client_item_id = Uuid::new_v4().to_string();
    private.purpose = KnowledgePurpose::Internal;
    let private_result = repo
        .import_batch(&scope, vec![private.clone()])
        .await
        .unwrap();
    let private_source = private_result.items[0].source.as_ref().unwrap().source_id;
    let private_job = private_result.items[0]
        .import_job
        .as_ref()
        .unwrap()
        .import_job_id;
    sqlx::query("UPDATE knowledge_sources SET purpose='public' WHERE source_id=$1")
        .bind(private_source)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        repo.resolve_import_receipt(&scope, &private)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.get_import_progress(&scope, private_job, KnowledgePurpose::Internal)
            .await
            .unwrap()
            .is_some()
    );
    let unsupported = ImportItem {
        client_item_id: Uuid::new_v4().to_string(),
        kind: SourceKind::Url,
        name: "generic source".into(),
        purpose: KnowledgePurpose::Internal,
        text: None,
        url: Some("https://example.test/resource".into()),
        object_id: None,
        knowledge_release_id: None,
    };
    let failure = repo
        .import_batch(&scope, vec![unsupported.clone()])
        .await
        .unwrap();
    assert_eq!(failure.items[0].status, ImportStatus::Failed);
    let progress = repo
        .resolve_import_receipt(&scope, &unsupported)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.status, ImportStatus::Failed);
    assert_eq!(progress.import_job_id, None);
    assert_eq!(progress.source_id, None);
    assert_eq!(progress.error_count, 1);
    assert_eq!(progress.errors[0].code, "capability_missing");
}
