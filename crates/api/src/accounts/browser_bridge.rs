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

    /// Check transport availability without waiting for provider identity.
    pub async fn desktop_ready(&self, id: Uuid) -> Result<(), AppError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Readiness {
            ready: bool,
        }
        let response = self
            .client
            .get(self.endpoint(id, "/desktop-readiness"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| {
                AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "browser runner unavailable",
                )
            })?;
        let readiness: Readiness = Self::response(response).await?;
        if !readiness.ready {
            return Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "browser desktop unavailable",
            ));
        }
        Ok(())
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

    /// Discover a generation before persisting a new session's ownership.
    /// This cannot establish the generation that owns an existing session.
    pub async fn lifecycle_generation(&self) -> Result<Uuid, AppError> {
        let response = self
            .client
            .get(format!("{}/v1/lifecycle", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| Self::lifecycle_unavailable())?;
        let receipt: RunnerLifecycle = Self::response(response).await?;
        Ok(receipt.generation)
    }

    /// The caller must persist the chosen generation before starting a session.
    /// A missing generation echo (including from an older runner) is ambiguous.
    pub async fn start_in_generation(
        &self,
        session_id: Uuid,
        expected_generation: Uuid,
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
                    expected_generation: Some(expected_generation),
                })
                .send()
                .await
                .map_err(|_| Self::lifecycle_unavailable())?;
            let receipt: GenerationStartResponse = Self::response(response).await?;
            if receipt.session_id != session_id || receipt.generation != expected_generation {
                return Err(Self::lifecycle_unavailable());
            }
            Ok(())
        }
        .await;
        if started.is_err() {
            // Never issue legacy DELETE after a pinned start: the endpoint
            // could now route to a different instance. Failure stays ambiguous.
            let _ = self
                .dispose_in_generation(session_id, expected_generation)
                .await;
        }
        started
    }

    /// Confirm local browser resource disposal for a durably recorded owner.
    /// An ordinary 404, current generation discovery, or legacy close is not
    /// proof. This receipt does not prove deletion of provider conversations.
    pub async fn dispose_in_generation(
        &self,
        session_id: Uuid,
        expected_generation: Uuid,
    ) -> Result<BrowserDisposalReceipt, AppError> {
        let response = self
            .client
            .post(self.endpoint(session_id, "/dispose"))
            .bearer_auth(&self.token)
            .json(&DisposeBrowserSession {
                expected_generation,
            })
            .send()
            .await
            .map_err(|_| Self::lifecycle_unavailable())?;
        let receipt: DisposalResponse = Self::response(response).await?;
        if receipt.session_id != session_id || receipt.generation != expected_generation {
            return Err(Self::lifecycle_unavailable());
        }
        let DisposalState::Disposed = receipt.state;
        Ok(BrowserDisposalReceipt {
            session_id: receipt.session_id,
            generation: receipt.generation,
        })
    }

    fn lifecycle_unavailable() -> AppError {
        AppError::new(
            ErrorCode::DependencyUnavailable,
            "browser lifecycle confirmation unavailable",
        )
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
                    expected_generation: None,
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
        if !geo_domain::provider_conversation_cleanup_supported(&expected_identity.provider)
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

    /// Read-only recovery of an already identified conversation. The caller
    /// must validate and persist the returned evidence against the immutable
    /// original source using ObservationCompletionRecoveryInput::validate.
    /// This transport receipt alone is neither completion nor deletion proof.
    pub async fn recover_conversation_completion(
        &self,
        session_id: Uuid,
        request: &CompletionRecoveryRequest,
    ) -> Result<CompletionRecoveryResult, AppError> {
        request.validate(session_id)?;
        let response = self
            .client
            .post(self.endpoint(session_id, "/recover-conversation-completion"))
            .timeout(std::time::Duration::from_secs(65))
            .bearer_auth(&self.token)
            .json(request)
            .send()
            .await
            .map_err(|_| Self::recovery_unavailable())?;
        let result: CompletionRecoveryResult = Self::response(response).await?;
        result.validate(request)?;
        Ok(result)
    }

    fn recovery_unavailable() -> AppError {
        AppError::new(
            ErrorCode::DependencyUnavailable,
            "conversation recovery response unavailable",
        )
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

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompletionRecoveryIdentity {
    pub provider: String,
    pub platform_account_id: String,
}

/// Trusted execution scope only; no prompt, navigation, action or ticket.
#[derive(Clone, Serialize)]
pub struct CompletionRecoveryRequest {
    pub execution_id: Uuid,
    pub expected_identity: CompletionRecoveryIdentity,
    pub external_conversation_id: String,
    pub request_message_id: String,
    pub response_message_id: String,
}

impl CompletionRecoveryRequest {
    fn validate(&self, session_id: Uuid) -> Result<(), AppError> {
        let identifier = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        };
        let message_id = |value: &str| {
            !value.is_empty()
                && value.len() <= 16
                && value.bytes().all(|c| c.is_ascii_digit())
                && (value == "0" || !value.starts_with('0'))
                && value
                    .parse::<u64>()
                    .is_ok_and(|id| id <= 9_007_199_254_740_991)
        };
        if session_id.is_nil()
            || self.execution_id.is_nil()
            || self.expected_identity.provider != "deepseek"
            || !identifier(&self.expected_identity.platform_account_id)
            || !identifier(&self.external_conversation_id)
            || !message_id(&self.request_message_id)
            || !message_id(&self.response_message_id)
            || self.request_message_id == self.response_message_id
        {
            return Err(AppError::invalid_request(
                "invalid conversation recovery scope",
            ));
        }
        Ok(())
    }
}

/// Closed transport evidence, intentionally not a domain completion proof.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompletionRecoveryResult {
    Recovered {
        execution_id: Uuid,
        external_conversation_id: String,
        recovery_identity: CompletionRecoveryIdentity,
        observed_at: chrono::DateTime<chrono::Utc>,
        history_json: String,
        history_sha256: String,
    },
    Retained {
        execution_id: Uuid,
        external_conversation_id: String,
        diagnostic: Option<CompletionRecoveryDiagnostic>,
    },
    NeedsLogin {
        execution_id: Uuid,
        external_conversation_id: String,
        diagnostic: Option<CompletionRecoveryDiagnostic>,
    },
    Unknown {
        execution_id: Uuid,
        external_conversation_id: String,
        diagnostic: Option<CompletionRecoveryDiagnostic>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionRecoveryDiagnostic {
    pub stage: CompletionRecoveryStage,
    pub code: CompletionRecoveryCode,
    pub reason: Option<CompletionRecoveryReason>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionRecoveryReason {
    DocumentUnavailable,
    ReadDeadlineExceeded,
    TransportUnavailable,
    ResponseEnvelopeUnverified,
    HistoryEnvelopeUnverified,
    HistoryBindingMismatch,
    InventoryUnverified,
    CompletionUnverified,
}

impl CompletionRecoveryReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentUnavailable => "document_unavailable",
            Self::ReadDeadlineExceeded => "read_deadline_exceeded",
            Self::TransportUnavailable => "transport_unavailable",
            Self::ResponseEnvelopeUnverified => "response_envelope_unverified",
            Self::HistoryEnvelopeUnverified => "history_envelope_unverified",
            Self::HistoryBindingMismatch => "history_binding_mismatch",
            Self::InventoryUnverified => "inventory_unverified",
            Self::CompletionUnverified => "completion_unverified",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionRecoveryStage {
    Scope,
    Identity,
    History,
    Deadline,
    Runner,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionRecoveryCode {
    UnsupportedPlatform,
    ReauthRequired,
    AccountMismatch,
    CompletionUnverified,
    DeadlineExceeded,
    TransportUnknown,
}

impl CompletionRecoveryResult {
    fn validate(&self, request: &CompletionRecoveryRequest) -> Result<(), AppError> {
        let (execution_id, chat_id) = match self {
            Self::Recovered {
                execution_id,
                external_conversation_id,
                recovery_identity,
                history_json,
                history_sha256,
                ..
            } => {
                if recovery_identity != &request.expected_identity
                    || history_json.is_empty()
                    || history_json.len() > geo_domain::MAX_OBSERVATION_SOURCE_BYTES
                    || sha256_hex(history_json.as_bytes()) != *history_sha256
                    || serde_json::from_str::<serde_json::Value>(history_json).is_err()
                {
                    return Err(BrowserBridge::recovery_unavailable());
                }
                (execution_id, external_conversation_id)
            }
            Self::Retained {
                execution_id,
                external_conversation_id,
                diagnostic,
            }
            | Self::NeedsLogin {
                execution_id,
                external_conversation_id,
                diagnostic,
            }
            | Self::Unknown {
                execution_id,
                external_conversation_id,
                diagnostic,
            } => {
                if diagnostic.as_ref().is_some_and(|diagnostic| {
                    !matches!(
                        (&diagnostic.stage, &diagnostic.code),
                        (
                            CompletionRecoveryStage::Scope,
                            CompletionRecoveryCode::UnsupportedPlatform
                        ) | (
                            CompletionRecoveryStage::Identity,
                            CompletionRecoveryCode::ReauthRequired
                                | CompletionRecoveryCode::AccountMismatch
                        ) | (
                            CompletionRecoveryStage::History,
                            CompletionRecoveryCode::CompletionUnverified
                        ) | (
                            CompletionRecoveryStage::Deadline,
                            CompletionRecoveryCode::DeadlineExceeded
                        ) | (
                            CompletionRecoveryStage::Runner,
                            CompletionRecoveryCode::TransportUnknown
                        )
                    ) || (diagnostic.reason.is_some()
                        && (!matches!(self, Self::Retained { .. })
                            || !matches!(
                                (&diagnostic.stage, &diagnostic.code),
                                (
                                    CompletionRecoveryStage::History,
                                    CompletionRecoveryCode::CompletionUnverified
                                )
                            )))
                }) {
                    return Err(BrowserBridge::recovery_unavailable());
                }
                (execution_id, external_conversation_id)
            }
        };
        if *execution_id != request.execution_id || *chat_id != request.external_conversation_id {
            return Err(BrowserBridge::recovery_unavailable());
        }
        Ok(())
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
    /// Strict identity-bound protocol absence, not an arbitrary HTTP 404.
    Absent,
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
    #[serde(default)]
    pub login_entry_available: Option<bool>,
    #[serde(default)]
    pub login_supported: Option<bool>,
    #[serde(default)]
    pub model_discovery_supported: bool,
    // Explicitly never use a self-reported verification claim to authorize
    // publishing; independent publication/readback history is the authority.
    #[allow(dead_code)]
    pub verified: bool,
}

/// Ambiguous or unversioned adapter advertisements never confer capability.
pub(crate) fn measurement_connector_available(
    connectors: &[RunnerConnector],
    provider: &str,
) -> bool {
    let matching: Vec<_> = connectors
        .iter()
        .filter(|connector| connector.platform == provider && connector.placement_slot == "primary")
        .collect();
    matching.len() == 1
        && !matching[0].connector_version.trim().is_empty()
        && matching[0]
            .operations
            .iter()
            .any(|operation| operation == "measure")
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

#[derive(Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_generation: Option<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunnerLifecycle {
    generation: Uuid,
}

#[derive(Deserialize)]
struct GenerationStartResponse {
    session_id: Uuid,
    generation: Uuid,
}

#[derive(Serialize)]
struct DisposeBrowserSession {
    expected_generation: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DisposalState {
    Disposed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisposalResponse {
    session_id: Uuid,
    generation: Uuid,
    state: DisposalState,
}

/// Constructed only after authenticated, exact-generation disposal confirmation.
/// Deliberately not deserializable from caller-provided data.
pub struct BrowserDisposalReceipt {
    session_id: Uuid,
    generation: Uuid,
}

impl BrowserDisposalReceipt {
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn generation(&self) -> Uuid {
        self.generation
    }
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
mod completion_recovery_tests {
    use super::*;
    use axum::{Router, body::to_bytes, extract::Request, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn request() -> CompletionRecoveryRequest {
        CompletionRecoveryRequest {
            execution_id: Uuid::new_v4(),
            expected_identity: CompletionRecoveryIdentity {
                provider: "deepseek".into(),
                platform_account_id: "fixture-account".into(),
            },
            external_conversation_id: "fixture-chat".into(),
            request_message_id: "0".into(),
            response_message_id: "1".into(),
        }
    }

    fn receipt(request: &CompletionRecoveryRequest) -> serde_json::Value {
        // Transport validates integrity, not provider completion semantics.
        let history = r#"{"synthetic":"history"}"#;
        serde_json::json!({
            "status": "recovered",
            "execution_id": request.execution_id,
            "external_conversation_id": request.external_conversation_id,
            "recovery_identity": request.expected_identity,
            "observed_at": "2026-01-01T00:00:00Z",
            "history_json": history,
            "history_sha256": sha256_hex(history.as_bytes())
        })
    }

    #[tokio::test]
    async fn invalid_recovery_scope_never_contacts_runner() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().fallback(move || {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let bridge =
            BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap();
        let mut invalid = vec![];
        for provider in ["kimi", "DeepSeek", "unknown", ""] {
            let mut value = request();
            value.expected_identity.provider = provider.into();
            invalid.push(value);
        }
        for message in [
            "",
            "-1",
            "01",
            "1.0",
            "9007199254740992",
            "10000000000000000",
            "秘密",
        ] {
            let mut value = request();
            value.request_message_id = message.into();
            invalid.push(value);
            let mut value = request();
            value.response_message_id = message.into();
            invalid.push(value);
        }
        for identifier in ["", "../chat", "has space", &"a".repeat(129)] {
            let mut value = request();
            value.external_conversation_id = identifier.into();
            invalid.push(value);
            let mut value = request();
            value.expected_identity.platform_account_id = identifier.into();
            invalid.push(value);
        }
        let mut value = request();
        value.execution_id = Uuid::nil();
        invalid.push(value);
        let mut value = request();
        value.response_message_id = value.request_message_id.clone();
        invalid.push(value);
        for value in invalid {
            let error = bridge
                .recover_conversation_completion(Uuid::new_v4(), &value)
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, ErrorCode::InvalidRequest);
        }
        let error = bridge
            .recover_conversation_completion(Uuid::nil(), &request())
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn recovery_wire_is_closed_read_only_and_receipts_are_bound() {
        for mutation in [
            "none",
            "execution",
            "chat",
            "account",
            "provider",
            "digest",
            "uppercase_digest",
            "oversized_history",
            "oversized_response",
            "invalid_json",
            "timestamp",
            "unknown_field",
            "identity_extra",
            "missing_identity",
            "wrong_type",
            "malformed",
        ] {
            let request = request();
            let session_id = Uuid::new_v4();
            let mut response = receipt(&request);
            match mutation {
                "execution" => response["execution_id"] = serde_json::json!(Uuid::new_v4()),
                "chat" => response["external_conversation_id"] = "other-chat".into(),
                "account" => {
                    response["recovery_identity"]["platform_account_id"] = "other-account".into()
                }
                "provider" => response["recovery_identity"]["provider"] = "kimi".into(),
                "digest" => response["history_sha256"] = "0".repeat(64).into(),
                "uppercase_digest" => {
                    response["history_sha256"] = response["history_sha256"]
                        .as_str()
                        .unwrap()
                        .to_uppercase()
                        .into()
                }
                "oversized_history" | "oversized_response" | "invalid_json" => {
                    let history = if mutation == "invalid_json" {
                        "{".into()
                    } else {
                        serde_json::to_string(&"a".repeat(if mutation == "oversized_response" {
                            8 * 1024 * 1024
                        } else {
                            geo_domain::MAX_OBSERVATION_SOURCE_BYTES
                        }))
                        .unwrap()
                    };
                    response["history_sha256"] = sha256_hex(history.as_bytes()).into();
                    response["history_json"] = history.into();
                }
                "timestamp" => response["observed_at"] = "not-a-time".into(),
                "unknown_field" => response["payload"] = serde_json::json!({"action":"delete"}),
                "identity_extra" => response["recovery_identity"]["raw"] = "private".into(),
                "missing_identity" => {
                    response
                        .as_object_mut()
                        .unwrap()
                        .remove("recovery_identity");
                }
                "wrong_type" => response["history_json"] = serde_json::json!({}),
                _ => {}
            }
            let expected = serde_json::to_value(&request).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route(
                &format!("/v1/sessions/{session_id}/recover-conversation-completion"),
                post(move |wire: Request| {
                    let expected = expected.clone();
                    let response = response.clone();
                    let observed = observed.clone();
                    async move {
                        observed.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(wire.headers()["authorization"], "Bearer fixture-token");
                        let body = to_bytes(wire.into_body(), 8192).await.unwrap();
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body, expected);
                        assert_eq!(body.as_object().unwrap().len(), 5);
                        assert!(body.get("payload").is_none());
                        assert!(body.get("action").is_none());
                        if mutation == "malformed" {
                            "{".into()
                        } else {
                            response.to_string()
                        }
                    }
                }),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let bridge =
                BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap();
            let result = bridge
                .recover_conversation_completion(session_id, &request)
                .await;
            assert_eq!(result.is_ok(), mutation == "none", "{mutation}");
            assert_eq!(calls.load(Ordering::SeqCst), 1, "no fallback or retry");
            server.abort();
        }
    }

    #[test]
    fn failure_union_and_diagnostics_are_closed() {
        let request = request();
        for status in ["retained", "needs_login", "unknown"] {
            let base = serde_json::json!({
                "status": status, "execution_id": request.execution_id,
                "external_conversation_id": request.external_conversation_id
            });
            let result: CompletionRecoveryResult = serde_json::from_value(base.clone()).unwrap();
            assert!(result.validate(&request).is_ok());
            for (stage, code) in [
                ("scope", "unsupported_platform"),
                ("identity", "reauth_required"),
                ("identity", "account_mismatch"),
                ("history", "completion_unverified"),
                ("deadline", "deadline_exceeded"),
                ("runner", "transport_unknown"),
            ] {
                let mut value = base.clone();
                value["diagnostic"] = serde_json::json!({"stage":stage,"code":code});
                let result: CompletionRecoveryResult = serde_json::from_value(value).unwrap();
                assert!(result.validate(&request).is_ok());
                for reason in [
                    "document_unavailable",
                    "read_deadline_exceeded",
                    "transport_unavailable",
                    "response_envelope_unverified",
                    "history_envelope_unverified",
                    "history_binding_mismatch",
                    "inventory_unverified",
                    "completion_unverified",
                ] {
                    let mut value = base.clone();
                    value["diagnostic"] =
                        serde_json::json!({"stage":stage,"code":code,"reason":reason});
                    let result: CompletionRecoveryResult = serde_json::from_value(value).unwrap();
                    assert_eq!(
                        result.validate(&request).is_ok(),
                        status == "retained" && stage == "history"
                    );
                    let parsed: CompletionRecoveryReason =
                        serde_json::from_value(reason.into()).unwrap();
                    assert_eq!(parsed.as_str(), reason);
                }
            }
            for diagnostic in [
                serde_json::json!({"stage":"history","code":"arbitrary private text"}),
                serde_json::json!({"stage":"history","code":"completion_unverified","raw":"private"}),
                serde_json::json!({"stage":"arbitrary","code":"completion_unverified"}),
                serde_json::json!({"stage":"identity","code":"completion_unverified"}),
                serde_json::json!({"stage":"history","code":"completion_unverified","reason":"synthetic private detail"}),
                serde_json::json!({"stage":"history","code":"completion_unverified","reason":7}),
                serde_json::json!({"stage":"history","code":"completion_unverified","reason":{"reason":"completion_unverified"}}),
                serde_json::json!({"stage":"history","code":"completion_unverified","reason":"inventory_unverified","raw":"synthetic excluded"}),
            ] {
                let mut value = base.clone();
                value["diagnostic"] = diagnostic;
                assert!(
                    serde_json::from_value::<CompletionRecoveryResult>(value)
                        .map_or(true, |result| result.validate(&request).is_err())
                );
            }
            let mut value = base;
            value["history_json"] = "{}".into();
            assert!(serde_json::from_value::<CompletionRecoveryResult>(value).is_err());
        }
        let mut value = receipt(&request);
        value["status"] = "deleted".into();
        assert!(serde_json::from_value::<CompletionRecoveryResult>(value).is_err());
    }

    #[test]
    fn recovery_message_ids_accept_safe_integer_boundaries_only() {
        let mut value = request();
        value.response_message_id = "9007199254740991".into();
        assert!(value.validate(Uuid::new_v4()).is_ok());
        value.response_message_id = "9007199254740992".into();
        assert!(value.validate(Uuid::new_v4()).is_err());
    }
}

#[cfg(test)]
#[path = "browser_bridge_lifecycle_tests.rs"]
mod lifecycle_tests;

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
    async fn unregistered_cleanup_provider_or_invalid_scope_is_rejected_before_network() {
        use axum::{Router, routing::post};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let session_id = Uuid::new_v4();
        let app = Router::new().route(
            &format!("/v1/sessions/{session_id}/cleanup-conversation"),
            post(move || {
                let calls = observed.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let bridge = BrowserBridge::new(format!("http://{addr}"), "fixture-token".into()).unwrap();
        for provider in ["DeepSeek", "doubao", "glm", "unknown", "Kimi"] {
            for action in [CleanupAction::Delete, CleanupAction::Reconcile] {
                let error = bridge
                    .cleanup_conversation(
                        session_id,
                        Uuid::new_v4(),
                        &CleanupExpectedIdentity {
                            provider: provider.into(),
                            platform_account_id: "fixture-account".into(),
                        },
                        "fixture-chat",
                        action,
                        Some("aabb"),
                    )
                    .await
                    .err()
                    .expect("unregistered provider must not reach runner");
                assert_eq!(error.code, ErrorCode::InvalidRequest);
            }
        }
        for provider in ["kimi", "deepseek"] {
            for (account, chat, ticket) in [
                ("", "fixture-chat", "aabb"),
                ("fixture-account", "", "aabb"),
                ("fixture-account", "fixture-chat", ""),
                ("fixture-account", "fixture-chat", "not-a-ticket"),
            ] {
                for action in [CleanupAction::Delete, CleanupAction::Reconcile] {
                    let error = bridge
                        .cleanup_conversation(
                            session_id,
                            Uuid::new_v4(),
                            &CleanupExpectedIdentity {
                                provider: provider.into(),
                                platform_account_id: account.into(),
                            },
                            chat,
                            action,
                            Some(ticket),
                        )
                        .await
                        .err()
                        .expect("invalid scope must not reach runner");
                    assert_eq!(error.code, ErrorCode::InvalidRequest);
                }
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn cleanup_wire_is_scoped_and_rejects_mismatched_receipts() {
        use axum::{Router, body::to_bytes, extract::Request, routing::post};
        for provider in ["kimi", "deepseek"] {
            for mismatch in [None, Some("execution_id"), Some("external_conversation_id")] {
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
                        "expected_identity": { "provider": provider, "platform_account_id": "fixture-account" },
                        "external_conversation_id": "fixture-chat",
                        "action": "delete", "authorization_ticket": "aabb"
                    }));
                    axum::Json(serde_json::json!({
                        "execution_id": if mismatch == Some("execution_id") { Uuid::new_v4() } else { execution_id },
                        "external_conversation_id": if mismatch == Some("external_conversation_id") { "other-chat" } else { "fixture-chat" },
                        "status": "deleted"
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
                            provider: provider.into(),
                            platform_account_id: "fixture-account".into(),
                        },
                        "fixture-chat",
                        CleanupAction::Delete,
                        Some("aabb"),
                    )
                    .await;
                if mismatch.is_some() {
                    assert!(result.is_err());
                } else {
                    assert_eq!(result.unwrap().status, CleanupStatus::Deleted);
                }
                server.abort();
            }
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
                        schema_version: 2,
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
                schema_version: 2,
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
