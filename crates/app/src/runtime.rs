//! Opt-in local development runtime assembly. Persistent development use
//! checks the exact server-owned scope before a process-wide key is resolved.

use std::{fmt, io::Read, sync::Arc, time::Duration};

use async_trait::async_trait;
use geo_api::{
    AppState, EmbeddedAgentRuntime, ModelProviderBridge, ProjectConfiguredModelBridge,
    ProviderClientBridge, RepositoryHostOps, SharedModelProvider,
};
use geo_domain::TenantScope;
use geo_provider::{
    HttpTransport, ProviderClient, ProviderError, ResolvedToken, SecretRef, TokenCenter, Transport,
};
use geo_worker::{HostOp, HostOpError, ModelCompletion, ModelCompletionRequest};
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

/// This wrapper is attached only to the opt-in PostgreSQL development path.
/// Authorization happens before the inner bridge can resolve its secret or
/// touch the HTTP transport. A missing project in the pin permits all projects
/// within the pinned operator and tenant; a present project must match exactly.
struct PinnedDevelopmentProvider {
    scope: TenantScope,
    inner: SharedModelProvider,
}

#[async_trait]
impl ModelProviderBridge for PinnedDevelopmentProvider {
    async fn complete(
        &self,
        scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        if !self.scope.contains(scope) {
            return Err(HostOpError::denied(
                HostOp::ModelComplete,
                "development model unavailable for this scope",
            ));
        }
        self.inner.complete(scope, request).await
    }
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
        return assemble_project_configured(state);
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
    let runtime =
        assemble_with_provider(state, &ai.bundle_path, &ai.bundle_sha256, Arc::new(bridge))?;
    for usage in geo_domain::ProjectAiUsage::ALL {
        state
            .project_ai_settings()
            .with_inherited_model(usage, ai.model.clone());
    }
    Ok(runtime)
}

pub fn assemble_persistent(
    state: &AppState,
    ai: &DevelopmentAiConfig,
    scope: &TenantScope,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    assemble_persistent_with_transport(
        state,
        ai,
        scope,
        Arc::new(HttpTransport::new().map_err(|_| AssemblyError::ProviderConfiguration)?),
    )
}

fn assemble_persistent_with_transport<T: Transport + 'static>(
    state: &AppState,
    ai: &DevelopmentAiConfig,
    scope: &TenantScope,
    transport: Arc<T>,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    let provider = Arc::new(PinnedDevelopmentProvider {
        scope: scope.clone(),
        inner: Arc::new(provider_bridge(ai, transport)?),
    });
    let runtime = assemble_with_provider(state, &ai.bundle_path, &ai.bundle_sha256, provider)?;
    for usage in geo_domain::ProjectAiUsage::ALL {
        state
            .project_ai_settings()
            .with_inherited_scope(usage, scope.clone());
        state
            .project_ai_settings()
            .with_inherited_model(usage, ai.model.clone());
    }
    Ok(runtime)
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
    assemble_with_optional_provider(state, bundle_path, bundle_sha256, Some(provider))
}

fn assemble_with_optional_provider(
    state: &AppState,
    bundle_path: &str,
    bundle_sha256: &str,
    inherited: Option<SharedModelProvider>,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    let source = load_verified_bundle(bundle_path, bundle_sha256)?;
    let settings = state.project_ai_settings();
    settings.with_inherited_provider(
        geo_domain::ProjectAiUsage::WorkbenchContent,
        inherited.clone(),
        None,
    );
    settings.with_inherited_provider(
        geo_domain::ProjectAiUsage::ObservationAnalysis,
        inherited.clone(),
        None,
    );
    let provider: SharedModelProvider = Arc::new(
        ProjectConfiguredModelBridge::new(settings, inherited)
            .map_err(|_| AssemblyError::ProviderConfiguration)?,
    );
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
    Ok(Arc::new(
        EmbeddedAgentRuntime::with_bundle(bundle, BUNDLE_SPECIFIER, Arc::new(capabilities))
            .with_tool_call_repository(state.agent_repository()),
    ))
}

/// Bundle availability is independent of model credentials. A verified bundle
/// starts the runtime now so a later project-settings save can enable calls
/// without requiring the process to restart.
pub(crate) fn assemble_project_configured(
    state: &AppState,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    let path = std::env::var("GEO_PROJECT_AGENT_BUNDLE_PATH");
    let digest = std::env::var("GEO_PROJECT_AGENT_BUNDLE_SHA256");
    match (path, digest) {
        (Ok(path), Ok(digest)) => assemble_with_optional_provider(state, &path, &digest, None),
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {
            assemble_packaged_project_runtime(state)
        }
        _ => Err(AssemblyError::ProviderConfiguration),
    }
}

fn assemble_packaged_project_runtime(
    state: &AppState,
) -> Result<Arc<EmbeddedAgentRuntime>, AssemblyError> {
    // The immutable application image supplies both bundle and build-produced
    // digest. Local/source deployments can provide the explicit pair above.
    let directory = std::path::Path::new("/opt/geo/bundles");
    let manifest = directory.join("SHA256SUMS");
    if !manifest.exists() {
        return Ok(Arc::new(EmbeddedAgentRuntime::unconfigured()));
    }
    let sums = std::fs::read_to_string(manifest).map_err(|_| AssemblyError::BundleRead)?;
    let name = "memeloop-agent-loop.bundle.mjs";
    let digest = sums
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let digest = parts.next()?;
            (parts.next()? == name).then_some(digest)
        })
        .ok_or(AssemblyError::BundleDigest)?;
    assemble_with_optional_provider(state, &directory.join(name).to_string_lossy(), digest, None)
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
    injected_provider_bridge(&ai.base_url, &ai.api_key, &ai.model, transport)
}

/// Shared injected-credential construction; the caller owns its authorization
/// wrapper. In particular the scoped deployment path must check exact scope
/// before calling this bridge (which can resolve its token and send HTTP).
pub(crate) fn injected_provider<T: Transport + 'static>(
    base_url: &str,
    api_key: &str,
    model: &str,
    transport: Arc<T>,
) -> Result<SharedModelProvider, AssemblyError> {
    Ok(Arc::new(injected_provider_bridge(
        base_url, api_key, model, transport,
    )?))
}

fn injected_provider_bridge<T: Transport + 'static>(
    base_url: &str,
    api_key: &str,
    model: &str,
    transport: Arc<T>,
) -> Result<ProviderClientBridge<T, LocalTokenCenter>, AssemblyError> {
    let token =
        ResolvedToken::new(api_key.to_owned()).map_err(|_| AssemblyError::ProviderConfiguration)?;
    let client = ProviderClient::new(
        base_url,
        SecretRef::new(LOCAL_SECRET_REF).expect("fixed secret reference is valid"),
        transport,
        Arc::new(LocalTokenCenter { token }),
    )
    .map_err(|_| AssemblyError::ProviderConfiguration)?;
    let bridge = ProviderClientBridge::new(client, model.to_owned(), Duration::from_secs(60))
        .map_err(|_| AssemblyError::ProviderConfiguration)?;
    Ok(bridge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_api::ModelProviderBridge;
    use geo_domain::{
        AgentRuntime, AppendMessage, CreateConversation, RuntimeCapability, TenantScope,
    };
    use geo_provider::{RequestControl, TransportRequest, TransportResponse};
    use geo_worker::ModelCompletionRequest;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeTransport(Mutex<usize>, bool);

    #[async_trait]
    impl Transport for FakeTransport {
        async fn send(
            &self,
            request: TransportRequest,
            _control: RequestControl,
        ) -> Result<TransportResponse, ProviderError> {
            assert_eq!(request.bearer_token(), "fake-test-only-secret");
            assert_eq!(request.url, "http://127.0.0.1:1/v1/chat/completions");
            let mut calls = self.0.lock().unwrap();
            *calls += 1;
            let model_asks_for_search = self.1 && *calls == 1;
            Ok(TransportResponse {
                status: 200,
                body: (if model_asks_for_search {
                    r#"{"id":"tool-request","model":"local-model","choices":[{"message":{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"knowledge_search","arguments":"{\"query\":\"public example\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#
                } else {
                    r#"{"id":"local-request","model":"local-model","choices":[{"message":{"content":"local answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#
                })
                .into(),
            })
        }
    }

    #[test]
    fn absent_ai_configuration_keeps_runtime_unconfigured() {
        let runtime = assemble_packaged_project_runtime(&AppState::development()).unwrap();
        if std::path::Path::new("/opt/geo/bundles/SHA256SUMS").exists() {
            assert!(runtime.is_configured());
            return;
        }
        assert!(!runtime.is_configured());
    }

    #[tokio::test]
    async fn verified_bundle_without_inherited_model_accepts_later_project_configuration() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("project-runtime-{}.mjs", uuid::Uuid::new_v4()));
        let source = b"export async function main() {}";
        std::fs::write(&path, source).unwrap();
        let state = AppState::development();
        let runtime = assemble_with_optional_provider(
            &state,
            &path.to_string_lossy(),
            &hex::encode(Sha256::digest(source)),
            None,
        )
        .unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(runtime.is_configured());
        assert!(state.content_model_available());
        let scope = TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        );
        let settings = state.project_ai_settings();
        assert!(
            settings
                .resolve(&scope, geo_domain::ProjectAiUsage::WorkbenchContent)
                .await
                .unwrap()
                .is_none()
        );
        let configuration = |base_url: &str| geo_api::UpdateProjectAiSettings {
            expected_revision: 0,
            mode: geo_domain::ProjectAiMode::Custom,
            model: Some("saved-model".into()),
            base_url: Some(base_url.into()),
            api_key: Some("synthetic-project-secret".into()),
            clear_api_key: false,
            prefer_connected_account: None,
        };
        // A configured runtime does not authorize tenant-selected private
        // endpoints. This failure must not consume the settings revision.
        let rejected = settings
            .save(
                &scope,
                geo_domain::ProjectAiUsage::WorkbenchContent,
                configuration("http://127.0.0.1:1/v1"),
            )
            .await;
        assert!(
            matches!(rejected, Err(error) if error.code == geo_domain::ErrorCode::InvalidRequest)
        );
        // Saving a DNS hostname does not resolve it or invoke inference. The
        // custom transport checks its resolved addresses when a call is made.
        settings
            .save(
                &scope,
                geo_domain::ProjectAiUsage::WorkbenchContent,
                configuration("https://models.example.invalid/v1"),
            )
            .await
            .unwrap();
        assert_eq!(
            settings
                .resolve(&scope, geo_domain::ProjectAiUsage::WorkbenchContent)
                .await
                .unwrap()
                .unwrap()
                .model,
            "saved-model"
        );
        assert!(runtime.is_configured());
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
    async fn persistent_dev_provider_denies_other_scopes_before_transport() {
        let ai = DevelopmentAiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: "fake-test-only-secret".into(),
            model: "local-model".into(),
            bundle_path: String::new(),
            bundle_sha256: String::new(),
        };
        let transport = Arc::new(FakeTransport::default());
        let pinned = TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        );
        let bridge = PinnedDevelopmentProvider {
            scope: pinned.clone(),
            inner: Arc::new(provider_bridge(&ai, Arc::clone(&transport)).unwrap()),
        };
        let request = ModelCompletionRequest {
            prompt: "question".into(),
            system: None,
            model: None,
            max_output_tokens: None,
            messages: Vec::new(),
            tools: Vec::new(),
        };
        let denied = [
            TenantScope::new(
                uuid::Uuid::new_v4().into(),
                pinned.tenant_id,
                pinned.project_id,
            ),
            TenantScope::new(
                pinned.operator_id,
                uuid::Uuid::new_v4().into(),
                pinned.project_id,
            ),
            TenantScope::new(pinned.operator_id, pinned.tenant_id, None),
            TenantScope::new(
                pinned.operator_id,
                pinned.tenant_id,
                Some(uuid::Uuid::new_v4().into()),
            ),
        ];
        for scope in denied {
            let error = bridge.complete(&scope, &request).await.unwrap_err();
            assert_eq!(error.code, geo_worker::HostOpErrorCode::Denied);
            assert_eq!(*transport.0.lock().unwrap(), 0);
        }
        assert_eq!(
            bridge.complete(&pinned, &request).await.unwrap().text,
            "local answer"
        );
        assert_eq!(*transport.0.lock().unwrap(), 1);
        let tenant_wide = PinnedDevelopmentProvider {
            scope: TenantScope::new(pinned.operator_id, pinned.tenant_id, None),
            inner: Arc::new(provider_bridge(&ai, Arc::clone(&transport)).unwrap()),
        };
        tenant_wide.complete(&pinned, &request).await.unwrap();
        assert_eq!(*transport.0.lock().unwrap(), 2);
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
        let transport = Arc::new(FakeTransport(Mutex::new(0), true));
        let state = AppState::development();
        let run_scope = TenantScope::new(
            uuid::Uuid::from_u128(1).into(),
            uuid::Uuid::from_u128(2).into(),
            Some(uuid::Uuid::from_u128(3).into()),
        );
        let runtime =
            assemble_persistent_with_transport(&state, &ai, &run_scope, Arc::clone(&transport))
                .expect("generated bundle must pass size, encoding, and digest validation");
        let repository = state.agent_repository();
        let conversation = repository
            .create_conversation(&run_scope, None, CreateConversation::default())
            .await
            .expect("conversation");
        let accepted = repository
            .append_message(
                &run_scope,
                conversation.id,
                AppendMessage {
                    content: "question".into(),
                    attachments: Vec::new(),
                    metadata: serde_json::Value::Null,
                },
                "bundle-turn".into(),
                "bundle-request".into(),
                RuntimeCapability::available("test", None),
            )
            .await
            .expect("run acceptance");
        repository
            .begin_run(&run_scope, accepted.run.id)
            .await
            .expect("begin")
            .expect("queued run must be claimed before invoking any host capability");
        let input = repository
            .load_turn_input(&run_scope, conversation.id, accepted.run.id)
            .await
            .expect("repository owns run input");
        let answer = runtime
            .run_turn(&run_scope, input)
            .await
            .expect("MemeLoop bundle must report a completed answer");
        assert_eq!(answer.content, "local answer");
        assert_eq!(answer.metadata["model"], "local-model");
        assert_eq!(*transport.0.lock().unwrap(), 2);
        let entries = repository
            .list_tool_calls(&run_scope, accepted.run.id)
            .await
            .expect("real host invocations");
        assert!(
            entries
                .iter()
                .any(|entry| entry.tool_name == "model.complete.v1")
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.tool_name == "knowledge.search.v1")
        );
        assert!(entries.iter().all(|entry| {
            entry.outcome == geo_domain::ToolCallOutcome::Succeeded
                && entry.attempt_count == 1
                && entry.cost_minor.is_none()
        }));
    }
}
