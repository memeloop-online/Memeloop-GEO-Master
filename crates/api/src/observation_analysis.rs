//! Saved-source interpretation. This service cannot send a consumer question,
//! modify the original measurement, or grant remote cleanup authority.
use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    middleware,
    routing::get,
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelJobRepository, ChannelTargetInput, ChannelTargetView,
    ObservationAnalysisOutcome, ObservationAnalysisRepository, ObservationAnalysisRequest,
    ObservationAnalysisResult, ObservationAnalysisRevision, ObservationAnalysisSource,
    ObservationCaptureRepository, ObservationCaptureSnapshot, ProjectAiUsage, ProjectId,
    TenantScope, observation_analysis_source_json, sha256_hex,
};
use geo_worker::{HostOpError, HostOpErrorCode, ModelCompletionRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthContext, ProjectConfiguredModelBridge, RequestContext, api_error,
};

const PROMPT_VERSION: &str = "geo.observation.extract.v2";
const PARSER_VERSION: &str = "geo.observation.extract.v4";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyzeSavedSource {
    pub idempotency_key: String,
    pub capture_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SavedSource {
    pub source: ObservationAnalysisSource,
    pub source_sha256: String,
    pub observed_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageQuery {
    after: Option<Uuid>,
    limit: Option<usize>,
    tenant_id: Option<String>,
    project_id: Option<ProjectId>,
}

#[derive(Serialize)]
pub(crate) struct AnalysisPage {
    sources: Vec<SavedSource>,
    items: Vec<ObservationAnalysisRevision>,
    next_after: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundingResult {
    pub candidate_json: Option<String>,
    pub outcome: ObservationAnalysisOutcome,
}

#[async_trait::async_trait]
pub trait SavedObservationGrounder: Send + Sync {
    async fn ground(
        &self,
        source: &str,
        digest: &str,
        candidate: &str,
        protocol: Value,
    ) -> Result<GroundingResult, AppError>;
}

/// A service-only call to the existing deterministic JS grounding validator.
/// No browser session, credentials or model configuration enter this request.
#[derive(Clone)]
pub struct HttpSavedObservationGrounder {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl HttpSavedObservationGrounder {
    pub fn new(base_url: &str, token: &str) -> Result<Self, AppError> {
        let url = reqwest::Url::parse(base_url)
            .map_err(|_| AppError::invalid_request("invalid grounding service configuration"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || token.is_empty()
        {
            return Err(AppError::invalid_request(
                "invalid grounding service configuration",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AppError::capability_missing("grounding service unavailable"))?;
        Ok(Self {
            client,
            endpoint: format!(
                "{}/v1/observation-analyses/ground",
                base_url.trim_end_matches('/')
            ),
            token: token.into(),
        })
    }
}

#[async_trait::async_trait]
impl SavedObservationGrounder for HttpSavedObservationGrounder {
    async fn ground(
        &self,
        source: &str,
        digest: &str,
        candidate: &str,
        protocol: Value,
    ) -> Result<GroundingResult, AppError> {
        let unavailable =
            || AppError::capability_missing("saved observation grounding unavailable");
        let mut response = self.client.post(&self.endpoint).bearer_auth(&self.token)
            .json(&json!({"source_json":source,"source_sha256":digest,"candidate_json":candidate,"protocol":protocol}))
            .send().await.map_err(|_| unavailable())?;
        if response.status() != StatusCode::OK {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len() + chunk.len() > 3_000_000 {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| unavailable())
    }
}

#[derive(Clone)]
pub struct ObservationAnalysisService {
    repository: Arc<dyn ObservationAnalysisRepository>,
    jobs: Arc<dyn ChannelJobRepository>,
    captures: Arc<dyn ObservationCaptureRepository>,
    model: Arc<ProjectConfiguredModelBridge>,
    grounder: Arc<dyn SavedObservationGrounder>,
}

impl ObservationAnalysisService {
    async fn interrupt_stale(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<(), AppError> {
        let now = Utc::now();
        self.repository
            .interrupt_stale(
                scope,
                target_id,
                attempt_id,
                now - chrono::Duration::seconds(180),
                now,
            )
            .await
    }
    pub fn new(
        repository: Arc<dyn ObservationAnalysisRepository>,
        jobs: Arc<dyn ChannelJobRepository>,
        captures: Arc<dyn ObservationCaptureRepository>,
        model: ProjectConfiguredModelBridge,
        grounder: Arc<dyn SavedObservationGrounder>,
    ) -> Self {
        Self {
            repository,
            jobs,
            captures,
            model: Arc::new(model),
            grounder,
        }
    }

    fn request(
        target_id: Uuid,
        attempt_id: Uuid,
        source: SavedSource,
    ) -> ObservationAnalysisRequest {
        ObservationAnalysisRequest {
            revision_id: Uuid::new_v4(),
            target_id,
            attempt_id,
            source: source.source,
            source_sha256: source.source_sha256,
            observed_at: source.observed_at,
            prompt_version: PROMPT_VERSION.into(),
            parser_version: PARSER_VERSION.into(),
        }
    }

    async fn sources(
        &self,
        scope: &TenantScope,
        target: &ChannelTargetView,
        attempt_id: Uuid,
    ) -> Result<Vec<SavedSource>, AppError> {
        let target_id = target.target.target_id;
        let attempt = target
            .attempts
            .iter()
            .find(|a| a.attempt_id == attempt_id)
            .ok_or_else(|| AppError::not_found("measurement attempt not found"))?;
        let mut sources = Vec::new();
        for capture in self
            .captures
            .list_sources_for_attempt(scope, target_id, attempt_id)
            .await?
        {
            if let ObservationCaptureSnapshot::Source { source_sha256, .. } =
                &capture.input.snapshot
            {
                let source = SavedSource {
                    source: ObservationAnalysisSource::Capture {
                        capture_id: capture.input.capture_id,
                    },
                    source_sha256: source_sha256.clone(),
                    observed_at: capture.input.observed_at,
                };
                let request = Self::request(target_id, attempt_id, source.clone());
                if observation_analysis_source_json(scope, &request, target, Some(&capture)).is_ok()
                {
                    sources.push(source);
                }
            }
        }
        if let Some(outcome) = &attempt.outcome {
            for (index, evidence) in outcome.runner_evidence.iter().enumerate() {
                let Some(digest) = evidence["source_sha256"].as_str() else {
                    continue;
                };
                let observed_at = match evidence.get("observed_at") {
                    Some(value) => match serde_json::from_value(value.clone()) {
                        Ok(at) => at,
                        Err(_) => continue,
                    },
                    None => outcome.occurred_at,
                };
                let source = SavedSource {
                    source: ObservationAnalysisSource::AttemptEvidence {
                        evidence_index: index as u32,
                    },
                    source_sha256: digest.into(),
                    observed_at,
                };
                let request = Self::request(target_id, attempt_id, source.clone());
                if observation_analysis_source_json(scope, &request, target, None).is_ok()
                    && !sources
                        .iter()
                        .any(|prior| prior.source_sha256 == source.source_sha256)
                {
                    sources.push(source);
                }
            }
        }
        Ok(sources)
    }

    pub async fn submit(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        input: AnalyzeSavedSource,
    ) -> Result<ObservationAnalysisRevision, AppError> {
        let target = self.jobs.get_target(scope, target_id).await?;
        self.interrupt_stale(scope, target_id, attempt_id).await?;
        let source = self.sources(scope, &target, attempt_id).await?.into_iter()
            .find(|source| input.capture_id.is_none_or(|id| matches!(source.source, ObservationAnalysisSource::Capture { capture_id } if capture_id == id)))
            .ok_or_else(|| AppError::conflict("no saved source available for analysis"))?;
        let request = Self::request(target_id, attempt_id, source);
        let digest = sha256_hex(
            &serde_json::to_vec(&(
                target_id,
                attempt_id,
                &request.source,
                &request.source_sha256,
                request.observed_at,
                PROMPT_VERSION,
                PARSER_VERSION,
            ))
            .map_err(|_| AppError::invalid_request("invalid analysis request"))?,
        );
        self.repository
            .create(scope, &input.idempotency_key, &digest, request, Utc::now())
            .await
    }

    pub async fn execute(&self, scope: &TenantScope, revision_id: Uuid) -> Result<(), AppError> {
        let Some(claim) = self
            .repository
            .claim(scope, revision_id, Utc::now())
            .await?
        else {
            return Ok(());
        };
        let result = match tokio::time::timeout(
            Duration::from_secs(150),
            self.interpret(scope, &claim.revision.request),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => failed("analysis_source_unavailable"),
            Err(_) => failed("analysis_timeout"),
        };
        self.repository
            .finish(scope, revision_id, claim.claim_token, result, Utc::now())
            .await?;
        Ok(())
    }

    async fn interpret(
        &self,
        scope: &TenantScope,
        request: &ObservationAnalysisRequest,
    ) -> Result<ObservationAnalysisResult, AppError> {
        let target = self.jobs.get_target(scope, request.target_id).await?;
        let capture = match request.source {
            ObservationAnalysisSource::Capture { capture_id } => {
                self.captures.get(scope, capture_id).await?
            }
            ObservationAnalysisSource::AttemptEvidence { .. } => None,
        };
        let source = observation_analysis_source_json(scope, request, &target, capture.as_ref())?;
        let prompt = crate::observation_ai_callback::extraction_prompt(&source)
            .map_err(|_| AppError::invalid_request("invalid saved source"))?;
        let completion = self
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
            .await;
        let (completion, config_revision) = match completion {
            Ok(completion) => completion,
            // Classify only bridge-owned vocabulary. Never persist an error
            // message, upstream body, request, endpoint or credential.
            Err(error) => return Ok(failed(model_failure_code(&error))),
        };
        let output_reason = if completion.text.len() > 150_000 {
            "model_output_too_large"
        } else if completion.finish_reason != "stop" {
            "model_output_incomplete"
        } else if !completion.tool_calls.is_empty() {
            "model_output_tool_calls"
        } else {
            "model_output_invalid"
        };
        let mut result = ObservationAnalysisResult {
            config_revision: Some(config_revision),
            actual_model: Some(completion.model),
            candidate_json: None,
            outcome: ObservationAnalysisOutcome::Unverified {
                reason: output_reason.into(),
            },
            prompt_tokens: completion.prompt_tokens,
            completion_tokens: completion.completion_tokens,
        };
        if completion.text.len() <= 150_000
            && completion.finish_reason == "stop"
            && completion.tool_calls.is_empty()
        {
            let ChannelTargetInput::Measure {
                provider,
                model,
                surface,
                search_mode,
                protocol_version,
                market,
                language,
                ..
            } = &target.target.input
            else {
                return Err(AppError::invalid_request("measurement required"));
            };
            match self.grounder.ground(&source, &request.source_sha256, &completion.text,
                json!({ "provider":provider,"model":model,"surface":surface,"search_mode":search_mode,
                    "protocol_version":protocol_version,"market":market,"language":language })).await
            {
                Ok(grounded) => { result.candidate_json = grounded.candidate_json; result.outcome = grounded.outcome; }
                Err(_) => { result.outcome = ObservationAnalysisOutcome::Failed { code: "grounding_unavailable".into() }; }
            }
        }
        if result.validate().is_err() {
            // An invalid candidate/audit must not erase a model response that
            // was actually received. Retain its bounded model/config/usage
            // provenance while discarding the invalid interpretation payload.
            result.candidate_json = None;
            result.outcome = ObservationAnalysisOutcome::Failed {
                code: "analysis_result_invalid".into(),
            };
            if result.validate().is_err() {
                result.actual_model = None;
            }
        }
        Ok(result)
    }
}

fn model_failure_code(error: &HostOpError) -> &'static str {
    match error.code {
        HostOpErrorCode::InvalidRequest => "model_request_invalid",
        HostOpErrorCode::CapabilityMissing => "model_unconfigured",
        HostOpErrorCode::Denied => "model_access_denied",
        HostOpErrorCode::BudgetExceeded => "model_budget_exceeded",
        HostOpErrorCode::DeadlineExceeded => "model_timeout",
        HostOpErrorCode::Cancelled => "model_cancelled",
        HostOpErrorCode::Failed => {
            if let Some(status) = error
                .message
                .strip_prefix("model provider returned HTTP status ")
                .and_then(|status| status.parse::<u16>().ok())
            {
                return match status {
                    401 => "model_http_unauthorized",
                    403 => "model_http_forbidden",
                    408 | 504 => "model_http_timeout",
                    429 => "model_http_rate_limited",
                    500..=599 => "model_http_server_error",
                    _ => "model_http_rejected",
                };
            }
            match error.message.as_str() {
                "HTTP request failed"
                | "HTTP response failed"
                | "HTTP client initialization failed" => "model_transport_failed",
                "provider response exceeds the size limit" | "provider response is too large" => {
                    "model_response_too_large"
                }
                "project model settings unavailable" => "model_settings_unavailable",
                "response is missing id"
                | "response is missing model"
                | "response is missing choices"
                | "response is missing message"
                | "response contains malformed tool_calls"
                | "response contains empty tool_calls"
                | "response repeats a tool call id"
                | "response is missing text content"
                | "response has neither answer content nor tool calls"
                | "response is missing usage"
                | "usage is missing prompt_tokens"
                | "usage is missing completion_tokens"
                | "usage is missing total_tokens"
                | "provider response is not UTF-8" => "model_response_invalid",
                message if message.starts_with("invalid JSON:") => "model_response_invalid",
                _ => "model_failed",
            }
        }
        _ => "model_failed",
    }
}

fn failed(code: &str) -> ObservationAnalysisResult {
    ObservationAnalysisResult {
        config_revision: None,
        actual_model: None,
        candidate_json: None,
        outcome: ObservationAnalysisOutcome::Failed { code: code.into() },
        prompt_tokens: 0,
        completion_tokens: 0,
    }
}

fn service(state: &AppState) -> Result<&ObservationAnalysisService, AppError> {
    state
        .observation_analysis
        .as_ref()
        .ok_or_else(|| AppError::capability_missing("saved source analysis unavailable"))
}

async fn submit(
    State(state): State<AppState>,
    Path((project_id, target_id, attempt_id)): Path<(ProjectId, Uuid, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<AnalyzeSavedSource>,
) -> Result<(StatusCode, Json<ObservationAnalysisRevision>), ApiError> {
    let map = |e| api_error(e, context.request_id);
    crate::require_project_writer(&auth).map_err(map)?;
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(map)?;
    let service = service(&state).map_err(map)?.clone();
    let revision = service
        .submit(&scope, target_id, attempt_id, input)
        .await
        .map_err(map)?;
    let revision_id = revision.request.revision_id;
    tokio::spawn(async move {
        // Persisted running claims are never blindly reclaimed after restart.
        // Replaying a queued POST safely reaches the same atomic claim.
        if service.execute(&scope, revision_id).await.is_err() {
            tracing::warn!("saved observation analysis could not record completion");
        }
    });
    Ok((StatusCode::ACCEPTED, Json(revision)))
}

async fn list(
    State(state): State<AppState>,
    Path((project_id, target_id, attempt_id)): Path<(ProjectId, Uuid, Uuid)>,
    Query(query): Query<PageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<AnalysisPage>, ApiError> {
    let map = |e| api_error(e, context.request_id);
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(map)?;
    let service = service(&state).map_err(map)?;
    let target = service
        .jobs
        .get_target(&scope, target_id)
        .await
        .map_err(map)?;
    service
        .interrupt_stale(&scope, target_id, attempt_id)
        .await
        .map_err(map)?;
    let sources = service
        .sources(&scope, &target, attempt_id)
        .await
        .map_err(map)?;
    let limit = query.limit.unwrap_or(20);
    let _ = (query.tenant_id, query.project_id);
    if !(1..=99).contains(&limit) {
        return Err(map(AppError::invalid_request("limit must be 1 to 99")));
    }
    let mut items = service
        .repository
        .list_for_attempt(&scope, target_id, attempt_id, query.after, limit + 1)
        .await
        .map_err(map)?;
    let more = items.len() > limit;
    items.truncate(limit);
    let next_after = if more {
        items.last().map(|revision| revision.request.revision_id)
    } else {
        None
    };
    Ok(Json(AnalysisPage {
        sources,
        items,
        next_after,
    }))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/channel-targets/{target_id}/attempts/{attempt_id}/analyses",
            get(list).post(submit),
        )
        .layer(middleware::from_fn(crate::csrf_origin_from_request))
        .layer(middleware::from_fn(crate::auth_scope_from_request))
        .layer(middleware::from_fn(crate::no_store_middleware))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelProviderBridge, ProjectAiSettingsService};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header::SET_COOKIE},
    };
    use geo_domain::{
        ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID, MemoryObservationAnalysisRepository,
        MemoryObservationCaptureRepository, ObservationCaptureInput, ObservationProviderIdentity,
        ProjectCreate, ProjectSettings, StandaloneMeasurementPlan,
    };
    use geo_worker::{HostOpError, ModelCompletion};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    struct Model(AtomicUsize);
    #[async_trait::async_trait]
    impl ModelProviderBridge for Model {
        async fn complete(
            &self,
            _: &TenantScope,
            request: &ModelCompletionRequest,
        ) -> Result<ModelCompletion, HostOpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert!(request.prompt.contains("/messages/0"));
            assert!(request.tools.is_empty());
            assert!(request.model.is_none());
            Ok(ModelCompletion {
                model: "actual-synthetic-model".into(),
                text: "{\"complete\":true}".into(),
                finish_reason: "stop".into(),
                tool_calls: vec![],
                prompt_tokens: 12,
                completion_tokens: 4,
            })
        }
    }
    struct Grounder {
        unavailable: bool,
    }

    struct FailingModel(AtomicUsize);
    struct InvalidAuditGrounder;
    #[async_trait::async_trait]
    impl SavedObservationGrounder for InvalidAuditGrounder {
        async fn ground(
            &self,
            _: &str,
            _: &str,
            candidate: &str,
            _: Value,
        ) -> Result<GroundingResult, AppError> {
            Ok(GroundingResult {
                candidate_json: Some(candidate.into()),
                outcome: ObservationAnalysisOutcome::Grounded {
                    raw_answer: "Synthetic saved answer".into(),
                    citations: vec![],
                    audit: json!({"refs":[]}),
                },
            })
        }
    }

    #[tokio::test]
    async fn invalid_interpretation_preserves_received_model_config_and_usage() {
        let mut f = fixture(false, true, false).await;
        f.service.grounder = Arc::new(InvalidAuditGrounder);
        let queued = f
            .service
            .submit(
                &f.scope,
                f.target_id,
                f.attempt_id,
                AnalyzeSavedSource {
                    idempotency_key: "invalid-audit".into(),
                    capture_id: None,
                },
            )
            .await
            .unwrap();
        f.service
            .execute(&f.scope, queued.request.revision_id)
            .await
            .unwrap();
        let result = f
            .service
            .repository
            .get(&f.scope, queued.request.revision_id)
            .await
            .unwrap()
            .unwrap()
            .result
            .unwrap();
        assert_eq!(
            result.outcome,
            ObservationAnalysisOutcome::Failed {
                code: "analysis_result_invalid".into()
            }
        );
        assert_eq!(
            result.actual_model.as_deref(),
            Some("actual-synthetic-model")
        );
        assert_eq!(result.config_revision, Some(0));
        assert_eq!(result.prompt_tokens, 12);
        assert_eq!(result.completion_tokens, 4);
        assert!(result.candidate_json.is_none());
    }

    #[async_trait::async_trait]
    impl ModelProviderBridge for FailingModel {
        async fn complete(
            &self,
            _: &TenantScope,
            _: &ModelCompletionRequest,
        ) -> Result<ModelCompletion, HostOpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(HostOpError::failed(
                geo_worker::HostOp::ModelComplete,
                "model provider returned HTTP status 503",
            ))
        }
    }

    #[test]
    fn model_failures_use_fixed_codes_without_serializing_error_context() {
        use geo_worker::HostOp;
        for (message, expected) in [
            (
                "model provider returned HTTP status 401",
                "model_http_unauthorized",
            ),
            (
                "model provider returned HTTP status 429",
                "model_http_rate_limited",
            ),
            (
                "model provider returned HTTP status 503",
                "model_http_server_error",
            ),
            ("HTTP request failed", "model_transport_failed"),
            ("response is missing usage", "model_response_invalid"),
            (
                "invalid JSON: synthetic-private-response",
                "model_response_invalid",
            ),
            (
                "provider response exceeds the size limit",
                "model_response_too_large",
            ),
            (
                "project model settings unavailable",
                "model_settings_unavailable",
            ),
            ("synthetic-private-response", "model_failed"),
            (
                "model provider returned HTTP status 503 private-suffix",
                "model_failed",
            ),
        ] {
            let error = HostOpError::failed(HostOp::ModelComplete, message);
            assert_eq!(model_failure_code(&error), expected);
            let result = failed(model_failure_code(&error));
            result.validate().unwrap();
            let serialized = serde_json::to_string(&result).unwrap();
            assert!(!serialized.contains("synthetic-private"));
            assert!(!serialized.contains("private-suffix"));
            assert!(result.actual_model.is_none());
            assert!(result.config_revision.is_none());
        }
        assert_eq!(
            model_failure_code(&HostOpError::deadline_exceeded(
                HostOp::ModelComplete,
                60_000
            )),
            "model_timeout"
        );
        assert_eq!(
            model_failure_code(&HostOpError::denied(HostOp::ModelComplete, "synthetic")),
            "model_access_denied"
        );
    }

    #[tokio::test]
    async fn model_failure_is_terminal_without_inventing_response_provenance_or_retrying() {
        let mut f = fixture(false, true, false).await;
        let model = Arc::new(FailingModel(AtomicUsize::new(0)));
        f.service.model = Arc::new(
            ProjectConfiguredModelBridge::new(
                ProjectAiSettingsService::development(),
                Some(model.clone()),
            )
            .unwrap(),
        );
        let queued = f
            .service
            .submit(
                &f.scope,
                f.target_id,
                f.attempt_id,
                AnalyzeSavedSource {
                    idempotency_key: "model-failure".into(),
                    capture_id: None,
                },
            )
            .await
            .unwrap();
        f.service
            .execute(&f.scope, queued.request.revision_id)
            .await
            .unwrap();
        f.service
            .execute(&f.scope, queued.request.revision_id)
            .await
            .unwrap();
        let revision = f
            .service
            .repository
            .get(&f.scope, queued.request.revision_id)
            .await
            .unwrap()
            .unwrap();
        let result = revision.result.unwrap();
        assert_eq!(
            result.outcome,
            ObservationAnalysisOutcome::Failed {
                code: "model_http_server_error".into()
            }
        );
        assert!(result.actual_model.is_none());
        assert!(result.config_revision.is_none());
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.service
                .jobs
                .get_target(&f.scope, f.target_id)
                .await
                .unwrap(),
            f.original
        );
    }
    #[async_trait::async_trait]
    impl SavedObservationGrounder for Grounder {
        async fn ground(
            &self,
            source: &str,
            digest: &str,
            candidate: &str,
            protocol: Value,
        ) -> Result<GroundingResult, AppError> {
            assert_eq!(sha256_hex(source.as_bytes()), digest);
            assert_eq!(protocol["model"], "measured-model");
            if self.unavailable {
                return Err(AppError::capability_missing("unavailable"));
            }
            Ok(GroundingResult {
                candidate_json: Some(candidate.into()),
                outcome: ObservationAnalysisOutcome::Grounded {
                    raw_answer: "Synthetic saved answer".into(),
                    citations: vec![],
                    audit: json!({"refs":[{"path":"/messages/0/content","quote":"Synthetic saved answer"}],"source_sha256":digest}),
                },
            })
        }
    }
    struct Fixture {
        state: AppState,
        scope: TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        service: ObservationAnalysisService,
        model: Arc<Model>,
        original: ChannelTargetView,
    }
    async fn fixture(fixture_receipt: bool, identity_bound: bool, unavailable: bool) -> Fixture {
        fixture_with_detail(
            fixture_receipt,
            identity_bound,
            unavailable,
            "execution_deadline",
        )
        .await
    }

    async fn fixture_with_detail(
        fixture_receipt: bool,
        identity_bound: bool,
        unavailable: bool,
        detail: &str,
    ) -> Fixture {
        let state = AppState::development_with_password("analysis-test");
        let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
        let project = state
            .project_repository()
            .create(
                &tenant,
                ProjectCreate {
                    slug: None,
                    display_name: "Synthetic workspace".into(),
                    settings: ProjectSettings::default(),
                },
            )
            .await
            .unwrap();
        let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
        let jobs = state.channel_job_repository();
        let captures = Arc::new(MemoryObservationCaptureRepository::new(jobs.clone()));
        let repository = Arc::new(MemoryObservationAnalysisRepository::new(
            jobs.clone(),
            captures.clone(),
        ));
        let (target_id, attempt_id, account_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let now = Utc::now() - chrono::Duration::seconds(5);
        jobs.create_measurement_plan(
            &scope,
            "synthetic-plan",
            "synthetic-plan-digest",
            StandaloneMeasurementPlan {
                plan_id: Uuid::new_v4(),
                project_id: project.id,
                title: "Synthetic".into(),
                input_hash: "synthetic-plan-digest".into(),
                revision: 1,
                created_at: now,
                targets: vec![ChannelTarget {
                    target_id,
                    input: ChannelTargetInput::Measure {
                        account_id,
                        provider: "synthetic".into(),
                        model: "measured-model".into(),
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
        let source = r#"{"messages":[{"content":"Synthetic saved answer"}]}"#;
        captures
            .save(
                &scope,
                ObservationCaptureInput {
                    capture_id: Uuid::new_v4(),
                    target_id,
                    attempt_id,
                    account_id,
                    runner_session_id: Uuid::new_v4(),
                    original_identity: identity_bound.then(|| ObservationProviderIdentity {
                        provider: "synthetic".into(),
                        platform_account_id: "synthetic-account".into(),
                    }),
                    ordinal: 0,
                    observed_at: now + chrono::Duration::seconds(1),
                    snapshot: ObservationCaptureSnapshot::Source {
                        source_json: source.into(),
                        source_sha256: sha256_hex(source.as_bytes()),
                    },
                    owned_conversation: None,
                    completion: None,
                },
            )
            .await
            .unwrap();
        let original = jobs
            .finish(
                &scope,
                target_id,
                attempt_id,
                ChannelOutcome {
                    status: ChannelOutcomeStatus::Unknown,
                    detail: Some(detail.into()),
                    occurred_at: now,
                    raw_answer: None,
                    citations: vec![],
                    public_url: None,
                    screenshot_ref: None,
                    connector_version: None,
                    fixture: fixture_receipt,
                    runner_evidence: vec![json!({
                        "kind":"runner_receipt",
                        "schema_version":"geo.runner.receipt.v1",
                        "provenance":"unknown",
                        "execution_id":Uuid::new_v4(),
                        "connector_version":"synthetic.v1",
                        "occurred_at":null,
                    })],
                },
                now + chrono::Duration::seconds(2),
            )
            .await
            .unwrap();
        let model = Arc::new(Model(AtomicUsize::new(0)));
        let bridge = ProjectConfiguredModelBridge::new(
            ProjectAiSettingsService::development(),
            Some(model.clone()),
        )
        .unwrap();
        let service = ObservationAnalysisService::new(
            repository,
            jobs,
            captures,
            bridge,
            Arc::new(Grounder { unavailable }),
        );
        let state = state.with_observation_analysis(service.clone());
        Fixture {
            state,
            scope,
            target_id,
            attempt_id,
            service,
            model,
            original,
        }
    }

    #[tokio::test]
    async fn policy_timeout_capture_is_discoverable_and_reparsed_without_upgrading_unknown() {
        let f =
            fixture_with_detail(true, true, false, "official_search_observation_unverified").await;
        let sources = f
            .service
            .sources(&f.scope, &f.original, f.attempt_id)
            .await
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert!(matches!(
            sources[0].source,
            ObservationAnalysisSource::Capture { .. }
        ));
        let queued = f
            .service
            .submit(
                &f.scope,
                f.target_id,
                f.attempt_id,
                AnalyzeSavedSource {
                    idempotency_key: "policy-source".into(),
                    capture_id: None,
                },
            )
            .await
            .unwrap();
        f.service
            .execute(&f.scope, queued.request.revision_id)
            .await
            .unwrap();
        let revision = f
            .service
            .repository
            .get(&f.scope, queued.request.revision_id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            revision.result.unwrap().outcome,
            ObservationAnalysisOutcome::Grounded { .. }
        ));
        assert_eq!(f.model.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.service
                .jobs
                .get_target(&f.scope, f.target_id)
                .await
                .unwrap(),
            f.original
        );
        let receipt = f.original.attempts[0].outcome.as_ref().unwrap();
        assert_eq!(receipt.status, ChannelOutcomeStatus::Unknown);
        assert!(receipt.fixture);
        assert_eq!(
            receipt.detail.as_deref(),
            Some("official_search_observation_unverified")
        );
        let other = TenantScope::new(
            f.scope.operator_id,
            f.scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert!(
            f.service
                .sources(&other, &f.original, f.attempt_id)
                .await
                .unwrap()
                .is_empty()
        );
        let missing_identity =
            fixture_with_detail(true, false, false, "official_search_observation_unverified").await;
        assert!(
            missing_identity
                .service
                .sources(
                    &missing_identity.scope,
                    &missing_identity.original,
                    missing_identity.attempt_id
                )
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn saved_capture_survives_lost_response_and_analysis_never_changes_measurement() {
        let f = fixture(true, true, false).await;
        let input = || AnalyzeSavedSource {
            idempotency_key: "analysis-once".into(),
            capture_id: None,
        };
        let queued = f
            .service
            .submit(&f.scope, f.target_id, f.attempt_id, input())
            .await
            .unwrap();
        let replay = f
            .service
            .submit(&f.scope, f.target_id, f.attempt_id, input())
            .await
            .unwrap();
        assert_eq!(queued, replay);
        let id = queued.request.revision_id;
        let (a, b) = tokio::join!(
            f.service.execute(&f.scope, id),
            f.service.execute(&f.scope, id)
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(f.model.0.load(Ordering::SeqCst), 1);
        let finished = f
            .service
            .repository
            .get(&f.scope, id)
            .await
            .unwrap()
            .unwrap();
        let result = finished.result.unwrap();
        assert_eq!(
            result.actual_model.as_deref(),
            Some("actual-synthetic-model")
        );
        assert_eq!(result.config_revision, Some(0));
        assert!(matches!(
            result.outcome,
            ObservationAnalysisOutcome::Grounded { .. }
        ));
        assert_eq!(
            f.service
                .jobs
                .get_target(&f.scope, f.target_id)
                .await
                .unwrap(),
            f.original
        );
        let wrong_scope = TenantScope::new(
            f.scope.operator_id,
            f.scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert!(
            f.service
                .submit(&wrong_scope, f.target_id, f.attempt_id, input())
                .await
                .is_err()
        );
        let f = fixture(true, false, false).await;
        assert!(
            f.service
                .submit(&f.scope, f.target_id, f.attempt_id, input())
                .await
                .is_err()
        );
        assert_eq!(f.model.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn grounding_failure_is_terminal_and_preserves_actual_model_provenance() {
        let f = fixture(false, true, true).await;
        let queued = f
            .service
            .submit(
                &f.scope,
                f.target_id,
                f.attempt_id,
                AnalyzeSavedSource {
                    idempotency_key: "failure".into(),
                    capture_id: None,
                },
            )
            .await
            .unwrap();
        f.service
            .execute(&f.scope, queued.request.revision_id)
            .await
            .unwrap();
        let finished = f
            .service
            .repository
            .get(&f.scope, queued.request.revision_id)
            .await
            .unwrap()
            .unwrap();
        let result = finished.result.unwrap();
        assert_eq!(
            result.actual_model.as_deref(),
            Some("actual-synthetic-model")
        );
        assert_eq!(
            result.outcome,
            ObservationAnalysisOutcome::Failed {
                code: "grounding_unavailable".into()
            }
        );
        f.service
            .execute(&f.scope, queued.request.revision_id)
            .await
            .unwrap();
        assert_eq!(f.model.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.service
                .jobs
                .get_target(&f.scope, f.target_id)
                .await
                .unwrap(),
            f.original
        );
    }

    fn http(
        method: &str,
        path: &str,
        auth: Option<&(String, String)>,
        body: Value,
    ) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("{path}?tenant_id={DEVELOPMENT_TENANT_ID}"))
            .header("host", "localhost:8080")
            .header("origin", "http://localhost:5173")
            .header("content-type", "application/json");
        if let Some((cookie, csrf)) = auth {
            request = request
                .header("cookie", cookie)
                .header(crate::CSRF_HEADER, csrf);
        }
        request.body(Body::from(body.to_string())).unwrap()
    }
    async fn json_body(response: axum::response::Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn http_requires_auth_and_rejects_caller_source_or_model_overrides() {
        let f = fixture(false, true, false).await;
        let path = format!(
            "/api/v1/projects/{}/channel-targets/{}/attempts/{}/analyses",
            f.scope.project_id.unwrap(),
            f.target_id,
            f.attempt_id
        );
        let app = crate::router(f.state);
        assert_eq!(
            app.clone()
                .oneshot(http("GET", &path, None, json!({})))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let login = app
            .clone()
            .oneshot(http(
                "POST",
                "/api/v1/auth/login",
                None,
                json!({"login_name":"demo@localhost","password":"analysis-test"}),
            ))
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login.headers()[SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let auth = (
            cookie,
            json_body(login).await["csrf_token"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        let response = app
            .clone()
            .oneshot(http("GET", &path, Some(&auth), json!({})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await["sources"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        for field in [
            "source_json",
            "source_sha256",
            "model",
            "observed_at",
            "scope",
        ] {
            let mut body = json!({"idempotency_key":"key"});
            body[field] = json!("caller override");
            assert_eq!(
                app.clone()
                    .oneshot(http("POST", &path, Some(&auth), body))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        let response = app
            .oneshot(http(
                "POST",
                &path,
                Some(&auth),
                json!({"idempotency_key":"key"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(json_body(response).await["state"], "queued");
    }
}
