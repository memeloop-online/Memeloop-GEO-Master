//! Per-call project model selection. Credentials stay in this Rust adapter;
//! neither the worker nor inherited production routing receives custom keys.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use geo_domain::{ProjectAiUsage, TenantScope};
use geo_provider::{
    HttpTransport, ProviderClient, ProviderError, ResolvedToken, SecretRef, TokenCenter, Transport,
};
use geo_worker::{HostOp, HostOpError, ModelCompletion, ModelCompletionRequest};

use crate::{
    ModelProviderBridge, ProjectAiSettingsService, ProviderClientBridge, SharedModelProvider,
};

pub struct ProjectConfiguredModelBridge<T = HttpTransport> {
    settings: ProjectAiSettingsService,
    inherited: Option<SharedModelProvider>,
    transport: Arc<T>,
}

impl ProjectConfiguredModelBridge<HttpTransport> {
    pub fn new(
        settings: ProjectAiSettingsService,
        inherited: Option<SharedModelProvider>,
    ) -> Result<Self, ProviderError> {
        Ok(Self::with_transport(
            settings,
            inherited,
            Arc::new(HttpTransport::public_only()?),
        ))
    }
}

impl<T: Transport + 'static> ProjectConfiguredModelBridge<T> {
    pub fn with_transport(
        settings: ProjectAiSettingsService,
        inherited: Option<SharedModelProvider>,
        transport: Arc<T>,
    ) -> Self {
        Self {
            settings,
            inherited,
            transport,
        }
    }

    pub async fn complete_for_usage(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.complete_for_usage_with_revision(scope, usage, request)
            .await
            .map(|(completion, _)| completion)
    }

    pub async fn complete_for_usage_with_revision(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
        request: &ModelCompletionRequest,
    ) -> Result<(ModelCompletion, i64), HostOpError> {
        self.complete_selected(scope, usage, request, None).await
    }

    pub async fn complete_for_usage_at_revision(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
        expected_revision: i64,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.complete_selected(scope, usage, request, Some(expected_revision))
            .await
            .map(|(completion, _)| completion)
    }

    async fn complete_selected(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
        request: &ModelCompletionRequest,
        expected_revision: Option<i64>,
    ) -> Result<(ModelCompletion, i64), HostOpError> {
        // Read the authoritative store each time; saving or clearing a route
        // affects the next call without caching a decrypted credential.
        let (config, revision) = self
            .settings
            .resolve_with_revision(scope, usage)
            .await
            .map_err(|_| {
                HostOpError::failed(HostOp::ModelComplete, "project model settings unavailable")
            })?;
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(HostOpError::denied(
                HostOp::ModelComplete,
                "project model settings revision changed",
            ));
        }
        let Some(config) = config else {
            return match &self.inherited {
                Some(inherited) => inherited
                    .complete(scope, request)
                    .await
                    .map(|completion| (completion, revision)),
                None => Err(HostOpError::capability_missing(
                    HostOp::ModelComplete,
                    "no inherited or project model is configured",
                )),
            };
        };
        let token = ResolvedToken::new(config.api_key).map_err(|_| custom_unavailable())?;
        let reference =
            SecretRef::new("project-configured-model").map_err(|_| custom_unavailable())?;
        let client = ProviderClient::new(
            config.base_url,
            reference.clone(),
            Arc::clone(&self.transport),
            Arc::new(ProjectCredential { reference, token }),
        )
        .map_err(|_| custom_unavailable())?;
        let bridge = ProviderClientBridge::new(client, config.model, Duration::from_secs(60))?;
        // ProviderClientBridge's single-model allowlist rejects a worker
        // selecting any model other than the saved project configuration.
        bridge
            .complete(scope, request)
            .await
            .map(|completion| (completion, revision))
    }
}

#[async_trait]
impl<T: Transport + 'static> ModelProviderBridge for ProjectConfiguredModelBridge<T> {
    async fn complete(
        &self,
        scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.complete_for_usage(scope, ProjectAiUsage::WorkbenchContent, request)
            .await
    }
}

fn custom_unavailable() -> HostOpError {
    HostOpError::failed(HostOp::ModelComplete, "project model unavailable")
}

struct ProjectCredential {
    reference: SecretRef,
    token: ResolvedToken,
}

#[async_trait]
impl TokenCenter for ProjectCredential {
    async fn resolve(&self, reference: &SecretRef) -> Result<ResolvedToken, ProviderError> {
        if reference.as_str() != self.reference.as_str() {
            return Err(ProviderError::TokenUnavailable(
                "project model credential unavailable".into(),
            ));
        }
        Ok(self.token.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UpdateProjectAiSettings;
    use geo_domain::ProjectAiMode;
    use geo_provider::{RequestControl, TransportRequest, TransportResponse};
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingTransport(Mutex<Vec<String>>);

    #[async_trait]
    impl Transport for RecordingTransport {
        async fn send(
            &self,
            request: TransportRequest,
            _: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            assert_eq!(request.bearer_token(), "synthetic-project-secret");
            let model = request.body["model"].as_str().unwrap().to_owned();
            self.0.lock().unwrap().push(model.clone());
            Ok(TransportResponse {
                status: 200,
                body: serde_json::json!({
                    "id": "synthetic-request", "model": model,
                    "choices": [{"message": {"content": "answer"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string(),
            })
        }
    }

    fn scope() -> TenantScope {
        TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        )
    }

    fn request() -> ModelCompletionRequest {
        ModelCompletionRequest {
            prompt: "question".into(),
            system: None,
            model: None,
            max_output_tokens: Some(16),
            messages: Vec::new(),
            tools: Vec::new(),
        }
    }

    async fn save(
        service: &ProjectAiSettingsService,
        scope: &TenantScope,
        usage: ProjectAiUsage,
        revision: i64,
        model: Option<&str>,
    ) {
        service
            .save(
                scope,
                usage,
                UpdateProjectAiSettings {
                    expected_revision: revision,
                    mode: if model.is_some() {
                        ProjectAiMode::Custom
                    } else {
                        ProjectAiMode::Inherit
                    },
                    model: model.map(str::to_owned),
                    base_url: model.map(|_| "https://models.example.invalid/v1".into()),
                    api_key: model.map(|_| "synthetic-project-secret".into()),
                    clear_api_key: false,
                    prefer_connected_account: None,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn saved_route_changes_apply_next_call_without_inherited_provider() {
        let settings = ProjectAiSettingsService::development();
        let transport = Arc::new(RecordingTransport::default());
        let bridge = ProjectConfiguredModelBridge::with_transport(
            settings.clone(),
            None,
            Arc::clone(&transport),
        );
        let scope = scope();
        let usage = ProjectAiUsage::WorkbenchContent;
        assert_eq!(
            bridge.complete(&scope, &request()).await.unwrap_err().code,
            geo_worker::HostOpErrorCode::CapabilityMissing
        );
        save(&settings, &scope, usage, 0, Some("first-model")).await;
        assert_eq!(
            bridge.complete(&scope, &request()).await.unwrap().model,
            "first-model"
        );
        save(&settings, &scope, usage, 1, Some("second-model")).await;
        assert!(
            bridge
                .complete_for_usage_at_revision(&scope, usage, 1, &request())
                .await
                .is_err()
        );
        assert_eq!(
            bridge.complete(&scope, &request()).await.unwrap().model,
            "second-model"
        );
        let mut alternate = request();
        alternate.model = Some("unconfigured-model".into());
        assert!(bridge.complete(&scope, &alternate).await.is_err());
        assert_eq!(
            *transport.0.lock().unwrap(),
            ["first-model", "second-model"]
        );
        save(&settings, &scope, usage, 2, None).await;
        assert_eq!(
            bridge.complete(&scope, &request()).await.unwrap_err().code,
            geo_worker::HostOpErrorCode::CapabilityMissing
        );
    }

    #[tokio::test]
    async fn routes_are_isolated_by_operator_tenant_project_and_usage() {
        let settings = ProjectAiSettingsService::development();
        let transport = Arc::new(RecordingTransport::default());
        let bridge = ProjectConfiguredModelBridge::with_transport(
            settings.clone(),
            None,
            Arc::clone(&transport),
        );
        let allowed = scope();
        save(
            &settings,
            &allowed,
            ProjectAiUsage::WorkbenchContent,
            0,
            Some("workbench-model"),
        )
        .await;
        save(
            &settings,
            &allowed,
            ProjectAiUsage::ObservationAnalysis,
            0,
            Some("analysis-model"),
        )
        .await;
        let forbidden = [
            TenantScope::new(
                uuid::Uuid::new_v4().into(),
                allowed.tenant_id,
                allowed.project_id,
            ),
            TenantScope::new(
                allowed.operator_id,
                uuid::Uuid::new_v4().into(),
                allowed.project_id,
            ),
            TenantScope::new(
                allowed.operator_id,
                allowed.tenant_id,
                Some(uuid::Uuid::new_v4().into()),
            ),
            TenantScope::new(allowed.operator_id, allowed.tenant_id, None),
        ];
        for scope in forbidden {
            assert!(bridge.complete(&scope, &request()).await.is_err());
        }
        assert!(transport.0.lock().unwrap().is_empty());
        assert_eq!(
            bridge.complete(&allowed, &request()).await.unwrap().model,
            "workbench-model"
        );
        assert_eq!(
            bridge
                .complete_for_usage(&allowed, ProjectAiUsage::ObservationAnalysis, &request())
                .await
                .unwrap()
                .model,
            "analysis-model"
        );
    }

    struct Inherited;
    #[async_trait]
    impl ModelProviderBridge for Inherited {
        async fn complete(
            &self,
            _: &TenantScope,
            _: &ModelCompletionRequest,
        ) -> Result<ModelCompletion, HostOpError> {
            Err(HostOpError::denied(
                HostOp::ModelComplete,
                "inherited authorization",
            ))
        }
    }

    #[tokio::test]
    async fn inherit_preserves_existing_provider_authorization() {
        let settings = ProjectAiSettingsService::development();
        let transport = Arc::new(RecordingTransport::default());
        let bridge = ProjectConfiguredModelBridge::with_transport(
            settings.clone(),
            Some(Arc::new(Inherited)),
            Arc::clone(&transport),
        );
        let scope = scope();
        assert_eq!(
            bridge.complete(&scope, &request()).await.unwrap_err().code,
            geo_worker::HostOpErrorCode::Denied
        );
        save(
            &settings,
            &scope,
            ProjectAiUsage::WorkbenchContent,
            0,
            Some("custom-model"),
        )
        .await;
        assert_eq!(
            bridge.complete(&scope, &request()).await.unwrap().model,
            "custom-model"
        );
        // A workbench override never becomes observation-analysis inheritance.
        assert_eq!(
            bridge
                .complete_for_usage(&scope, ProjectAiUsage::ObservationAnalysis, &request())
                .await
                .unwrap_err()
                .code,
            geo_worker::HostOpErrorCode::Denied
        );
        save(&settings, &scope, ProjectAiUsage::WorkbenchContent, 1, None).await;
        assert_eq!(
            bridge.complete(&scope, &request()).await.unwrap_err().code,
            geo_worker::HostOpErrorCode::Denied
        );
        assert_eq!(*transport.0.lock().unwrap(), ["custom-model"]);
    }
}
