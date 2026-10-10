use geo_domain::{
    ChunkLocator, DocxElement, DocxTableCell, DocxTableRow, DocxUnit, ImportItem, ImportStatus,
    KnowledgePurpose, KnowledgeRepository, OFFICE_PARSE_SCHEMA_VERSION, OfficeDocumentManifest,
    OfficeDocumentStructure, OfficeUnitResult, SourceKind, TenantScope, UploadSessionCommand,
    XlsxCell, XlsxCellKind, XlsxRow, XlsxUnit, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgKnowledgeRepository};
use sqlx::PgPool;
use uuid::Uuid;

const PROFILE: &str = "poi-5.4.1_ooxml-struct-v1";
const DOCX: &[u8] = b"synthetic-office-fixture-not-a-real-document";
const DOCX_MEDIA: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX_MEDIA: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

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
    sqlx::query(
        "INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Office fixture')",
    )
    .bind(operator)
    .bind(format!("office-{operator}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO tenants (operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,'Office fixture')")
        .bind(operator).bind(tenant).bind(format!("office-{tenant}"))
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO projects (operator_id,tenant_id,project_id,slug,display_name,status) VALUES ($1,$2,$3,$4,'Office fixture','active')")
        .bind(operator).bind(tenant).bind(project).bind(format!("office-{project}"))
        .execute(pool).await.unwrap();
    TenantScope::new(operator.into(), tenant.into(), Some(project.into()))
}

async fn upload(repo: &PgKnowledgeRepository, scope: &TenantScope, attachment: bool) -> Uuid {
    let session = repo
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "fixture.docx".to_owned(),
                declared_media_type: DOCX_MEDIA.to_owned(),
                expected_size: DOCX.len() as u64,
                expected_sha256: sha256_hex(DOCX),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .unwrap();
    repo.put_upload_content(scope, session.upload_session_id, DOCX.to_vec())
        .await
        .unwrap();
    if attachment {
        repo.complete_attachment_upload(scope, session.upload_session_id, "office-attachment")
            .await
            .unwrap()
            .0
            .object_id
    } else {
        session.upload_session_id
    }
}

fn manifest() -> OfficeDocumentManifest {
    OfficeDocumentManifest {
        schema_version: OFFICE_PARSE_SCHEMA_VERSION.to_owned(),
        input_sha256: sha256_hex(DOCX),
        parser_version: PROFILE.to_owned(),
        media_type: DOCX_MEDIA.to_owned(),
        document: OfficeDocumentStructure::Docx {
            units: vec![
                DocxUnit {
                    unit_id: 0,
                    start_body_element: 0,
                    end_body_element: 0,
                },
                DocxUnit {
                    unit_id: 1,
                    start_body_element: 1,
                    end_body_element: 1,
                },
            ],
        },
    }
}

fn paragraph(unit_id: u32, text: &str) -> OfficeUnitResult {
    OfficeUnitResult::DocxSuccess {
        unit_id,
        elements: vec![DocxElement::Paragraph {
            body_element_index: unit_id,
            paragraph_index: unit_id,
            heading_path: vec!["General".to_owned()],
            text: text.to_owned(),
        }],
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn office_migration_queues_scoped_document_and_fences_partial_retry() {
    let db = database().await;
    let owner = scope(db.pool()).await;
    let foreign = scope(db.pool()).await;
    let repo =
        PgKnowledgeRepository::from_database(&db).with_office_parser_profile(PROFILE.to_owned());
    let caps = repo.capabilities(&owner).await.unwrap();
    assert!(caps.docx_parser && caps.xlsx_parser);
    let upload_id = upload(&repo, &owner, false).await;
    let receipt = repo
        .complete_upload(&owner, upload_id, "upload-office")
        .await
        .unwrap();
    assert_eq!(receipt.status, ImportStatus::Queued);
    assert_eq!(
        repo.complete_upload(&owner, upload_id, "upload-office")
            .await
            .unwrap(),
        receipt
    );
    let job_id = receipt.import_job.as_ref().unwrap().import_job_id;
    assert_eq!(
        repo.office_parse_candidates(None, 200)
            .await
            .unwrap()
            .iter()
            .find(|candidate| candidate.job_id == job_id)
            .unwrap()
            .scope,
        owner
    );
    assert!(
        repo.claim_office_parse(&foreign, job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    let lease = repo
        .claim_office_parse(&owner, job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(repo.office_parse_input(&foreign, &lease).await.is_err());
    let input = repo.office_parse_input(&owner, &lease).await.unwrap();
    assert_eq!(input.bytes, DOCX);
    assert_eq!(input.input_sha256, sha256_hex(DOCX));
    assert!(input.manifest.is_none());
    repo.record_office_manifest(&owner, &lease, manifest())
        .await
        .unwrap();
    assert!(
        sqlx::query(
            "UPDATE knowledge_office_parse_tasks SET manifest='{}'::jsonb
         WHERE import_job_id=$1"
        )
        .bind(job_id)
        .execute(db.pool())
        .await
        .is_err()
    );
    repo.record_office_unit(
        &owner,
        &lease,
        paragraph(0, "FIRST_MARKER original paragraph"),
    )
    .await
    .unwrap();
    repo.record_office_unit(
        &owner,
        &lease,
        OfficeUnitResult::Failure {
            unit_id: 1,
            code: "parse_failed".to_owned(),
        },
    )
    .await
    .unwrap();
    assert!(
        repo.record_office_unit(&owner, &lease, paragraph(0, "tampered"))
            .await
            .is_err()
    );
    sqlx::query("UPDATE knowledge_import_jobs SET lease_until=clock_timestamp()-interval '1 second' WHERE import_job_id=$1")
        .bind(job_id).execute(db.pool()).await.unwrap();
    assert!(repo.finish_office_parse(&owner, &lease).await.is_err());
    let reclaimed = repo
        .claim_office_parse(&owner, job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(reclaimed.fencing_token > lease.fencing_token);
    assert!(
        repo.record_office_unit(&owner, &lease, paragraph(1, "stale"))
            .await
            .is_err()
    );
    let partial = repo.finish_office_parse(&owner, &reclaimed).await.unwrap();
    assert_eq!(partial.status, ImportStatus::Partial);
    assert_eq!(partial.release.as_ref().unwrap().coverage.chunk_count, 1);
    assert!(
        partial
            .release
            .as_ref()
            .unwrap()
            .coverage
            .blocked_reasons
            .iter()
            .any(|reason| reason == "office_failed_units:1")
    );
    let progress = repo
        .get_import_progress(&owner, job_id, KnowledgePurpose::Public)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.status, ImportStatus::Partial);
    assert_eq!(
        progress.knowledge_release_id,
        partial.release.as_ref().map(|r| r.knowledge_release_id)
    );
    assert_eq!(progress.error_count, 1);
    let original_release = partial.release.unwrap();
    assert!(
        sqlx::query(
            "UPDATE knowledge_office_parse_units SET status='pending',result=NULL,
         result_sha256=NULL WHERE import_job_id=$1 AND ordinal=0"
        )
        .bind(job_id)
        .execute(db.pool())
        .await
        .is_err()
    );
    let restarted =
        PgKnowledgeRepository::from_database(&db).with_office_parser_profile(PROFILE.to_owned());
    assert!(
        restarted
            .retry_office_parse(&foreign, job_id)
            .await
            .is_err()
    );
    let retry = restarted.retry_office_parse(&owner, job_id).await.unwrap();
    assert_eq!(retry.completed_units, 1);
    assert_eq!(retry.resumed_from, Some(job_id));
    assert_eq!(
        restarted.retry_office_parse(&owner, job_id).await.unwrap(),
        retry
    );
    let next_lease = restarted
        .claim_office_parse(&owner, retry.import_job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let next_input = restarted
        .office_parse_input(&owner, &next_lease)
        .await
        .unwrap();
    assert_eq!(next_input.successful_units, vec![0]);
    assert_eq!(next_input.manifest, Some(manifest()));
    assert!(
        restarted
            .record_office_unit(&owner, &next_lease, paragraph(0, "overwritten"))
            .await
            .is_err()
    );
    restarted
        .record_office_unit(
            &owner,
            &next_lease,
            OfficeUnitResult::DocxSuccess {
                unit_id: 1,
                elements: vec![DocxElement::Table {
                    body_element_index: 1,
                    table_index: 0,
                    heading_path: vec!["General".to_owned()],
                    rows: vec![DocxTableRow {
                        row_index: 0,
                        cells: vec![DocxTableCell {
                            column_index: 0,
                            text: "SECOND_MARKER recovered table".to_owned(),
                            row_span: 1,
                            col_span: 1,
                            merged: false,
                        }],
                    }],
                }],
            },
        )
        .await
        .unwrap();
    let ready = restarted
        .finish_office_parse(&owner, &next_lease)
        .await
        .unwrap();
    assert_eq!(ready.status, ImportStatus::Succeeded);
    assert_eq!(ready.release.as_ref().unwrap().coverage.chunk_count, 2);
    assert_eq!(
        ready.source_version.as_ref().unwrap().content_sha256,
        sha256_hex(DOCX)
    );
    assert_ne!(
        ready.source_version.as_ref().unwrap().source_version_id,
        partial.source_version.as_ref().unwrap().source_version_id
    );
    let old = restarted
        .get_release(&owner, original_release.knowledge_release_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.coverage.chunk_count, 1);
    let detail = restarted
        .get_source_detail(&owner, partial.source.unwrap().source_id)
        .await
        .unwrap()
        .unwrap();
    assert!(detail.chunks.iter().any(|chunk| matches!(
        &chunk.locator,
        ChunkLocator::Docx {
            table_row: Some(0),
            table_column: Some(0),
            ..
        }
    )));
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn office_object_import_and_zero_success_do_not_create_release() {
    let db = database().await;
    let owner = scope(db.pool()).await;
    let repo =
        PgKnowledgeRepository::from_database(&db).with_office_parser_profile(PROFILE.to_owned());
    let object = upload(&repo, &owner, true).await;
    let item = ImportItem {
        client_item_id: "office-existing-object".to_owned(),
        kind: SourceKind::Object,
        name: "fixture.docx".to_owned(),
        purpose: KnowledgePurpose::Public,
        text: None,
        url: None,
        object_id: Some(object),
        knowledge_release_id: None,
    };
    let accepted = repo
        .import_batch(&owner, vec![item.clone()])
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(accepted.status, ImportStatus::Queued);
    assert_eq!(
        repo.import_batch(&owner, vec![item])
            .await
            .unwrap()
            .items
            .remove(0),
        accepted
    );
    let job_id = accepted.import_job.as_ref().unwrap().import_job_id;
    let lease = repo
        .claim_office_parse(&owner, job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    repo.record_office_manifest(&owner, &lease, manifest())
        .await
        .unwrap();
    for unit_id in 0..2 {
        repo.record_office_unit(
            &owner,
            &lease,
            OfficeUnitResult::Failure {
                unit_id,
                code: "parse_failed".to_owned(),
            },
        )
        .await
        .unwrap();
    }
    let failed = repo.finish_office_parse(&owner, &lease).await.unwrap();
    assert_eq!(failed.status, ImportStatus::Failed);
    assert!(failed.release.is_none());
    let progress = repo
        .get_import_progress(&owner, job_id, KnowledgePurpose::Public)
        .await
        .unwrap()
        .unwrap();
    assert!(progress.knowledge_release_id.is_none());
    assert!(
        repo.office_parse_operation(&owner, job_id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn office_xlsx_sheet_and_exact_cell_evidence_survive_release_restart() {
    let db = database().await;
    let owner = scope(db.pool()).await;
    let repo =
        PgKnowledgeRepository::from_database(&db).with_office_parser_profile(PROFILE.to_owned());
    let session = repo
        .create_upload_session(
            &owner,
            UploadSessionCommand {
                filename: "table.xlsx".to_owned(),
                declared_media_type: XLSX_MEDIA.to_owned(),
                expected_size: DOCX.len() as u64,
                expected_sha256: sha256_hex(DOCX),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    repo.put_upload_content(&owner, session.upload_session_id, DOCX.to_vec())
        .await
        .unwrap();
    let accepted = repo
        .complete_upload(&owner, session.upload_session_id, "xlsx-upload")
        .await
        .unwrap();
    let job_id = accepted.import_job.as_ref().unwrap().import_job_id;
    let lease = repo
        .claim_office_parse(&owner, job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    repo.record_office_manifest(
        &owner,
        &lease,
        OfficeDocumentManifest {
            schema_version: OFFICE_PARSE_SCHEMA_VERSION.to_owned(),
            input_sha256: sha256_hex(DOCX),
            parser_version: PROFILE.to_owned(),
            media_type: XLSX_MEDIA.to_owned(),
            document: OfficeDocumentStructure::Xlsx {
                units: vec![XlsxUnit {
                    unit_id: 0,
                    sheet_name: "Prices".to_owned(),
                    sheet_index: 0,
                    start_row: 3,
                    end_row: 3,
                }],
            },
        },
    )
    .await
    .unwrap();
    repo.record_office_unit(
        &owner,
        &lease,
        OfficeUnitResult::XlsxSuccess {
            unit_id: 0,
            rows: vec![XlsxRow {
                row: 3,
                header_range: Some("A1:B1".to_owned()),
                cells: vec![XlsxCell {
                    reference: "B3".to_owned(),
                    column: 2,
                    kind: XlsxCellKind::Number,
                    value: "123.45".to_owned(),
                    display_value: Some("$123.45".to_owned()),
                    formula: None,
                    cached_kind: None,
                    cached_value: None,
                    merged_range: None,
                }],
            }],
        },
    )
    .await
    .unwrap();
    let ready = repo.finish_office_parse(&owner, &lease).await.unwrap();
    assert_eq!(ready.status, ImportStatus::Succeeded);
    let restarted =
        PgKnowledgeRepository::from_database(&db).with_office_parser_profile(PROFILE.to_owned());
    let detail = restarted
        .get_source_detail(&owner, accepted.source.unwrap().source_id)
        .await
        .unwrap()
        .unwrap();
    let chunk = detail.chunks.iter().find(|c| c.text == "123.45").unwrap();
    assert!(
        matches!(&chunk.locator,ChunkLocator::Xlsx {sheet,range,header_range,..}
        if sheet=="Prices" && range=="B3" && header_range.as_deref()==Some("A1:B1"))
    );
    assert_eq!(ready.release.unwrap().coverage.chunk_count, 1);
    assert!(restarted.office_parse_input(&owner, &lease).await.is_err());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn office_publication_rechecks_original_bytes_and_rejects_late_terminal_failure() {
    let db = database().await;
    let owner = scope(db.pool()).await;
    let repo =
        PgKnowledgeRepository::from_database(&db).with_office_parser_profile(PROFILE.to_owned());
    let session_id = upload(&repo, &owner, false).await;
    let accepted = repo
        .complete_upload(&owner, session_id, "office-recheck")
        .await
        .unwrap();
    let job_id = accepted.import_job.unwrap().import_job_id;
    let lease = repo
        .claim_office_parse(&owner, job_id, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.office_parse_input(&owner, &lease).await.unwrap().bytes,
        DOCX
    );
    repo.record_office_manifest(&owner, &lease, manifest())
        .await
        .unwrap();
    assert!(
        repo.fail_office_parse(&owner, &lease, "invalid_docx")
            .await
            .is_err()
    );
    repo.record_office_unit(&owner, &lease, paragraph(0, "stored paragraph"))
        .await
        .unwrap();
    repo.record_office_unit(
        &owner,
        &lease,
        OfficeUnitResult::Failure {
            unit_id: 1,
            code: "empty_text".to_owned(),
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE knowledge_upload_blobs SET content=$2 WHERE upload_session_id=$1")
        .bind(session_id)
        .bind(b"modified".as_slice())
        .execute(db.pool())
        .await
        .unwrap();
    assert!(repo.finish_office_parse(&owner, &lease).await.is_err());
    let state = repo
        .get_source_detail(&owner, accepted.source.unwrap().source_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        state
            .import_jobs
            .iter()
            .all(|job| job.status != ImportStatus::Partial)
    );
}
