//! Approved native MemeLoop first-fanout execution.
//!
//! HTTP acceptance persists the execution first, then dispatches this
//! reference-only run. An invocation may be repeated after interruption:
//! the approved script re-reads Rust-owned item states on every entry.
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
use std::{future::Future, pin::Pin};

use geo_domain::{AppError, ErrorCode, TenantScope};
use geo_worker::{
    HostBridge, HostOp, HostOpBudgets, HostOpLimits, HostOps, HostRuntime, WorkerError,
};
use uuid::Uuid;

const WORKFLOW_DEADLINE: Duration = Duration::from_secs(3600);
const V8_HEAP_LIMIT: usize = 64 * 1024 * 1024;
pub const CONTENT_WORKFLOW_ENTRY: &str = "memeloop://bundle/content-workflow.mjs";
pub const CONTENT_COMPLETION_TOPIC: &str = "content.completed";
pub const DISTRIBUTION_PREPARED_TOPIC: &str = "distribution.prepared";

fn workflow_budgets() -> HostOpBudgets {
    // Match the existing finite planner's 10,000-item bound. Interactive-turn
    // defaults must not truncate a valid background manifest halfway through.
    let mut budgets = HostOpBudgets::default()
        .with_limits(HostOp::ContentItemsRead, HostOpLimits::new(15_000, 10_000));
    for op in [
        HostOp::ContentPrepare,
        HostOp::ContentGenerate,
        HostOp::ContentCheck,
    ] {
        budgets = budgets.with_limits(op, HostOpLimits::new(120_000, 10_000));
    }
    for op in [HostOp::DistributionResume, HostOp::DistributionTargetsRead] {
        // 10,000 documents x up to 3 placements is > 256 cells. Recovery
        // revisits every target page, including a previously completed freeze.
        budgets = budgets.with_limits(op, HostOpLimits::new(120_000, 10_000));
    }
    budgets
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn background_budget_covers_every_item_of_a_supported_manifest() {
        let budgets = workflow_budgets();
        for op in [
            HostOp::ContentItemsRead,
            HostOp::ContentPrepare,
            HostOp::ContentGenerate,
            HostOp::ContentCheck,
            HostOp::DistributionResume,
            HostOp::DistributionTargetsRead,
        ] {
            assert!(budgets.limits(op).max_calls >= 10_000);
        }
    }
}

/// The application schedules this only after an execution is durably started.
/// Re-dispatch of the same reference is safe: Rust step leases and persisted
/// states arbitrate completed work rather than JS memory.
pub trait ContentWorkflowExecutor: Send + Sync {
    fn dispatch(&self, scope: TenantScope, execution_id: Uuid) -> Result<(), AppError>;

    /// Durable dispatch requires an awaited run. A fire-and-forget executor
    /// must explicitly implement this method before it can hold a DB lease.
    fn run_supervised(
        &self,
        _scope: TenantScope,
        _execution_id: Uuid,
        _cancellation: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + '_>> {
        Box::pin(async {
            Err(AppError::capability_missing(
                "supervised content workflow unavailable",
            ))
        })
    }
}

#[cfg(test)]
mod supervision_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DispatchOnly(AtomicUsize);

    impl ContentWorkflowExecutor for DispatchOnly {
        fn dispatch(&self, _scope: TenantScope, _id: Uuid) -> Result<(), AppError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn unsupervised_executor_cannot_falsely_hold_a_durable_lease() {
        let executor = DispatchOnly(AtomicUsize::new(0));
        let scope = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
        let error = executor
            .run_supervised(scope, Uuid::new_v4(), Arc::new(AtomicBool::new(false)))
            .await
            .expect_err("fire-and-forget dispatch must not report supervised success");
        assert_eq!(error.code, ErrorCode::CapabilityMissing);
        assert_eq!(executor.0.load(Ordering::SeqCst), 0);
    }
}

#[derive(Clone)]
pub struct EmbeddedContentWorkflowExecutor {
    bundle: &'static [(&'static str, &'static str)],
    entry: &'static str,
    capabilities: Arc<dyn HostOps>,
}

impl EmbeddedContentWorkflowExecutor {
    /// `bundle` must be the startup-verified, approved generated artifact.
    /// No tenant-provided source, specifier, or path is accepted here.
    pub fn with_bundle(
        bundle: &'static [(&'static str, &'static str)],
        entry: &'static str,
        capabilities: Arc<dyn HostOps>,
    ) -> Self {
        Self {
            bundle,
            entry,
            capabilities,
        }
    }

    /// Wait for a dispatched run; useful for a supervised recovery worker and
    /// integration tests. The HTTP dispatch path schedules the same operation.
    pub async fn run(&self, scope: TenantScope, execution_id: Uuid) -> Result<(), AppError> {
        self.run_with_cancellation(scope, execution_id, Arc::new(AtomicBool::new(false)))
            .await
    }

    async fn run_with_cancellation(
        &self,
        scope: TenantScope,
        execution_id: Uuid,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), AppError> {
        let bundle = self.bundle;
        let entry = self.entry;
        let capabilities = Arc::clone(&self.capabilities);
        let application = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let engine = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| runtime_error(format!("isolate runtime unavailable: {error}")))?;
            engine.block_on(async move {
                let bridge = HostBridge::new(capabilities, scope, application)
                    .with_cancellation(cancellation)
                    .with_budgets(workflow_budgets());
                let mut runtime =
                    HostRuntime::new(bundle, bridge, Some(V8_HEAP_LIMIT)).map_err(worker_error)?;
                runtime.install_heap_limit_guard(Arc::new(AtomicBool::new(false)));
                runtime
                    .call_main(
                        entry,
                        &serde_json::json!({ "execution_id": execution_id }).to_string(),
                        WORKFLOW_DEADLINE,
                    )
                    .await
                    .map_err(worker_error)?;
                let completed = runtime.host_state().events.iter().any(|event| {
                    event.topic == DISTRIBUTION_PREPARED_TOPIC
                        && serde_json::from_str::<serde_json::Value>(&event.payload)
                            .ok()
                            .and_then(|value| {
                                Some((
                                    value.get("execution_id")?.as_str()?.to_owned(),
                                    value.get("cycle_id")?.as_str()?.to_owned(),
                                    value.get("manifest_id")?.as_str()?.to_owned(),
                                    value.get("handoff_id")?.as_str()?.to_owned(),
                                    value.get("targets")?.as_u64()?,
                                ))
                            })
                            .is_some_and(|(id, cycle, manifest, handoff, _)| {
                                id == execution_id.to_string()
                                    && [cycle, manifest, handoff]
                                        .iter()
                                        .all(|id| Uuid::parse_str(id).is_ok())
                            })
                });
                if !completed {
                    return Err(runtime_error(
                        "native workflow did not prepare the distribution handoff",
                    ));
                }
                Ok(())
            })
        })
        .await
        .map_err(|error| runtime_error(format!("isolate task failed: {error}")))?
    }
}

impl ContentWorkflowExecutor for EmbeddedContentWorkflowExecutor {
    fn run_supervised(
        &self,
        scope: TenantScope,
        execution_id: Uuid,
        cancellation: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send + '_>> {
        Box::pin(self.run_with_cancellation(scope, execution_id, cancellation))
    }
    fn dispatch(&self, scope: TenantScope, execution_id: Uuid) -> Result<(), AppError> {
        let runtime = self.clone();
        tokio::runtime::Handle::try_current()
            .map_err(|_| runtime_error("content workflow requires an application runtime"))?
            .spawn(async move {
                if runtime.run(scope, execution_id).await.is_err() {
                    tracing::warn!(%execution_id, "content workflow interrupted; durable item state remains resumable");
                }
            });
        Ok(())
    }
}

fn worker_error(error: WorkerError) -> AppError {
    runtime_error(format!("native content workflow failed: {error}"))
}

fn runtime_error(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Internal, message)
}
