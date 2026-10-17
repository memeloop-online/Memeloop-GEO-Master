//! Saved-evidence interpretation revisions. These never replace a measurement
//! outcome, increase its denominator, or authorize provider cleanup.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    AppError, ChannelAttempt, ChannelJobRepository, ChannelOutcomeStatus, ChannelTargetInput,
    ChannelTargetView, ObservationCaptureRepository, ObservationCaptureSource, TenantScope,
    sha256_hex,
};

pub const MAX_ANALYSIS_CANDIDATE_BYTES: usize = 150_000;
pub const MAX_ANALYSIS_ANSWER_BYTES: usize = 100_000;
pub const MAX_ANALYSIS_AUDIT_BYTES: usize = 512_000;
pub const MAX_ANALYSIS_PROJECTION_ATTEMPTS: usize = 1000;

/// Interpretation provenance is supplementary evidence, never a live receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SavedAnalysisProvenance {
    pub revision_id: Uuid,
    pub source: ObservationAnalysisSource,
    pub source_sha256: String,
    pub observed_at: DateTime<Utc>,
    pub analyzed_at: DateTime<Utc>,
    pub actual_model: String,
    pub config_revision: Option<i64>,
    pub prompt_version: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EffectiveObservation {
    pub raw_answer: String,
    pub citations: Vec<String>,
    pub observed_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub provenance: Option<SavedAnalysisProvenance>,
}

/// Repository selection checks persisted acceptance and time only. The authenticated
/// grounder owns semantics; consumers additionally validate source binding.
pub fn observation_analysis_projection_eligible(
    revision: &ObservationAnalysisRevision,
    as_of: DateTime<Utc>,
) -> bool {
    revision.state == ObservationAnalysisState::Completed
        && revision.created_at <= as_of
        && revision.request.observed_at <= as_of
        && revision.analyzed_at.is_some_and(|at| at <= as_of)
        && revision.result.as_ref().is_some_and(|result| {
            matches!(result.outcome, ObservationAnalysisOutcome::Grounded { .. })
        })
}

/// Select one interpretation for the current attempt, never union revisions.
/// `analyses` must have passed scoped source/digest validation at the reader.
/// The audit checks below verify the existing grounder's acceptance contract;
/// they do not interpret any provider-specific fields.
pub fn effective_observation(
    view: &ChannelTargetView,
    analyses: &[ObservationAnalysisRevision],
    as_of: DateTime<Utc>,
    is_trusted_search: impl Fn(&ChannelTargetView, &ChannelAttempt) -> bool,
) -> Option<EffectiveObservation> {
    let attempt = view.attempts.last()?;
    let outcome = attempt.outcome.as_ref()?;
    let received_at = attempt.received_at.filter(|at| *at <= as_of)?;
    let ChannelTargetInput::Measure {
        provider,
        model,
        surface,
        search_mode,
        protocol_version,
        market,
        language,
        ..
    } = &view.target.input
    else {
        return None;
    };
    let expected_protocol = serde_json::json!({
        "provider":provider,"model":model,"surface":surface,"search_mode":search_mode,
        "protocol_version":protocol_version,"market":market,"language":language
    });
    let selected = analyses
        .iter()
        .filter(|revision| {
            if revision.request.target_id != view.target.target_id
                || revision.request.attempt_id != attempt.attempt_id
                || !observation_analysis_projection_eligible(revision, as_of)
                || revision.request.observed_at < attempt.claimed_at
                || revision.request.observed_at > received_at
            {
                return false;
            }
            let result = revision.result.as_ref().expect("eligible result");
            let ObservationAnalysisOutcome::Grounded { audit, .. } = &result.outcome else {
                return false;
            };
            result.validate().is_ok()
                && audit["kind"] == "observation_extraction"
                && audit["method"] == "llm_grounded"
                && audit["prompt_version"] == revision.request.prompt_version
                && audit["source_sha256"] == revision.request.source_sha256
                && audit["protocol"] == expected_protocol
                && ["completion", "search_activity", "answer_segment"]
                    .iter()
                    .all(|role| {
                        audit["refs"]
                            .as_array()
                            .is_some_and(|refs| refs.iter().any(|entry| entry["role"] == *role))
                    })
        })
        .max_by_key(|revision| (revision.created_at, revision.request.revision_id));
    if let Some(revision) = selected {
        let result = revision.result.as_ref()?;
        let ObservationAnalysisOutcome::Grounded {
            raw_answer,
            citations,
            ..
        } = &result.outcome
        else {
            return None;
        };
        return Some(EffectiveObservation {
            raw_answer: raw_answer.clone(),
            citations: citations.clone(),
            observed_at: revision.request.observed_at,
            received_at,
            provenance: Some(SavedAnalysisProvenance {
                revision_id: revision.request.revision_id,
                source: revision.request.source.clone(),
                source_sha256: revision.request.source_sha256.clone(),
                observed_at: revision.request.observed_at,
                analyzed_at: revision.analyzed_at?,
                actual_model: result.actual_model.clone()?,
                config_revision: result.config_revision,
                prompt_version: revision.request.prompt_version.clone(),
                parser_version: revision.request.parser_version.clone(),
            }),
        });
    }
    (outcome.status == ChannelOutcomeStatus::Observed
        && !outcome.fixture
        && outcome.occurred_at <= as_of
        && is_trusted_search(view, attempt))
    .then(|| EffectiveObservation {
        raw_answer: outcome.raw_answer.clone().unwrap_or_default(),
        citations: outcome.citations.clone(),
        observed_at: outcome.occurred_at,
        received_at,
        provenance: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationAnalysisSource {
    Capture { capture_id: Uuid },
    AttemptEvidence { evidence_index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationAnalysisRequest {
    pub revision_id: Uuid,
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    pub source: ObservationAnalysisSource,
    pub source_sha256: String,
    pub observed_at: DateTime<Utc>,
    pub prompt_version: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationAnalysisState {
    Queued,
    Running,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationAnalysisOutcome {
    Grounded {
        raw_answer: String,
        citations: Vec<String>,
        /// Pointer/quote audit produced by the deterministic grounding check,
        /// not an unrestricted explanation supplied by the model.
        audit: Value,
    },
    Unverified {
        reason: String,
    },
    Failed {
        code: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationAnalysisResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_revision: Option<i64>,
    pub actual_model: Option<String>,
    pub candidate_json: Option<String>,
    pub outcome: ObservationAnalysisOutcome,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationAnalysisRevision {
    pub request: ObservationAnalysisRequest,
    pub request_digest: String,
    pub state: ObservationAnalysisState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub analyzed_at: Option<DateTime<Utc>>,
    pub result: Option<ObservationAnalysisResult>,
    /// A completed response (or proof no request was sent), not lease expiry.
    pub inference_resolved: bool,
}

impl ObservationAnalysisRevision {
    pub fn blocks_automatic_admission(&self) -> bool {
        self.state != ObservationAnalysisState::Completed
            || (!self.inference_resolved
                && !self.result.as_ref().is_some_and(|result| {
                    matches!(
                        result.outcome,
                        ObservationAnalysisOutcome::Grounded { .. }
                            | ObservationAnalysisOutcome::Unverified { .. }
                    )
                }))
    }
}

/// Explicitly shared by the two memory repositories. Lock before either
/// repository's records; never hold this across inference.
#[derive(Clone, Default)]
pub struct MemoryObservationAdmissionGate(pub(crate) Arc<Mutex<()>>);

fn bounded_label(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn fixed_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

impl ObservationAnalysisRequest {
    pub fn validate(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        created_at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        if self.revision_id.is_nil()
            || self.target_id.is_nil()
            || self.attempt_id.is_nil()
            || matches!(self.source, ObservationAnalysisSource::Capture { capture_id } if capture_id.is_nil())
            || !digest(&self.source_sha256)
            || !digest(request_digest)
            || !bounded_label(idempotency_key, 256)
            || !bounded_label(&self.prompt_version, 128)
            || !bounded_label(&self.parser_version, 128)
            || self.observed_at > created_at
        {
            return Err(AppError::invalid_request(
                "invalid observation analysis request",
            ));
        }
        Ok(())
    }

    /// Generated revision IDs and acceptance timestamps do not change the
    /// meaning of an idempotent submission.
    pub fn same_input(&self, other: &Self) -> bool {
        let mut normalized = other.clone();
        normalized.revision_id = self.revision_id;
        self == &normalized
    }
}

impl ObservationAnalysisResult {
    pub fn validate(&self) -> Result<(), AppError> {
        let invalid = || AppError::invalid_request("invalid observation analysis result");
        if self
            .actual_model
            .as_ref()
            .is_some_and(|model| !bounded_label(model, 256))
            || self.config_revision.is_some_and(|revision| revision < 0)
            || self.candidate_json.as_ref().is_some_and(|candidate| {
                candidate.len() > MAX_ANALYSIS_CANDIDATE_BYTES
                    || serde_json::from_str::<Value>(candidate).is_err()
            })
            || (!matches!(self.outcome, ObservationAnalysisOutcome::Failed { .. })
                && self.actual_model.is_none())
        {
            return Err(invalid());
        }
        match &self.outcome {
            ObservationAnalysisOutcome::Grounded {
                raw_answer,
                citations,
                audit,
            } => {
                if raw_answer.trim().is_empty()
                    || raw_answer.len() > MAX_ANALYSIS_ANSWER_BYTES
                    || citations.len() > 50
                    || citations.iter().any(|citation| {
                        citation.len() > 2048
                            || !url::Url::parse(citation).is_ok_and(|url| {
                                matches!(url.scheme(), "https" | "http")
                                    && url.host_str().is_some()
                                    && url.username().is_empty()
                                    && url.password().is_none()
                            })
                    })
                    || !audit.is_object()
                    || !audit["refs"]
                        .as_array()
                        .is_some_and(|refs| !refs.is_empty())
                    || serde_json::to_vec(audit).map_err(|_| invalid())?.len()
                        > MAX_ANALYSIS_AUDIT_BYTES
                    || self.candidate_json.is_none()
                {
                    return Err(invalid());
                }
            }
            ObservationAnalysisOutcome::Unverified { reason } if !fixed_code(reason) => {
                return Err(invalid());
            }
            ObservationAnalysisOutcome::Failed { code } if !fixed_code(code) => {
                return Err(invalid());
            }
            _ => {}
        }
        Ok(())
    }
}

/// Resolve only recognized saved evidence. This checks byte integrity and
/// ownership, not answer semantics. Callers perform interpretation and grounding
/// separately and must not infer cleanup permission from this return value.
pub fn observation_analysis_source_json(
    scope: &TenantScope,
    request: &ObservationAnalysisRequest,
    target: &ChannelTargetView,
    capture: Option<&ObservationCaptureSource>,
) -> Result<String, AppError> {
    if scope.project_id.is_none() {
        return Err(AppError::forbidden("project scope required"));
    }
    if target.target.target_id != request.target_id
        || !matches!(target.target.input, ChannelTargetInput::Measure { .. })
    {
        return Err(AppError::forbidden("analysis measurement target mismatch"));
    }
    let attempt = target
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == request.attempt_id)
        .ok_or_else(|| AppError::not_found("analysis measurement attempt not found"))?;
    let outcome = attempt
        .outcome
        .as_ref()
        .ok_or_else(|| AppError::conflict("measurement attempt has not finished"))?;
    let received_at = attempt
        .received_at
        .ok_or_else(|| AppError::conflict("measurement receipt unavailable"))?;
    if attempt.claimed_at > request.observed_at || request.observed_at > received_at {
        return Err(AppError::conflict(
            "analysis source provenance or time mismatch",
        ));
    }
    let (source_json, source_sha256, observed_at) = match request.source {
        ObservationAnalysisSource::Capture { capture_id } => {
            let capture = capture
                .filter(|capture| {
                    capture.capture_id == capture_id
                        && capture.target_id == request.target_id
                        && capture.attempt_id == request.attempt_id
                        && capture.account_id == target.target.input.account_id()
                })
                .ok_or_else(|| AppError::not_found("analysis source capture not found"))?;
            capture.validate(scope)?;
            // An unavailable runner response is conservatively marked fixture
            // by dispatch. Only an independently persisted, identity-bound
            // checkpoint may recover interpretation from an explicitly unknown
            // receipt. Failure-detail strings identify a stage, not provenance:
            // saved-source eligibility must not depend on which stage timed out.
            // Missing, conflicting, fixture or injected receipts are not proof.
            let unknown_receipt = |evidence: &Value| {
                evidence["kind"] == "runner_receipt"
                    && evidence["schema_version"] == "geo.runner.receipt.v1"
                    && evidence["provenance"] == "unknown"
            };
            let receipt_allows_saved_source = outcome.status == ChannelOutcomeStatus::Unknown
                && outcome.runner_evidence.iter().any(unknown_receipt)
                && outcome.runner_evidence.iter().all(|evidence| {
                    evidence["kind"] != "runner_receipt" || unknown_receipt(evidence)
                });
            if outcome.runner_evidence.iter().any(|evidence| {
                evidence["kind"] == "runner_receipt"
                    && (matches!(
                        evidence["provenance"].as_str(),
                        Some("fixture" | "injected")
                    ) || evidence["connector_version"]
                        .as_str()
                        .is_some_and(|version| version.starts_with("fixture")))
            }) {
                return Err(AppError::conflict("analysis source provenance unavailable"));
            }
            if outcome.fixture
                && (!receipt_allows_saved_source
                    || !capture.original_identity.as_ref().is_some_and(|identity| {
                    matches!(&target.target.input, ChannelTargetInput::Measure { provider, .. }
                        if identity.provider == *provider && !identity.platform_account_id.is_empty())
                }))
            {
                return Err(AppError::conflict("analysis source provenance unavailable"));
            }
            (
                capture.source_json.clone(),
                capture.source_sha256.clone(),
                capture.observed_at,
            )
        }
        ObservationAnalysisSource::AttemptEvidence { evidence_index } => {
            if outcome.fixture {
                return Err(AppError::conflict("analysis source provenance unavailable"));
            }
            let evidence = outcome
                .runner_evidence
                .get(evidence_index as usize)
                .ok_or_else(|| AppError::not_found("analysis source evidence not found"))?;
            let source_capture = evidence["kind"] == "observation_capture"
                && evidence["schema_version"] == "geo.observation.capture.v1"
                && evidence["phase"] == "source";
            let extraction_audit = evidence["kind"] == "observation_extraction"
                && evidence["method"] == "llm_grounded";
            if !source_capture && !extraction_audit {
                return Err(AppError::invalid_request(
                    "unsupported saved observation evidence",
                ));
            }
            let source_json = evidence["source_json"]
                .as_str()
                .ok_or_else(|| AppError::invalid_request("saved observation source missing"))?;
            let source_sha256 = evidence["source_sha256"]
                .as_str()
                .ok_or_else(|| AppError::invalid_request("saved observation digest missing"))?;
            // An audit without its own observation timestamp remains bound to
            // the original attempt outcome time, never the replay time.
            let observed_at = match evidence.get("observed_at") {
                Some(value) => serde_json::from_value::<DateTime<Utc>>(value.clone())
                    .map_err(|_| AppError::invalid_request("saved observation time invalid"))?,
                None => outcome.occurred_at,
            };
            (
                source_json.to_owned(),
                source_sha256.to_owned(),
                observed_at,
            )
        }
    };
    if source_json.len() > crate::MAX_OBSERVATION_SOURCE_BYTES
        || sha256_hex(source_json.as_bytes()) != source_sha256
        || source_sha256 != request.source_sha256
        || observed_at != request.observed_at
    {
        return Err(AppError::conflict(
            "analysis source digest or time mismatch",
        ));
    }
    let document: Value = serde_json::from_str(&source_json)
        .map_err(|_| AppError::invalid_request("saved observation document invalid"))?;
    if !super::observation_capture::valid_observation_source_document(&document) {
        return Err(AppError::invalid_request(
            "saved observation document invalid",
        ));
    }
    Ok(source_json)
}

#[async_trait]
pub trait ObservationAnalysisRepository: Send + Sync {
    fn memory_admission_gate(&self) -> Option<MemoryObservationAdmissionGate> {
        None
    }
    /// Internal integration hook; persistent repositories implement their own
    /// transaction rather than attempting a cross-repository memory operation.
    fn memory_repository(&self) -> Option<&MemoryObservationAnalysisRepository> {
        None
    }
    async fn has_unresolved_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<bool, AppError>;
    /// Atomically save a deterministic interpretation of an already persisted
    /// response. Retries must match the source request, digest and result; there
    /// is no inference claim or partially written running revision to recover.
    async fn project_response(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        result: ObservationAnalysisResult,
        analyzed_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError>;
    /// At most one latest completed grounded revision per scoped target/attempt,
    /// selected by creation time and ID descending; returned in target/attempt
    /// order. Later failures never erase grounded work.
    /// Empty batches are allowed; nonempty batches contain at most 1000 pairs.
    async fn latest_grounded_for_attempts(
        &self,
        scope: &TenantScope,
        attempts: &[(Uuid, Uuid)],
        as_of: DateTime<Utc>,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError>;
    async fn get(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Option<ObservationAnalysisRevision>, AppError>;
    async fn list_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        after_revision_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError>;
}

pub type SharedObservationAnalysisRepository = Arc<dyn ObservationAnalysisRepository>;

struct StoredAnalysis {
    scope: TenantScope,
    key_hash: String,
    revision: ObservationAnalysisRevision,
    claim_token: Option<Uuid>,
    durable_linked: bool,
}

pub struct MemoryObservationAnalysisRepository {
    jobs: Arc<dyn ChannelJobRepository>,
    captures: Arc<dyn ObservationCaptureRepository>,
    records: Mutex<HashMap<Uuid, StoredAnalysis>>,
    admission_gate: MemoryObservationAdmissionGate,
}

impl MemoryObservationAnalysisRepository {
    pub fn new(
        jobs: Arc<dyn ChannelJobRepository>,
        captures: Arc<dyn ObservationCaptureRepository>,
    ) -> Self {
        Self {
            jobs,
            captures,
            records: Mutex::new(HashMap::new()),
            admission_gate: MemoryObservationAdmissionGate::default(),
        }
    }
    pub fn with_admission_gate(mut self, gate: MemoryObservationAdmissionGate) -> Self {
        self.admission_gate = gate;
        self
    }

    /// Caller owns the shared admission gate for the entire admission.
    pub(crate) async fn create_gate_held(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        created_at: DateTime<Utc>,
    ) -> Result<(ObservationAnalysisRevision, bool), AppError> {
        request.validate(scope, idempotency_key, request_digest, created_at)?;
        let target = self.jobs.get_target(scope, request.target_id).await?;
        let capture = match request.source {
            ObservationAnalysisSource::Capture { capture_id } => {
                self.captures.get_source(scope, capture_id).await?
            }
            ObservationAnalysisSource::AttemptEvidence { .. } => None,
        };
        observation_analysis_source_json(scope, &request, &target, capture.as_ref())?;
        let key_hash = sha256_hex(idempotency_key.as_bytes());
        let mut records = self.records.lock().await;
        if let Some(prior) = records
            .values()
            .find(|stored| stored.scope == *scope && stored.key_hash == key_hash)
        {
            return if prior.revision.request_digest == request_digest
                && prior.revision.request.same_input(&request)
            {
                Ok((prior.revision.clone(), false))
            } else {
                Err(AppError::conflict("analysis idempotency request differs"))
            };
        }
        if records.contains_key(&request.revision_id) {
            return Err(AppError::conflict(
                "analysis revision identity already exists",
            ));
        }
        let revision = ObservationAnalysisRevision {
            request,
            request_digest: request_digest.into(),
            state: ObservationAnalysisState::Queued,
            created_at,
            started_at: None,
            analyzed_at: None,
            result: None,
            inference_resolved: false,
        };
        records.insert(
            revision.request.revision_id,
            StoredAnalysis {
                scope: scope.clone(),
                key_hash,
                revision: revision.clone(),
                claim_token: None,
                durable_linked: true,
            },
        );
        Ok((revision, true))
    }

    /// Called only after the extraction transition has been validated on a
    /// clone, while the common admission gate remains held.
    pub(crate) async fn sync_linked_gate_held(
        &self,
        scope: &TenantScope,
        intent: &crate::ObservationExtractionIntent,
        token: Uuid,
        failure: Option<(ObservationAnalysisResult, bool)>,
    ) -> Result<(), AppError> {
        use crate::ObservationExtractionState as State;
        let Some(id) = intent.request.manual_revision_id else {
            return Ok(());
        };
        let mut records = self.records.lock().await;
        let stored = records
            .get_mut(&id)
            .filter(|stored| stored.scope == *scope && stored.durable_linked)
            .ok_or_else(|| AppError::conflict("linked analysis revision missing"))?;
        if crate::ObservationExtractionRequest::for_manual(&stored.revision.request)
            != intent.request
        {
            return Err(AppError::conflict("linked analysis binding differs"));
        }
        if stored.claim_token.is_some_and(|prior| prior != token) || token.is_nil() {
            return Err(AppError::conflict("linked analysis claim differs"));
        }
        let revision = &mut stored.revision;
        match intent.state {
            State::Running => {
                if revision.state != ObservationAnalysisState::Queued {
                    return Err(AppError::conflict("linked analysis not queued"));
                }
                revision.state = ObservationAnalysisState::Running;
                revision.started_at = intent.started_at;
                stored.claim_token = Some(token);
            }
            State::ResponseReceived => {
                if revision.state != ObservationAnalysisState::Running {
                    return Err(AppError::conflict("linked analysis not running"));
                }
                revision.inference_resolved = true;
            }
            State::Completed | State::Interrupted => {
                let (mut result, resolved) = if intent.state == State::Completed {
                    (
                        intent
                            .completion
                            .clone()
                            .ok_or_else(|| AppError::conflict("extraction completion missing"))?,
                        true,
                    )
                } else {
                    failure.ok_or_else(|| AppError::conflict("extraction failure missing"))?
                };
                if intent.state == State::Interrupted
                    && let Some(response) = &intent.response
                {
                    result.actual_model = Some(response.actual_model.clone());
                    result.config_revision = response.config_revision;
                    result.prompt_tokens = response.prompt_tokens;
                    result.completion_tokens = response.completion_tokens;
                }
                result.validate()?;
                if revision.state == ObservationAnalysisState::Completed {
                    if revision.result.as_ref() == Some(&result) {
                        return Ok(());
                    }
                    return Err(AppError::conflict("linked analysis result differs"));
                }
                if revision.state != ObservationAnalysisState::Running {
                    return Err(AppError::conflict("linked analysis not running"));
                }
                if intent.state == State::Completed {
                    let target = self
                        .jobs
                        .get_target(scope, revision.request.target_id)
                        .await?;
                    let capture = match revision.request.source {
                        ObservationAnalysisSource::Capture { capture_id } => {
                            self.captures.get_source(scope, capture_id).await?
                        }
                        ObservationAnalysisSource::AttemptEvidence { .. } => None,
                    };
                    observation_analysis_source_json(
                        scope,
                        &revision.request,
                        &target,
                        capture.as_ref(),
                    )?;
                }
                revision.result = Some(result);
                revision.state = ObservationAnalysisState::Completed;
                revision.analyzed_at = intent.finished_at;
                revision.inference_resolved |= resolved || intent.response.is_some();
                if intent.state == State::Interrupted {
                    stored.claim_token = Some(Uuid::new_v4());
                }
            }
            State::Queued => return Err(AppError::conflict("invalid linked analysis transition")),
        }
        Ok(())
    }
}

fn require_project(scope: &TenantScope) -> Result<(), AppError> {
    if scope.project_id.is_none() {
        Err(AppError::forbidden("project scope required"))
    } else {
        Ok(())
    }
}

#[async_trait]
impl ObservationAnalysisRepository for MemoryObservationAnalysisRepository {
    fn memory_repository(&self) -> Option<&MemoryObservationAnalysisRepository> {
        Some(self)
    }
    fn memory_admission_gate(&self) -> Option<MemoryObservationAdmissionGate> {
        Some(self.admission_gate.clone())
    }
    async fn has_unresolved_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<bool, AppError> {
        require_project(scope)?;
        Ok(self.records.lock().await.values().any(|stored| {
            stored.scope == *scope
                && stored.revision.request.target_id == target_id
                && stored.revision.request.attempt_id == attempt_id
                && stored.revision.blocks_automatic_admission()
        }))
    }
    async fn project_response(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        result: ObservationAnalysisResult,
        analyzed_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError> {
        let _admission = self.admission_gate.0.lock().await;
        request.validate(scope, idempotency_key, request_digest, analyzed_at)?;
        result.validate()?;
        let target = self.jobs.get_target(scope, request.target_id).await?;
        let capture = match request.source {
            ObservationAnalysisSource::Capture { capture_id } => {
                self.captures.get_source(scope, capture_id).await?
            }
            ObservationAnalysisSource::AttemptEvidence { .. } => None,
        };
        observation_analysis_source_json(scope, &request, &target, capture.as_ref())?;
        let key_hash = sha256_hex(idempotency_key.as_bytes());
        let mut records = self.records.lock().await;
        if let Some(prior) = records
            .values()
            .find(|stored| stored.scope == *scope && stored.key_hash == key_hash)
        {
            return if prior.revision.request_digest == request_digest
                && prior.revision.request.same_input(&request)
                && prior.revision.state == ObservationAnalysisState::Completed
                && prior.revision.result.as_ref() == Some(&result)
            {
                Ok(prior.revision.clone())
            } else {
                Err(AppError::conflict("analysis projection already differs"))
            };
        }
        if records.contains_key(&request.revision_id) {
            return Err(AppError::conflict(
                "analysis revision identity already exists",
            ));
        }
        let revision = ObservationAnalysisRevision {
            request,
            request_digest: request_digest.into(),
            state: ObservationAnalysisState::Completed,
            created_at: analyzed_at,
            started_at: Some(analyzed_at),
            analyzed_at: Some(analyzed_at),
            result: Some(result),
            inference_resolved: true,
        };
        records.insert(
            revision.request.revision_id,
            StoredAnalysis {
                scope: scope.clone(),
                key_hash,
                revision: revision.clone(),
                claim_token: None,
                durable_linked: false,
            },
        );
        Ok(revision)
    }
    async fn latest_grounded_for_attempts(
        &self,
        scope: &TenantScope,
        attempts: &[(Uuid, Uuid)],
        as_of: DateTime<Utc>,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError> {
        require_project(scope)?;
        if attempts.len() > MAX_ANALYSIS_PROJECTION_ATTEMPTS
            || attempts
                .iter()
                .any(|(target, attempt)| target.is_nil() || attempt.is_nil())
        {
            return Err(AppError::invalid_request(
                "invalid analysis projection batch",
            ));
        }
        let requested: std::collections::HashSet<_> = attempts.iter().copied().collect();
        let records = self.records.lock().await;
        let mut selected: std::collections::BTreeMap<_, &ObservationAnalysisRevision> =
            std::collections::BTreeMap::new();
        for stored in records.values().filter(|stored| stored.scope == *scope) {
            let revision = &stored.revision;
            let key = (revision.request.target_id, revision.request.attempt_id);
            if requested.contains(&key)
                && observation_analysis_projection_eligible(revision, as_of)
                && selected.get(&key).is_none_or(|prior| {
                    (revision.created_at, revision.request.revision_id)
                        > (prior.created_at, prior.request.revision_id)
                })
            {
                selected.insert(key, revision);
            }
        }
        Ok(selected.into_values().cloned().collect())
    }
    async fn get(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Option<ObservationAnalysisRevision>, AppError> {
        require_project(scope)?;
        Ok(self
            .records
            .lock()
            .await
            .get(&revision_id)
            .filter(|stored| stored.scope == *scope)
            .map(|stored| stored.revision.clone()))
    }

    async fn list_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        after_revision_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError> {
        require_project(scope)?;
        if !(1..=100).contains(&limit) {
            return Err(AppError::invalid_request("invalid analysis page size"));
        }
        let records = self.records.lock().await;
        let matches = |stored: &&StoredAnalysis| {
            stored.scope == *scope
                && stored.revision.request.target_id == target_id
                && stored.revision.request.attempt_id == attempt_id
        };
        if after_revision_id
            .is_some_and(|id| records.get(&id).is_none_or(|stored| !matches(&stored)))
        {
            return Err(AppError::invalid_request("invalid analysis cursor"));
        }
        let cursor = after_revision_id.map(|id| {
            let revision = &records[&id].revision;
            (revision.created_at, revision.request.revision_id)
        });
        let mut revisions: Vec<_> = records
            .values()
            .filter(matches)
            .filter(|stored| {
                cursor.is_none_or(|cursor| {
                    (
                        stored.revision.created_at,
                        stored.revision.request.revision_id,
                    ) < cursor
                })
            })
            .map(|stored| stored.revision.clone())
            .collect();
        revisions.sort_by_key(|revision| {
            std::cmp::Reverse((revision.created_at, revision.request.revision_id))
        });
        revisions.truncate(limit);
        Ok(revisions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ErrorCode, MemoryChannelJobRepository,
        MemoryObservationCaptureRepository, ObservationCapture, ObservationCaptureInput,
        ObservationCaptureSnapshot, StandaloneMeasurementPlan,
    };
    use chrono::Duration;
    use serde_json::json;

    fn observation_analysis_source_json(
        scope: &TenantScope,
        request: &ObservationAnalysisRequest,
        target: &ChannelTargetView,
        capture: Option<&ObservationCapture>,
    ) -> Result<String, AppError> {
        let source = capture
            .map(ObservationCaptureSource::try_from)
            .transpose()?;
        super::observation_analysis_source_json(scope, request, target, source.as_ref())
    }

    struct Fixture {
        scope: TenantScope,
        jobs: Arc<MemoryChannelJobRepository>,
        captures: Arc<MemoryObservationCaptureRepository>,
        store: MemoryObservationAnalysisRepository,
        request: ObservationAnalysisRequest,
        source_capture_id: Uuid,
        now: DateTime<Utc>,
    }

    async fn fixture() -> Fixture {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let jobs = Arc::new(MemoryChannelJobRepository::default());
        let captures = Arc::new(MemoryObservationCaptureRepository::new(jobs.clone()));
        let (target_id, attempt_id, account_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let source_json = r#"{"messages":[{"content":"Synthetic saved answer"}]}"#.to_owned();
        let source_sha256 = sha256_hex(source_json.as_bytes());
        let observed_at = now + Duration::seconds(1);
        jobs.create_measurement_plan(
            &scope,
            "synthetic-plan",
            "synthetic-digest",
            StandaloneMeasurementPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                title: "Synthetic".into(),
                input_hash: "synthetic-digest".into(),
                revision: 1,
                created_at: now,
                targets: vec![ChannelTarget {
                    target_id,
                    input: ChannelTargetInput::Measure {
                        account_id,
                        provider: "synthetic".into(),
                        model: "synthetic".into(),
                        surface: "consumer_web".into(),
                        search_mode: "web_search".into(),
                        protocol_version: "v1".into(),
                        question_set_version: "adhoc".into(),
                        question: "Synthetic question?".into(),
                        market: "global".into(),
                        language: "en".into(),
                        scheduled_at: now,
                        sample_ordinal: 0,
                        question_binding: None,
                    },
                }],
            },
        )
        .await
        .unwrap();
        jobs.claim(&scope, target_id, attempt_id, now)
            .await
            .unwrap();
        let source_capture_id = Uuid::new_v4();
        captures
            .save(
                &scope,
                ObservationCaptureInput {
                    capture_id: source_capture_id,
                    target_id,
                    attempt_id,
                    account_id,
                    runner_session_id: Uuid::new_v4(),
                    original_identity: None,
                    ordinal: 0,
                    observed_at,
                    snapshot: ObservationCaptureSnapshot::Source {
                        source_json: source_json.clone(),
                        source_sha256: source_sha256.clone(),
                    },
                    owned_conversation: None,
                    completion: None,
                },
            )
            .await
            .unwrap();
        jobs.finish(&scope, target_id, attempt_id, ChannelOutcome {
            status: ChannelOutcomeStatus::Unknown, detail: Some("interpretation unavailable".into()),
            occurred_at: observed_at, raw_answer: None, citations: vec![], public_url: None,
            screenshot_ref: None, connector_version: Some("synthetic-live".into()), fixture: false,
            runner_evidence: vec![json!({
                "kind":"observation_capture","schema_version":"geo.observation.capture.v1",
                "phase":"source","source_json":source_json,"source_sha256":source_sha256,"observed_at":observed_at,
            })],
        }, now + Duration::seconds(2)).await.unwrap();
        let store = MemoryObservationAnalysisRepository::new(jobs.clone(), captures.clone());
        Fixture {
            scope,
            jobs,
            captures,
            store,
            source_capture_id,
            now: now + Duration::seconds(3),
            request: ObservationAnalysisRequest {
                revision_id: Uuid::new_v4(),
                target_id,
                attempt_id,
                source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
                source_sha256,
                observed_at,
                prompt_version: "extract.v1".into(),
                parser_version: "ground.v1".into(),
            },
        }
    }

    fn result() -> ObservationAnalysisResult {
        ObservationAnalysisResult {
            config_revision: None,
            actual_model: Some("synthetic".into()),
            candidate_json: Some(r#"{"decision":"unverified"}"#.into()),
            outcome: ObservationAnalysisOutcome::Unverified {
                reason: "model_unverified".into(),
            },
            prompt_tokens: 3,
            completion_tokens: 1,
        }
    }

    #[tokio::test]
    async fn known_failed_response_is_resolved_only_with_checkpoint() {
        let f = fixture().await;
        let mut revision = f
            .store
            .project_response(
                &f.scope,
                "classification",
                &sha256_hex(b"classification"),
                f.request,
                result(),
                f.now,
            )
            .await
            .unwrap();
        revision.state = ObservationAnalysisState::Completed;
        for code in [
            "grounding_unavailable",
            "analysis_result_invalid",
            "model_timeout",
            "analysis_interrupted",
        ] {
            let mut failed = result();
            failed.outcome = ObservationAnalysisOutcome::Failed { code: code.into() };
            revision.result = Some(failed);
            revision.inference_resolved = false;
            assert!(revision.blocks_automatic_admission());
            revision.inference_resolved = true;
            assert!(!revision.blocks_automatic_admission());
        }
        revision.inference_resolved = false;
        revision.result = Some(result());
        assert!(!revision.blocks_automatic_admission());
    }

    fn automatic_request(f: &Fixture) -> crate::ObservationExtractionRequest {
        crate::ObservationExtractionRequest {
            target_id: f.request.target_id,
            attempt_id: f.request.attempt_id,
            source_capture_id: Some(f.source_capture_id),
            source_evidence_index: None,
            manual_revision_id: None,
            source_sha256: f.request.source_sha256.clone(),
            prompt_version: "extract.v2".into(),
            parser_version: "ground.v2".into(),
            policy_version: "policy.v1".into(),
        }
    }

    #[tokio::test]
    async fn durable_manual_sources_replay_lifecycle_and_late_write_fencing() {
        use crate::{
            ObservationExtractionRepository, ObservationExtractionResponse,
            ObservationExtractionResponseDisposition, ObservationExtractionState,
        };
        for capture_source in [false, true] {
            let f = fixture().await;
            let gate = MemoryObservationAdmissionGate::default();
            let analyses = Arc::new(f.store.with_admission_gate(gate.clone()));
            let extraction = crate::MemoryObservationExtractionRepository::new(f.captures.clone())
                .with_automatic_admission(gate, analyses.clone());
            let mut request = f.request.clone();
            if capture_source {
                request.source = ObservationAnalysisSource::Capture {
                    capture_id: f.source_capture_id,
                };
            }
            let digest = sha256_hex(b"manual-source");
            let admitted = extraction
                .admit_manual(&f.scope, "manual", &digest, request.clone(), f.now)
                .await
                .unwrap();
            let intent = admitted.intent;
            assert_eq!(intent.request.manual_revision_id, Some(request.revision_id));
            let mut replay_request = request.clone();
            replay_request.revision_id = Uuid::new_v4();
            let replay = extraction
                .admit_manual(&f.scope, "manual", &digest, replay_request.clone(), f.now)
                .await
                .unwrap();
            assert_eq!(replay.intent, intent);
            assert_eq!(replay.revision, admitted.revision);
            assert!(
                extraction
                    .admit_manual(
                        &f.scope,
                        "manual",
                        &sha256_hex(b"different"),
                        replay_request.clone(),
                        f.now
                    )
                    .await
                    .is_err()
            );
            let later = f.now + Duration::seconds(600);
            assert_eq!(
                analyses
                    .get(&f.scope, request.revision_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                ObservationAnalysisState::Queued
            );
            let claim = extraction
                .claim(&f.scope, intent.intent_id, later)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                analyses
                    .get(&f.scope, request.revision_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                ObservationAnalysisState::Running
            );
            extraction
                .record_response(
                    &f.scope,
                    intent.intent_id,
                    claim.claim_token,
                    ObservationExtractionResponse {
                        model_text: "{}".into(),
                        disposition: ObservationExtractionResponseDisposition::Complete,
                        actual_model: "synthetic".into(),
                        config_revision: None,
                        prompt_tokens: 3,
                        completion_tokens: 1,
                    },
                    later,
                )
                .await
                .unwrap();
            assert!(
                analyses
                    .get(&f.scope, request.revision_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .inference_resolved
            );
            let checkpoint = extraction
                .claim_response(&f.scope, intent.intent_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(checkpoint.claim_token, claim.claim_token);
            assert!(
                extraction
                    .claim(&f.scope, intent.intent_id, later)
                    .await
                    .unwrap()
                    .is_none()
            );
            let completed = extraction
                .complete(
                    &f.scope,
                    intent.intent_id,
                    checkpoint.claim_token,
                    result(),
                    later,
                )
                .await
                .unwrap();
            assert_eq!(completed.state, ObservationExtractionState::Completed);
            let revision = analyses
                .get(&f.scope, request.revision_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(revision.result, Some(result()));
            assert_eq!(revision.state, ObservationAnalysisState::Completed);
            assert_eq!(
                analyses
                    .list_for_attempt(&f.scope, request.target_id, request.attempt_id, None, 100)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            let next = extraction
                .admit_manual(&f.scope, "explicit-next", &digest, replay_request, later)
                .await
                .unwrap();
            assert_ne!(next.intent.intent_id, intent.intent_id);
            let foreign = TenantScope::new(
                f.scope.operator_id,
                Uuid::new_v4().into(),
                f.scope.project_id,
            );
            assert!(
                extraction
                    .get(&foreign, intent.intent_id)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                extraction
                    .admit_manual(&foreign, "manual", &digest, request, later)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn durable_manual_and_automatic_admission_race() {
        use crate::{AutomaticExtractionAdmission, ObservationExtractionRepository};
        let f = fixture().await;
        let auto = automatic_request(&f);
        let gate = MemoryObservationAdmissionGate::default();
        let analyses = Arc::new(f.store.with_admission_gate(gate.clone()));
        let extraction = crate::MemoryObservationExtractionRepository::new(f.captures)
            .with_automatic_admission(gate, analyses.clone());
        let digest = sha256_hex(b"manual");
        let mut request = f.request.clone();
        request.revision_id = Uuid::new_v4();
        request.source = ObservationAnalysisSource::Capture {
            capture_id: f.source_capture_id,
        };
        let (manual, automatic) = tokio::join!(
            extraction.admit_manual(&f.scope, "explicit", &digest, request.clone(), f.now),
            extraction.admit_automatic(&f.scope, auto.clone(), f.now),
        );
        let manual = manual.unwrap().intent;
        match automatic.unwrap() {
            AutomaticExtractionAdmission::Intent(_) => {}
            AutomaticExtractionAdmission::DeferredManual => {}
        }
        // Once admitted, automatic work reuses the earliest durable intent.
        let AutomaticExtractionAdmission::Intent(reused) = extraction
            .admit_automatic(&f.scope, auto, f.now)
            .await
            .unwrap()
        else {
            panic!("expected reuse")
        };
        assert!(
            reused.intent_id == manual.intent_id || reused.request.manual_revision_id.is_none()
        );
    }

    #[tokio::test]
    async fn durable_manual_generation_does_not_adopt_existing_automatic_work() {
        use crate::{AutomaticExtractionAdmission, ObservationExtractionRepository};
        let f = fixture().await;
        let auto_request = automatic_request(&f);
        let gate = MemoryObservationAdmissionGate::default();
        let analyses = Arc::new(f.store.with_admission_gate(gate.clone()));
        let extraction = crate::MemoryObservationExtractionRepository::new(f.captures)
            .with_automatic_admission(gate, analyses);
        let AutomaticExtractionAdmission::Intent(auto) = extraction
            .admit_automatic(&f.scope, auto_request.clone(), f.now)
            .await
            .unwrap()
        else {
            panic!("expected automatic intent")
        };
        let mut request = f.request;
        request.source = ObservationAnalysisSource::Capture {
            capture_id: f.source_capture_id,
        };
        let manual = extraction
            .admit_manual(
                &f.scope,
                "explicit-generation",
                &sha256_hex(b"explicit"),
                request,
                f.now + Duration::seconds(1),
            )
            .await
            .unwrap()
            .intent;
        assert_ne!(manual.intent_id, auto.intent_id);
        assert_ne!(manual.intent_key, auto.intent_key);
        let AutomaticExtractionAdmission::Intent(reused) = extraction
            .admit_automatic(&f.scope, auto_request, f.now + Duration::seconds(2))
            .await
            .unwrap()
        else {
            panic!("expected earliest reuse")
        };
        assert_eq!(reused.intent_id, auto.intent_id);
    }

    #[tokio::test]
    async fn durable_manual_interruption_preserves_reason_and_completion_evidence() {
        use crate::{
            ObservationExtractionInterruption, ObservationExtractionRepository,
            ObservationExtractionResponse, ObservationExtractionResponseDisposition,
        };
        for (response_received, known_finished) in [(false, false), (false, true), (true, false)] {
            let f = fixture().await;
            let gate = MemoryObservationAdmissionGate::default();
            let analyses = Arc::new(f.store.with_admission_gate(gate.clone()));
            let extraction = crate::MemoryObservationExtractionRepository::new(f.captures)
                .with_automatic_admission(gate, analyses.clone());
            let admitted = extraction
                .admit_manual(
                    &f.scope,
                    "manual",
                    &sha256_hex(b"manual"),
                    f.request.clone(),
                    f.now,
                )
                .await
                .unwrap();
            let intent = admitted.intent;
            let claim = extraction
                .claim(&f.scope, intent.intent_id, f.now)
                .await
                .unwrap()
                .unwrap();
            if response_received {
                extraction
                    .record_response(
                        &f.scope,
                        intent.intent_id,
                        claim.claim_token,
                        ObservationExtractionResponse {
                            model_text: "{}".into(),
                            disposition: ObservationExtractionResponseDisposition::Complete,
                            actual_model: "synthetic".into(),
                            config_revision: Some(2),
                            prompt_tokens: 5,
                            completion_tokens: 2,
                        },
                        f.now,
                    )
                    .await
                    .unwrap();
            }
            let failure = ObservationAnalysisResult {
                config_revision: None,
                actual_model: None,
                candidate_json: None,
                outcome: ObservationAnalysisOutcome::Failed {
                    code: "model_timeout".into(),
                },
                prompt_tokens: 0,
                completion_tokens: 0,
            };
            extraction
                .interrupt_with_result(
                    &f.scope,
                    intent.intent_id,
                    claim.claim_token,
                    ObservationExtractionInterruption::InferenceInterrupted,
                    failure,
                    known_finished,
                    f.now,
                )
                .await
                .unwrap();
            let revision = analyses
                .get(&f.scope, f.request.revision_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                revision.inference_resolved,
                response_received || known_finished
            );
            let recorded = revision.result.unwrap();
            assert_eq!(
                recorded.outcome,
                ObservationAnalysisOutcome::Failed {
                    code: "model_timeout".into()
                }
            );
            assert_eq!(recorded.actual_model.is_some(), response_received);
            if response_received {
                assert_eq!((recorded.prompt_tokens, recorded.completion_tokens), (5, 2));
            }
            assert!(
                extraction
                    .complete(
                        &f.scope,
                        intent.intent_id,
                        claim.claim_token,
                        result(),
                        f.now
                    )
                    .await
                    .is_err()
            );
            assert!(
                extraction
                    .claim_response(&f.scope, intent.intent_id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn automatic_admission_shares_versions_and_preserves_terminal_intent() {
        use crate::{AutomaticExtractionAdmission as Admission, ObservationExtractionRepository};
        let f = fixture().await;
        let request = automatic_request(&f);
        let gate = MemoryObservationAdmissionGate::default();
        let analyses = Arc::new(f.store.with_admission_gate(gate.clone()));
        let extraction = crate::MemoryObservationExtractionRepository::new(f.captures)
            .with_automatic_admission(gate, analyses.clone());
        let mut newer = request.clone();
        newer.prompt_version = "extract.v3".into();
        newer.parser_version = "ground.v3".into();
        let (a, b) = tokio::join!(
            extraction.admit_automatic(&f.scope, request.clone(), f.now),
            extraction.admit_automatic(&f.scope, newer.clone(), f.now),
        );
        let Admission::Intent(intent) = a.unwrap() else {
            panic!("unexpected deferral")
        };
        assert_eq!(b.unwrap(), Admission::Intent(intent.clone()));
        // Explicit later manual work remains allowed.
        extraction
            .admit_manual(
                &f.scope,
                "later-manual",
                &sha256_hex(b"manual"),
                f.request,
                f.now,
            )
            .await
            .unwrap();
        let claim = extraction
            .claim(&f.scope, intent.intent_id, f.now)
            .await
            .unwrap()
            .unwrap();
        let terminal = extraction
            .interrupt(
                &f.scope,
                intent.intent_id,
                claim.claim_token,
                crate::ObservationExtractionInterruption::InferenceInterrupted,
                f.now,
            )
            .await
            .unwrap();
        assert_eq!(
            extraction
                .admit_automatic(&f.scope, newer, f.now)
                .await
                .unwrap(),
            Admission::Intent(Box::new(terminal))
        );
        assert!(
            extraction
                .claim(&f.scope, intent.intent_id, f.now)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn automatic_admission_rejects_independent_memory_gates() {
        use crate::ObservationExtractionRepository;
        let f = fixture().await;
        let request = automatic_request(&f);
        let extraction = crate::MemoryObservationExtractionRepository::new(f.captures)
            .with_automatic_admission(MemoryObservationAdmissionGate::default(), Arc::new(f.store));
        assert!(
            extraction
                .admit_automatic(&f.scope, request, f.now)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn received_response_projection_is_atomic_write_once_and_source_scoped() {
        let f = fixture().await;
        let digest = sha256_hex(b"received-response");
        let key = "automatic:observation-extraction:synthetic";
        let (a, b) = tokio::join!(
            f.store
                .project_response(&f.scope, key, &digest, f.request.clone(), result(), f.now),
            f.store.project_response(
                &f.scope,
                key,
                &digest,
                f.request.clone(),
                result(),
                f.now + Duration::seconds(1)
            )
        );
        let saved = a.unwrap();
        assert_eq!(saved, b.unwrap());
        assert_eq!(saved.state, ObservationAnalysisState::Completed);
        assert_eq!(saved.result, Some(result()));
        assert_eq!(
            f.store
                .list_for_attempt(
                    &f.scope,
                    f.request.target_id,
                    f.request.attempt_id,
                    None,
                    20
                )
                .await
                .unwrap()
                .len(),
            1
        );
        let mut changed = result();
        changed.completion_tokens += 1;
        assert!(
            f.store
                .project_response(&f.scope, key, &digest, f.request.clone(), changed, f.now)
                .await
                .is_err()
        );
        assert!(
            f.store
                .project_response(
                    &f.scope,
                    key,
                    &sha256_hex(b"different"),
                    f.request.clone(),
                    result(),
                    f.now
                )
                .await
                .is_err()
        );
        let other = TenantScope::new(
            f.scope.operator_id,
            f.scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert!(
            f.store
                .project_response(&other, key, &digest, f.request.clone(), result(), f.now)
                .await
                .is_err()
        );
        let mut invalid = f.request.clone();
        invalid.source_sha256 = sha256_hex(b"wrong-source");
        assert!(
            f.store
                .project_response(&f.scope, "other", &digest, invalid, result(), f.now)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn projection_batch_selects_latest_grounded_as_of_with_scope_and_bounds() {
        let f = fixture().await;
        let hash = sha256_hex(b"projection");
        let mut ids = Vec::new();
        for ordinal in 0..3 {
            let mut request = f.request.clone();
            request.revision_id = Uuid::new_v4();
            ids.push(request.revision_id);
            let at = f.now + Duration::seconds(ordinal);
            let mut completion = result();
            if ordinal < 2 {
                completion.outcome = ObservationAnalysisOutcome::Grounded {
                    raw_answer: "Synthetic saved answer".into(),
                    citations: vec![],
                    audit: json!({"refs":[{"role":"answer_source"}]}),
                };
            }
            f.store
                .project_response(
                    &f.scope,
                    &format!("projection-{ordinal}"),
                    &hash,
                    request,
                    completion,
                    at,
                )
                .await
                .unwrap();
        }
        let pairs = [(f.request.target_id, f.request.attempt_id); 2];
        let latest = f
            .store
            .latest_grounded_for_attempts(&f.scope, &pairs, f.now + Duration::seconds(3))
            .await
            .unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].request.revision_id, ids[1]);
        let early = f
            .store
            .latest_grounded_for_attempts(&f.scope, &pairs, f.now)
            .await
            .unwrap();
        assert_eq!(early[0].request.revision_id, ids[0]);
        assert!(
            f.store
                .latest_grounded_for_attempts(&f.scope, &pairs, f.now - Duration::seconds(1))
                .await
                .unwrap()
                .is_empty()
        );
        let mut other = f.scope.clone();
        other.project_id = Some(Uuid::new_v4().into());
        assert!(
            f.store
                .latest_grounded_for_attempts(&other, &pairs, f.now)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            f.store
                .latest_grounded_for_attempts(&f.scope, &[], f.now)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            f.store
                .latest_grounded_for_attempts(
                    &f.scope,
                    &vec![pairs[0]; MAX_ANALYSIS_PROJECTION_ATTEMPTS + 1],
                    f.now
                )
                .await
                .is_err()
        );
        assert!(
            f.store
                .latest_grounded_for_attempts(&f.scope, &[(Uuid::nil(), pairs[0].1)], f.now)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn analysis_scoped_reads_writes_and_cursor_pages() {
        let f = fixture().await;
        let hash = sha256_hex(b"synthetic-request");
        f.store
            .project_response(&f.scope, "first", &hash, f.request.clone(), result(), f.now)
            .await
            .unwrap();
        let mut other = f.scope.clone();
        other.tenant_id = Uuid::new_v4().into();
        assert!(
            f.store
                .get(&other, f.request.revision_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .project_response(&other, "first", &hash, f.request.clone(), result(), f.now)
                .await
                .is_err()
        );
        let mut projectless = f.scope.clone();
        projectless.project_id = None;
        assert_eq!(
            f.store
                .get(&projectless, f.request.revision_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
        let mut next = f.request.clone();
        next.revision_id = Uuid::new_v4();
        f.store
            .project_response(&f.scope, "second", &hash, next, result(), f.now)
            .await
            .unwrap();
        let first_page = f
            .store
            .list_for_attempt(&f.scope, f.request.target_id, f.request.attempt_id, None, 1)
            .await
            .unwrap();
        let next_page = f
            .store
            .list_for_attempt(
                &f.scope,
                f.request.target_id,
                f.request.attempt_id,
                Some(first_page[0].request.revision_id),
                1,
            )
            .await
            .unwrap();
        assert_eq!(next_page.len(), 1);
        assert_ne!(
            next_page[0].request.revision_id,
            first_page[0].request.revision_id
        );
        assert!(
            f.store
                .list_for_attempt(
                    &other,
                    f.request.target_id,
                    f.request.attempt_id,
                    Some(first_page[0].request.revision_id),
                    1
                )
                .await
                .is_err()
        );
        assert!(
            f.store
                .list_for_attempt(&f.scope, f.request.target_id, f.request.attempt_id, None, 0)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn analysis_replays_exact_redaction_manifest_and_rejects_tampering() {
        let f = fixture().await;
        let target = f
            .jobs
            .get_target(&f.scope, f.request.target_id)
            .await
            .unwrap();
        let manifest = json!({
            "schema_version": "geo.observation.source-redaction.v1",
            "policy": "reasoning-and-private-fields",
            "omitted_paths": ["/messages/0/reasoning"]
        });
        for omission in [
            json!(["/messages/0/reasoning"]),
            json!(["/messages/0/content"]),
            json!(["/messages/1/reasoning"]),
            json!(["/messages/0/invalid~2"]),
        ] {
            let valid = omission == manifest["omitted_paths"];
            let mut document = json!({
                "messages": [{"content": "Synthetic saved answer"}],
                "source_redaction": manifest
            });
            document["source_redaction"]["omitted_paths"] = omission;
            let raw = document.to_string();
            let digest = sha256_hex(raw.as_bytes());
            let mut request = f.request.clone();
            request.source_sha256 = digest.clone();
            let mut replay_target = target.clone();
            let evidence = &mut replay_target.attempts[0]
                .outcome
                .as_mut()
                .unwrap()
                .runner_evidence[0];
            evidence["source_json"] = json!(raw);
            evidence["source_sha256"] = json!(digest);
            let replay = observation_analysis_source_json(&f.scope, &request, &replay_target, None);
            assert_eq!(replay.is_ok(), valid);
            if valid {
                assert_eq!(replay.unwrap(), raw);
                replay_target.attempts[0]
                    .outcome
                    .as_mut()
                    .unwrap()
                    .runner_evidence[0]["source_json"] =
                    json!(raw.replace("/messages/0/reasoning", "/messages/0/thinking"));
                assert!(
                    observation_analysis_source_json(&f.scope, &request, &replay_target, None)
                        .is_err()
                );
            }
            let mut capture = f
                .captures
                .get(&f.scope, f.source_capture_id)
                .await
                .unwrap()
                .unwrap();
            capture.input.snapshot = ObservationCaptureSnapshot::Source {
                source_json: raw,
                source_sha256: digest,
            };
            request.source = ObservationAnalysisSource::Capture {
                capture_id: f.source_capture_id,
            };
            if valid {
                capture.receipt.digest_sha256 = capture.input.validate(&f.scope).unwrap();
                assert!(
                    observation_analysis_source_json(&f.scope, &request, &target, Some(&capture))
                        .is_ok()
                );
            } else {
                assert!(capture.input.validate(&f.scope).is_err());
                assert!(
                    observation_analysis_source_json(&f.scope, &request, &target, Some(&capture))
                        .is_err()
                );
            }
        }
    }

    #[tokio::test]
    async fn analysis_sources_reject_missing_tampered_or_rebound_bytes() {
        let f = fixture().await;
        let hash = sha256_hex(b"synthetic-request");
        let mut capture_request = f.request.clone();
        capture_request.source = ObservationAnalysisSource::Capture {
            capture_id: f.source_capture_id,
        };
        f.store
            .project_response(
                &f.scope,
                "capture",
                &hash,
                capture_request.clone(),
                result(),
                f.now,
            )
            .await
            .unwrap();
        let target = f
            .jobs
            .get_target(&f.scope, f.request.target_id)
            .await
            .unwrap();
        let mut capture = f
            .captures
            .get(&f.scope, f.source_capture_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            observation_analysis_source_json(&f.scope, &capture_request, &target, Some(&capture))
                .is_ok()
        );
        let ObservationCaptureSnapshot::Source { source_json, .. } = &mut capture.input.snapshot
        else {
            unreachable!()
        };
        source_json.push(' ');
        assert!(
            observation_analysis_source_json(&f.scope, &capture_request, &target, Some(&capture))
                .is_err()
        );
        let mut wrong_digest = f.request.clone();
        wrong_digest.revision_id = Uuid::new_v4();
        wrong_digest.source_sha256 = "0".repeat(64);
        assert!(
            f.store
                .project_response(&f.scope, "tamper", &hash, wrong_digest, result(), f.now)
                .await
                .is_err()
        );
        let mut missing = f.request.clone();
        missing.source = ObservationAnalysisSource::AttemptEvidence { evidence_index: 99 };
        assert!(
            f.store
                .project_response(&f.scope, "missing", &hash, missing, result(), f.now)
                .await
                .is_err()
        );
        let mut tampered = target.clone();
        tampered.attempts[0]
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence[0]["source_json"] = json!(r#"{"messages":[]}"#);
        assert!(observation_analysis_source_json(&f.scope, &f.request, &tampered, None).is_err());
        let mut fixture_target = target.clone();
        fixture_target.attempts[0].outcome.as_mut().unwrap().fixture = true;
        assert!(
            observation_analysis_source_json(&f.scope, &f.request, &fixture_target, None).is_err()
        );
        let mut audit_target = target.clone();
        let evidence = &mut audit_target.attempts[0]
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence[0];
        evidence["kind"] = json!("observation_extraction");
        evidence["method"] = json!("llm_grounded");
        evidence.as_object_mut().unwrap().remove("observed_at");
        assert!(
            observation_analysis_source_json(&f.scope, &f.request, &audit_target, None).is_ok()
        );
    }

    #[tokio::test]
    async fn unknown_receipt_allows_only_independently_bound_source_capture() {
        let f = fixture().await;
        let mut request = f.request.clone();
        request.source = ObservationAnalysisSource::Capture {
            capture_id: f.source_capture_id,
        };
        let mut target = f
            .jobs
            .get_target(&f.scope, request.target_id)
            .await
            .unwrap();
        let outcome = target.attempts[0].outcome.as_mut().unwrap();
        outcome.fixture = true;
        outcome.status = ChannelOutcomeStatus::Unknown;
        outcome.detail = Some("execution_deadline".into());
        outcome.runner_evidence = vec![json!({
            "kind":"runner_receipt",
            "schema_version":"geo.runner.receipt.v1",
            "provenance":"unknown",
            "execution_id":Uuid::new_v4(),
            "connector_version":"synthetic.v1",
            "occurred_at":null,
        })];
        let mut capture = f
            .captures
            .get(&f.scope, f.source_capture_id)
            .await
            .unwrap()
            .unwrap();
        capture.input.original_identity = Some(crate::ObservationProviderIdentity {
            provider: "synthetic".into(),
            platform_account_id: "synthetic-account".into(),
        });
        let ObservationCaptureSnapshot::Source {
            source_json,
            source_sha256,
        } = &mut capture.input.snapshot
        else {
            unreachable!();
        };
        let mut document: Value = serde_json::from_str(source_json).unwrap();
        document["capture_status"] = json!({
            "schema_version": "geo.observation.capture-status.v1",
            "transport": "partial",
            "termination": "timeout"
        });
        *source_json = document.to_string();
        *source_sha256 = sha256_hex(source_json.as_bytes());
        request.source_sha256 = source_sha256.clone();
        assert!(capture.input.owned_conversation.is_none());
        assert!(capture.input.completion.is_none());
        capture.receipt.digest_sha256 = capture.input.validate(&f.scope).unwrap();
        let retained_capture = capture.clone();
        for detail in [
            Some("execution_deadline"),
            Some("official_search_observation_unverified"),
            Some("another_interpretation_stage_failed"),
            None,
        ] {
            target.attempts[0].outcome.as_mut().unwrap().detail = detail.map(str::to_owned);
            let original = target.clone();
            assert!(
                observation_analysis_source_json(&f.scope, &request, &target, Some(&capture))
                    .is_ok(),
                "{detail:?}",
            );
            assert_eq!(target, original);
            assert_eq!(capture, retained_capture);
            assert!(!capture.has_complete_conversation_evidence(&f.scope));
        }
        assert!(observation_analysis_source_json(&f.scope, &f.request, &target, None).is_err());
        for provenance in ["fixture", "injected", "live", ""] {
            let mut rejected = target.clone();
            rejected.attempts[0]
                .outcome
                .as_mut()
                .unwrap()
                .runner_evidence[0]["provenance"] = json!(provenance);
            assert!(
                observation_analysis_source_json(&f.scope, &request, &rejected, Some(&capture))
                    .is_err()
            );
        }
        let mut malformed = target.clone();
        malformed.attempts[0]
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence[0]["schema_version"] = json!("unrecognized");
        assert!(
            observation_analysis_source_json(&f.scope, &request, &malformed, Some(&capture))
                .is_err()
        );
        let mut missing_receipt = target.clone();
        missing_receipt.attempts[0]
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence
            .clear();
        assert!(
            observation_analysis_source_json(&f.scope, &request, &missing_receipt, Some(&capture))
                .is_err()
        );
        let mut conflicting = target.clone();
        conflicting.attempts[0].outcome.as_mut().unwrap().runner_evidence.push(json!({
            "kind":"runner_receipt","schema_version":"geo.runner.receipt.v1","provenance":"fixture"
        }));
        for fixture in [true, false] {
            conflicting.attempts[0].outcome.as_mut().unwrap().fixture = fixture;
            assert!(
                observation_analysis_source_json(&f.scope, &request, &conflicting, Some(&capture))
                    .is_err()
            );
        }
        let mut not_unknown = target.clone();
        let mut fixture_connector = target.clone();
        fixture_connector.attempts[0]
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence[0]["connector_version"] = json!("fixture-v1");
        assert!(
            observation_analysis_source_json(
                &f.scope,
                &request,
                &fixture_connector,
                Some(&capture)
            )
            .is_err()
        );
        not_unknown.attempts[0].outcome.as_mut().unwrap().status = ChannelOutcomeStatus::Observed;
        assert!(
            observation_analysis_source_json(&f.scope, &request, &not_unknown, Some(&capture))
                .is_err()
        );
        for field in [
            "capture", "account", "attempt", "target", "provider", "receipt",
        ] {
            let mut rebound = capture.clone();
            match field {
                "capture" => rebound.input.capture_id = Uuid::new_v4(),
                "account" => rebound.input.account_id = Uuid::new_v4(),
                "attempt" => rebound.input.attempt_id = Uuid::new_v4(),
                "target" => rebound.input.target_id = Uuid::new_v4(),
                "provider" => {
                    rebound.input.original_identity.as_mut().unwrap().provider = "other".into()
                }
                "receipt" => rebound.receipt.capture_id = Uuid::new_v4(),
                _ => unreachable!(),
            }
            rebound.receipt.digest_sha256 = rebound.input.validate(&f.scope).unwrap();
            assert!(
                observation_analysis_source_json(&f.scope, &request, &target, Some(&rebound))
                    .is_err(),
                "{field}"
            );
        }
        for other in [
            TenantScope::new(Uuid::new_v4().into(), f.scope.tenant_id, f.scope.project_id),
            TenantScope::new(
                f.scope.operator_id,
                Uuid::new_v4().into(),
                f.scope.project_id,
            ),
            TenantScope::new(
                f.scope.operator_id,
                f.scope.tenant_id,
                Some(Uuid::new_v4().into()),
            ),
        ] {
            // Scope ownership belongs to repository reads; captures do not
            // embed tenant IDs and their content digest is not a scope token.
            assert!(
                f.store
                    .project_response(
                        &other,
                        "foreign-capture",
                        &sha256_hex(b"foreign"),
                        request.clone(),
                        result(),
                        f.now
                    )
                    .await
                    .is_err()
            );
        }
        for observed_at in [
            target.attempts[0].claimed_at - Duration::seconds(1),
            target.attempts[0].received_at.unwrap() + Duration::seconds(1),
        ] {
            let mut mistimed = request.clone();
            mistimed.observed_at = observed_at;
            assert!(
                observation_analysis_source_json(&f.scope, &mistimed, &target, Some(&capture))
                    .is_err()
            );
        }
        capture.input.original_identity = None;
        capture.receipt.digest_sha256 = capture.input.validate(&f.scope).unwrap();
        assert!(
            observation_analysis_source_json(&f.scope, &request, &target, Some(&capture)).is_err()
        );
    }

    #[test]
    fn analysis_result_bounds_and_model_provenance() {
        let mut valid = result();
        assert!(valid.validate().is_ok());
        valid.actual_model = None;
        assert!(valid.validate().is_err());
        valid.outcome = ObservationAnalysisOutcome::Failed {
            code: "model_unavailable".into(),
        };
        assert!(valid.validate().is_ok());
        valid.candidate_json = Some("x".repeat(MAX_ANALYSIS_CANDIDATE_BYTES + 1));
        assert!(valid.validate().is_err());
        let mut grounded = result();
        grounded.outcome = ObservationAnalysisOutcome::Grounded {
            raw_answer: "Synthetic saved answer".into(),
            citations: vec!["https://example.org/source".into()],
            audit: json!({"refs":[{"path":"/messages/0/content","role":"answer_source"}]}),
        };
        assert!(grounded.validate().is_ok());
        if let ObservationAnalysisOutcome::Grounded { citations, .. } = &mut grounded.outcome {
            citations[0] = "https://secret@example.org/".into();
        }
        assert!(grounded.validate().is_err());
        let mut too_large = result();
        too_large.outcome = ObservationAnalysisOutcome::Grounded {
            raw_answer: "x".repeat(MAX_ANALYSIS_ANSWER_BYTES + 1),
            citations: vec![],
            audit: json!({"refs":[{}]}),
        };
        assert!(too_large.validate().is_err());
    }

    #[test]
    fn recommendation_audit_survives_result_round_trip() {
        let recommendations = json!({
            "version": "geo.recommendations.v1",
            "status": "verified",
            "items": [
            {
                "name": "Example Air 100",
                "kind": "product",
                "mention_order": 1,
                "rank": 1,
                "reason": "quiet operation",
                "evidence_quote": "First choice: Example Air 100 for quiet operation.",
                "citation_urls": ["https://example.org/product"]
            },
            {
                "name": "Example Kitchen",
                "kind": "restaurant",
                "mention_order": 2,
                "rank": null,
                "reason": null,
                "evidence_quote": "Also consider Example Kitchen.",
                "citation_urls": []
            }
            ]
        });
        let mut grounded = result();
        grounded.outcome = ObservationAnalysisOutcome::Grounded {
            raw_answer:
                "First choice: Example Air 100 for quiet operation. Also consider Example Kitchen."
                    .into(),
            citations: vec!["https://example.org/product".into()],
            audit: json!({
                "refs": [{"path": "/messages/0/content", "role": "answer_source"}],
                "prompt_version": "geo.observation.extract.v4",
                "recommendations": recommendations
            }),
        };
        grounded.validate().unwrap();
        let encoded = serde_json::to_vec(&grounded).unwrap();
        let restored: ObservationAnalysisResult = serde_json::from_slice(&encoded).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, grounded);
        let ObservationAnalysisOutcome::Grounded { audit, .. } = restored.outcome else {
            panic!("grounded result was not preserved");
        };
        assert_eq!(audit["recommendations"], recommendations);
        assert!(audit["recommendations"]["items"][1]["rank"].is_null());
    }

    #[test]
    fn expanded_audit_budget_accepts_exact_boundary_without_truncation() {
        let mut grounded = result();
        let mut audit = json!({"refs":[{}],"padding":""});
        let overhead = serde_json::to_vec(&audit).unwrap().len();
        audit["padding"] = json!("x".repeat(MAX_ANALYSIS_AUDIT_BYTES - overhead));
        assert_eq!(serde_json::to_vec(&audit).unwrap().len(), 512_000);
        grounded.outcome = ObservationAnalysisOutcome::Grounded {
            raw_answer: "Synthetic answer".into(),
            citations: vec![],
            audit,
        };
        assert!(grounded.validate().is_ok());
        if let ObservationAnalysisOutcome::Grounded { audit, .. } = &mut grounded.outcome {
            audit["padding"] = json!("x".repeat(MAX_ANALYSIS_AUDIT_BYTES - overhead + 1));
        }
        assert!(grounded.validate().is_err());
    }
}
