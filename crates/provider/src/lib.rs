//! Provider-side transport for OpenAI-compatible model APIs.
//!
//! This crate deliberately does not contain an HTTP implementation.  The
//! application supplies a [`Transport`] and a [`TokenCenter`] implementation,
//! which keeps endpoints and credentials outside the worker and makes timeout,
//! cancellation, and provider-contract tests deterministic.

use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::time::timeout;
use url::Url;

const MAX_MODEL_LENGTH: usize = 256;
const MAX_MESSAGE_LENGTH: usize = 1_000_000;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Which observation surface a request belongs to.
///
/// Consumer surfaces are never silently represented as official API calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSurface {
    OfficialApi,
    ConsumerSurface,
}

/// Official provider search capability requested for a completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Disabled,
    Official,
}

/// A reference to a Token Center secret.  It is not the secret itself.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 256 {
            return Err(ProviderError::InvalidRequest(
                "secret reference must be non-empty and at most 256 bytes".into(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretRef(***)")
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("***")
    }
}

/// A resolved credential.  Its value is intentionally not exposed publicly.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedToken(String);

impl ResolvedToken {
    /// Constructs a resolved token inside a Token Center implementation.
    ///
    /// The token is intentionally not serializable and its `Debug`/`Display`
    /// implementations are redacted.
    pub fn new(value: String) -> Result<Self, ProviderError> {
        if value.trim().is_empty() {
            return Err(ProviderError::TokenUnavailable(
                "Token Center returned an empty token".into(),
            ));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for ResolvedToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolvedToken(***)")
    }
}

/// Token Center boundary.  Implementations may use a secret manager, but this
/// contract never stores a provider key in configuration or a request payload.
#[async_trait]
pub trait TokenCenter: Send + Sync {
    async fn resolve(&self, secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError>;
}

/// A cancellation/deadline policy supplied to the transport.
#[derive(Clone, Debug)]
pub struct RequestControl {
    timeout: Duration,
    cancellation: Arc<AtomicBool>,
}

impl RequestControl {
    pub fn new(timeout: Duration) -> Result<Self, ProviderError> {
        if timeout.is_zero() {
            return Err(ProviderError::InvalidRequest(
                "request timeout must be greater than zero".into(),
            ));
        }
        Ok(Self {
            timeout,
            cancellation: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn with_cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::SeqCst)
    }
}

/// The transport request passed to an injected HTTP implementation.
#[derive(Clone, PartialEq)]
pub struct TransportRequest {
    pub url: String,
    pub body: Value,
    token: ResolvedToken,
}

impl TransportRequest {
    pub fn bearer_token(&self) -> &str {
        &self.token.0
    }
}

impl fmt::Debug for TransportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportRequest")
            .field("url", &self.url)
            .field("body", &self.body)
            .field("token", &"***")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct TransportResponse {
    pub status: u16,
    pub body: String,
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(
        &self,
        request: TransportRequest,
        control: RequestControl,
    ) -> Result<TransportResponse, ProviderError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    pub surface: ProviderSurface,
    #[serde(default)]
    pub search_mode: SearchMode,
    #[serde(default)]
    pub include_citations: bool,
}

impl CompletionRequest {
    pub fn validate(&self) -> Result<(), ProviderError> {
        let model = self.model.trim();
        if model.is_empty() || model.len() > MAX_MODEL_LENGTH || model.contains("://") {
            return Err(ProviderError::InvalidRequest(
                "model must be a routing identifier, not an endpoint".into(),
            ));
        }
        if self.messages.is_empty() {
            return Err(ProviderError::InvalidRequest(
                "at least one message is required".into(),
            ));
        }
        for message in &self.messages {
            if !matches!(
                message.role.as_str(),
                "system" | "user" | "assistant" | "tool"
            ) {
                return Err(ProviderError::InvalidRequest(
                    "message role is not supported".into(),
                ));
            }
            if message.content.trim().is_empty() || message.content.len() > MAX_MESSAGE_LENGTH {
                return Err(ProviderError::InvalidRequest(
                    "message content must be non-empty and within the size limit".into(),
                ));
            }
        }
        if self.max_output_tokens == Some(0) {
            return Err(ProviderError::InvalidRequest(
                "max_output_tokens must be greater than zero".into(),
            ));
        }
        if self
            .temperature
            .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
        {
            return Err(ProviderError::InvalidRequest(
                "temperature must be finite and between 0 and 2".into(),
            ));
        }
        if self.surface == ProviderSurface::ConsumerSurface
            && self.search_mode == SearchMode::Official
        {
            return Err(ProviderError::InvalidRequest(
                "official search is only valid for official_api observations".into(),
            ));
        }
        if self.include_citations && self.search_mode != SearchMode::Official {
            return Err(ProviderError::InvalidRequest(
                "citations require official search mode".into(),
            ));
        }
        Ok(())
    }

    fn to_provider_body(&self) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": self.messages,
        });
        if let Some(value) = self.max_output_tokens {
            body["max_tokens"] = json!(value);
        }
        if let Some(value) = self.temperature {
            body["temperature"] = json!(value);
        }
        if self.search_mode == SearchMode::Official {
            body["web_search_options"] = json!({});
        }
        if self.include_citations {
            body["include"] = json!(["web_search_call.action.sources"]);
        }
        body
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedCompletion {
    pub request_id: String,
    pub model: String,
    pub text: String,
    pub finish_reason: String,
    pub citations: Vec<Citation>,
    pub usage: TokenUsage,
    pub surface: ProviderSurface,
    pub search_mode: SearchMode,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    #[error("invalid provider request: {0}")]
    InvalidRequest(String),
    #[error("token unavailable: {0}")]
    TokenUnavailable(String),
    #[error("provider request timed out")]
    Timeout,
    #[error("provider request cancelled")]
    Cancelled,
    #[error("provider returned HTTP status {status}: {message}")]
    Http { status: u16, message: String },
    #[error("provider response could not be normalized: {0}")]
    InvalidResponse(String),
    #[error("transport failed: {0}")]
    Transport(String),
}

impl ProviderError {
    fn redact(self) -> Self {
        match self {
            Self::Http { status, message } => Self::Http {
                status,
                message: redact_secrets(&message),
            },
            Self::Transport(message) => Self::Transport(redact_secrets(&message)),
            Self::TokenUnavailable(message) => Self::TokenUnavailable(redact_secrets(&message)),
            other => other,
        }
    }
}

pub fn redact_secrets(message: &str) -> String {
    message
        .split_whitespace()
        .map(|token| {
            let trimmed = token.trim_matches(|character: char| {
                matches!(character, '"' | '\'' | ',' | ';' | '=' | ':')
            });
            if trimmed.len() >= 24
                && !trimmed.contains('/')
                && trimmed.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "-_+.".contains(character)
                })
            {
                "***"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct ProviderClient<T, C> {
    base_url: Url,
    secret_ref: SecretRef,
    transport: Arc<T>,
    token_center: Arc<C>,
}

impl<T, C> ProviderClient<T, C>
where
    T: Transport + 'static,
    C: TokenCenter + 'static,
{
    pub fn new(
        base_url: impl AsRef<str>,
        secret_ref: SecretRef,
        transport: Arc<T>,
        token_center: Arc<C>,
    ) -> Result<Self, ProviderError> {
        let mut base_url = Url::parse(base_url.as_ref()).map_err(|_| {
            ProviderError::InvalidRequest("base URL must be an absolute URL".into())
        })?;
        if base_url.username() != "" || base_url.password().is_some() {
            return Err(ProviderError::InvalidRequest(
                "base URL must not contain credentials".into(),
            ));
        }
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(ProviderError::InvalidRequest(
                "base URL must use HTTP or HTTPS".into(),
            ));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self {
            base_url,
            secret_ref,
            transport,
            token_center,
        })
    }

    pub async fn complete(
        &self,
        request: CompletionRequest,
        control: RequestControl,
    ) -> Result<NormalizedCompletion, ProviderError> {
        request.validate()?;
        if control.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let resolve = self.token_center.resolve(&self.secret_ref);
        tokio::pin!(resolve);
        let deadline = timeout(control.timeout(), &mut resolve);
        tokio::pin!(deadline);
        let token = tokio::select! {
            biased;
            result = &mut deadline => result.map_err(|_| ProviderError::Timeout)?,
            () = wait_for_cancellation(control.clone()) => return Err(ProviderError::Cancelled),
        }
        .map_err(ProviderError::redact)?;
        if control.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let url = self.base_url.join("chat/completions").map_err(|_| {
            ProviderError::InvalidRequest("base URL cannot address completions".into())
        })?;
        let transport_request = TransportRequest {
            url: url.to_string(),
            body: request.to_provider_body(),
            token,
        };
        let sent = self.transport.send(transport_request, control.clone());
        tokio::pin!(sent);
        let deadline = timeout(control.timeout(), &mut sent);
        tokio::pin!(deadline);
        let response = tokio::select! {
            biased;
            result = &mut deadline => result.map_err(|_| ProviderError::Timeout)?,
            () = wait_for_cancellation(control.clone()) => return Err(ProviderError::Cancelled),
        }
        .map_err(ProviderError::redact)?;
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(ProviderError::InvalidResponse(
                "provider response exceeds the size limit".into(),
            ));
        }
        if control.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if !(200..300).contains(&response.status) {
            return Err(ProviderError::Http {
                status: response.status,
                message: error_message(&response.body),
            }
            .redact());
        }
        normalize_response(&response.body, &request)
    }
}

async fn wait_for_cancellation(control: RequestControl) {
    while !control.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "provider returned an error".into())
}

pub fn normalize_response(
    body: &str,
    request: &CompletionRequest,
) -> Result<NormalizedCompletion, ProviderError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| ProviderError::InvalidResponse(format!("invalid JSON: {error}")))?;
    let request_id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::InvalidResponse("response is missing id".into()))?;
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::InvalidResponse("response is missing model".into()))?;
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| ProviderError::InvalidResponse("response is missing choices".into()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| ProviderError::InvalidResponse("response is missing message".into()))?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::InvalidResponse("response is missing text content".into()))?;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let usage = value
        .get("usage")
        .ok_or_else(|| ProviderError::InvalidResponse("response is missing usage".into()))?;
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| ProviderError::InvalidResponse("usage is missing prompt_tokens".into()))?;
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ProviderError::InvalidResponse("usage is missing completion_tokens".into())
        })?;
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| ProviderError::InvalidResponse("usage is missing total_tokens".into()))?;
    let citation_value = value
        .get("citations")
        .cloned()
        .or_else(|| {
            value
                .get("web_search")
                .and_then(|search| search.get("citations"))
                .cloned()
        })
        .or_else(|| message.get("annotations").cloned());
    let citations = citation_value
        .map(parse_citations)
        .transpose()?
        .unwrap_or_default();
    if request.include_citations && citations.is_empty() {
        return Err(ProviderError::InvalidResponse(
            "official search response is missing citations".into(),
        ));
    }
    Ok(NormalizedCompletion {
        request_id: request_id.into(),
        model: model.into(),
        text: text.into(),
        finish_reason,
        citations,
        usage: TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        },
        surface: request.surface,
        search_mode: request.search_mode,
    })
}

fn parse_citations(value: Value) -> Result<Vec<Citation>, ProviderError> {
    let entries = value
        .as_array()
        .ok_or_else(|| ProviderError::InvalidResponse("citations must be an array".into()))?;
    let mut citations = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) == Some("url_citation") {
            let nested = entry.get("url_citation").ok_or_else(|| {
                ProviderError::InvalidResponse("url citation is missing details".into())
            })?;
            citations.push(serde_json::from_value(nested.clone()).map_err(|error| {
                ProviderError::InvalidResponse(format!("invalid URL citation: {error}"))
            })?);
        } else {
            citations.push(serde_json::from_value(entry.clone()).map_err(|error| {
                ProviderError::InvalidResponse(format!("invalid citation: {error}"))
            })?);
        }
    }
    Ok(citations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::time::sleep;

    struct FakeTokenCenter;

    #[async_trait]
    impl TokenCenter for FakeTokenCenter {
        async fn resolve(&self, secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError> {
            assert_eq!(secret_ref.as_str(), "tenant-key");
            ResolvedToken::new("sk-test-secret-that-must-not-log".into())
        }
    }

    struct FakeTransport {
        response: Mutex<Option<TransportResponse>>,
    }

    #[async_trait]
    impl Transport for FakeTransport {
        async fn send(
            &self,
            request: TransportRequest,
            _control: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            assert_eq!(request.bearer_token(), "sk-test-secret-that-must-not-log");
            Ok(self.response.lock().unwrap().take().unwrap())
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "configured-model".into(),
            messages: vec![Message {
                role: "user".into(),
                content: "hello".into(),
            }],
            max_output_tokens: Some(32),
            temperature: None,
            surface: ProviderSurface::OfficialApi,
            search_mode: SearchMode::Official,
            include_citations: true,
        }
    }

    #[test]
    fn validation_keeps_surface_and_official_search_explicit() {
        let mut request = request();
        request.surface = ProviderSurface::ConsumerSurface;
        assert!(matches!(
            request.validate(),
            Err(ProviderError::InvalidRequest(message))
                if message.contains("official_api")
        ));
        request.surface = ProviderSurface::OfficialApi;
        request.search_mode = SearchMode::Disabled;
        assert!(request.validate().is_err());
    }

    #[test]
    fn provider_body_uses_official_search_and_citation_fields() {
        let body = request().to_provider_body();
        assert_eq!(body["web_search_options"], json!({}));
        assert_eq!(body["include"], json!(["web_search_call.action.sources"]));
        assert_eq!(body["max_tokens"], 32);
    }

    #[test]
    fn normalizes_text_citations_and_usage() {
        let body = json!({
            "id": "req-1",
            "model": "answered-model",
            "choices": [{"message": {"content": "answer"}, "finish_reason": "stop"}],
            "citations": [{"url": "https://example.invalid/a", "title": "A"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 5, "total_tokens": 9}
        });
        let result = normalize_response(&body.to_string(), &request()).unwrap();
        assert_eq!(result.text, "answer");
        assert_eq!(result.citations[0].url, "https://example.invalid/a");
        assert_eq!(result.usage.total_tokens, 9);
    }

    #[tokio::test]
    async fn client_injects_url_and_secret_without_logging_secret() {
        let transport = Arc::new(FakeTransport {
            response: Mutex::new(Some(TransportResponse {
                status: 200,
                body: json!({
                    "id": "req-1",
                    "model": "configured-model",
                    "choices": [{"message": {"content": "answer"}, "finish_reason": "stop"}],
                    "citations": [{"url": "https://example.invalid"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string(),
            })),
        });
        let client = ProviderClient::new(
            "https://configured.example/v1/",
            SecretRef::new("tenant-key").unwrap(),
            transport,
            Arc::new(FakeTokenCenter),
        )
        .unwrap();
        let result = client
            .complete(
                request(),
                RequestControl::new(Duration::from_secs(1)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.request_id, "req-1");
    }

    struct SlowTransport;

    #[async_trait]
    impl Transport for SlowTransport {
        async fn send(
            &self,
            _request: TransportRequest,
            _control: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            sleep(Duration::from_millis(50)).await;
            unreachable!("the client timeout should fire first")
        }
    }

    #[tokio::test]
    async fn request_timeout_is_enforced_by_client() {
        let client = ProviderClient::new(
            "https://configured.example/v1/",
            SecretRef::new("tenant-key").unwrap(),
            Arc::new(SlowTransport),
            Arc::new(FakeTokenCenter),
        )
        .unwrap();
        let error = client
            .complete(
                request(),
                RequestControl::new(Duration::from_millis(1)).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::Timeout);
    }

    #[tokio::test]
    async fn request_cancellation_is_enforced_in_flight() {
        let cancellation = Arc::new(AtomicBool::new(false));
        let client = Arc::new(
            ProviderClient::new(
                "https://configured.example/v1/",
                SecretRef::new("tenant-key").unwrap(),
                Arc::new(SlowTransport),
                Arc::new(FakeTokenCenter),
            )
            .unwrap(),
        );
        let control = RequestControl::new(Duration::from_secs(1))
            .unwrap()
            .with_cancellation(Arc::clone(&cancellation));
        let task = tokio::spawn(async move { client.complete(request(), control).await });
        tokio::time::sleep(Duration::from_millis(5)).await;
        cancellation.store(true, Ordering::SeqCst);
        assert_eq!(task.await.unwrap().unwrap_err(), ProviderError::Cancelled);
    }

    #[test]
    fn diagnostics_redact_long_secret_tokens() {
        let error = ProviderError::Transport(
            "provider rejected sk-live-abcdefghijklmnopqrstuvwxyz0123456789".into(),
        )
        .redact();
        assert!(!error.to_string().contains("sk-live-"));
        assert!(error.to_string().contains("***"));
    }
}
