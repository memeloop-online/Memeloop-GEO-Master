//! Durable inference shared by live callbacks and saved-response continuation.
//! HTTP waiters never own or cancel a model call. Running claims are not reclaimed.
use std::{collections::VecDeque, sync::Arc, time::Duration};

use chrono::Utc;
use geo_domain::{
    AppError, AutomaticExtractionAdmission, ObservationAnalysisResult,
    ObservationCaptureRepository, ObservationCaptureSnapshot, ObservationExtractionClaim,
    ObservationExtractionIntent, ObservationExtractionInterruption,
    ObservationExtractionRepository, ObservationExtractionRequest, ObservationExtractionResponse,
    ObservationExtractionResponseDisposition, ObservationExtractionState, ProjectAiUsage,
    TenantScope,
};
use geo_worker::ModelCompletionRequest;
use tokio::{sync::Notify, task::JoinSet};
use uuid::Uuid;

use crate::{ObservationAnalysisService, ProjectConfiguredModelBridge};

pub const EXTRACTION_POLICY_VERSION: &str = "geo.observation.automatic.v1";
const WORKERS: usize = 4;

fn validate_versions(request: &ObservationExtractionRequest) -> Result<(), AppError> {
    if request.policy_version != EXTRACTION_POLICY_VERSION
        || !matches!(
            (
                request.prompt_version.as_str(),
                request.parser_version.as_str()
            ),
            ("geo.observation.extract.v3", "geo.observation.extract.v5")
                | ("geo.observation.extract.v2", "geo.observation.extract.v4")
        )
    {
        return Err(AppError::invalid_request("extraction version unsupported"));
    }
    Ok(())
}

#[async_trait::async_trait]
pub trait ObservationExtractionContinuation: Send + Sync {
    async fn manual_source(
        &self,
        _scope: &TenantScope,
        _intent: &ObservationExtractionIntent,
    ) -> Result<String, AppError> {
        Err(AppError::capability_missing(
            "manual source resolver unavailable",
        ))
    }
    /// None defers projection until the original attempt has a terminal receipt.
    /// This continuation must never invoke inference or change browser ownership.
    async fn continue_response(
        &self,
        scope: &TenantScope,
        intent: &ObservationExtractionIntent,
    ) -> Result<Option<ObservationAnalysisResult>, AppError>;
}

#[async_trait::async_trait]
impl ObservationExtractionContinuation for ObservationAnalysisService {
    async fn manual_source(
        &self,
        scope: &TenantScope,
        intent: &ObservationExtractionIntent,
    ) -> Result<String, AppError> {
        Ok(
            ObservationAnalysisService::manual_source(self, scope, intent)
                .await?
                .2,
        )
    }
    async fn continue_response(
        &self,
        scope: &TenantScope,
        intent: &ObservationExtractionIntent,
    ) -> Result<Option<ObservationAnalysisResult>, AppError> {
        if intent.request.manual_revision_id.is_some() {
            return self
                .interpret_manual_response(scope, intent)
                .await
                .map(Some);
        }
        Ok(self
            .project_extraction_response(scope, intent)
            .await?
            .and_then(|revision| revision.result))
    }
}

#[derive(Clone)]
pub struct ObservationExtractionService {
    repository: Arc<dyn ObservationExtractionRepository>,
    captures: Arc<dyn ObservationCaptureRepository>,
    model: Arc<ProjectConfiguredModelBridge>,
    continuation: Option<Arc<dyn ObservationExtractionContinuation>>,
    wake: Arc<Notify>,
}

impl ObservationExtractionService {
    pub(crate) fn wake_handle(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    pub fn new(
        repository: Arc<dyn ObservationExtractionRepository>,
        captures: Arc<dyn ObservationCaptureRepository>,
        model: impl Into<Arc<ProjectConfiguredModelBridge>>,
    ) -> Self {
        Self {
            repository,
            captures,
            model: model.into(),
            continuation: None,
            wake: Arc::new(Notify::new()),
        }
    }

    pub fn with_continuation(
        mut self,
        continuation: Arc<dyn ObservationExtractionContinuation>,
    ) -> Self {
        self.continuation = Some(continuation);
        self
    }

    /// The caller must first authenticate the sealed source callback ticket.
    pub(crate) async fn enqueue_source(
        &self,
        scope: &TenantScope,
        capture_id: Uuid,
        source_sha256: &str,
        prompt_version: &str,
    ) -> Result<ObservationExtractionIntent, AppError> {
        let parser_version = match prompt_version {
            "geo.observation.extract.v3" => "geo.observation.extract.v5",
            "geo.observation.extract.v2" => "geo.observation.extract.v4",
            _ => return Err(AppError::invalid_request("extraction version unsupported")),
        };
        let capture = self
            .captures
            .get(scope, capture_id)
            .await?
            .ok_or_else(|| AppError::not_found("extraction source not found"))?;
        let request = ObservationExtractionRequest {
            target_id: capture.input.target_id,
            attempt_id: capture.input.attempt_id,
            source_capture_id: Some(capture_id),
            source_evidence_index: None,
            manual_revision_id: None,
            source_sha256: source_sha256.into(),
            prompt_version: prompt_version.into(),
            parser_version: parser_version.into(),
            policy_version: EXTRACTION_POLICY_VERSION.into(),
        };
        request.validate_source(scope, &capture)?;
        // Check the versioned prompt before enqueuing an inference intent.
        let ObservationCaptureSnapshot::Source { source_json, .. } = &capture.input.snapshot else {
            return Err(AppError::conflict("extraction requires source capture"));
        };
        crate::observation_ai_callback::extraction_prompt_versioned(source_json, prompt_version)
            .map_err(|_| AppError::invalid_request("invalid extraction source"))?;
        let intent = match self
            .repository
            .admit_automatic(scope, request.clone(), Utc::now())
            .await?
        {
            AutomaticExtractionAdmission::Intent(intent) => *intent,
            AutomaticExtractionAdmission::DeferredManual => {
                return Err(AppError::conflict("extraction deferred by saved analysis"));
            }
            AutomaticExtractionAdmission::DeferredHistoryChanged => {
                return Err(AppError::conflict("extraction history changed"));
            }
        };
        if intent.request.prompt_version != request.prompt_version
            || intent.request.parser_version != request.parser_version
            || intent.request.policy_version != request.policy_version
        {
            // Reusing the durable task never licenses returning an incompatible
            // raw model schema to a live callback, nor starting another call.
            return Err(AppError::conflict("extraction response version conflict"));
        }
        self.wake.notify_one();
        Ok(intent)
    }

    pub(crate) async fn wait_response(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        budget: Duration,
    ) -> Result<Option<ObservationExtractionResponse>, AppError> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let intent = self
                .repository
                .get(scope, intent_id)
                .await?
                .ok_or_else(|| AppError::not_found("extraction intent not found"))?;
            if let Some(response) = intent.response {
                return Ok(Some(response));
            }
            if intent.state == ObservationExtractionState::Interrupted {
                return Err(AppError::capability_missing(
                    "extraction inference interrupted",
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep_until(
                (tokio::time::Instant::now() + Duration::from_millis(100)).min(deadline),
            )
            .await;
        }
    }

    pub(crate) async fn dispatch(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<(), AppError> {
        if let Some(claim) = self.repository.claim(scope, intent_id, Utc::now()).await? {
            let mut failure = crate::observation_analysis::failed("analysis_version_unsupported");
            // Local validation fails before dispatch. Once the model future is
            // entered, only a received response or known local configuration
            // rejection establishes that inference is resolved.
            let mut inference_resolved = true;
            let outcome = async {
                validate_versions(&claim.intent.request)?;
                failure = crate::observation_analysis::failed("analysis_source_unavailable");
                let source_json = if claim.intent.request.manual_revision_id.is_some() {
                    self.continuation
                        .as_ref()
                        .ok_or_else(|| {
                            AppError::capability_missing("manual source resolver unavailable")
                        })?
                        .manual_source(scope, &claim.intent)
                        .await?
                } else {
                    let capture =
                        self.captures
                            .get(
                                scope,
                                claim.intent.request.source_capture_id.ok_or_else(|| {
                                    AppError::conflict("extraction capture required")
                                })?,
                            )
                            .await?
                            .ok_or_else(|| AppError::not_found("extraction source not found"))?;
                    claim.intent.request.validate_source(scope, &capture)?;
                    let ObservationCaptureSnapshot::Source { source_json, .. } =
                        capture.input.snapshot
                    else {
                        return Err(AppError::conflict("extraction requires source capture"));
                    };
                    source_json
                };
                let prompt = crate::observation_ai_callback::extraction_prompt_versioned(
                    &source_json,
                    &claim.intent.request.prompt_version,
                )
                .map_err(|_| AppError::invalid_request("invalid extraction source"))?;
                inference_resolved = false;
                let (completion, revision) = self
                    .model
                    .complete_for_usage_with_revision(
                        scope,
                        ProjectAiUsage::ObservationAnalysis,
                        &ModelCompletionRequest {
                            prompt,
                            system: None,
                            model: None,
                            max_output_tokens: Some(24_000),
                            messages: vec![],
                            tools: vec![],
                        },
                    )
                    .await
                    .map_err(|error| {
                        failure = crate::observation_analysis::failed(
                            crate::observation_analysis::model_failure_code(&error),
                        );
                        inference_resolved = matches!(
                            error.message.as_str(),
                            "project model settings unavailable"
                                | "project model unavailable"
                                | "no inherited or project model is configured"
                        );
                        tracing::info!(
                            failure = crate::observation_analysis::model_failure_code(&error),
                            "durable observation inference failed"
                        );
                        AppError::capability_missing("extraction inference unavailable")
                    })?;
                inference_resolved = true;
                let disposition = if !completion.tool_calls.is_empty() {
                    ObservationExtractionResponseDisposition::ToolCalls
                } else if completion.finish_reason == "stop" {
                    ObservationExtractionResponseDisposition::Complete
                } else if completion.finish_reason == "length" {
                    ObservationExtractionResponseDisposition::Incomplete
                } else {
                    ObservationExtractionResponseDisposition::UnexpectedFinish
                };
                Ok::<_, AppError>(ObservationExtractionResponse {
                    model_text: completion.text,
                    actual_model: completion.model,
                    config_revision: Some(revision),
                    prompt_tokens: completion.prompt_tokens,
                    completion_tokens: completion.completion_tokens,
                    disposition,
                })
            }
            .await;
            let response = match outcome {
                Ok(response) => response,
                Err(error) => {
                    // Only failures before obtaining a response terminate inference.
                    self.repository
                        .interrupt_with_result(
                            scope,
                            intent_id,
                            claim.claim_token,
                            ObservationExtractionInterruption::InferenceFailed,
                            failure,
                            inference_resolved,
                            Utc::now(),
                        )
                        .await?;
                    return Err(error);
                }
            };
            if response.validate().is_err() {
                // NUL/unbounded responses cannot be represented faithfully by
                // this checkpoint. Do not truncate, repair, or invoke again.
                self.repository
                    .interrupt_with_result(
                        scope,
                        intent_id,
                        claim.claim_token,
                        ObservationExtractionInterruption::ResponseNotPersistable,
                        {
                            let mut result =
                                crate::observation_analysis::failed("analysis_result_invalid");
                            result.config_revision = response.config_revision;
                            result.actual_model = Some(response.actual_model.clone());
                            result.prompt_tokens = response.prompt_tokens;
                            result.completion_tokens = response.completion_tokens;
                            if result.validate().is_err() {
                                result.actual_model = None;
                            }
                            result
                        },
                        true,
                        Utc::now(),
                    )
                    .await?;
                tracing::info!(
                    failure = "model_response_not_persistable",
                    "durable observation response rejected"
                );
                return Ok(());
            }
            // A write error can follow a committed transaction. Reconcile or
            // retry only the exact obtained response, never infer or interrupt.
            self.checkpoint_response(scope, &claim, response).await?;
        }
        if let Some(continuation) = &self.continuation
            && let Some(claim) = self.repository.claim_response(scope, intent_id).await?
        {
            validate_versions(&claim.intent.request)?;
            if let Some(result) = continuation.continue_response(scope, &claim.intent).await? {
                self.repository
                    .complete(scope, intent_id, claim.claim_token, result, Utc::now())
                    .await?;
            }
        }
        Ok(())
    }

    async fn checkpoint_response(
        &self,
        scope: &TenantScope,
        claim: &ObservationExtractionClaim,
        response: ObservationExtractionResponse,
    ) -> Result<(), AppError> {
        let received_at = Utc::now();
        for attempt in 0..3 {
            match self
                .repository
                .record_response(
                    scope,
                    claim.intent.intent_id,
                    claim.claim_token,
                    response.clone(),
                    received_at,
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) => {
                    match self.repository.get(scope, claim.intent.intent_id).await {
                        Ok(Some(stored))
                            if matches!(
                                stored.state,
                                ObservationExtractionState::ResponseReceived
                                    | ObservationExtractionState::Completed
                            ) && stored.response.as_ref() == Some(&response) =>
                        {
                            return Ok(());
                        }
                        Ok(Some(stored))
                            if stored.state == ObservationExtractionState::Running
                                && stored.request == claim.intent.request
                                && attempt < 2 =>
                        {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        // Failed reads/unknown claims remain unresolved. A future
                        // scanner may find a received checkpoint; never fence it.
                        _ => return Err(error),
                    }
                }
            }
        }
        unreachable!("checkpoint retries terminate on their final attempt")
    }
}

struct HistoricalCandidate {
    item: geo_domain::ObservationExtractionBackfillItem,
    cursor: crate::observation_analysis::HistoricalSourceCursor,
}

/// A fixed-cutoff keyset pass survives ticks. Each pending source gets one
/// revision page per tick; long histories cannot monopolize the scheduler.
pub(crate) struct HistoricalScan {
    cutoff: chrono::DateTime<Utc>,
    after: Option<Uuid>,
    exhausted: bool,
    pending: VecDeque<HistoricalCandidate>,
}

impl HistoricalScan {
    pub(crate) fn new() -> Self {
        Self {
            cutoff: Utc::now(),
            after: None,
            exhausted: false,
            pending: VecDeque::new(),
        }
    }

    pub(crate) async fn tick(
        &mut self,
        service: &ObservationExtractionService,
        analysis: &ObservationAnalysisService,
    ) -> Result<(), AppError> {
        const PAGE: usize = 16;
        // Keep memory bounded while allowing other candidates to advance next
        // to a source with arbitrarily many historical revisions.
        if !self.exhausted && self.pending.len() < PAGE {
            let items = service
                .repository
                .scan_unqueued_sources(self.cutoff, self.after, PAGE - self.pending.len())
                .await?;
            self.exhausted = items.len() < PAGE - self.pending.len();
            for item in items {
                self.after = Some(item.capture_id);
                self.pending.push_back(HistoricalCandidate {
                    item,
                    cursor: Default::default(),
                });
            }
        }
        let work = self.pending.len().min(PAGE);
        for _ in 0..work {
            let Some(mut candidate) = self.pending.pop_front() else {
                break;
            };
            let outcome = async {
                let Some(capture) = service
                    .captures
                    .get(&candidate.item.scope, candidate.item.capture_id)
                    .await?
                else {
                    return Ok::<_, AppError>(true);
                };
                let preparation = analysis
                    .prepare_historical_source_page(
                        &candidate.item.scope,
                        capture.input.target_id,
                        capture.input.attempt_id,
                        candidate.item.capture_id,
                        &mut candidate.cursor,
                    )
                    .await?;
                match preparation {
                    None => Ok(false),
                    Some(crate::observation_analysis::HistoricalSourcePreparation::Ready(
                        request,
                    )) => {
                        let request = ObservationExtractionRequest {
                            target_id: request.target_id,
                            attempt_id: request.attempt_id,
                            source_capture_id: Some(candidate.item.capture_id),
                            source_evidence_index: None,
                            manual_revision_id: None,
                            source_sha256: request.source_sha256,
                            prompt_version: request.prompt_version,
                            parser_version: request.parser_version,
                            policy_version: EXTRACTION_POLICY_VERSION.into(),
                        };
                        request.validate_source(&candidate.item.scope, &capture)?;
                        let admission = service
                            .repository
                            .admit_automatic_with_history(
                                &candidate.item.scope,
                                request,
                                Utc::now(),
                                candidate
                                    .cursor
                                    .history_snapshot
                                    .as_deref()
                                    .ok_or_else(|| {
                                        AppError::conflict("extraction history snapshot missing")
                                    })?,
                            )
                            .await?;
                        if matches!(
                            admission,
                            AutomaticExtractionAdmission::DeferredHistoryChanged
                        ) {
                            candidate.cursor = Default::default();
                            return Ok(false);
                        }
                        service.wake.notify_one();
                        Ok(true)
                    }
                    // Missing receipts and manual work are reconsidered next
                    // pass, without mutating or expiring that work.
                    Some(_) => Ok(true),
                }
            }
            .await;
            match outcome {
                Ok(false) => self.pending.push_back(candidate),
                Ok(true) => {}
                Err(error) => {
                    tracing::warn!(code = ?error.code, "observation history preparation failed");
                }
            }
        }
        if self.exhausted && self.pending.is_empty() {
            *self = Self::new();
        }
        Ok(())
    }
}

/// Deployment cutover must explicitly enable this after legacy direct calls
/// drain. Existing queued/received recovery is independent of this scheduler.
pub fn spawn_observation_history_scheduler(
    service: ObservationExtractionService,
    analysis: ObservationAnalysisService,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut scan = HistoricalScan::new();
        let mut ticks = tokio::time::interval(Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Err(error) = scan.tick(&service, &analysis).await {
                tracing::warn!(code = ?error.code, "observation history scan failed");
            }
        }
    })
}

/// One bounded dispatcher per application assembly. Startup scans immediately;
/// enqueue wakes it, and periodic keyset scans recover queued/received checkpoints.
pub fn spawn_observation_extraction_dispatcher(
    service: ObservationExtractionService,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { _ = ticks.tick() => {}, _ = service.wake.notified() => {} }
            let as_of = Utc::now();
            let mut after = None;
            let mut workers = JoinSet::new();
            loop {
                let items = match service.repository.scan_ready(as_of, after, 100).await {
                    Ok(items) => items,
                    Err(error) => {
                        tracing::warn!(code = ?error.code, "observation extraction scan failed");
                        break;
                    }
                };
                let count = items.len();
                for item in items {
                    after = Some(item.intent_id);
                    if workers.len() >= WORKERS {
                        let _ = workers.join_next().await;
                    }
                    let service = service.clone();
                    workers.spawn(async move {
                        if let Err(error) = service.dispatch(&item.scope, item.intent_id).await {
                            tracing::warn!(code = ?error.code, "observation extraction dispatch failed");
                        }
                    });
                }
                if count < 100 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            while workers.join_next().await.is_some() {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelProviderBridge, ProjectAiSettingsService};
    use geo_domain::{
        MemoryObservationExtractionRepository, ObservationAnalysisOutcome, ObservationCapture,
        ObservationCaptureInput, ObservationCaptureReceipt,
    };
    use geo_worker::{HostOpError, ModelCompletion};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct LostCheckpointAck {
        inner: Arc<MemoryObservationExtractionRepository>,
        lose_next_ack: AtomicBool,
        interruptions: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ObservationExtractionRepository for LostCheckpointAck {
        async fn interrupt_with_result(
            &self,
            scope: &TenantScope,
            id: Uuid,
            token: Uuid,
            reason: ObservationExtractionInterruption,
            result: ObservationAnalysisResult,
            resolved: bool,
            at: chrono::DateTime<Utc>,
        ) -> Result<ObservationExtractionIntent, AppError> {
            self.interruptions.fetch_add(1, Ordering::SeqCst);
            self.inner
                .interrupt_with_result(scope, id, token, reason, result, resolved, at)
                .await
        }
        async fn admit_manual(
            &self,
            scope: &TenantScope,
            key: &str,
            digest: &str,
            request: geo_domain::ObservationAnalysisRequest,
            at: chrono::DateTime<Utc>,
        ) -> Result<geo_domain::ManualExtractionAdmission, AppError> {
            self.inner
                .admit_manual(scope, key, digest, request, at)
                .await
        }
        async fn admit_automatic_with_history(
            &self,
            scope: &TenantScope,
            request: ObservationExtractionRequest,
            at: chrono::DateTime<Utc>,
            expected_history: &str,
        ) -> Result<AutomaticExtractionAdmission, AppError> {
            self.inner
                .admit_automatic_with_history(scope, request, at, expected_history)
                .await
        }
        async fn admit_automatic(
            &self,
            scope: &TenantScope,
            request: ObservationExtractionRequest,
            at: chrono::DateTime<Utc>,
        ) -> Result<AutomaticExtractionAdmission, AppError> {
            self.inner.admit_automatic(scope, request, at).await
        }
        async fn scan_unqueued_sources(
            &self,
            at: chrono::DateTime<Utc>,
            after: Option<Uuid>,
            limit: usize,
        ) -> Result<Vec<geo_domain::ObservationExtractionBackfillItem>, AppError> {
            self.inner.scan_unqueued_sources(at, after, limit).await
        }
        async fn enqueue(
            &self,
            scope: &TenantScope,
            request: ObservationExtractionRequest,
            at: chrono::DateTime<Utc>,
        ) -> Result<ObservationExtractionIntent, AppError> {
            self.inner.enqueue(scope, request, at).await
        }
        async fn get(
            &self,
            scope: &TenantScope,
            id: Uuid,
        ) -> Result<Option<ObservationExtractionIntent>, AppError> {
            self.inner.get(scope, id).await
        }
        async fn status_for_attempt(
            &self,
            scope: &TenantScope,
            target_id: Uuid,
            attempt_id: Uuid,
        ) -> Result<Option<geo_domain::ObservationExtractionStatus>, AppError> {
            self.inner
                .status_for_attempt(scope, target_id, attempt_id)
                .await
        }
        async fn scan_ready(
            &self,
            at: chrono::DateTime<Utc>,
            after: Option<Uuid>,
            limit: usize,
        ) -> Result<Vec<geo_domain::ObservationExtractionReadyItem>, AppError> {
            self.inner.scan_ready(at, after, limit).await
        }
        async fn claim(
            &self,
            scope: &TenantScope,
            id: Uuid,
            at: chrono::DateTime<Utc>,
        ) -> Result<Option<ObservationExtractionClaim>, AppError> {
            self.inner.claim(scope, id, at).await
        }
        async fn claim_response(
            &self,
            scope: &TenantScope,
            id: Uuid,
        ) -> Result<Option<ObservationExtractionClaim>, AppError> {
            self.inner.claim_response(scope, id).await
        }
        async fn record_response(
            &self,
            scope: &TenantScope,
            id: Uuid,
            token: Uuid,
            response: ObservationExtractionResponse,
            at: chrono::DateTime<Utc>,
        ) -> Result<ObservationExtractionIntent, AppError> {
            let saved = self
                .inner
                .record_response(scope, id, token, response, at)
                .await?;
            if self.lose_next_ack.swap(false, Ordering::SeqCst) {
                Err(AppError::capability_missing(
                    "synthetic checkpoint acknowledgement lost",
                ))
            } else {
                Ok(saved)
            }
        }
        async fn complete(
            &self,
            scope: &TenantScope,
            id: Uuid,
            token: Uuid,
            result: ObservationAnalysisResult,
            at: chrono::DateTime<Utc>,
        ) -> Result<ObservationExtractionIntent, AppError> {
            self.inner.complete(scope, id, token, result, at).await
        }
        async fn interrupt(
            &self,
            scope: &TenantScope,
            id: Uuid,
            token: Uuid,
            reason: ObservationExtractionInterruption,
            at: chrono::DateTime<Utc>,
        ) -> Result<ObservationExtractionIntent, AppError> {
            self.interruptions.fetch_add(1, Ordering::SeqCst);
            self.inner.interrupt(scope, id, token, reason, at).await
        }
    }

    struct Captures {
        scope: TenantScope,
        capture: ObservationCapture,
    }
    #[async_trait::async_trait]
    impl ObservationCaptureRepository for Captures {
        async fn scan_sources(
            &self,
            _: chrono::DateTime<Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<geo_domain::ObservationCaptureSourceItem>, AppError> {
            unreachable!("explicit-source extraction test store")
        }
        async fn save(
            &self,
            _: &TenantScope,
            _: ObservationCaptureInput,
        ) -> Result<ObservationCaptureReceipt, AppError> {
            Err(AppError::conflict("read only synthetic capture"))
        }
        async fn get(
            &self,
            scope: &TenantScope,
            id: Uuid,
        ) -> Result<Option<ObservationCapture>, AppError> {
            Ok(
                (scope == &self.scope && id == self.capture.input.capture_id)
                    .then(|| self.capture.clone()),
            )
        }
    }
    #[derive(Default)]
    struct Model {
        calls: AtomicUsize,
        started: Notify,
        release: Notify,
        response: tokio::sync::Mutex<Option<ModelCompletion>>,
    }
    #[async_trait::async_trait]
    impl ModelProviderBridge for Model {
        async fn complete(
            &self,
            _: &TenantScope,
            _: &ModelCompletionRequest,
        ) -> Result<ModelCompletion, HostOpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            if let Some(response) = self.response.lock().await.take() {
                return Ok(response);
            }
            Ok(ModelCompletion {
                text: "{\"completion\":\"unknown\"}".into(),
                model: "synthetic-actual-model".into(),
                finish_reason: "stop".into(),
                tool_calls: vec![],
                prompt_tokens: 17,
                completion_tokens: 9,
            })
        }
    }
    fn fixture() -> (
        TenantScope,
        Arc<Captures>,
        Arc<MemoryObservationExtractionRepository>,
        Arc<Model>,
        ObservationExtractionService,
    ) {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let source = r#"{"messages":[{"text":"synthetic answer"}]}"#;
        let input = ObservationCaptureInput {
            capture_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            runner_session_id: Uuid::new_v4(),
            original_identity: None,
            ordinal: 0,
            observed_at: Utc::now(),
            snapshot: ObservationCaptureSnapshot::Source {
                source_json: source.into(),
                source_sha256: geo_domain::sha256_hex(source.as_bytes()),
            },
            owned_conversation: None,
            completion: None,
        };
        let captures = Arc::new(Captures {
            scope: scope.clone(),
            capture: ObservationCapture {
                receipt: ObservationCaptureReceipt {
                    capture_id: input.capture_id,
                    schema_version: 1,
                    digest_sha256: input.validate(&scope).unwrap(),
                    stored_at: Utc::now(),
                },
                input,
            },
        });
        let gate = geo_domain::MemoryObservationAdmissionGate::default();
        let analyses = Arc::new(
            geo_domain::MemoryObservationAnalysisRepository::new(
                Arc::new(geo_domain::MemoryChannelJobRepository::default()),
                captures.clone(),
            )
            .with_admission_gate(gate.clone()),
        );
        let repository = Arc::new(
            MemoryObservationExtractionRepository::new(captures.clone())
                .with_automatic_admission(gate, analyses),
        );
        let model = Arc::new(Model::default());
        let bridge = ProjectConfiguredModelBridge::new(
            ProjectAiSettingsService::development(),
            Some(model.clone()),
        )
        .unwrap();
        let service =
            ObservationExtractionService::new(repository.clone(), captures.clone(), bridge);
        (scope, captures, repository, model, service)
    }
    async fn enqueue(
        service: &ObservationExtractionService,
        scope: &TenantScope,
        captures: &Captures,
    ) -> ObservationExtractionIntent {
        let ObservationCaptureSnapshot::Source { source_sha256, .. } =
            &captures.capture.input.snapshot
        else {
            unreachable!()
        };
        service
            .enqueue_source(
                scope,
                captures.capture.input.capture_id,
                source_sha256,
                "geo.observation.extract.v3",
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn concurrent_callbacks_and_timed_out_waiter_share_one_inference() {
        let (scope, captures, repository, model, service) = fixture();
        let (first, duplicate) = tokio::join!(
            enqueue(&service, &scope, &captures),
            enqueue(&service, &scope, &captures)
        );
        assert_eq!(first.intent_id, duplicate.intent_id);
        let dispatcher = spawn_observation_extraction_dispatcher(service.clone());
        model.started.notified().await;
        assert!(
            service
                .wait_response(&scope, first.intent_id, Duration::from_millis(2))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repository
                .get(&scope, first.intent_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ObservationExtractionState::Running
        );
        assert!(
            repository
                .scan_ready(Utc::now(), None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        // Dropping a live waiter is independent of dispatcher-owned inference.
        let waiter = tokio::spawn({
            let service = service.clone();
            let scope = scope.clone();
            async move {
                service
                    .wait_response(&scope, first.intent_id, Duration::from_secs(2))
                    .await
            }
        });
        waiter.abort();
        model.release.notify_one();
        let response = service
            .wait_response(&scope, duplicate.intent_id, Duration::from_secs(2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.actual_model, "synthetic-actual-model");
        assert_eq!(
            (
                response.config_revision,
                response.prompt_tokens,
                response.completion_tokens
            ),
            (Some(0), 17, 9)
        );
        assert_eq!(
            enqueue(&service, &scope, &captures).await.intent_id,
            first.intent_id
        );
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        dispatcher.abort();
    }

    struct Continue(AtomicUsize);
    #[async_trait::async_trait]
    impl ObservationExtractionContinuation for Continue {
        async fn continue_response(
            &self,
            _: &TenantScope,
            intent: &ObservationExtractionIntent,
        ) -> Result<Option<ObservationAnalysisResult>, AppError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let response = intent.response.as_ref().unwrap();
            Ok(Some(ObservationAnalysisResult {
                actual_model: Some(response.actual_model.clone()),
                config_revision: response.config_revision,
                prompt_tokens: response.prompt_tokens,
                completion_tokens: response.completion_tokens,
                candidate_json: None,
                outcome: ObservationAnalysisOutcome::Unverified {
                    reason: "synthetic_unverified".into(),
                },
            }))
        }
    }
    #[tokio::test]
    async fn startup_recovers_response_checkpoint_without_model_call() {
        let (scope, captures, repository, model, service) = fixture();
        let intent = enqueue(&service, &scope, &captures).await;
        let claim = repository
            .claim(&scope, intent.intent_id, Utc::now())
            .await
            .unwrap()
            .unwrap();
        repository
            .record_response(
                &scope,
                intent.intent_id,
                claim.claim_token,
                ObservationExtractionResponse {
                    model_text: "{}".into(),
                    actual_model: "synthetic-saved-model".into(),
                    config_revision: Some(4),
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    disposition: ObservationExtractionResponseDisposition::Complete,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let continuation = Arc::new(Continue(AtomicUsize::new(0)));
        let dispatcher = spawn_observation_extraction_dispatcher(
            service.with_continuation(continuation.clone()),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if repository
                    .get(&scope, intent.intent_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state
                    == ObservationExtractionState::Completed
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert_eq!(continuation.0.load(Ordering::SeqCst), 1);
        dispatcher.abort();
    }

    #[tokio::test]
    async fn cross_version_callback_refuses_output_without_second_inference() {
        let (scope, captures, repository, model, service) = fixture();
        let current = enqueue(&service, &scope, &captures).await;
        model.release.notify_one();
        service.dispatch(&scope, current.intent_id).await.unwrap();
        let legacy = service
            .enqueue_source(
                &scope,
                captures.capture.input.capture_id,
                &current.request.source_sha256,
                "geo.observation.extract.v2",
            )
            .await
            .unwrap_err();
        assert_eq!(legacy.message, "extraction response version conflict");
        assert_eq!(current.request.parser_version, "geo.observation.extract.v5");
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        assert!(
            repository
                .get(&scope, current.intent_id)
                .await
                .unwrap()
                .unwrap()
                .response
                .is_some()
        );
        let foreign = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        assert!(
            service
                .enqueue_source(
                    &foreign,
                    captures.capture.input.capture_id,
                    &current.request.source_sha256,
                    "geo.observation.extract.v3"
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn incomplete_large_response_is_saved_with_provenance_and_never_inferred_again() {
        let (scope, captures, repository, model, service) = fixture();
        let intent = enqueue(&service, &scope, &captures).await;
        *model.response.lock().await = Some(ModelCompletion {
            text: "x".repeat(150_001),
            model: "synthetic-incomplete-model".into(),
            finish_reason: "length".into(),
            tool_calls: vec![],
            prompt_tokens: 31,
            completion_tokens: 24_000,
        });
        model.release.notify_one();
        service.dispatch(&scope, intent.intent_id).await.unwrap();
        service.dispatch(&scope, intent.intent_id).await.unwrap();
        let response = repository
            .get(&scope, intent.intent_id)
            .await
            .unwrap()
            .unwrap()
            .response
            .unwrap();
        assert_eq!(response.model_text.len(), 150_001);
        assert_eq!(
            response.disposition,
            ObservationExtractionResponseDisposition::Incomplete
        );
        assert_eq!(response.actual_model, "synthetic-incomplete-model");
        assert_eq!(
            (response.prompt_tokens, response.completion_tokens),
            (31, 24_000)
        );
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn response_that_cannot_be_stored_is_explicitly_interrupted_without_retry() {
        let (scope, captures, repository, model, service) = fixture();
        let intent = enqueue(&service, &scope, &captures).await;
        *model.response.lock().await = Some(ModelCompletion {
            text: "synthetic\0response".into(),
            model: "synthetic-model".into(),
            finish_reason: "stop".into(),
            tool_calls: vec![],
            prompt_tokens: 2,
            completion_tokens: 3,
        });
        model.release.notify_one();
        service.dispatch(&scope, intent.intent_id).await.unwrap();
        service.dispatch(&scope, intent.intent_id).await.unwrap();
        let stored = repository
            .get(&scope, intent.intent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, ObservationExtractionState::Interrupted);
        assert_eq!(
            stored.interruption,
            Some(ObservationExtractionInterruption::ResponseNotPersistable)
        );
        assert!(stored.response.is_none());
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn lost_checkpoint_ack_keeps_response_scan_ready_and_recovers_without_inference() {
        let (scope, captures, repository, model, mut service) = fixture();
        let faulty = Arc::new(LostCheckpointAck {
            inner: repository.clone(),
            lose_next_ack: AtomicBool::new(true),
            interruptions: AtomicUsize::new(0),
        });
        service.repository = faulty.clone();
        let intent = enqueue(&service, &scope, &captures).await;
        model.release.notify_one();
        service.dispatch(&scope, intent.intent_id).await.unwrap();
        let saved = repository
            .get(&scope, intent.intent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.state, ObservationExtractionState::ResponseReceived);
        assert!(saved.response.is_some());
        assert_eq!(
            repository.scan_ready(Utc::now(), None, 100).await.unwrap()[0].intent_id,
            intent.intent_id
        );
        assert_eq!(faulty.interruptions.load(Ordering::SeqCst), 0);
        let continuation = Arc::new(Continue(AtomicUsize::new(0)));
        let recovered = service.with_continuation(continuation.clone());
        recovered.dispatch(&scope, intent.intent_id).await.unwrap();
        assert_eq!(
            repository
                .get(&scope, intent.intent_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ObservationExtractionState::Completed
        );
        assert_eq!(continuation.0.load(Ordering::SeqCst), 1);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(faulty.interruptions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn received_response_with_unsupported_policy_never_calls_continuation() {
        let (scope, captures, repository, model, service) = fixture();
        let prototype = enqueue(&service, &scope, &captures).await;
        let mut request = prototype.request;
        request.policy_version = "geo.observation.automatic.future".into();
        let intent = repository
            .enqueue(&scope, request, Utc::now())
            .await
            .unwrap();
        let claim = repository
            .claim(&scope, intent.intent_id, Utc::now())
            .await
            .unwrap()
            .unwrap();
        repository
            .record_response(
                &scope,
                intent.intent_id,
                claim.claim_token,
                ObservationExtractionResponse {
                    model_text: "{}".into(),
                    actual_model: "synthetic-model".into(),
                    config_revision: Some(2),
                    prompt_tokens: 5,
                    completion_tokens: 3,
                    disposition: ObservationExtractionResponseDisposition::Complete,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let continuation = Arc::new(Continue(AtomicUsize::new(0)));
        assert!(
            service
                .with_continuation(continuation.clone())
                .dispatch(&scope, intent.intent_id)
                .await
                .is_err()
        );
        assert_eq!(continuation.0.load(Ordering::SeqCst), 0);
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            repository
                .get(&scope, intent.intent_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ObservationExtractionState::ResponseReceived
        );
    }
}
