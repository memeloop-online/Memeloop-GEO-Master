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

use std::collections::HashSet;
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
use geo_domain::{
    AppError, AttachmentReference, ContentCoverage, ContentExecutionStatus, ContentItemStatus,
    DistributionTargetStatus, ImportStatus, KnowledgePurpose, PublicationLookupFinding,
    ReportSnapshot,
};
pub use geo_domain::{KnowledgeSearchRequest, KnowledgeSearchResult, TenantScope};

/// The version of the host-op surface this crate registers.
///
/// A run records the version it was accepted against, so an operator can tell
/// which script/worker pair produced a result.
pub const HOST_OPS_VERSION: &str = "geo.hostops.v7";

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
    /// Explicitly import attachments bound to this run into project knowledge.
    KnowledgeImportAttachments,
    /// Read a page of a frozen document or distribution manifest.
    ManifestRead,
    /// Submit one document revision to one platform target.
    Publish,
    /// Take one independent AI channel measurement sample.
    Measure,
    /// Read an immutable scoped report snapshot.
    ReportGet,
    /// Reduce a due cycle from server-owned evidence.
    ReportReduce,
    ChannelDiscover,
    ChannelPlan,
    ChannelManifestRead,
    ChannelTargetExecute,
    ContentItemsRead,
    ContentPrepare,
    ContentGenerate,
    ContentCheck,
    ContentRepair,
    ContentClose,
    ContentStart,
    ContentExecutionRead,
    DistributionStart,
    DistributionRead,
    DistributionResume,
    DistributionTargetsRead,
}

impl HostOp {
    /// The number of declared capabilities.
    pub const COUNT: usize = 24;

    /// Every declared capability, in budget-array order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::ModelComplete,
        Self::KnowledgeSearch,
        Self::KnowledgeImportAttachments,
        Self::ManifestRead,
        Self::Publish,
        Self::Measure,
        Self::ReportGet,
        Self::ReportReduce,
        Self::ChannelDiscover,
        Self::ChannelPlan,
        Self::ChannelManifestRead,
        Self::ChannelTargetExecute,
        Self::ContentItemsRead,
        Self::ContentPrepare,
        Self::ContentGenerate,
        Self::ContentCheck,
        Self::ContentRepair,
        Self::ContentClose,
        Self::ContentStart,
        Self::ContentExecutionRead,
        Self::DistributionStart,
        Self::DistributionRead,
        Self::DistributionResume,
        Self::DistributionTargetsRead,
    ];

    /// The JS-visible name.  The trailing version is part of the contract.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ModelComplete => "model.complete.v1",
            Self::KnowledgeSearch => "knowledge.search.v1",
            Self::KnowledgeImportAttachments => "knowledge.import_attachments.v1",
            Self::ManifestRead => "manifest.read.v2",
            Self::Publish => "publish.submit.v2",
            Self::Measure => "measure.sample.v2",
            Self::ReportGet => "report.get.v1",
            Self::ReportReduce => "report.reduce.v1",
            Self::ChannelDiscover => "channel.discover.v1",
            Self::ChannelPlan => "channel.plan.v1",
            Self::ChannelManifestRead => "channel.manifest.read.v1",
            Self::ChannelTargetExecute => "channel.target.execute.v1",
            Self::ContentItemsRead => "content.items.read.v1",
            Self::ContentPrepare => "content.prepare.v1",
            Self::ContentGenerate => "content.generate.v1",
            Self::ContentCheck => "content.check.v1",
            Self::ContentRepair => "content.repair.v1",
            Self::ContentClose => "content.close.v1",
            Self::ContentStart => "content.start.v1",
            Self::ContentExecutionRead => "content.execution.read.v1",
            Self::DistributionStart => "distribution.start.v1",
            Self::DistributionRead => "distribution.read.v1",
            Self::DistributionResume => "distribution.resume.v1",
            Self::DistributionTargetsRead => "distribution.targets.read.v1",
        }
    }

    /// The registered isolate op that carries this capability.
    pub const fn op_name(self) -> &'static str {
        match self {
            Self::ModelComplete => "op_host_model_complete_v1",
            Self::KnowledgeSearch => "op_host_knowledge_search_v1",
            Self::KnowledgeImportAttachments => "op_host_knowledge_import_attachments_v1",
            Self::ManifestRead => "op_host_manifest_read_v2",
            Self::Publish => "op_host_publish_submit_v2",
            Self::Measure => "op_host_measure_sample_v2",
            Self::ReportGet => "op_host_report_get_v1",
            Self::ReportReduce => "op_host_report_reduce_v1",
            Self::ChannelDiscover => "op_host_channel_discover_v1",
            Self::ChannelPlan => "op_host_channel_plan_v1",
            Self::ChannelManifestRead => "op_host_channel_manifest_read_v1",
            Self::ChannelTargetExecute => "op_host_channel_target_execute_v1",
            Self::ContentItemsRead => "op_host_content_items_read_v1",
            Self::ContentPrepare => "op_host_content_prepare_v1",
            Self::ContentGenerate => "op_host_content_generate_v1",
            Self::ContentCheck => "op_host_content_check_v1",
            Self::ContentRepair => "op_host_content_repair_v1",
            Self::ContentClose => "op_host_content_close_v1",
            Self::ContentStart => "op_host_content_start_v1",
            Self::ContentExecutionRead => "op_host_content_execution_read_v1",
            Self::DistributionStart => "op_host_distribution_start_v1",
            Self::DistributionRead => "op_host_distribution_read_v1",
            Self::DistributionResume => "op_host_distribution_resume_v1",
            Self::DistributionTargetsRead => "op_host_distribution_targets_read_v1",
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
                HostOpLimits::new(120_000, 32),
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(60_000, 16),
                HostOpLimits::new(120_000, 32),
                HostOpLimits::new(15_000, 32),
                HostOpLimits::new(120_000, 4),
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(60_000, 16),
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(120_000, 32),
                HostOpLimits::new(15_000, 128), // paged content reads
                HostOpLimits::new(120_000, 2_048), // prepare
                HostOpLimits::new(120_000, 2_048), // generate
                HostOpLimits::new(120_000, 6_144), // check, up to three passes
                HostOpLimits::new(120_000, 4_096), // repair, up to two passes
                HostOpLimits::new(30_000, 4),   // close
                HostOpLimits::new(30_000, 4),   // start
                HostOpLimits::new(15_000, 32),  // execution read
                HostOpLimits::new(120_000, 4),  // freeze and advance one page
                HostOpLimits::new(15_000, 64),  // manifest state
                HostOpLimits::new(120_000, 32), // bounded expansion/revisit
                HostOpLimits::new(15_000, 128), // paged target reads
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
    /// Reconcile by the stable business key; do not resubmit blindly.
    UnknownResult,
    /// The same stable business idempotency key was used with a different
    /// payload.  This is a caller conflict, never a safe retry.
    IdempotencyConflict,
    /// The bridge could not produce a trustworthy result.
    Internal,
}

impl HostOpErrorCode {
    /// Whether retrying the same op with the same request could plausibly
    /// succeed.  An unknown external effect requires reconciliation, not a
    /// resend, even though a later query may succeed.
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::BudgetExceeded | Self::DeadlineExceeded | Self::Failed
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
            retryable: code.retryable()
                && !(matches!(
                    op,
                    HostOp::Publish | HostOp::Measure | HostOp::ChannelTargetExecute
                ) && matches!(
                    code,
                    HostOpErrorCode::DeadlineExceeded | HostOpErrorCode::Failed
                )),
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

    pub fn idempotency_conflict(op: HostOp, message: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::IdempotencyConflict, message)
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
/// - Before an external effect, implementations must resolve the supplied
///   intent/target IDs against the run's frozen, scope-owned manifest, reserve
///   the stable key with its binding hash durably, and persist an attempt.
///   UUID shape validation by the worker is not authorization.
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

    async fn knowledge_import_attachments(
        &self,
        _scope: &TenantScope,
        _request: KnowledgeImportAttachmentsRequest,
        _attachments: &[AttachmentReference],
    ) -> Result<KnowledgeImportAttachmentsResult, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::KnowledgeImportAttachments,
            "attachment knowledge import is not configured",
        ))
    }

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

    async fn report_get(
        &self,
        _scope: &TenantScope,
        _request: ReportGetRequest,
    ) -> Result<ReportSnapshot, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ReportGet,
            "report reads are not configured",
        ))
    }

    async fn report_reduce(
        &self,
        _scope: &TenantScope,
        _request: ReportReduceRequest,
    ) -> Result<ReportSnapshot, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ReportReduce,
            "report reduction is not configured",
        ))
    }

    async fn channel_discover(
        &self,
        _scope: &TenantScope,
        _request: ChannelDiscoverRequest,
    ) -> Result<ChannelDiscoveryPage, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ChannelDiscover,
            "channel discovery is not configured",
        ))
    }

    async fn channel_plan(
        &self,
        _scope: &TenantScope,
        _request: ChannelPlanRequest,
    ) -> Result<ChannelPlanReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ChannelPlan,
            "channel planning is not configured",
        ))
    }

    async fn channel_manifest_read(
        &self,
        _scope: &TenantScope,
        _request: ChannelManifestReadRequest,
    ) -> Result<ChannelManifestPage, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ChannelManifestRead,
            "channel manifest reading is not configured",
        ))
    }

    async fn channel_target_execute(
        &self,
        _scope: &TenantScope,
        _request: ChannelTargetExecuteRequest,
    ) -> Result<ChannelExecutionResult, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ChannelTargetExecute,
            "channel target execution is not configured",
        ))
    }

    async fn content_items_read(
        &self,
        _scope: &TenantScope,
        _request: ContentItemsReadRequest,
    ) -> Result<ContentItemsPage, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentItemsRead,
            "content execution is not configured",
        ))
    }

    async fn content_prepare(
        &self,
        _scope: &TenantScope,
        _request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentPrepare,
            "content execution is not configured",
        ))
    }

    async fn content_generate(
        &self,
        _scope: &TenantScope,
        _request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentGenerate,
            "content execution is not configured",
        ))
    }

    async fn content_check(
        &self,
        _scope: &TenantScope,
        _request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentCheck,
            "content execution is not configured",
        ))
    }

    async fn content_repair(
        &self,
        _scope: &TenantScope,
        _request: ContentStepRequest,
    ) -> Result<ContentItemRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentRepair,
            "content repair is not configured",
        ))
    }

    async fn content_close(
        &self,
        _scope: &TenantScope,
        _request: ContentCloseRequest,
    ) -> Result<ContentHandoffRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentClose,
            "content handoff is not configured",
        ))
    }

    async fn content_start(
        &self,
        _scope: &TenantScope,
        _request: ContentStartRequest,
    ) -> Result<ContentExecutionRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentStart,
            "content workflow dispatch is not configured",
        ))
    }

    async fn content_execution_read(
        &self,
        _scope: &TenantScope,
        _request: ContentExecutionReadRequest,
    ) -> Result<ContentExecutionRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ContentExecutionRead,
            "content execution is not configured",
        ))
    }

    async fn distribution_start(
        &self,
        _scope: &TenantScope,
        _request: DistributionStartRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::DistributionStart,
            "distribution is not configured",
        ))
    }

    async fn distribution_read(
        &self,
        _scope: &TenantScope,
        _request: DistributionReadRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::DistributionRead,
            "distribution is not configured",
        ))
    }

    async fn distribution_resume(
        &self,
        _scope: &TenantScope,
        _request: DistributionResumeRequest,
    ) -> Result<DistributionManifestRef, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::DistributionResume,
            "distribution is not configured",
        ))
    }

    async fn distribution_targets_read(
        &self,
        _scope: &TenantScope,
        _request: DistributionTargetsReadRequest,
    ) -> Result<DistributionTargetsPage, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::DistributionTargetsRead,
            "distribution is not configured",
        ))
    }
}

/// Selectors only: the trusted service freezes handoff, platform capabilities
/// and content versions. None of these requests contains publishable material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionStartRequest {
    #[serde(default)]
    pub cycle_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionReadRequest {
    #[serde(default)]
    pub cycle_id: Option<Uuid>,
    #[serde(default)]
    pub manifest_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionResumeRequest {
    pub manifest_id: Uuid,
    #[serde(default)]
    pub after_ordinal: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionTargetsReadRequest {
    pub manifest_id: Uuid,
    #[serde(default)]
    pub after_ordinal: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionManifestRef {
    pub manifest_id: Uuid,
    pub cycle_id: Uuid,
    pub revision: i32,
    pub document_manifest_id: Uuid,
    pub content_execution_id: Uuid,
    pub content_handoff_id: Uuid,
    pub expected_count: u64,
    pub expansion_cursor: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionTargetRef {
    pub target_id: Uuid,
    pub ordinal: u64,
    pub document_item_id: Uuid,
    pub content_revision_id: Option<Uuid>,
    pub platform_id: String,
    pub variant_id: Option<Uuid>,
    pub publication_intent_id: Option<Uuid>,
    pub status: DistributionTargetStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionTargetsPage {
    pub manifest_id: Uuid,
    pub expected_count: u64,
    pub items: Vec<DistributionTargetRef>,
    pub next_ordinal: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentStartRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentExecutionReadRequest {
    pub execution_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentExecutionRef {
    pub execution_id: Uuid,
    pub cycle_id: Uuid,
    pub status: ContentExecutionStatus,
    pub coverage: ContentCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentCloseRequest {
    pub execution_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentHandoffRef {
    pub execution_id: Uuid,
    pub handoff_id: Uuid,
    pub total: u64,
}

/// Only durable references and state cross the workflow boundary. Briefs,
/// evidence, document bodies, and model prompts remain inside Rust services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentItemsReadRequest {
    pub execution_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentStepRequest {
    pub execution_id: Uuid,
    pub item_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentItemRef {
    pub item_id: Uuid,
    pub branch_key: String,
    pub status: ContentItemStatus,
    pub automatic_repair_count: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentItemsPage {
    pub execution_id: Uuid,
    pub total: u64,
    pub items: Vec<ContentItemRef>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelDiscoveryKind {
    PublicSources,
    Accounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelDiscoverRequest {
    pub kind: ChannelDiscoveryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChannelDiscoveryItem {
    PublicSource {
        source_id: Uuid,
        source_version_id: Uuid,
        name: String,
        media_type: String,
    },
    Account {
        account_id: Uuid,
        platform: String,
        display_name: Option<String>,
        status: String,
        enabled: bool,
        owner_kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDiscoveryPage {
    pub kind: ChannelDiscoveryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_cycle_id: Option<Uuid>,
    pub items: Vec<ChannelDiscoveryItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPublicationPlanItem {
    pub source_id: Uuid,
    pub source_version_id: Uuid,
    pub platform: String,
    pub account_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelMeasurementPlanItem {
    pub account_id: Uuid,
    pub provider: String,
    pub model: String,
    pub surface: String,
    pub search_mode: String,
    pub protocol_version: String,
    pub question_set_version: String,
    pub question: String,
    pub market: String,
    pub language: String,
    pub scheduled_at: DateTime<Utc>,
    pub sample_ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPlanRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<Uuid>,
    pub publications: Vec<ChannelPublicationPlanItem>,
    pub measurements: Vec<ChannelMeasurementPlanItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPlanReceipt {
    pub plan_id: Uuid,
    pub cycle_id: Uuid,
    pub revision: i32,
    pub expected_count: u64,
    pub dispatch_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelManifestReadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelTargetKind {
    Publish,
    Measure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelExecutionState {
    Pending,
    Deferred,
    UnknownResult,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelExecutionResult {
    pub target_id: Uuid,
    pub state: ChannelExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelTargetSummary {
    pub target_id: Uuid,
    pub kind: ChannelTargetKind,
    pub account_id: Uuid,
    pub platform_or_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<Utc>>,
    pub execution: ChannelExecutionResult,
    /// Independent, read-only lookup of an ambiguous send. An observed asset
    /// proves existence, not that the original attempt published or verified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_lookup: Option<ChannelPublicationLookupSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPublicationLookupSummary {
    pub query_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due_at: Option<DateTime<Utc>>,
    pub in_progress: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_observation: Option<ChannelPublicationLookupObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPublicationLookupObservation {
    pub finding: PublicationLookupFinding,
    pub observed_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelManifestPage {
    pub plan_id: Uuid,
    pub cycle_id: Uuid,
    pub revision: i32,
    pub sealed: bool,
    pub expected_count: u64,
    pub items: Vec<ChannelTargetSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelTargetExecuteRequest {
    pub target_id: Uuid,
}

fn channel_cursor_valid(cursor: &Option<String>) -> bool {
    cursor
        .as_ref()
        .is_none_or(|value| !value.is_empty() && value.len() <= 512)
}

fn channel_label_valid(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 512
}

fn channel_lookup_error_valid(value: &str) -> bool {
    matches!(
        value,
        "candidate_missing"
            | "candidate_invalid"
            | "target_mismatch"
            | "connector_version_missing"
            | "connector_version_invalid"
            | "binding_missing"
            | "binding_unavailable"
            | "runner_unavailable"
            | "account_busy"
            | "account_reservation_failed"
            | "account_or_network_unavailable"
            | "connector_version_mismatch"
            | "lookup_preflight_expired"
            | "readback_unverified"
            | "lookup_unavailable"
            | "lookup_error"
    )
}

impl ChannelDiscoverRequest {
    pub fn validate(&self) -> Result<(), String> {
        if !channel_cursor_valid(&self.cursor)
            || self.limit.is_some_and(|limit| limit == 0 || limit > 100)
        {
            return Err("channel discovery cursor or limit is invalid".into());
        }
        Ok(())
    }
}
impl ChannelDiscoveryPage {
    pub fn validate_for(&self, request: &ChannelDiscoverRequest) -> Result<(), String> {
        if self.kind != request.kind
            || self.current_cycle_id.is_some_and(|id| id.is_nil())
            || self.items.len() > request.limit.unwrap_or(25) as usize
            || !channel_cursor_valid(&self.next_cursor)
            || self.items.iter().any(|item| match item {
                ChannelDiscoveryItem::PublicSource {
                    source_id,
                    source_version_id,
                    name,
                    media_type,
                } => {
                    request.kind != ChannelDiscoveryKind::PublicSources
                        || source_id.is_nil()
                        || source_version_id.is_nil()
                        || !channel_label_valid(name)
                        || !channel_label_valid(media_type)
                }
                ChannelDiscoveryItem::Account {
                    account_id,
                    platform,
                    display_name,
                    status,
                    owner_kind,
                    ..
                } => {
                    request.kind != ChannelDiscoveryKind::Accounts
                        || account_id.is_nil()
                        || !channel_label_valid(platform)
                        || display_name
                            .as_ref()
                            .is_some_and(|value| !channel_label_valid(value))
                        || !channel_label_valid(status)
                        || !channel_label_valid(owner_kind)
                }
            })
        {
            return Err("channel discovery page does not match the request".into());
        }
        Ok(())
    }
}
impl ChannelPlanRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.cycle_id.is_some_and(|id| id.is_nil())
            || self.publications.len() + self.measurements.len() == 0
            || self.publications.len() + self.measurements.len() > 100
            || self.publications.iter().any(|item| {
                item.source_id.is_nil()
                    || item.source_version_id.is_nil()
                    || item.account_id.is_nil()
                    || !channel_label_valid(&item.platform)
            })
            || self.measurements.iter().any(|item| {
                item.account_id.is_nil()
                    || [
                        &item.provider,
                        &item.model,
                        &item.surface,
                        &item.search_mode,
                        &item.protocol_version,
                        &item.question_set_version,
                        &item.market,
                        &item.language,
                    ]
                    .iter()
                    .any(|value| !channel_label_valid(value))
                    || item.question.trim().is_empty()
                    || item.question.len() > MAX_MEASUREMENT_QUESTION_BYTES
            })
        {
            return Err("channel plan contains invalid or excess targets".into());
        }
        Ok(())
    }
}
impl ChannelPlanReceipt {
    pub fn validate_for(&self, request: &ChannelPlanRequest) -> Result<(), String> {
        if self.plan_id.is_nil()
            || self.cycle_id.is_nil()
            || self.revision <= 0
            || request.cycle_id.is_some_and(|id| id != self.cycle_id)
            || self.expected_count
                != (request.publications.len() + request.measurements.len()) as u64
            || self.dispatch_state != "pending"
        {
            return Err("channel plan receipt does not match the request".into());
        }
        Ok(())
    }
}
impl ChannelManifestReadRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.cycle_id.is_some_and(|id| id.is_nil())
            || self.revision.is_some_and(|revision| revision <= 0)
            || !channel_cursor_valid(&self.cursor)
            || self.limit.is_some_and(|limit| limit == 0 || limit > 100)
        {
            return Err("channel manifest cursor, revision or limit is invalid".into());
        }
        Ok(())
    }
}
impl ChannelExecutionResult {
    pub fn validate_for(&self, target_id: Uuid) -> Result<(), String> {
        if target_id.is_nil()
            || self.target_id != target_id
            || self.attempt_id.is_some_and(|id| id.is_nil())
            || self.evidence_ref.is_some_and(|id| id.is_nil())
            || [
                &self.outcome_status,
                &self.deferred_reason,
                &self.public_url,
            ]
            .iter()
            .any(|value| value.as_ref().is_some_and(|value| value.len() > 2048))
        {
            return Err("channel execution does not match the target".into());
        }
        Ok(())
    }
}
impl ChannelManifestPage {
    pub fn validate_for(&self, request: &ChannelManifestReadRequest) -> Result<(), String> {
        if self.plan_id.is_nil()
            || self.cycle_id.is_nil()
            || self.revision <= 0
            || request.cycle_id.is_some_and(|id| id != self.cycle_id)
            || request
                .revision
                .is_some_and(|revision| revision != self.revision)
            || self.items.len() > request.limit.unwrap_or(25) as usize
            || !channel_cursor_valid(&self.next_cursor)
            || self.items.iter().any(|item| {
                item.target_id.is_nil()
                    || item.account_id.is_nil()
                    || !channel_label_valid(&item.platform_or_provider)
                    || item.source_id.is_some_and(|id| id.is_nil())
                    || item.source_version_id.is_some_and(|id| id.is_nil())
                    || item.execution.validate_for(item.target_id).is_err()
                    || item.publication_lookup.as_ref().is_some_and(|lookup| {
                        item.kind != ChannelTargetKind::Publish
                            || item.execution.state != ChannelExecutionState::UnknownResult
                            || item.execution.attempt_id.is_none()
                            || lookup
                                .last_error_code
                                .as_deref()
                                .is_some_and(|code| !channel_lookup_error_valid(code))
                            || lookup
                                .latest_observation
                                .as_ref()
                                .is_some_and(|observation| {
                                    observation.observed_at > observation.received_at
                                })
                    })
                    || match item.kind {
                        ChannelTargetKind::Publish => {
                            item.source_id.is_none() || item.source_version_id.is_none()
                        }
                        ChannelTargetKind::Measure => {
                            item.source_id.is_some()
                                || item.source_version_id.is_some()
                                || item.scheduled_at.is_none()
                        }
                    }
            })
        {
            return Err("channel manifest page does not match the request".into());
        }
        Ok(())
    }
}
impl ChannelTargetExecuteRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.target_id.is_nil() {
            return Err("target ID must be non-zero".into());
        }
        Ok(())
    }
}

/// Scope is supplied by the Rust bridge, never by JavaScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportGetRequest {
    #[serde(default)]
    pub report_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportReduceRequest {
    #[serde(default)]
    pub cycle_id: Option<Uuid>,
    #[serde(default)]
    pub correction_of: Option<Uuid>,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// A configured model routing identifier, not an endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ModelMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ModelToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMessage {
    pub role: String,
    /// Null is valid for assistant messages that contain tool calls.
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ModelToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ModelToolFunctionDefinition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelToolFunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ModelToolFunctionCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelToolFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCompletion {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ModelToolCall>,
    /// The routing identifier that actually answered, so a run can record which
    /// model produced its content.
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub finish_reason: String,
}

/// Model-selectable IDs and purpose only. Object metadata and business keys
/// come exclusively from the Rust-owned run binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeImportAttachmentsRequest {
    pub items: Vec<KnowledgeImportAttachmentItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeImportAttachmentItem {
    pub attachment_id: Uuid,
    pub purpose: KnowledgePurpose,
}

impl KnowledgeImportAttachmentsRequest {
    pub fn validate(&self, attachments: &[AttachmentReference]) -> Result<(), HostOpError> {
        let op = HostOp::KnowledgeImportAttachments;
        if self.items.is_empty() || self.items.len() > 100 {
            return Err(HostOpError::invalid_request(
                op,
                "items must contain 1 to 100 attachments",
            ));
        }
        let bound: HashSet<_> = attachments
            .iter()
            .map(|item| item.attachment_id.as_uuid())
            .collect();
        let mut requested = HashSet::with_capacity(self.items.len());
        for item in &self.items {
            if item.attachment_id.is_nil() || !requested.insert(item.attachment_id) {
                return Err(HostOpError::invalid_request(
                    op,
                    "attachment IDs must be non-nil and unique",
                ));
            }
            if !bound.contains(&item.attachment_id) {
                return Err(HostOpError::denied(
                    op,
                    "attachment is not bound to this run",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeImportAttachmentsResult {
    pub items: Vec<KnowledgeImportAttachmentResultItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeImportAttachmentResultItem {
    pub attachment_id: Uuid,
    pub status: ImportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_release_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
}

#[cfg(test)]
mod model_contract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_tool_loop_payload_roundtrips_without_scope_or_endpoint_fields() {
        let request = json!({
            "messages": [
                {"role":"user","content":"question"},
                {"role":"assistant","content":null,"tool_calls":[
                    {"id":"call_1","type":"function","function":{"name":"knowledge_search","arguments":"{\"query\":\"warranty\"}"}}
                ]},
                {"role":"tool","content":"{\"answer\":\"found\"}","tool_call_id":"call_1"}
            ],
            "tools":[{"type":"function","function":{
                "name":"knowledge_search","description":"Search project knowledge",
                "parameters":{"type":"object","properties":{"query":{"type":"string"}}}
            }}]
        });
        let parsed: ModelCompletionRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), request);
        assert_eq!(parsed.messages[1].content, None);
        assert_eq!(parsed.messages[2].tool_call_id.as_deref(), Some("call_1"));
        let mut forbidden = request;
        forbidden["tenant_id"] = json!("different-tenant");
        assert!(serde_json::from_value::<ModelCompletionRequest>(forbidden).is_err());
    }
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
    /// The frozen manifest to read.  `None` is only a discovery request; a
    /// production implementation must resolve it from the run's Rust-owned
    /// state and must never accept a project or tenant selector from JS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_id: Option<Uuid>,
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
    /// Planning identity exists before a document revision is generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_manifest_item_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_state: Option<ManifestPlanningState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_revision_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_target_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestPlanningState {
    Planned,
    Blocked,
    Deferred,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestCoverage {
    pub total: u64,
    pub planned: u64,
    pub blocked: u64,
    pub deferred: u64,
    pub not_applicable: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<ManifestCoverage>,
    pub items: Vec<ManifestItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One document revision addressed to one platform target.
///
/// The script presents a frozen intent and target.  The Rust implementation
/// must verify their membership and derive the external key from its own
/// scoped ledger; presenting a well-formed UUID does not authorize a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    /// Stable logical publication intent.  Retries reuse this ID and must not
    /// create a replacement intent merely because an external result is
    /// unknown.
    pub publication_intent_id: Uuid,
    pub document_revision_id: Uuid,
    pub platform_target_id: Uuid,
    /// SHA-256 of `body`, lower-case hexadecimal.  The Rust bridge verifies it
    /// before any connector is called, binding the intent to its payload.
    pub payload_sha256: String,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementSurface {
    OfficialApi,
    ConsumerWeb,
    MobileApp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureRequest {
    pub measurement_protocol_id: Uuid,
    /// Stable scheduled sample target.  Technical retries reuse this ID and
    /// therefore do not increase the measurement denominator.
    pub scheduled_sample_id: Uuid,
    pub question: String,
    /// Provider/channel identifier named by the frozen protocol.
    pub channel: String,
    /// Observation surfaces have separate denominators and may not be
    /// silently substituted when one becomes unavailable.
    pub surface: MeasurementSurface,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasureSample {
    pub sample_id: Uuid,
    pub channel: String,
    pub surface: MeasurementSurface,
    pub answer: String,
    /// Reference to immutable raw request/response evidence.  Citation refs
    /// alone are insufficient because a no-citation answer is still a sample.
    pub observation_ref: Uuid,
    pub evidence_refs: Vec<Uuid>,
    pub observed_at: DateTime<Utc>,
}

/// Maximum payload sizes enforced before a connector or measurement adapter is
/// reached.  These are deliberately conservative; larger artifacts belong in
/// object storage and are referenced by a versioned content ID.
pub const MAX_PUBLISH_BODY_BYTES: usize = 256 * 1024;
pub const MAX_MEASUREMENT_QUESTION_BYTES: usize = 16 * 1024;

impl ManifestReadRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.revision.is_some_and(|revision| revision <= 0) {
            return Err("manifest revision must be positive".to_owned());
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.len() > 512)
        {
            return Err("manifest cursor exceeds 512 bytes".to_owned());
        }
        Ok(())
    }
}

impl ManifestPage {
    pub fn validate_for(&self, request: &ManifestReadRequest) -> Result<(), String> {
        if self.manifest_id.is_nil()
            || self.kind != request.kind
            || self.revision <= 0
            || request
                .manifest_id
                .is_some_and(|manifest_id| manifest_id != self.manifest_id)
            || request
                .revision
                .is_some_and(|revision| revision != self.revision)
        {
            return Err("manifest page does not match the requested frozen manifest".to_owned());
        }
        if self.items.len() > request.limit.unwrap_or(100) as usize
            || self.items.iter().any(|item| {
                item.branch_id.is_empty()
                    || item.document_revision_id.is_some_and(|id| id.is_nil())
                    || item.document_manifest_item_id.is_some_and(|id| id.is_nil())
                    || item.platform_target_id.is_some_and(|id| id.is_nil())
                    || match self.kind {
                        ManifestKind::Document => {
                            item.document_manifest_item_id.is_none()
                                || item.planning_state.is_none()
                        }
                        ManifestKind::Distribution => {
                            item.document_revision_id.is_none() || item.platform_target_id.is_none()
                        }
                    }
            })
        {
            return Err("manifest page contains invalid or excess items".to_owned());
        }
        Ok(())
    }
}

impl PublishRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.publication_intent_id.is_nil()
            || self.document_revision_id.is_nil()
            || self.platform_target_id.is_nil()
        {
            return Err("publication IDs must be non-zero UUIDs".to_owned());
        }
        if self.body.is_empty() {
            return Err("publication body must not be empty".to_owned());
        }
        if self.body.len() > MAX_PUBLISH_BODY_BYTES {
            return Err(format!(
                "publication body exceeds {MAX_PUBLISH_BODY_BYTES} bytes"
            ));
        }
        if !is_sha256_hex(&self.payload_sha256)
            || geo_domain::sha256_hex(self.body.as_bytes()) != self.payload_sha256
        {
            return Err("payload_sha256 must match the publication body".to_owned());
        }
        Ok(())
    }

    /// An opaque, scope-bound key suitable for a connector's idempotency
    /// ledger.  The payload hash is deliberately *not* in this key: the ledger
    /// compares the stored hash separately, so a changed body under the same
    /// logical intent conflicts instead of silently becoming a new publish.
    pub fn idempotency_key(&self, scope: &TenantScope) -> String {
        let material = format!(
            "geo.publication.v1|{}|{}",
            scope.storage_key(),
            self.publication_intent_id,
        );
        geo_domain::sha256_hex(material.as_bytes())
    }

    /// Bind the stable key to both destination and content.  The durable
    /// ledger rejects a different digest under an existing intent.
    pub fn binding_hash(&self) -> String {
        let material = format!(
            "{}|{}|{}",
            self.document_revision_id, self.platform_target_id, self.payload_sha256
        );
        geo_domain::sha256_hex(material.as_bytes())
    }
}

impl PublishReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.publish_attempt_id.is_nil()
            || (matches!(self.state, PublishState::Published)
                && self.evidence_ref.is_none_or(|id| id.is_nil()))
        {
            return Err("publication receipt lacks an attempt or published evidence".to_owned());
        }
        Ok(())
    }
}

impl MeasureRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.measurement_protocol_id.is_nil() || self.scheduled_sample_id.is_nil() {
            return Err(
                "measurement protocol and scheduled sample IDs must be non-zero UUIDs".to_owned(),
            );
        }
        if self.question.trim().is_empty() {
            return Err("measurement question must not be empty".to_owned());
        }
        if self.question.len() > MAX_MEASUREMENT_QUESTION_BYTES {
            return Err(format!(
                "measurement question exceeds {MAX_MEASUREMENT_QUESTION_BYTES} bytes"
            ));
        }
        if self.channel.trim().is_empty() || self.channel.len() > 128 {
            return Err("measurement channel must be between 1 and 128 bytes".to_owned());
        }
        Ok(())
    }

    /// A stable key for a scheduled observation.  Repeating the technical
    /// request therefore replays/queries the same sample rather than creating
    /// an extra denominator entry.
    pub fn idempotency_key(&self, scope: &TenantScope) -> String {
        let material = format!(
            "geo.measurement.v1|{}|{}|{}",
            scope.storage_key(),
            self.scheduled_sample_id,
            self.measurement_protocol_id
        );
        geo_domain::sha256_hex(material.as_bytes())
    }

    /// Bind the scheduled sample to its exact question and observation
    /// surface, preventing a fallback from changing a frozen denominator.
    pub fn binding_hash(&self) -> String {
        let material = serde_json::to_vec(&(
            self.measurement_protocol_id,
            &self.question,
            &self.channel,
            self.surface,
        ))
        .expect("measurement identity contains only serializable fields");
        geo_domain::sha256_hex(&material)
    }
}

impl MeasureSample {
    pub fn validate_for(&self, request: &MeasureRequest) -> Result<(), String> {
        if self.sample_id != request.scheduled_sample_id
            || self.channel != request.channel
            || self.surface != request.surface
            || self.observation_ref.is_nil()
        {
            return Err(
                "measurement sample does not match its scheduled target or raw evidence".to_owned(),
            );
        }
        Ok(())
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value == value.to_ascii_lowercase()
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
    attachments: Arc<[AttachmentReference]>,
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
            attachments: Arc::from(Vec::<AttachmentReference>::new()),
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

    /// Immutable attachment references accepted with this run's root message.
    pub fn with_attachments(mut self, attachments: Vec<AttachmentReference>) -> Self {
        self.attachments = Arc::from(attachments);
        self
    }

    pub fn attachments(&self) -> &[AttachmentReference] {
        &self.attachments
    }

    /// Shares the run's cancellation flag.  Raising it stops later ops before
    /// they start; an in-flight publication or measurement becomes uncertain
    /// and requires reconciliation instead of a blind retry.
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
        () = &mut cancellation => {
            if matches!(op, HostOp::Publish | HostOp::Measure | HostOp::ChannelTargetExecute) {
                Err(HostOpError::unknown_result(op, "in-flight external result must be reconciled after cancellation"))
            } else {
                Err(HostOpError::cancelled(op))
            }
        },
        () = &mut deadline => {
            if matches!(op, HostOp::Publish | HostOp::Measure | HostOp::ChannelTargetExecute) {
                Err(HostOpError::unknown_result(op, "in-flight external result must be reconciled after deadline"))
            } else {
                Err(HostOpError::deadline_exceeded(op, limits.timeout_ms))
            }
        },
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
    use std::collections::HashMap;

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
        assert_eq!(budgets.limits(HostOp::ContentGenerate).max_calls, 2_048);
        assert_eq!(budgets.limits(HostOp::ContentCheck).max_calls, 6_144);
        assert_eq!(budgets.limits(HostOp::ContentRepair).max_calls, 4_096);
        assert_eq!(budgets.limits(HostOp::ContentClose).max_calls, 4);
        assert_eq!(budgets.limits(HostOp::ContentStart).max_calls, 4);
        assert_eq!(budgets.limits(HostOp::DistributionStart).max_calls, 4);
        assert_eq!(
            budgets.limits(HostOp::DistributionTargetsRead).max_calls,
            128
        );
    }

    #[test]
    fn distribution_selectors_reject_scope_and_publishable_payloads() {
        for raw in [
            r#"{"cycle_id":null,"tenant_id":"other"}"#,
            r#"{"cycle_id":null,"platform_id":"unverified"}"#,
            r#"{"cycle_id":null,"body":"model-authored text"}"#,
            r#"{"cycle_id":null,"capability_version":"claimed"}"#,
        ] {
            assert!(serde_json::from_str::<DistributionStartRequest>(raw).is_err());
        }
        assert!(serde_json::from_str::<DistributionTargetsReadRequest>(
            r#"{"manifest_id":"00000000-0000-4000-8000-000000000001","account_id":"00000000-0000-4000-8000-000000000002"}"#
        ).is_err());
        assert_eq!(
            serde_json::from_str::<DistributionStartRequest>("{}")
                .unwrap()
                .cycle_id,
            None
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

    #[test]
    fn publication_intent_replays_same_payload_and_conflicts_on_changed_payload() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let intent = Uuid::new_v4();
        let document = Uuid::new_v4();
        let target = Uuid::new_v4();
        let request = |body: &str| PublishRequest {
            publication_intent_id: intent,
            document_revision_id: document,
            platform_target_id: target,
            payload_sha256: geo_domain::sha256_hex(body.as_bytes()),
            body: body.to_owned(),
        };
        let first = request("first revision");
        let replay = request("first revision");
        let conflict = request("changed revision");
        let mut ledger = HashMap::new();
        let key = first.idempotency_key(&scope);
        assert_eq!(first.validate(), Ok(()));
        ledger.insert(key.clone(), first.binding_hash());
        assert_eq!(replay.idempotency_key(&scope), key);
        assert_eq!(ledger.get(&key), Some(&replay.binding_hash()));
        assert_eq!(conflict.idempotency_key(&scope), key);
        assert_ne!(ledger.get(&key), Some(&conflict.binding_hash()));
        assert_ne!(
            ledger.get(&key),
            Some(
                &PublishRequest {
                    platform_target_id: Uuid::new_v4(),
                    ..first.clone()
                }
                .binding_hash()
            )
        );
        let error = HostOpError::idempotency_conflict(
            HostOp::Publish,
            "publication intent is bound to a different payload",
        );
        assert_eq!(error.code, HostOpErrorCode::IdempotencyConflict);
        assert!(!error.retryable);
        assert_ne!(
            first.idempotency_key(&TenantScope::new(
                scope.operator_id,
                Uuid::new_v4().into(),
                scope.project_id,
            )),
            key
        );
    }

    #[test]
    fn external_effects_fail_closed_and_require_evidence() {
        for op in [HostOp::Publish, HostOp::Measure] {
            for code in [
                HostOpErrorCode::Failed,
                HostOpErrorCode::DeadlineExceeded,
                HostOpErrorCode::UnknownResult,
            ] {
                assert!(
                    !HostOpError::new(op, code, "unconfirmed external result").retryable,
                    "{op:?}/{code:?} must reconcile rather than resend"
                );
            }
        }
        let receipt = PublishReceipt {
            publish_attempt_id: Uuid::new_v4(),
            state: PublishState::Published,
            external_url: None,
            evidence_ref: None,
        };
        assert!(receipt.validate().is_err());
        assert!(
            PublishReceipt {
                evidence_ref: Some(Uuid::new_v4()),
                ..receipt
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn scheduled_measurement_identity_is_scope_bound_and_surface_stable() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let request = MeasureRequest {
            measurement_protocol_id: Uuid::new_v4(),
            scheduled_sample_id: Uuid::new_v4(),
            question: "How is the product described?".to_owned(),
            channel: "test-channel".to_owned(),
            surface: MeasurementSurface::ConsumerWeb,
        };
        assert_eq!(request.validate(), Ok(()));
        let different_surface = MeasureRequest {
            surface: MeasurementSurface::OfficialApi,
            ..request.clone()
        };
        assert_eq!(
            request.idempotency_key(&scope),
            different_surface.idempotency_key(&scope)
        );
        assert_ne!(request.binding_hash(), different_surface.binding_hash());
        let sample = MeasureSample {
            sample_id: request.scheduled_sample_id,
            channel: request.channel.clone(),
            surface: request.surface,
            answer: "No answer".to_owned(),
            observation_ref: Uuid::new_v4(),
            evidence_refs: Vec::new(),
            observed_at: Utc::now(),
        };
        assert_eq!(sample.validate_for(&request), Ok(()));
        assert!(
            MeasureSample {
                surface: MeasurementSurface::OfficialApi,
                ..sample
            }
            .validate_for(&request)
            .is_err()
        );
    }

    #[tokio::test]
    async fn in_flight_effect_timeout_and_cancellation_require_reconciliation() {
        for op in [
            HostOp::Publish,
            HostOp::Measure,
            HostOp::ChannelTargetExecute,
        ] {
            let cancelled = Arc::new(AtomicBool::new(true));
            let error = under_budget(
                op,
                HostOpLimits::new(50, 1),
                cancelled,
                std::future::pending::<Result<(), HostOpError>>(),
            )
            .await
            .expect_err("an in-flight cancellation is uncertain");
            assert_eq!(error.code, HostOpErrorCode::UnknownResult);
            assert!(!error.retryable);

            let error = under_budget(
                op,
                HostOpLimits::new(1, 1),
                Arc::new(AtomicBool::new(false)),
                std::future::pending::<Result<(), HostOpError>>(),
            )
            .await
            .expect_err("an in-flight deadline is uncertain");
            assert_eq!(error.code, HostOpErrorCode::UnknownResult);
            assert!(!error.retryable);
        }
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
