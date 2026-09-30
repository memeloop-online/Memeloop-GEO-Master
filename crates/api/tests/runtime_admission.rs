//! Admission is shared by directly driven and threaded isolates. A detached
//! blocking turn must continue to own its slot until its isolate is gone.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use geo_api::{EmbeddedAgentRuntime, ModelProviderBridge, RepositoryHostOps};
use geo_domain::{AgentRuntime, ErrorCode, MemoryKnowledgeRepository, TenantScope, TurnInput};
use geo_worker::{
    HOST_BUNDLE, HOST_MAIN_MODULE, HostOpBudgets, HostOpError, ModelCompletion,
    ModelCompletionRequest,
};
use tokio::sync::Notify;

fn scope() -> TenantScope {
    TenantScope::new(
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
        Some(uuid::Uuid::new_v4().into()),
    )
}

fn input() -> TurnInput {
    TurnInput {
        conversation_id: uuid::Uuid::new_v4().into(),
        turn_id: uuid::Uuid::new_v4().into(),
        run_id: uuid::Uuid::new_v4().into(),
        prompt: "test prompt".into(),
    }
}

fn runtime(
    provider: Arc<dyn ModelProviderBridge>,
    bundle: &'static [(&'static str, &'static str)],
    entry: &'static str,
) -> EmbeddedAgentRuntime {
    let ops = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()))
        .with_model_provider(provider);
    EmbeddedAgentRuntime::with_bundle_and_limits(
        bundle,
        entry,
        Arc::new(ops),
        EmbeddedAgentRuntime::MIN_V8_HEAP_LIMIT_BYTES,
        1,
    )
    .expect("valid admission settings")
}

struct DelayedProvider {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ModelProviderBridge for DelayedProvider {
    async fn complete(
        &self,
        _scope: &TenantScope,
        _request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ModelCompletion {
            text: "completed".into(),
            model: "test".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop".into(),
        })
    }
}

fn delayed() -> (Arc<DelayedProvider>, Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    (
        Arc::new(DelayedProvider {
            entered: entered.clone(),
            release: release.clone(),
        }),
        entered,
        release,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_start_and_threaded_turn_share_one_slot() {
    let (provider, _, _) = delayed();
    let runtime = runtime(provider, HOST_BUNDLE, HOST_MAIN_MODULE);
    let run_scope = scope();
    let direct = runtime.start(&run_scope, HostOpBudgets::default()).unwrap();
    assert_eq!(
        runtime
            .start(&run_scope, HostOpBudgets::default())
            .expect_err("a second direct isolate is refused")
            .stage,
        "capacity"
    );
    let error = runtime.run_turn(&run_scope, input()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    assert!(error.message.contains("capacity"));
    drop(direct);
    let next = runtime.start(&run_scope, HostOpBudgets::default()).unwrap();
    drop(next);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detached_caller_does_not_release_live_isolate_capacity() {
    let (provider, entered, release) = delayed();
    let runtime = Arc::new(runtime(provider, HOST_BUNDLE, HOST_MAIN_MODULE));
    let run_scope = scope();
    let task = {
        let runtime = runtime.clone();
        let run_scope = run_scope.clone();
        tokio::spawn(async move { runtime.run_turn(&run_scope, input()).await })
    };
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("first isolate reached the provider");
    task.abort();
    let _ = task.await;
    let error = runtime.run_turn(&run_scope, input()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::DependencyUnavailable);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(run) = runtime.start(&run_scope, HostOpBudgets::default()) {
                drop(run);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("slot is released after the detached isolate exits");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_turn_releases_capacity() {
    let (provider, _, _) = delayed();
    let runtime = runtime(provider, &[], "memeloop://bundle/missing.js");
    let run_scope = scope();
    let error = runtime.run_turn(&run_scope, input()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(runtime.start(&run_scope, HostOpBudgets::default()).is_ok());
}

#[test]
fn zero_concurrency_is_invalid_configuration() {
    let (provider, _, _) = delayed();
    let ops = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()))
        .with_model_provider(provider);
    let error = EmbeddedAgentRuntime::with_bundle_and_limits(
        HOST_BUNDLE,
        HOST_MAIN_MODULE,
        Arc::new(ops),
        EmbeddedAgentRuntime::MIN_V8_HEAP_LIMIT_BYTES,
        0,
    )
    .unwrap_err();
    assert_eq!(error.stage, "configuration");
    assert!(error.message.contains("concurrent runs"));
}
