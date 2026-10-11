//! Service-only deletion recheck under the existing shared account reservation.
//! This authorizes one exact remote resource, never a user's whole history.
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
    routing::post,
};
use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    AppError, ProviderCleanupAction, ProviderCleanupClaim, ProviderConversationCleanupRepository,
    TenantScope,
};
use geo_provider::SecretEnvelope;
use ring::hmac;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const AAD: &[u8] = b"geo-provider-cleanup-ticket-v1";
const AUTH_MESSAGE: &[u8] = b"geo-provider-cleanup-service-v1";
const MAX_BODY_BYTES: usize = 8192;
const MAX_TICKET_CHARS: usize = 4096;
pub(crate) const CALLBACK_PATH: &str = "/internal/v1/provider-conversation-cleanup/authorize";

#[derive(Clone)]
pub struct ProviderCleanupCallbackService {
    repository: Arc<dyn ProviderConversationCleanupRepository>,
    cipher: Arc<SecretEnvelope>,
    bearer_tag: hmac::Tag,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ticket {
    version: u8,
    scope: TenantScope,
    claim: ProviderCleanupClaim,
    reservation_id: Uuid,
    runner_session_id: Uuid,
    expires_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizeRequest {
    schema_version: u8,
    authorization_ticket: String,
    runner_session_id: Uuid,
    platform_account_id: String,
    external_conversation_id: String,
}

#[derive(Serialize)]
struct Grant {
    authorized: bool,
    platform_account_id: String,
    external_conversation_id: String,
    retained_message_inventory_sha256: String,
    delete_not_after: DateTime<Utc>,
}

impl ProviderCleanupCallbackService {
    pub fn new(
        repository: Arc<dyn ProviderConversationCleanupRepository>,
        cipher: Arc<SecretEnvelope>,
        bearer: &str,
    ) -> Result<Self, AppError> {
        if !(32..=512).contains(&bearer.len())
            || !bearer.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(AppError::invalid_request(
                "cleanup service credential invalid",
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

    pub fn issue_ticket(
        &self,
        scope: &TenantScope,
        claim: &ProviderCleanupClaim,
        reservation_id: Uuid,
        runner_session_id: Uuid,
    ) -> Result<String, AppError> {
        claim.original_identity.validate()?;
        let now = Utc::now();
        if scope.project_id.is_none()
            || claim.action != ProviderCleanupAction::Delete
            || !geo_domain::provider_conversation_cleanup_supported(&claim.provider)
            || claim.original_identity.provider != claim.provider
            || !valid_id(&claim.external_conversation_id)
            || claim.lease_until <= now
            || [
                claim.cleanup_id,
                claim.capture_id,
                claim.account_id,
                claim.lease_id,
                reservation_id,
                runner_session_id,
            ]
            .iter()
            .any(Uuid::is_nil)
        {
            return Err(AppError::invalid_request("cleanup binding invalid"));
        }
        let expiry = claim.lease_until.min(now + Duration::seconds(90));
        let expires_at = DateTime::from_timestamp_micros(expiry.timestamp_micros())
            .ok_or_else(|| AppError::invalid_request("cleanup expiry invalid"))?;
        let ticket = Ticket {
            version: 1,
            scope: scope.clone(),
            claim: claim.clone(),
            reservation_id,
            runner_session_id,
            expires_at,
        };
        let bytes = serde_json::to_vec(&ticket)
            .map_err(|_| AppError::conflict("cleanup ticket unavailable"))?;
        let sealed = self
            .cipher
            .seal(AAD, &bytes)
            .map_err(|_| AppError::conflict("cleanup ticket unavailable"))?;
        let encoded = hex::encode(sealed);
        if encoded.len() > MAX_TICKET_CHARS {
            return Err(AppError::invalid_request("cleanup ticket too large"));
        }
        Ok(encoded)
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
        (32..=512).contains(&bearer.len())
            && hmac::verify(
                &hmac::Key::new(hmac::HMAC_SHA256, bearer.as_bytes()),
                AUTH_MESSAGE,
                self.bearer_tag.as_ref(),
            )
            .is_ok()
    }

    async fn authorize(&self, request: AuthorizeRequest) -> Result<Json<Grant>, StatusCode> {
        if request.schema_version != 1 || request.authorization_ticket.len() > MAX_TICKET_CHARS {
            return Err(StatusCode::BAD_REQUEST);
        }
        let sealed =
            hex::decode(&request.authorization_ticket).map_err(|_| StatusCode::FORBIDDEN)?;
        let bytes = self
            .cipher
            .open(AAD, &sealed)
            .map_err(|_| StatusCode::FORBIDDEN)?;
        let ticket: Ticket = serde_json::from_slice(&bytes).map_err(|_| StatusCode::FORBIDDEN)?;
        if ticket.version != 1
            || ticket.expires_at <= Utc::now()
            || ticket.claim.action != ProviderCleanupAction::Delete
            || ticket.runner_session_id != request.runner_session_id
            || ticket.claim.original_identity.platform_account_id != request.platform_account_id
            || ticket.claim.external_conversation_id != request.external_conversation_id
        {
            return Err(StatusCode::FORBIDDEN);
        }
        // This is the authoritative, current DB check, not the earlier scanner
        // result or the runner's assertion that it retained some bytes.
        let current = self
            .repository
            .authorize_delete(
                &ticket.scope,
                ticket.claim.cleanup_id,
                ticket.claim.lease_id,
                ticket.reservation_id,
            )
            .await
            .map_err(|error| {
                StatusCode::from_u16(error.code.default_status())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            })?;
        // Leave room for the bounded provider request and session close. A
        // delayed callback cannot dispatch a delete at the account lease edge.
        if current != ticket.claim || ticket.expires_at <= Utc::now() + Duration::seconds(15) {
            return Err(StatusCode::CONFLICT);
        }
        let retained_message_inventory_sha256 = current
            .retained_message_inventory_sha256
            .ok_or(StatusCode::CONFLICT)?;
        Ok(Json(Grant {
            authorized: true,
            platform_account_id: current.original_identity.platform_account_id,
            external_conversation_id: current.external_conversation_id,
            retained_message_inventory_sha256,
            delete_not_after: ticket.expires_at.min(current.lease_until),
        }))
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

async fn service_auth(
    State(service): State<ProviderCleanupCallbackService>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if !service.authenticate(request.headers()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

async fn authorize(
    State(service): State<ProviderCleanupCallbackService>,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<Grant>, StatusCode> {
    service.authorize(request).await
}

pub(crate) fn routes(service: Option<ProviderCleanupCallbackService>) -> Router {
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
    use geo_domain::{
        ErrorCode, ObservationProviderIdentity, OperatorId, ProjectId, ProviderCleanupBackfillItem,
        ProviderCleanupDueItem, ProviderCleanupOutcome, TenantId,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tower::ServiceExt;

    const BEARER: &str = "synthetic-cleanup-callback-service-credential";

    struct Store {
        scope: TenantScope,
        claim: ProviderCleanupClaim,
        reservation_id: Uuid,
        calls: AtomicUsize,
        fail: AtomicBool,
    }
    #[async_trait::async_trait]
    impl ProviderConversationCleanupRepository for Store {
        async fn scan_unqueued(
            &self,
            _: DateTime<Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<ProviderCleanupBackfillItem>, AppError> {
            unreachable!()
        }
        async fn scan_due(
            &self,
            _: DateTime<Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<ProviderCleanupDueItem>, AppError> {
            unreachable!()
        }
        async fn enqueue(&self, _: &TenantScope, _: Uuid) -> Result<Uuid, AppError> {
            unreachable!()
        }
        async fn claim(
            &self,
            _: &TenantScope,
            _: Uuid,
        ) -> Result<Option<ProviderCleanupClaim>, AppError> {
            unreachable!()
        }
        async fn claim_due(
            &self,
            _: &TenantScope,
        ) -> Result<Option<ProviderCleanupClaim>, AppError> {
            unreachable!()
        }
        async fn finish(
            &self,
            _: &TenantScope,
            _: Uuid,
            _: Uuid,
            _: ProviderCleanupOutcome,
        ) -> Result<(), AppError> {
            unreachable!()
        }
        async fn authorize_delete(
            &self,
            scope: &TenantScope,
            id: Uuid,
            lease: Uuid,
            reservation: Uuid,
        ) -> Result<ProviderCleanupClaim, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "synthetic private detail",
                ));
            }
            if scope != &self.scope
                || id != self.claim.cleanup_id
                || lease != self.claim.lease_id
                || reservation != self.reservation_id
            {
                return Err(AppError::conflict("cleanup binding differs"));
            }
            Ok(self.claim.clone())
        }
    }

    fn fixture() -> (
        ProviderCleanupCallbackService,
        Arc<Store>,
        serde_json::Value,
    ) {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let claim = ProviderCleanupClaim {
            has_prior_delete_attempt: false,
            cleanup_id: Uuid::new_v4(),
            capture_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            provider: "kimi".into(),
            external_conversation_id: "synthetic-owned-chat".into(),
            original_identity: ObservationProviderIdentity {
                provider: "kimi".into(),
                platform_account_id: "synthetic-platform-user".into(),
            },
            retained_message_inventory_sha256: Some(geo_domain::sha256_hex(
                br#"[["synthetic-message","assistant"]]"#,
            )),
            lease_id: Uuid::new_v4(),
            lease_until: Utc::now() + Duration::minutes(2),
            action: ProviderCleanupAction::Delete,
        };
        let store = Arc::new(Store {
            scope: scope.clone(),
            claim,
            reservation_id: Uuid::new_v4(),
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let service = ProviderCleanupCallbackService::new(
            store.clone(),
            Arc::new(SecretEnvelope::ephemeral()),
            BEARER,
        )
        .unwrap();
        let session = Uuid::new_v4();
        let ticket = service
            .issue_ticket(&scope, &store.claim, store.reservation_id, session)
            .unwrap();
        let body = serde_json::json!({
            "schema_version": 1, "authorization_ticket": ticket, "runner_session_id": session,
            "platform_account_id": store.claim.original_identity.platform_account_id,
            "external_conversation_id": store.claim.external_conversation_id,
        });
        (service, store, body)
    }

    async fn submit(
        service: &ProviderCleanupCallbackService,
        body: &serde_json::Value,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder()
            .method("POST")
            .uri(CALLBACK_PATH)
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = routes(Some(service.clone()))
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
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
    async fn authentic_service_rechecks_bound_database_state() {
        let (service, store, body) = fixture();
        let (status, text) = submit(&service, &body, Some(BEARER)).await;
        assert_eq!(status, StatusCode::OK);
        let grant: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(grant["authorized"], true);
        assert_eq!(grant["platform_account_id"], body["platform_account_id"]);
        assert_eq!(
            grant["external_conversation_id"],
            body["external_conversation_id"]
        );
        assert!(
            grant["delete_not_after"]
                .as_str()
                .unwrap()
                .parse::<DateTime<Utc>>()
                .unwrap()
                > Utc::now()
        );
        assert_eq!(store.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn wrong_identity_session_ticket_or_auth_never_reaches_repository() {
        let (service, store, body) = fixture();
        assert_eq!(
            submit(&service, &body, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            submit(&service, &body, Some("synthetic-wrong-service-credential"))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        for field in [
            "platform_account_id",
            "external_conversation_id",
            "runner_session_id",
            "authorization_ticket",
        ] {
            let mut wrong = body.clone();
            wrong[field] = serde_json::json!(if field == "runner_session_id" {
                Uuid::new_v4().to_string()
            } else {
                "different".into()
            });
            assert_eq!(
                submit(&service, &wrong, Some(BEARER)).await.0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(store.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expired_ticket_and_unknown_fields_are_rejected() {
        let (service, store, body) = fixture();
        let bytes = service
            .cipher
            .open(
                AAD,
                &hex::decode(body["authorization_ticket"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
        let mut ticket: Ticket = serde_json::from_slice(&bytes).unwrap();
        ticket.expires_at = Utc::now() - Duration::seconds(1);
        let mut expired = body.clone();
        expired["authorization_ticket"] = serde_json::json!(hex::encode(
            service
                .cipher
                .seal(AAD, &serde_json::to_vec(&ticket).unwrap())
                .unwrap()
        ));
        assert_eq!(
            submit(&service, &expired, Some(BEARER)).await.0,
            StatusCode::FORBIDDEN
        );
        let mut unknown = body.clone();
        unknown["authorized"] = serde_json::json!(true);
        assert_eq!(
            submit(&service, &unknown, Some(BEARER)).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(store.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn nearly_expired_grant_cannot_start_a_provider_request() {
        let (service, store, mut body) = fixture();
        let bytes = service
            .cipher
            .open(
                AAD,
                &hex::decode(body["authorization_ticket"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
        let mut ticket: Ticket = serde_json::from_slice(&bytes).unwrap();
        ticket.expires_at = Utc::now() + Duration::seconds(10);
        body["authorization_ticket"] = serde_json::json!(hex::encode(
            service
                .cipher
                .seal(AAD, &serde_json::to_vec(&ticket).unwrap())
                .unwrap()
        ));
        assert_eq!(
            submit(&service, &body, Some(BEARER)).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(store.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn store_failure_exposes_no_private_detail_or_grant() {
        let (service, store, body) = fixture();
        store.fail.store(true, Ordering::SeqCst);
        let (status, text) = submit(&service, &body, Some(BEARER)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!text.contains("private") && !text.contains("authorized"));
    }

    #[test]
    fn unregistered_provider_cannot_receive_cleanup_ticket() {
        let (service, store, _) = fixture();
        for provider in ["deepseek", "doubao", "glm", "unknown", "Kimi"] {
            let mut claim = store.claim.clone();
            claim.provider = provider.into();
            claim.original_identity.provider = provider.into();
            assert!(
                service
                    .issue_ticket(&store.scope, &claim, store.reservation_id, Uuid::new_v4())
                    .is_err()
            );
        }
        assert_eq!(store.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn no_delete_ticket_for_reconciliation_or_expired_lease() {
        let (service, store, _) = fixture();
        let mut claim = store.claim.clone();
        claim.action = ProviderCleanupAction::Reconcile;
        assert!(
            service
                .issue_ticket(&store.scope, &claim, store.reservation_id, Uuid::new_v4())
                .is_err()
        );
        claim.action = ProviderCleanupAction::Delete;
        claim.lease_until = Utc::now() - Duration::seconds(1);
        assert!(
            service
                .issue_ticket(&store.scope, &claim, store.reservation_id, Uuid::new_v4())
                .is_err()
        );
    }
}
