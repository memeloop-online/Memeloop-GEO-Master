//! Approved native MemeLoop first-fanout execution.
//!
//! HTTP acceptance persists the execution first, then dispatches this
//! reference-only run. An invocation may be repeated after interruption:
//! the approved script re-reads Rust-owned item states on every entry.
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
use std::{future::Future, pin::Pin};

use geo_domain::{
    AppError, ContentExecutionOrigin, ContentExecutionStatus, ErrorCode, TenantScope,
};
use geo_worker::{
    ContentExecutionReadRequest, HostBridge, HostOp, HostOpBudgets, HostOpLimits, HostOps,
    HostRuntime, WorkerError,
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
        .with_limits(HostOp::ContentItemsRead, HostOpLimits::new(15_000, 10_000))
        .with_limits(
            HostOp::ContentSourceRead,
            HostOpLimits::new(15_000, 320_000),
        )
        .with_limits(HostOp::ModelComplete, HostOpLimits::new(120_000, 320_000));
    for op in [
        HostOp::ContentPrepare,
        HostOp::ContentGenerateBegin,
        HostOp::ContentGenerateFinish,
        HostOp::ContentCheckBegin,
        HostOp::ContentCheckFinish,
        HostOp::ContentRepairBegin,
        HostOp::ContentRepairFinish,
    ] {
        budgets = budgets.with_limits(
            op,
            HostOpLimits::new(
                120_000,
                match op {
                    HostOp::ContentCheckBegin | HostOp::ContentCheckFinish => 30_000,
                    HostOp::ContentRepairBegin | HostOp::ContentRepairFinish => 20_000,
                    _ => 10_000,
                },
            ),
        );
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
            HostOp::ContentSourceRead,
            HostOp::ModelComplete,
            HostOp::ContentPrepare,
            HostOp::ContentGenerateBegin,
            HostOp::ContentGenerateFinish,
            HostOp::ContentCheckBegin,
            HostOp::ContentCheckFinish,
            HostOp::ContentRepairBegin,
            HostOp::ContentRepairFinish,
            HostOp::DistributionResume,
            HostOp::DistributionTargetsRead,
        ] {
            assert!(budgets.limits(op).max_calls >= 10_000);
        }
        assert_eq!(budgets.limits(HostOp::ContentCheckBegin).max_calls, 30_000);
        assert_eq!(budgets.limits(HostOp::ContentRepairBegin).max_calls, 20_000);
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
        let read_scope = scope.clone();
        let application = tokio::runtime::Handle::current();
        let events = tokio::task::spawn_blocking(move || {
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
                Ok::<_, AppError>(runtime.host_state().events)
            })
        })
        .await
        .map_err(|error| runtime_error(format!("isolate task failed: {error}")))??;
        let execution = self
            .capabilities
            .content_execution_read(&read_scope, ContentExecutionReadRequest { execution_id })
            .await
            .map_err(|error| runtime_error(error.to_string()))?;
        let Some(handoff_id) = execution.handoff_id else {
            return Err(runtime_error("content workflow has no persisted handoff"));
        };
        if execution.execution_id != execution_id
            || execution.status != ContentExecutionStatus::Closed
        {
            return Err(runtime_error(
                "content workflow has not closed its execution",
            ));
        }
        let completed = events.iter().any(|event| {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.payload) else {
                return false;
            };
            let event_id = |key: &str| value.get(key)?.as_str()?.parse::<Uuid>().ok();
            if event_id("execution_id") != Some(execution_id)
                || event_id("handoff_id") != Some(handoff_id)
            {
                return false;
            }
            match &execution.origin {
                ContentExecutionOrigin::OnDemand { .. } => {
                    event.topic == CONTENT_COMPLETION_TOPIC
                        && value.get("total").and_then(|v| v.as_u64())
                            == Some(execution.coverage.total)
                }
                ContentExecutionOrigin::Cycle { cycle_id } => {
                    event.topic == DISTRIBUTION_PREPARED_TOPIC
                        && event_id("cycle_id") == Some(*cycle_id)
                        && value
                            .get("manifest_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|id| Uuid::parse_str(id).is_ok())
                        && value.get("targets").and_then(|v| v.as_u64()).is_some()
                }
            }
        });
        if !completed {
            return Err(runtime_error(
                "native workflow did not complete its persisted handoff",
            ));
        }
        Ok(())
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

#[cfg(test)]
mod completion_tests {
    use super::*;
    use crate::{
        AppState, EventBus, MemoryIdempotencyStore, MemoryOperationStore, RepositoryHostOps,
    };
    use geo_domain::{
        ContentCreateDocument, ContentCreateRequest, ContentItemStatus, ImportItem,
        KnowledgePurpose, KnowledgeRepository, MemoryAuthRepository, MemoryKnowledgeRepository,
        MemoryProjectRepository, ProjectCreate, ProjectRepository, ProjectSettings, SourceKind,
    };

    const CLOSED: &[(&str, &str)] = &[(
        CONTENT_WORKFLOW_ENTRY,
        r#"
        export async function main(input) {
            const handoff = JSON.parse(await Deno.core.ops.op_host_content_close_v1(JSON.stringify(input)));
            Deno.core.ops.op_host_emit('content.completed', JSON.stringify(handoff));
        }
    "#,
    )];
    const NO_HANDOFF: &[(&str, &str)] = &[(
        CONTENT_WORKFLOW_ENTRY,
        r#"
        export async function main(input) {
            Deno.core.ops.op_host_emit('content.completed', JSON.stringify({
                execution_id: input.execution_id,
                handoff_id: '00000000-0000-4000-8000-000000000001', total: 1
            }));
        }
    "#,
    )];
    const WRONG_HANDOFF: &[(&str, &str)] = &[(
        CONTENT_WORKFLOW_ENTRY,
        r#"
        export async function main(input) {
            const handoff = JSON.parse(await Deno.core.ops.op_host_content_close_v1(JSON.stringify(input)));
            handoff.handoff_id = '00000000-0000-4000-8000-000000000001';
            Deno.core.ops.op_host_emit('content.completed', JSON.stringify(handoff));
        }
    "#,
    )];
    const NO_EVENT: &[(&str, &str)] = &[(
        CONTENT_WORKFLOW_ENTRY,
        r#"
        export async function main(input) {
            await Deno.core.ops.op_host_content_close_v1(JSON.stringify(input));
        }
    "#,
    )];

    async fn on_demand_fixture() -> (AppState, TenantScope, Uuid) {
        let tenant = TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None);
        let projects = Arc::new(MemoryProjectRepository::default());
        let project = projects
            .create(
                &tenant,
                ProjectCreate {
                    slug: None,
                    display_name: "Runtime content".into(),
                    settings: ProjectSettings::default(),
                },
            )
            .await
            .unwrap();
        let scope = TenantScope {
            project_id: Some(project.id),
            ..tenant
        };
        let knowledge = Arc::new(MemoryKnowledgeRepository::default());
        let imported = knowledge
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "runtime-source".into(),
                    kind: SourceKind::Text,
                    name: "Runtime source".into(),
                    purpose: KnowledgePurpose::Public,
                    text: Some("A documented fact".into()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap();
        let state = AppState::with_stores_and_auth_and_projects_and_knowledge(
            Arc::new(MemoryOperationStore::default()),
            Arc::new(MemoryIdempotencyStore::default()),
            Arc::new(MemoryAuthRepository::development_with_password("unused")),
            projects,
            knowledge,
            EventBus::default(),
            false,
        );
        let execution = state
            .content_service()
            .create_on_demand(
                &scope,
                ContentCreateRequest {
                    request_id: Uuid::new_v4(),
                    topic: "A documented topic".into(),
                    source_version_ids: vec![
                        imported.items[0]
                            .source_version
                            .as_ref()
                            .unwrap()
                            .source_version_id,
                    ],
                    documents: vec![ContentCreateDocument {
                        key: "article".into(),
                        content_type: "article".into(),
                        title: None,
                        instructions: None,
                    }],
                    language: "en".into(),
                    market: None,
                    audience: None,
                },
            )
            .await
            .unwrap();
        // A classified branch is a legitimate terminal result. Completion
        // does not imply that every requested document became a ready asset.
        let repository = state.content_service().repository();
        let item = repository
            .list_items(&scope, execution.execution_id)
            .await
            .unwrap()
            .remove(0);
        repository
            .classify(
                &scope,
                execution.execution_id,
                item.item_id,
                ContentItemStatus::Blocked,
                "The selected source does not support this requested document",
            )
            .await
            .unwrap();
        (state, scope, execution.execution_id)
    }

    #[tokio::test]
    async fn on_demand_runtime_requires_the_persisted_closed_handoff() {
        let (state, scope, execution_id) = on_demand_fixture().await;
        let capabilities = Arc::new(
            RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone()),
        );
        EmbeddedContentWorkflowExecutor::with_bundle(CLOSED, CONTENT_WORKFLOW_ENTRY, capabilities)
            .run(scope.clone(), execution_id)
            .await
            .unwrap();
        let execution = state
            .content_service()
            .repository()
            .get_execution(&scope, execution_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(execution.status, ContentExecutionStatus::Closed);
        assert!(execution.handoff_id.is_some());
        assert!(matches!(
            execution.origin,
            ContentExecutionOrigin::OnDemand { .. }
        ));
        assert_eq!(execution.coverage.blocked, 1);
    }

    #[tokio::test]
    async fn on_demand_completion_event_cannot_replace_a_missing_persisted_handoff() {
        let (state, scope, execution_id) = on_demand_fixture().await;
        let capabilities = Arc::new(
            RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone()),
        );
        let error = EmbeddedContentWorkflowExecutor::with_bundle(
            NO_HANDOFF,
            CONTENT_WORKFLOW_ENTRY,
            capabilities,
        )
        .run(scope.clone(), execution_id)
        .await
        .unwrap_err();
        assert!(error.message.contains("no persisted handoff"));
        let execution = state
            .content_service()
            .repository()
            .get_execution(&scope, execution_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(execution.status, ContentExecutionStatus::Running);
        assert_eq!(execution.handoff_id, None);
    }

    #[tokio::test]
    async fn on_demand_completion_event_must_match_the_persisted_handoff() {
        let (state, scope, execution_id) = on_demand_fixture().await;
        let capabilities = Arc::new(
            RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone()),
        );
        let error = EmbeddedContentWorkflowExecutor::with_bundle(
            WRONG_HANDOFF,
            CONTENT_WORKFLOW_ENTRY,
            capabilities,
        )
        .run(scope.clone(), execution_id)
        .await
        .unwrap_err();
        assert!(
            error
                .message
                .contains("did not complete its persisted handoff")
        );
        assert_eq!(
            state
                .content_service()
                .repository()
                .get_execution(&scope, execution_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            ContentExecutionStatus::Closed
        );
    }

    #[tokio::test]
    async fn on_demand_persisted_handoff_alone_does_not_complete_the_runtime() {
        let (state, scope, execution_id) = on_demand_fixture().await;
        let capabilities = Arc::new(
            RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone()),
        );
        let error = EmbeddedContentWorkflowExecutor::with_bundle(
            NO_EVENT,
            CONTENT_WORKFLOW_ENTRY,
            capabilities,
        )
        .run(scope, execution_id)
        .await
        .unwrap_err();
        assert!(
            error
                .message
                .contains("did not complete its persisted handoff")
        );
    }
}
