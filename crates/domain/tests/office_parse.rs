use geo_domain::{
    ChunkLocator, DocxElement, DocxUnit, ImportStatus, KnowledgeImportProgress, KnowledgePurpose,
    KnowledgeRepository, KnowledgeSearchRequest, MemoryKnowledgeRepository,
    OFFICE_PARSE_SCHEMA_VERSION, OfficeDocumentManifest, OfficeDocumentStructure, OfficeFormat,
    OfficeParseLease, OfficeUnitResult, SourceState, TenantScope, UploadSessionCommand, XlsxCell,
    XlsxCellKind, XlsxRow, XlsxUnit, sha256_hex,
};
use uuid::Uuid;

const PROFILE: &str = "poi-5.4.1_ooxml-struct-v1";

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

async fn queued(
    repo: &MemoryKnowledgeRepository,
    scope: &TenantScope,
    format: OfficeFormat,
) -> (Uuid, Uuid, Vec<u8>) {
    let bytes = format!("PK\x03\x04 verified synthetic {format:?} original").into_bytes();
    let upload = repo
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "synthetic".to_owned(),
                declared_media_type: format.media_type().to_owned(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(&bytes),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .unwrap();
    repo.put_upload_content(scope, upload.upload_session_id, bytes.clone())
        .await
        .unwrap();
    let first = repo
        .complete_upload(scope, upload.upload_session_id, "original")
        .await
        .unwrap();
    assert_eq!(first.status, ImportStatus::Queued);
    assert!(first.release.is_none());
    assert_eq!(
        first,
        repo.complete_upload(scope, upload.upload_session_id, "original")
            .await
            .unwrap()
    );
    (
        first.import_job.unwrap().import_job_id,
        first.source.unwrap().source_id,
        bytes,
    )
}

fn manifest(format: OfficeFormat, bytes: &[u8]) -> OfficeDocumentManifest {
    OfficeDocumentManifest {
        schema_version: OFFICE_PARSE_SCHEMA_VERSION.to_owned(),
        input_sha256: sha256_hex(bytes),
        parser_version: PROFILE.to_owned(),
        media_type: format.media_type().to_owned(),
        document: match format {
            OfficeFormat::Docx => OfficeDocumentStructure::Docx {
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
            OfficeFormat::Xlsx => OfficeDocumentStructure::Xlsx {
                units: vec![
                    XlsxUnit {
                        unit_id: 0,
                        sheet_name: "数据".to_owned(),
                        sheet_index: 0,
                        start_row: 2,
                        end_row: 2,
                    },
                    XlsxUnit {
                        unit_id: 1,
                        sheet_name: "数据".to_owned(),
                        sheet_index: 0,
                        start_row: 5,
                        end_row: 5,
                    },
                ],
            },
        },
    }
}

fn success(format: OfficeFormat, id: u32) -> OfficeUnitResult {
    match format {
        OfficeFormat::Docx => OfficeUnitResult::DocxSuccess {
            unit_id: id,
            elements: vec![DocxElement::Paragraph {
                body_element_index: id,
                paragraph_index: id,
                heading_path: vec!["产品说明".to_owned()],
                text: format!("可靠性 🦀 证据 {id}"),
            }],
        },
        OfficeFormat::Xlsx => OfficeUnitResult::XlsxSuccess {
            unit_id: id,
            rows: vec![XlsxRow {
                row: if id == 0 { 2 } else { 5 },
                cells: vec![XlsxCell {
                    reference: if id == 0 { "B2" } else { "B5" }.to_owned(),
                    column: 2,
                    kind: XlsxCellKind::String,
                    value: format!("可靠性 🦀 证据 {id}"),
                    display_value: None,
                    formula: None,
                    cached_kind: None,
                    cached_value: None,
                    merged_range: None,
                }],
                header_range: Some("A1:B1".to_owned()),
            }],
        },
    }
}

async fn lease(
    repo: &MemoryKnowledgeRepository,
    scope: &TenantScope,
    job: Uuid,
) -> OfficeParseLease {
    repo.claim_office_parse(scope, job, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap()
}

async fn progress(
    repo: &MemoryKnowledgeRepository,
    scope: &TenantScope,
    id: Uuid,
) -> KnowledgeImportProgress {
    repo.get_import_progress(scope, id, KnowledgePurpose::Public)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn independent_office_formats_are_configured_not_claimed_by_default() {
    let scope = scope();
    let empty = MemoryKnowledgeRepository::default();
    let caps = empty.capabilities(&scope).await.unwrap();
    assert!(!caps.docx_parser && !caps.xlsx_parser);
    let repo =
        MemoryKnowledgeRepository::with_office_parser_profiles(Some(PROFILE.to_owned()), None);
    let caps = repo.capabilities(&scope).await.unwrap();
    assert!(caps.docx_parser && !caps.xlsx_parser);
    assert!(
        caps.supported_media_types
            .iter()
            .any(|media| media == OfficeFormat::Docx.media_type())
    );
    assert!(
        !caps
            .supported_media_types
            .iter()
            .any(|media| media == OfficeFormat::Xlsx.media_type())
    );
}

#[tokio::test]
async fn docx_partial_release_retry_fences_old_worker_and_keeps_unicode_evidence() {
    let repo =
        MemoryKnowledgeRepository::with_office_parser_profiles(Some(PROFILE.to_owned()), None);
    let scope = scope();
    let (job, source, bytes) = queued(&repo, &scope, OfficeFormat::Docx).await;
    let refs = repo.office_parse_candidates(None, 10).await.unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].job_id, job);
    assert!(
        repo.office_parse_candidates(
            Some(geo_domain::OfficeParseCursor {
                created_at: refs[0].created_at,
                job_id: job
            }),
            10,
        )
        .await
        .unwrap()
        .is_empty()
    );
    let foreign = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert!(
        repo.claim_office_parse(&foreign, job, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let held = lease(&repo, &scope, job).await;
    assert!(
        repo.claim_office_parse(&scope, job, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let input = repo.office_parse_input(&scope, &held).await.unwrap();
    assert_eq!(input.bytes, bytes);
    assert!(input.successful_units.is_empty());
    let spec = manifest(OfficeFormat::Docx, &bytes);
    repo.record_office_manifest(&scope, &held, spec.clone())
        .await
        .unwrap();
    assert!(
        repo.record_office_manifest(&scope, &held, manifest(OfficeFormat::Xlsx, &bytes))
            .await
            .is_err()
    );
    repo.record_office_unit(&scope, &held, success(OfficeFormat::Docx, 0))
        .await
        .unwrap();
    repo.record_office_unit(
        &scope,
        &held,
        OfficeUnitResult::Failure {
            unit_id: 1,
            code: "parse_failed".to_owned(),
        },
    )
    .await
    .unwrap();
    assert!(
        repo.record_office_unit(&scope, &held, success(OfficeFormat::Docx, 0))
            .await
            .is_ok()
    );
    assert!(
        repo.record_office_unit(
            &scope,
            &held,
            OfficeUnitResult::Failure {
                unit_id: 0,
                code: "parse_failed".to_owned()
            }
        )
        .await
        .is_err()
    );
    let first = repo.finish_office_parse(&scope, &held).await.unwrap();
    assert_eq!(first.status, ImportStatus::Partial);
    let first_version = first.source_version.unwrap();
    assert_eq!(first_version.content_sha256, sha256_hex(&bytes));
    assert_eq!(progress(&repo, &scope, job).await.completed_units, 1);
    let retry = repo.retry_office_parse(&scope, job).await.unwrap();
    assert_eq!(retry.resumed_from, Some(job));
    assert_eq!(
        repo.retry_office_parse(&scope, job)
            .await
            .unwrap()
            .import_job_id,
        retry.import_job_id
    );
    let next_lease = lease(&repo, &scope, retry.import_job_id).await;
    assert_eq!(
        repo.office_parse_input(&scope, &next_lease)
            .await
            .unwrap()
            .successful_units,
        vec![0]
    );
    assert!(
        repo.record_office_unit(&scope, &held, success(OfficeFormat::Docx, 1))
            .await
            .is_err()
    );
    repo.record_office_manifest(&scope, &next_lease, spec)
        .await
        .unwrap();
    repo.record_office_unit(&scope, &next_lease, success(OfficeFormat::Docx, 1))
        .await
        .unwrap();
    let second = repo.finish_office_parse(&scope, &next_lease).await.unwrap();
    assert_eq!(second.status, ImportStatus::Succeeded);
    let latest = second.source_version.unwrap();
    assert_eq!(
        latest.parent_version_id,
        Some(first_version.source_version_id)
    );
    assert_eq!(latest.content_sha256, first_version.content_sha256);
    assert_eq!(
        progress(&repo, &scope, job).await.source_version_id,
        Some(first_version.source_version_id)
    );
    let found = repo
        .search(
            &scope,
            KnowledgeSearchRequest {
                query: "可靠性 🦀".to_owned(),
                purpose: KnowledgePurpose::Public,
                knowledge_release_id: second.release.map(|release| release.knowledge_release_id),
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(found.evidence.len(), 2);
    assert!(found.evidence.iter().all(|evidence| matches!(&evidence.locator,ChunkLocator::Docx {heading_path,..} if heading_path == &vec!["产品说明".to_owned()])));
    let source_detail = repo
        .get_source_detail(&scope, source)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source_detail.source.state, SourceState::Active);
}

#[tokio::test]
async fn xlsx_zero_success_never_releases_and_retry_preserves_exact_coordinate() {
    let repo =
        MemoryKnowledgeRepository::with_office_parser_profiles(None, Some(PROFILE.to_owned()));
    let scope = scope();
    let (job, _source, bytes) = queued(&repo, &scope, OfficeFormat::Xlsx).await;
    let held = lease(&repo, &scope, job).await;
    repo.record_office_manifest(&scope, &held, manifest(OfficeFormat::Xlsx, &bytes))
        .await
        .unwrap();
    for id in 0..2 {
        repo.record_office_unit(
            &scope,
            &held,
            OfficeUnitResult::Failure {
                unit_id: id,
                code: "parse_failed".to_owned(),
            },
        )
        .await
        .unwrap();
    }
    let first = repo.finish_office_parse(&scope, &held).await.unwrap();
    assert_eq!(first.status, ImportStatus::Failed);
    assert!(first.source_version.is_none() && first.release.is_none());
    let retry = repo.retry_office_parse(&scope, job).await.unwrap();
    let fresh = lease(&repo, &scope, retry.import_job_id).await;
    assert!(
        repo.office_parse_input(&scope, &fresh)
            .await
            .unwrap()
            .successful_units
            .is_empty()
    );
    repo.record_office_manifest(&scope, &fresh, manifest(OfficeFormat::Xlsx, &bytes))
        .await
        .unwrap();
    repo.record_office_unit(&scope, &fresh, success(OfficeFormat::Xlsx, 0))
        .await
        .unwrap();
    repo.record_office_unit(
        &scope,
        &fresh,
        OfficeUnitResult::Failure {
            unit_id: 1,
            code: "empty_text".to_owned(),
        },
    )
    .await
    .unwrap();
    let released = repo.finish_office_parse(&scope, &fresh).await.unwrap();
    assert_eq!(released.status, ImportStatus::Partial);
    let evidence = repo
        .search(
            &scope,
            KnowledgeSearchRequest {
                query: "🦀".to_owned(),
                purpose: KnowledgePurpose::Public,
                knowledge_release_id: released.release.map(|release| release.knowledge_release_id),
                limit: 5,
            },
        )
        .await
        .unwrap();
    assert_eq!(evidence.evidence.len(), 1);
    assert!(
        matches!(&evidence.evidence[0].locator,ChunkLocator::Xlsx{sheet,range,header_range,..}
        if sheet == "数据" && range == "B2" && header_range.as_deref()==Some("A1:B1"))
    );
}
