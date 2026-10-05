//! The embedded JavaScript runtime as an application capability.
//!
//! The API process never executes tenant JavaScript inline: an isolate is not
//! `Send`, so it is created, driven and dropped on one worker thread.  What the
//! process holds instead is this seam — the approved bundle and the
//! [`HostOps`] implementation — and [`AgentRuntime::run_turn`] is what the run
//! executor calls to run one turn.
//!
//! That worker thread runs a current-thread runtime of its own, and the
//! capability work is handed back to the application runtime: the engine's op
//! driver only survives the former, and a connection pool only survives the
//! latter.  [`HostBridge::new`] carries the reasoning for both halves.
//!
//! Three rules are enforced here rather than documented and hoped for:
//!
//! - An unconfigured runtime reports `capability_missing`.  It never reports
//!   itself available, and the API therefore records a failed run instead of a
//!   fabricated answer.
//! - The capability an op reaches is the one the scope names.  The scope is
//!   taken from the run, and no request payload can widen it.
//! - A bundle that produces no answer produces a failed run.  The only result
//!   channel is the bundle's own completion event; a turn that returned without
//!   reporting one is an error, never an empty answer.

use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AgentRuntime, AppError, AttachmentReference, ChannelOutcomeStatus, DistributionManifest,
    DistributionTarget, DocumentManifestItemState, DocumentManifestState, ErrorCode, ImportItem,
    ImportStatus, KnowledgeRepository, RUNTIME_NOT_CONFIGURED, ReportSnapshot, RuntimeCapability,
    SourceKind, TenantScope, TurnInput, TurnReport,
};
use geo_worker::{
    ChannelDiscoverRequest, ChannelDiscoveryPage, ChannelExecutionResult, ChannelManifestPage,
    ChannelManifestReadRequest, ChannelPlanReceipt, ChannelPlanRequest,
    ChannelPublicationLookupObservation, ChannelPublicationLookupSummary,
    ChannelTargetExecuteRequest, DistributionManifestRef, DistributionReadRequest,
    DistributionResumeRequest, DistributionStartRequest, DistributionTargetRef,
    DistributionTargetsPage, DistributionTargetsReadRequest, HOST_BUNDLE, HOST_MAIN_MODULE,
    HOST_OPS_VERSION, HostBridge, HostOp, HostOpBudgets, HostOpError, HostOpErrorCode, HostOps,
    HostRuntime, ManifestCoverage, ManifestItem, ManifestKind, ManifestPage, ManifestPlanningState,
    ManifestReadRequest, MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest,
    PublishReceipt, PublishRequest, ReportGetRequest, ReportReduceRequest, TURN_COMPLETION_TOPIC,
    WorkerError,
};
use serde_json::{Value, json};

use crate::channel_tools::ChannelToolService;
use crate::provider_bridge::SharedModelProvider;
use crate::{AppState, reduce_cycle_report};

/// The bundle and capabilities one production runtime is assembled from.
///
/// The entry specifier is configuration rather than a constant so that this
/// crate never names a particular bundle's module path: swapping in the
/// approved MemeLoop bundle is a change of value, not of layering.
#[derive(Clone)]
struct Configured {
    bundle: &'static [(&'static str, &'static str)],
    entry: &'static str,
    capabilities: Arc<dyn HostOps>,
    v8_heap_limit_bytes: usize,
}

/// The wall-clock ceiling for one turn.
///
/// Deliberately an outer net rather than the primary control: every host op
/// already carries its own budget, and that is what bounds legitimate work.
/// This only has to stop a script that spins in JavaScript and never reaches an
/// op, so it sits above the op budgets rather than guessing at a "reasonable"
/// turn length and tearing down real work.
const TURN_DEADLINE: Duration = Duration::from_secs(600);

/// A Rust-hosted JavaScript runtime, or the explicit absence of one.
///
/// A runtime whose bundle or provider bridge is not approved must not accept
/// runs it cannot execute, so the absence is a value rather than a flag: there
/// is no environment variable that can turn a configured runtime on.
pub struct EmbeddedAgentRuntime {
    configured: Option<Configured>,
}

impl EmbeddedAgentRuntime {
    /// The default hard V8 heap cap for one isolated turn.
    ///
    /// This is deliberately well below a typical process memory limit: each
    /// active run owns an isolate, and host work is budgeted independently.
    pub const DEFAULT_V8_HEAP_LIMIT_BYTES: usize = 64 * 1024 * 1024;

    /// The smallest heap that has room for the approved runtime bootstrap and
    /// a useful turn.  Smaller values risk turning configuration errors into
    /// startup failures before user code can run.
    pub const MIN_V8_HEAP_LIMIT_BYTES: usize = 16 * 1024 * 1024;

    /// The explicit absence of a runtime.
    ///
    /// This is what the application assembles until an approved bundle and a
    /// provider bridge exist: every accepted run is durably failed with
    /// `capability_missing`, and no answer is fabricated.
    pub fn unconfigured() -> Self {
        Self { configured: None }
    }

    /// A runtime over the crate's reference bundle and the given capabilities.
    ///
    /// The reference bundle declares the host-op surface and does not run a
    /// turn; the embedding host calls its `main` once per turn.  An approved
    /// MemeLoop bundle replaces it via [`Self::with_bundle`].
    ///
    /// The caller vouches that the capability set is complete.  Reporting
    /// `available` is a claim the UI acts on, so a bundle or bridge that is only
    /// partly wired must be assembled as [`Self::unconfigured`] instead: a run
    /// is then refused up front rather than accepted and failed op by op.
    pub fn configured(capabilities: Arc<dyn HostOps>) -> Self {
        Self::with_bundle(HOST_BUNDLE, HOST_MAIN_MODULE, capabilities)
    }

    /// A reference-bundle runtime with an explicit, validated V8 heap cap.
    ///
    /// Use this at application assembly when a deployment needs a tighter
    /// per-turn envelope than [`Self::DEFAULT_V8_HEAP_LIMIT_BYTES`].  The cap
    /// is finite for every configured runtime; `0` and values too small to
    /// initialise the approved runtime are rejected rather than silently
    /// disabling the bound.
    pub fn configured_with_heap_limit(
        capabilities: Arc<dyn HostOps>,
        v8_heap_limit_bytes: usize,
    ) -> Result<Self, WorkerError> {
        Self::with_bundle_and_heap_limit(
            HOST_BUNDLE,
            HOST_MAIN_MODULE,
            capabilities,
            v8_heap_limit_bytes,
        )
    }

    /// A runtime over an explicit, approved bundle and its entry module.
    pub fn with_bundle(
        bundle: &'static [(&'static str, &'static str)],
        entry: &'static str,
        capabilities: Arc<dyn HostOps>,
    ) -> Self {
        Self::with_bundle_with_validated_heap_limit(
            bundle,
            entry,
            capabilities,
            Self::DEFAULT_V8_HEAP_LIMIT_BYTES,
        )
    }

    /// A runtime over an explicit, approved bundle and a validated V8 heap
    /// cap.
    ///
    /// The near-heap guard is installed for every run this constructor creates,
    /// so approaching the cap terminates the isolate and reports a recoverable
    /// failure instead of aborting the API process.
    pub fn with_bundle_and_heap_limit(
        bundle: &'static [(&'static str, &'static str)],
        entry: &'static str,
        capabilities: Arc<dyn HostOps>,
        v8_heap_limit_bytes: usize,
    ) -> Result<Self, WorkerError> {
        Self::validate_v8_heap_limit(v8_heap_limit_bytes)?;
        Ok(Self::with_bundle_with_validated_heap_limit(
            bundle,
            entry,
            capabilities,
            v8_heap_limit_bytes,
        ))
    }

    fn with_bundle_with_validated_heap_limit(
        bundle: &'static [(&'static str, &'static str)],
        entry: &'static str,
        capabilities: Arc<dyn HostOps>,
        v8_heap_limit_bytes: usize,
    ) -> Self {
        Self {
            configured: Some(Configured {
                bundle,
                entry,
                capabilities,
                v8_heap_limit_bytes,
            }),
        }
    }

    fn validate_v8_heap_limit(v8_heap_limit_bytes: usize) -> Result<(), WorkerError> {
        if v8_heap_limit_bytes < Self::MIN_V8_HEAP_LIMIT_BYTES {
            return Err(WorkerError::new(
                "configuration",
                format!(
                    "V8 heap limit must be at least {} bytes",
                    Self::MIN_V8_HEAP_LIMIT_BYTES
                ),
            ));
        }
        Ok(())
    }

    pub fn is_configured(&self) -> bool {
        self.configured.is_some()
    }

    /// The versioned op surface a configured runtime registers, so an operator
    /// can tell which capability set a run was accepted against.
    pub fn op_surface() -> Vec<String> {
        HostRuntime::op_surface()
    }

    /// Starts one isolated run authorised for `scope`, driven by the caller.
    ///
    /// Refuses an unconfigured runtime before creating an isolate.
    ///
    /// The returned isolate is `!Send` and is meant to be driven on the
    /// current-thread runtime the caller is already inside, so capability work
    /// is bound to that same runtime.  [`Self::run`] does not use this: it
    /// drives the isolate on a current-thread runtime of its own while the
    /// capabilities stay on the application's.
    pub fn start(
        &self,
        scope: &TenantScope,
        budgets: HostOpBudgets,
    ) -> Result<HostRuntime, WorkerError> {
        let Some(configured) = self.configured.as_ref() else {
            return Err(WorkerError::new("runtime", RUNTIME_NOT_CONFIGURED));
        };
        let bridge = HostBridge::new(
            Arc::clone(&configured.capabilities),
            scope.clone(),
            tokio::runtime::Handle::current(),
        )
        .with_budgets(budgets);
        Self::new_host_runtime(configured.bundle, bridge, configured.v8_heap_limit_bytes)
    }

    /// Builds a V8-capped production isolate and installs its guard before the
    /// approved bundle can evaluate or call `main`.
    ///
    /// `HostRuntime::new` only evaluates its fixed Rust-owned error bootstrap;
    /// no approved bundle module or tenant-controlled turn data has executed
    /// until this returns.  Keeping creation and guard installation in one
    /// helper prevents the direct [`Self::start`] seam and the threaded
    /// production path from drifting apart.
    fn new_host_runtime(
        bundle: &[(&str, &str)],
        bridge: HostBridge,
        v8_heap_limit_bytes: usize,
    ) -> Result<HostRuntime, WorkerError> {
        let mut runtime = HostRuntime::new(bundle, bridge, Some(v8_heap_limit_bytes))?;
        runtime.install_heap_limit_guard(Arc::new(AtomicBool::new(false)));
        Ok(runtime)
    }
}

impl Default for EmbeddedAgentRuntime {
    fn default() -> Self {
        Self::unconfigured()
    }
}

impl fmt::Debug for EmbeddedAgentRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddedAgentRuntime")
            .field("configured", &self.is_configured())
            .field("ops", &HostRuntime::op_surface())
            .finish_non_exhaustive()
    }
}

impl EmbeddedAgentRuntime {
    /// Runs one turn on its own thread and reports what the bundle produced.
    async fn run(
        &self,
        scope: &TenantScope,
        input: TurnInput,
        cancellation: Arc<AtomicBool>,
    ) -> Result<TurnReport, AppError> {
        let Some(configured) = self.configured.as_ref() else {
            return Err(AppError::capability_missing(RUNTIME_NOT_CONFIGURED));
        };
        let argument = turn_argument(&input);
        let bundle = configured.bundle;
        let entry = configured.entry;
        let capabilities = Arc::clone(&configured.capabilities);
        let v8_heap_limit_bytes = configured.v8_heap_limit_bytes;
        let run_scope = scope.clone();
        let attachments = input.attachments.clone();
        let worker_cancellation = Arc::clone(&cancellation);
        if cancellation.load(Ordering::SeqCst) {
            return Err(AppError::new(ErrorCode::Conflict, "run was cancelled"));
        }
        // The isolate is `!Send`, so it is built, driven and dropped inside one
        // blocking task — and on a current-thread runtime of its own, because
        // that is the only flavor the engine's op driver can be driven on.  The
        // capability work is moved back onto the application runtime instead,
        // so the repository pools stay where they were built.  See
        // [`HostBridge::new`] for why both halves are load-bearing.
        let application = tokio::runtime::Handle::current();
        let finished = tokio::task::spawn_blocking(move || {
            let engine = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    WorkerError::new("runtime", format!("an isolate thread was refused: {error}"))
                })?;
            engine.block_on(async {
                let bridge = HostBridge::new(capabilities, run_scope, application)
                    .with_attachments(attachments)
                    .with_cancellation(Arc::clone(&worker_cancellation))
                    .with_budgets(HostOpBudgets::default());
                let mut runtime = Self::new_host_runtime(bundle, bridge, v8_heap_limit_bytes)?;
                let isolate_handle = runtime.thread_safe_handle();
                let (stop, stopped) = std::sync::mpsc::channel();
                let watch_cancel = Arc::clone(&worker_cancellation);
                let watcher = std::thread::spawn(move || {
                    while !watch_cancel.load(Ordering::SeqCst) {
                        if !matches!(
                            stopped.recv_timeout(Duration::from_millis(5)),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        ) {
                            return;
                        }
                    }
                    isolate_handle.terminate_execution();
                });
                // The isolate can be in a synchronous JS loop, so a Tokio
                // timer alone cannot interrupt it. For suspended promises the
                // select also wakes the current-thread event loop promptly.
                let called = tokio::select! {
                    result = runtime.call_main(entry, &argument, TURN_DEADLINE) => result,
                    () = wait_for_cancel(Arc::clone(&worker_cancellation)) => {
                        Err(WorkerError::new("cancelled", "run was cancelled"))
                    }
                };
                let _ = stop.send(());
                let _ = watcher.join();
                called?;
                if worker_cancellation.load(Ordering::SeqCst) {
                    return Err(WorkerError::new("cancelled", "run was cancelled"));
                }
                Ok::<_, WorkerError>(runtime.host_state())
            })
        })
        // A detached task would leave a panicked run `running` forever, which
        // is the same defect one state later.
        .await
        .map_err(|error| {
            AppError::new(
                ErrorCode::Internal,
                format!("the turn task did not finish: {error}"),
            )
        })?;
        let state = finished.map_err(turn_failure)?;
        if cancellation.load(Ordering::SeqCst) {
            return Err(AppError::new(ErrorCode::Conflict, "run was cancelled"));
        }
        let completed = state
            .events
            .iter()
            .rev()
            .find(|event| event.topic == TURN_COMPLETION_TOPIC)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Internal,
                    format!(
                        "the turn returned without reporting `{TURN_COMPLETION_TOPIC}`, so it produced no answer"
                    ),
                )
            })?;
        let payload: Value = serde_json::from_str(&completed.payload).map_err(|error| {
            AppError::new(
                ErrorCode::Internal,
                format!("the turn's reported result is not readable: {error}"),
            )
        })?;
        let content = payload
            .get("answer")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Internal,
                    "the turn's reported result carries no answer".to_owned(),
                )
            })?
            .to_owned();
        Ok(TurnReport {
            content,
            metadata: payload,
        })
    }
}

#[async_trait]
impl AgentRuntime for EmbeddedAgentRuntime {
    async fn capability(&self) -> RuntimeCapability {
        match self.configured {
            Some(_) => RuntimeCapability::available("deno_core", Some(HOST_OPS_VERSION.to_owned())),
            None => RuntimeCapability::missing(RUNTIME_NOT_CONFIGURED),
        }
    }

    async fn run_turn(
        &self,
        scope: &TenantScope,
        input: TurnInput,
    ) -> Result<TurnReport, AppError> {
        self.run(scope, input, Arc::new(AtomicBool::new(false)))
            .await
    }

    async fn run_turn_with_cancellation(
        &self,
        scope: &TenantScope,
        input: TurnInput,
        cancellation: Arc<AtomicBool>,
    ) -> Result<TurnReport, AppError> {
        self.run(scope, input, cancellation).await
    }
}

async fn wait_for_cancel(cancellation: Arc<AtomicBool>) {
    while !cancellation.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The single argument a turn is called with.
///
/// Deliberately explicit rather than an echo of the request: this is the
/// contract with the bundle, so naming it here is what makes it testable.  A
/// later turn may carry a retrieval query distinct from the prompt, which is
/// why the two are separate now rather than one field under two names.
fn turn_argument(input: &TurnInput) -> String {
    json!({
        "prompt": input.prompt,
        "query": input.prompt,
        "run_id": input.run_id,
        "turn_id": input.turn_id,
        "conversation_id": input.conversation_id,
        "message_id": input.message_id,
        "attachments": input.attachments,
        "history": input.history,
        "history_omitted_turns": input.history_omitted_turns,
    })
    .to_string()
}

/// Maps a worker failure onto a typed application error.
///
/// A capability gap must not arrive as an opaque 500-class failure.  A host op
/// reports failure by throwing the registered `GeoHostOpError` whose message is
/// the structured error the bridge produced, so the class survives to here and
/// does not have to be inferred from prose.  Only a failure of the *call* can
/// carry one: loading and evaluating a module never reaches an op.
fn turn_failure(error: WorkerError) -> AppError {
    if error.stage == "call"
        && let Some(code) = reported_host_op_code(&error.message)
    {
        return AppError::new(error_code_for_host_op(code), error.to_string());
    }
    AppError::new(ErrorCode::Internal, error.to_string())
}

/// Recovers the declared host-op failure class from an exception's message.
///
/// The payload is located inside the message rather than assumed to be the
/// whole of it, because the engine prefixes what it throws with the exception's
/// class name.
fn reported_host_op_code(message: &str) -> Option<HostOpErrorCode> {
    let start = message.find('{')?;
    let end = message.rfind('}')?;
    let payload: Value = serde_json::from_str(message.get(start..=end)?).ok()?;
    let code = payload.get("code")?.as_str()?;
    serde_json::from_value(Value::String(code.to_owned())).ok()
}

/// The application class for a declared host-op failure.
///
/// Exhaustive on purpose: a new op failure class has to be placed here
/// deliberately rather than defaulting into whatever the wildcard happened to
/// be.
fn error_code_for_host_op(code: HostOpErrorCode) -> ErrorCode {
    match code {
        HostOpErrorCode::CapabilityMissing => ErrorCode::CapabilityMissing,
        HostOpErrorCode::Denied => ErrorCode::Forbidden,
        HostOpErrorCode::NotFound => ErrorCode::NotFound,
        HostOpErrorCode::InvalidRequest => ErrorCode::InvalidRequest,
        HostOpErrorCode::IdempotencyConflict => ErrorCode::Conflict,
        // Operational rather than a programming error: the run's own budget or
        // deadline ran out, or the bridge could not produce a trustworthy
        // result for an effect that may have happened.
        HostOpErrorCode::BudgetExceeded
        | HostOpErrorCode::DeadlineExceeded
        | HostOpErrorCode::UnknownResult => ErrorCode::DependencyUnavailable,
        // The op was cancelled under a run that is no longer running, so this
        // completion no longer applies.
        HostOpErrorCode::Cancelled => ErrorCode::Conflict,
        HostOpErrorCode::Failed | HostOpErrorCode::Internal => ErrorCode::Internal,
    }
}

/// The product capabilities behind the host ops, as this process provides them.
///
/// Only the ops the process can actually honour are implemented.  The rest
/// return a typed `capability_missing`: an empty result a loop could mistake for
/// a complete one is never an acceptable stand-in for a capability that is
/// absent.
///
/// This set is deliberately incomplete while distribution iteration, publishing
/// and measurement are unbuilt, so an assembly over it alone is not
/// [`EmbeddedAgentRuntime::configured`].
pub struct RepositoryHostOps {
    knowledge: Arc<dyn KnowledgeRepository>,
    model_provider: Option<SharedModelProvider>,
    report_service: Option<Arc<dyn ReportService>>,
    channel_service: Option<Arc<dyn ChannelToolService>>,
    content_state: Option<AppState>,
}

#[async_trait]
trait ReportService: Send + Sync {
    async fn get(
        &self,
        scope: &TenantScope,
        request: ReportGetRequest,
    ) -> Result<ReportSnapshot, AppError>;
    async fn reduce(
        &self,
        scope: &TenantScope,
        request: ReportReduceRequest,
        now: DateTime<Utc>,
    ) -> Result<ReportSnapshot, AppError>;
}

#[async_trait]
impl ReportService for AppState {
    async fn get(
        &self,
        scope: &TenantScope,
        request: ReportGetRequest,
    ) -> Result<ReportSnapshot, AppError> {
        if let Some(id) = request.report_id {
            self.report_repository().get(scope, id).await
        } else {
            let project_id = scope
                .project_id
                .ok_or_else(|| AppError::forbidden("project scope required"))?;
            self.report_repository()
                .list(scope, project_id)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| AppError::not_found("no report exists for this project"))
        }
    }

    async fn reduce(
        &self,
        scope: &TenantScope,
        request: ReportReduceRequest,
        now: DateTime<Utc>,
    ) -> Result<ReportSnapshot, AppError> {
        let cycle_id = if let Some(id) = request.cycle_id {
            id
        } else {
            let project_id = scope
                .project_id
                .ok_or_else(|| AppError::forbidden("project scope required"))?;
            self.project_repository()
                .get(scope, project_id)
                .await?
                .ok_or_else(|| AppError::not_found("project not found"))?
                .current_cycle_id
                .ok_or_else(|| AppError::not_found("current cycle not found"))?
        };
        reduce_cycle_report(self, scope, cycle_id, request.correction_of, now).await
    }
}

impl RepositoryHostOps {
    /// The ops that read product state, over the repositories the API process
    /// already holds.
    pub fn new(knowledge: Arc<dyn KnowledgeRepository>) -> Self {
        Self {
            knowledge,
            model_provider: None,
            report_service: None,
            channel_service: None,
            content_state: None,
        }
    }

    /// Attaches the provider capability while keeping its endpoint and token
    /// implementation outside worker requests and persisted state.
    pub fn with_model_provider(mut self, provider: SharedModelProvider) -> Self {
        self.model_provider = Some(provider);
        self
    }

    /// Grants report reads/reduction through the same scoped API service as
    /// HTTP. A runtime without this adapter returns `capability_missing`.
    pub fn with_report_state(mut self, state: AppState) -> Self {
        self.report_service = Some(Arc::new(state));
        self
    }

    /// Attaches project-scoped channel discovery, planning, frozen reads and
    /// one-shot target execution. The absent adapter fails explicitly.
    pub fn with_channels(mut self, state: AppState) -> Self {
        self.channel_service = Some(Arc::new(state));
        self
    }

    pub fn with_content(mut self, state: AppState) -> Self {
        self.content_state = Some(state);
        self
    }

    fn content_state(&self, op: HostOp) -> Result<&AppState, HostOpError> {
        self.content_state.as_ref().ok_or_else(|| {
            HostOpError::capability_missing(op, "content capabilities are not configured")
        })
    }
}

fn distribution_ref(manifest: DistributionManifest) -> DistributionManifestRef {
    DistributionManifestRef {
        manifest_id: manifest.manifest_id,
        cycle_id: manifest.cycle_id,
        revision: manifest.revision,
        document_manifest_id: manifest.document_manifest_id,
        content_execution_id: manifest.content_execution_id,
        content_handoff_id: manifest.content_handoff_id,
        expected_count: manifest.expected_count,
        expansion_cursor: manifest.expansion_cursor,
        complete: manifest.complete,
    }
}

fn distribution_target_ref(target: DistributionTarget) -> DistributionTargetRef {
    DistributionTargetRef {
        target_id: target.target_id,
        ordinal: target.ordinal,
        document_item_id: target.document_item_id,
        content_revision_id: target.content_revision_id,
        platform_id: target.platform_id,
        variant_id: target.variant_id,
        publication_intent_id: target.publication_intent_id,
        status: target.status,
        reason: target.reason,
        original_channel_target_id: None,
        publication_lookup: None,
    }
}

async fn distribution_lookup_summary(
    state: &AppState,
    scope: &TenantScope,
    channel_target_id: uuid::Uuid,
) -> Result<Option<ChannelPublicationLookupSummary>, AppError> {
    let view = match state
        .channel_job_repository()
        .get_target(scope, channel_target_id)
        .await
    {
        Ok(view) => view,
        // An outbox command need not have been projected into a channel job
        // yet. A missing target is not evidence of a missing publication.
        Err(error) if error.code == ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(attempt) = view.attempts.last() else {
        return Ok(None);
    };
    if !view.target.input.is_publication()
        || !attempt
            .outcome
            .as_ref()
            .is_none_or(|outcome| outcome.status == ChannelOutcomeStatus::Unknown)
    {
        return Ok(None);
    }
    let read =
        crate::publication_lookup::read_publication_lookup(state, scope, channel_target_id, None)
            .await?;
    let Some(job) = read.job else {
        return Ok(None);
    };
    Ok(Some(ChannelPublicationLookupSummary {
        query_count: u32::try_from(job.query_count)
            .map_err(|_| AppError::invalid_request("invalid lookup query count"))?,
        next_due_at: job.next_due_at,
        in_progress: job.in_progress,
        last_error_code: job.last_error_code.map(str::to_owned),
        latest_observation: read.observations.first().map(|observation| {
            ChannelPublicationLookupObservation {
                finding: observation.finding,
                observed_at: observation.observed_at,
                received_at: observation.received_at,
            }
        }),
    }))
}

async fn current_distribution_cycle(
    state: &AppState,
    scope: &TenantScope,
) -> Result<uuid::Uuid, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    state
        .project_repository()
        .get_current_cycle(scope, project_id)
        .await?
        .map(|cycle| cycle.cycle_id)
        .ok_or_else(|| AppError::not_found("current cycle not found"))
}

async fn scoped_distribution_manifest(
    state: &AppState,
    scope: &TenantScope,
    manifest_id: uuid::Uuid,
) -> Result<DistributionManifest, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let manifest = state.distribution_service().get(scope, manifest_id).await?;
    if manifest.project_id != project_id {
        return Err(AppError::not_found("distribution manifest not found"));
    }
    state
        .project_repository()
        .get_report_cycle(scope, project_id, manifest.cycle_id)
        .await?
        .ok_or_else(|| AppError::not_found("cycle not found"))?;
    Ok(manifest)
}

impl fmt::Debug for RepositoryHostOps {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryHostOps")
            .finish_non_exhaustive()
    }
}

/// Maps a domain failure onto the stable host-op vocabulary, so a loop branches
/// on the same classes it would get from any other bridge.
fn worker_error(op: HostOp, error: AppError) -> HostOpError {
    match error.code {
        ErrorCode::CapabilityMissing => HostOpError::capability_missing(op, error.message),
        ErrorCode::NotFound => HostOpError::not_found(op, error.message),
        ErrorCode::InvalidRequest => HostOpError::invalid_request(op, error.message),
        ErrorCode::Forbidden | ErrorCode::Unauthorized => HostOpError::denied(op, error.message),
        _ => HostOpError::failed(op, error.message),
    }
}

#[async_trait]
impl HostOps for RepositoryHostOps {
    async fn distribution_start(
        &self,
        scope: &TenantScope,
        request: DistributionStartRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        let op = HostOp::DistributionStart;
        let state = self.content_state(op)?;
        let cycle_id = match request.cycle_id {
            Some(id) => id,
            None => current_distribution_cycle(state, scope)
                .await
                .map_err(|error| worker_error(op, error))?,
        };
        let service = state.distribution_service();
        let frozen = service
            .freeze(scope, cycle_id)
            .await
            .map_err(|error| worker_error(op, error))?;
        service
            .resume(scope, frozen.manifest_id, 1)
            .await
            .map(distribution_ref)
            .map_err(|error| worker_error(op, error))
    }

    async fn distribution_read(
        &self,
        scope: &TenantScope,
        request: DistributionReadRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        let op = HostOp::DistributionRead;
        let state = self.content_state(op)?;
        let manifest = if let Some(manifest_id) = request.manifest_id {
            scoped_distribution_manifest(state, scope, manifest_id).await
        } else {
            let cycle_id = match request.cycle_id {
                Some(id) => id,
                None => current_distribution_cycle(state, scope)
                    .await
                    .map_err(|error| worker_error(op, error))?,
            };
            state
                .distribution_service()
                .latest_for_cycle(scope, cycle_id)
                .await
                .map_err(|error| worker_error(op, error))?
                .ok_or_else(|| AppError::not_found("distribution manifest not frozen"))
        }
        .map_err(|error| worker_error(op, error))?;
        Ok(distribution_ref(manifest))
    }

    async fn distribution_resume(
        &self,
        scope: &TenantScope,
        request: DistributionResumeRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        let op = HostOp::DistributionResume;
        let state = self.content_state(op)?;
        scoped_distribution_manifest(state, scope, request.manifest_id)
            .await
            .map_err(|error| worker_error(op, error))?;
        state
            .distribution_service()
            .resume_from(scope, request.manifest_id, 4, request.after_ordinal)
            .await
            .map(distribution_ref)
            .map_err(|error| worker_error(op, error))
    }

    async fn distribution_targets_read(
        &self,
        scope: &TenantScope,
        request: DistributionTargetsReadRequest,
    ) -> Result<DistributionTargetsPage, HostOpError> {
        let op = HostOp::DistributionTargetsRead;
        let state = self.content_state(op)?;
        scoped_distribution_manifest(state, scope, request.manifest_id)
            .await
            .map_err(|error| worker_error(op, error))?;
        let page = state
            .distribution_service()
            .targets(
                scope,
                request.manifest_id,
                request.after_ordinal,
                request.limit.unwrap_or(25) as usize,
            )
            .await
            .map_err(|error| worker_error(op, error))?;
        let service = state.distribution_service();
        let mut items = Vec::with_capacity(page.rows.len());
        for target in page.rows {
            let binding = if target.publication_intent_id.is_some() {
                service
                    .publication_target(scope, request.manifest_id, target.target_id)
                    .await
                    .map_err(|error| worker_error(op, error))?
            } else {
                None
            };
            let mut item = distribution_target_ref(target);
            if let Some(binding) = binding {
                item.original_channel_target_id = Some(binding.channel_target_id);
                item.publication_lookup =
                    distribution_lookup_summary(state, scope, binding.channel_target_id)
                        .await
                        .map_err(|error| worker_error(op, error))?;
            }
            items.push(item);
        }
        Ok(DistributionTargetsPage {
            manifest_id: page.manifest_id,
            expected_count: page.expected_count,
            items,
            next_ordinal: page.next_ordinal,
        })
    }

    async fn content_start(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentStartRequest,
    ) -> Result<geo_worker::ContentExecutionRef, HostOpError> {
        let op = HostOp::ContentStart;
        crate::content_tools::start(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }

    async fn content_execution_read(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentExecutionReadRequest,
    ) -> Result<geo_worker::ContentExecutionRef, HostOpError> {
        let op = HostOp::ContentExecutionRead;
        crate::content_tools::read(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }

    async fn content_items_read(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentItemsReadRequest,
    ) -> Result<geo_worker::ContentItemsPage, HostOpError> {
        let op = HostOp::ContentItemsRead;
        crate::content_tools::items(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }

    async fn content_prepare(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentStepRequest,
    ) -> Result<geo_worker::ContentItemRef, HostOpError> {
        let op = HostOp::ContentPrepare;
        crate::content_tools::prepare(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }

    async fn content_generate(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentStepRequest,
    ) -> Result<geo_worker::ContentItemRef, HostOpError> {
        let op = HostOp::ContentGenerate;
        crate::content_tools::generate(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }

    async fn content_check(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentStepRequest,
    ) -> Result<geo_worker::ContentItemRef, HostOpError> {
        let op = HostOp::ContentCheck;
        crate::content_tools::check(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }

    async fn content_repair(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentStepRequest,
    ) -> Result<geo_worker::ContentItemRef, HostOpError> {
        let op = HostOp::ContentRepair;
        crate::content_tools::repair(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }

    async fn content_close(
        &self,
        scope: &TenantScope,
        request: geo_worker::ContentCloseRequest,
    ) -> Result<geo_worker::ContentHandoffRef, HostOpError> {
        let op = HostOp::ContentClose;
        crate::content_tools::close(self.content_state(op)?, scope, request)
            .await
            .map_err(|error| worker_error(op, error))
    }
    async fn channel_discover(
        &self,
        scope: &TenantScope,
        request: ChannelDiscoverRequest,
    ) -> Result<ChannelDiscoveryPage, HostOpError> {
        let op = HostOp::ChannelDiscover;
        request
            .validate()
            .map_err(|reason| HostOpError::invalid_request(op, reason))?;
        let service = self.channel_service.as_ref().ok_or_else(|| {
            HostOpError::capability_missing(op, "channel service is not configured")
        })?;
        let result = service
            .discover(scope, request.clone())
            .await
            .map_err(|error| worker_error(op, error))?;
        result
            .validate_for(&request)
            .map_err(|reason| HostOpError::internal(op, reason))?;
        Ok(result)
    }

    async fn channel_plan(
        &self,
        scope: &TenantScope,
        request: ChannelPlanRequest,
    ) -> Result<ChannelPlanReceipt, HostOpError> {
        let op = HostOp::ChannelPlan;
        request
            .validate()
            .map_err(|reason| HostOpError::invalid_request(op, reason))?;
        let service = self.channel_service.as_ref().ok_or_else(|| {
            HostOpError::capability_missing(op, "channel service is not configured")
        })?;
        let result = service
            .plan(scope, request.clone())
            .await
            .map_err(|error| worker_error(op, error))?;
        result
            .validate_for(&request)
            .map_err(|reason| HostOpError::internal(op, reason))?;
        Ok(result)
    }

    async fn channel_manifest_read(
        &self,
        scope: &TenantScope,
        request: ChannelManifestReadRequest,
    ) -> Result<ChannelManifestPage, HostOpError> {
        let op = HostOp::ChannelManifestRead;
        request
            .validate()
            .map_err(|reason| HostOpError::invalid_request(op, reason))?;
        let service = self.channel_service.as_ref().ok_or_else(|| {
            HostOpError::capability_missing(op, "channel service is not configured")
        })?;
        let result = service
            .manifest_read(scope, request.clone())
            .await
            .map_err(|error| worker_error(op, error))?;
        result
            .validate_for(&request)
            .map_err(|reason| HostOpError::internal(op, reason))?;
        Ok(result)
    }

    async fn channel_target_execute(
        &self,
        scope: &TenantScope,
        request: ChannelTargetExecuteRequest,
    ) -> Result<ChannelExecutionResult, HostOpError> {
        let op = HostOp::ChannelTargetExecute;
        request
            .validate()
            .map_err(|reason| HostOpError::invalid_request(op, reason))?;
        let service = self.channel_service.as_ref().ok_or_else(|| {
            HostOpError::capability_missing(op, "channel service is not configured")
        })?;
        let result = service
            .target_execute(scope, request.clone())
            .await
            .map_err(|error| worker_error(op, error))?;
        result
            .validate_for(request.target_id)
            .map_err(|reason| HostOpError::internal(op, reason))?;
        Ok(result)
    }

    async fn report_get(
        &self,
        scope: &TenantScope,
        request: ReportGetRequest,
    ) -> Result<ReportSnapshot, HostOpError> {
        let service = self.report_service.as_ref().ok_or_else(|| {
            HostOpError::capability_missing(HostOp::ReportGet, "report service is not configured")
        })?;
        service
            .get(scope, request)
            .await
            .map_err(|error| worker_error(HostOp::ReportGet, error))
    }

    async fn report_reduce(
        &self,
        scope: &TenantScope,
        request: ReportReduceRequest,
    ) -> Result<ReportSnapshot, HostOpError> {
        let service = self.report_service.as_ref().ok_or_else(|| {
            HostOpError::capability_missing(
                HostOp::ReportReduce,
                "report service is not configured",
            )
        })?;
        service
            .reduce(scope, request, Utc::now())
            .await
            .map_err(|error| worker_error(HostOp::ReportReduce, error))
    }

    async fn knowledge_import_attachments(
        &self,
        scope: &TenantScope,
        request: geo_worker::KnowledgeImportAttachmentsRequest,
        attachments: &[AttachmentReference],
    ) -> Result<geo_worker::KnowledgeImportAttachmentsResult, HostOpError> {
        let mut items = Vec::with_capacity(request.items.len());
        for requested in request.items {
            let result = async {
                let bound = attachments
                    .iter()
                    .find(|attachment| {
                        attachment.attachment_id.as_uuid() == requested.attachment_id
                    })
                    .ok_or_else(|| {
                        AppError::new(ErrorCode::Forbidden, "attachment is not bound to this turn")
                    })?;
                let (object, filename) = self
                    .knowledge
                    .get_attachment_object(scope, requested.attachment_id)
                    .await?
                    .ok_or_else(|| AppError::not_found("attachment not found"))?;
                if bound.object_id != object.object_id.to_string()
                    || bound.filename != filename
                    || bound.object_version.as_deref()
                        != Some(object.object_version.to_string().as_str())
                    || bound.sha256.as_deref() != Some(object.sha256.as_str())
                    || bound.size_bytes != Some(object.actual_size)
                    || bound.media_type.as_deref() != Some(object.detected_media_type.as_str())
                {
                    return Err(AppError::conflict(
                        "attachment binding no longer matches the committed object",
                    ));
                }
                let response = self
                    .knowledge
                    .import_batch(
                        scope,
                        vec![ImportItem {
                            client_item_id: format!("agent-attachment:{}", requested.attachment_id),
                            kind: SourceKind::Object,
                            name: filename,
                            purpose: requested.purpose,
                            text: None,
                            url: None,
                            object_id: Some(object.object_id),
                            knowledge_release_id: None,
                        }],
                    )
                    .await?;
                response.items.into_iter().next().ok_or_else(|| {
                    AppError::new(ErrorCode::Internal, "attachment import produced no receipt")
                })
            }
            .await;
            let mut item = geo_worker::KnowledgeImportAttachmentResultItem {
                attachment_id: requested.attachment_id,
                status: ImportStatus::Failed,
                source_id: None,
                source_version_id: None,
                knowledge_release_id: None,
                error: None,
            };
            match result {
                Ok(receipt) => {
                    item.status = receipt.status;
                    item.source_id = receipt.source.map(|source| source.source_id);
                    item.source_version_id = receipt
                        .source_version
                        .map(|version| version.source_version_id);
                    item.knowledge_release_id =
                        receipt.release.map(|release| release.knowledge_release_id);
                    item.error = receipt.error;
                }
                Err(error) => item.error = Some(error),
            }
            items.push(item);
        }
        Ok(geo_worker::KnowledgeImportAttachmentsResult { items })
    }

    async fn model_complete(
        &self,
        scope: &TenantScope,
        request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let Some(provider) = self.model_provider.as_ref() else {
            return Err(HostOpError::capability_missing(
                HostOp::ModelComplete,
                "no model provider bridge is configured",
            ));
        };
        provider.complete(scope, &request).await
    }

    async fn knowledge_search(
        &self,
        scope: &TenantScope,
        request: geo_worker::KnowledgeSearchRequest,
    ) -> Result<geo_worker::KnowledgeSearchResult, HostOpError> {
        self.knowledge
            .search(scope, request)
            .await
            .map_err(|error| worker_error(HostOp::KnowledgeSearch, error))
    }

    async fn manifest_read(
        &self,
        scope: &TenantScope,
        request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        let op = HostOp::ManifestRead;
        request
            .validate()
            .map_err(|reason| HostOpError::invalid_request(op, reason))?;
        if request.kind != ManifestKind::Document {
            return Err(HostOpError::capability_missing(
                op,
                "distribution manifest iteration is not implemented yet",
            ));
        }
        // Discovery needs a Rust-owned cycle/run binding. An arbitrary JS
        // request must not select whichever project manifest happens to exist.
        let manifest_id = request.manifest_id.ok_or_else(|| {
            HostOpError::invalid_request(op, "a bound document manifest ID is required")
        })?;
        if manifest_id.is_nil() {
            return Err(HostOpError::invalid_request(
                op,
                "manifest ID must be non-zero",
            ));
        }
        let limit = request.limit.unwrap_or(100);
        if !(1..=100).contains(&limit) {
            return Err(HostOpError::invalid_request(
                op,
                "limit must be between 1 and 100",
            ));
        }
        let manifest = self
            .knowledge
            .get_document_manifest(scope, manifest_id)
            .await
            .map_err(|error| worker_error(op, error))?
            .ok_or_else(|| HostOpError::not_found(op, "document manifest not found"))?;
        if !manifest.sealed {
            return Err(HostOpError::failed(
                op,
                "document manifest is not a sealed snapshot",
            ));
        }
        if request
            .revision
            .is_some_and(|revision| revision != manifest.revision)
        {
            return Err(HostOpError::invalid_request(
                op,
                "document manifest revision does not match",
            ));
        }
        let offset = match request.cursor.as_deref() {
            Some(cursor) => {
                let (version, position, digest) = {
                    let mut parts = cursor.split('.');
                    match (parts.next(), parts.next(), parts.next(), parts.next()) {
                        (Some(version), Some(position), Some(digest), None) => {
                            (version, position, digest)
                        }
                        _ => {
                            return Err(HostOpError::invalid_request(
                                op,
                                "invalid manifest cursor",
                            ));
                        }
                    }
                };
                let offset = position
                    .parse::<usize>()
                    .map_err(|_| HostOpError::invalid_request(op, "invalid manifest cursor"))?;
                if version != "v1"
                    || offset >= manifest.items.len()
                    || digest
                        != manifest_cursor_digest(scope, manifest_id, manifest.revision, offset)
                {
                    return Err(HostOpError::invalid_request(
                        op,
                        "manifest cursor does not match the scoped snapshot",
                    ));
                }
                offset
            }
            None => 0,
        };
        let end = offset
            .saturating_add(limit as usize)
            .min(manifest.items.len());
        let items = manifest.items[offset..end]
            .iter()
            .map(|item| ManifestItem {
                branch_id: item.document_key.clone(),
                document_manifest_item_id: Some(item.document_manifest_item_id),
                planning_state: Some(match item.state {
                    DocumentManifestItemState::Planned => ManifestPlanningState::Planned,
                    DocumentManifestItemState::Blocked => ManifestPlanningState::Blocked,
                    DocumentManifestItemState::Deferred => ManifestPlanningState::Deferred,
                    DocumentManifestItemState::NotApplicable => {
                        ManifestPlanningState::NotApplicable
                    }
                }),
                block_reason: item.block_reason.clone(),
                // A planning item has no generated revision. Never manufacture
                // a UUID merely to satisfy a publishing-oriented DTO.
                document_revision_id: None,
                platform_target_id: None,
            })
            .collect();
        let next_cursor = (end < manifest.items.len()).then(|| {
            format!(
                "v1.{end}.{}",
                manifest_cursor_digest(scope, manifest_id, manifest.revision, end)
            )
        });
        Ok(ManifestPage {
            kind: ManifestKind::Document,
            manifest_id,
            revision: manifest.revision,
            state: match manifest.state {
                DocumentManifestState::AwaitingKnowledge => "awaiting_knowledge",
                DocumentManifestState::Planning => "planning",
                DocumentManifestState::Ready => "ready",
                DocumentManifestState::Closed => "closed",
            }
            .to_owned(),
            sealed: manifest.sealed,
            expected_count: manifest.expected_count,
            coverage: Some(ManifestCoverage {
                total: manifest.coverage.total,
                planned: manifest.coverage.planned,
                blocked: manifest.coverage.blocked,
                deferred: manifest.coverage.deferred,
                not_applicable: manifest.coverage.not_applicable,
            }),
            items,
            next_cursor,
        })
    }

    async fn publish_submit(
        &self,
        _scope: &TenantScope,
        _request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Publish,
            "platform publishing is not implemented yet",
        ))
    }

    async fn measure_sample(
        &self,
        _scope: &TenantScope,
        _request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Measure,
            "independent channel measurement is not implemented yet",
        ))
    }
}

fn manifest_cursor_digest(
    scope: &TenantScope,
    manifest_id: uuid::Uuid,
    revision: i32,
    offset: usize,
) -> String {
    geo_domain::sha256_hex(
        format!(
            "geo.manifest.page.v1|{}|{manifest_id}|{revision}|{offset}",
            scope.storage_key()
        )
        .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_bridge::ModelProviderBridge;
    use async_trait::async_trait;
    use geo_worker::HostOpErrorCode;
    use std::sync::Arc;

    struct FakeModelProvider;

    #[async_trait]
    impl ModelProviderBridge for FakeModelProvider {
        async fn complete(
            &self,
            _scope: &TenantScope,
            request: &ModelCompletionRequest,
        ) -> Result<ModelCompletion, HostOpError> {
            Ok(ModelCompletion {
                text: request.prompt.clone(),
                tool_calls: Vec::new(),
                model: "fake-model".into(),
                prompt_tokens: 1,
                completion_tokens: 1,
                finish_reason: "stop".into(),
            })
        }
    }

    /// The published codes are a vocabulary, not a pass-through: a repository
    /// refusal must arrive at a script as one of the classes the surface
    /// declares.
    #[test]
    fn domain_failures_map_onto_the_declared_host_op_codes() {
        let cases = [
            (
                AppError::capability_missing("no adapter"),
                HostOpErrorCode::CapabilityMissing,
            ),
            (
                AppError::not_found("no such source"),
                HostOpErrorCode::NotFound,
            ),
            (
                AppError::invalid_request("query must not be empty"),
                HostOpErrorCode::InvalidRequest,
            ),
            (AppError::forbidden("not a writer"), HostOpErrorCode::Denied),
            (
                AppError::unauthorized("no session"),
                HostOpErrorCode::Denied,
            ),
            (
                AppError::conflict("already sealed"),
                HostOpErrorCode::Failed,
            ),
            (
                AppError::not_ready("still importing"),
                HostOpErrorCode::Failed,
            ),
        ];
        for (error, expected) in cases {
            let mapped = worker_error(HostOp::KnowledgeSearch, error);
            assert_eq!(mapped.op, HostOp::KnowledgeSearch);
            assert_eq!(mapped.code, expected, "{mapped:?}");
        }
    }

    #[tokio::test]
    async fn repository_host_ops_delegates_model_completion_to_injected_bridge() {
        let ops =
            RepositoryHostOps::new(Arc::new(geo_domain::MemoryKnowledgeRepository::default()))
                .with_model_provider(Arc::new(FakeModelProvider));
        let request = ModelCompletionRequest {
            prompt: "hello".into(),
            system: None,
            model: None,
            max_output_tokens: None,
            messages: Vec::new(),
            tools: Vec::new(),
        };
        let result = ops.model_complete(&test_scope(), request).await.unwrap();
        assert_eq!(result.text, "hello");
        assert_eq!(result.model, "fake-model");
    }

    fn test_scope() -> TenantScope {
        TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        )
    }

    /// An unconfigured runtime reports the same reason the domain's own missing
    /// runtime reports, so a client cannot tell the two apart and neither can be
    /// mistaken for a working deployment.
    #[tokio::test]
    async fn an_unconfigured_runtime_matches_the_domain_default() {
        let ours = EmbeddedAgentRuntime::unconfigured().capability().await;
        let theirs = geo_domain::MissingAgentRuntime.capability().await;
        assert_eq!(ours, theirs);
        assert!(!ours.is_available());
        assert_eq!(ours.reason.as_deref(), Some(RUNTIME_NOT_CONFIGURED));
    }

    /// An unconfigured runtime cannot run a turn either, so a configuration
    /// that cannot execute work cannot accept it.
    #[tokio::test]
    async fn an_unconfigured_runtime_refuses_to_run_a_turn() {
        let scope = TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        );
        let error = EmbeddedAgentRuntime::unconfigured()
            .run_turn(&scope, turn_input())
            .await
            .expect_err("an unconfigured runtime must refuse to run a turn");
        assert_eq!(error.code, ErrorCode::CapabilityMissing);
        assert_eq!(error.message, RUNTIME_NOT_CONFIGURED);
    }

    /// The turn argument is the contract with the bundle, so it is asserted
    /// rather than left to whatever the request happened to carry.
    #[test]
    fn the_turn_argument_names_everything_a_bundle_needs() {
        let mut input = turn_input();
        let root_message_id = uuid::Uuid::new_v4().into();
        input.history = vec![
            geo_domain::TurnHistoryMessage {
                message_id: root_message_id,
                role: geo_domain::MessageRole::User,
                content: "earlier question".to_owned(),
                root_message_id,
                sequence: 1,
            },
            geo_domain::TurnHistoryMessage {
                message_id: uuid::Uuid::new_v4().into(),
                role: geo_domain::MessageRole::Assistant,
                content: "earlier answer".to_owned(),
                root_message_id,
                sequence: 2,
            },
        ];
        input.history_omitted_turns = 4;
        let argument: Value = serde_json::from_str(&turn_argument(&input)).expect("valid JSON");
        assert_eq!(argument["prompt"], "how long is the warranty?");
        assert_eq!(argument["query"], "how long is the warranty?");
        assert_eq!(argument["run_id"], input.run_id.to_string().as_str());
        assert_eq!(argument["turn_id"], input.turn_id.to_string().as_str());
        assert_eq!(
            argument["conversation_id"],
            input.conversation_id.to_string().as_str()
        );
        assert_eq!(
            argument["history"],
            serde_json::to_value(&input.history).unwrap()
        );
        assert_eq!(argument["history_omitted_turns"], 4);
        assert_eq!(argument["attachments"], json!([]));
        assert!(argument["history"][0].get("attachments").is_none());
        assert!(argument["history"][1].get("metadata").is_none());
    }

    /// A thrown host-op failure keeps its declared class instead of degrading
    /// into an opaque internal error, so an unfinished provider bridge is
    /// recorded as the capability gap it is.
    #[test]
    fn a_thrown_host_op_failure_keeps_its_declared_class() {
        let thrown = WorkerError::new(
            "call",
            r#"Uncaught GeoHostOpError: {"code":"capability_missing","op":"model_complete","message":"no model provider bridge is configured"}"#,
        );
        let mapped = turn_failure(thrown);
        assert_eq!(mapped.code, ErrorCode::CapabilityMissing);

        let denied = WorkerError::new(
            "call",
            r#"Uncaught GeoHostOpError: {"code":"denied","op":"publish_submit","message":"not permitted"}"#,
        );
        assert_eq!(turn_failure(denied).code, ErrorCode::Forbidden);

        // A failure that never reached an op cannot claim a host-op class.
        let unloadable = WorkerError::new("evaluate", "SyntaxError: unexpected token");
        assert_eq!(turn_failure(unloadable).code, ErrorCode::Internal);

        let opaque = WorkerError::new("call", "Uncaught Error: turn exploded");
        assert_eq!(turn_failure(opaque).code, ErrorCode::Internal);
    }

    fn turn_input() -> TurnInput {
        TurnInput {
            conversation_id: geo_domain::ConversationId::from(uuid::Uuid::new_v4()),
            message_id: geo_domain::MessageId::from(uuid::Uuid::new_v4()),
            turn_id: geo_domain::TurnId::from(uuid::Uuid::new_v4()),
            run_id: geo_domain::RunId::from(uuid::Uuid::new_v4()),
            prompt: "how long is the warranty?".to_owned(),
            attachments: Vec::new(),
            history: Vec::new(),
            history_omitted_turns: 0,
        }
    }
}
