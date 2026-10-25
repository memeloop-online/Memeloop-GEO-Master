//! Service-only interpretation of an exact persisted browser source.
//! Rust owns credentials and routing; the existing JS adapter prepares prompts.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    routing::post,
};
use geo_domain::ProjectAiUsage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ObservationCaptureCallbackService, ObservationExtractionService, ProjectAiSettingsService,
};

const POLICY_PATH: &str = "/internal/v1/observation-ai/policy";
const EXTRACT_PATH: &str = "/internal/v1/observation-ai/extract";

#[derive(Clone)]
pub struct ObservationAiCallbackService {
    capture: ObservationCaptureCallbackService,
    settings: ProjectAiSettingsService,
    extraction: ObservationExtractionService,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRequest {
    schema_version: u8,
    capture_ticket: String,
    source_capture_id: Uuid,
    source_sha256: String,
    prompt_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractRequest {
    schema_version: u8,
    capture_ticket: String,
    source_capture_id: Uuid,
    source_sha256: String,
    prompt_version: String,
}

#[derive(Serialize)]
struct Policy {
    prefer_connected_account: bool,
    config_version: i64,
}

#[derive(Serialize)]
struct ExtractionAccepted {
    status: &'static str,
    intent_id: Uuid,
}

impl ObservationAiCallbackService {
    pub fn new(
        capture: ObservationCaptureCallbackService,
        settings: ProjectAiSettingsService,
        extraction: ObservationExtractionService,
    ) -> Self {
        Self {
            capture,
            settings,
            extraction,
        }
    }

    async fn source(
        &self,
        request: &SourceRequest,
    ) -> Result<(geo_domain::TenantScope, String), StatusCode> {
        if request.schema_version != 1 {
            return Err(StatusCode::BAD_REQUEST);
        }
        self.capture
            .load_bound_source(
                &request.capture_ticket,
                request.source_capture_id,
                &request.source_sha256,
            )
            .await
    }
}

async fn policy(
    State(service): State<ObservationAiCallbackService>,
    Json(request): Json<SourceRequest>,
) -> Result<Json<Policy>, StatusCode> {
    let total = std::time::Instant::now();
    policy_timing("entered", total, true);
    let result = async {
        // This includes repository read AND validation; do not label it DB-only.
        let started = std::time::Instant::now();
        let source = service.source(&request).await;
        policy_timing("source_load_validate", started, source.is_ok());
        let (scope, _) = source?;
        // get also resolves inherited model metadata when applicable.
        let started = std::time::Instant::now();
        let view = service
            .settings
            .get(&scope, ProjectAiUsage::ObservationAnalysis)
            .await;
        policy_timing("settings_get", started, view.is_ok());
        let view = view.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        Ok(Json(Policy {
            prefer_connected_account: view.prefer_connected_account,
            config_version: view.revision,
        }))
    }
    .await;
    policy_timing("total", total, result.is_ok());
    result
}

fn policy_timing(stage: &'static str, started: std::time::Instant, success: bool) {
    tracing::info!(
        event = "observation_policy_timing",
        stage,
        elapsed_ms = started.elapsed().as_millis() as u64,
        success,
        "observation policy timing"
    );
}

#[tracing::instrument(name = "observation_extract_callback", skip_all, fields(call_id = %Uuid::new_v4()))]
async fn extract(
    State(service): State<ObservationAiCallbackService>,
    Json(request): Json<ExtractRequest>,
) -> Result<(StatusCode, Json<ExtractionAccepted>), StatusCode> {
    let request = SourceRequest {
        schema_version: request.schema_version,
        capture_ticket: request.capture_ticket,
        source_capture_id: request.source_capture_id,
        source_sha256: request.source_sha256,
        prompt_version: Some(request.prompt_version),
    };
    let (scope, _) = service.source(&request).await?;
    let version = request
        .prompt_version
        .as_deref()
        .ok_or(StatusCode::BAD_REQUEST)?;
    if version != "geo.observation.extract.v5" {
        return Err(StatusCode::BAD_REQUEST);
    }
    let intent = service
        .extraction
        .enqueue_source(
            &scope,
            request.source_capture_id,
            &request.source_sha256,
            version,
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ExtractionAccepted {
            status: "accepted",
            intent_id: intent.intent_id,
        }),
    ))
}

async fn service_auth(
    State(service): State<ObservationAiCallbackService>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if !service.capture.authenticate(request.headers()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

pub(crate) fn routes(service: Option<ObservationAiCallbackService>) -> Router {
    let Some(service) = service else {
        return Router::new();
    };
    Router::new()
        .route(POLICY_PATH, post(policy))
        .route(EXTRACT_PATH, post(extract))
        .layer(DefaultBodyLimit::max(8192))
        .layer(middleware::from_fn_with_state(
            service.clone(),
            service_auth,
        ))
        .layer(middleware::from_fn(crate::no_store_middleware))
        .with_state(service)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelProviderBridge, ProjectConfiguredModelBridge};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header::AUTHORIZATION},
    };
    use geo_domain::{
        AppError, ObservationCapture, ObservationCaptureInput, ObservationCaptureReceipt,
        ObservationCaptureRepository, TenantScope,
    };
    use geo_provider::SecretEnvelope;
    use geo_worker::{HostOpError, ModelCompletion, ModelCompletionRequest};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tower::ServiceExt;

    const BEARER: &str = "synthetic-observation-service-credential";
    struct Preparation;
    #[async_trait::async_trait]
    impl crate::SavedObservationGrounder for Preparation {
        async fn prepare(
            &self,
            source: &str,
            digest: &str,
            prompt_version: &str,
            parser_version: &str,
        ) -> Result<crate::PreparedObservationPrompt, AppError> {
            assert_eq!(geo_domain::sha256_hex(source.as_bytes()), digest);
            assert_eq!(prompt_version, "geo.observation.extract.v5");
            assert_eq!(parser_version, "geo.observation.extract.v7");
            Ok(crate::PreparedObservationPrompt {
                prompt: "synthetic prepared observation prompt\n".into(),
                source_sha256: digest.into(),
                prompt_version: prompt_version.into(),
                parser_version: parser_version.into(),
            })
        }

        async fn ground(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: serde_json::Value,
            _: &str,
            _: &str,
        ) -> Result<crate::GroundingResult, AppError> {
            unreachable!("callback only checkpoints inference")
        }
    }

    #[derive(Default)]
    struct Store(tokio::sync::Mutex<Option<(TenantScope, ObservationCapture)>>);
    #[async_trait::async_trait]
    impl ObservationCaptureRepository for Store {
        async fn scan_sources(
            &self,
            _: chrono::DateTime<chrono::Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<geo_domain::ObservationCaptureSourceItem>, AppError> {
            unreachable!("callback-only test store")
        }
        async fn save(
            &self,
            scope: &TenantScope,
            input: ObservationCaptureInput,
        ) -> Result<ObservationCaptureReceipt, AppError> {
            let receipt = ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: input.validate(scope)?,
                stored_at: chrono::Utc::now(),
            };
            *self.0.lock().await = Some((
                scope.clone(),
                ObservationCapture {
                    input,
                    receipt: receipt.clone(),
                },
            ));
            Ok(receipt)
        }
        async fn get_source(
            &self,
            scope: &TenantScope,
            capture_id: Uuid,
        ) -> Result<Option<geo_domain::ObservationCaptureSource>, AppError> {
            self.get(scope, capture_id)
                .await?
                .as_ref()
                .map(geo_domain::ObservationCaptureSource::try_from)
                .transpose()
        }
        async fn get(
            &self,
            scope: &TenantScope,
            id: Uuid,
        ) -> Result<Option<ObservationCapture>, AppError> {
            Ok(self
                .0
                .lock()
                .await
                .as_ref()
                .filter(|(saved_scope, saved)| saved_scope == scope && saved.input.capture_id == id)
                .map(|(_, saved)| saved.clone()))
        }
    }
    #[derive(Default)]
    struct Model(AtomicUsize);
    #[async_trait::async_trait]
    impl ModelProviderBridge for Model {
        async fn complete(
            &self,
            _: &TenantScope,
            request: &ModelCompletionRequest,
        ) -> Result<ModelCompletion, HostOpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.prompt, "synthetic prepared observation prompt\n");
            assert!(request.model.is_none());
            assert!(request.tools.is_empty());
            tokio::time::sleep(std::time::Duration::from_secs(56)).await;
            Ok(ModelCompletion {
                text: "{\"completion\":\"unknown\"}".into(),
                model: "actual-model".into(),
                finish_reason: "stop".into(),
                tool_calls: vec![],
                prompt_tokens: 1,
                completion_tokens: 1,
            })
        }
    }

    async fn post_request(
        router: Router,
        path: &str,
        body: serde_json::Value,
        bearer: Option<&str>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(bearer) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {bearer}"));
        }
        let response = router
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        (
            status,
            String::from_utf8(
                to_bytes(response.into_body(), 160_000)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap(),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn callbacks_require_auth_and_exact_source_and_accept_before_slow_inference() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let store = Arc::new(Store::default());
        let capture = ObservationCaptureCallbackService::new(
            store.clone(),
            Arc::new(SecretEnvelope::ephemeral()),
            BEARER,
        )
        .unwrap();
        let ticket = capture
            .issue_ticket(
                &scope,
                crate::observation_capture_callback::ObservationCaptureBinding {
                    target_id: Uuid::new_v4(),
                    attempt_id: Uuid::new_v4(),
                    account_id: Uuid::new_v4(),
                    runner_session_id: Uuid::new_v4(),
                    original_identity: geo_domain::ObservationProviderIdentity {
                        provider: "synthetic".into(),
                        platform_account_id: "synthetic-account".into(),
                    },
                },
                chrono::Utc::now() + chrono::Duration::minutes(5),
            )
            .unwrap();
        let source = r#"{"messages":[{"text":"synthetic answer"}]}"#;
        let (status, receipt) = post_request(crate::observation_capture_callback::routes(Some(capture.clone())), crate::observation_capture_callback::CALLBACK_PATH, serde_json::json!({
            "schema_version":1, "capture_ticket":ticket, "ordinal":0, "observed_at":chrono::Utc::now(),
            "snapshot":{"phase":"source","source_json":source,"source_sha256":geo_domain::sha256_hex(source.as_bytes())}
        }), Some(BEARER)).await;
        assert_eq!(status, StatusCode::OK);
        let receipt: ObservationCaptureReceipt = serde_json::from_str(&receipt).unwrap();
        let settings = ProjectAiSettingsService::development();
        let model = Arc::new(Model::default());
        let bridge =
            ProjectConfiguredModelBridge::new(settings.clone(), Some(model.clone())).unwrap();
        let gate = geo_domain::MemoryObservationAdmissionGate::default();
        let analyses = Arc::new(
            geo_domain::MemoryObservationAnalysisRepository::new(
                Arc::new(geo_domain::MemoryChannelJobRepository::default()),
                store.clone(),
            )
            .with_admission_gate(gate.clone()),
        );
        let extraction = ObservationExtractionService::new(
            Arc::new(
                geo_domain::MemoryObservationExtractionRepository::new(store.clone())
                    .with_automatic_admission(gate, analyses),
            ),
            store,
            bridge,
            Arc::new(Preparation),
        );
        let dispatcher = crate::spawn_observation_extraction_dispatcher(extraction.clone());
        let service = ObservationAiCallbackService::new(capture, settings, extraction.clone());
        let body = serde_json::json!({"schema_version":1,"capture_ticket":ticket,"source_capture_id":receipt.capture_id,"source_sha256":geo_domain::sha256_hex(source.as_bytes()),"prompt_version":"geo.observation.extract.v5"});
        for path in [POLICY_PATH, EXTRACT_PATH] {
            for bearer in [None, Some("wrong")] {
                assert_eq!(
                    post_request(routes(Some(service.clone())), path, body.clone(), bearer)
                        .await
                        .0,
                    StatusCode::UNAUTHORIZED
                );
            }
            for field in ["prompt", "scope", "model", "api_key", "base_url"] {
                let mut injected = body.clone();
                injected[field] = serde_json::json!("synthetic-private-override");
                let (status, text) =
                    post_request(routes(Some(service.clone())), path, injected, Some(BEARER)).await;
                assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
                assert!(!text.contains("synthetic-private-override"));
            }
            let mut mismatch = body.clone();
            mismatch["source_sha256"] = serde_json::json!("0".repeat(64));
            assert_eq!(
                post_request(routes(Some(service.clone())), path, mismatch, Some(BEARER))
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        let (status, policy) = post_request(
            routes(Some(service.clone())),
            POLICY_PATH,
            body.clone(),
            Some(BEARER),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&policy).unwrap(),
            serde_json::json!({"prefer_connected_account":true,"config_version":0})
        );
        for version in [
            "geo.observation.extract.v3",
            "geo.observation.extract.future",
        ] {
            let mut invalid = body.clone();
            invalid["prompt_version"] = serde_json::json!(version);
            assert_eq!(
                post_request(
                    routes(Some(service.clone())),
                    EXTRACT_PATH,
                    invalid,
                    Some(BEARER)
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        let mut obsolete = body.clone();
        obsolete["wait_budget_ms"] = serde_json::json!(60_000);
        assert_eq!(
            post_request(
                routes(Some(service.clone())),
                EXTRACT_PATH,
                obsolete,
                Some(BEARER)
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let started = tokio::time::Instant::now();
        let (status, result) = post_request(
            routes(Some(service.clone())),
            EXTRACT_PATH,
            body.clone(),
            Some(BEARER),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(started.elapsed(), std::time::Duration::ZERO);
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        let intent_id: Uuid = serde_json::from_value(result["intent_id"].clone()).unwrap();
        assert_eq!(
            result,
            serde_json::json!({"status":"accepted","intent_id":intent_id})
        );
        assert!(
            extraction
                .wait_response(&scope, intent_id, std::time::Duration::ZERO)
                .await
                .unwrap()
                .is_none()
        );
        let (status, duplicate) =
            post_request(routes(Some(service)), EXTRACT_PATH, body, Some(BEARER)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&duplicate).unwrap(),
            result
        );
        assert_eq!(started.elapsed(), std::time::Duration::ZERO);
        let response = extraction
            .wait_response(&scope, intent_id, std::time::Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.model_text, "{\"completion\":\"unknown\"}");
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        dispatcher.abort();
    }
}
