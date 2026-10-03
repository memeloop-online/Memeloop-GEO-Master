//! Approved native MemeLoop first-fanout execution.
//!
//! HTTP acceptance persists the execution first, then dispatches this
//! reference-only run. An invocation may be repeated after interruption:
//! the approved script re-reads Rust-owned item states on every entry.
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

use geo_domain::{AppError, ErrorCode, TenantScope};
use geo_worker::{
    HostBridge, HostOp, HostOpBudgets, HostOpLimits, HostOps, HostRuntime, WorkerError,
};
use uuid::Uuid;

const WORKFLOW_DEADLINE: Duration = Duration::from_secs(3600);
const V8_HEAP_LIMIT: usize = 64 * 1024 * 1024;
pub const CONTENT_WORKFLOW_ENTRY: &str = "memeloop://bundle/content-workflow.mjs";
pub const CONTENT_COMPLETION_TOPIC: &str = "content.completed";

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
                    event.topic == CONTENT_COMPLETION_TOPIC
                        && serde_json::from_str::<serde_json::Value>(&event.payload)
                            .ok()
                            .and_then(|value| {
                                value
                                    .get("execution_id")
                                    .and_then(|id| id.as_str())
                                    .map(str::to_owned)
                            })
                            .as_deref()
                            == Some(execution_id.to_string().as_str())
                });
                if !completed {
                    return Err(runtime_error(
                        "native workflow did not persist a completion handoff",
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
