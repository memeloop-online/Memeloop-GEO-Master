//! Internal authenticated browser runner client. The browser never receives
//! storageState, proxy credentials, runner token, or arbitrary navigation APIs.

use geo_domain::{
    AppError, AuthorizedMediaSnapshot, ErrorCode, MAX_MEDIA_SNAPSHOT_BYTES,
    MAX_MEDIA_SNAPSHOT_IMAGES, RichPublicationPayload, sha256_hex,
};
use reqwest::{Client, StatusCode, multipart};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone)]
pub struct BrowserBridge {
    client: Client,
    base_url: String,
    token: String,
}

impl BrowserBridge {
    const DEFAULT_REQUEST_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(geo_domain::CHANNEL_BROWSER_REQUEST_TIMEOUT_SECONDS);
    const EXECUTE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(130);
    const MEASUREMENT_REQUEST_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(geo_domain::CHANNEL_MEASUREMENT_REQUEST_TIMEOUT_SECONDS);

    fn execute_request(&self, operation: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}/v1/executions", self.base_url))
            // Measurement includes source capture and independent analysis:
            // its runner deadline is 240s; publication/lookup remain 120s.
            // Leave 10s to receive the completed or unknown receipt without
            // extending ordinary requests or publication execution.
            .timeout(if operation == "measure" {
                Self::MEASUREMENT_REQUEST_TIMEOUT
            } else {
                Self::EXECUTE_REQUEST_TIMEOUT
            })
            .bearer_auth(&self.token)
    }

    pub(crate) fn desktop_connection(&self, id: Uuid) -> Result<(String, String), AppError> {
        let mut url = reqwest::Url::parse(&self.endpoint(id, "/desktop"))
            .map_err(|_| AppError::new(ErrorCode::Internal, "browser runner URL invalid"))?;
        url.set_scheme(match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            _ => {
                return Err(AppError::new(
                    ErrorCode::Internal,
                    "browser runner URL invalid",
                ));
            }
        })
        .map_err(|_| AppError::new(ErrorCode::Internal, "browser runner URL invalid"))?;
        Ok((url.to_string(), self.token.clone()))
    }

    pub async fn desktop_status(&self, id: Uuid) -> Result<BrowserDesktopStatus, AppError> {
        let response = self
            .client
            .get(self.endpoint(id, "/status"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        Self::response(response).await
    }
    /// Discover only the version advertised by the authenticated running
    /// adapter. This is not publication verification or an enablement claim.
    pub async fn capabilities(&self) -> Result<RunnerCapabilities, AppError> {
        let response = self
            .client
            .get(format!("{}/v1/capabilities", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        Self::response(response).await
    }

    pub fn new(base_url: String, token: String) -> Result<Self, AppError> {
        let url = reqwest::Url::parse(&base_url)
            .map_err(|_| AppError::invalid_request("invalid browser runner configuration"))?;
        if token.is_empty()
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(AppError::invalid_request(
                "invalid browser runner configuration",
            ));
        }
        let base_url = base_url.trim_end_matches('/').to_string();
        let client = Client::builder()
            .timeout(Self::DEFAULT_REQUEST_TIMEOUT)
            // Bearer credentials never follow a redirect, including one to
            // another origin.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AppError::new(ErrorCode::Internal, "browser HTTP client unavailable"))?;
        Ok(Self {
            client,
            base_url,
            token,
        })
    }

    fn endpoint(&self, id: Uuid, suffix: &str) -> String {
        format!("{}/v1/sessions/{id}{suffix}", self.base_url)
    }

    async fn response<T: serde::de::DeserializeOwned>(
        mut response: reqwest::Response,
    ) -> Result<T, AppError> {
        let status = response.status();
        if !status.is_success() {
            return Err(
                if status == StatusCode::CONFLICT || status == StatusCode::UNPROCESSABLE_ENTITY {
                    AppError::conflict("browser login has not verified the account identity")
                } else if status == StatusCode::NOT_FOUND {
                    AppError::not_found("browser login session expired")
                } else {
                    AppError::new(
                        ErrorCode::DependencyUnavailable,
                        "browser runner unavailable",
                    )
                },
            );
        }
        const MAX_RUNNER_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            AppError::new(
                ErrorCode::DependencyUnavailable,
                "browser runner response invalid",
            )
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_RUNNER_RESPONSE_BYTES {
                return Err(AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner response exceeds limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| {
            AppError::new(
                ErrorCode::DependencyUnavailable,
                "browser runner response invalid",
            )
        })
    }

    pub async fn start(
        &self,
        session_id: Uuid,
        platform: &str,
        proxy: Option<BrowserProxy>,
        storage_state: Option<&serde_json::Value>,
    ) -> Result<(), AppError> {
        let started = async {
            let response = self
                .client
                .post(format!("{}/v1/sessions", self.base_url))
                .bearer_auth(&self.token)
                .json(&StartBrowserSession {
                    session_id,
                    platform,
                    proxy,
                    storage_state,
                })
                .send()
                .await
                .map_err(|_| {
                    AppError::new(
                        ErrorCode::DependencyUnavailable,
                        "browser runner unavailable",
                    )
                })?;
            let result: StartResponse = Self::response(response).await?;
            if result.session_id != session_id {
                return Err(AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser session mismatch",
                ));
            }
            Ok(())
        }
        .await;
        // The runner may have created a context even when its response timed
        // out, was malformed, or mismatched. The caller has no session handle
        // on this path; delete by the already-fixed UUID before returning.
        if started.is_err() {
            let _ = self.close(session_id).await;
        }
        started
    }

    pub async fn complete(&self, id: Uuid) -> Result<VerifiedBrowserSession, AppError> {
        let response = self
            .client
            .post(self.endpoint(id, "/complete"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        Self::response(response).await
    }

    pub async fn measurement_options(&self, id: Uuid) -> Result<MeasurementOptions, AppError> {
        let response = self
            .client
            .get(self.endpoint(id, "/measurement-options"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser model discovery unavailable; retry when the connection is restored",
                )
            })?;
        if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(AppError::capability_missing(
                "website model menu is unavailable; reconnect or retry later",
            ));
        }
        let options: MeasurementOptions = Self::response(response).await?;
        options.validate()?;
        Ok(options)
    }

    pub async fn close(&self, id: Uuid) -> Result<(), AppError> {
        let response = self
            .client
            .delete(self.endpoint(id, ""))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "browser runner unavailable",
            ))
        }
    }

    /// Trusted Rust-side execution only. No user-facing arbitrary browser
    /// action, navigation or payload-to-success path is exposed.
    pub async fn execute(
        &self,
        execution_id: Uuid,
        session_id: Uuid,
        operation: &str,
        payload: &serde_json::Value,
    ) -> Result<BrowserExecution, AppError> {
        self.execute_with_source_capture_ticket(execution_id, session_id, operation, payload, None)
            .await
    }

    /// The ticket is sealed by the API after an authoritative measurement
    /// claim. It is not part of the model/user payload and is omitted for
    /// older runners or installations without capture callback configuration.
    pub async fn execute_with_source_capture_ticket(
        &self,
        execution_id: Uuid,
        session_id: Uuid,
        operation: &str,
        payload: &serde_json::Value,
        source_capture_ticket: Option<&str>,
    ) -> Result<BrowserExecution, AppError> {
        if !matches!(operation, "publish" | "measure" | "lookup")
            || !payload.is_object()
            || source_capture_ticket.is_some_and(|ticket| {
                operation != "measure" || ticket.is_empty() || ticket.len() > 4096
            })
        {
            return Err(AppError::invalid_request("unsupported browser operation"));
        }
        let response = self
            .execute_request(operation)
            .json(&BrowserExecutionRequest {
                execution_id,
                session_id,
                operation,
                payload,
                source_capture_ticket,
            })
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        Self::response(response).await
    }

    /// Exact-account cleanup is separate from generic browser execution.
    /// A lost response is ambiguous; callers must reconcile, never retry delete.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn cleanup_conversation(
        &self,
        session_id: Uuid,
        execution_id: Uuid,
        expected_identity: &CleanupExpectedIdentity,
        chat_id: &str,
        action: CleanupAction,
        ticket: Option<&str>,
    ) -> Result<CleanupResult, AppError> {
        let valid_id = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        };
        if expected_identity.provider != "kimi"
            || !valid_id(&expected_identity.platform_account_id)
            || !valid_id(chat_id)
            || ticket.is_some_and(|value| {
                value.is_empty()
                    || value.len() > 4096
                    || !value.len().is_multiple_of(2)
                    || !value
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        {
            return Err(AppError::invalid_request(
                "invalid conversation cleanup scope",
            ));
        }
        let response = self
            .client
            .post(self.endpoint(session_id, "/cleanup-conversation"))
            .timeout(std::time::Duration::from_secs(65))
            .bearer_auth(&self.token)
            .json(&CleanupRequest {
                execution_id,
                expected_identity,
                external_conversation_id: chat_id,
                action,
                authorization_ticket: ticket,
            })
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        let result: CleanupResult = Self::response(response).await?;
        if result.execution_id != execution_id || result.external_conversation_id != chat_id {
            return Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "cleanup response identity differs",
            ));
        }
        Ok(result)
    }

    /// A separate bounded binary protocol: neither the general JSON execute
    /// endpoint nor a model-supplied payload may carry media bytes or a ticket.
    pub async fn execute_rich(
        &self,
        execution_id: Uuid,
        session_id: Uuid,
        callback_ticket: &str,
        variant: RichExecutionVariant<'_>,
        payload: &RichPublicationPayload,
        snapshots: Vec<AuthorizedMediaSnapshot>,
    ) -> Result<BrowserExecution, AppError> {
        if callback_ticket.is_empty() || callback_ticket.len() > 4096 {
            return Err(AppError::invalid_request("rich publication ticket invalid"));
        }
        let mut expected = std::collections::BTreeMap::new();
        for item in &payload.media {
            let identity = (item.object.object_id, item.object.object_version);
            let image = (
                &item.object.sha256,
                &item.media_type,
                item.byte_len,
                item.width,
                item.height,
            );
            if expected
                .insert(identity, image)
                .is_some_and(|old| old != image)
            {
                return Err(AppError::conflict(
                    "rich publication media identity differs",
                ));
            }
        }
        if expected.len() != snapshots.len() || expected.len() > MAX_MEDIA_SNAPSHOT_IMAGES {
            return Err(AppError::conflict(
                "rich publication media snapshot differs",
            ));
        }
        let metadata = serde_json::to_vec(&RichExecutionMetadata {
            schema_version: 1,
            execution_id,
            attempt_id: execution_id,
            callback_ticket,
            variant,
            payload,
        })
        .map_err(|_| AppError::conflict("rich publication metadata unavailable"))?;
        // The frozen markdown alone may be 8 MiB. This limit applies only to
        // the typed rich route; ordinary JSON operations remain unchanged.
        if metadata.len() > 16 * 1024 * 1024 {
            return Err(AppError::conflict(
                "rich publication metadata exceeds limit",
            ));
        }
        let mut form = multipart::Form::new().part(
            "metadata",
            multipart::Part::bytes(metadata)
                .mime_str("application/json")
                .map_err(|_| AppError::conflict("rich publication metadata invalid"))?,
        );
        let mut seen = std::collections::BTreeSet::new();
        let mut total = 0u64;
        for snapshot in snapshots {
            let key = &snapshot.image.key;
            let identity = (key.object_id, key.object_version);
            if !seen.insert(identity) {
                return Err(AppError::conflict("rich publication media duplicated"));
            }
            let Some((digest, media_type, length, width, height)) = expected.get(&identity) else {
                return Err(AppError::conflict(
                    "rich publication media snapshot differs",
                ));
            };
            if key.sha256 != **digest
                || snapshot.image.media_type != **media_type
                || snapshot.image.byte_len != *length
                || snapshot.image.width != *width
                || snapshot.image.height != *height
                || snapshot.bytes.len() as u64 != *length
                || sha256_hex(&snapshot.bytes) != key.sha256
            {
                return Err(AppError::conflict(
                    "rich publication media snapshot differs",
                ));
            }
            total = total
                .checked_add(*length)
                .filter(|total| *total <= MAX_MEDIA_SNAPSHOT_BYTES)
                .ok_or_else(|| AppError::conflict("rich publication media exceeds limit"))?;
            form = form.part(
                format!("media_{}_{}", key.object_id, key.object_version),
                multipart::Part::bytes(snapshot.bytes)
                    .mime_str(&snapshot.image.media_type)
                    .map_err(|_| AppError::conflict("rich publication media type invalid"))?,
            );
        }
        let response = self
            .client
            .post(self.endpoint(session_id, "/execute-rich"))
            .timeout(Self::EXECUTE_REQUEST_TIMEOUT)
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        Self::response(response).await
    }
}

#[derive(Serialize)]
pub(crate) struct CleanupExpectedIdentity {
    pub provider: String,
    pub platform_account_id: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CleanupAction {
    Delete,
    Reconcile,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CleanupStatus {
    Deleted,
    Present,
    Unknown,
    Retained,
    NeedsLogin,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CleanupResult {
    pub execution_id: Uuid,
    pub external_conversation_id: String,
    pub status: CleanupStatus,
    #[serde(default, deserialize_with = "cleanup_diagnostic")]
    pub diagnostic: Option<geo_domain::ProviderCleanupDiagnostic>,
}

fn cleanup_diagnostic<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<geo_domain::ProviderCleanupDiagnostic>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    // Optional diagnostics must not turn a receipt into an error, nor admit
    // arbitrary strings/extra fields into persistent operational history.
    Ok(serde_json::from_value(value).ok())
}

#[derive(Serialize)]
struct CleanupRequest<'a> {
    execution_id: Uuid,
    expected_identity: &'a CleanupExpectedIdentity,
    external_conversation_id: &'a str,
    action: CleanupAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    authorization_ticket: Option<&'a str>,
}

#[derive(Serialize)]
struct BrowserExecutionRequest<'a> {
    execution_id: Uuid,
    session_id: Uuid,
    operation: &'a str,
    payload: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_capture_ticket: Option<&'a str>,
}

#[derive(Serialize)]
pub struct RichExecutionVariant<'a> {
    pub title: &'a str,
    pub markdown: &'a str,
    pub payload_hash: &'a str,
}

#[derive(Serialize)]
struct RichExecutionMetadata<'a> {
    schema_version: u8,
    execution_id: Uuid,
    attempt_id: Uuid,
    callback_ticket: &'a str,
    variant: RichExecutionVariant<'a>,
    payload: &'a RichPublicationPayload,
}

#[derive(Debug, Deserialize)]
pub struct RunnerCapabilities {
    pub connectors: Vec<RunnerConnector>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MeasurementModel {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MeasurementOptions {
    pub models: Vec<MeasurementModel>,
    pub selected_model: Option<String>,
}

impl MeasurementOptions {
    fn validate(&self) -> Result<(), AppError> {
        let mut ids = std::collections::HashSet::new();
        if self.models.is_empty()
            || self.models.len() > 64
            || self.models.iter().any(|model| {
                model.id.is_empty()
                    || model.id.len() > 128
                    || !model
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
                    || model.label.trim().is_empty()
                    || model.label.chars().count() > 200
                    || model.label.chars().any(char::is_control)
                    || !ids.insert(&model.id)
            })
            || self
                .selected_model
                .as_ref()
                .is_some_and(|id| !ids.contains(id))
        {
            return Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "website model menu response invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerConnector {
    pub platform: String,
    pub placement_slot: String,
    pub connector_version: String,
    pub operations: Vec<String>,
    // Explicitly never use a self-reported verification claim to authorize
    // publishing; independent publication/readback history is the authority.
    #[allow(dead_code)]
    pub verified: bool,
}

/// Private runner receipt. This is not a client-submittable success claim.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserReceiptProvenance {
    Live,
    Fixture,
}

#[derive(Deserialize)]
pub struct BrowserExecution {
    pub execution_id: Uuid,
    // Missing provenance remains untrusted. Unknown values fail receipt
    // deserialization, leaving ambiguous publishes in the unknown state.
    #[serde(default)]
    pub provenance: Option<BrowserReceiptProvenance>,
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub evidence: Vec<serde_json::Value>,
    #[serde(default)]
    pub public_url: Option<String>,
    #[serde(default)]
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub connector_version: Option<String>,
    #[serde(default)]
    pub stage: Option<String>,
}

#[derive(Serialize)]
pub struct BrowserProxy {
    pub server: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[derive(Serialize)]
struct StartBrowserSession<'a> {
    session_id: Uuid,
    platform: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: Option<BrowserProxy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_state: Option<&'a serde_json::Value>,
}

#[derive(Deserialize)]
struct StartResponse {
    session_id: Uuid,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct BrowserIdentity {
    pub platform_account_id: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct BrowserDesktopStatus {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<BrowserIdentity>,
}

/// This type is never Serialize/Debug: raw storageState is server-only.
#[derive(Deserialize)]
pub struct VerifiedBrowserSession {
    pub identity: BrowserIdentity,
    pub storage_state: serde_json::Value,
}

#[cfg(test)]
mod measurement_options_tests {
    use super::*;

    #[test]
    fn cleanup_diagnostics_are_optional_closed_and_non_authoritative() {
        let base = serde_json::json!({
            "execution_id": Uuid::new_v4(),
            "external_conversation_id": "fixture-chat",
            "status": "retained"
        });
        let old: CleanupResult = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(old.diagnostic, None);
        for diagnostic in [
            serde_json::Value::Null,
            serde_json::json!({"stage":"delete","code":"unexpected secret text"}),
            serde_json::json!({"stage":"unexpected endpoint","code":"http_error"}),
            serde_json::json!({"stage":"delete","code":"http_error","raw":"secret"}),
            serde_json::json!({"stage":"delete","code":null}),
            serde_json::json!("arbitrary error"),
        ] {
            let mut value = base.clone();
            value["diagnostic"] = diagnostic;
            let result: CleanupResult = serde_json::from_value(value).unwrap();
            assert_eq!(result.status, CleanupStatus::Retained);
            assert_eq!(result.diagnostic, None);
        }
        let mut valid = base;
        valid["diagnostic"] = serde_json::json!({"stage":"delete","code":"http_error"});
        let result: CleanupResult = serde_json::from_value(valid).unwrap();
        assert_eq!(result.status, CleanupStatus::Retained);
        assert_eq!(
            serde_json::to_value(result.diagnostic).unwrap(),
            serde_json::json!({"stage":"delete","code":"http_error"})
        );
    }

    #[tokio::test]
    async fn cleanup_wire_is_scoped_and_rejects_mismatched_receipts() {
        use axum::{Router, body::to_bytes, extract::Request, routing::post};
        for mismatch in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let session_id = Uuid::new_v4();
            let execution_id = Uuid::new_v4();
            let app = Router::new().route(
                &format!("/v1/sessions/{session_id}/cleanup-conversation"),
                post(move |request: Request| async move {
                    assert_eq!(request.headers()["authorization"], "Bearer fixture-token");
                    let bytes = to_bytes(request.into_body(), 8192).await.unwrap();
                    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(body, serde_json::json!({
                        "execution_id": execution_id,
                        "expected_identity": { "provider": "kimi", "platform_account_id": "fixture-account" },
                        "external_conversation_id": "fixture-chat",
                        "action": "delete", "authorization_ticket": "aabb"
                    }));
                    axum::Json(serde_json::json!({
                        "execution_id": if mismatch { Uuid::new_v4() } else { execution_id },
                        "external_conversation_id": "fixture-chat", "status": "deleted"
                    }))
                }),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let bridge =
                BrowserBridge::new(format!("http://{addr}"), "fixture-token".into()).unwrap();
            let result = bridge
                .cleanup_conversation(
                    session_id,
                    execution_id,
                    &CleanupExpectedIdentity {
                        provider: "kimi".into(),
                        platform_account_id: "fixture-account".into(),
                    },
                    "fixture-chat",
                    CleanupAction::Delete,
                    Some("aabb"),
                )
                .await;
            if mismatch {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap().status, CleanupStatus::Deleted);
            }
            server.abort();
        }
    }

    #[test]
    fn source_capture_ticket_is_absent_from_legacy_execute_request() {
        let payload = serde_json::json!({"question":"synthetic"});
        let request = BrowserExecutionRequest {
            execution_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            operation: "measure",
            payload: &payload,
            source_capture_ticket: None,
        };
        let legacy = serde_json::to_value(&request).unwrap();
        assert!(legacy.get("source_capture_ticket").is_none());
        let bound = serde_json::to_value(BrowserExecutionRequest {
            source_capture_ticket: Some("sealed-ticket"),
            ..request
        })
        .unwrap();
        assert_eq!(bound["source_capture_ticket"], "sealed-ticket");
        assert_eq!(bound["payload"], payload);
    }

    #[tokio::test]
    async fn rich_wire_uses_separate_authenticated_multipart_endpoint() {
        use axum::{Router, body::to_bytes, extract::Request, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let execution_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let route = format!("/v1/sessions/{session_id}/execute-rich");
        let app = Router::new().route(
            &route,
            post(move |request: Request| async move {
                assert_eq!(request.headers()["authorization"], "Bearer service-token");
                assert!(
                    request.headers()["content-type"]
                        .to_str()
                        .unwrap()
                        .starts_with("multipart/form-data; boundary=")
                );
                let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                let wire = String::from_utf8(body.to_vec()).unwrap();
                assert!(wire.contains("name=\"metadata\""));
                assert!(wire.contains("\"schema_version\":1"));
                assert!(wire.contains(&format!("\"execution_id\":\"{execution_id}\"")));
                assert!(wire.contains(&format!("\"attempt_id\":\"{execution_id}\"")));
                assert!(wire.contains("\"callback_ticket\":\"sealed-ticket\""));
                assert!(wire.contains("\"payload_hash\":\"frozen-hash\""));
                axum::Json(serde_json::json!({
                    "execution_id": execution_id,
                    "status": "unsupported",
                    "provenance": "fixture"
                }))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let bridge = BrowserBridge::new(format!("http://{addr}"), "service-token".into()).unwrap();
        let result = bridge
            .execute_rich(
                execution_id,
                session_id,
                "sealed-ticket",
                RichExecutionVariant {
                    title: "Title",
                    markdown: "Body",
                    payload_hash: "frozen-hash",
                },
                &RichPublicationPayload {
                    schema_version: 2,
                    format: "rich_markdown.v2".into(),
                    content_revision_id: Uuid::new_v4(),
                    policy_version: "test".into(),
                    document: geo_domain::StructuredDocument {
                        title: "Title".into(),
                        blocks: vec![],
                        schema_version: Some(2),
                    },
                    media: vec![],
                },
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(result.execution_id, execution_id);
        assert_eq!(result.status, "unsupported");
        server.abort();
    }

    #[tokio::test]
    async fn rich_transport_rejects_manifest_snapshot_mismatch_before_network() {
        let bridge =
            BrowserBridge::new("http://127.0.0.1:1".into(), "fixture-token".into()).unwrap();
        let payload = RichPublicationPayload {
            schema_version: 2,
            format: "rich_markdown.v2".into(),
            content_revision_id: Uuid::new_v4(),
            policy_version: "test".into(),
            document: geo_domain::StructuredDocument {
                title: "Title".into(),
                blocks: vec![],
                schema_version: Some(2),
            },
            media: vec![],
        };
        let result = bridge
            .execute_rich(
                Uuid::new_v4(),
                Uuid::new_v4(),
                "sealed-ticket",
                RichExecutionVariant {
                    title: "Title",
                    markdown: "Body",
                    payload_hash: "frozen-hash",
                },
                &payload,
                vec![AuthorizedMediaSnapshot {
                    image: geo_domain::VerifiedImage {
                        key: geo_domain::MediaObjectKey {
                            object_id: Uuid::new_v4(),
                            object_version: 1,
                            sha256: sha256_hex(b"image"),
                        },
                        media_type: "image/png".into(),
                        byte_len: 5,
                        width: 1,
                        height: 1,
                    },
                    bytes: b"image".to_vec(),
                }],
            )
            .await;
        assert_eq!(result.err().unwrap().code, ErrorCode::Conflict);
    }

    #[test]
    fn execute_timeout_outlives_runner_without_extending_other_requests() {
        let bridge =
            BrowserBridge::new("http://127.0.0.1:1234".into(), "fixture-token".into()).unwrap();
        assert_eq!(
            BrowserBridge::DEFAULT_REQUEST_TIMEOUT,
            std::time::Duration::from_secs(60)
        );
        assert!(BrowserBridge::EXECUTE_REQUEST_TIMEOUT > std::time::Duration::from_secs(120));
        for operation in ["publish", "lookup"] {
            assert_eq!(
                bridge.execute_request(operation).build().unwrap().timeout(),
                Some(&std::time::Duration::from_secs(130))
            );
        }
        assert_eq!(
            bridge.execute_request("measure").build().unwrap().timeout(),
            Some(&std::time::Duration::from_secs(250))
        );
        assert!(BrowserBridge::MEASUREMENT_REQUEST_TIMEOUT > std::time::Duration::from_secs(240));
        assert_eq!(
            geo_domain::CHANNEL_MEASUREMENT_LEASE.to_std().unwrap(),
            BrowserBridge::DEFAULT_REQUEST_TIMEOUT * 3
                + BrowserBridge::MEASUREMENT_REQUEST_TIMEOUT
                + std::time::Duration::from_secs(30),
            "lease covers identity, execution, renewal, close and terminal margin"
        );
        assert_eq!(geo_domain::CHANNEL_EXECUTION_LEASE.num_seconds(), 300);
        assert_eq!(
            bridge
                .client
                .get(format!("{}/v1/capabilities", bridge.base_url))
                .build()
                .unwrap()
                .timeout(),
            None,
            "ordinary requests inherit the client's 60-second timeout"
        );
    }

    #[test]
    fn model_menu_rejects_unobserved_selection_and_duplicate_or_unsafe_ids() {
        let mut options = MeasurementOptions {
            models: vec![MeasurementModel {
                id: "observed-model".into(),
                label: "Observed model".into(),
            }],
            selected_model: None,
        };
        assert!(options.validate().is_ok());
        options.selected_model = Some("invented-default".into());
        assert!(options.validate().is_err());
        options.selected_model = Some("observed-model".into());
        assert!(options.validate().is_ok());
        options.models.push(MeasurementModel {
            id: "observed-model".into(),
            label: "Duplicate".into(),
        });
        assert!(options.validate().is_err());
        options.models.pop();
        options.models[0].id = "../invalid".into();
        assert!(options.validate().is_err());
        options.models.clear();
        assert!(options.validate().is_err());
    }
}
