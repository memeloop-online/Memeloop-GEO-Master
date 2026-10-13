//! Durable inference shared by live callbacks and saved-response continuation.
//! HTTP waiters never own or cancel a model call. Running claims are not reclaimed.
use std::{sync::Arc, time::Duration};

use chrono::Utc;
use geo_domain::{
    AppError, ObservationAnalysisResult, ObservationCaptureRepository, ObservationCaptureSnapshot,
    ObservationExtractionClaim, ObservationExtractionIntent, ObservationExtractionInterruption,
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
    async fn continue_response(
        &self,
        scope: &TenantScope,
        intent: &ObservationExtractionIntent,
    ) -> Result<Option<ObservationAnalysisResult>, AppError> {
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
    pub fn new(
        repository: Arc<dyn ObservationExtractionRepository>,
        captures: Arc<dyn ObservationCaptureRepository>,
        model: ProjectConfiguredModelBridge,
    ) -> Self {
        Self {
            repository,
            captures,
            model: Arc::new(model),
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
            source_capture_id: capture_id,
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
        let intent = self.repository.enqueue(scope, request, Utc::now()).await?;
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

    async fn dispatch(&self, scope: &TenantScope, intent_id: Uuid) -> Result<(), AppError> {
        if let Some(claim) = self.repository.claim(scope, intent_id, Utc::now()).await? {
            let outcome = async {
                validate_versions(&claim.intent.request)?;
                let capture = self
                    .captures
                    .get(scope, claim.intent.request.source_capture_id)
                    .await?
                    .ok_or_else(|| AppError::not_found("extraction source not found"))?;
                claim.intent.request.validate_source(scope, &capture)?;
                let ObservationCaptureSnapshot::Source { source_json, .. } =
                    &capture.input.snapshot
                else {
                    return Err(AppError::conflict("extraction requires source capture"));
                };
                let prompt = crate::observation_ai_callback::extraction_prompt_versioned(
                    source_json,
                    &claim.intent.request.prompt_version,
                )
                .map_err(|_| AppError::invalid_request("invalid extraction source"))?;
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
                        tracing::info!(
                            failure = crate::observation_analysis::model_failure_code(&error),
                            "durable observation inference failed"
                        );
                        AppError::capability_missing("extraction inference unavailable")
                    })?;
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
                        .interrupt(
                            scope,
                            intent_id,
                            claim.claim_token,
                            ObservationExtractionInterruption::InferenceFailed,
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
                    .interrupt(
                        scope,
                        intent_id,
                        claim.claim_token,
                        ObservationExtractionInterruption::ResponseNotPersistable,
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
        let repository = Arc::new(MemoryObservationExtractionRepository::new(captures.clone()));
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
    async fn legacy_and_current_intents_keep_distinct_parser_bindings() {
        let (scope, captures, _, _, service) = fixture();
        let current = enqueue(&service, &scope, &captures).await;
        let legacy = service
            .enqueue_source(
                &scope,
                captures.capture.input.capture_id,
                &current.request.source_sha256,
                "geo.observation.extract.v2",
            )
            .await
            .unwrap();
        assert_ne!(current.intent_id, legacy.intent_id);
        assert_eq!(current.request.parser_version, "geo.observation.extract.v5");
        assert_eq!(legacy.request.parser_version, "geo.observation.extract.v4");
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
