//! Real-service Office integration is explicitly ignored by default; CI
//! supplies the pinned Java parser endpoint when running that test.

use axum::{Json, Router, http::StatusCode, routing::post};
use geo_api::{
    AppState, OFFICE_PARSER_PROFILE, OfficeParserClient, dispatch_office_parse_job,
    spawn_office_parse_scanner,
};
use geo_domain::{
    ChunkLocator, ImportStatus, KnowledgePurpose, KnowledgeRepository, KnowledgeSearchRequest,
    OfficeFormat, TenantScope, UploadSessionCommand, sha256_hex,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

/// A tiny stored-entry ZIP built from text in this test, not a checked-in
/// binary fixture or an external document. It exercises the real OOXML parser.
fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
    fn put_u16(output: &mut Vec<u8>, value: u16) {
        output.extend_from_slice(&value.to_le_bytes());
    }
    fn put_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }
    let mut result = Vec::new();
    let mut directory = Vec::new();
    for (name, text) in entries {
        let offset = result.len() as u32;
        let name = name.as_bytes();
        let bytes = text.as_bytes();
        let crc = crc32(bytes);
        put_u32(&mut result, 0x04034b50);
        put_u16(&mut result, 20);
        put_u16(&mut result, 0);
        put_u16(&mut result, 0);
        put_u16(&mut result, 0);
        put_u16(&mut result, 0);
        put_u32(&mut result, crc);
        put_u32(&mut result, bytes.len() as u32);
        put_u32(&mut result, bytes.len() as u32);
        put_u16(&mut result, name.len() as u16);
        put_u16(&mut result, 0);
        result.extend_from_slice(name);
        result.extend_from_slice(bytes);

        put_u32(&mut directory, 0x02014b50);
        put_u16(&mut directory, 20);
        put_u16(&mut directory, 20);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, 0);
        put_u32(&mut directory, crc);
        put_u32(&mut directory, bytes.len() as u32);
        put_u32(&mut directory, bytes.len() as u32);
        put_u16(&mut directory, name.len() as u16);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, 0);
        put_u32(&mut directory, 0);
        put_u32(&mut directory, offset);
        directory.extend_from_slice(name);
    }
    let directory_start = result.len() as u32;
    result.extend_from_slice(&directory);
    put_u32(&mut result, 0x06054b50);
    put_u16(&mut result, 0);
    put_u16(&mut result, 0);
    put_u16(&mut result, entries.len() as u16);
    put_u16(&mut result, entries.len() as u16);
    put_u32(&mut result, directory.len() as u32);
    put_u32(&mut result, directory_start);
    put_u16(&mut result, 0);
    result
}

fn docx() -> Vec<u8> {
    zip(&[
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/document.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Verified heading</w:t></w:r></w:p><w:p><w:r><w:t>Almond evidence paragraph</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Price</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42 USD</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#,
        ),
    ])
}

fn xlsx() -> Vec<u8> {
    zip(&[
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Pricing" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Price</t></is></c><c r="B1"><v>42</v></c><c r="C1"><f>SUM(B1,1)</f><v>43</v></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Available</t></is></c><c r="B2" t="b"><v>1</v></c></row></sheetData></worksheet>"#,
        ),
    ])
}

async fn upload(
    repository: &dyn KnowledgeRepository,
    scope: &TenantScope,
    bytes: &[u8],
) -> geo_domain::ImportAcceptance {
    let session = repository
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "example.docx".into(),
                declared_media_type: OfficeFormat::Docx.media_type().into(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(scope, session.upload_session_id, bytes.to_vec())
        .await
        .unwrap();
    repository
        .complete_upload(scope, session.upload_session_id, "office-upload")
        .await
        .unwrap()
}

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

fn inspect(bytes: &[u8]) -> Value {
    json!({
        "schema_version":"geo.office.parse.v1",
        "input_sha256":sha256_hex(bytes),
        "parser_version":OFFICE_PARSER_PROFILE,
        "media_type":OfficeFormat::Docx.media_type(),
        "document":{"format":"docx","units":[
            {"unit_id":0,"start_body_element":0,"end_body_element":0}
        ]}
    })
}

fn parsed(bytes: &[u8]) -> Value {
    json!({
        "schema_version":"geo.office.parse.v1",
        "input_sha256":sha256_hex(bytes),
        "parser_version":OFFICE_PARSER_PROFILE,
        "media_type":OfficeFormat::Docx.media_type(),
        "result":{"kind":"docx_success","unit_id":0,"elements":[
            {"kind":"paragraph","body_element_index":0,"paragraph_index":0,
             "heading_path":[],"text":"verified adapter evidence"}
        ]}
    })
}

async fn mock_parser(
    manifest: Value,
    unit: Value,
    inspect_status: StatusCode,
) -> OfficeParserClient {
    let app = Router::new()
        .route(
            "/health",
            axum::routing::get(|| async {
                Json(json!({
                    "schema_version":"geo.pdf.parse.v1",
                    "parser_version":geo_api::PDF_PARSER_PROFILE,
                    "capacity":1,
                    "office_schema_version":"geo.office.parse.v1",
                    "office_parser_version":OFFICE_PARSER_PROFILE,
                    "office_capacity":1
                }))
            }),
        )
        .route(
            "/v1/office/inspect",
            post(move || {
                let manifest = manifest.clone();
                async move { (inspect_status, Json(manifest)) }
            }),
        )
        .route(
            "/v1/office/units/0/parse",
            post(move || {
                let unit = unit.clone();
                async move { Json(unit) }
            }),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let parser = OfficeParserClient::new(&endpoint).unwrap();
    parser.check_ready().await.unwrap();
    parser
}

#[tokio::test]
async fn unconfigured_office_formats_have_no_parser_capability() {
    let state = AppState::development_with_password("password");
    let scope = scope();
    let capability = state
        .knowledge_repository()
        .capabilities(&scope)
        .await
        .unwrap();
    assert!(!capability.docx_parser);
    assert!(!capability.xlsx_parser);
    let accepted = upload(state.knowledge_repository().as_ref(), &scope, &docx()).await;
    assert_eq!(accepted.status, ImportStatus::Failed);
    assert!(accepted.source_version.is_none());
    assert!(accepted.release.is_none());
}

#[tokio::test]
async fn parser_identity_and_format_mismatch_never_generate_evidence() {
    let bytes = docx();
    for index in 0..6 {
        let mut manifest = inspect(&bytes);
        let mut unit = parsed(&bytes);
        match index {
            0 => manifest["input_sha256"] = json!("wrong"),
            1 => manifest["parser_version"] = json!("wrong"),
            2 => manifest["media_type"] = json!(OfficeFormat::Xlsx.media_type()),
            3 => unit["result"]["unit_id"] = json!(99),
            4 => unit["result"]["kind"] = json!("xlsx_success"),
            _ => unit["input_sha256"] = json!("wrong"),
        }
        let parser = mock_parser(manifest, unit, StatusCode::OK).await;
        let state = AppState::development_with_office_parser_profile(
            "password",
            OFFICE_PARSER_PROFILE.into(),
        );
        let repository = state.knowledge_repository();
        let scope = scope();
        let acceptance = upload(repository.as_ref(), &scope, &bytes).await;
        assert_eq!(acceptance.status, ImportStatus::Queued);
        let job = repository
            .office_parse_candidates(None, 10)
            .await
            .unwrap()
            .remove(0);
        assert!(dispatch_office_parse_job(state, parser, job).await.is_err());
        let detail = repository
            .get_source_detail(&scope, acceptance.source.unwrap().source_id)
            .await
            .unwrap()
            .unwrap();
        assert!(detail.versions.is_empty());
        assert!(detail.chunks.is_empty());
    }
}

#[tokio::test]
async fn parser_errors_are_fixed_and_do_not_leak_raw_response() {
    let bytes = docx();
    let parser = mock_parser(
        json!({"error":{"code":"invalid_docx","retryable":false}}),
        parsed(&bytes),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let state =
        AppState::development_with_office_parser_profile("password", OFFICE_PARSER_PROFILE.into());
    let repository = state.knowledge_repository();
    let scope = scope();
    let accepted = upload(repository.as_ref(), &scope, &bytes).await;
    let job = repository
        .office_parse_candidates(None, 10)
        .await
        .unwrap()
        .remove(0);
    dispatch_office_parse_job(state, parser, job).await.unwrap();
    let progress = repository
        .get_import_progress(
            &scope,
            accepted.import_job.unwrap().import_job_id,
            KnowledgePurpose::Internal,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.status, ImportStatus::Failed);
    assert_eq!(progress.errors[0].code, "invalid_docx");
}

#[tokio::test]
async fn redirects_and_malformed_errors_never_turn_into_permanent_parse_failures() {
    let bytes = docx();
    for (status, body) in [
        (StatusCode::FOUND, json!({"location":"untrusted"})),
        (
            StatusCode::BAD_REQUEST,
            json!({"message":"sensitive parser detail must not escape"}),
        ),
    ] {
        let parser = mock_parser(body, parsed(&bytes), status).await;
        let state = AppState::development_with_office_parser_profile(
            "password",
            OFFICE_PARSER_PROFILE.into(),
        );
        let repository = state.knowledge_repository();
        let scope = scope();
        let accepted = upload(repository.as_ref(), &scope, &bytes).await;
        let job = repository
            .office_parse_candidates(None, 10)
            .await
            .unwrap()
            .remove(0);
        let error = dispatch_office_parse_job(state, parser, job)
            .await
            .expect_err("malformed or redirected response defers job");
        assert_eq!(error.code, geo_domain::ErrorCode::DependencyUnavailable);
        assert!(!error.message.contains("sensitive"));
        assert!(
            repository
                .get_source_detail(&scope, accepted.source.unwrap().source_id)
                .await
                .unwrap()
                .unwrap()
                .versions
                .is_empty()
        );
    }
}

#[tokio::test]
async fn scanner_reserves_capacity_before_claiming_or_loading_originals() {
    let bytes = docx();
    let hash = sha256_hex(&bytes);
    let gate = Arc::new(Semaphore::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let manifest = inspect(&bytes);
    let response = parsed(&bytes);
    let app = Router::new()
        .route(
            "/health",
            axum::routing::get(|| async {
                Json(json!({
                    "schema_version":"geo.pdf.parse.v1",
                    "parser_version":geo_api::PDF_PARSER_PROFILE,
                    "capacity":1,
                    "office_schema_version":"geo.office.parse.v1",
                    "office_parser_version":OFFICE_PARSER_PROFILE,
                    "office_capacity":1
                }))
            }),
        )
        .route(
            "/v1/office/inspect",
            post({
                let gate = gate.clone();
                let entered = entered.clone();
                move || {
                    let gate = gate.clone();
                    let entered = entered.clone();
                    let manifest = manifest.clone();
                    async move {
                        entered.fetch_add(1, Ordering::SeqCst);
                        let _permit = gate.acquire().await.unwrap();
                        Json(manifest)
                    }
                }
            }),
        )
        .route(
            "/v1/office/units/0/parse",
            post(move || {
                let response = response.clone();
                async move { Json(response) }
            }),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let parser = OfficeParserClient::new(&endpoint).unwrap();
    parser.check_ready().await.unwrap();
    let state =
        AppState::development_with_office_parser_profile("password", OFFICE_PARSER_PROFILE.into());
    let scope = scope();
    let repository = state.knowledge_repository();
    for index in 0..3 {
        let session = repository
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "example.docx".into(),
                    declared_media_type: OfficeFormat::Docx.media_type().into(),
                    expected_size: bytes.len() as u64,
                    expected_sha256: hash.clone(),
                    purpose: KnowledgePurpose::Public,
                },
            )
            .await
            .unwrap();
        repository
            .put_upload_content(&scope, session.upload_session_id, bytes.clone())
            .await
            .unwrap();
        assert_eq!(
            repository
                .complete_upload(&scope, session.upload_session_id, &format!("queue-{index}"))
                .await
                .unwrap()
                .status,
            ImportStatus::Queued
        );
    }
    spawn_office_parse_scanner(state, parser);
    tokio::time::timeout(Duration::from_secs(3), async {
        while entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first parser execution began");
    assert_eq!(
        repository
            .office_parse_candidates(None, 10)
            .await
            .unwrap()
            .iter()
            .filter(|candidate| candidate.scope == scope)
            .count(),
        2,
        "remaining jobs must remain unclaimed while parser slot is busy"
    );
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    gate.add_permits(3);
}

#[tokio::test]
#[ignore = "requires pinned Java service running at GEO_TEST_OFFICE_PARSER_URL"]
async fn real_java_docx_retains_structured_provenance_and_search() {
    let endpoint = std::env::var("GEO_TEST_OFFICE_PARSER_URL")
        .expect("set GEO_TEST_OFFICE_PARSER_URL to the running Java parser");
    let parser = OfficeParserClient::new(&endpoint).unwrap();
    parser.check_ready().await.unwrap();
    let state =
        AppState::development_with_office_parser_profile("password", OFFICE_PARSER_PROFILE.into());
    let scope = scope();
    let repository = state.knowledge_repository();
    let bytes = docx();
    let accepted = upload(repository.as_ref(), &scope, &bytes).await;
    assert_eq!(accepted.status, ImportStatus::Queued);
    let source_id = accepted.source.unwrap().source_id;
    let candidate = repository
        .office_parse_candidates(None, 10)
        .await
        .unwrap()
        .remove(0);
    dispatch_office_parse_job(state, parser, candidate)
        .await
        .expect("real DOCX parsing");
    let detail = repository
        .get_source_detail(&scope, source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.import_jobs[0].status, ImportStatus::Succeeded);
    assert_eq!(detail.versions[0].content_sha256, sha256_hex(&bytes));
    assert!(detail.chunks.iter().any(|chunk| matches!(
        chunk.locator,
        ChunkLocator::Docx {
            table_index: Some(_),
            ..
        }
    ) && chunk.text.contains("42 USD")));
    let search = repository
        .search(
            &scope,
            KnowledgeSearchRequest {
                query: "Almond".into(),
                purpose: KnowledgePurpose::Public,
                limit: 10,
                knowledge_release_id: None,
            },
        )
        .await
        .unwrap();
    assert!(search.evidence.iter().any(|evidence| matches!(
        evidence.locator,
        ChunkLocator::Docx { .. }
    ) && evidence.text.contains("Almond")));
}

#[tokio::test]
#[ignore = "requires pinned Java service running at GEO_TEST_OFFICE_PARSER_URL"]
async fn real_java_xlsx_retains_raw_typed_cells_and_cached_formula() {
    let endpoint = std::env::var("GEO_TEST_OFFICE_PARSER_URL")
        .expect("set GEO_TEST_OFFICE_PARSER_URL to the running Java parser");
    let parser = OfficeParserClient::new(&endpoint).unwrap();
    parser.check_ready().await.unwrap();
    let state =
        AppState::development_with_office_parser_profile("password", OFFICE_PARSER_PROFILE.into());
    let scope = scope();
    let repository = state.knowledge_repository();
    let bytes = xlsx();
    let session = repository
        .create_upload_session(
            &scope,
            UploadSessionCommand {
                filename: "example.xlsx".into(),
                declared_media_type: OfficeFormat::Xlsx.media_type().into(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(&bytes),
                purpose: KnowledgePurpose::Public,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(&scope, session.upload_session_id, bytes.clone())
        .await
        .unwrap();
    let accepted = repository
        .complete_upload(&scope, session.upload_session_id, "xlsx-upload")
        .await
        .unwrap();
    assert_eq!(accepted.status, ImportStatus::Queued);
    let source_id = accepted.source.unwrap().source_id;
    let candidate = repository
        .office_parse_candidates(None, 10)
        .await
        .unwrap()
        .remove(0);
    dispatch_office_parse_job(state, parser, candidate)
        .await
        .expect("real XLSX parsing");
    let detail = repository
        .get_source_detail(&scope, source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.import_jobs[0].status, ImportStatus::Succeeded);
    assert_eq!(detail.versions[0].content_sha256, sha256_hex(&bytes));
    assert!(detail.chunks.iter().any(|chunk| {
        matches!(
            &chunk.locator,
            ChunkLocator::Xlsx { sheet, range, cell_kind: Some(kind), formula: Some(formula),
                cached_value: Some(cached), ..}
            if sheet == "Pricing" && range == "C1" && kind == "formulacached"
                && formula == "SUM(B1,1)" && cached == "43"
        ) && chunk.text == "43"
    }));
    assert!(detail.chunks.iter().any(|chunk| matches!(
        &chunk.locator, ChunkLocator::Xlsx { sheet, range, .. }
        if sheet == "Pricing" && range == "B1"
    ) && chunk.text == "42"));
}
