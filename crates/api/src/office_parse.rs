//! Isolated Office parser adapter and lease-fenced, recoverable dispatch.
//! Only trusted application configuration can select the parser destination.

use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
    time::Duration,
};

use geo_domain::{
    AppError, ErrorCode, KnowledgeRepository, MAX_UPLOAD_BYTES, OFFICE_MAX_UNIT_TEXT_BYTES,
    OFFICE_PARSE_SCHEMA_VERSION, OfficeDocumentManifest, OfficeFormat, OfficeParseCursor,
    OfficeParseInput, OfficeParseJobRef, OfficeParseLease, OfficeUnitResult,
    office_document_error_code, office_unit_error_code, sha256_hex,
};
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tracing::warn;
use uuid::Uuid;

use crate::AppState;

pub const OFFICE_PARSER_PROFILE: &str = "poi-5.4.1_ooxml-struct-v1";
const LEASE_SECONDS: i64 = 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_HEALTH_RESPONSE: usize = 4096;
const MAX_MANIFEST_RESPONSE: usize = 12 * 1024 * 1024;
const MAX_UNIT_RESPONSE: usize = OFFICE_MAX_UNIT_TEXT_BYTES * 2 + 4096;

#[derive(Clone)]
pub struct OfficeParserClient {
    client: Client,
    base: Url,
    execution_slots: Arc<OnceLock<Arc<Semaphore>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthResponse {
    office_schema_version: String,
    office_parser_version: String,
    office_capacity: usize,
    // Shared Java service still includes its PDF health contract.
    schema_version: String,
    parser_version: String,
    capacity: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnitResponse {
    schema_version: String,
    input_sha256: String,
    parser_version: String,
    media_type: String,
    result: OfficeUnitResult,
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

enum ParserFailure {
    Transient,
    Permanent(&'static str),
}

fn parser_error() -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "Office parser temporarily unavailable",
    )
}

impl OfficeParserClient {
    /// Endpoint comes only from server-side configuration, not an uploaded file.
    pub fn new(endpoint: &str) -> Result<Self, AppError> {
        let mut base = Url::parse(endpoint)
            .map_err(|_| AppError::invalid_request("invalid Office parser configuration"))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(AppError::invalid_request(
                "invalid Office parser configuration",
            ));
        }
        base.set_path("/");
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| parser_error())?;
        Ok(Self {
            client,
            base,
            execution_slots: Arc::new(OnceLock::new()),
        })
    }

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
                .is_some_and(|length| length > MAX_HEALTH_RESPONSE as u64)
        {
            return Err(parser_error());
        }
        let mut bytes = Vec::new();
        while let Some(part) = response.chunk().await.map_err(|_| parser_error())? {
            if part.len() > MAX_HEALTH_RESPONSE.saturating_sub(bytes.len()) {
                return Err(parser_error());
            }
            bytes.extend_from_slice(&part);
        }
        let health: HealthResponse = serde_json::from_slice(&bytes).map_err(|_| parser_error())?;
        if health.office_schema_version != OFFICE_PARSE_SCHEMA_VERSION
            || health.office_parser_version != OFFICE_PARSER_PROFILE
            || !(1..=64).contains(&health.office_capacity)
            || health.schema_version != geo_domain::PDF_PARSE_SCHEMA_VERSION
            || health.parser_version != crate::PDF_PARSER_PROFILE
            || health.capacity != health.office_capacity
        {
            return Err(parser_error());
        }
        self.execution_slots
            .get_or_init(|| Arc::new(Semaphore::new(health.office_capacity)));
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
        input: &OfficeParseInput,
        max_response: usize,
    ) -> Result<Vec<u8>, ParserFailure> {
        let url = self.base.join(path).map_err(|_| ParserFailure::Transient)?;
        let mut response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, &input.media_type)
            .body(input.bytes.clone())
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
        let limit = if status.is_success() {
            max_response
        } else {
            MAX_HEALTH_RESPONSE
        };
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(ParserFailure::Transient);
        }
        let mut bytes = Vec::new();
        while let Some(part) = response
            .chunk()
            .await
            .map_err(|_| ParserFailure::Transient)?
        {
            if part.len() > limit.saturating_sub(bytes.len()) {
                return Err(ParserFailure::Transient);
            }
            bytes.extend_from_slice(&part);
        }
        if status.is_success() {
            return Ok(bytes);
        }
        let error: ParserErrorResponse =
            serde_json::from_slice(&bytes).map_err(|_| ParserFailure::Transient)?;
        if error.error.retryable || !office_document_error_code(&error.error.code) {
            return Err(ParserFailure::Transient);
        }
        let code = match error.error.code.as_str() {
            "invalid_docx" => "invalid_docx",
            "invalid_xlsx" => "invalid_xlsx",
            "encrypted_office" => "encrypted_office",
            "unit_limit" => "unit_limit",
            "unsupported_content" => "unsupported_content",
            _ => "parse_failed",
        };
        Err(ParserFailure::Permanent(code))
    }

    async fn inspect(
        &self,
        input: &OfficeParseInput,
    ) -> Result<OfficeDocumentManifest, ParserFailure> {
        let bytes = self
            .post("v1/office/inspect", input, MAX_MANIFEST_RESPONSE)
            .await?;
        let manifest: OfficeDocumentManifest =
            serde_json::from_slice(&bytes).map_err(|_| ParserFailure::Transient)?;
        manifest
            .validate(&input.input_sha256, &input.parser_profile)
            .map_err(|_| ParserFailure::Transient)?;
        if manifest.media_type != input.media_type {
            return Err(ParserFailure::Transient);
        }
        Ok(manifest)
    }

    async fn parse_unit(
        &self,
        input: &OfficeParseInput,
        manifest: &OfficeDocumentManifest,
        unit_id: u32,
    ) -> Result<OfficeUnitResult, ParserFailure> {
        let bytes = self
            .post(
                &format!("v1/office/units/{unit_id}/parse"),
                input,
                MAX_UNIT_RESPONSE,
            )
            .await?;
        let parsed: UnitResponse =
            serde_json::from_slice(&bytes).map_err(|_| ParserFailure::Transient)?;
        if parsed.schema_version != OFFICE_PARSE_SCHEMA_VERSION
            || parsed.input_sha256 != input.input_sha256
            || parsed.parser_version != input.parser_profile
            || parsed.media_type != input.media_type
            || parsed.result.unit_id() != unit_id
        {
            return Err(ParserFailure::Transient);
        }
        parsed
            .result
            .validate(manifest)
            .map_err(|_| ParserFailure::Transient)?;
        Ok(parsed.result)
    }
}

/// The local execution slot is acquired before claim and original-byte read.
pub async fn dispatch_office_parse_job(
    state: AppState,
    parser: OfficeParserClient,
    job: OfficeParseJobRef,
) -> Result<(), AppError> {
    let slot = parser.reserve_execution().await?;
    let Some(lease) = state
        .knowledge_repository()
        .claim_office_parse(&job.scope, job.job_id, Uuid::new_v4(), LEASE_SECONDS)
        .await?
    else {
        return Ok(());
    };
    dispatch_claimed_office_parse_job(state, parser, job, lease, slot).await
}

async fn dispatch_claimed_office_parse_job(
    state: AppState,
    parser: OfficeParserClient,
    job: OfficeParseJobRef,
    lease: OfficeParseLease,
    _slot: OwnedSemaphorePermit,
) -> Result<(), AppError> {
    let repository = state.knowledge_repository();
    if !state.durable_storage()
        && let Some(operation) = repository
            .office_parse_operation(&job.scope, job.job_id)
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
    mut lease: OfficeParseLease,
    mut stop: watch::Receiver<bool>,
    lost: watch::Sender<bool>,
) {
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                match repository.renew_office_parse(&scope, &lease, LEASE_SECONDS).await {
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
    parser: &OfficeParserClient,
    job: &OfficeParseJobRef,
    lease: &OfficeParseLease,
    lost: &mut watch::Receiver<bool>,
) -> Result<(), AppError> {
    let input = repository.office_parse_input(&job.scope, lease).await?;
    if input.bytes.is_empty()
        || input.bytes.len() as u64 > MAX_UPLOAD_BYTES
        || input.parser_profile != OFFICE_PARSER_PROFILE
        || !matches!(
            input.media_type.as_str(),
            media if media == OfficeFormat::Docx.media_type()
                || media == OfficeFormat::Xlsx.media_type()
        )
        || sha256_hex(&input.bytes) != input.input_sha256
    {
        return Err(AppError::new(
            ErrorCode::Internal,
            "Office object verification failed",
        ));
    }
    let manifest = if let Some(manifest) = &input.manifest {
        manifest.validate(&input.input_sha256, &input.parser_profile)?;
        if manifest.media_type != input.media_type {
            return Err(AppError::new(
                ErrorCode::Internal,
                "Office object verification failed",
            ));
        }
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
                    .record_office_manifest(&job.scope, lease, manifest.clone())
                    .await?;
                manifest
            }
            Err(ParserFailure::Permanent(code)) => {
                if !*lost.borrow() {
                    let acceptance = repository
                        .fail_office_parse(&job.scope, lease, code)
                        .await?;
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
    let completed: HashSet<u32> = input.successful_units.iter().copied().collect();
    for ordinal in 0..manifest.unit_count() {
        let unit_id = manifest
            .unit_id(ordinal)
            .expect("validated manifest has unit");
        if completed.contains(&unit_id) {
            continue;
        }
        let mut result = Err(ParserFailure::Transient);
        for attempt in 0..3 {
            result =
                request_or_lease_loss(lost, parser.parse_unit(&input, &manifest, unit_id)).await;
            if !matches!(result, Err(ParserFailure::Transient)) || attempt == 2 || *lost.borrow() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
        }
        let result = match result {
            Ok(result) => result,
            Err(ParserFailure::Permanent(code)) => OfficeUnitResult::Failure {
                unit_id,
                code: if office_unit_error_code(code) {
                    code
                } else {
                    "parse_failed"
                }
                .into(),
            },
            Err(ParserFailure::Transient) => return Err(parser_error()),
        };
        if *lost.borrow() {
            return Err(parser_error());
        }
        repository
            .record_office_unit(&job.scope, lease, result)
            .await?;
    }
    if *lost.borrow() {
        return Err(parser_error());
    }
    let acceptance = repository.finish_office_parse(&job.scope, lease).await?;
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

/// Persistent claims fence stale workers; each pass scans beyond the first page.
pub fn spawn_office_parse_scanner(state: AppState, parser: OfficeParserClient) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(5));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let repository = state.knowledge_repository();
            let mut after: Option<OfficeParseCursor> = None;
            loop {
                match repository.office_parse_candidates(after, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for job in candidates {
                            after = Some(OfficeParseCursor {
                                created_at: job.created_at,
                                job_id: job.job_id,
                            });
                            let slot = match parser.reserve_execution().await {
                                Ok(slot) => slot,
                                Err(error) => {
                                    warn!(code = ?error.code, "Office parser capacity unavailable");
                                    break;
                                }
                            };
                            let lease = match repository
                                .claim_office_parse(
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
                                    warn!(code = ?error.code, "Office job claim deferred");
                                    continue;
                                }
                            };
                            let state = state.clone();
                            let parser = parser.clone();
                            tokio::spawn(async move {
                                let work = tokio::spawn(dispatch_claimed_office_parse_job(
                                    state, parser, job, lease, slot,
                                ));
                                match work.await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => {
                                        warn!(code = ?error.code, "Office job deferred")
                                    }
                                    Err(_) => {
                                        warn!("Office parser job panicked; lease recovery pending")
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
                        warn!(code = ?error.code, "Office job discovery deferred");
                        break;
                    }
                }
            }
        }
    });
}
