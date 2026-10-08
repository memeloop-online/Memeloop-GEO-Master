//! Model provider bridge used by the host-op implementation.
//!
//! The API owns provider routing and the Token Center boundary.  The worker
//! only sees the narrow [`geo_worker::ModelCompletion`] DTO, never an endpoint
//! or credential.  Concrete HTTP and Token Center implementations are injected
//! by the application; this module intentionally contains neither.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use geo_provider::{
    CompletionRequest, FunctionCall, FunctionDefinition, Message, NormalizedCompletion,
    ProviderClient, ProviderError, ProviderSurface, RequestControl, SearchMode, SecretRef,
    TokenCenter, ToolCall, ToolDefinition, Transport,
};
use geo_worker::{
    HostOp, HostOpError, ModelCompletion, ModelCompletionRequest, ModelToolCall, TenantScope,
};

/// The API-side model capability used by [`crate::RepositoryHostOps`].
///
/// `scope` is part of the contract even when a particular adapter uses a
/// process-wide route.  Tenant-aware implementations must use it to select a
/// permitted provider route and secret reference; callers cannot supply either
/// through the worker request.
#[async_trait]
pub trait ModelProviderBridge: Send + Sync {
    async fn complete(
        &self,
        scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError>;
}

/// A provider route selected by the API for one tenant/project scope.
///
/// The route contains only an opaque Token Center reference.  Implementations
/// must never put the resolved token into this value or persist the endpoint in
/// a worker request.
#[derive(Clone)]
pub struct ProviderRoute {
    pub base_url: String,
    pub secret_ref: SecretRef,
    pub model: String,
}

/// Resolves a provider route from the Rust-owned run scope.
#[async_trait]
pub trait ProviderRouteResolver: Send + Sync {
    async fn resolve(
        &self,
        scope: &TenantScope,
        requested_model: Option<&str>,
    ) -> Result<ProviderRoute, ProviderError>;
}

/// Scope-aware adapter that constructs a short-lived generic provider client
/// from an injected route.  This is the preferred multi-tenant integration;
/// the simpler [`ProviderClientBridge`] is useful only for a process-wide
/// route.
pub struct RoutedProviderClientBridge<T, C, R> {
    transport: Arc<T>,
    token_center: Arc<C>,
    routes: Arc<R>,
    timeout: Duration,
}

impl<T, C, R> RoutedProviderClientBridge<T, C, R>
where
    T: Transport + 'static,
    C: TokenCenter + 'static,
    R: ProviderRouteResolver + 'static,
{
    pub fn new(
        transport: Arc<T>,
        token_center: Arc<C>,
        routes: Arc<R>,
        timeout: Duration,
    ) -> Result<Self, HostOpError> {
        if timeout.is_zero() {
            return Err(HostOpError::invalid_request(
                HostOp::ModelComplete,
                "provider timeout must be greater than zero",
            ));
        }
        Ok(Self {
            transport,
            token_center,
            routes,
            timeout,
        })
    }
}

#[async_trait]
impl<T, C, R> ModelProviderBridge for RoutedProviderClientBridge<T, C, R>
where
    T: Transport + 'static,
    C: TokenCenter + 'static,
    R: ProviderRouteResolver + 'static,
{
    async fn complete(
        &self,
        scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let route = tokio::time::timeout_at(
            deadline,
            self.routes.resolve(scope, request.model.as_deref()),
        )
        .await
        .map_err(|_| map_provider_error(ProviderError::Timeout, self.timeout))?
        .map_err(|error| map_provider_error(error, self.timeout))?;
        let client = ProviderClient::new(
            route.base_url,
            route.secret_ref,
            Arc::clone(&self.transport),
            Arc::clone(&self.token_center),
        )
        .map_err(|error| map_provider_error(error, self.timeout))?;
        let model = if route.model.trim().is_empty() {
            return Err(HostOpError::invalid_request(
                HostOp::ModelComplete,
                "provider route resolver returned an empty model",
            ));
        } else {
            route.model
        };
        let provider_request = provider_request(model, request);
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(map_provider_error(ProviderError::Timeout, self.timeout));
        }
        let control = RequestControl::new(remaining)
            .map_err(|error| map_provider_error(error, self.timeout))?;
        client
            .complete(provider_request, control)
            .await
            .map(map_completion)
            .map_err(|error| map_provider_error(error, self.timeout))
    }
}

/// Adapter from the generic provider client to the worker host-op contract.
///
/// The URL and [`geo_provider::SecretRef`] are supplied at application
/// assembly.  They are not serialised, logged, or accepted from JavaScript.
pub struct ProviderClientBridge<T, C> {
    client: ProviderClient<T, C>,
    default_model: String,
    allowed_models: BTreeSet<String>,
    timeout: Duration,
}

impl<T, C> ProviderClientBridge<T, C>
where
    T: Transport + 'static,
    C: TokenCenter + 'static,
{
    pub fn new(
        client: ProviderClient<T, C>,
        default_model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, HostOpError> {
        let default_model = default_model.into();
        if default_model.trim().is_empty() {
            return Err(HostOpError::invalid_request(
                HostOp::ModelComplete,
                "provider default model must not be empty",
            ));
        }
        if timeout.is_zero() {
            return Err(HostOpError::invalid_request(
                HostOp::ModelComplete,
                "provider timeout must be greater than zero",
            ));
        }
        let allowed_models = [default_model.clone()].into_iter().collect();
        Ok(Self {
            client,
            default_model,
            allowed_models,
            timeout,
        })
    }

    /// Replaces the default single-model allow-list with explicit routing IDs.
    ///
    /// The list is held in the Rust bridge and is never supplied by the
    /// worker.  An empty list or a list that omits the configured default is
    /// rejected so a route can never silently become unusable.
    pub fn with_allowed_models(
        mut self,
        allowed_models: impl IntoIterator<Item = String>,
    ) -> Result<Self, HostOpError> {
        let allowed_models = allowed_models.into_iter().collect::<BTreeSet<_>>();
        if allowed_models.is_empty() || !allowed_models.contains(&self.default_model) {
            return Err(HostOpError::invalid_request(
                HostOp::ModelComplete,
                "provider model allow-list must include the default model",
            ));
        }
        self.allowed_models = allowed_models;
        Ok(self)
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[async_trait]
impl<T, C> ModelProviderBridge for ProviderClientBridge<T, C>
where
    T: Transport + 'static,
    C: TokenCenter + 'static,
{
    async fn complete(
        &self,
        _scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.default_model)
            .to_owned();
        if !self.allowed_models.contains(&model) {
            return Err(HostOpError::invalid_request(
                HostOp::ModelComplete,
                "requested model is not allowed by the provider route",
            ));
        }
        let provider_request = provider_request(model, request);
        let control = RequestControl::new(self.timeout)
            .map_err(|error| map_provider_error(error, self.timeout))?;
        let completion = self
            .client
            .complete(provider_request, control)
            .await
            .map_err(|error| map_provider_error(error, self.timeout))?;
        Ok(map_completion(completion))
    }
}

fn provider_request(model: String, request: &ModelCompletionRequest) -> CompletionRequest {
    let mut messages = Vec::with_capacity(request.messages.len().max(1) + 1);
    if let Some(system) = request.system.as_ref() {
        messages.push(Message {
            role: "system".into(),
            content: Some(system.clone()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    if request.messages.is_empty() {
        messages.push(Message {
            role: "user".into(),
            content: Some(request.prompt.clone()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    } else {
        messages.extend(request.messages.iter().map(|message| Message {
            role: message.role.clone(),
            content: message.content.clone(),
            tool_calls: message.tool_calls.iter().map(map_tool_call).collect(),
            tool_call_id: message.tool_call_id.clone(),
        }));
    }
    CompletionRequest {
        model,
        messages,
        tools: request
            .tools
            .iter()
            .map(|tool| ToolDefinition {
                kind: tool.kind.clone(),
                function: FunctionDefinition {
                    name: tool.function.name.clone(),
                    description: tool.function.description.clone(),
                    parameters: tool.function.parameters.clone(),
                },
            })
            .collect(),
        max_output_tokens: request.max_output_tokens,
        temperature: None,
        surface: ProviderSurface::OfficialApi,
        search_mode: SearchMode::Disabled,
        include_citations: false,
    }
}

fn map_tool_call(call: &ModelToolCall) -> ToolCall {
    ToolCall {
        id: call.id.clone(),
        kind: call.kind.clone(),
        function: FunctionCall {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        },
    }
}

fn map_completion(completion: NormalizedCompletion) -> ModelCompletion {
    ModelCompletion {
        text: completion.text,
        tool_calls: completion
            .tool_calls
            .into_iter()
            .map(|call| ModelToolCall {
                id: call.id,
                kind: call.kind,
                function: geo_worker::ModelToolFunctionCall {
                    name: call.function.name,
                    arguments: call.function.arguments,
                },
            })
            .collect(),
        model: completion.model,
        prompt_tokens: completion.usage.prompt_tokens,
        completion_tokens: completion.usage.completion_tokens,
        finish_reason: completion.finish_reason,
    }
}

fn map_provider_error(error: ProviderError, timeout: Duration) -> HostOpError {
    match error {
        ProviderError::InvalidRequest(message) => {
            HostOpError::invalid_request(HostOp::ModelComplete, message)
        }
        ProviderError::Cancelled => HostOpError::cancelled(HostOp::ModelComplete),
        ProviderError::Timeout => HostOpError::deadline_exceeded(
            HostOp::ModelComplete,
            timeout.as_millis().min(u64::MAX as u128) as u64,
        ),
        ProviderError::Http { status, .. } => HostOpError::failed(
            HostOp::ModelComplete,
            format!("model provider returned HTTP status {status}"),
        ),
        ProviderError::TokenUnavailable(message)
        | ProviderError::InvalidResponse(message)
        | ProviderError::Transport(message) => HostOpError::failed(HostOp::ModelComplete, message),
    }
}

pub type SharedModelProvider = Arc<dyn ModelProviderBridge>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_http_failure_keeps_status_without_response_details() {
        for status in [401, 429, 503] {
            let error = map_provider_error(
                ProviderError::Http {
                    status,
                    message: "synthetic confidential response body".into(),
                },
                Duration::from_secs(30),
            );
            assert_eq!(
                error.message,
                format!("model provider returned HTTP status {status}")
            );
            assert!(!error.message.contains("confidential"));
        }
    }

    use async_trait::async_trait;
    use geo_domain::TenantScope;
    use geo_provider::{ResolvedToken, SecretRef, TransportRequest, TransportResponse};
    use std::sync::Mutex;
    use uuid::Uuid;

    struct Token;

    #[async_trait]
    impl TokenCenter for Token {
        async fn resolve(&self, secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError> {
            assert_eq!(secret_ref.as_str(), "tenant-provider-ref");
            ResolvedToken::new("injected-token".into())
        }
    }

    struct TransportStub {
        requests: Mutex<Vec<TransportRequest>>,
    }

    #[async_trait]
    impl Transport for TransportStub {
        async fn send(
            &self,
            request: TransportRequest,
            _control: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            self.requests.lock().unwrap().push(request);
            Ok(TransportResponse {
                status: 200,
                body: r#"{"id":"req-1","model":"model-a","choices":[{"message":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#.into(),
            })
        }
    }

    struct RouteStub {
        seen: Mutex<Option<(TenantScope, Option<String>)>>,
    }

    #[async_trait]
    impl ProviderRouteResolver for RouteStub {
        async fn resolve(
            &self,
            scope: &TenantScope,
            requested_model: Option<&str>,
        ) -> Result<ProviderRoute, ProviderError> {
            *self.seen.lock().unwrap() = Some((scope.clone(), requested_model.map(str::to_owned)));
            Ok(ProviderRoute {
                base_url: "https://provider.invalid/v1".into(),
                secret_ref: SecretRef::new("tenant-provider-ref")?,
                model: requested_model.unwrap_or("resolved-model").into(),
            })
        }
    }

    fn scope() -> TenantScope {
        TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None)
    }

    struct HangingRoute;

    #[async_trait]
    impl ProviderRouteResolver for HangingRoute {
        async fn resolve(
            &self,
            _: &TenantScope,
            _: Option<&str>,
        ) -> Result<ProviderRoute, ProviderError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn routed_lookup_is_inside_provider_deadline() {
        let transport = Arc::new(TransportStub {
            requests: Mutex::new(Vec::new()),
        });
        let bridge = RoutedProviderClientBridge::new(
            Arc::clone(&transport),
            Arc::new(Token),
            Arc::new(HangingRoute),
            Duration::from_millis(10),
        )
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            bridge.complete(
                &scope(),
                &ModelCompletionRequest {
                    prompt: "question".into(),
                    system: None,
                    model: None,
                    max_output_tokens: None,
                    messages: Vec::new(),
                    tools: Vec::new(),
                },
            ),
        )
        .await
        .expect("route lookup must not hang outside the provider deadline");
        assert_eq!(
            result.unwrap_err().code,
            geo_worker::HostOpErrorCode::DeadlineExceeded
        );
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn adapter_maps_host_request_without_exposing_token_or_endpoint() {
        let transport = Arc::new(TransportStub {
            requests: Mutex::new(Vec::new()),
        });
        let client = ProviderClient::new(
            "https://provider.invalid/v1",
            SecretRef::new("tenant-provider-ref").unwrap(),
            Arc::clone(&transport),
            Arc::new(Token),
        )
        .unwrap();
        let bridge =
            ProviderClientBridge::new(client, "model-default", Duration::from_secs(2)).unwrap();
        let result = bridge
            .complete(
                &scope(),
                &ModelCompletionRequest {
                    prompt: "question".into(),
                    system: Some("system".into()),
                    model: None,
                    max_output_tokens: Some(64),
                    messages: Vec::new(),
                    tools: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            result,
            ModelCompletion {
                text: "answer".into(),
                tool_calls: Vec::new(),
                model: "model-a".into(),
                prompt_tokens: 2,
                completion_tokens: 3,
                finish_reason: "stop".into(),
            }
        );
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].bearer_token(), "injected-token");
        assert_eq!(requests[0].body["model"], "model-default");
        assert_eq!(requests[0].body["messages"][0]["role"], "system");
        assert_eq!(requests[0].body["messages"][1]["content"], "question");
    }

    #[tokio::test]
    async fn adapter_rejects_zero_timeout_before_provider_call() {
        let client = ProviderClient::new(
            "https://provider.invalid/v1",
            SecretRef::new("ref").unwrap(),
            Arc::new(TransportStub {
                requests: Mutex::new(Vec::new()),
            }),
            Arc::new(Token),
        )
        .unwrap();
        let result = ProviderClientBridge::new(client, "model", Duration::ZERO);
        assert_eq!(
            result.as_ref().err().map(|error| error.code),
            Some(geo_worker::HostOpErrorCode::InvalidRequest)
        );
    }

    #[tokio::test]
    async fn process_bridge_rejects_models_outside_rust_allow_list() {
        let client = ProviderClient::new(
            "https://provider.invalid/v1",
            SecretRef::new("ref").unwrap(),
            Arc::new(TransportStub {
                requests: Mutex::new(Vec::new()),
            }),
            Arc::new(Token),
        )
        .unwrap();
        let bridge =
            ProviderClientBridge::new(client, "model-default", Duration::from_secs(2)).unwrap();
        let error = bridge
            .complete(
                &scope(),
                &ModelCompletionRequest {
                    prompt: "question".into(),
                    system: None,
                    model: Some("model-untrusted".into()),
                    max_output_tokens: None,
                    messages: Vec::new(),
                    tools: Vec::new(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, geo_worker::HostOpErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn routed_bridge_resolves_route_from_rust_owned_scope() {
        let transport = Arc::new(TransportStub {
            requests: Mutex::new(Vec::new()),
        });
        let routes = Arc::new(RouteStub {
            seen: Mutex::new(None),
        });
        let bridge = RoutedProviderClientBridge::new(
            Arc::clone(&transport),
            Arc::new(Token),
            Arc::clone(&routes),
            Duration::from_secs(2),
        )
        .unwrap();
        let run_scope = scope();
        bridge
            .complete(
                &run_scope,
                &ModelCompletionRequest {
                    prompt: "question".into(),
                    system: None,
                    model: Some("model-requested".into()),
                    max_output_tokens: None,
                    messages: Vec::new(),
                    tools: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            routes.seen.lock().unwrap().as_ref(),
            Some(&(run_scope, Some("model-requested".into())))
        );
        assert_eq!(
            transport.requests.lock().unwrap()[0].body["model"],
            "model-requested"
        );
    }

    struct ToolTransport {
        requests: Mutex<Vec<TransportRequest>>,
    }

    #[async_trait]
    impl Transport for ToolTransport {
        async fn send(
            &self,
            request: TransportRequest,
            _control: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            self.requests.lock().unwrap().push(request);
            Ok(TransportResponse {
                status: 200,
                body: r#"{"id":"req-2","model":"model-a","choices":[{"message":{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"knowledge_search","arguments":"{\"query\":\"warranty\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#.into(),
            })
        }
    }

    #[tokio::test]
    async fn tool_definition_and_assistant_result_roundtrip_through_bridge() {
        let transport = Arc::new(ToolTransport {
            requests: Mutex::new(Vec::new()),
        });
        let client = ProviderClient::new(
            "https://provider.invalid/v1",
            SecretRef::new("tenant-provider-ref").unwrap(),
            Arc::clone(&transport),
            Arc::new(Token),
        )
        .unwrap();
        let bridge = ProviderClientBridge::new(client, "model-a", Duration::from_secs(2)).unwrap();
        let request = ModelCompletionRequest {
            prompt: String::new(),
            system: None,
            model: None,
            max_output_tokens: None,
            tools: vec![geo_worker::ModelToolDefinition {
                kind: "function".into(),
                function: geo_worker::ModelToolFunctionDefinition {
                    name: "knowledge_search".into(),
                    description: "Search scoped knowledge".into(),
                    parameters: serde_json::json!({"type":"object","properties":{"query":{"type":"string"}}}),
                },
            }],
            messages: vec![geo_worker::ModelMessage {
                role: "user".into(),
                content: Some("question".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
        };
        let result = bridge.complete(&scope(), &request).await.unwrap();
        assert!(result.text.is_empty());
        assert_eq!(result.tool_calls[0].id, "call_1");
        assert_eq!(
            result.tool_calls[0].function.arguments,
            r#"{"query":"warranty"}"#
        );
        {
            let requests = transport.requests.lock().unwrap();
            assert_eq!(
                requests[0].body["tools"][0]["function"]["name"],
                "knowledge_search"
            );
            assert_eq!(requests[0].body["messages"][0]["content"], "question");
        }

        let mut followup = request;
        followup.messages.push(geo_worker::ModelMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: result.tool_calls,
            tool_call_id: None,
        });
        followup.messages.push(geo_worker::ModelMessage {
            role: "tool".into(),
            content: Some("{\"result\":\"found\"}".into()),
            tool_calls: Vec::new(),
            tool_call_id: Some("call_1".into()),
        });
        bridge.complete(&scope(), &followup).await.unwrap();
        let requests = transport.requests.lock().unwrap();
        assert!(requests[1].body["messages"][1]["content"].is_null());
        assert_eq!(requests[1].body["messages"][2]["tool_call_id"], "call_1");
    }
}
