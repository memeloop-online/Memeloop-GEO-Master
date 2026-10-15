//! One durable API inference shared by live and background interpretation.
//! No state in this module authorizes browser resubmission or remote cleanup.
use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    AppError, ObservationAnalysisResult, ObservationCapture, ObservationCaptureRepository,
    ObservationCaptureSnapshot, TenantScope, sha256_hex,
};

pub const MAX_EXTRACTION_MODEL_TEXT_BYTES: usize = 2_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationExtractionRequest {
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_capture_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_evidence_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_revision_id: Option<Uuid>,
    pub source_sha256: String,
    pub prompt_version: String,
    pub parser_version: String,
    pub policy_version: String,
}

fn label(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

pub fn validate_observation_extraction_scope(scope: &TenantScope) -> Result<(), AppError> {
    if scope.operator_id.as_uuid().is_nil()
        || scope.tenant_id.as_uuid().is_nil()
        || scope.project_id.is_none_or(|id| id.as_uuid().is_nil())
    {
        return Err(AppError::forbidden("extraction project scope required"));
    }
    Ok(())
}

impl ObservationExtractionRequest {
    pub fn for_manual(request: &crate::ObservationAnalysisRequest) -> Self {
        let (source_capture_id, source_evidence_index) = match request.source {
            crate::ObservationAnalysisSource::Capture { capture_id } => (Some(capture_id), None),
            crate::ObservationAnalysisSource::AttemptEvidence { evidence_index } => {
                (None, Some(evidence_index))
            }
        };
        Self {
            target_id: request.target_id,
            attempt_id: request.attempt_id,
            source_capture_id,
            source_evidence_index,
            manual_revision_id: Some(request.revision_id),
            source_sha256: request.source_sha256.clone(),
            prompt_version: request.prompt_version.clone(),
            parser_version: request.parser_version.clone(),
            policy_version: "geo.observation.automatic.v1".into(),
        }
    }
    /// Canonical versioned identity includes the full tenant and source binding,
    /// so equal source bytes from different attempts never share an inference.
    pub fn intent_key(&self, scope: &TenantScope) -> Result<String, AppError> {
        validate_observation_extraction_scope(scope)?;
        if self.target_id.is_nil()
            || self.attempt_id.is_nil()
            || self.source_capture_id.is_some_and(|id| id.is_nil())
            || self.source_capture_id.is_some() == self.source_evidence_index.is_some()
            || self.manual_revision_id.is_some_and(|id| id.is_nil())
            || (self.manual_revision_id.is_none() && self.source_capture_id.is_none())
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || [
                &self.prompt_version,
                &self.parser_version,
                &self.policy_version,
            ]
            .iter()
            .any(|v| !label(v, 128))
        {
            return Err(AppError::invalid_request("invalid extraction binding"));
        }
        let bytes = serde_json::to_vec(&("observation-extraction-intent.v1", scope, self))
            .map_err(|_| AppError::invalid_request("invalid extraction binding"))?;
        Ok(sha256_hex(&bytes))
    }

    /// The capture must have been fetched under the requested authoritative scope.
    /// Capture storage already binds its target, attempt and account. An active
    /// measurement may enqueue before its eventual receipt has been persisted.
    pub fn validate_source(
        &self,
        scope: &TenantScope,
        capture: &ObservationCapture,
    ) -> Result<(), AppError> {
        self.intent_key(scope)?;
        let ObservationCaptureSnapshot::Source {
            source_json,
            source_sha256,
        } = &capture.input.snapshot
        else {
            return Err(AppError::conflict("extraction requires source capture"));
        };
        if Some(capture.input.capture_id) != self.source_capture_id
            || Some(capture.receipt.capture_id) != self.source_capture_id
            || capture.input.target_id != self.target_id
            || capture.input.attempt_id != self.attempt_id
            || capture.receipt.schema_version != 1
            || capture.input.ordinal != 0
            || capture.input.validate(scope)? != capture.receipt.digest_sha256
            || source_sha256 != &self.source_sha256
            || sha256_hex(source_json.as_bytes()) != self.source_sha256
        {
            return Err(AppError::conflict("extraction source binding differs"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationExtractionState {
    Queued,
    Running,
    ResponseReceived,
    Completed,
    Interrupted,
}

impl ObservationExtractionState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::ResponseReceived)
    }
}

/// Public task status contains no source binding, model response or claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationExtractionStatus {
    pub state: ObservationExtractionState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub interruption: Option<ObservationExtractionInterruption>,
}

impl From<&ObservationExtractionIntent> for ObservationExtractionStatus {
    fn from(intent: &ObservationExtractionIntent) -> Self {
        Self {
            state: intent.state,
            created_at: intent.created_at,
            started_at: intent.started_at,
            finished_at: intent.finished_at,
            interruption: intent.interruption,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationExtractionInterruption {
    InferenceInterrupted,
    InferenceFailed,
    GroundingInterrupted,
    ResponseNotPersistable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationExtractionResponseDisposition {
    Complete,
    Incomplete,
    ToolCalls,
    UnexpectedFinish,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationExtractionResponse {
    /// Exact UTF-8 model text, including malformed JSON. Never truncate or repair.
    pub model_text: String,
    pub disposition: ObservationExtractionResponseDisposition,
    pub actual_model: String,
    pub config_revision: Option<i64>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl ObservationExtractionResponse {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.model_text.len() > MAX_EXTRACTION_MODEL_TEXT_BYTES
            || self.model_text.contains('\0')
            || !label(&self.actual_model, 256)
            || self.config_revision.is_some_and(|v| v < 0)
        {
            return Err(AppError::invalid_request("invalid extraction response"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationExtractionIntent {
    pub intent_id: Uuid,
    pub request: ObservationExtractionRequest,
    pub intent_key: String,
    pub state: ObservationExtractionState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub response_received_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub response: Option<ObservationExtractionResponse>,
    pub completion: Option<ObservationAnalysisResult>,
    pub interruption: Option<ObservationExtractionInterruption>,
}

impl ObservationExtractionIntent {
    pub fn queued(
        request: ObservationExtractionRequest,
        intent_key: String,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            intent_id: Uuid::new_v4(),
            request,
            intent_key,
            state: ObservationExtractionState::Queued,
            created_at,
            started_at: None,
            response_received_at: None,
            finished_at: None,
            response: None,
            completion: None,
            interruption: None,
        }
    }

    pub fn record_response(
        &mut self,
        response: ObservationExtractionResponse,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        response.validate()?;
        if matches!(
            self.state,
            ObservationExtractionState::ResponseReceived | ObservationExtractionState::Completed
        ) && self.response.as_ref() == Some(&response)
        {
            return Ok(());
        }
        if self.state != ObservationExtractionState::Running
            || self.started_at.is_none_or(|started| at < started)
        {
            return Err(AppError::conflict("extraction response is not current"));
        }
        self.response = Some(response);
        self.response_received_at = Some(at);
        self.state = ObservationExtractionState::ResponseReceived;
        Ok(())
    }

    pub fn complete(
        &mut self,
        result: ObservationAnalysisResult,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        result.validate()?;
        if self.state == ObservationExtractionState::Completed
            && self.completion.as_ref() == Some(&result)
        {
            return Ok(());
        }
        let response = self
            .response
            .as_ref()
            .ok_or_else(|| AppError::conflict("extraction response required"))?;
        if self.state != ObservationExtractionState::ResponseReceived
            || self
                .response_received_at
                .is_none_or(|received| at < received)
            || result.actual_model.as_ref() != Some(&response.actual_model)
            || result.config_revision != response.config_revision
            || result.prompt_tokens != response.prompt_tokens
            || result.completion_tokens != response.completion_tokens
            || (response.disposition != ObservationExtractionResponseDisposition::Complete
                && matches!(
                    result.outcome,
                    crate::ObservationAnalysisOutcome::Grounded { .. }
                ))
            || matches!(&result.outcome, crate::ObservationAnalysisOutcome::Grounded { audit, .. }
                if audit["source_sha256"] != self.request.source_sha256
                    || audit["prompt_version"] != self.request.prompt_version)
        {
            return Err(AppError::conflict("extraction completion binding differs"));
        }
        self.completion = Some(result);
        self.finished_at = Some(at);
        self.state = ObservationExtractionState::Completed;
        Ok(())
    }

    pub fn interrupt(
        &mut self,
        reason: ObservationExtractionInterruption,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if !matches!(
            self.state,
            ObservationExtractionState::Running | ObservationExtractionState::ResponseReceived
        ) || at
            < self
                .response_received_at
                .or(self.started_at)
                .unwrap_or(self.created_at)
        {
            return Err(AppError::conflict("extraction interruption is not current"));
        }
        self.interruption = Some(reason);
        self.finished_at = Some(at);
        self.state = ObservationExtractionState::Interrupted;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationExtractionClaim {
    pub intent: ObservationExtractionIntent,
    pub claim_token: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationExtractionReadyItem {
    pub scope: TenantScope,
    pub intent_id: Uuid,
    pub state: ObservationExtractionState,
}

/// Discovery is not authorization to enqueue or perform inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationExtractionBackfillItem {
    pub scope: TenantScope,
    pub capture_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomaticExtractionAdmission {
    Intent(Box<ObservationExtractionIntent>),
    DeferredManual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualExtractionAdmission {
    pub revision: crate::ObservationAnalysisRevision,
    pub intent: ObservationExtractionIntent,
}

pub fn validate_observation_extraction_page_size(limit: usize) -> Result<(), AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request(
            "extraction scan limit must be between 1 and 100",
        ));
    }
    Ok(())
}

#[async_trait]
pub trait ObservationExtractionRepository: Send + Sync {
    async fn admit_manual(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: crate::ObservationAnalysisRequest,
        created_at: DateTime<Utc>,
    ) -> Result<ManualExtractionAdmission, AppError>;
    async fn admit_automatic(
        &self,
        scope: &TenantScope,
        request: ObservationExtractionRequest,
        created_at: DateTime<Utc>,
    ) -> Result<AutomaticExtractionAdmission, AppError>;
    /// Service-only source metadata with no extraction intent in any state or
    /// version for the exact scoped capture. Fix the stored_at cutoff across
    /// pages; UUID ordering is exclusive of `after`. Eligibility is decided by
    /// the caller, not by this scan, and concurrent enqueue remains possible.
    async fn scan_unqueued_sources(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationExtractionBackfillItem>, AppError>;
    async fn get(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<Option<ObservationExtractionIntent>, AppError>;
    /// One scoped status: newest active task, otherwise newest terminal task.
    /// Order ties by intent ID. An older active task must not be hidden by a
    /// newer terminal task; no status is inferred from elapsed time.
    async fn status_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<ObservationExtractionStatus>, AppError>;
    /// Service-only keyset discovery. Fix as_of across pages; no claim tokens or
    /// response text are returned. Running inference is never rediscovered.
    async fn scan_ready(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationExtractionReadyItem>, AppError>;
    /// Atomic queued -> running. No expiration or automatic inference reclaim.
    async fn claim(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> Result<Option<ObservationExtractionClaim>, AppError>;
    /// Only deterministic grounding may repeat. Concurrent continuations share
    /// the response claim; completion is immutable and mismatched results fail.
    /// This must never call inference, regardless of response validity.
    async fn claim_response(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<Option<ObservationExtractionClaim>, AppError>;
    async fn record_response(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        response: ObservationExtractionResponse,
        received_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError>;
    async fn complete(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        result: ObservationAnalysisResult,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError>;
    /// Explicit owner interruption fences all late writes and retains response.
    async fn interrupt(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        reason: ObservationExtractionInterruption,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError>;
    #[allow(clippy::too_many_arguments)] // Failure provenance and completion proof are independent.
    async fn interrupt_with_result(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        reason: ObservationExtractionInterruption,
        result: ObservationAnalysisResult,
        inference_resolved: bool,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError>;
}

struct Stored {
    scope: TenantScope,
    intent: ObservationExtractionIntent,
    claim_token: Option<Uuid>,
}

pub struct MemoryObservationExtractionRepository {
    captures: Arc<dyn ObservationCaptureRepository>,
    records: Mutex<HashMap<Uuid, Stored>>,
    automatic_admission: Option<(
        crate::MemoryObservationAdmissionGate,
        Arc<dyn crate::ObservationAnalysisRepository>,
    )>,
}

impl MemoryObservationExtractionRepository {
    pub fn new(captures: Arc<dyn ObservationCaptureRepository>) -> Self {
        Self {
            captures,
            records: Mutex::new(HashMap::new()),
            automatic_admission: None,
        }
    }
    pub fn with_automatic_admission(
        mut self,
        gate: crate::MemoryObservationAdmissionGate,
        analyses: Arc<dyn crate::ObservationAnalysisRepository>,
    ) -> Self {
        self.automatic_admission = Some((gate, analyses));
        self
    }
    async fn update(
        &self,
        scope: &TenantScope,
        id: Uuid,
        token: Uuid,
        apply: impl FnOnce(&mut ObservationExtractionIntent) -> Result<(), AppError>,
        fence: bool,
        failure: Option<(ObservationAnalysisResult, bool)>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        let _admission = match &self.automatic_admission {
            Some((gate, _)) => Some(gate.0.lock().await),
            None => None,
        };
        validate_observation_extraction_scope(scope)?;
        let mut records = self.records.lock().await;
        let stored = records
            .get_mut(&id)
            .filter(|stored| stored.scope == *scope)
            .ok_or_else(|| AppError::not_found("extraction intent not found"))?;
        if token.is_nil() || stored.claim_token != Some(token) {
            return Err(AppError::conflict("extraction claim is not current"));
        }
        let mut next = stored.intent.clone();
        apply(&mut next)?;
        if next.request.manual_revision_id.is_some() {
            self.automatic_admission
                .as_ref()
                .and_then(|(_, analyses)| analyses.memory_repository())
                .ok_or_else(|| AppError::conflict("manual admission is not configured"))?
                .sync_linked_gate_held(scope, &next, token, failure)
                .await?;
        }
        stored.intent = next;
        if fence {
            stored.claim_token = Some(Uuid::new_v4());
        }
        Ok(stored.intent.clone())
    }
}

#[async_trait]
impl ObservationExtractionRepository for MemoryObservationExtractionRepository {
    async fn admit_manual(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: crate::ObservationAnalysisRequest,
        created_at: DateTime<Utc>,
    ) -> Result<ManualExtractionAdmission, AppError> {
        let (gate, analyses) = self
            .automatic_admission
            .as_ref()
            .ok_or_else(|| AppError::conflict("manual admission is not configured"))?;
        if !analyses
            .memory_admission_gate()
            .is_some_and(|other| Arc::ptr_eq(&gate.0, &other.0))
        {
            return Err(AppError::conflict("manual admission gate differs"));
        }
        let analyses = analyses
            .memory_repository()
            .ok_or_else(|| AppError::conflict("manual memory admission is not configured"))?;
        let _admission = gate.0.lock().await;
        let extraction_request = ObservationExtractionRequest::for_manual(&request);
        let key = extraction_request.intent_key(scope)?;
        // Acquire both stores before the first mutation. There is no await
        // between publishing the revision and its intent, including replay.
        let mut records = self.records.lock().await;
        let (revision, inserted) = analyses
            .create_gate_held(scope, idempotency_key, request_digest, request, created_at)
            .await?;
        if !inserted {
            let intent = records
                .values()
                .find(|stored| {
                    stored.scope == *scope
                        && stored.intent.request.manual_revision_id
                            == Some(revision.request.revision_id)
                })
                .map(|stored| stored.intent.clone())
                .ok_or_else(|| AppError::conflict("manual extraction intent missing"))?;
            return Ok(ManualExtractionAdmission { revision, intent });
        }
        let intent = ObservationExtractionIntent::queued(extraction_request, key, created_at);
        records.insert(
            intent.intent_id,
            Stored {
                scope: scope.clone(),
                intent: intent.clone(),
                claim_token: None,
            },
        );
        Ok(ManualExtractionAdmission { revision, intent })
    }
    async fn admit_automatic(
        &self,
        scope: &TenantScope,
        request: ObservationExtractionRequest,
        created_at: DateTime<Utc>,
    ) -> Result<AutomaticExtractionAdmission, AppError> {
        let (gate, analyses) = self
            .automatic_admission
            .as_ref()
            .ok_or_else(|| AppError::conflict("automatic admission is not configured"))?;
        if !analyses
            .memory_admission_gate()
            .is_some_and(|other| Arc::ptr_eq(&gate.0, &other.0))
        {
            return Err(AppError::conflict("automatic admission gate differs"));
        }
        let _admission = gate.0.lock().await;
        if request.manual_revision_id.is_some() || request.source_evidence_index.is_some() {
            return Err(AppError::invalid_request(
                "automatic extraction requires capture",
            ));
        }
        let key = request.intent_key(scope)?;
        let capture = self
            .captures
            .get(
                scope,
                request
                    .source_capture_id
                    .ok_or_else(|| AppError::invalid_request("capture required"))?,
            )
            .await?
            .ok_or_else(|| AppError::not_found("extraction source not found"))?;
        request.validate_source(scope, &capture)?;
        let mut records = self.records.lock().await;
        if let Some(stored) = records
            .values()
            .filter(|stored| {
                stored.scope == *scope
                    && stored.intent.request.source_capture_id == request.source_capture_id
            })
            .min_by_key(|stored| (stored.intent.created_at, stored.intent.intent_id))
        {
            return Ok(AutomaticExtractionAdmission::Intent(Box::new(
                stored.intent.clone(),
            )));
        }
        if analyses
            .has_unresolved_for_attempt(scope, request.target_id, request.attempt_id)
            .await?
        {
            return Ok(AutomaticExtractionAdmission::DeferredManual);
        }
        let intent = ObservationExtractionIntent::queued(request, key, created_at);
        records.insert(
            intent.intent_id,
            Stored {
                scope: scope.clone(),
                intent: intent.clone(),
                claim_token: None,
            },
        );
        Ok(AutomaticExtractionAdmission::Intent(Box::new(intent)))
    }
    async fn scan_unqueued_sources(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationExtractionBackfillItem>, AppError> {
        validate_observation_extraction_page_size(limit)?;
        let mut items = Vec::with_capacity(limit);
        let mut cursor = after;
        loop {
            let sources = self.captures.scan_sources(as_of, cursor, limit).await?;
            let exhausted = sources.len() < limit;
            let records = self.records.lock().await;
            for source in sources {
                cursor = Some(source.capture_id);
                if records.values().any(|stored| {
                    stored.scope == source.scope
                        && stored.intent.request.source_capture_id == Some(source.capture_id)
                }) {
                    continue;
                }
                items.push(ObservationExtractionBackfillItem {
                    scope: source.scope,
                    capture_id: source.capture_id,
                });
                if items.len() == limit {
                    return Ok(items);
                }
            }
            if exhausted {
                return Ok(items);
            }
        }
    }
    async fn get(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<Option<ObservationExtractionIntent>, AppError> {
        validate_observation_extraction_scope(scope)?;
        Ok(self
            .records
            .lock()
            .await
            .get(&intent_id)
            .filter(|s| s.scope == *scope)
            .map(|s| s.intent.clone()))
    }
    async fn scan_ready(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationExtractionReadyItem>, AppError> {
        validate_observation_extraction_page_size(limit)?;
        let mut items: Vec<_> = self
            .records
            .lock()
            .await
            .values()
            .filter(|s| {
                s.intent.created_at <= as_of
                    && after.is_none_or(|id| s.intent.intent_id > id)
                    && matches!(
                        s.intent.state,
                        ObservationExtractionState::Queued
                            | ObservationExtractionState::ResponseReceived
                    )
            })
            .map(|s| ObservationExtractionReadyItem {
                scope: s.scope.clone(),
                intent_id: s.intent.intent_id,
                state: s.intent.state,
            })
            .collect();
        items.sort_by_key(|item| item.intent_id);
        items.truncate(limit);
        Ok(items)
    }
    async fn status_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<ObservationExtractionStatus>, AppError> {
        validate_observation_extraction_scope(scope)?;
        Ok(self
            .records
            .lock()
            .await
            .values()
            .filter(|stored| {
                stored.scope == *scope
                    && stored.intent.request.target_id == target_id
                    && stored.intent.request.attempt_id == attempt_id
            })
            .max_by_key(|stored| {
                (
                    stored.intent.state.is_active(),
                    stored.intent.created_at,
                    stored.intent.intent_id,
                )
            })
            .map(|stored| ObservationExtractionStatus::from(&stored.intent)))
    }
    async fn claim(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> Result<Option<ObservationExtractionClaim>, AppError> {
        let _admission = match &self.automatic_admission {
            Some((gate, _)) => Some(gate.0.lock().await),
            None => None,
        };
        validate_observation_extraction_scope(scope)?;
        let mut records = self.records.lock().await;
        let stored = records
            .get_mut(&intent_id)
            .filter(|s| s.scope == *scope)
            .ok_or_else(|| AppError::not_found("extraction intent not found"))?;
        if stored.intent.state != ObservationExtractionState::Queued {
            return Ok(None);
        }
        if started_at < stored.intent.created_at {
            return Err(AppError::invalid_request("invalid extraction start time"));
        }
        let mut next = stored.intent.clone();
        next.state = ObservationExtractionState::Running;
        next.started_at = Some(started_at);
        let token = Uuid::new_v4();
        if next.request.manual_revision_id.is_some() {
            self.automatic_admission
                .as_ref()
                .and_then(|(_, analyses)| analyses.memory_repository())
                .ok_or_else(|| AppError::conflict("manual admission is not configured"))?
                .sync_linked_gate_held(scope, &next, token, None)
                .await?;
        }
        stored.intent = next;
        stored.claim_token = Some(token);
        Ok(Some(ObservationExtractionClaim {
            intent: stored.intent.clone(),
            claim_token: token,
        }))
    }
    async fn claim_response(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<Option<ObservationExtractionClaim>, AppError> {
        validate_observation_extraction_scope(scope)?;
        let records = self.records.lock().await;
        let stored = records
            .get(&intent_id)
            .filter(|s| s.scope == *scope)
            .ok_or_else(|| AppError::not_found("extraction intent not found"))?;
        Ok(
            (stored.intent.state == ObservationExtractionState::ResponseReceived).then(|| {
                ObservationExtractionClaim {
                    intent: stored.intent.clone(),
                    claim_token: stored.claim_token.expect("response has claim"),
                }
            }),
        )
    }
    async fn record_response(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        response: ObservationExtractionResponse,
        received_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        self.update(
            scope,
            intent_id,
            claim_token,
            |intent| intent.record_response(response, received_at),
            false,
            None,
        )
        .await
    }
    async fn complete(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        result: ObservationAnalysisResult,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        self.update(
            scope,
            intent_id,
            claim_token,
            |intent| intent.complete(result, finished_at),
            false,
            None,
        )
        .await
    }
    async fn interrupt(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        reason: ObservationExtractionInterruption,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        let result = ObservationAnalysisResult {
            config_revision: None,
            actual_model: None,
            candidate_json: None,
            outcome: crate::ObservationAnalysisOutcome::Failed {
                code: "analysis_interrupted".into(),
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        };
        self.interrupt_with_result(
            scope,
            intent_id,
            claim_token,
            reason,
            result,
            false,
            finished_at,
        )
        .await
    }
    async fn interrupt_with_result(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        reason: ObservationExtractionInterruption,
        result: ObservationAnalysisResult,
        inference_resolved: bool,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        result.validate()?;
        if !matches!(
            result.outcome,
            crate::ObservationAnalysisOutcome::Failed { .. }
        ) {
            return Err(AppError::invalid_request("interruption requires failure"));
        }
        self.update(
            scope,
            intent_id,
            claim_token,
            |intent| intent.interrupt(reason, finished_at),
            true,
            Some((result, inference_resolved)),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MemoryChannelJobRepository, MemoryObservationCaptureRepository, ObservationAnalysisOutcome,
        ObservationCaptureInput, ObservationCaptureReceipt,
    };

    fn scope() -> TenantScope {
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        )
    }
    fn request() -> ObservationExtractionRequest {
        ObservationExtractionRequest {
            target_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            source_capture_id: Some(Uuid::new_v4()),
            source_evidence_index: None,
            manual_revision_id: None,
            source_sha256: sha256_hex(b"source"),
            prompt_version: "extract.v1".into(),
            parser_version: "ground.v1".into(),
            policy_version: "automatic.v1".into(),
        }
    }
    fn response() -> ObservationExtractionResponse {
        ObservationExtractionResponse {
            model_text: "  {invalid JSON}\n中文  ".into(),
            disposition: ObservationExtractionResponseDisposition::Complete,
            actual_model: "synthetic-model".into(),
            config_revision: Some(3),
            prompt_tokens: 17,
            completion_tokens: 9,
        }
    }
    fn result() -> ObservationAnalysisResult {
        let response = response();
        ObservationAnalysisResult {
            config_revision: response.config_revision,
            actual_model: Some(response.actual_model),
            candidate_json: None,
            outcome: ObservationAnalysisOutcome::Failed {
                code: "invalid_model_response".into(),
            },
            prompt_tokens: response.prompt_tokens,
            completion_tokens: response.completion_tokens,
        }
    }

    struct SourceMetadata(Vec<crate::ObservationCaptureSourceItem>);

    #[async_trait]
    impl ObservationCaptureRepository for SourceMetadata {
        async fn scan_sources(
            &self,
            _as_of: DateTime<Utc>,
            after: Option<Uuid>,
            limit: usize,
        ) -> Result<Vec<crate::ObservationCaptureSourceItem>, AppError> {
            validate_observation_extraction_page_size(limit)?;
            Ok(self
                .0
                .iter()
                .filter(|item| after.is_none_or(|id| item.capture_id > id))
                .take(limit)
                .cloned()
                .collect())
        }
        async fn save(
            &self,
            _scope: &TenantScope,
            _input: ObservationCaptureInput,
        ) -> Result<ObservationCaptureReceipt, AppError> {
            panic!("discovery must not write captures")
        }
        async fn get(
            &self,
            _scope: &TenantScope,
            _id: Uuid,
        ) -> Result<Option<ObservationCapture>, AppError> {
            panic!("discovery must not load capture bodies")
        }
    }

    #[tokio::test]
    async fn backfill_skips_all_states_and_versions_without_short_pages() {
        let now = Utc::now();
        let scope = scope();
        let other = super::tests::scope();
        let sources = (1..=8)
            .map(|id| crate::ObservationCaptureSourceItem {
                scope: if id == 7 {
                    other.clone()
                } else {
                    scope.clone()
                },
                capture_id: Uuid::from_u128(id),
            })
            .collect();
        let store = MemoryObservationExtractionRepository::new(Arc::new(SourceMetadata(sources)));
        for (index, state) in [
            ObservationExtractionState::Queued,
            ObservationExtractionState::Running,
            ObservationExtractionState::ResponseReceived,
            ObservationExtractionState::Completed,
            ObservationExtractionState::Interrupted,
        ]
        .into_iter()
        .enumerate()
        {
            let mut request = request();
            request.source_capture_id = Some(Uuid::from_u128(index as u128 + 1));
            request.prompt_version = format!("old-prompt.{index}");
            request.parser_version = format!("old-parser.{index}");
            request.policy_version = format!("old-policy.{index}");
            let mut intent = ObservationExtractionIntent::queued(
                request.clone(),
                request.intent_key(&scope).unwrap(),
                now,
            );
            intent.state = state;
            store.records.lock().await.insert(
                intent.intent_id,
                Stored {
                    scope: scope.clone(),
                    intent,
                    claim_token: None,
                },
            );
        }
        // Equal digests and even a same-ID intent in another scope do not
        // suppress the source. The exact scoped capture is the only identity.
        let mut request = request();
        request.source_capture_id = Some(Uuid::from_u128(7));
        let intent = ObservationExtractionIntent::queued(
            request.clone(),
            request.intent_key(&scope).unwrap(),
            now,
        );
        store.records.lock().await.insert(
            intent.intent_id,
            Stored {
                scope: scope.clone(),
                intent,
                claim_token: None,
            },
        );
        let first = store.scan_unqueued_sources(now, None, 2).await.unwrap();
        assert_eq!(
            first.iter().map(|item| item.capture_id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(6), Uuid::from_u128(7)]
        );
        assert_eq!(first[1].scope, other);
        let last = store
            .scan_unqueued_sources(now, Some(first[1].capture_id), 2)
            .await
            .unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].capture_id, Uuid::from_u128(8));
        assert!(
            store
                .scan_unqueued_sources(now, Some(last[0].capture_id), 2)
                .await
                .unwrap()
                .is_empty()
        );
        for limit in [0, 101] {
            assert!(store.scan_unqueued_sources(now, None, limit).await.is_err());
        }
    }
    async fn seeded() -> (
        MemoryObservationExtractionRepository,
        TenantScope,
        ObservationExtractionIntent,
    ) {
        let captures = Arc::new(MemoryObservationCaptureRepository::new(Arc::new(
            MemoryChannelJobRepository::default(),
        )));
        let store = MemoryObservationExtractionRepository::new(captures);
        let scope = scope();
        let request = request();
        let intent = ObservationExtractionIntent::queued(
            request.clone(),
            request.intent_key(&scope).unwrap(),
            Utc::now(),
        );
        store.records.lock().await.insert(
            intent.intent_id,
            Stored {
                scope: scope.clone(),
                intent: intent.clone(),
                claim_token: None,
            },
        );
        (store, scope, intent)
    }
    #[test]
    fn canonical_identity_and_manual_source_selectors() {
        let scope = TenantScope::new(
            Uuid::from_u128(1).into(),
            Uuid::from_u128(2).into(),
            Some(Uuid::from_u128(3).into()),
        );
        let mut request = ObservationExtractionRequest {
            target_id: Uuid::from_u128(4),
            attempt_id: Uuid::from_u128(5),
            source_capture_id: Some(Uuid::from_u128(6)),
            source_evidence_index: None,
            manual_revision_id: None,
            source_sha256: "0".repeat(64),
            prompt_version: "extract.v1".into(),
            parser_version: "ground.v1".into(),
            policy_version: "automatic.v1".into(),
        };
        let automatic = request.intent_key(&scope).unwrap();
        assert_eq!(
            automatic,
            sha256_hex(
                &serde_json::to_vec(&("observation-extraction-intent.v1", &scope, &request))
                    .unwrap()
            )
        );
        let json = serde_json::to_value(&request).unwrap();
        assert!(json.get("manual_revision_id").is_none());
        assert!(json.get("source_evidence_index").is_none());
        assert_eq!(
            serde_json::from_value::<ObservationExtractionRequest>(json).unwrap(),
            request
        );
        request.manual_revision_id = Some(Uuid::from_u128(7));
        let manual = request.intent_key(&scope).unwrap();
        assert_ne!(manual, automatic);
        request.source_evidence_index = Some(0);
        assert!(request.intent_key(&scope).is_err());
        request.source_capture_id = None;
        assert_ne!(request.intent_key(&scope).unwrap(), manual);
        request.manual_revision_id = None;
        assert!(request.intent_key(&scope).is_err());
        request.source_evidence_index = None;
        assert!(request.intent_key(&scope).is_err());
    }

    #[test]
    fn identity_binds_scope_attempt_capture_and_all_versions() {
        let scope = scope();
        let request = request();
        let key = request.intent_key(&scope).unwrap();
        assert_eq!(key, request.clone().intent_key(&scope).unwrap());
        assert_ne!(key, request.intent_key(&super::tests::scope()).unwrap());
        for field in 0..7 {
            let mut changed = request.clone();
            match field {
                0 => changed.target_id = Uuid::new_v4(),
                1 => changed.attempt_id = Uuid::new_v4(),
                2 => changed.source_capture_id = Some(Uuid::new_v4()),
                3 => changed.source_sha256 = sha256_hex(b"other"),
                4 => changed.prompt_version = "extract.v2".into(),
                5 => changed.parser_version = "ground.v2".into(),
                _ => changed.policy_version = "automatic.v2".into(),
            };
            assert_ne!(key, changed.intent_key(&scope).unwrap());
        }
        assert!(
            request
                .intent_key(&TenantScope::new(scope.operator_id, scope.tenant_id, None))
                .is_err()
        );
    }

    #[tokio::test]
    async fn status_is_scoped_and_selects_latest_active_before_terminal() {
        let (store, scope, oldest) = seeded().await;
        let target = oldest.request.target_id;
        let attempt = oldest.request.attempt_id;
        let mut newer = oldest.clone();
        newer.intent_id = Uuid::new_v4();
        newer.created_at += chrono::Duration::seconds(1);
        newer.state = ObservationExtractionState::Interrupted;
        newer.interruption = Some(ObservationExtractionInterruption::InferenceInterrupted);
        store.records.lock().await.insert(
            newer.intent_id,
            Stored {
                scope: scope.clone(),
                intent: newer.clone(),
                claim_token: Some(Uuid::new_v4()),
            },
        );
        let status = store
            .status_for_attempt(&scope, target, attempt)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, ObservationExtractionStatus::from(&oldest));
        for wrong in [
            TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
            TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
            TenantScope::new(
                scope.operator_id,
                scope.tenant_id,
                Some(Uuid::new_v4().into()),
            ),
        ] {
            assert!(
                store
                    .status_for_attempt(&wrong, target, attempt)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            store
                .status_for_attempt(&scope, Uuid::new_v4(), attempt)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .status_for_attempt(&scope, target, Uuid::new_v4())
                .await
                .unwrap()
                .is_none()
        );

        let claim = store
            .claim(&scope, oldest.intent_id, oldest.created_at)
            .await
            .unwrap()
            .unwrap();
        // An old running claim remains the stored state; age is not completion.
        assert_eq!(
            store
                .status_for_attempt(&scope, target, attempt)
                .await
                .unwrap()
                .unwrap()
                .state,
            ObservationExtractionState::Running
        );
        store
            .interrupt(
                &scope,
                oldest.intent_id,
                claim.claim_token,
                ObservationExtractionInterruption::InferenceInterrupted,
                newer.created_at,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .status_for_attempt(&scope, target, attempt)
                .await
                .unwrap()
                .unwrap(),
            ObservationExtractionStatus::from(&newer)
        );
        let serialized = serde_json::to_value(status).unwrap();
        assert_eq!(serialized.as_object().unwrap().len(), 5);
        for secret in [
            "request",
            "response",
            "intent_id",
            "claim_token",
            "completion",
            "intent_key",
        ] {
            assert!(serialized.get(secret).is_none());
        }
    }

    #[tokio::test]
    async fn status_orders_matching_tasks_by_created_at_then_intent_id() {
        let (store, scope, oldest) = seeded().await;
        let mut first = oldest.clone();
        first.intent_id = Uuid::from_u128(1);
        first.created_at += chrono::Duration::seconds(1);
        let mut second = first.clone();
        second.intent_id = Uuid::from_u128(2);
        second.state = ObservationExtractionState::Running;
        second.started_at = Some(second.created_at);
        for intent in [first.clone(), second.clone()] {
            store.records.lock().await.insert(
                intent.intent_id,
                Stored {
                    scope: scope.clone(),
                    intent,
                    claim_token: Some(Uuid::new_v4()),
                },
            );
        }
        assert_eq!(
            store
                .status_for_attempt(&scope, oldest.request.target_id, oldest.request.attempt_id)
                .await
                .unwrap()
                .unwrap(),
            ObservationExtractionStatus::from(&second)
        );
        first.created_at += chrono::Duration::seconds(1);
        store
            .records
            .lock()
            .await
            .get_mut(&first.intent_id)
            .unwrap()
            .intent = first.clone();
        assert_eq!(
            store
                .status_for_attempt(&scope, oldest.request.target_id, oldest.request.attempt_id)
                .await
                .unwrap()
                .unwrap(),
            ObservationExtractionStatus::from(&first)
        );
    }
    #[test]
    fn source_binding_rejects_tamper_and_mismatched_attempt() {
        let scope = scope();
        let mut request = request();
        let source_json = r#"{"messages":[{"content":"Synthetic response"}]}"#.to_owned();
        request.source_sha256 = sha256_hex(source_json.as_bytes());
        let input = ObservationCaptureInput {
            capture_id: request.source_capture_id.unwrap(),
            target_id: request.target_id,
            attempt_id: request.attempt_id,
            account_id: Uuid::new_v4(),
            runner_session_id: Uuid::new_v4(),
            original_identity: None,
            ordinal: 0,
            observed_at: Utc::now(),
            snapshot: ObservationCaptureSnapshot::Source {
                source_json,
                source_sha256: request.source_sha256.clone(),
            },
            owned_conversation: None,
            completion: None,
        };
        let digest = input.validate(&scope).unwrap();
        let capture = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: digest,
                stored_at: Utc::now(),
            },
            input,
        };
        request.validate_source(&scope, &capture).unwrap();
        let mut changed = request.clone();
        changed.attempt_id = Uuid::new_v4();
        assert!(changed.validate_source(&scope, &capture).is_err());
        let mut tampered = capture.clone();
        tampered.receipt.digest_sha256 = sha256_hex(b"tampered");
        assert!(request.validate_source(&scope, &tampered).is_err());
    }
    #[tokio::test]
    async fn one_claim_response_resume_and_write_once_completion() {
        let (store, scope, intent) = seeded().await;
        let now = intent.created_at;
        let (a, b) = tokio::join!(
            store.claim(&scope, intent.intent_id, now),
            store.claim(&scope, intent.intent_id, now)
        );
        let claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
        assert_eq!(claims.len(), 1);
        let token = claims[0].claim_token;
        assert!(store.scan_ready(now, None, 10).await.unwrap().is_empty());
        assert!(
            store
                .claim(&scope, intent.intent_id, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .record_response(&scope, intent.intent_id, Uuid::new_v4(), response(), now)
                .await
                .is_err()
        );
        let checkpoint = store
            .record_response(&scope, intent.intent_id, token, response(), now)
            .await
            .unwrap();
        assert_eq!(
            checkpoint.response.as_ref().unwrap().model_text,
            response().model_text
        );
        assert_eq!(
            store.scan_ready(now, None, 10).await.unwrap()[0].state,
            ObservationExtractionState::ResponseReceived
        );
        let resumed = store
            .claim_response(&scope, intent.intent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resumed.claim_token, token);
        let mut wrong = result();
        wrong.actual_model = Some("different-model".into());
        assert!(
            store
                .complete(&scope, intent.intent_id, token, wrong, now)
                .await
                .is_err()
        );
        let completed = store
            .complete(&scope, intent.intent_id, token, result(), now)
            .await
            .unwrap();
        assert_eq!(completed.state, ObservationExtractionState::Completed);
        assert_eq!(
            store
                .complete(&scope, intent.intent_id, token, result(), now)
                .await
                .unwrap(),
            completed
        );
        let mut different = result();
        different.outcome = ObservationAnalysisOutcome::Failed {
            code: "different_failure".into(),
        };
        assert!(
            store
                .complete(&scope, intent.intent_id, token, different, now)
                .await
                .is_err()
        );
        assert!(store.scan_ready(now, None, 10).await.unwrap().is_empty());
        assert!(
            store
                .claim_response(&scope, intent.intent_id)
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn interruption_preserves_response_and_fences_old_owner() {
        let (store, scope, intent) = seeded().await;
        let now = intent.created_at;
        let token = store
            .claim(&scope, intent.intent_id, now)
            .await
            .unwrap()
            .unwrap()
            .claim_token;
        store
            .record_response(&scope, intent.intent_id, token, response(), now)
            .await
            .unwrap();
        let terminal = store
            .interrupt(
                &scope,
                intent.intent_id,
                token,
                ObservationExtractionInterruption::GroundingInterrupted,
                now,
            )
            .await
            .unwrap();
        assert_eq!(terminal.response, Some(response()));
        assert!(
            store
                .complete(&scope, intent.intent_id, token, result(), now)
                .await
                .is_err()
        );
        assert!(
            store
                .record_response(&scope, intent.intent_id, token, response(), now)
                .await
                .is_err()
        );
        assert!(
            store
                .get(&super::tests::scope(), intent.intent_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.scan_ready(now, None, 10).await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn ready_scan_uses_keyset_cutoff_and_bounded_pages() {
        let (store, scope, intent) = seeded().await;
        let now = intent.created_at;
        let mut second = intent.clone();
        second.intent_id = Uuid::new_v4();
        store.records.lock().await.insert(
            second.intent_id,
            Stored {
                scope,
                intent: second,
                claim_token: None,
            },
        );
        let first = store.scan_ready(now, None, 1).await.unwrap();
        assert_eq!(first.len(), 1);
        let second = store
            .scan_ready(now, Some(first[0].intent_id), 1)
            .await
            .unwrap();
        assert_eq!(second.len(), 1);
        assert!(second[0].intent_id > first[0].intent_id);
        assert!(
            store
                .scan_ready(now, Some(second[0].intent_id), 1)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .scan_ready(now - chrono::Duration::seconds(1), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        for limit in [0, 101, usize::MAX] {
            assert!(store.scan_ready(now, None, limit).await.is_err());
        }
    }
    #[test]
    fn exact_response_bound_counts_utf8_bytes_and_retains_invalid_json() {
        response().validate().unwrap();
        let mut value = response();
        value.model_text = "文".repeat(MAX_EXTRACTION_MODEL_TEXT_BYTES / 3);
        value
            .model_text
            .push_str(&"x".repeat(MAX_EXTRACTION_MODEL_TEXT_BYTES % 3));
        value.validate().unwrap();
        value.model_text.push('x');
        assert!(value.validate().is_err());
        value.model_text = "\0".into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn incomplete_dispositions_cannot_become_grounded_but_keep_metadata() {
        for disposition in [
            ObservationExtractionResponseDisposition::Incomplete,
            ObservationExtractionResponseDisposition::ToolCalls,
            ObservationExtractionResponseDisposition::UnexpectedFinish,
        ] {
            let request = request();
            let scope = scope();
            let now = Utc::now();
            let mut intent = ObservationExtractionIntent::queued(
                request.clone(),
                request.intent_key(&scope).unwrap(),
                now,
            );
            intent.state = ObservationExtractionState::Running;
            intent.started_at = Some(now);
            let mut response = response();
            response.disposition = disposition;
            intent.record_response(response.clone(), now).unwrap();
            let mut grounded = result();
            grounded.candidate_json = Some("{}".into());
            grounded.outcome = ObservationAnalysisOutcome::Grounded {
                raw_answer: "Synthetic answer".into(),
                citations: vec![],
                audit: serde_json::json!({"refs":[{"role":"answer_segment"}],"source_sha256":request.source_sha256,"prompt_version":request.prompt_version}),
            };
            assert!(intent.complete(grounded, now).is_err());
            let mut unverified = result();
            unverified.outcome = ObservationAnalysisOutcome::Unverified {
                reason: "model_output_incomplete".into(),
            };
            intent.complete(unverified, now).unwrap();
            assert_eq!(intent.response, Some(response));
        }
    }
}
