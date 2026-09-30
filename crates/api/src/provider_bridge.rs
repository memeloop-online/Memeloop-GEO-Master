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
    CompletionRequest, Message, NormalizedCompletion, ProviderClient, ProviderError,
    ProviderSurface, RequestControl, SearchMode, SecretRef, TokenCenter, Transport,
};
use geo_worker::{HostOp, HostOpError, ModelCompletion, ModelCompletionRequest, TenantScope};

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
        let route = self
            .routes
            .resolve(scope, request.model.as_deref())
            .await
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
        let provider_request = CompletionRequest {
            model,
            messages: {
                let mut messages = Vec::with_capacity(2);
                if let Some(system) = request.system.as_ref() {
                    messages.push(Message {
                        role: "system".into(),
                        content: system.clone(),
                    });
                }
                messages.push(Message {
                    role: "user".into(),
                    content: request.prompt.clone(),
                });
                messages
            },
            max_output_tokens: request.max_output_tokens,
            temperature: None,
            surface: ProviderSurface::OfficialApi,
            search_mode: SearchMode::Disabled,
            include_citations: false,
        };
        let control = RequestControl::new(self.timeout)
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
        let mut messages = Vec::with_capacity(2);
        if let Some(system) = request.system.as_ref() {
            messages.push(Message {
                role: "system".into(),
                content: system.clone(),
            });
        }
        messages.push(Message {
            role: "user".into(),
            content: request.prompt.clone(),
        });
        let provider_request = CompletionRequest {
            model,
            messages,
            max_output_tokens: request.max_output_tokens,
            temperature: None,
            surface: ProviderSurface::OfficialApi,
            search_mode: SearchMode::Disabled,
            include_citations: false,
        };
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

fn map_completion(completion: NormalizedCompletion) -> ModelCompletion {
    ModelCompletion {
        text: completion.text,
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
        ProviderError::TokenUnavailable(message)
        | ProviderError::Http { message, .. }
        | ProviderError::InvalidResponse(message)
        | ProviderError::Transport(message) => HostOpError::failed(HostOp::ModelComplete, message),
    }
}

pub type SharedModelProvider = Arc<dyn ModelProviderBridge>;

#[cfg(test)]
mod tests {
    use super::*;
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
                },
            )
            .await
            .unwrap();
        assert_eq!(
            result,
            ModelCompletion {
                text: "answer".into(),
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
}
