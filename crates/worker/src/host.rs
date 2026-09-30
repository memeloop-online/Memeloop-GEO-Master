//! The production host-op surface: the complete set of capabilities a tenant
//! script may reach.
//!
//! The list below is closed.  There is deliberately no op for SQL, object
//! storage, arbitrary network access, files, processes, environment variables
//! or credentials, so a script cannot obtain those capabilities by
//! construction rather than by being policed after the fact.  Every op is
//! versioned: changing a request or response shape means adding a new version
//! instead of silently changing what a pinned script sees.
//!
//! The op bodies are thin.  They parse, delegate to [`HostOps`] — a trait the
//! API process implements over its own repositories and bridges — and encode
//! the outcome.  `geo-worker` therefore never depends on `geo-api`; the
//! dependency runs the other way, which keeps the isolate boundary a real seam
//! instead of a naming convention.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// The canonical domain vocabulary the surface speaks.  Re-exported so an
// implementation of [`HostOps`] needs one import path, and so the worker never
// grows a parallel set of types for the same concepts.
pub use geo_domain::{KnowledgeSearchRequest, KnowledgeSearchResult, TenantScope};

/// The version of the host-op surface this crate registers.
///
/// A run records the version it was accepted against, so an operator can tell
/// which script/worker pair produced a result.
pub const HOST_OPS_VERSION: &str = "geo.hostops.v1";

/// The JavaScript error class every host-op failure carries.
///
/// The message is the serialised [`HostOpError`], so a script can branch on a
/// typed `code` instead of pattern-matching a provider's prose.
pub const HOST_OP_ERROR_NAME: &str = "GeoHostOpError";

/// The script that registers [`HOST_OP_ERROR_NAME`] with the isolate.
///
/// `deno_core` turns a Rust error into a JS exception by looking the class name
/// up in its own error-class registry, so an unregistered name yields
/// `undefined` and the rejection path then fails with an unrelated `TypeError`
/// instead of the structured failure.  Registering the class before any script
/// runs is what makes a host-op failure typed all the way to the loop.
pub const HOST_OP_ERROR_BOOTSTRAP: &str = r#"
globalThis.GeoHostOpError = class GeoHostOpError extends Error {
  constructor(message) {
    super(message);
    this.name = "GeoHostOpError";
  }
};
Deno.core.registerErrorClass("GeoHostOpError", globalThis.GeoHostOpError);
"#;

/// One declared capability.  This enum is the whole surface; a name that is not
/// here has no op, and therefore no Rust body to reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOp {
    /// One model completion through the Rust-side provider bridge.
    ModelComplete,
    /// Evidence retrieval restricted to the run's frozen knowledge release.
    KnowledgeSearch,
    /// Read a page of a frozen document or distribution manifest.
    ManifestRead,
    /// Submit one document revision to one platform target.
    Publish,
    /// Take one independent AI channel measurement sample.
    Measure,
}

impl HostOp {
    /// The number of declared capabilities.
    pub const COUNT: usize = 5;

    /// Every declared capability, in budget-array order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::ModelComplete,
        Self::KnowledgeSearch,
        Self::ManifestRead,
        Self::Publish,
        Self::Measure,
    ];

    /// The JS-visible name.  The trailing version is part of the contract.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ModelComplete => "model.complete.v1",
            Self::KnowledgeSearch => "knowledge.search.v1",
            Self::ManifestRead => "manifest.read.v1",
            Self::Publish => "publish.submit.v1",
            Self::Measure => "measure.sample.v1",
        }
    }

    /// The registered isolate op that carries this capability.
    pub const fn op_name(self) -> &'static str {
        match self {
            Self::ModelComplete => "op_host_model_complete_v1",
            Self::KnowledgeSearch => "op_host_knowledge_search_v1",
            Self::ManifestRead => "op_host_manifest_read_v1",
            Self::Publish => "op_host_publish_submit_v1",
            Self::Measure => "op_host_measure_sample_v1",
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// One op's budget for a single run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOpLimits {
    /// Wall-clock budget for one invocation.
    pub timeout_ms: u64,
    /// Invocations allowed per run, counted whether or not they succeed.
    pub max_calls: u32,
}

impl HostOpLimits {
    pub const fn new(timeout_ms: u64, max_calls: u32) -> Self {
        Self {
            timeout_ms,
            max_calls,
        }
    }

    pub fn timeout(self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

/// Per-run budgets, one entry per [`HostOp`] in [`HostOp::ALL`] order.
///
/// A budget is a Rust-side decision: the isolate can exhaust it, never raise
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOpBudgets {
    limits: [HostOpLimits; HostOp::COUNT],
}

impl Default for HostOpBudgets {
    fn default() -> Self {
        Self {
            limits: [
                HostOpLimits::new(120_000, 32),
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(60_000, 16),
                HostOpLimits::new(120_000, 32),
            ],
        }
    }
}

impl HostOpBudgets {
    pub fn limits(&self, op: HostOp) -> HostOpLimits {
        self.limits[op.index()]
    }

    pub fn with_limits(mut self, op: HostOp, limits: HostOpLimits) -> Self {
        self.limits[op.index()] = limits;
        self
    }
}

/// The stable failure classes a script can branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOpErrorCode {
    /// The payload did not match the op's declared contract.
    InvalidRequest,
    /// The op exists, but the capability behind it is not configured.
    CapabilityMissing,
    /// The op is not permitted for this run.
    Denied,
    /// The addressed object does not exist in this run's scope.
    NotFound,
    /// The run's invocation budget for this op is spent.
    BudgetExceeded,
    /// The op exceeded its wall-clock budget.
    DeadlineExceeded,
    /// The run was cancelled while the op was in flight.
    Cancelled,
    /// The Rust-side provider or bridge failed.
    Failed,
    /// The external effect may have happened but no receipt was observed.
    UnknownResult,
    /// The bridge could not produce a trustworthy result.
    Internal,
}

impl HostOpErrorCode {
    /// Whether retrying the same op with the same request could plausibly
    /// succeed.  Mirrors the vocabulary the loop needs for backoff decisions.
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::BudgetExceeded | Self::DeadlineExceeded | Self::Failed | Self::UnknownResult
        )
    }
}

/// A structured host-op failure.
///
/// Scripts receive this as `GeoHostOpError` whose `message` is the serialised
/// error, so a loop can inspect `code` rather than a raw string.  Messages are
/// redacted on construction for the paths that can carry provider output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOpError {
    pub op: HostOp,
    pub code: HostOpErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl HostOpError {
    pub fn new(op: HostOp, code: HostOpErrorCode, message: impl Into<String>) -> Self {
        Self {
            op,
            code,
            message: message.into(),
            retryable: code.retryable(),
        }
    }

    pub fn invalid_request(op: HostOp, message: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::InvalidRequest, message)
    }

    /// The op exists but the capability behind it is absent.  Returning this is
    /// always preferable to returning a plausible-looking success.
    pub fn capability_missing(op: HostOp, reason: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::CapabilityMissing, reason)
    }

    pub fn denied(op: HostOp, reason: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::Denied, reason)
    }

    pub fn not_found(op: HostOp, message: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::NotFound, message)
    }

    /// A bridge or provider failure.  The message is redacted because provider
    /// errors commonly quote the credential that caused them.
    pub fn failed(op: HostOp, message: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::Failed, redact_secrets(&message.into()))
    }

    pub fn internal(op: HostOp, message: impl Into<String>) -> Self {
        Self::new(
            op,
            HostOpErrorCode::Internal,
            redact_secrets(&message.into()),
        )
    }

    pub fn unknown_result(op: HostOp, message: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::UnknownResult, message)
    }

    pub fn budget_exceeded(op: HostOp, max_calls: u32) -> Self {
        Self::new(
            op,
            HostOpErrorCode::BudgetExceeded,
            format!(
                "{} was invoked more than {max_calls} times in this run",
                op.name()
            ),
        )
    }

    pub fn deadline_exceeded(op: HostOp, timeout_ms: u64) -> Self {
        Self::new(
            op,
            HostOpErrorCode::DeadlineExceeded,
            format!("{} exceeded its {timeout_ms} ms budget", op.name()),
        )
    }

    pub fn cancelled(op: HostOp) -> Self {
        Self::new(
            op,
            HostOpErrorCode::Cancelled,
            format!("{} was cancelled", op.name()),
        )
    }

    /// Returns this error with credential-shaped substrings masked.
    ///
    /// Applied on the way to the isolate, so an implementation that builds an
    /// error around a raw provider message cannot leak it into the script.
    pub fn redacted(mut self) -> Self {
        self.message = redact_secrets(&self.message);
        self
    }
}

impl std::fmt::Display for HostOpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: {:?}: {}",
            self.op.name(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for HostOpError {}

/// Masks credential-shaped substrings so a provider message can reach the
/// isolate without carrying a key.
///
/// Credentials are never part of a host-op request, so this is defence in
/// depth: the only way a key could appear here is inside a third-party error
/// string the bridge is quoting back.
pub fn redact_secrets(message: &str) -> String {
    let mut redacted = String::with_capacity(message.len());
    for (index, token) in message.split(' ').enumerate() {
        if index > 0 {
            redacted.push(' ');
        }
        if is_credential_shaped(token) {
            redacted.push_str("***");
        } else {
            redacted.push_str(token);
        }
    }
    redacted
}

/// A conservative, opaque-token heuristic: long, dense, with no URL or path
/// punctuation.  Over-masking a diagnostic is acceptable; leaking is not.
fn is_credential_shaped(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(character, '"' | '\'' | ',' | ';' | '=' | ':' | '(' | ')')
    });
    if token.len() < 24 || token.contains('/') || token.contains('\\') {
        return false;
    }
    token.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '+' | '.')
    })
}

/// The Rust-side capabilities one run may reach.
///
/// Implementations live outside this crate (the API process implements them
/// over its repositories and provider bridge).  Two rules are part of the
/// contract:
///
/// - The scope is always passed in.  JavaScript cannot widen it, because no
///   request payload carries an operator, tenant or project selector, and the
///   request types reject unknown fields.
/// - An op the implementation does not support must return
///   [`HostOpError::capability_missing`].  Fabricating a plausible result is
///   never acceptable, and a genuinely absent capability must reach the user as
///   an explicit failure.
#[async_trait]
pub trait HostOps: Send + Sync {
    async fn model_complete(
        &self,
        scope: &TenantScope,
        request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError>;

    async fn knowledge_search(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError>;

    async fn manifest_read(
        &self,
        scope: &TenantScope,
        request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError>;

    async fn publish_submit(
        &self,
        scope: &TenantScope,
        request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError>;

    async fn measure_sample(
        &self,
        scope: &TenantScope,
        request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError>;
}

/// A model completion request.
///
/// The provider endpoint and its credential are owned by the Rust bridge and
/// cannot appear here: `model` is a routing identifier the bridge validates
/// against its configured allow-list, never a URL.  Unknown fields are refused
/// so a script cannot smuggle an extra destination into the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCompletionRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// A configured model routing identifier, not an endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCompletion {
    pub text: String,
    /// The routing identifier that actually answered, so a run can record which
    /// model produced its content.
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestKind {
    Document,
    Distribution,
}

/// Reads one page of a frozen manifest.  Pagination is explicit because the
/// two fan-out stages must never materialise a full cartesian product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestReadRequest {
    pub kind: ManifestKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestItem {
    /// The deterministic branch identity for this item.
    pub branch_id: String,
    pub document_revision_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_target_id: Option<Uuid>,
}

/// A manifest page.  `sealed` and `expected_count` are reported as they are: an
/// unsealed manifest is visible as unsealed rather than presented as an empty
/// but complete list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPage {
    pub kind: ManifestKind,
    pub manifest_id: Uuid,
    pub revision: i32,
    pub state: String,
    pub sealed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_count: Option<i64>,
    pub items: Vec<ManifestItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One document revision addressed to one platform target.
///
/// The branch identity and the idempotency key are derived by Rust from the
/// frozen manifest, so a script cannot submit the same branch twice under a new
/// key or invent a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub document_revision_id: Uuid,
    pub platform_target_id: Uuid,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishState {
    /// Accepted and durably recorded, with no external effect yet.
    Accepted,
    /// The platform returned a receipt.
    Published,
    /// The request may have reached the platform but no receipt was observed.
    /// Callers must query rather than blindly resend.
    UnknownResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishReceipt {
    pub publish_attempt_id: Uuid,
    pub state: PublishState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<Uuid>,
}

/// One measurement sample from an independent AI channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureRequest {
    pub measurement_protocol_id: Uuid,
    pub question: String,
    /// The observation surface named by the frozen protocol.
    pub channel: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasureSample {
    pub sample_id: Uuid,
    pub channel: String,
    pub answer: String,
    pub evidence_refs: Vec<Uuid>,
    pub observed_at: DateTime<Utc>,
}

/// Per-run invocation accounting.
///
/// The worker owns the counters, so a script cannot raise its own budget by
/// asking for a fresh one; the only way to observe the count is through
/// [`HostBridge::meter`].
#[derive(Debug)]
pub struct HostOpMeter {
    calls: [AtomicU32; HostOp::COUNT],
}

impl Default for HostOpMeter {
    fn default() -> Self {
        Self {
            calls: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

impl HostOpMeter {
    /// Reserves one invocation.  Attempts are counted whether or not they
    /// subsequently succeed, so a retry loop cannot spend the budget twice.
    fn reserve(&self, op: HostOp, limits: HostOpLimits) -> Result<(), HostOpError> {
        let used = self.calls[op.index()].fetch_add(1, Ordering::SeqCst);
        if used >= limits.max_calls {
            return Err(HostOpError::budget_exceeded(op, limits.max_calls));
        }
        Ok(())
    }

    /// Invocations attempted for `op` in this run.
    pub fn calls(&self, op: HostOp) -> u32 {
        self.calls[op.index()].load(Ordering::SeqCst)
    }
}

/// Everything one run is allowed to reach: the host capabilities, the
/// authorised scope, the budgets, the meter, the run's cancellation flag, and
/// the runtime the capability work is polled on.
///
/// The isolate sees this only through the ops; the script has no handle that
/// could replace the implementation or the scope.
#[derive(Clone)]
pub struct HostBridge {
    capabilities: Arc<dyn HostOps>,
    scope: TenantScope,
    budgets: HostOpBudgets,
    meter: Arc<HostOpMeter>,
    cancellation: Arc<AtomicBool>,
    executor: tokio::runtime::Handle,
}

impl HostBridge {
    /// A bridge whose capability work runs on `executor`.
    ///
    /// `executor` is a required argument rather than a default, because the two
    /// runtimes involved are not interchangeable and choosing wrongly is silent:
    ///
    /// - The **isolate** has to be driven on a *current-thread* runtime.
    ///   `deno_core`'s op driver spawns a pending op future through
    ///   `deno_unsync::tokio::spawn`, which asserts that flavor and masks the
    ///   `!Send` future as `Send` on the strength of it.  On a multi-threaded
    ///   runtime the assertion aborts the process under `debug_assertions`, and
    ///   without them it hands `Rc`-held engine state to another worker thread.
    /// - The **capabilities** have to run on the application runtime.  A tokio
    ///   I/O resource is bound to the runtime that created it, so a pooled
    ///   database connection acquired while one turn's isolate runtime was
    ///   current would be unusable under the next turn's.
    ///
    /// So a caller driving an isolate itself passes its own handle, and the API
    /// passes the application's while the isolate runs on a thread of its own.
    pub fn new(
        capabilities: Arc<dyn HostOps>,
        scope: TenantScope,
        executor: tokio::runtime::Handle,
    ) -> Self {
        Self {
            capabilities,
            scope,
            budgets: HostOpBudgets::default(),
            meter: Arc::new(HostOpMeter::default()),
            cancellation: Arc::new(AtomicBool::new(false)),
            executor,
        }
    }

    pub fn with_budgets(mut self, budgets: HostOpBudgets) -> Self {
        self.budgets = budgets;
        self
    }

    /// Shares the run's cancellation flag.  Raising it fails in-flight ops with
    /// a typed `cancelled` error and stops later ones before they start.
    pub fn with_cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// The Rust-side capability implementation behind every op.
    pub fn capabilities(&self) -> &dyn HostOps {
        self.capabilities.as_ref()
    }

    /// The operator/tenant/project scope this run was authorised for.
    pub fn scope(&self) -> &TenantScope {
        &self.scope
    }

    pub fn budgets(&self) -> HostOpBudgets {
        self.budgets
    }

    pub fn meter(&self) -> &Arc<HostOpMeter> {
        &self.meter
    }

    pub fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancellation)
    }

    /// Reserves the op's budget and runs one invocation under it.
    ///
    /// The deadline and the run's cancellation are enforced here rather than in
    /// each op body, so every declared capability gets the same guarantee.
    ///
    /// `work` receives the bridge rather than borrowing the caller's, because
    /// the invocation is moved onto [`Self::executor`] and so has to be
    /// `'static`.  Reserving still happens here, on the isolate's thread, so an
    /// exhausted budget is refused without a task ever being created.
    pub async fn invoke<T, F, W>(&self, op: HostOp, work: F) -> Result<T, HostOpError>
    where
        F: FnOnce(HostBridge) -> W + Send + 'static,
        W: Future<Output = Result<T, HostOpError>> + Send + 'static,
        T: Send + 'static,
    {
        let limits = self.budgets.limits(op);
        self.meter.reserve(op, limits)?;
        if self.cancellation.load(Ordering::SeqCst) {
            return Err(HostOpError::cancelled(op));
        }
        let bridge = self.clone();
        let cancellation = Arc::clone(&self.cancellation);
        self.executor
            .spawn(async move { under_budget(op, limits, cancellation, work(bridge)).await })
            .await
            // A capability that panics is a defect in the bridge, and the run
            // records it as a typed failure instead of unwinding out of an op,
            // which the engine cannot survive.
            .map_err(|error| {
                HostOpError::internal(op, format!("the op's work did not finish: {error}"))
            })?
    }
}

/// One invocation, under the deadline and the run's cancellation.
async fn under_budget<T, W>(
    op: HostOp,
    limits: HostOpLimits,
    cancellation: Arc<AtomicBool>,
    work: W,
) -> Result<T, HostOpError>
where
    W: Future<Output = Result<T, HostOpError>>,
{
    let deadline = tokio::time::sleep(limits.timeout());
    let cancellation = wait_for_cancellation(cancellation);
    tokio::pin!(deadline, cancellation, work);
    tokio::select! {
        biased;
        result = &mut work => result,
        () = &mut cancellation => Err(HostOpError::cancelled(op)),
        () = &mut deadline => Err(HostOpError::deadline_exceeded(op, limits.timeout_ms)),
    }
}

impl std::fmt::Debug for HostBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostBridge")
            .field("scope", &self.scope.storage_key())
            .field("budgets", &self.budgets)
            .finish_non_exhaustive()
    }
}

/// Resolves once the run's cancellation flag is raised.  Polling mirrors the
/// probe's watchdog: it is independent of whatever the isolate is doing, so it
/// also fires while an op is blocked on a provider.
async fn wait_for_cancellation(cancellation: Arc<AtomicBool>) {
    let poll = Duration::from_millis(10);
    while !cancellation.load(Ordering::SeqCst) {
        tokio::time::sleep(poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_op_has_a_distinct_name_and_slot() {
        let mut names = HostOp::ALL.map(HostOp::name).to_vec();
        let mut op_names = HostOp::ALL.map(HostOp::op_name).to_vec();
        let mut slots = HostOp::ALL.map(HostOp::index).to_vec();
        for expected in 0..HostOp::COUNT {
            assert!(slots.contains(&expected), "slot {expected} is unused");
        }
        names.sort_unstable();
        names.dedup();
        op_names.sort_unstable();
        op_names.dedup();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(names.len(), HostOp::COUNT);
        assert_eq!(op_names.len(), HostOp::COUNT);
        assert_eq!(slots.len(), HostOp::COUNT);
    }

    #[test]
    fn budgets_are_addressed_per_op() {
        let budgets =
            HostOpBudgets::default().with_limits(HostOp::KnowledgeSearch, HostOpLimits::new(25, 1));
        assert_eq!(budgets.limits(HostOp::KnowledgeSearch).timeout_ms, 25);
        assert_eq!(budgets.limits(HostOp::KnowledgeSearch).max_calls, 1);
        assert_eq!(
            budgets.limits(HostOp::ModelComplete),
            HostOpBudgets::default().limits(HostOp::ModelComplete)
        );
    }

    #[test]
    fn the_error_bootstrap_registers_the_declared_class() {
        assert!(
            HOST_OP_ERROR_BOOTSTRAP.contains(HOST_OP_ERROR_NAME),
            "the bootstrap must register {HOST_OP_ERROR_NAME}"
        );
        assert!(
            HOST_OP_ERROR_BOOTSTRAP.contains("registerErrorClass"),
            "the class must be registered with the engine, not merely declared"
        );
    }

    #[test]
    fn credential_shaped_tokens_are_redacted() {
        let redacted = redact_secrets(
            "provider rejected key sk-live-abcdefghijklmnopqrstuvwxyz0123456789 for endpoint https://token.example/v1",
        );
        assert!(
            !redacted.contains("sk-live-abcdefghijklmnopqrstuvwxyz0123456789"),
            "the key must not survive: {redacted}"
        );
        assert!(redacted.contains("***"), "unexpected redaction: {redacted}");
        assert!(
            redacted.contains("https://token.example/v1"),
            "an endpoint is not a credential: {redacted}"
        );
    }

    #[tokio::test]
    async fn the_meter_counts_attempts_against_the_op_budget() {
        let meter = HostOpMeter::default();
        let limits = HostOpLimits::new(1_000, 2);
        assert!(meter.reserve(HostOp::Publish, limits).is_ok());
        assert!(meter.reserve(HostOp::Publish, limits).is_ok());
        let error = meter
            .reserve(HostOp::Publish, limits)
            .expect_err("the third attempt exceeds a budget of two");
        assert_eq!(error.code, HostOpErrorCode::BudgetExceeded);
        assert_eq!(error.op, HostOp::Publish);
        assert_eq!(meter.calls(HostOp::Publish), 3);
        assert_eq!(meter.calls(HostOp::Measure), 0);
    }
}
