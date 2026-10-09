//! Service-only checkpoint for raw browser observations before interpretation.
//! A receipt proves local persistence, not a verified measurement or remote cleanup.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
    routing::post,
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, CapturedConversation, ObservationCaptureCompletion, ObservationCaptureInput,
    ObservationCaptureReceipt, ObservationCaptureRepository, ObservationCaptureSnapshot,
    ObservationProviderIdentity, TenantScope,
};
use geo_provider::SecretEnvelope;
use ring::{digest, hmac};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const TICKET_AAD: &[u8] = b"geo-observation-capture-ticket-v1";
const AUTH_MESSAGE: &[u8] = b"geo-observation-capture-service-v1";
const ID_DOMAIN: &[u8] = b"geo-observation-capture-id-v1";
// source_json is itself a JSON string: quoting can double its 750 kB
// bound, and the envelope still needs room for ticket and ownership metadata.
const MAX_BODY_BYTES: usize = 2_000_000;
const MAX_TICKET_CHARS: usize = 4096;
const MAX_TICKET_LIFETIME_SECONDS: i64 = 3600;
pub(crate) const CALLBACK_PATH: &str = "/internal/v1/observation-captures";

/// Opt-in deployment assembly; owns service credentials, so intentionally not Debug.
#[derive(Clone)]
pub struct ObservationCaptureCallbackService {
    repository: Arc<dyn ObservationCaptureRepository>,
    cipher: Arc<SecretEnvelope>,
    bearer_tag: hmac::Tag,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ticket {
    version: u8,
    scope: TenantScope,
    target_id: Uuid,
    attempt_id: Uuid,
    account_id: Uuid,
    runner_session_id: Uuid,
    original_identity: ObservationProviderIdentity,
    expires_at: DateTime<Utc>,
}

/// Immutable preflight account and attempt selected by trusted Rust code.
pub struct ObservationCaptureBinding {
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    pub account_id: Uuid,
    pub runner_session_id: Uuid,
    pub original_identity: ObservationProviderIdentity,
}

/// Scope, target, account, attempt and runner identity never come from runner JSON.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRequest {
    schema_version: u8,
    capture_ticket: String,
    ordinal: u32,
    observed_at: DateTime<Utc>,
    snapshot: ObservationCaptureSnapshot,
    #[serde(default)]
    owned_conversation: Option<CapturedConversation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion: Option<ObservationCaptureCompletion>,
}

impl ObservationCaptureCallbackService {
    pub fn new(
        repository: Arc<dyn ObservationCaptureRepository>,
        cipher: Arc<SecretEnvelope>,
        bearer: &str,
    ) -> Result<Self, AppError> {
        if !(32..=512).contains(&bearer.len())
            || !bearer.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(AppError::invalid_request(
                "observation callback service credential is invalid",
            ));
        }
        Ok(Self {
            repository,
            cipher,
            bearer_tag: hmac::sign(
                &hmac::Key::new(hmac::HMAC_SHA256, bearer.as_bytes()),
                AUTH_MESSAGE,
            ),
        })
    }

    /// Trusted Rust execution path only, after the attempt and account have
    /// been selected. Tickets do not change the attempt's durable state.
    pub fn issue_ticket(
        &self,
        scope: &TenantScope,
        binding: ObservationCaptureBinding,
        expires_at: DateTime<Utc>,
    ) -> Result<String, AppError> {
        let ObservationCaptureBinding {
            target_id,
            attempt_id,
            account_id,
            runner_session_id,
            original_identity,
        } = binding;
        original_identity.validate()?;
        // PostgreSQL timestamp persistence is microsecond-precision. Seal that
        // precision so retries do not differ solely by clock serialization.
        let expires_at = DateTime::from_timestamp_micros(expires_at.timestamp_micros())
            .ok_or_else(|| AppError::invalid_request("observation callback expiry invalid"))?;
        let now = Utc::now();
        if scope.project_id.is_none()
            || [target_id, attempt_id, account_id, runner_session_id]
                .iter()
                .any(Uuid::is_nil)
            || expires_at <= now
            || expires_at > now + chrono::Duration::seconds(MAX_TICKET_LIFETIME_SECONDS)
        {
            return Err(AppError::invalid_request(
                "observation callback binding invalid",
            ));
        }
        let ticket = Ticket {
            version: 1,
            scope: scope.clone(),
            target_id,
            attempt_id,
            account_id,
            runner_session_id,
            original_identity,
            expires_at,
        };
        let plaintext = serde_json::to_vec(&ticket)
            .map_err(|_| AppError::conflict("observation callback ticket unavailable"))?;
        self.cipher
            .seal(TICKET_AAD, &plaintext)
            .map(hex::encode)
            .map_err(|_| AppError::conflict("observation callback ticket unavailable"))
    }

    pub(crate) fn authenticate(&self, headers: &axum::http::HeaderMap) -> bool {
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        let Some(bearer) = value.strip_prefix("Bearer ") else {
            return false;
        };
        if !(32..=512).contains(&bearer.len()) {
            return false;
        }
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, bearer.as_bytes()),
            AUTH_MESSAGE,
            self.bearer_tag.as_ref(),
        )
        .is_ok()
    }

    /// Resolve only ordinal-zero source bytes from the exact sealed execution.
    /// Neither scope nor prompt nor model selection is accepted from a caller.
    pub(crate) async fn load_bound_source(
        &self,
        capture_ticket: &str,
        source_capture_id: Uuid,
        source_sha256: &str,
    ) -> Result<(TenantScope, String), StatusCode> {
        if capture_ticket.len() > MAX_TICKET_CHARS || source_sha256.len() != 64 {
            return Err(StatusCode::BAD_REQUEST);
        }
        let sealed = hex::decode(capture_ticket).map_err(|_| StatusCode::FORBIDDEN)?;
        let plaintext = self
            .cipher
            .open(TICKET_AAD, &sealed)
            .map_err(|_| StatusCode::FORBIDDEN)?;
        let ticket: Ticket =
            serde_json::from_slice(&plaintext).map_err(|_| StatusCode::FORBIDDEN)?;
        if ticket.version != 1
            || ticket.scope.project_id.is_none()
            || ticket.expires_at <= Utc::now()
            || [
                ticket.target_id,
                ticket.attempt_id,
                ticket.account_id,
                ticket.runner_session_id,
            ]
            .iter()
            .any(Uuid::is_nil)
            || capture_id(&ticket, 0) != source_capture_id
        {
            return Err(StatusCode::FORBIDDEN);
        }
        let capture = self
            .repository
            .get(&ticket.scope, source_capture_id)
            .await
            .map_err(status)?
            .ok_or(StatusCode::NOT_FOUND)?;
        let expected_capture_digest = capture.input.validate(&ticket.scope).map_err(status)?;
        let input = capture.input;
        if input.capture_id != source_capture_id
            || input.ordinal != 0
            || input.target_id != ticket.target_id
            || input.attempt_id != ticket.attempt_id
            || input.account_id != ticket.account_id
            || input.runner_session_id != ticket.runner_session_id
            || input.original_identity.as_ref() != Some(&ticket.original_identity)
            || capture.receipt.capture_id != source_capture_id
            || capture.receipt.digest_sha256 != expected_capture_digest
        {
            return Err(StatusCode::FORBIDDEN);
        }
        match input.snapshot {
            ObservationCaptureSnapshot::Source {
                source_json,
                source_sha256: saved_digest,
            } if saved_digest == source_sha256
                && geo_domain::sha256_hex(source_json.as_bytes()) == source_sha256 =>
            {
                Ok((ticket.scope, source_json))
            }
            _ => Err(StatusCode::FORBIDDEN),
        }
    }

    async fn capture(
        &self,
        request: CaptureRequest,
    ) -> Result<Json<ObservationCaptureReceipt>, StatusCode> {
        if request.schema_version != 1 || request.capture_ticket.len() > MAX_TICKET_CHARS {
            return Err(StatusCode::BAD_REQUEST);
        }
        let sealed = hex::decode(&request.capture_ticket).map_err(|_| StatusCode::FORBIDDEN)?;
        let plaintext = self
            .cipher
            .open(TICKET_AAD, &sealed)
            .map_err(|_| StatusCode::FORBIDDEN)?;
        let ticket: Ticket =
            serde_json::from_slice(&plaintext).map_err(|_| StatusCode::FORBIDDEN)?;
        if ticket.version != 1
            || ticket.scope.project_id.is_none()
            || ticket.expires_at <= Utc::now()
            || [
                ticket.target_id,
                ticket.attempt_id,
                ticket.account_id,
                ticket.runner_session_id,
            ]
            .iter()
            .any(Uuid::is_nil)
        {
            return Err(StatusCode::FORBIDDEN);
        }
        let input = ObservationCaptureInput {
            capture_id: capture_id(&ticket, request.ordinal),
            target_id: ticket.target_id,
            attempt_id: ticket.attempt_id,
            account_id: ticket.account_id,
            runner_session_id: ticket.runner_session_id,
            original_identity: Some(ticket.original_identity),
            ordinal: request.ordinal,
            observed_at: DateTime::from_timestamp_micros(request.observed_at.timestamp_micros())
                .ok_or(StatusCode::BAD_REQUEST)?,
            snapshot: request.snapshot,
            owned_conversation: request.owned_conversation,
            completion: request.completion,
        };
        // Repository checks exact persisted attempt/account and commits bytes
        // before yielding a receipt. Interpretation or remote deletion is not
        // performed by this callback.
        let expected_digest = input.validate(&ticket.scope).map_err(status)?;
        let receipt = self
            .repository
            .save(&ticket.scope, input.clone())
            .await
            .map_err(status)?;
        if receipt.capture_id != input.capture_id
            || receipt.schema_version != 1
            || receipt.digest_sha256 != expected_digest
        {
            return Err(StatusCode::CONFLICT);
        }
        Ok(Json(receipt))
    }
}

fn capture_id(ticket: &Ticket, ordinal: u32) -> Uuid {
    let mut context = digest::Context::new(&digest::SHA256);
    context.update(ID_DOMAIN);
    context.update(ticket.scope.operator_id.as_uuid().as_bytes());
    context.update(ticket.scope.tenant_id.as_uuid().as_bytes());
    context.update(
        ticket
            .scope
            .project_id
            .expect("checked project")
            .as_uuid()
            .as_bytes(),
    );
    context.update(ticket.target_id.as_bytes());
    context.update(ticket.attempt_id.as_bytes());
    context.update(ticket.account_id.as_bytes());
    context.update(ticket.runner_session_id.as_bytes());
    context.update(&ordinal.to_be_bytes());
    let hash = context.finish();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.as_ref()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80; // UUID version 8: application-defined digest.
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn status(error: AppError) -> StatusCode {
    // Never reflect raw evidence, private target bindings or SQL details.
    StatusCode::from_u16(error.code.default_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

async fn service_auth(
    State(service): State<ObservationCaptureCallbackService>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if !service.authenticate(request.headers()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

async fn capture(
    State(service): State<ObservationCaptureCallbackService>,
    Json(request): Json<CaptureRequest>,
) -> Result<Json<ObservationCaptureReceipt>, StatusCode> {
    service.capture(request).await
}

pub(crate) fn routes(service: Option<ObservationCaptureCallbackService>) -> Router {
    let Some(service) = service else {
        return Router::new();
    };
    Router::new()
        .route(CALLBACK_PATH, post(capture))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
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
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use geo_domain::{
        ErrorCode, ObservationCapture, ObservationCaptureSnapshot, OperatorId, ProjectId, TenantId,
    };
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    const BEARER: &str = "synthetic-observation-service-credential";

    #[derive(Default)]
    struct Store {
        records: Mutex<HashMap<Uuid, (TenantScope, ObservationCapture)>>,
        saves: AtomicUsize,
        fail: AtomicBool,
    }

    #[async_trait::async_trait]
    impl ObservationCaptureRepository for Store {
        async fn save(
            &self,
            scope: &TenantScope,
            input: ObservationCaptureInput,
        ) -> Result<ObservationCaptureReceipt, AppError> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "synthetic private store detail",
                ));
            }
            let digest_sha256 = input.validate(scope)?;
            let mut records = self.records.lock().await;
            if let Some((old_scope, old)) = records.get(&input.capture_id) {
                return if old_scope == scope && old.input == input {
                    Ok(old.receipt.clone())
                } else {
                    Err(AppError::conflict("capture identity differs"))
                };
            }
            if let ObservationCaptureSnapshot::Candidate {
                source_capture_id, ..
            }
            | ObservationCaptureSnapshot::Extraction {
                source_capture_id, ..
            } = &input.snapshot
                && !records
                    .get(source_capture_id)
                    .is_some_and(|(source_scope, source)| {
                        source_scope == scope
                            && source.input.attempt_id == input.attempt_id
                            && matches!(
                                source.input.snapshot,
                                ObservationCaptureSnapshot::Source { .. }
                            )
                    })
            {
                return Err(AppError::conflict("source capture missing"));
            }
            let receipt = ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256,
                stored_at: Utc::now(),
            };
            records.insert(
                input.capture_id,
                (
                    scope.clone(),
                    ObservationCapture {
                        input,
                        receipt: receipt.clone(),
                    },
                ),
            );
            Ok(receipt)
        }

        async fn get(
            &self,
            scope: &TenantScope,
            capture_id: Uuid,
        ) -> Result<Option<ObservationCapture>, AppError> {
            Ok(self
                .records
                .lock()
                .await
                .get(&capture_id)
                .filter(|(stored_scope, _)| stored_scope == scope)
                .map(|(_, capture)| capture.clone()))
        }
    }

    struct Fixture {
        service: ObservationCaptureCallbackService,
        store: Arc<Store>,
        scope: TenantScope,
        body: serde_json::Value,
    }

    fn fixture() -> Fixture {
        fixture_for_provider("synthetic")
    }

    fn fixture_for_provider(provider: &str) -> Fixture {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let store = Arc::new(Store::default());
        let service = ObservationCaptureCallbackService::new(
            store.clone(),
            Arc::new(SecretEnvelope::ephemeral()),
            BEARER,
        )
        .unwrap();
        let ticket = service
            .issue_ticket(
                &scope,
                ObservationCaptureBinding {
                    target_id: Uuid::new_v4(),
                    attempt_id: Uuid::new_v4(),
                    account_id: Uuid::new_v4(),
                    runner_session_id: Uuid::new_v4(),
                    original_identity: ObservationProviderIdentity {
                        provider: provider.into(),
                        platform_account_id: "synthetic-account".into(),
                    },
                },
                Utc::now() + chrono::Duration::minutes(5),
            )
            .unwrap();
        let source_json =
            r#"{"messages":[{"text":"synthetic answer"}],"rendered_text":"synthetic answer"}"#;
        let body = serde_json::json!({
            "schema_version": 1,
            "capture_ticket": ticket,
            "ordinal": 0,
            "observed_at": Utc::now().to_rfc3339(),
            "snapshot": {
                "phase": "source",
                "source_json": source_json,
                "source_sha256": geo_domain::sha256_hex(source_json.as_bytes()),
            }
        });
        Fixture {
            service,
            store,
            scope,
            body,
        }
    }

    fn request(body: &serde_json::Value, bearer: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(CALLBACK_PATH)
            .header("content-type", "application/json");
        if let Some(bearer) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {bearer}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    async fn submit(
        service: &ObservationCaptureCallbackService,
        body: &serde_json::Value,
        bearer: Option<&str>,
    ) -> (StatusCode, String) {
        let response = routes(Some(service.clone()))
            .oneshot(request(body, bearer))
            .await
            .unwrap();
        let status = response.status();
        let text = String::from_utf8(
            to_bytes(response.into_body(), MAX_BODY_BYTES)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        (status, text)
    }

    #[tokio::test]
    async fn model_source_is_bound_to_sealed_scope_attempt_session_and_digest() {
        let fixture = fixture();
        let (_, text) = submit(&fixture.service, &fixture.body, Some(BEARER)).await;
        let receipt: ObservationCaptureReceipt = serde_json::from_str(&text).unwrap();
        let ticket = fixture.body["capture_ticket"].as_str().unwrap();
        let source_digest = fixture.body["snapshot"]["source_sha256"].as_str().unwrap();
        let (scope, source) = fixture
            .service
            .load_bound_source(ticket, receipt.capture_id, source_digest)
            .await
            .unwrap();
        assert_eq!(scope, fixture.scope);
        assert_eq!(
            source,
            fixture.body["snapshot"]["source_json"].as_str().unwrap()
        );
        assert_eq!(
            fixture
                .service
                .load_bound_source(ticket, receipt.capture_id, &"0".repeat(64))
                .await
                .unwrap_err(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            fixture
                .service
                .load_bound_source(ticket, Uuid::new_v4(), source_digest)
                .await
                .unwrap_err(),
            StatusCode::FORBIDDEN
        );
        let sealed: Ticket = serde_json::from_slice(
            &fixture
                .service
                .cipher
                .open(TICKET_AAD, &hex::decode(ticket).unwrap())
                .unwrap(),
        )
        .unwrap();
        for field in ["scope", "attempt", "session", "account", "expiry"] {
            let mut changed: Ticket =
                serde_json::from_value(serde_json::to_value(&sealed).unwrap()).unwrap();
            match field {
                "scope" => changed.scope.project_id = Some(ProjectId::new(Uuid::new_v4())),
                "attempt" => changed.attempt_id = Uuid::new_v4(),
                "session" => changed.runner_session_id = Uuid::new_v4(),
                "account" => changed.account_id = Uuid::new_v4(),
                _ => changed.expires_at = Utc::now() - chrono::Duration::seconds(1),
            }
            let ticket = hex::encode(
                fixture
                    .service
                    .cipher
                    .seal(TICKET_AAD, &serde_json::to_vec(&changed).unwrap())
                    .unwrap(),
            );
            assert_eq!(
                fixture
                    .service
                    .load_bound_source(&ticket, receipt.capture_id, source_digest)
                    .await
                    .unwrap_err(),
                StatusCode::FORBIDDEN
            );
        }
    }

    #[tokio::test]
    async fn source_is_durable_and_replayed_with_same_receipt() {
        let fixture = fixture();
        let (status, text) = submit(&fixture.service, &fixture.body, Some(BEARER)).await;
        assert_eq!(status, StatusCode::OK);
        let receipt: ObservationCaptureReceipt = serde_json::from_str(&text).unwrap();
        let capture = fixture
            .store
            .get(&fixture.scope, receipt.capture_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(capture.receipt, receipt);
        assert_eq!(
            capture.input.original_identity,
            Some(ObservationProviderIdentity {
                provider: "synthetic".into(),
                platform_account_id: "synthetic-account".into(),
            })
        );
        assert_eq!(
            capture.input.observed_at.timestamp_subsec_nanos() % 1_000,
            0
        );
        let (retry_status, retry_text) =
            submit(&fixture.service, &fixture.body, Some(BEARER)).await;
        assert_eq!(retry_status, StatusCode::OK);
        assert_eq!(retry_text, text);
        assert_eq!(fixture.store.saves.load(Ordering::SeqCst), 2);

        let mut changed = fixture.body.clone();
        changed["observed_at"] =
            serde_json::json!((Utc::now() + chrono::Duration::seconds(3)).to_rfc3339());
        assert_eq!(
            submit(&fixture.service, &changed, Some(BEARER)).await.0,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn completion_is_forwarded_and_bound_to_sealed_provider_identity() {
        for provider in ["kimi", "synthetic"] {
            let fixture = fixture_for_provider(provider);
            let mut request = fixture.body.clone();
            let source_json = serde_json::json!({"messages": [
                {"chat": {"id": "synthetic-chat"}},
                {"message": {"id": "synthetic-message", "chat_id": "synthetic-chat", "role": "assistant", "status": "COMPLETED"}}
            ]}).to_string();
            request["snapshot"]["source_sha256"] =
                serde_json::json!(geo_domain::sha256_hex(source_json.as_bytes()));
            request["snapshot"]["source_json"] = serde_json::json!(source_json);
            request["owned_conversation"] = serde_json::json!({
                "provider": provider, "external_conversation_id": "synthetic-chat",
                "purpose": "measurement", "correlation": "create_response"
            });
            request["completion"] = serde_json::json!({
                "protocol": "connect_json", "terminal": true,
                "assistant_message_ids": ["synthetic-message"]
            });
            let (status, body) = submit(&fixture.service, &request, Some(BEARER)).await;
            if provider != "kimi" {
                assert_eq!(status, StatusCode::BAD_REQUEST);
                continue;
            }
            assert_eq!(status, StatusCode::OK);
            let receipt: ObservationCaptureReceipt = serde_json::from_str(&body).unwrap();
            let stored = fixture
                .store
                .get(&fixture.scope, receipt.capture_id)
                .await
                .unwrap()
                .unwrap();
            assert!(stored.has_complete_conversation_evidence(&fixture.scope));
            assert_eq!(
                serde_json::to_value(stored.input.completion).unwrap(),
                request["completion"]
            );
            request["completion"]["terminal"] = serde_json::json!(false);
            assert_eq!(
                submit(&fixture.service, &request, Some(BEARER)).await.0,
                StatusCode::BAD_REQUEST
            );
            request["completion"]["protocol"] = serde_json::json!("unknown");
            assert_eq!(
                submit(&fixture.service, &request, Some(BEARER)).await.0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
    }

    #[tokio::test]
    async fn candidate_checkpoint_retains_long_quotes_offsets_and_rejected_shapes() {
        for candidate in [
            serde_json::json!({
                "decision": "searched_answer",
                "answer_segments": [
                    {"path": "/rendered_text", "quote": "原文 excerpt ".repeat(400)},
                    {"path": "/rendered_text", "start": 0, "end": 20}
                ]
            }),
            serde_json::json!({"decision": "unexpected_shape"}),
        ] {
            let fixture = fixture();
            let (status, body) = submit(&fixture.service, &fixture.body, Some(BEARER)).await;
            assert_eq!(status, StatusCode::OK);
            let source: ObservationCaptureReceipt = serde_json::from_str(&body).unwrap();
            let candidate_json = candidate.to_string();
            let mut request = fixture.body.clone();
            request["ordinal"] = serde_json::json!(1);
            request["snapshot"] = serde_json::json!({
                "phase": "candidate",
                "source_capture_id": source.capture_id,
                "route": "signed_in_browser",
                "candidate_sha256": geo_domain::sha256_hex(candidate_json.as_bytes()),
                "candidate_json": candidate_json,
                "grounding_reason": "shape_rejected"
            });
            let (status, body) = submit(&fixture.service, &request, Some(BEARER)).await;
            assert_eq!(status, StatusCode::OK);
            let receipt: ObservationCaptureReceipt = serde_json::from_str(&body).unwrap();
            let stored = fixture
                .store
                .get(&fixture.scope, receipt.capture_id)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                stored.input.snapshot,
                ObservationCaptureSnapshot::Candidate { candidate_json: stored, .. }
                    if stored == candidate_json
            ));
        }
    }

    #[tokio::test]
    async fn candidate_requires_source_and_independent_ordinal() {
        let fixture = fixture();
        let ticket: Ticket = serde_json::from_slice(
            &fixture
                .service
                .cipher
                .open(
                    TICKET_AAD,
                    &hex::decode(fixture.body["capture_ticket"].as_str().unwrap()).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        let source_id = capture_id(&ticket, 0);
        let candidate_json = r#"{"decision":"unverified"}"#;
        let mut candidate = fixture.body.clone();
        candidate["ordinal"] = serde_json::json!(1);
        candidate["snapshot"] = serde_json::json!({
            "phase": "candidate",
            "source_capture_id": source_id,
            "route": "signed_in_browser",
            "candidate_json": candidate_json,
            "candidate_sha256": geo_domain::sha256_hex(candidate_json.as_bytes()),
            "grounding_reason": "model_unverified",
        });
        assert_eq!(
            submit(&fixture.service, &candidate, Some(BEARER)).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            submit(&fixture.service, &fixture.body, Some(BEARER))
                .await
                .0,
            StatusCode::OK
        );
        let (status, text) = submit(&fixture.service, &candidate, Some(BEARER)).await;
        assert_eq!(status, StatusCode::OK);
        let receipt: ObservationCaptureReceipt = serde_json::from_str(&text).unwrap();
        assert_eq!(receipt.capture_id, capture_id(&ticket, 1));
        assert_ne!(receipt.capture_id, source_id);
    }

    #[tokio::test]
    async fn service_auth_binding_schema_and_store_failure_fail_closed() {
        let fixture = fixture();
        assert_eq!(
            submit(&fixture.service, &fixture.body, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            submit(&fixture.service, &fixture.body, Some("wrong"))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let mut extra = fixture.body.clone();
        extra["account_id"] = serde_json::json!(Uuid::new_v4());
        assert_eq!(
            submit(&fixture.service, &extra, Some(BEARER)).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut identity_override = fixture.body.clone();
        identity_override["original_identity"] = serde_json::json!({
            "provider": "synthetic", "platform_account_id": "different-account"
        });
        assert_eq!(
            submit(&fixture.service, &identity_override, Some(BEARER))
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut wrong_provider = fixture.body.clone();
        wrong_provider["owned_conversation"] = serde_json::json!({
            "provider": "other",
            "external_conversation_id": "synthetic-chat",
            "purpose": "measurement",
            "correlation": "create_response"
        });
        assert_eq!(
            submit(&fixture.service, &wrong_provider, Some(BEARER))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let mut bad_ticket = fixture.body.clone();
        bad_ticket["capture_ticket"] = serde_json::json!("bad");
        assert_eq!(
            submit(&fixture.service, &bad_ticket, Some(BEARER)).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(fixture.store.saves.load(Ordering::SeqCst), 0);
        fixture.store.fail.store(true, Ordering::SeqCst);
        let (status, text) = submit(&fixture.service, &fixture.body, Some(BEARER)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!text.contains("synthetic private"));
        assert_eq!(fixture.store.records.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn absent_service_and_oversize_body_do_not_persist() {
        let fixture = fixture();
        assert_eq!(
            routes(None)
                .oneshot(request(&fixture.body, Some(BEARER)))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
        );
        let mut oversize = fixture.body.clone();
        oversize["snapshot"]["source_json"] = serde_json::json!("x".repeat(MAX_BODY_BYTES));
        assert_eq!(
            submit(&fixture.service, &oversize, Some(BEARER)).await.0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(fixture.store.saves.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ticket_cannot_be_replayed_with_a_different_secret_or_past_expiry() {
        let fixture = fixture();
        let foreign = ObservationCaptureCallbackService::new(
            fixture.store.clone(),
            Arc::new(SecretEnvelope::ephemeral()),
            BEARER,
        )
        .unwrap();
        assert_eq!(
            submit(&foreign, &fixture.body, Some(BEARER)).await.0,
            StatusCode::FORBIDDEN
        );
        let ticket: Ticket = serde_json::from_slice(
            &fixture
                .service
                .cipher
                .open(
                    TICKET_AAD,
                    &hex::decode(fixture.body["capture_ticket"].as_str().unwrap()).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        let mut expired = fixture.body.clone();
        let old = Ticket {
            expires_at: Utc::now() - chrono::Duration::seconds(1),
            ..ticket
        };
        expired["capture_ticket"] = serde_json::json!(hex::encode(
            fixture
                .service
                .cipher
                .seal(TICKET_AAD, &serde_json::to_vec(&old).unwrap())
                .unwrap()
        ));
        assert_eq!(
            submit(&fixture.service, &expired, Some(BEARER)).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(fixture.store.saves.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ticket_rejects_missing_scope_and_long_lifetime() {
        let fixture = fixture();
        let absent_project =
            TenantScope::new(fixture.scope.operator_id, fixture.scope.tenant_id, None);
        let ticket: Ticket = serde_json::from_slice(
            &fixture
                .service
                .cipher
                .open(
                    TICKET_AAD,
                    &hex::decode(fixture.body["capture_ticket"].as_str().unwrap()).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        let execution_expiry = Utc::now() + geo_domain::CHANNEL_MEASUREMENT_LEASE;
        let execution_ticket = fixture
            .service
            .issue_ticket(
                &fixture.scope,
                ObservationCaptureBinding {
                    target_id: ticket.target_id,
                    attempt_id: ticket.attempt_id,
                    account_id: ticket.account_id,
                    runner_session_id: ticket.runner_session_id,
                    original_identity: ticket.original_identity.clone(),
                },
                execution_expiry,
            )
            .unwrap();
        let decoded: Ticket = serde_json::from_slice(
            &fixture
                .service
                .cipher
                .open(TICKET_AAD, &hex::decode(execution_ticket).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            decoded.expires_at.timestamp_micros(),
            execution_expiry.timestamp_micros()
        );
        assert!(
            fixture
                .service
                .issue_ticket(
                    &absent_project,
                    ObservationCaptureBinding {
                        target_id: ticket.target_id,
                        attempt_id: ticket.attempt_id,
                        account_id: ticket.account_id,
                        runner_session_id: ticket.runner_session_id,
                        original_identity: ticket.original_identity.clone(),
                    },
                    Utc::now() + chrono::Duration::minutes(1),
                )
                .is_err()
        );
        assert!(
            fixture
                .service
                .issue_ticket(
                    &fixture.scope,
                    ObservationCaptureBinding {
                        target_id: ticket.target_id,
                        attempt_id: ticket.attempt_id,
                        account_id: ticket.account_id,
                        runner_session_id: ticket.runner_session_id,
                        original_identity: ticket.original_identity,
                    },
                    Utc::now() + chrono::Duration::hours(2),
                )
                .is_err()
        );
    }
}
