//! Explicit deployment inference test for one pinned project. This does not
//! resolve durable model grants or change the production Token Center route.

use std::sync::Arc;

use async_trait::async_trait;
use geo_api::{AppState, EmbeddedAgentRuntime, ModelProviderBridge, SharedModelProvider};
use geo_domain::TenantScope;
use geo_provider::{HttpTransport, Transport};
use geo_worker::{HostOp, HostOpError, ModelCompletion, ModelCompletionRequest};

use crate::{
    config::ScopedTestAiConfig,
    runtime::{self, AssemblyError},
};

struct ExactProjectProvider {
    scope: TenantScope,
    inner: SharedModelProvider,
}

#[async_trait]
impl ModelProviderBridge for ExactProjectProvider {
    async fn complete(
        &self,
        scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        // Never treat a tenant-wide scope as a project grant. This check
        // precedes token resolution, transport, and the model allowlist.
        if self.scope.project_id.is_none() || self.scope != *scope {
            return Err(HostOpError::denied(
                HostOp::ModelComplete,
                "test model unavailable for this scope",
            ));
        }
        self.inner.complete(scope, request).await
    }
}

pub fn assemble(
    state: &AppState,
    ai: &ScopedTestAiConfig,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    assemble_with_transport(
        state,
        ai,
        Arc::new(HttpTransport::new().map_err(|_| AssemblyError::ProviderConfiguration)?),
    )
}

fn assemble_with_transport<T: Transport + 'static>(
    state: &AppState,
    ai: &ScopedTestAiConfig,
    transport: Arc<T>,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    let provider = guarded_provider(ai, transport)?;
    // The same exact-project provider is registered for P00 host ops and
    // content generation by assemble_with_provider.
    runtime::assemble_with_provider(state, &ai.bundle_path, &ai.bundle_sha256, provider)
}

fn guarded_provider<T: Transport + 'static>(
    ai: &ScopedTestAiConfig,
    transport: Arc<T>,
) -> Result<SharedModelProvider, AssemblyError> {
    if ai.scope.project_id.is_none() {
        return Err(AssemblyError::ProviderConfiguration);
    }
    let inner = runtime::injected_provider(&ai.base_url, &ai.api_key, &ai.model, transport)?;
    Ok(Arc::new(ExactProjectProvider {
        scope: ai.scope.clone(),
        inner,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_domain::{AgentRuntime, AppendMessage, CreateConversation, RuntimeCapability};
    use geo_provider::{ProviderError, RequestControl, TransportRequest, TransportResponse};
    use geo_worker::HostOpErrorCode;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingTransport(AtomicUsize);

    #[async_trait]
    impl Transport for CountingTransport {
        async fn send(
            &self,
            request: TransportRequest,
            _control: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            assert_eq!(request.bearer_token(), "fake-test-secret");
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(TransportResponse {
                status: 200,
                body: r#"{"id":"test-request","model":"test-model","choices":[{"message":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#.into(),
            })
        }
    }

    fn config() -> ScopedTestAiConfig {
        ScopedTestAiConfig {
            scope: TenantScope::new(
                uuid::Uuid::new_v4().into(),
                uuid::Uuid::new_v4().into(),
                Some(uuid::Uuid::new_v4().into()),
            ),
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "fake-test-secret".into(),
            model: "test-model".into(),
            bundle_path: String::new(),
            bundle_sha256: String::new(),
        }
    }

    fn request(model: Option<&str>) -> ModelCompletionRequest {
        ModelCompletionRequest {
            prompt: "question".into(),
            system: None,
            model: model.map(str::to_owned),
            max_output_tokens: None,
            messages: Vec::new(),
            tools: Vec::new(),
        }
    }

    #[tokio::test]
    async fn exact_project_provider_denies_other_scopes_before_transport() {
        let ai = config();
        let transport = Arc::new(CountingTransport::default());
        let provider = guarded_provider(&ai, Arc::clone(&transport)).unwrap();
        let forbidden = [
            TenantScope::new(
                uuid::Uuid::new_v4().into(),
                ai.scope.tenant_id,
                ai.scope.project_id,
            ),
            TenantScope::new(
                ai.scope.operator_id,
                uuid::Uuid::new_v4().into(),
                ai.scope.project_id,
            ),
            TenantScope::new(ai.scope.operator_id, ai.scope.tenant_id, None),
            TenantScope::new(
                ai.scope.operator_id,
                ai.scope.tenant_id,
                Some(uuid::Uuid::new_v4().into()),
            ),
        ];
        for scope in &forbidden {
            let error = provider.complete(scope, &request(None)).await.unwrap_err();
            assert_eq!(error.code, HostOpErrorCode::Denied);
            assert_eq!(transport.0.load(Ordering::SeqCst), 0);
        }
        assert!(
            provider
                .complete(&ai.scope, &request(Some("other-model")))
                .await
                .is_err()
        );
        assert_eq!(transport.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider
                .complete(&ai.scope, &request(None))
                .await
                .unwrap()
                .text,
            "answer"
        );
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);

        let mut invalid = ai;
        invalid.scope.project_id = None;
        assert!(guarded_provider(&invalid, transport).is_err());
    }

    #[test]
    fn bundle_must_exist_and_match_digest_before_runtime_is_configured() {
        let state = AppState::development();
        let transport = Arc::new(CountingTransport::default());
        let mut ai = config();
        ai.bundle_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("missing-scoped-ai-{}.mjs", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();
        ai.bundle_sha256 = "0".repeat(64);
        assert!(matches!(
            assemble_with_transport(&state, &ai, Arc::clone(&transport)),
            Err(AssemblyError::BundleRead)
        ));
        assert!(!state.content_model_available());
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("scoped-ai-{}.mjs", uuid::Uuid::new_v4()));
        let source = b"export async function main() {}";
        std::fs::write(&path, source).unwrap();
        ai.bundle_path = path.to_string_lossy().into_owned();
        assert!(matches!(
            assemble_with_transport(&state, &ai, Arc::clone(&transport)),
            Err(AssemblyError::BundleDigest)
        ));
        assert!(!state.content_model_available());
        ai.bundle_sha256 = hex::encode(Sha256::digest(source));
        assert!(
            assemble_with_transport(&state, &ai, transport)
                .unwrap()
                .is_configured()
        );
        assert!(state.content_model_available());
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires `pnpm agent:bundle`; generated ESM is intentionally not tracked"]
    async fn scoped_test_generated_bundle_runs_one_turn() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs");
        let source = std::fs::read(&path).expect("run pnpm agent:bundle first");
        let mut ai = config();
        ai.bundle_path = path.to_string_lossy().into_owned();
        ai.bundle_sha256 = hex::encode(Sha256::digest(source));
        let state = AppState::development();
        let transport = Arc::new(CountingTransport::default());
        let runtime = assemble_with_transport(&state, &ai, Arc::clone(&transport)).unwrap();
        assert!(state.content_model_available());
        let repository = state.agent_repository();
        let conversation = repository
            .create_conversation(&ai.scope, None, CreateConversation::default())
            .await
            .unwrap();
        let accepted = repository
            .append_message(
                &ai.scope,
                conversation.id,
                AppendMessage {
                    content: "question".into(),
                    attachments: Vec::new(),
                    metadata: serde_json::Value::Null,
                },
                "scoped-test-turn".into(),
                "scoped-test-request".into(),
                RuntimeCapability::available("test", None),
            )
            .await
            .unwrap();
        repository
            .begin_run(&ai.scope, accepted.run.id)
            .await
            .unwrap()
            .expect("queued run must be claimed");
        let input = repository
            .load_turn_input(&ai.scope, conversation.id, accepted.run.id)
            .await
            .unwrap();
        let answer = runtime.run_turn(&ai.scope, input).await.unwrap();
        assert_eq!(answer.content, "answer");
        assert_eq!(answer.metadata["model"], "test-model");
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
        let calls = repository
            .list_tool_calls(&ai.scope, accepted.run.id)
            .await
            .unwrap();
        assert!(
            calls
                .iter()
                .any(|call| call.tool_name == "model.complete.v1")
        );
    }
}
