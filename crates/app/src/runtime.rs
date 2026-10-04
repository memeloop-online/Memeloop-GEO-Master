//! Opt-in local development runtime assembly. A process-wide key must never
//! become a tenant-aware Token Center substitute in a durable deployment.

use std::{fmt, io::Read, sync::Arc, time::Duration};

use async_trait::async_trait;
use geo_api::{
    AppState, EmbeddedAgentRuntime, ProviderClientBridge, RepositoryHostOps, SharedModelProvider,
};
use geo_provider::{
    HttpTransport, ProviderClient, ProviderError, ResolvedToken, SecretRef, TokenCenter, Transport,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::DevelopmentAiConfig;

const BUNDLE_SPECIFIER: &str = "memeloop://bundle/memeloop-agent-loop.bundle.mjs";
const MAX_BUNDLE_BYTES: u64 = 8 * 1024 * 1024;
const LOCAL_SECRET_REF: &str = "local-development-model";

#[derive(Debug, Error)]
pub enum AssemblyError {
    #[error("development AI bundle could not be read")]
    BundleRead,
    #[error("development AI bundle exceeds the size limit")]
    BundleTooLarge,
    #[error("development AI bundle is not UTF-8")]
    BundleEncoding,
    #[error("development AI bundle SHA-256 does not match")]
    BundleDigest,
    #[error("development AI provider configuration is invalid")]
    ProviderConfiguration,
    #[error("content workflow requires a model and both approved bundle path and SHA-256")]
    ContentConfiguration,
}

struct LocalTokenCenter {
    token: ResolvedToken,
}

impl fmt::Debug for LocalTokenCenter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalTokenCenter(***)")
    }
}

#[async_trait]
impl TokenCenter for LocalTokenCenter {
    async fn resolve(&self, reference: &SecretRef) -> Result<ResolvedToken, ProviderError> {
        if reference.as_str() != LOCAL_SECRET_REF {
            return Err(ProviderError::TokenUnavailable(
                "unknown development secret reference".into(),
            ));
        }
        Ok(self.token.clone())
    }
}

pub fn assemble(
    state: &AppState,
    ai: Option<&DevelopmentAiConfig>,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    let Some(ai) = ai else {
        return Ok(Arc::new(EmbeddedAgentRuntime::unconfigured()));
    };
    assemble_with_transport(
        state,
        ai,
        Arc::new(HttpTransport::new().map_err(|_| AssemblyError::ProviderConfiguration)?),
    )
}

fn assemble_with_transport<T: Transport + 'static>(
    state: &AppState,
    ai: &DevelopmentAiConfig,
    transport: Arc<T>,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    let bridge = provider_bridge(ai, transport)?;
    assemble_with_provider(state, &ai.bundle_path, &ai.bundle_sha256, Arc::new(bridge))
}

pub(crate) fn load_verified_bundle(path: &str, sha256: &str) -> Result<String, AssemblyError> {
    // Verify a bounded byte stream before allocating the static module table.
    let mut file = std::fs::File::open(path).map_err(|_| AssemblyError::BundleRead)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_BUNDLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AssemblyError::BundleRead)?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(AssemblyError::BundleTooLarge);
    }
    let digest = Sha256::digest(&bytes);
    let expected = hex::decode(sha256).map_err(|_| AssemblyError::BundleDigest)?;
    if digest.as_slice() != expected {
        return Err(AssemblyError::BundleDigest);
    }
    String::from_utf8(bytes).map_err(|_| AssemblyError::BundleEncoding)
}

pub(crate) fn assemble_with_provider(
    state: &AppState,
    bundle_path: &str,
    bundle_sha256: &str,
    provider: SharedModelProvider,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    let source = load_verified_bundle(bundle_path, bundle_sha256)?;
    state.configure_content_model(Arc::clone(&provider));
    let capabilities = RepositoryHostOps::new(state.knowledge_repository())
        .with_model_provider(provider)
        .with_report_state(state.clone())
        .with_channels(state.clone())
        .with_content(state.clone());
    // This API currently takes a static allow-list. One startup allocation is
    // intentional for the digest-approved bundle; no unapproved imports exist.
    let source: &'static str = Box::leak(source.into_boxed_str());
    let bundle: &'static [(&'static str, &'static str)] =
        Box::leak(Box::new([(BUNDLE_SPECIFIER, source)]));
    Ok(Arc::new(EmbeddedAgentRuntime::with_bundle(
        bundle,
        BUNDLE_SPECIFIER,
        Arc::new(capabilities),
    )))
}

pub(crate) fn configure_content_workflow(state: &AppState) -> Result<(), AssemblyError> {
    let path = std::env::var("GEO_CONTENT_BUNDLE_PATH");
    let digest = std::env::var("GEO_CONTENT_BUNDLE_SHA256");
    if matches!(path, Err(std::env::VarError::NotPresent))
        && matches!(digest, Err(std::env::VarError::NotPresent))
    {
        return Ok(());
    }
    let (Ok(path), Ok(digest)) = (path, digest) else {
        return Err(AssemblyError::ContentConfiguration);
    };
    // Closed content executions can recover distribution preparation without
    // generating text. Generation/bootstrapping is gated at dispatch instead.
    assemble_content_workflow(state, &path, &digest)
}

pub(crate) fn assemble_content_workflow(
    state: &AppState,
    path: &str,
    digest: &str,
) -> Result<(), AssemblyError> {
    use geo_api::content_runtime::{CONTENT_WORKFLOW_ENTRY, EmbeddedContentWorkflowExecutor};
    let source: &'static str = Box::leak(load_verified_bundle(path, digest)?.into_boxed_str());
    let bundle: &'static [(&'static str, &'static str)] =
        Box::leak(Box::new([(CONTENT_WORKFLOW_ENTRY, source)]));
    let capabilities =
        RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone());
    state.configure_content_executor(Arc::new(EmbeddedContentWorkflowExecutor::with_bundle(
        bundle,
        CONTENT_WORKFLOW_ENTRY,
        Arc::new(capabilities),
    )));
    Ok(())
}

fn provider_bridge<T: Transport + 'static>(
    ai: &DevelopmentAiConfig,
    transport: Arc<T>,
) -> Result<ProviderClientBridge<T, LocalTokenCenter>, AssemblyError> {
    let token =
        ResolvedToken::new(ai.api_key.clone()).map_err(|_| AssemblyError::ProviderConfiguration)?;
    let client = ProviderClient::new(
        &ai.base_url,
        SecretRef::new(LOCAL_SECRET_REF).expect("fixed secret reference is valid"),
        transport,
        Arc::new(LocalTokenCenter { token }),
    )
    .map_err(|_| AssemblyError::ProviderConfiguration)?;
    let bridge = ProviderClientBridge::new(client, ai.model.clone(), Duration::from_secs(60))
        .map_err(|_| AssemblyError::ProviderConfiguration)?;
    Ok(bridge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_api::ModelProviderBridge;
    use geo_domain::{AgentRuntime, TenantScope, TurnInput};
    use geo_provider::{RequestControl, TransportRequest, TransportResponse};
    use geo_worker::ModelCompletionRequest;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeTransport(Mutex<usize>);

    #[async_trait]
    impl Transport for FakeTransport {
        async fn send(
            &self,
            request: TransportRequest,
            _control: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            assert_eq!(request.bearer_token(), "fake-test-only-secret");
            assert_eq!(request.url, "http://127.0.0.1:1/v1/chat/completions");
            *self.0.lock().unwrap() += 1;
            Ok(TransportResponse {
                status: 200,
                body: r#"{"id":"local-request","model":"local-model","choices":[{"message":{"content":"local answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#.into(),
            })
        }
    }

    #[test]
    fn absent_ai_configuration_keeps_runtime_unconfigured() {
        let runtime = assemble(&AppState::development(), None).unwrap();
        assert!(!runtime.is_configured());
    }

    #[test]
    fn verified_bundle_assembles_without_network_or_exposing_key() {
        let path = std::env::temp_dir().join(format!("geo-test-bundle-{}.mjs", std::process::id()));
        let source = b"export async function main() {}";
        std::fs::write(&path, source).unwrap();
        let mut ai = DevelopmentAiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "fake-test-only-secret".into(),
            model: "local-model".into(),
            bundle_path: path.to_string_lossy().into_owned(),
            bundle_sha256: hex::encode(Sha256::digest(source)),
        };
        let state = AppState::development();
        let transport = Arc::new(FakeTransport::default());
        let assembled = assemble_with_transport(&state, &ai, Arc::clone(&transport)).unwrap();
        assert!(assembled.is_configured());
        assert_eq!(*transport.0.lock().unwrap(), 0);
        ai.bundle_sha256 = "0".repeat(64);
        assert!(matches!(
            assemble_with_transport(&state, &ai, transport),
            Err(AssemblyError::BundleDigest)
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn development_bridge_uses_fake_transport_and_single_model_allowlist() {
        let ai = DevelopmentAiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "fake-test-only-secret".into(),
            model: "local-model".into(),
            bundle_path: String::new(),
            bundle_sha256: String::new(),
        };
        let transport = Arc::new(FakeTransport::default());
        let bridge = provider_bridge(&ai, Arc::clone(&transport)).unwrap();
        let scope = TenantScope::new(
            uuid::Uuid::from_u128(1).into(),
            uuid::Uuid::from_u128(2).into(),
            None,
        );
        let mut request = ModelCompletionRequest {
            prompt: "question".into(),
            system: None,
            model: Some("other-model".into()),
            max_output_tokens: Some(32),
            messages: Vec::new(),
            tools: Vec::new(),
        };
        assert!(bridge.complete(&scope, &request).await.is_err());
        assert_eq!(*transport.0.lock().unwrap(), 0);
        request.model = None;
        let answer = bridge.complete(&scope, &request).await.unwrap();
        assert_eq!(answer.text, "local answer");
        assert_eq!(*transport.0.lock().unwrap(), 1);
    }

    #[tokio::test]
    #[ignore = "requires `pnpm agent:bundle`; generated ESM is intentionally not tracked"]
    async fn generated_bundle_runs_one_turn_through_assembled_provider() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs");
        let source = std::fs::read(&path).expect("run pnpm agent:bundle first");
        let ai = DevelopmentAiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "fake-test-only-secret".into(),
            model: "local-model".into(),
            bundle_path: path.to_string_lossy().into_owned(),
            bundle_sha256: hex::encode(Sha256::digest(source)),
        };
        let transport = Arc::new(FakeTransport::default());
        let runtime =
            assemble_with_transport(&AppState::development(), &ai, Arc::clone(&transport))
                .expect("generated bundle must pass size, encoding, and digest validation");
        let answer = runtime
            .run_turn(
                &TenantScope::new(
                    uuid::Uuid::from_u128(1).into(),
                    uuid::Uuid::from_u128(2).into(),
                    None,
                ),
                TurnInput {
                    conversation_id: uuid::Uuid::new_v4().into(),
                    message_id: uuid::Uuid::new_v4().into(),
                    turn_id: uuid::Uuid::new_v4().into(),
                    run_id: uuid::Uuid::new_v4().into(),
                    prompt: "question".into(),
                    attachments: Vec::new(),
                },
            )
            .await
            .expect("MemeLoop bundle must report a completed answer");
        assert_eq!(answer.content, "local answer");
        assert_eq!(answer.metadata["model"], "local-model");
        assert_eq!(*transport.0.lock().unwrap(), 1);
    }
}
