//! Isolated PDF parser HTTP adapter and crash-recoverable background dispatch.
//! Only the application supplies the parser endpoint; imported documents cannot
//! influence the destination of an HTTP request.

use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
    time::Duration,
};

use geo_domain::{
    AppError, ErrorCode, KnowledgeRepository, MAX_UPLOAD_BYTES, PDF_MAX_PAGE_TEXT_BYTES,
    PDF_MAX_PAGES, PDF_PARSE_SCHEMA_VERSION, PdfDocumentManifest, PdfPageResult, PdfParseCursor,
    PdfParseInput, PdfParseJobRef, PdfParseLease, sha256_hex,
};
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tracing::warn;
use uuid::Uuid;

use crate::AppState;

pub const PDF_PARSER_PROFILE: &str = "tika-3.2.3_pdfbox-3.0.5_text-v1";
const LEASE_SECONDS: i64 = 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_INSPECT_RESPONSE: usize = 4096;
const MAX_PAGE_RESPONSE: usize = PDF_MAX_PAGE_TEXT_BYTES * 6 + 4096;

#[derive(Clone)]
pub struct PdfParserClient {
    client: Client,
    base: Url,
    execution_slots: Arc<OnceLock<Arc<Semaphore>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectResponse {
    schema_version: String,
    input_sha256: String,
    parser_version: String,
    page_count: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageResponse {
    schema_version: String,
    input_sha256: String,
    parser_version: String,
    page: u32,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParserErrorResponse {
    error: ParserErrorBody,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParserErrorBody {
    code: String,
    retryable: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthResponse {
    schema_version: String,
    parser_version: String,
    capacity: usize,
}

enum ParserFailure {
    Transient,
    Permanent(&'static str),
}

impl PdfParserClient {
    /// `base` comes from trusted application configuration, never an upload,
    /// request parameter or document. The builder does not emit the URL.
    pub fn new(endpoint: &str) -> Result<Self, AppError> {
        let mut base = Url::parse(endpoint)
            .map_err(|_| AppError::invalid_request("invalid PDF parser configuration"))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(AppError::invalid_request(
                "invalid PDF parser configuration",
            ));
        }
        base.set_path("/");
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| AppError::not_ready("PDF parser client unavailable"))?;
        Ok(Self {
            client,
            base,
            execution_slots: Arc::new(OnceLock::new()),
        })
    }

    /// Startup validates that the configured service actually implements this
    /// pinned extraction profile before imports claim it is available.
    pub async fn check_ready(&self) -> Result<(), AppError> {
        let url = self.base.join("health").map_err(|_| parser_error())?;
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| parser_error())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > MAX_INSPECT_RESPONSE as u64)
        {
            return Err(parser_error());
        }
        let mut body = Vec::new();
        while let Some(part) = response.chunk().await.map_err(|_| parser_error())? {
            if part.len() > MAX_INSPECT_RESPONSE.saturating_sub(body.len()) {
                return Err(parser_error());
            }
            body.extend_from_slice(&part);
        }
        let health: HealthResponse = serde_json::from_slice(&body).map_err(|_| parser_error())?;
        if health.schema_version != PDF_PARSE_SCHEMA_VERSION
            || health.parser_version != PDF_PARSER_PROFILE
            || !(1..=64).contains(&health.capacity)
        {
            return Err(parser_error());
        }
        self.execution_slots
            .get_or_init(|| Arc::new(Semaphore::new(health.capacity)));
        Ok(())
    }

    async fn reserve_execution(&self) -> Result<OwnedSemaphorePermit, AppError> {
        self.execution_slots
            .get()
            .ok_or_else(parser_error)?
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| parser_error())
    }

    async fn post(
        &self,
        path: &str,
        bytes: &[u8],
        max_response: usize,
    ) -> Result<Vec<u8>, ParserFailure> {
        let url = self.base.join(path).map_err(|_| ParserFailure::Transient)?;
        let mut response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/pdf")
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| ParserFailure::Transient)?;
        let status = response.status();
        if status.is_redirection()
            || status.is_server_error()
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            return Err(ParserFailure::Transient);
        }
        let cap = if status.is_success() {
            max_response
        } else {
            MAX_INSPECT_RESPONSE
        };
        if response
            .content_length()
            .is_some_and(|length| length > cap as u64)
        {
            return Err(ParserFailure::Transient);
        }
        let mut body = Vec::new();
        while let Some(part) = response
            .chunk()
            .await
            .map_err(|_| ParserFailure::Transient)?
        {
            if part.len() > cap.saturating_sub(body.len()) {
                return Err(ParserFailure::Transient);
            }
            body.extend_from_slice(&part);
        }
        if status.is_success() {
            return Ok(body);
        }
        let parsed: ParserErrorResponse =
            serde_json::from_slice(&body).map_err(|_| ParserFailure::Transient)?;
        if parsed.error.retryable {
            return Err(ParserFailure::Transient);
        }
        let code = match parsed.error.code.as_str() {
            "invalid_pdf" => "invalid_pdf",
            "encrypted_pdf" => "encrypted_pdf",
            "ocr_required" => "ocr_required",
            "empty_text" => "empty_text",
            "page_limit" | "page_limit_exceeded" | "output_too_large" => "page_limit",
            _ => "parse_failed",
        };
        Err(ParserFailure::Permanent(code))
    }

    async fn inspect(&self, input: &PdfParseInput) -> Result<PdfDocumentManifest, ParserFailure> {
        let body = self
            .post("v1/pdf/inspect", &input.bytes, MAX_INSPECT_RESPONSE)
            .await?;
        let parsed: InspectResponse =
            serde_json::from_slice(&body).map_err(|_| ParserFailure::Transient)?;
        let manifest = PdfDocumentManifest {
            schema_version: parsed.schema_version,
            input_sha256: parsed.input_sha256,
            parser_version: parsed.parser_version,
            page_count: parsed.page_count,
        };
        manifest
            .validate(&input.input_sha256, &input.parser_profile)
            .map_err(|_| ParserFailure::Transient)?;
        Ok(manifest)
    }

    async fn parse_page(
        &self,
        input: &PdfParseInput,
        page: u32,
    ) -> Result<PdfPageResult, ParserFailure> {
        let body = self
            .post(
                &format!("v1/pdf/pages/{page}/parse"),
                &input.bytes,
                MAX_PAGE_RESPONSE,
            )
            .await?;
        let parsed: PageResponse =
            serde_json::from_slice(&body).map_err(|_| ParserFailure::Transient)?;
        if parsed.schema_version != PDF_PARSE_SCHEMA_VERSION
            || parsed.input_sha256 != input.input_sha256
            || parsed.parser_version != input.parser_profile
            || parsed.page != page
        {
            return Err(ParserFailure::Transient);
        }
        let result = match (parsed.text, parsed.reason.as_deref()) {
            (Some(text), None)
                if !text.trim().is_empty() && text.len() <= PDF_MAX_PAGE_TEXT_BYTES =>
            {
                PdfPageResult::Success { page, text }
            }
            (Some(text), None) if text.len() > PDF_MAX_PAGE_TEXT_BYTES => PdfPageResult::Failure {
                page,
                code: "page_limit".into(),
            },
            (Some(text), Some("ocr_required")) if text.is_empty() => PdfPageResult::Failure {
                page,
                code: "ocr_required".into(),
            },
            (Some(text), Some("empty_text")) if text.is_empty() => PdfPageResult::Failure {
                page,
                code: "empty_text".into(),
            },
            (None, Some("parse_failed")) => PdfPageResult::Failure {
                page,
                code: "parse_failed".into(),
            },
            (None, Some("page_limit")) => PdfPageResult::Failure {
                page,
                code: "page_limit".into(),
            },
            (Some(text), None) if text.trim().is_empty() => PdfPageResult::Failure {
                page,
                code: "empty_text".into(),
            },
            _ => return Err(ParserFailure::Transient),
        };
        result
            .validate(PDF_MAX_PAGES)
            .map_err(|_| ParserFailure::Transient)?;
        Ok(result)
    }
}

fn parser_error() -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "PDF parser temporarily unavailable",
    )
}

/// A claimed job is processed without holding a database transaction during
/// the parser call. The renewal task fences every subsequent repository write.
pub async fn dispatch_pdf_parse_job(
    state: AppState,
    parser: PdfParserClient,
    job: PdfParseJobRef,
) -> Result<(), AppError> {
    let slot = parser.reserve_execution().await?;
    let Some(lease) = state
        .knowledge_repository()
        .claim_pdf_parse(&job.scope, job.job_id, Uuid::new_v4(), LEASE_SECONDS)
        .await?
    else {
        return Ok(());
    };
    dispatch_claimed_pdf_parse_job(state, parser, job, lease, slot).await
}

async fn dispatch_claimed_pdf_parse_job(
    state: AppState,
    parser: PdfParserClient,
    job: PdfParseJobRef,
    lease: PdfParseLease,
    _slot: OwnedSemaphorePermit,
) -> Result<(), AppError> {
    let repository = state.knowledge_repository();
    if !state.durable_storage()
        && let Some(operation) = repository
            .pdf_parse_operation(&job.scope, job.job_id)
            .await?
    {
        state.operation_store().save(operation).await?;
    }
    let (stop_tx, stop_rx) = watch::channel(false);
    let (lost_tx, mut lost_rx) = watch::channel(false);
    let heartbeat = tokio::spawn(heartbeat(
        repository.clone(),
        job.scope.clone(),
        lease.clone(),
        stop_rx,
        lost_tx,
    ));
    let outcome = process_claimed(&state, repository, &parser, &job, &lease, &mut lost_rx).await;
    let _ = stop_tx.send(true);
    let _ = heartbeat.await;
    outcome
}

async fn heartbeat(
    repository: Arc<dyn KnowledgeRepository>,
    scope: geo_domain::TenantScope,
    mut lease: PdfParseLease,
    mut stop: watch::Receiver<bool>,
    lost: watch::Sender<bool>,
) {
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                match repository.renew_pdf_parse(&scope, &lease, LEASE_SECONDS).await {
                    Ok(Some(new_lease)) => lease = new_lease,
                    _ => {
                        let _ = lost.send(true);
                        break;
                    }
                }
            }
        }
    }
}

async fn request_or_lease_loss<T>(
    lost: &mut watch::Receiver<bool>,
    request: impl std::future::Future<Output = Result<T, ParserFailure>>,
) -> Result<T, ParserFailure> {
    if *lost.borrow() {
        return Err(ParserFailure::Transient);
    }
    tokio::select! {
        outcome = request => outcome,
        _ = lost.changed() => Err(ParserFailure::Transient),
    }
}

async fn process_claimed(
    state: &AppState,
    repository: Arc<dyn KnowledgeRepository>,
    parser: &PdfParserClient,
    job: &PdfParseJobRef,
    lease: &PdfParseLease,
    lost: &mut watch::Receiver<bool>,
) -> Result<(), AppError> {
    let input = repository.pdf_parse_input(&job.scope, lease).await?;
    if input.bytes.is_empty()
        || input.bytes.len() as u64 > MAX_UPLOAD_BYTES
        || input.media_type != "application/pdf"
        || input.parser_profile != PDF_PARSER_PROFILE
        || sha256_hex(&input.bytes) != input.input_sha256
    {
        // A corrupted object must never be sent to the parser or released.
        return Err(AppError::new(
            ErrorCode::Internal,
            "PDF object verification failed",
        ));
    }
    let manifest = if let Some(manifest) = input.manifest.as_ref() {
        manifest.validate(&input.input_sha256, &input.parser_profile)?;
        manifest.clone()
    } else {
        let mut result = Err(ParserFailure::Transient);
        for attempt in 0..3 {
            result = request_or_lease_loss(lost, parser.inspect(&input)).await;
            if !matches!(result, Err(ParserFailure::Transient)) || attempt == 2 || *lost.borrow() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
        }
        match result {
            Ok(manifest) => {
                repository
                    .record_pdf_manifest(&job.scope, lease, manifest.clone())
                    .await?;
                manifest
            }
            Err(ParserFailure::Permanent(code)) => {
                if !*lost.borrow() {
                    let code = if matches!(code, "invalid_pdf" | "encrypted_pdf" | "page_limit") {
                        code
                    } else {
                        "parse_failed"
                    };
                    let acceptance = repository.fail_pdf_parse(&job.scope, lease, code).await?;
                    if !state.durable_storage()
                        && let Some(operation) = acceptance.operation
                    {
                        state.operation_store().save(operation).await?;
                    }
                }
                return Ok(());
            }
            Err(ParserFailure::Transient) => return Err(parser_error()),
        }
    };
    let completed: HashSet<u32> = input.successful_pages.iter().copied().collect();
    for page in 1..=manifest.page_count {
        if completed.contains(&page) {
            continue;
        }
        let mut result = Err(ParserFailure::Transient);
        for attempt in 0..3 {
            result = request_or_lease_loss(lost, parser.parse_page(&input, page)).await;
            if !matches!(result, Err(ParserFailure::Transient)) || attempt == 2 || *lost.borrow() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
        }
        let page_result = match result {
            Ok(result) => result,
            Err(ParserFailure::Permanent(code)) => {
                let code = if matches!(code, "ocr_required" | "empty_text" | "page_limit") {
                    code
                } else {
                    "parse_failed"
                };
                PdfPageResult::Failure {
                    page,
                    code: code.into(),
                }
            }
            Err(ParserFailure::Transient) => return Err(parser_error()),
        };
        if *lost.borrow() {
            return Err(parser_error());
        }
        repository
            .record_pdf_page(&job.scope, lease, page_result)
            .await?;
    }
    if *lost.borrow() {
        return Err(parser_error());
    }
    let acceptance = repository.finish_pdf_parse(&job.scope, lease).await?;
    if !state.durable_storage() {
        if let Some(operation) = acceptance.operation.as_ref() {
            state.operation_store().save(operation.clone()).await?;
        }
        if let Some(release) = acceptance.release.as_ref() {
            state.publish_event(geo_domain::EventEnvelope::new(
                "knowledge.release.created",
                job.scope.clone(),
                release.knowledge_release_id,
                release.sequence as u64,
                acceptance
                    .operation
                    .as_ref()
                    .map_or(release.knowledge_release_id, |operation| operation.id),
            ));
        }
    }
    Ok(())
}

/// Concurrent jobs are independent; persistent claims provide both
/// multi-replica exclusion and recovery after a process terminates.
pub fn spawn_pdf_parse_scanner(state: AppState, parser: PdfParserClient) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(5));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let repository = state.knowledge_repository();
            let mut after: Option<PdfParseCursor> = None;
            loop {
                match repository.pdf_parse_candidates(after, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for job in candidates {
                            after = Some(PdfParseCursor {
                                created_at: job.created_at,
                                job_id: job.job_id,
                            });
                            // Reserve the service's actual execution capacity
                            // before claiming or loading any original bytes.
                            let slot = match parser.reserve_execution().await {
                                Ok(slot) => slot,
                                Err(error) => {
                                    warn!(code = ?error.code, "PDF parser capacity unavailable");
                                    break;
                                }
                            };
                            let lease = match repository
                                .claim_pdf_parse(
                                    &job.scope,
                                    job.job_id,
                                    Uuid::new_v4(),
                                    LEASE_SECONDS,
                                )
                                .await
                            {
                                Ok(Some(lease)) => lease,
                                Ok(None) => continue,
                                Err(error) => {
                                    warn!(code = ?error.code, "PDF job claim deferred");
                                    continue;
                                }
                            };
                            let state = state.clone();
                            let parser = parser.clone();
                            tokio::spawn(async move {
                                let work = tokio::spawn(dispatch_claimed_pdf_parse_job(
                                    state, parser, job, lease, slot,
                                ));
                                match work.await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => {
                                        warn!(code = ?error.code, "PDF parser job deferred")
                                    }
                                    Err(_) => {
                                        warn!("PDF parser job panicked; lease recovery pending")
                                    }
                                }
                            });
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "PDF job discovery deferred");
                        break;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::post};
    use geo_domain::{ImportStatus, KnowledgePurpose, TenantScope, UploadSessionCommand};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    fn input() -> PdfParseInput {
        PdfParseInput {
            bytes: b"%PDF-1.4\n".to_vec(),
            input_sha256: "verified-hash".into(),
            media_type: "application/pdf".into(),
            parser_profile: PDF_PARSER_PROFILE.into(),
            successful_pages: Vec::new(),
            manifest: None,
        }
    }

    fn valid_inspect() -> Value {
        json!({
            "schema_version": PDF_PARSE_SCHEMA_VERSION,
            "input_sha256": "verified-hash",
            "parser_version": PDF_PARSER_PROFILE,
            "page_count": 2
        })
    }

    fn valid_page() -> Value {
        json!({
            "schema_version": PDF_PARSE_SCHEMA_VERSION,
            "input_sha256": "verified-hash",
            "parser_version": PDF_PARSER_PROFILE,
            "page": 2,
            "text": "verified page text"
        })
    }

    async fn fixture(status: StatusCode, body: Value, capacity: usize) -> PdfParserClient {
        let app = Router::new()
            .route(
                "/health",
                axum::routing::get(move || async move {
                    Json(json!({
                        "schema_version": PDF_PARSE_SCHEMA_VERSION,
                        "parser_version": PDF_PARSER_PROFILE,
                        "capacity": capacity,
                    }))
                }),
            )
            .route(
                "/v1/pdf/inspect",
                post({
                    let body = body.clone();
                    move || {
                        let body = body.clone();
                        async move { (status, Json(body)) }
                    }
                }),
            )
            .route(
                "/v1/pdf/pages/2/parse",
                post(move || {
                    let body = body.clone();
                    async move { (status, Json(body)) }
                }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let parser = PdfParserClient::new(&endpoint).unwrap();
        if (1..=64).contains(&capacity) {
            parser.check_ready().await.unwrap();
        }
        parser
    }

    #[tokio::test]
    async fn inspect_rejects_unverified_identity_and_excess_response() {
        for (key, replacement) in [
            ("schema_version", json!("other")),
            ("input_sha256", json!("wrong")),
            ("parser_version", json!("wrong")),
            ("page_count", json!(0)),
            ("page_count", json!(10001)),
        ] {
            let mut body = valid_inspect();
            body[key] = replacement;
            let parser = fixture(StatusCode::OK, body, 1).await;
            assert!(matches!(
                parser.inspect(&input()).await,
                Err(ParserFailure::Transient)
            ));
        }
        let parser = fixture(StatusCode::OK, json!({"oversized": "x".repeat(5000)}), 1).await;
        assert!(matches!(
            parser.inspect(&input()).await,
            Err(ParserFailure::Transient)
        ));
    }

    #[tokio::test]
    async fn page_rejects_wrong_identity_and_stores_only_fixed_failures() {
        for (key, replacement) in [
            ("schema_version", json!("other")),
            ("input_sha256", json!("wrong")),
            ("parser_version", json!("wrong")),
            ("page", json!(1)),
        ] {
            let mut body = valid_page();
            body[key] = replacement;
            let parser = fixture(StatusCode::OK, body, 1).await;
            assert!(matches!(
                parser.parse_page(&input(), 2).await,
                Err(ParserFailure::Transient)
            ));
        }
        let mut body = valid_page();
        body["text"] = json!("");
        body["reason"] = json!("ocr_required");
        let parser = fixture(StatusCode::OK, body, 1).await;
        assert!(matches!(
            parser.parse_page(&input(), 2).await,
            Ok(PdfPageResult::Failure { page: 2, code }) if code == "ocr_required"
        ));
    }

    #[tokio::test]
    async fn redirects_server_errors_and_malformed_errors_are_transient() {
        for (status, body) in [
            (StatusCode::FOUND, json!({})),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":{"code":"parser_busy","retryable":false}}),
            ),
            (StatusCode::BAD_REQUEST, json!({"message":"untrusted"})),
        ] {
            let parser = fixture(status, body, 1).await;
            assert!(matches!(
                parser.inspect(&input()).await,
                Err(ParserFailure::Transient)
            ));
        }
        let parser = fixture(
            StatusCode::BAD_REQUEST,
            json!({"error":{"code":"invalid_pdf","retryable":false}}),
            1,
        )
        .await;
        assert!(matches!(
            parser.inspect(&input()).await,
            Err(ParserFailure::Permanent("invalid_pdf"))
        ));
    }

    #[tokio::test]
    async fn service_capacity_is_required_and_shared_by_client_clones() {
        let parser = fixture(StatusCode::OK, valid_inspect(), 1).await;
        let held_slot = parser.reserve_execution().await.unwrap();
        assert!(
            parser
                .clone()
                .execution_slots
                .get()
                .unwrap()
                .try_acquire()
                .is_err()
        );
        drop(held_slot);
        assert!(
            parser
                .clone()
                .execution_slots
                .get()
                .unwrap()
                .try_acquire()
                .is_ok()
        );
        for invalid in [0, 65] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().route("/health", axum::routing::get(move || async move {
                Json(json!({"schema_version":PDF_PARSE_SCHEMA_VERSION,"parser_version":PDF_PARSER_PROFILE,"capacity":invalid}))
            }));
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            assert!(
                PdfParserClient::new(&endpoint)
                    .unwrap()
                    .check_ready()
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn scanner_does_not_claim_more_jobs_than_service_can_execute() {
        let gate = Arc::new(Semaphore::new(0));
        let entered = Arc::new(AtomicUsize::new(0));
        let bytes = b"%PDF-1.4\nfixture".to_vec();
        let hash = sha256_hex(&bytes);
        let inspect = {
            let gate = gate.clone();
            let entered = entered.clone();
            let hash = hash.clone();
            move || {
                let gate = gate.clone();
                let entered = entered.clone();
                let hash = hash.clone();
                async move {
                    entered.fetch_add(1, Ordering::SeqCst);
                    let _permit = gate.acquire().await.unwrap();
                    Json(
                        json!({"schema_version":PDF_PARSE_SCHEMA_VERSION,"input_sha256":hash,
                        "parser_version":PDF_PARSER_PROFILE,"page_count":1}),
                    )
                }
            }
        };
        let app = Router::new()
            .route("/health", axum::routing::get(|| async {
                Json(json!({"schema_version":PDF_PARSE_SCHEMA_VERSION,
                    "parser_version":PDF_PARSER_PROFILE,"capacity":1}))
            }))
            .route("/v1/pdf/inspect", post(inspect))
            .route("/v1/pdf/pages/1/parse", post(move || {
                let hash = hash.clone();
                async move { Json(json!({"schema_version":PDF_PARSE_SCHEMA_VERSION,
                    "input_sha256":hash,"parser_version":PDF_PARSER_PROFILE,"page":1,"text":"readable"})) }
            }));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let parser = PdfParserClient::new(&endpoint).unwrap();
        parser.check_ready().await.unwrap();
        let state =
            AppState::development_with_pdf_parser_profile("password", PDF_PARSER_PROFILE.into());
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let repository = state.knowledge_repository();
        for _ in 0..3 {
            let session = repository
                .create_upload_session(
                    &scope,
                    UploadSessionCommand {
                        filename: "sample.pdf".into(),
                        declared_media_type: "application/pdf".into(),
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
            assert_eq!(
                repository
                    .complete_upload(&scope, session.upload_session_id, "queue")
                    .await
                    .unwrap()
                    .status,
                ImportStatus::Queued
            );
        }
        spawn_pdf_parse_scanner(state, parser);
        tokio::time::timeout(Duration::from_secs(3), async {
            while entered.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first parser execution began");
        let jobs = repository.pdf_parse_candidates(None, 10).await.unwrap();
        let queued = jobs
            .iter()
            .filter(|candidate| candidate.scope == scope)
            .count();
        assert_eq!(
            queued, 2,
            "two jobs remain unclaimed while service's sole slot is busy"
        );
        assert_eq!(
            entered.load(Ordering::SeqCst),
            1,
            "the service has one in-flight execution"
        );
        gate.add_permits(3);
    }
}
