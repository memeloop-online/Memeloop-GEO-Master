use geo_domain::{
    ImportItem, ImportStatus, KnowledgePurpose, KnowledgeRepository, KnowledgeSearchRequest,
    PDF_MAX_PAGE_TEXT_BYTES, PDF_PARSE_SCHEMA_VERSION, PdfDocumentManifest, PdfPageResult,
    SourceKind, TenantScope, UploadSessionCommand, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgKnowledgeRepository};
use sqlx::PgPool;
use uuid::Uuid;

const PROFILE: &str = "tika-3.2.3_pdfbox-3.0.5_text-v1";
const PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\n%%EOF\n";

async fn database() -> Database {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable GEO_TEST_DATABASE_URL");
    Database::connect_and_migrate(&DatabaseConfig::from_url(url).expect("database URL"))
        .await
        .expect("migrate database")
}

async fn scope(pool: &PgPool) -> TenantScope {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'PDF test')")
        .bind(operator)
        .bind(format!("pdf-{operator}"))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,'PDF test')",
    ).bind(operator).bind(tenant).bind(format!("pdf-{tenant}"))
    .execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO projects (operator_id,tenant_id,project_id,slug,display_name,status)
         VALUES ($1,$2,$3,$4,'PDF test','active')",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(format!("pdf-{project}"))
    .execute(pool)
    .await
    .unwrap();
    TenantScope::new(operator.into(), tenant.into(), Some(project.into()))
}

async fn upload(repo: &PgKnowledgeRepository, scope: &TenantScope, attachment: bool) -> Uuid {
    let session = repo
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "fixture.pdf".to_owned(),
                declared_media_type: "application/pdf".to_owned(),
                expected_size: PDF.len() as u64,
                expected_sha256: sha256_hex(PDF),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .unwrap();
    repo.put_upload_content(scope, session.upload_session_id, PDF.to_vec())
        .await
        .unwrap();
    if attachment {
        repo.complete_attachment_upload(scope, session.upload_session_id, "attachment-complete")
            .await
            .unwrap()
            .0
            .object_id
    } else {
        session.upload_session_id
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn pdf_upload_is_fenced_partial_and_retries_only_missing_pages() {
    let db = database().await;
    let other = scope(db.pool()).await;
    let scope = scope(db.pool()).await;
    let repo =
        PgKnowledgeRepository::from_database(&db).with_pdf_parser_profile(PROFILE.to_owned());
    assert!(repo.capabilities(&scope).await.unwrap().pdf_parser);
    let upload_id = upload(&repo, &scope, false).await;
    let accepted = repo
        .complete_upload(&scope, upload_id, "pdf-upload")
        .await
        .unwrap();
    assert_eq!(accepted.status, ImportStatus::Queued);
    assert_eq!(
        repo.complete_upload(&scope, upload_id, "pdf-upload")
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        accepted.source_version.as_ref().unwrap().content_sha256,
        sha256_hex(PDF)
    );
    let job = accepted.import_job.as_ref().unwrap().import_job_id;
    let candidate = repo
        .pdf_parse_candidates(None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.job_id == job)
        .expect("queued candidate");
    assert_eq!(candidate.scope, scope);
    let lease = repo
        .claim_pdf_parse(&scope, job, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(
        repo.claim_pdf_parse(&scope, job, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    assert!(repo.pdf_parse_input(&other, &lease).await.is_err());
    let input = repo.pdf_parse_input(&scope, &lease).await.unwrap();
    assert_eq!(input.bytes, PDF);
    assert_eq!(input.input_sha256, sha256_hex(PDF));
    let manifest = PdfDocumentManifest {
        schema_version: PDF_PARSE_SCHEMA_VERSION.to_owned(),
        input_sha256: input.input_sha256,
        parser_version: PROFILE.to_owned(),
        page_count: 3,
    };
    repo.record_pdf_manifest(&scope, &lease, manifest.clone())
        .await
        .unwrap();
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Success {
            page: 1,
            text: "FIRST_MARKER paragraph on page one".to_owned(),
        },
    )
    .await
    .unwrap();
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Failure {
            page: 2,
            code: "parse_failed".to_owned(),
        },
    )
    .await
    .unwrap();
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Success {
            page: 3,
            text: "THIRD_MARKER paragraph on page three".to_owned(),
        },
    )
    .await
    .unwrap();
    assert!(
        repo.record_pdf_page(
            &scope,
            &lease,
            PdfPageResult::Success {
                page: 1,
                text: "tampered".to_owned(),
            }
        )
        .await
        .is_err()
    );
    let progress = repo
        .get_source_detail(&scope, accepted.source.as_ref().unwrap().source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.import_jobs[0].completed_units, 2);
    assert_eq!(progress.import_jobs[0].failed_units, 1);
    sqlx::query(
        "UPDATE knowledge_import_jobs SET lease_until=clock_timestamp()-interval '1 second'
         WHERE import_job_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4",
    )
    .bind(job)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.unwrap().as_uuid())
    .execute(db.pool())
    .await
    .unwrap();
    assert!(repo.finish_pdf_parse(&scope, &lease).await.is_err());
    let reclaimed = repo
        .claim_pdf_parse(&scope, job, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(reclaimed.fencing_token > lease.fencing_token);
    assert!(
        repo.record_pdf_page(
            &scope,
            &lease,
            PdfPageResult::Success {
                page: 2,
                text: "stale takeover".to_owned(),
            }
        )
        .await
        .is_err()
    );
    let first = repo.finish_pdf_parse(&scope, &reclaimed).await.unwrap();
    assert_eq!(first.status, ImportStatus::Partial);
    assert_eq!(
        first.release.as_ref().unwrap().coverage.failed_source_count,
        1
    );
    assert_eq!(first.release.as_ref().unwrap().coverage.chunk_count, 2);
    assert!(
        repo.record_pdf_page(
            &scope,
            &lease,
            PdfPageResult::Success {
                page: 2,
                text: "stale".to_owned(),
            }
        )
        .await
        .is_err()
    );
    let original_release = first.release.unwrap();
    let restarted =
        PgKnowledgeRepository::from_database(&db).with_pdf_parser_profile(PROFILE.to_owned());
    let retry = restarted.retry_pdf_parse(&scope, job).await.unwrap();
    assert_eq!(retry.resumed_from, Some(job));
    assert_eq!(retry.completed_units, 2);
    assert!(restarted.retry_pdf_parse(&scope, job).await.is_err());
    let lease2 = restarted
        .claim_pdf_parse(&scope, retry.import_job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let retry_input = restarted.pdf_parse_input(&scope, &lease2).await.unwrap();
    assert_eq!(retry_input.successful_pages, vec![1, 3]);
    assert_eq!(retry_input.manifest, Some(manifest));
    assert!(
        restarted
            .record_pdf_page(
                &scope,
                &lease2,
                PdfPageResult::Success {
                    page: 1,
                    text: "changed".to_owned(),
                }
            )
            .await
            .is_err()
    );
    restarted
        .record_pdf_page(
            &scope,
            &lease2,
            PdfPageResult::Success {
                page: 2,
                text: "SECOND_MARKER recovered page".to_owned(),
            },
        )
        .await
        .unwrap();
    let full = restarted.finish_pdf_parse(&scope, &lease2).await.unwrap();
    assert_eq!(full.status, ImportStatus::Succeeded);
    assert_eq!(full.release.as_ref().unwrap().coverage.chunk_count, 3);
    assert_eq!(
        full.release.as_ref().unwrap().coverage.failed_source_count,
        0
    );
    assert_ne!(
        full.source_version.as_ref().unwrap().source_version_id,
        first.source_version.as_ref().unwrap().source_version_id
    );
    assert_eq!(
        full.source_version.as_ref().unwrap().content_sha256,
        sha256_hex(PDF)
    );
    let original = restarted
        .get_release(&scope, original_release.knowledge_release_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.coverage.chunk_count, 2);
    let detail = restarted
        .get_source_detail(&scope, accepted.source.as_ref().unwrap().source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.versions.len(), 2);
    assert_eq!(
        detail
            .chunks
            .iter()
            .filter(|chunk| {
                chunk.source_version_id == full.source_version.as_ref().unwrap().source_version_id
            })
            .count(),
        3
    );
    assert!(
        restarted
            .retry_pdf_parse(&other, retry.import_job_id)
            .await
            .is_err()
    );
    let search = restarted
        .search(
            &scope,
            KnowledgeSearchRequest {
                query: "SECOND_MARKER".to_owned(),
                purpose: KnowledgePurpose::Public,
                knowledge_release_id: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(search.evidence.len(), 1);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn committed_attachment_pdf_stays_unreleased_on_terminal_parse_failure() {
    let db = database().await;
    let scope = scope(db.pool()).await;
    let repo =
        PgKnowledgeRepository::from_database(&db).with_pdf_parser_profile(PROFILE.to_owned());
    let object_id = upload(&repo, &scope, true).await;
    let item = ImportItem {
        client_item_id: "explicit-attachment-import".to_owned(),
        kind: SourceKind::Object,
        name: "fixture.pdf".to_owned(),
        purpose: KnowledgePurpose::Internal,
        text: None,
        url: None,
        object_id: Some(object_id),
        knowledge_release_id: None,
    };
    let accepted = repo
        .import_batch(&scope, vec![item.clone()])
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(accepted.status, ImportStatus::Queued);
    assert_eq!(
        repo.import_batch(&scope, vec![item]).await.unwrap().items[0],
        accepted
    );
    let job = accepted.import_job.unwrap().import_job_id;
    let lease = repo
        .claim_pdf_parse(&scope, job, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let failure = repo
        .fail_pdf_parse(&scope, &lease, "encrypted_pdf")
        .await
        .unwrap();
    assert_eq!(failure.status, ImportStatus::Failed);
    assert!(failure.release.is_none());
    assert!(
        repo.current_release(&scope)
            .await
            .unwrap()
            .knowledge_release_id
            .is_none()
    );
    let next = repo.retry_pdf_parse(&scope, job).await.unwrap();
    let lease = repo
        .claim_pdf_parse(&scope, next.import_job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    repo.record_pdf_manifest(
        &scope,
        &lease,
        PdfDocumentManifest {
            schema_version: PDF_PARSE_SCHEMA_VERSION.to_owned(),
            input_sha256: sha256_hex(PDF),
            parser_version: PROFILE.to_owned(),
            page_count: 1,
        },
    )
    .await
    .unwrap();
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Failure {
            page: 1,
            code: "parse_failed".to_owned(),
        },
    )
    .await
    .unwrap();
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Success {
            page: 1,
            text: "Private internal PDF text".to_owned(),
        },
    )
    .await
    .unwrap();
    let success = repo.finish_pdf_parse(&scope, &lease).await.unwrap();
    assert_eq!(success.status, ImportStatus::Succeeded);
    let search = repo
        .search(
            &scope,
            KnowledgeSearchRequest {
                query: "Private internal PDF text".to_owned(),
                purpose: KnowledgePurpose::Public,
                knowledge_release_id: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert!(search.evidence.is_empty());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn page_limit_replays_after_lease_takeover_without_stuck_job() {
    let db = database().await;
    let scope = scope(db.pool()).await;
    let repo =
        PgKnowledgeRepository::from_database(&db).with_pdf_parser_profile(PROFILE.to_owned());
    let upload_id = upload(&repo, &scope, false).await;
    let accepted = repo
        .complete_upload(&scope, upload_id, "limit-replay")
        .await
        .unwrap();
    let job = accepted.import_job.unwrap().import_job_id;
    let lease = repo
        .claim_pdf_parse(&scope, job, Uuid::new_v4(), 300)
        .await
        .unwrap()
        .unwrap();
    repo.record_pdf_manifest(
        &scope,
        &lease,
        PdfDocumentManifest {
            schema_version: PDF_PARSE_SCHEMA_VERSION.to_owned(),
            input_sha256: sha256_hex(PDF),
            parser_version: PROFILE.to_owned(),
            page_count: 9,
        },
    )
    .await
    .unwrap();
    let full_page = "x".repeat(PDF_MAX_PAGE_TEXT_BYTES);
    for page in 1..=8 {
        repo.record_pdf_page(
            &scope,
            &lease,
            PdfPageResult::Success {
                page,
                text: full_page.clone(),
            },
        )
        .await
        .unwrap();
    }
    repo.record_pdf_page(
        &scope,
        &lease,
        PdfPageResult::Success {
            page: 9,
            text: "over-limit".to_owned(),
        },
    )
    .await
    .unwrap();
    let progress = repo
        .get_source_detail(&scope, accepted.source.as_ref().unwrap().source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.import_jobs[0].completed_units, 8);
    assert_eq!(progress.import_jobs[0].failed_units, 1);
    assert_eq!(progress.import_jobs[0].errors[0]["code"], "page_limit");
    sqlx::query("UPDATE knowledge_import_jobs SET lease_until=clock_timestamp()-interval '1 second' WHERE import_job_id=$1")
        .bind(job).execute(db.pool()).await.unwrap();
    let reclaimed = repo
        .claim_pdf_parse(&scope, job, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    // Recovered parser retries page nine; its immutable page_limit outcome is replayed.
    assert_eq!(
        repo.pdf_parse_input(&scope, &reclaimed)
            .await
            .unwrap()
            .successful_pages,
        (1..=8).collect::<Vec<_>>()
    );
    repo.record_pdf_page(
        &scope,
        &reclaimed,
        PdfPageResult::Success {
            page: 9,
            text: "over-limit".to_owned(),
        },
    )
    .await
    .unwrap();
    let replayed = repo
        .get_source_detail(&scope, accepted.source.as_ref().unwrap().source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replayed.import_jobs[0].completed_units, 8);
    assert_eq!(replayed.import_jobs[0].failed_units, 1);
    assert_eq!(replayed.import_jobs[0].errors[0]["code"], "page_limit");
    assert!(repo.pdf_parse_input(&scope, &lease).await.is_err());
}
