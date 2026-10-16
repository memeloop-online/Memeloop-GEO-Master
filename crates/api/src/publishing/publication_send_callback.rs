//! Private service callback for the one-shot rich-publication send boundary.
//! This is not human approval and does not enable the browser transport.

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
    AppError, AuthorizePublicationSend, PublicationSendAuthorizationRepository,
    PublicationSendDecision, RegisterPublicationSend, TenantScope,
};
use geo_provider::SecretEnvelope;
use ring::hmac;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const TICKET_AAD: &[u8] = b"geo-publication-send-callback-ticket-v1";
const AUTH_MESSAGE: &[u8] = b"geo-publication-send-callback-service-v1";
const MAX_BODY_BYTES: usize = 8192;
const MAX_TICKET_CHARS: usize = 4096;
const MAX_TICKET_LIFETIME_SECONDS: i64 = 300;
pub(crate) const CALLBACK_PATH: &str = "/internal/v1/publication-send/authorize";

/// Explicit assembly only. No Debug implementation: this owns credentials.
#[derive(Clone)]
pub struct PublicationSendCallbackService {
    repository: Arc<dyn PublicationSendAuthorizationRepository>,
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
    publication_intent_id: Uuid,
    payload_hash: String,
    encrypted_binding_sha256: String,
    send_not_after: DateTime<Utc>,
}

// Deliberately not a public/OpenAPI DTO. Scope and account come only from the
// authenticated ticket, never request headers, cookies, or runner JSON.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizeRequest {
    schema_version: u8,
    callback_ticket: String,
    attempt_id: Uuid,
    runner_session_id: Uuid,
    payload_hash: String,
}

#[derive(Serialize)]
struct GrantResponse {
    status: &'static str,
    attempt_id: Uuid,
    runner_session_id: Uuid,
    payload_hash: String,
    send_not_after: DateTime<Utc>,
}

impl PublicationSendCallbackService {
    /// The bearer must be independently generated deployment configuration,
    /// never a browser cookie, model argument, or account credential.
    pub fn new(
        repository: Arc<dyn PublicationSendAuthorizationRepository>,
        cipher: Arc<SecretEnvelope>,
        bearer: &str,
    ) -> Result<Self, AppError> {
        if !(32..=512).contains(&bearer.len())
            || !bearer.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(AppError::invalid_request(
                "publication callback service credential is invalid",
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

    /// Trusted Rust preflight only, after committing the original encrypted
    /// execution binding. Registration does not grant permission to send.
    /// The caller must retain the original deadline on recovery; minting a
    /// replacement ticket cannot reset the repository's one-shot transition.
    pub async fn register_ticket(
        &self,
        scope: &TenantScope,
        expected: &AuthorizePublicationSend,
        send_not_after: DateTime<Utc>,
    ) -> Result<String, AppError> {
        // PostgreSQL timestamptz persists microseconds, not nanoseconds.
        // Seal exactly the same deadline that the repository can return:
        // otherwise a successful one-shot transition could be rejected only
        // after its permission was irreversibly consumed.
        let send_not_after = DateTime::from_timestamp_micros(send_not_after.timestamp_micros())
            .ok_or_else(|| AppError::invalid_request("publication callback deadline is invalid"))?;
        let now = Utc::now();
        if scope.project_id.is_none()
            || send_not_after <= now
            || send_not_after > now + chrono::Duration::seconds(MAX_TICKET_LIFETIME_SECONDS)
            || !is_digest(&expected.payload_hash)
            || !is_digest(&expected.encrypted_binding_sha256)
        {
            return Err(AppError::invalid_request(
                "publication callback binding is invalid",
            ));
        }
        let ticket = Ticket {
            version: 1,
            scope: scope.clone(),
            target_id: expected.target_id,
            attempt_id: expected.attempt_id,
            account_id: expected.account_id,
            runner_session_id: expected.runner_session_id,
            publication_intent_id: expected.publication_intent_id,
            payload_hash: expected.payload_hash.clone(),
            encrypted_binding_sha256: expected.encrypted_binding_sha256.clone(),
            send_not_after,
        };
        let plaintext = serde_json::to_vec(&ticket)
            .map_err(|_| AppError::conflict("publication callback ticket unavailable"))?;
        let sealed = self
            .cipher
            .seal(TICKET_AAD, &plaintext)
            .map_err(|_| AppError::conflict("publication callback ticket unavailable"))?;
        self.repository
            .register_publication_send(
                scope,
                &RegisterPublicationSend {
                    target_id: expected.target_id,
                    attempt_id: expected.attempt_id,
                    account_id: expected.account_id,
                    runner_session_id: expected.runner_session_id,
                    encrypted_binding_sha256: expected.encrypted_binding_sha256.clone(),
                    send_not_after,
                },
            )
            .await?;
        Ok(hex::encode(sealed))
    }

    fn authenticate(&self, headers: &axum::http::HeaderMap) -> bool {
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
        // Maintained ring verification is constant-time for the fixed-size MAC;
        // no homegrown byte equality or early-exit credential comparison.
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, bearer.as_bytes()),
            AUTH_MESSAGE,
            self.bearer_tag.as_ref(),
        )
        .is_ok()
    }

    async fn authorize(
        &self,
        request: AuthorizeRequest,
    ) -> Result<Json<GrantResponse>, StatusCode> {
        if request.schema_version != 1
            || request.callback_ticket.len() > MAX_TICKET_CHARS
            || !is_digest(&request.payload_hash)
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let sealed = hex::decode(&request.callback_ticket).map_err(|_| StatusCode::FORBIDDEN)?;
        let plaintext = self
            .cipher
            .open(TICKET_AAD, &sealed)
            .map_err(|_| StatusCode::FORBIDDEN)?;
        let ticket: Ticket =
            serde_json::from_slice(&plaintext).map_err(|_| StatusCode::FORBIDDEN)?;
        if ticket.version != 1
            || ticket.scope.project_id.is_none()
            || ticket.send_not_after <= Utc::now()
            || ticket.attempt_id != request.attempt_id
            || ticket.runner_session_id != request.runner_session_id
            || ticket.payload_hash != request.payload_hash
        {
            return Err(StatusCode::FORBIDDEN);
        }
        let expected = AuthorizePublicationSend {
            target_id: ticket.target_id,
            attempt_id: ticket.attempt_id,
            account_id: ticket.account_id,
            runner_session_id: ticket.runner_session_id,
            publication_intent_id: ticket.publication_intent_id,
            payload_hash: ticket.payload_hash,
            encrypted_binding_sha256: ticket.encrypted_binding_sha256,
        };
        // The repository rechecks the actual attempt, account, source/media
        // use, and persisted deadline in its authoritative transaction.
        match self
            .repository
            .authorize_publication_send(&ticket.scope, &expected)
            .await
        {
            Ok(PublicationSendDecision::Granted(grant)) => {
                // Fail closed if an incorrectly assembled repository returns
                // authority for anything other than this ticket's execution.
                if grant.attempt_id != expected.attempt_id
                    || grant.runner_session_id != expected.runner_session_id
                    || grant.payload_hash != expected.payload_hash
                    || grant.send_not_after != ticket.send_not_after
                    || grant.send_not_after <= Utc::now()
                {
                    return Err(StatusCode::CONFLICT);
                }
                Ok(Json(GrantResponse {
                    status: "granted",
                    attempt_id: grant.attempt_id,
                    runner_session_id: grant.runner_session_id,
                    payload_hash: grant.payload_hash,
                    send_not_after: grant.send_not_after,
                }))
            }
            // No permission on retry, including a lost first response.
            Ok(PublicationSendDecision::AlreadyConsumed) => Err(StatusCode::CONFLICT),
            // Do not serialize repository details or confidential bindings.
            Err(error) => Err(StatusCode::from_u16(error.code.default_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)),
        }
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn service_auth(
    State(service): State<PublicationSendCallbackService>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if !service.authenticate(request.headers()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

async fn authorize(
    State(service): State<PublicationSendCallbackService>,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<GrantResponse>, StatusCode> {
    service.authorize(request).await
}

pub(crate) fn routes(service: Option<PublicationSendCallbackService>) -> Router {
    let Some(service) = service else {
        return Router::new();
    };
    Router::new()
        .route(CALLBACK_PATH, post(authorize))
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
    use geo_domain::{OperatorId, ProjectId, PublicationSendGrant, TenantId};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tower::ServiceExt;

    const TEST_BEARER: &str = "synthetic-service-credential-for-callback-tests";

    struct Repository {
        scope: TenantScope,
        expected: AuthorizePublicationSend,
        deadline: DateTime<Utc>,
        calls: AtomicUsize,
        consumed: AtomicBool,
        deny: AtomicBool,
    }

    #[async_trait::async_trait]
    impl PublicationSendAuthorizationRepository for Repository {
        async fn register_publication_send(
            &self,
            scope: &TenantScope,
            registration: &RegisterPublicationSend,
        ) -> Result<(), AppError> {
            assert_eq!(scope, &self.scope);
            assert_eq!(registration.target_id, self.expected.target_id);
            assert_eq!(registration.attempt_id, self.expected.attempt_id);
            assert_eq!(registration.account_id, self.expected.account_id);
            assert_eq!(
                registration.runner_session_id,
                self.expected.runner_session_id
            );
            assert_eq!(
                registration.encrypted_binding_sha256,
                self.expected.encrypted_binding_sha256
            );
            assert_eq!(
                registration.send_not_after,
                DateTime::from_timestamp_micros(self.deadline.timestamp_micros()).unwrap()
            );
            Ok(())
        }

        async fn authorize_publication_send(
            &self,
            scope: &TenantScope,
            expected: &AuthorizePublicationSend,
        ) -> Result<PublicationSendDecision, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(scope, &self.scope);
            assert_eq!(expected, &self.expected);
            if self.deny.load(Ordering::SeqCst) {
                return Err(AppError::conflict("synthetic private repository detail"));
            }
            if self.consumed.swap(true, Ordering::SeqCst) {
                return Ok(PublicationSendDecision::AlreadyConsumed);
            }
            Ok(PublicationSendDecision::Granted(PublicationSendGrant {
                attempt_id: expected.attempt_id,
                runner_session_id: expected.runner_session_id,
                payload_hash: expected.payload_hash.clone(),
                // Simulate a PostgreSQL timestamptz round trip, even when
                // the trusted caller supplied a nanosecond-precision value.
                send_not_after: DateTime::from_timestamp_micros(self.deadline.timestamp_micros())
                    .unwrap(),
            }))
        }
    }

    async fn fixture() -> (
        PublicationSendCallbackService,
        Arc<Repository>,
        serde_json::Value,
    ) {
        let repository = Arc::new(Repository {
            scope: TenantScope::new(
                OperatorId::new(Uuid::new_v4()),
                TenantId::new(Uuid::new_v4()),
                Some(ProjectId::new(Uuid::new_v4())),
            ),
            expected: AuthorizePublicationSend {
                target_id: Uuid::new_v4(),
                attempt_id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                runner_session_id: Uuid::new_v4(),
                publication_intent_id: Uuid::new_v4(),
                payload_hash: geo_domain::sha256_hex(b"synthetic payload"),
                encrypted_binding_sha256: geo_domain::sha256_hex(b"synthetic binding"),
            },
            deadline: DateTime::from_timestamp(Utc::now().timestamp() + 120, 123_456_789).unwrap(),
            calls: AtomicUsize::new(0),
            consumed: AtomicBool::new(false),
            deny: AtomicBool::new(false),
        });
        let service = PublicationSendCallbackService::new(
            repository.clone(),
            Arc::new(SecretEnvelope::ephemeral()),
            TEST_BEARER,
        )
        .unwrap();
        let ticket = service
            .register_ticket(&repository.scope, &repository.expected, repository.deadline)
            .await
            .unwrap();
        let request = serde_json::json!({
            "schema_version": 1,
            "callback_ticket": ticket,
            "attempt_id": repository.expected.attempt_id,
            "runner_session_id": repository.expected.runner_session_id,
            "payload_hash": repository.expected.payload_hash,
        });
        (service, repository, request)
    }

    fn request(body: &serde_json::Value, bearer: Option<&str>) -> Request<Body> {
        let mut request = Request::builder()
            .method("POST")
            .uri(CALLBACK_PATH)
            .header("content-type", "application/json");
        if let Some(bearer) = bearer {
            request = request.header(AUTHORIZATION, format!("Bearer {bearer}"));
        }
        request.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn nanosecond_deadline_is_sealed_and_registered_at_postgres_precision() {
        let (service, repository, body) = fixture().await;
        assert_eq!(repository.deadline.timestamp_subsec_nanos(), 123_456_789);
        let sealed = hex::decode(body["callback_ticket"].as_str().unwrap()).unwrap();
        let ticket: Ticket =
            serde_json::from_slice(&service.cipher.open(TICKET_AAD, &sealed).unwrap()).unwrap();
        assert_eq!(ticket.send_not_after.timestamp_subsec_nanos(), 123_456_000);
        assert!(ticket.send_not_after < repository.deadline);
        let response = routes(Some(service))
            .oneshot(request(&body, Some(TEST_BEARER)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let grant: serde_json::Value = serde_json::from_slice(
            &to_bytes(response.into_body(), MAX_BODY_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(grant["status"], "granted");
        assert_eq!(
            grant["send_not_after"],
            serde_json::to_value(ticket.send_not_after).unwrap()
        );
        assert!(repository.consumed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn callback_request_schema_is_versioned_and_has_no_legacy_aliases() {
        let (service, repository, body) = fixture().await;
        let app = routes(Some(service));
        for version in [0, 2] {
            let mut changed = body.clone();
            changed["schema_version"] = version.into();
            assert_eq!(
                app.clone()
                    .oneshot(request(&changed, Some(TEST_BEARER)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        for field in ["schema_version", "callback_ticket"] {
            let mut changed = body.clone();
            changed.as_object_mut().unwrap().remove(field);
            assert_eq!(
                app.clone()
                    .oneshot(request(&changed, Some(TEST_BEARER)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        let mut legacy = body.clone();
        legacy["ticket"] = legacy
            .as_object_mut()
            .unwrap()
            .remove("callback_ticket")
            .unwrap();
        assert_eq!(
            app.oneshot(request(&legacy, Some(TEST_BEARER)))
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(repository.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn callback_is_opt_in_and_not_in_public_openapi() {
        let response = crate::router(crate::AppState::development())
            .oneshot(request(&serde_json::json!({}), None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let (service, repository, body) = fixture().await;
        let app =
            crate::router(crate::AppState::development().with_publication_send_callback(service));
        // No Host scope headers, browser cookie, CSRF header, or Origin.
        let response = app
            .clone()
            .oneshot(request(&body, Some(TEST_BEARER)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let grant: serde_json::Value = serde_json::from_slice(
            &to_bytes(response.into_body(), MAX_BODY_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            grant,
            serde_json::json!({
                "status": "granted",
                "attempt_id": repository.expected.attempt_id,
                "runner_session_id": repository.expected.runner_session_id,
                "payload_hash": repository.expected.payload_hash,
                "send_not_after": DateTime::from_timestamp_micros(repository.deadline.timestamp_micros()).unwrap(),
            })
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(CALLBACK_PATH));
    }

    #[tokio::test]
    async fn service_auth_precedes_json_and_rejects_browser_or_duplicate_auth() {
        let (service, repository, body) = fixture().await;
        let app = routes(Some(service));
        for bearer in [None, Some("incorrect-service-credential-of-matching-size")] {
            let mut req = request(&body, bearer);
            req.headers_mut()
                .insert("cookie", "geo_session=synthetic".parse().unwrap());
            assert_eq!(
                app.clone().oneshot(req).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let mut duplicate = request(&body, Some(TEST_BEARER));
        duplicate.headers_mut().append(
            AUTHORIZATION,
            format!("Bearer {TEST_BEARER}").parse().unwrap(),
        );
        assert_eq!(
            app.clone().oneshot(duplicate).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let invalid_json = Request::builder()
            .method("POST")
            .uri(CALLBACK_PATH)
            .header("content-type", "application/json")
            .body(Body::from("{"))
            .unwrap();
        assert_eq!(
            app.oneshot(invalid_json).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(repository.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn callback_rejects_tampering_expiry_and_runner_scope_selection() {
        let (service, repository, body) = fixture().await;
        let app = routes(Some(service.clone()));
        for field in [
            "attempt_id",
            "runner_session_id",
            "payload_hash",
            "callback_ticket",
        ] {
            let mut changed = body.clone();
            changed[field] = match field {
                "payload_hash" => geo_domain::sha256_hex(b"other").into(),
                "callback_ticket" => "00".repeat(48).into(),
                _ => Uuid::new_v4().to_string().into(),
            };
            assert_eq!(
                app.clone()
                    .oneshot(request(&changed, Some(TEST_BEARER)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        for field in [
            "scope",
            "tenant_id",
            "project_id",
            "account_id",
            "send_not_after",
            "target_id",
        ] {
            let mut changed = body.clone();
            changed[field] = Uuid::new_v4().to_string().into();
            assert_eq!(
                app.clone()
                    .oneshot(request(&changed, Some(TEST_BEARER)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        let sealed = hex::decode(body["callback_ticket"].as_str().unwrap()).unwrap();
        let mut ticket: Ticket =
            serde_json::from_slice(&service.cipher.open(TICKET_AAD, &sealed).unwrap()).unwrap();
        ticket.send_not_after = Utc::now() - chrono::Duration::seconds(1);
        let mut expired = body.clone();
        expired["callback_ticket"] = hex::encode(
            service
                .cipher
                .seal(TICKET_AAD, &serde_json::to_vec(&ticket).unwrap())
                .unwrap(),
        )
        .into();
        assert_eq!(
            app.oneshot(request(&expired, Some(TEST_BEARER)))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(repository.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn one_grant_under_concurrency_and_no_replay_after_response_loss_or_restart() {
        let (service, repository, body) = fixture().await;
        let app = routes(Some(service.clone()));
        let (first, second) = tokio::join!(
            app.clone().oneshot(request(&body, Some(TEST_BEARER))),
            app.oneshot(request(&body, Some(TEST_BEARER))),
        );
        let mut statuses = [
            first.unwrap().status().as_u16(),
            second.unwrap().status().as_u16(),
        ];
        statuses.sort();
        assert_eq!(statuses, [200, 409]);
        // Discard both responses and reconstruct process-local service state.
        let restarted = PublicationSendCallbackService::new(
            repository.clone(),
            service.cipher.clone(),
            TEST_BEARER,
        )
        .unwrap();
        let mut replacement = body.clone();
        replacement["callback_ticket"] = restarted
            .register_ticket(&repository.scope, &repository.expected, repository.deadline)
            .await
            .unwrap()
            .into();
        let app = routes(Some(restarted));
        let response = app
            .clone()
            .oneshot(request(&body, Some(TEST_BEARER)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            app.oneshot(request(&replacement, Some(TEST_BEARER)))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn bounded_requests_and_authority_failures_never_return_permission_or_details() {
        let (service, repository, body) = fixture().await;
        let app = routes(Some(service));
        let mut oversized = body.clone();
        oversized["callback_ticket"] = "a".repeat(MAX_BODY_BYTES).into();
        assert_eq!(
            app.clone()
                .oneshot(request(&oversized, Some(TEST_BEARER)))
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        oversized["callback_ticket"] = "a".repeat(MAX_TICKET_CHARS + 1).into();
        assert_eq!(
            app.clone()
                .oneshot(request(&oversized, Some(TEST_BEARER)))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(repository.calls.load(Ordering::SeqCst), 0);
        repository.deny.store(true, Ordering::SeqCst);
        let response = app
            .oneshot(request(&body, Some(TEST_BEARER)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(!repository.consumed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn registration_and_cipher_are_bounded_and_purpose_separated() {
        let (service, repository, body) = fixture().await;
        for deadline in [
            Utc::now() - chrono::Duration::seconds(1),
            Utc::now() + chrono::Duration::seconds(MAX_TICKET_LIFETIME_SECONDS + 5),
        ] {
            assert!(
                service
                    .register_ticket(&repository.scope, &repository.expected, deadline)
                    .await
                    .is_err()
            );
        }
        assert!(
            PublicationSendCallbackService::new(repository.clone(), service.cipher.clone(), "",)
                .is_err()
        );
        let sealed = hex::decode(body["callback_ticket"].as_str().unwrap()).unwrap();
        assert!(service.cipher.open(b"geo-channel-v1", &sealed).is_err());
        let different_key = PublicationSendCallbackService::new(
            repository.clone(),
            Arc::new(SecretEnvelope::ephemeral()),
            TEST_BEARER,
        )
        .unwrap();
        assert_eq!(
            routes(Some(different_key))
                .oneshot(request(&body, Some(TEST_BEARER)))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(repository.calls.load(Ordering::SeqCst), 0);
    }
}
