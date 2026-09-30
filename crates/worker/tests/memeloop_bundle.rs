//! Compatibility probe for the generated, self-contained MemeLoop ESM bundle.
//!
//! The artifact is deliberately generated rather than checked in. Run this
//! test explicitly after `pnpm agent:bundle`; its ignored status makes a
//! missing local artifact visible instead of treating the probe as a passing
//! no-op in ordinary Rust test runs.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use geo_domain::{KnowledgeSearchRequest, KnowledgeSearchResult, TenantScope};
use geo_worker::{
    HostBridge, HostOp, HostOpError, HostOps, HostRuntime, ManifestPage, ManifestReadRequest,
    MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest, PublishReceipt,
    PublishRequest,
};

const BUNDLE_SPECIFIER: &str = "memeloop://bundle/memeloop-agent-loop.bundle.mjs";
const TURN_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct RecordingHostOps {
    model_calls: Mutex<Vec<(String, ModelCompletionRequest)>>,
}

impl RecordingHostOps {
    fn model_calls(&self) -> Vec<(String, ModelCompletionRequest)> {
        self.model_calls
            .lock()
            .expect("model call recorder must not be poisoned")
            .clone()
    }

    fn unavailable(op: HostOp) -> HostOpError {
        HostOpError::capability_missing(
            op,
            "this compatibility probe only exposes a model completion provider",
        )
    }
}

#[async_trait]
impl HostOps for RecordingHostOps {
    async fn model_complete(
        &self,
        scope: &TenantScope,
        request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.model_calls
            .lock()
            .expect("model call recorder must not be poisoned")
            .push((scope.storage_key(), request));
        Ok(ModelCompletion {
            text: "The warranty lasts two years.".to_owned(),
            model: "probe-model".to_owned(),
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "stop".to_owned(),
        })
    }

    async fn knowledge_search(
        &self,
        _scope: &TenantScope,
        _request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError> {
        Err(Self::unavailable(HostOp::KnowledgeSearch))
    }

    async fn manifest_read(
        &self,
        _scope: &TenantScope,
        _request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        Err(Self::unavailable(HostOp::ManifestRead))
    }

    async fn publish_submit(
        &self,
        _scope: &TenantScope,
        _request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        Err(Self::unavailable(HostOp::Publish))
    }

    async fn measure_sample(
        &self,
        _scope: &TenantScope,
        _request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        Err(Self::unavailable(HostOp::Measure))
    }
}

fn generated_bundle_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs")
}

fn generated_bundle() -> String {
    let path = generated_bundle_path();
    fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "the generated MemeLoop ESM bundle is required for this probe ({path:?}): {error}. \
             Run `pnpm agent:bundle` from the repository root, then rerun \
             `cargo test -p geo-worker --test memeloop_bundle -- --ignored`."
        )
    })
}

fn test_scope() -> TenantScope {
    TenantScope::new(
        uuid::Uuid::from_u128(1).into(),
        uuid::Uuid::from_u128(2).into(),
        Some(uuid::Uuid::from_u128(3).into()),
    )
}

/// Runs the actual generated MemeLoop loop through the production host-op
/// surface. The current-thread runtime is a hard requirement of deno_core's
/// async-op driver; changing this flavor can otherwise be unsound.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test memeloop_bundle -- --ignored`"]
async fn generated_memeloop_bundle_runs_a_turn_through_the_rust_host() {
    let source = generated_bundle();
    let bundle = [(BUNDLE_SPECIFIER, source.as_str())];
    let provider = Arc::new(RecordingHostOps::default());
    let scope = test_scope();
    let expected_scope = scope.storage_key();
    let bridge = HostBridge::new(
        Arc::clone(&provider) as Arc<dyn HostOps>,
        scope,
        tokio::runtime::Handle::current(),
    );
    let mut runtime = HostRuntime::new(&bundle, bridge, Some(64 * 1024 * 1024))
        .expect("the generated bundle must construct within the production heap budget");
    runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));

    assert_eq!(
        runtime.allowlisted_specifiers(),
        vec![BUNDLE_SPECIFIER.to_owned()],
        "the real bundle must be served only from the in-memory allow-list"
    );

    let unknown_import = runtime
        .evaluate_module("memeloop://bundle/not-approved.mjs", TURN_DEADLINE)
        .await
        .expect_err("an import outside the generated bundle must be denied");
    assert_eq!(unknown_import.stage, "load");
    assert!(
        unknown_import
            .message
            .contains("not part of the approved bundle"),
        "unexpected unknown-import error: {unknown_import:?}"
    );

    runtime
        .call_main(
            BUNDLE_SPECIFIER,
            &serde_json::json!({
                "conversation_id": "conversation-bundle-probe-0001",
                "prompt": "How long is the warranty?",
                "run_id": "run-bundle-probe-0001",
                "timestamp": 1_700_000_000_000_u64,
                "turn_id": "turn-bundle-probe-0001",
            })
            .to_string(),
            TURN_DEADLINE,
        )
        .await
        .expect("the generated MemeLoop bundle must complete its turn");

    let model_calls = provider.model_calls();
    assert_eq!(model_calls.len(), 1, "the loop must make one model call");
    let (call_scope, request) = model_calls
        .first()
        .expect("the checked model call must remain available");
    assert_eq!(call_scope, &expected_scope);
    assert!(
        request.prompt.contains("user: How long is the warranty?"),
        "the model bridge must receive the user message: {:?}",
        request
    );

    let state = runtime.host_state();
    let completed = state
        .events
        .iter()
        .find(|event| event.topic == "loop.completed")
        .expect("the real MemeLoop loop must emit its completion");
    let completion: serde_json::Value =
        serde_json::from_str(&completed.payload).expect("completion must be valid JSON");
    assert_eq!(completion["answer"], "The warranty lasts two years.");
    assert_eq!(
        completion["conversation_id"],
        "conversation-bundle-probe-0001"
    );
    assert_eq!(completion["model"], "probe-model");
    assert_eq!(completion["run_id"], "run-bundle-probe-0001");
    assert_eq!(completion["turn_id"], "turn-bundle-probe-0001");
    assert_eq!(
        state
            .events
            .iter()
            .filter(|event| event.topic == "loop.completed")
            .count(),
        1,
        "a real turn must report exactly one completion"
    );
}
