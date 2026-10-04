//! Internal authenticated browser runner client. The browser never receives
//! storageState, proxy credentials, runner token, or arbitrary navigation APIs.

use geo_domain::{AppError, ErrorCode};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone)]
pub struct BrowserBridge {
    client: Client,
    base_url: String,
    token: String,
}

impl BrowserBridge {
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
            .timeout(std::time::Duration::from_secs(60))
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

    pub async fn snapshot(&self, id: Uuid) -> Result<BrowserSnapshot, AppError> {
        let response = self
            .client
            .get(self.endpoint(id, "/snapshot"))
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

    pub async fn action(
        &self,
        id: Uuid,
        action: &BrowserAction,
    ) -> Result<BrowserSnapshot, AppError> {
        let response = self
            .client
            .post(self.endpoint(id, "/actions"))
            .bearer_auth(&self.token)
            .json(action)
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
        if !matches!(operation, "publish" | "measure" | "lookup") || !payload.is_object() {
            return Err(AppError::invalid_request("unsupported browser operation"));
        }
        let response = self
            .client
            .post(format!("{}/v1/executions", self.base_url))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "execution_id":execution_id,
                "session_id":session_id,
                "operation":operation,
                "payload":payload,
            }))
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

#[derive(Debug, Deserialize)]
pub struct RunnerCapabilities {
    pub connectors: Vec<RunnerConnector>,
}

#[derive(Debug, Deserialize)]
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
#[derive(Deserialize)]
pub struct BrowserExecution {
    pub execution_id: Uuid,
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

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct BrowserSnapshot {
    pub phase: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub screenshot_base64: String,
    #[serde(default)]
    pub identity: Option<BrowserIdentity>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserAction {
    Click { x: f64, y: f64 },
    Type { text: String },
    Key { key: String },
    Scroll { delta_y: f64 },
}

impl Serialize for BrowserAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Click { x, y } => {
                map.serialize_entry("kind", "click")?;
                map.serialize_entry("x", x)?;
                map.serialize_entry("y", y)?;
            }
            Self::Type { text } => {
                map.serialize_entry("kind", "type")?;
                map.serialize_entry("text", text)?;
            }
            Self::Key { key } => {
                map.serialize_entry("kind", "key")?;
                map.serialize_entry("key", key)?;
            }
            Self::Scroll { delta_y } => {
                map.serialize_entry("kind", "scroll")?;
                map.serialize_entry("delta_y", delta_y)?;
            }
        }
        map.end()
    }
}

/// This type is never Serialize/Debug: raw storageState is server-only.
#[derive(Deserialize)]
pub struct VerifiedBrowserSession {
    pub identity: BrowserIdentity,
    pub storage_state: serde_json::Value,
}
