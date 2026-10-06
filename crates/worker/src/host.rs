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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    ReportPreview, ReportPreviewKind, ReportSnapshot,
};
pub use geo_domain::{CreateQuestionSet, QuestionReference, ReviseQuestionSet};
pub use geo_domain::{KnowledgeSearchRequest, KnowledgeSearchResult, TenantScope};
pub use geo_domain::{ToolCallIdentity, ToolCallOutcome};

/// A scoped, durable sink installed by Rust for one authorised run. `begin`
/// confirms that an intent exists (including idempotent replay); `attempt`
/// returns `true` only for the unique caller that atomically claims dispatch.
#[async_trait]
pub trait ToolCallRecorder: Send + Sync {
    async fn begin(&self, identity: &ToolCallIdentity) -> Result<bool, HostOpError>;
    async fn attempt(&self, identity: &ToolCallIdentity) -> Result<bool, HostOpError>;
    async fn finish(
        &self,
        identity: &ToolCallIdentity,
        outcome: ToolCallOutcome,
    ) -> Result<(), HostOpError>;
}

// Validate inside the application-runtime task before writing Succeeded. No
// raw response, question, publication body or provider output reaches the
// ledger; failed conversion also has a static, non-sensitive error.
fn validate_recorded_return(
    op: HostOp,
    scope: &TenantScope,
    request: &serde_json::Value,
    result: &serde_json::Value,
) -> Result<bool, HostOpError> {
    fn typed<T: serde::de::DeserializeOwned>(
        op: HostOp,
        value: &serde_json::Value,
    ) -> Result<T, HostOpError> {
        serde_json::from_value(value.clone()).map_err(|_| {
            if matches!(
                op,
                HostOp::Publish | HostOp::Measure | HostOp::ChannelTargetExecute
            ) {
                HostOpError::unknown_result(op, "external result is malformed")
            } else {
                HostOpError::internal(op, "typed host result is malformed")
            }
        })
    }
    let invalid = |_: String| HostOpError::internal(op, "typed host result does not match request");
    match op {
        HostOp::ProjectCurrent | HostOp::ProjectRevise => {
            let response: ProjectCurrentResult = typed(op, result)?;
            response.validate_for(scope).map_err(invalid)?;
        }
        HostOp::ProjectStart => {
            let response: geo_domain::ProjectStartAcceptance = typed(op, result)?;
            if response.operation_id.is_nil()
                || response.cycle_id.is_nil()
                || response.config_revision_id.is_nil()
            {
                return Err(HostOpError::internal(
                    op,
                    "project acceptance lacks durable references",
                ));
            }
        }
        HostOp::KnowledgeSearch => {
            let response: KnowledgeSearchResult = typed(op, result)?;
            if response.capability_missing.is_some() {
                return Err(HostOpError::capability_missing(
                    op,
                    "knowledge retrieval capability is unavailable",
                ));
            }
        }
        HostOp::KnowledgeTextRead => {
            let requested: KnowledgeTextReadRequest = typed(op, request)?;
            let response: KnowledgeTextReadResult = typed(op, result)?;
            if response.source.operator_id != scope.operator_id
                || response.source.tenant_id != scope.tenant_id
                || Some(response.source.project_id) != scope.project_id
                || response.source.source_id != requested.source_id
                || response.source_version.operator_id != scope.operator_id
                || response.source_version.tenant_id != scope.tenant_id
                || Some(response.source_version.project_id) != scope.project_id
                || response.source_version.source_id != requested.source_id
                || response.source_version.source_version_id != requested.source_version_id
                || response.content.source_version_id != requested.source_version_id
                || response.content.representation != response.source_version.representation
            {
                return Err(HostOpError::internal(
                    op,
                    "knowledge text read returned another source",
                ));
            }
        }
        HostOp::KnowledgeTextRevise => {
            let requested: KnowledgeTextReviseRequest = typed(op, request)?;
            let response: geo_domain::SourceTextRevisionReceipt = typed(op, result)?;
            if response.source.operator_id != scope.operator_id
                || response.source.tenant_id != scope.tenant_id
                || Some(response.source.project_id) != scope.project_id
                || response.source.source_id != requested.source_id
                || response.source.revision != requested.expected_revision + 1
                || response.source.current_version_id
                    != Some(response.source_version.source_version_id)
                || response.source_version.source_id != requested.source_id
                || response.source_version.operator_id != scope.operator_id
                || response.source_version.tenant_id != scope.tenant_id
                || Some(response.source_version.project_id) != scope.project_id
                || response.source_version.parent_version_id != Some(requested.base_version_id)
                || response.source_version.representation
                    != geo_domain::SourceVersionRepresentation::AuthoredText
                || response.source_version.content_sha256
                    != geo_domain::sha256_hex(requested.text.as_bytes())
                || Some(response.knowledge_release.project_id) != scope.project_id
                || response.knowledge_release.operator_id != scope.operator_id
                || response.knowledge_release.tenant_id != scope.tenant_id
                || !response
                    .knowledge_release
                    .source_version_refs
                    .contains(&response.source_version.source_version_id)
            {
                return Err(HostOpError::internal(
                    op,
                    "knowledge text revision returned an unrelated receipt",
                ));
            }
        }
        HostOp::KnowledgeImportStatus => {
            let requested: KnowledgeImportStatusRequest = typed(op, request)?;
            let response: geo_domain::KnowledgeImportProgress = typed(op, result)?;
            validate_import_status(&response, requested.import_job_id).map_err(invalid)?;
        }
        HostOp::ManifestRead => {
            let requested: ManifestReadRequest = typed(op, request)?;
            let page: ManifestPage = typed(op, result)?;
            page.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::ChannelDiscover => {
            let requested: ChannelDiscoverRequest = typed(op, request)?;
            let page: ChannelDiscoveryPage = typed(op, result)?;
            page.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::ChannelPlan => {
            let requested: ChannelPlanRequest = typed(op, request)?;
            let receipt: ChannelPlanReceipt = typed(op, result)?;
            receipt.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::QuestionDiscover => {
            let requested: QuestionDiscoverRequest = typed(op, request)?;
            let page: QuestionDiscoveryPage = typed(op, result)?;
            page.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::QuestionCreate => {
            let receipt: QuestionWriteReceipt = typed(op, result)?;
            receipt.validate().map_err(invalid)?;
        }
        HostOp::QuestionRevise => {
            let requested: QuestionReviseRequest = typed(op, request)?;
            let receipt: QuestionWriteReceipt = typed(op, result)?;
            receipt.validate().map_err(invalid)?;
            if receipt.question_set_id != requested.question_set_id {
                return Err(HostOpError::internal(
                    op,
                    "question revision returned another set",
                ));
            }
        }
        HostOp::MeasurementOptions => {
            let requested: MeasurementOptionsRequest = typed(op, request)?;
            let options: MeasurementOptionsResult = typed(op, result)?;
            options.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::MeasurementPlanCreate => {
            let requested: MeasurementPlanCreateRequest = typed(op, request)?;
            let receipt: MeasurementPlanReceipt = typed(op, result)?;
            receipt.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::MeasurementPlanRead => {
            let requested: MeasurementPlanReadRequest = typed(op, request)?;
            let status: MeasurementPlanStatus = typed(op, result)?;
            status.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::ChannelManifestRead => {
            let requested: ChannelManifestReadRequest = typed(op, request)?;
            let page: ChannelManifestPage = typed(op, result)?;
            page.validate_for(&requested).map_err(invalid)?;
        }
        HostOp::ChannelTargetExecute => {
            let requested: ChannelTargetExecuteRequest = typed(op, request)?;
            let response: ChannelExecutionResult = typed(op, result)?;
            response.validate_for(requested.target_id).map_err(|_| {
                HostOpError::unknown_result(op, "channel execution result is invalid")
            })?;
            if matches!(response.state, ChannelExecutionState::UnknownResult) {
                return Ok(true);
            }
        }
        HostOp::Publish => {
            let response: PublishReceipt = typed(op, result)?;
            response
                .validate()
                .map_err(|_| HostOpError::unknown_result(op, "publication receipt is invalid"))?;
            if matches!(response.state, PublishState::UnknownResult) {
                return Ok(true);
            }
        }
        HostOp::Measure => {
            let requested: MeasureRequest = typed(op, request)?;
            let response: MeasureSample = typed(op, result)?;
            response
                .validate_for(&requested)
                .map_err(|_| HostOpError::unknown_result(op, "measurement sample is invalid"))?;
        }
        HostOp::ReportPreview => {
            let requested: ReportPreviewRequest = typed(op, request)?;
            let response: ReportPreview = typed(op, result)?;
            if result.as_object().is_none_or(|fields| {
                ["report_id", "revision", "correction_of"]
                    .iter()
                    .any(|field| fields.contains_key(*field))
            }) || response.kind != ReportPreviewKind::Preview
                || scope.project_id != Some(response.project_id)
                || requested.cycle_id.is_some_and(|id| id != response.cycle_id)
                || response.cycle_id.is_nil()
                || response.evidence_as_of > response.cutoff_at
                || response.evidence_as_of > response.generated_at
            {
                return Err(HostOpError::internal(
                    op,
                    "preview returned invalid scope or evidence",
                ));
            }
        }
        HostOp::ContentPrepare
        | HostOp::ContentGenerate
        | HostOp::ContentCheck
        | HostOp::ContentRepair => {
            let requested: ContentStepRequest = typed(op, request)?;
            let response: ContentItemRef = typed(op, result)?;
            if requested.item_id != response.item_id || response.branch_key.is_empty() {
                return Err(HostOpError::internal(op, "step returned an unrelated item"));
            }
        }
        _ => {}
    }
    Ok(false)
}

fn validate_import_status(
    response: &geo_domain::KnowledgeImportProgress,
    requested_job_id: Uuid,
) -> Result<(), String> {
    if response.import_job_id != Some(requested_job_id)
        || response.completed_units < 0
        || response.failed_units < 0
        || response.errors.len() > 100
        || usize::try_from(response.error_count).unwrap_or(usize::MAX) < response.errors.len()
        || response.errors.iter().any(|error| {
            !matches!(
                error.code.as_str(),
                "capability_missing"
                    | "invalid_request"
                    | "not_found"
                    | "conflict"
                    | "dependency_unavailable"
                    | "invalid_pdf"
                    | "encrypted_pdf"
                    | "parse_failed"
                    | "page_limit"
                    | "ocr_required"
                    | "empty_text"
                    | "import_failed"
            ) || error.page == Some(0)
        })
    {
        return Err("job identity or bounded progress is invalid".into());
    }
    match response.status {
        ImportStatus::Queued | ImportStatus::Running => {
            if response.source_version_id.is_some() || response.knowledge_release_id.is_some() {
                return Err("pending import cannot expose ready evidence".into());
            }
        }
        ImportStatus::Succeeded | ImportStatus::Partial => {
            if response.source_id.is_none()
                || response.source_version_id.is_none()
                || response.knowledge_release_id.is_none()
            {
                return Err("completed import lacks exact evidence references".into());
            }
        }
        ImportStatus::Failed | ImportStatus::Cancelled => {
            if response.source_version_id.is_some() || response.knowledge_release_id.is_some() {
                return Err("unsuccessful import cannot expose ready evidence".into());
            }
        }
    }
    Ok(())
}

/// The version of the host-op surface this crate registers.
///
/// A run records the version it was accepted against, so an operator can tell
/// which script/worker pair produced a result.
pub const HOST_OPS_VERSION: &str = "geo.hostops.v13";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCurrentRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeTextReadRequest {
    pub source_id: Uuid,
    pub source_version_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeTextReadResult {
    pub source: geo_domain::Source,
    pub source_version: geo_domain::SourceVersion,
    pub content: geo_domain::SourceVersionContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeTextReviseRequest {
    pub source_id: Uuid,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub base_version_id: Uuid,
    pub media_type: String,
    pub text: String,
}

impl KnowledgeTextReviseRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.source_id.is_nil() || self.base_version_id.is_nil() {
            return Err("source and base version IDs must be non-zero".into());
        }
        validate_project_command(self.expected_revision, &self.idempotency_key)?;
        if self.expected_revision == i64::MAX {
            return Err("revision cannot advance beyond the supported range".into());
        }
        geo_domain::ReviseSourceTextCommand {
            base_version_id: self.base_version_id,
            media_type: self.media_type.clone(),
            text: self.text.clone(),
        }
        .validate()
        .map_err(|error| error.message)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectEstimateRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCurrentResult {
    pub project: geo_domain::Project,
    pub missing_fields: Vec<String>,
    pub initial_sources_redacted: bool,
    pub initial_source_count: usize,
}

impl ProjectCurrentResult {
    pub fn validate_for(&self, scope: &TenantScope) -> Result<(), String> {
        if self.project.scope() != *scope || self.project.revision < 1 {
            return Err("project result has invalid scope or revision".into());
        }
        if !self.project.settings.initial_sources.is_empty()
            || self.initial_sources_redacted != (self.initial_source_count > 0)
        {
            return Err("project source inputs must be redacted with explicit coverage".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectReviseRequest {
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub patch: serde_json::Value,
    #[serde(default)]
    pub source_version_ids: Vec<Uuid>,
}

impl ProjectReviseRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_project_command(self.expected_revision, &self.idempotency_key)?;
        if !self.patch.is_object()
            || self.source_version_ids.len() > 100
            || self.source_version_ids.iter().any(Uuid::is_nil)
        {
            return Err("invalid project patch or source references".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectStartRequest {
    pub expected_revision: i64,
    pub idempotency_key: String,
}

impl ProjectStartRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_project_command(self.expected_revision, &self.idempotency_key)
    }
}

fn validate_project_command(revision: i64, key: &str) -> Result<(), String> {
    if revision < 1 || key.trim().is_empty() || key.len() > 200 {
        return Err("positive revision and bounded idempotency key are required".into());
    }
    Ok(())
}

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
    KnowledgeTextRead,
    KnowledgeTextRevise,
    /// Explicitly import attachments bound to this run into project knowledge.
    KnowledgeImportAttachments,
    /// Read one scoped import job's actual persisted progress.
    KnowledgeImportStatus,
    /// Read a page of a frozen document or distribution manifest.
    ManifestRead,
    /// Submit one document revision to one platform target.
    Publish,
    /// Take one independent AI channel measurement sample.
    Measure,
    /// Read an immutable scoped report snapshot.
    ReportGet,
    /// Inspect a temporary current-cycle report without saving a snapshot.
    ReportPreview,
    /// Reduce a due cycle from server-owned evidence.
    ReportReduce,
    ChannelDiscover,
    ChannelPlan,
    QuestionDiscover,
    QuestionCreate,
    QuestionRevise,
    MeasurementOptions,
    MeasurementPlanCreate,
    MeasurementPlanRead,
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
    ProjectCurrent,
    ProjectRevise,
    ProjectEstimate,
    ProjectStart,
}

impl HostOp {
    /// The number of declared capabilities.
    pub const COUNT: usize = 38;

    /// Every declared capability, in budget-array order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::ModelComplete,
        Self::KnowledgeSearch,
        Self::KnowledgeTextRead,
        Self::KnowledgeTextRevise,
        Self::KnowledgeImportAttachments,
        Self::KnowledgeImportStatus,
        Self::ManifestRead,
        Self::Publish,
        Self::Measure,
        Self::ReportGet,
        Self::ReportPreview,
        Self::ReportReduce,
        Self::ChannelDiscover,
        Self::ChannelPlan,
        Self::QuestionDiscover,
        Self::QuestionCreate,
        Self::QuestionRevise,
        Self::MeasurementOptions,
        Self::MeasurementPlanCreate,
        Self::MeasurementPlanRead,
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
        Self::ProjectCurrent,
        Self::ProjectRevise,
        Self::ProjectEstimate,
        Self::ProjectStart,
    ];

    /// The JS-visible name.  The trailing version is part of the contract.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ModelComplete => "model.complete.v1",
            Self::KnowledgeSearch => "knowledge.search.v1",
            Self::KnowledgeTextRead => "knowledge.text.read.v1",
            Self::KnowledgeTextRevise => "knowledge.text.revise.v1",
            Self::KnowledgeImportAttachments => "knowledge.import_attachments.v1",
            Self::KnowledgeImportStatus => "knowledge.import_status.v1",
            Self::ManifestRead => "manifest.read.v2",
            Self::Publish => "publish.submit.v2",
            Self::Measure => "measure.sample.v2",
            Self::ReportGet => "report.get.v1",
            Self::ReportPreview => "report.preview.v1",
            Self::ReportReduce => "report.reduce.v1",
            Self::ChannelDiscover => "channel.discover.v1",
            Self::ChannelPlan => "channel.plan.v1",
            Self::QuestionDiscover => "question.discover.v1",
            Self::QuestionCreate => "question.create.v1",
            Self::QuestionRevise => "question.revise.v1",
            Self::MeasurementOptions => "measurement.options.v1",
            Self::MeasurementPlanCreate => "measurement.plan.create.v1",
            Self::MeasurementPlanRead => "measurement.plan.read.v1",
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
            Self::ProjectCurrent => "project.current.v1",
            Self::ProjectRevise => "project.revise.v1",
            Self::ProjectEstimate => "project.estimate.v1",
            Self::ProjectStart => "project.start.v1",
        }
    }

    /// The registered isolate op that carries this capability.
    pub const fn op_name(self) -> &'static str {
        match self {
            Self::ModelComplete => "op_host_model_complete_v1",
            Self::KnowledgeSearch => "op_host_knowledge_search_v1",
            Self::KnowledgeTextRead => "op_host_knowledge_text_read_v1",
            Self::KnowledgeTextRevise => "op_host_knowledge_text_revise_v1",
            Self::KnowledgeImportAttachments => "op_host_knowledge_import_attachments_v1",
            Self::KnowledgeImportStatus => "op_host_knowledge_import_status_v1",
            Self::ManifestRead => "op_host_manifest_read_v2",
            Self::Publish => "op_host_publish_submit_v2",
            Self::Measure => "op_host_measure_sample_v2",
            Self::ReportGet => "op_host_report_get_v1",
            Self::ReportPreview => "op_host_report_preview_v1",
            Self::ReportReduce => "op_host_report_reduce_v1",
            Self::ChannelDiscover => "op_host_channel_discover_v1",
            Self::ChannelPlan => "op_host_channel_plan_v1",
            Self::QuestionDiscover => "op_host_question_discover_v1",
            Self::QuestionCreate => "op_host_question_create_v1",
            Self::QuestionRevise => "op_host_question_revise_v1",
            Self::MeasurementOptions => "op_host_measurement_options_v1",
            Self::MeasurementPlanCreate => "op_host_measurement_plan_create_v1",
            Self::MeasurementPlanRead => "op_host_measurement_plan_read_v1",
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
            Self::ProjectCurrent => "op_host_project_current_v1",
            Self::ProjectRevise => "op_host_project_revise_v1",
            Self::ProjectEstimate => "op_host_project_estimate_v1",
            Self::ProjectStart => "op_host_project_start_v1",
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
    #[serde(with = "host_op_limits_array")]
    limits: [HostOpLimits; HostOp::COUNT],
}

// Serde's built-in fixed-array implementations stop at 32 entries. Keep the
// existing array-shaped wire contract and reject truncated/extra budgets.
mod host_op_limits_array {
    use super::{HostOp, HostOpLimits};
    use serde::{Deserialize, Serialize};

    pub fn serialize<S: serde::Serializer>(
        limits: &[HostOpLimits; HostOp::COUNT],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        limits.as_slice().serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[HostOpLimits; HostOp::COUNT], D::Error> {
        Vec::<HostOpLimits>::deserialize(deserializer)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("host-op budget count does not match version"))
    }
}

impl Default for HostOpBudgets {
    fn default() -> Self {
        Self {
            limits: [
                HostOpLimits::new(120_000, 32),
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(15_000, 32), // scoped exact text read
                HostOpLimits::new(60_000, 16), // durable optimistic text revision
                HostOpLimits::new(120_000, 32),
                HostOpLimits::new(15_000, 16), // one bounded job read per call
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(60_000, 16),
                HostOpLimits::new(120_000, 32),
                HostOpLimits::new(15_000, 32),
                HostOpLimits::new(15_000, 32), // temporary report preview
                HostOpLimits::new(120_000, 4),
                HostOpLimits::new(15_000, 64),
                HostOpLimits::new(60_000, 16),
                HostOpLimits::new(15_000, 64), // metadata-only question discovery
                HostOpLimits::new(60_000, 16), // scoped question-set creation
                HostOpLimits::new(60_000, 16), // optimistic question-set revision
                HostOpLimits::new(60_000, 16), // live model menu and saved identity
                HostOpLimits::new(120_000, 16), // scoped durable standalone scheduling
                HostOpLimits::new(15_000, 64), // actual persisted target status
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
                HostOpLimits::new(15_000, 16),  // project read
                HostOpLimits::new(30_000, 8),   // project revision
                HostOpLimits::new(15_000, 16),  // project estimate
                HostOpLimits::new(60_000, 4),   // project start acceptance
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
    /// A stale source revision or an in-progress parse prevents this revision.
    Conflict,
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

    pub fn conflict(op: HostOp, message: impl Into<String>) -> Self {
        Self::new(op, HostOpErrorCode::Conflict, message)
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

    async fn knowledge_text_read(
        &self,
        _scope: &TenantScope,
        _request: KnowledgeTextReadRequest,
    ) -> Result<KnowledgeTextReadResult, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::KnowledgeTextRead,
            "knowledge text read is not configured",
        ))
    }

    async fn knowledge_text_revise(
        &self,
        _scope: &TenantScope,
        _request: KnowledgeTextReviseRequest,
    ) -> Result<geo_domain::SourceTextRevisionReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::KnowledgeTextRevise,
            "knowledge text revision is not configured",
        ))
    }

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

    async fn knowledge_import_status(
        &self,
        _scope: &TenantScope,
        _request: KnowledgeImportStatusRequest,
    ) -> Result<geo_domain::KnowledgeImportProgress, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::KnowledgeImportStatus,
            "knowledge import status is not configured",
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

    async fn report_preview(
        &self,
        _scope: &TenantScope,
        _request: ReportPreviewRequest,
    ) -> Result<ReportPreview, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ReportPreview,
            "report preview is not configured",
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

    async fn project_current(
        &self,
        _scope: &TenantScope,
        _request: ProjectCurrentRequest,
    ) -> Result<ProjectCurrentResult, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ProjectCurrent,
            "project onboarding is not configured",
        ))
    }

    async fn project_revise(
        &self,
        _scope: &TenantScope,
        _request: ProjectReviseRequest,
    ) -> Result<ProjectCurrentResult, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ProjectRevise,
            "project onboarding is not configured",
        ))
    }

    async fn project_estimate(
        &self,
        _scope: &TenantScope,
        _request: ProjectEstimateRequest,
    ) -> Result<serde_json::Value, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ProjectEstimate,
            "project onboarding is not configured",
        ))
    }

    async fn project_start(
        &self,
        _scope: &TenantScope,
        _request: ProjectStartRequest,
    ) -> Result<geo_domain::ProjectStartAcceptance, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ProjectStart,
            "project onboarding is not configured",
        ))
    }

    async fn question_discover(
        &self,
        _scope: &TenantScope,
        _request: QuestionDiscoverRequest,
    ) -> Result<QuestionDiscoveryPage, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::QuestionDiscover,
            "question-set discovery is not configured",
        ))
    }

    async fn question_create(
        &self,
        _scope: &TenantScope,
        _request: CreateQuestionSet,
    ) -> Result<QuestionWriteReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::QuestionCreate,
            "question-set creation is not configured",
        ))
    }

    async fn question_revise(
        &self,
        _scope: &TenantScope,
        _request: QuestionReviseRequest,
    ) -> Result<QuestionWriteReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::QuestionRevise,
            "question-set revision is not configured",
        ))
    }

    async fn measurement_options(
        &self,
        _scope: &TenantScope,
        _request: MeasurementOptionsRequest,
    ) -> Result<MeasurementOptionsResult, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::MeasurementOptions,
            "website model discovery is not configured",
        ))
    }

    async fn measurement_plan_create(
        &self,
        _scope: &TenantScope,
        _request: MeasurementPlanCreateRequest,
    ) -> Result<MeasurementPlanReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::MeasurementPlanCreate,
            "standalone measurement planning is not configured",
        ))
    }

    async fn measurement_plan_read(
        &self,
        _scope: &TenantScope,
        _request: MeasurementPlanReadRequest,
    ) -> Result<MeasurementPlanStatus, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::MeasurementPlanRead,
            "standalone measurement reads are not configured",
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
    /// The original send target may belong to an earlier cycle. This is a
    /// navigation reference, not evidence that the send succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_channel_target_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_lookup: Option<ChannelPublicationLookupSummary>,
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

/// The reference is resolved inside Rust; this DTO cannot carry text or a
/// caller-selected split, market or language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelBoundMeasurementPlanItem {
    pub account_id: Uuid,
    pub provider: String,
    pub model: String,
    pub surface: String,
    pub search_mode: String,
    pub protocol_version: String,
    pub question: QuestionReference,
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
    #[serde(default)]
    pub bound_measurements: Vec<ChannelBoundMeasurementPlanItem>,
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
pub struct QuestionDiscoverRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_set_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_set_version_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionDiscoveryItem {
    pub reference: QuestionReference,
    pub purpose: geo_domain::QuestionPurpose,
    /// Frozen-evaluation text is absent even when a caller explicitly names
    /// its version. Only optimization text may be used as model input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimization_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionDiscoveryPage {
    pub sets: Vec<geo_domain::QuestionSetSummary>,
    pub versions: Vec<geo_domain::QuestionSetVersionSummary>,
    pub questions: Vec<QuestionDiscoveryItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionReviseRequest {
    pub question_set_id: Uuid,
    pub command: ReviseQuestionSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionWriteReceipt {
    pub question_set_id: Uuid,
    pub question_set_version_id: Uuid,
    pub revision: u32,
    pub optimization_count: u32,
    pub evaluation_count: u32,
}

/// An account reference is resolved against the Rust-bound project and its
/// saved browser identity. No provider endpoint or session reaches the isolate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementOptionsRequest {
    pub account_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementModelOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementOptionsResult {
    pub account_id: Uuid,
    pub models: Vec<MeasurementModelOption>,
    pub selected_model: Option<String>,
}

impl MeasurementOptionsResult {
    pub fn validate_for(&self, request: &MeasurementOptionsRequest) -> Result<(), String> {
        let mut ids = HashSet::new();
        if self.account_id != request.account_id
            || self.models.is_empty()
            || self.models.len() > 64
            || self.models.iter().any(|model| {
                model.id.is_empty()
                    || model.id.len() > 128
                    || !model
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
                    || model.label.trim().is_empty()
                    || model.label.chars().count() > 200
                    || model.label.chars().any(char::is_control)
                    || !ids.insert(&model.id)
            })
            || self
                .selected_model
                .as_ref()
                .is_some_and(|id| !ids.contains(id))
        {
            return Err("measurement options are not a scoped observed model menu".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPlanCreateRequest {
    pub account_id: Uuid,
    pub question: String,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl MeasurementPlanCreateRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.account_id.is_nil()
            || self.question.trim().is_empty()
            || self.question.len() > 4000
            || self.idempotency_key.trim().is_empty()
            || self.idempotency_key.len() > 200
            || self
                .model
                .as_ref()
                .is_some_and(|model| model.is_empty() || model.len() > 128)
        {
            return Err("measurement account, bounded question and stable key are required".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPlanReceipt {
    pub plan_id: Uuid,
    pub target_id: Uuid,
    pub account_id: Uuid,
    pub model: String,
    pub state: String,
}

impl MeasurementPlanReceipt {
    pub fn validate_for(&self, request: &MeasurementPlanCreateRequest) -> Result<(), String> {
        if self.plan_id.is_nil()
            || self.target_id.is_nil()
            || self.account_id != request.account_id
            || self.model.is_empty()
            || self.state != "accepted"
            || request
                .model
                .as_ref()
                .is_some_and(|model| model != &self.model)
        {
            return Err("measurement plan acceptance lacks matching durable references".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPlanReadRequest {
    pub plan_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementTargetStatus {
    pub target_id: Uuid,
    pub state: String,
    pub surface: String,
    pub outcome_status: Option<geo_domain::ChannelOutcomeStatus>,
    pub fixture: Option<bool>,
    pub received_at: Option<DateTime<Utc>>,
    /// Exact live, ad-hoc answer when it fits in the bounded tool response.
    /// Otherwise the target ID points to the full scoped result detail.
    pub answer: Option<String>,
    pub answer_available: bool,
    pub citations: Vec<String>,
    pub citations_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPlanStatus {
    pub plan_id: Uuid,
    pub targets: Vec<MeasurementTargetStatus>,
}

impl MeasurementPlanStatus {
    pub fn validate_for(&self, request: &MeasurementPlanReadRequest) -> Result<(), String> {
        let mut ids = HashSet::new();
        if self.plan_id != request.plan_id
            || self.targets.is_empty()
            || self.targets.len() > 100
            || self.targets.iter().any(|target| {
                target.target_id.is_nil()
                    || !ids.insert(target.target_id)
                    || !matches!(target.state.as_str(), "queued" | "attempting" | "completed")
                    || target.surface != "consumer_web"
                    || (target.state == "completed") != target.outcome_status.is_some()
                    || target.outcome_status.is_some() != target.fixture.is_some()
                    || target.outcome_status.is_some() != target.received_at.is_some()
                    || target
                        .answer
                        .as_ref()
                        .is_some_and(|answer| answer.len() > 16 * 1024)
                    || target.answer.is_some() && !target.answer_available
                    || !target.citations.is_empty() && !target.citations_available
                    || target.citations.len() > 50
                    || target.citations.iter().any(|url| url.len() > 2048)
                    || (target.answer_available || target.citations_available)
                        && (target.fixture != Some(false)
                            || !matches!(
                                target.outcome_status,
                                Some(
                                    geo_domain::ChannelOutcomeStatus::Observed
                                        | geo_domain::ChannelOutcomeStatus::Refused
                                )
                            ))
            })
        {
            return Err("measurement plan status is invalid or unrelated".into());
        }
        Ok(())
    }
}

impl QuestionDiscoverRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.question_set_id.is_some_and(|id| id.is_nil())
            || self.question_set_version_id.is_some_and(|id| id.is_nil())
            || (self.question_set_version_id.is_some() && self.question_set_id.is_none())
            || self.limit.is_some_and(|limit| !(1..=100).contains(&limit))
            || self
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.is_empty() || cursor.len() > 64)
        {
            return Err("invalid question discovery selector or page limit".into());
        }
        Ok(())
    }
}

impl QuestionDiscoveryPage {
    pub fn validate_for(&self, request: &QuestionDiscoverRequest) -> Result<(), String> {
        let limit = request.limit.unwrap_or(25) as usize;
        if self.sets.len() + self.versions.len() + self.questions.len() > limit
            || self
                .next_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.is_empty() || cursor.len() > 64)
            || self.sets.iter().any(|item| {
                item.id.is_nil()
                    || item.current_version_id.is_nil()
                    || item.current_revision == 0
                    || item.question_count != item.optimization_count + item.evaluation_count
                    || item.question_count > 100
            })
            || self.versions.iter().any(|item| {
                item.id.is_nil()
                    || Some(item.question_set_id) != request.question_set_id
                    || item.revision == 0
                    || item.optimization_count + item.evaluation_count > 100
            })
            || (request.question_set_id.is_none()
                && (!self.versions.is_empty() || !self.questions.is_empty()))
            || (request.question_set_id.is_some()
                && request.question_set_version_id.is_none()
                && (!self.sets.is_empty() || !self.questions.is_empty()))
            || (request.question_set_version_id.is_some()
                && (!self.sets.is_empty() || !self.versions.is_empty()))
            || self.questions.iter().any(|item| {
                item.reference.question_set_id != request.question_set_id.unwrap_or_default()
                    || item.reference.question_set_version_id
                        != request.question_set_version_id.unwrap_or_default()
                    || item.reference.question_id.is_nil()
                    || item.reference.question_revision_id.is_nil()
                    || match item.purpose {
                        geo_domain::QuestionPurpose::FrozenEvaluation => {
                            item.optimization_text.is_some()
                        }
                        geo_domain::QuestionPurpose::Optimization => {
                            item.optimization_text.as_ref().is_none_or(|text| {
                                text.trim().is_empty()
                                    || text.len() > MAX_MEASUREMENT_QUESTION_BYTES
                            })
                        }
                    }
            })
        {
            return Err("question discovery returned invalid or unsafe fields".into());
        }
        Ok(())
    }
}

impl QuestionWriteReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.question_set_id.is_nil()
            || self.question_set_version_id.is_nil()
            || self.revision == 0
            || self.optimization_count + self.evaluation_count == 0
            || self.optimization_count + self.evaluation_count > 100
        {
            return Err("question version receipt is invalid".into());
        }
        Ok(())
    }
}

impl QuestionReviseRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.question_set_id.is_nil() || self.command.base_version_id.is_nil() {
            return Err("question-set and base-version references must be nonzero".into());
        }
        validate_question_command(
            &self.command.idempotency_key,
            &self.command.name,
            &self.command.questions,
        )
    }
}

pub fn validate_question_create(request: &CreateQuestionSet) -> Result<(), String> {
    validate_question_command(&request.idempotency_key, &request.name, &request.questions)
}

fn validate_question_command(
    key: &str,
    name: &str,
    questions: &[geo_domain::QuestionDraft],
) -> Result<(), String> {
    if key.trim().is_empty()
        || key.len() > 256
        || name.trim().is_empty()
        || name.len() > 512
        || questions.is_empty()
        || questions.len() > 100
        || questions.iter().any(|item| {
            item.text.trim().is_empty()
                || item.text.len() > MAX_MEASUREMENT_QUESTION_BYTES
                || item.intent.trim().is_empty()
                || item.market.trim().is_empty()
                || item.language.trim().is_empty()
                || item.product_refs.len() > 100
                || item.product_refs.iter().any(Uuid::is_nil)
        })
    {
        return Err("question-set request is invalid or exceeds limits".into());
    }
    Ok(())
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
            || self.publications.len() + self.measurements.len() + self.bound_measurements.len()
                == 0
            || self.publications.len() + self.measurements.len() + self.bound_measurements.len()
                > 100
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
            || self.bound_measurements.iter().any(|item| {
                item.account_id.is_nil()
                    || item.question.question_set_id.is_nil()
                    || item.question.question_set_version_id.is_nil()
                    || item.question.question_id.is_nil()
                    || item.question.question_revision_id.is_nil()
                    || [
                        &item.provider,
                        &item.model,
                        &item.surface,
                        &item.search_mode,
                        &item.protocol_version,
                    ]
                    .iter()
                    .any(|value| !channel_label_valid(value))
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
                != (request.publications.len()
                    + request.measurements.len()
                    + request.bound_measurements.len()) as u64
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

/// A read-only projection for the current project cycle by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportPreviewRequest {
    #[serde(default)]
    pub cycle_id: Option<Uuid>,
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
pub struct KnowledgeImportStatusRequest {
    pub import_job_id: Uuid,
    pub purpose: KnowledgePurpose,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_job_id: Option<Uuid>,
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
    recorder: Option<(geo_domain::RunId, Arc<dyn ToolCallRecorder>)>,
    invocation_sequence: Arc<AtomicU64>,
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
            recorder: None,
            invocation_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Binds a durable recorder to the Rust-owned run; scripts cannot choose
    /// either this run identity or the sequence assigned to their invocations.
    pub fn with_recorder(
        mut self,
        run_id: geo_domain::RunId,
        recorder: Arc<dyn ToolCallRecorder>,
    ) -> Self {
        self.recorder = Some((run_id, recorder));
        self
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

    /// Production entry point: all recorder transitions, capability work and
    /// typed return validation run on the application's I/O runtime. This
    /// spawned task owns finalization even if the isolate drops its await.
    pub async fn invoke_recorded<R, T, F, W>(
        &self,
        op: HostOp,
        request: &R,
        work: F,
    ) -> Result<T, HostOpError>
    where
        R: Serialize,
        T: Serialize + Send + 'static,
        F: FnOnce(HostBridge) -> W + Send + 'static,
        W: Future<Output = Result<T, HostOpError>> + Send + 'static,
    {
        let limits = self.budgets.limits(op);
        let canonical = serde_json::to_value(request)
            .and_then(|value| serde_json::to_vec(&value))
            .map_err(|_| HostOpError::invalid_request(op, "request cannot be encoded"))?;
        let request_value: serde_json::Value = serde_json::from_slice(&canonical)
            .map_err(|_| HostOpError::invalid_request(op, "request cannot be encoded"))?;
        // Standalone content execution and compatibility probes predate the
        // Agent run ledger and have their own persistence boundary. The Agent
        // assembler installs the recorder on its per-run bridge.
        let Some((run_id, recorder)) = self.recorder.as_ref() else {
            return self
                .invoke(op, move |bridge| async move {
                    let scope = bridge.scope().clone();
                    let value = work(bridge).await?;
                    let actual = serde_json::to_value(&value).map_err(|_| {
                        HostOpError::internal(op, "capability response cannot be encoded")
                    })?;
                    validate_recorded_return(op, &scope, &request_value, &actual)?;
                    Ok(value)
                })
                .await;
        };
        self.meter.reserve(op, limits)?;
        if self.cancellation.load(Ordering::SeqCst) {
            return Err(HostOpError::cancelled(op));
        }
        let sequence = self.invocation_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let call_id = format!("rust:{sequence}");
        let idempotency_material = format!("geo.tool_call.v1|{run_id}|{call_id}");
        let identity = ToolCallIdentity {
            run_id: *run_id,
            tool_call_id: call_id,
            tool_name: op.name().to_owned(),
            arguments_hash: geo_domain::sha256_hex(&canonical),
            idempotency_key_hash: geo_domain::sha256_hex(idempotency_material.as_bytes()),
        };
        let recorder = Arc::clone(recorder);
        let bridge = self.clone();
        let scope = self.scope.clone();
        let cancellation = Arc::clone(&self.cancellation);
        self.executor
            .spawn(async move {
                let deadline = tokio::time::Instant::now() + limits.timeout();
                if cancellation.load(Ordering::SeqCst) {
                    return Err(HostOpError::cancelled(op));
                }
                let created = tokio::time::timeout_at(deadline, recorder.begin(&identity))
                    .await
                    .map_err(|_| HostOpError::deadline_exceeded(op, limits.timeout_ms))??;
                if !created {
                    return Err(HostOpError::idempotency_conflict(
                        op,
                        "tool-call identity already exists; reconcile instead of re-executing",
                    ));
                }
                if cancellation.load(Ordering::SeqCst) {
                    return Err(HostOpError::cancelled(op));
                }
                let attempted = tokio::time::timeout_at(deadline, recorder.attempt(&identity))
                    .await
                    .map_err(|_| HostOpError::deadline_exceeded(op, limits.timeout_ms))??;
                if !attempted {
                    return Err(HostOpError::unknown_result(
                        op,
                        "tool-call already attempted; reconcile instead of re-executing",
                    ));
                }
                if cancellation.load(Ordering::SeqCst) || tokio::time::Instant::now() >= deadline {
                    tokio::time::timeout(
                        limits.timeout(),
                        recorder.finish(&identity, ToolCallOutcome::Unknown),
                    )
                    .await
                    .map_err(|_| {
                        HostOpError::unknown_result(op, "tool-call outcome write timed out")
                    })??;
                    return Err(HostOpError::unknown_result(
                        op,
                        "invocation expired or cancelled after attempt claim",
                    ));
                }
                // A nested task catches capability panics without aborting the
                // outer ledger-finalization task. Abort it when the deadline
                // or cancellation wins, then persist the uncertain result.
                let mut capability = tokio::spawn(async move { work(bridge).await });
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                let execution = under_budget(
                    op,
                    HostOpLimits::new(
                        remaining.as_millis().try_into().unwrap_or(u64::MAX),
                        limits.max_calls,
                    ),
                    cancellation,
                    async {
                        (&mut capability).await.map_err(|_| {
                            HostOpError::unknown_result(op, "capability panicked after attempt")
                        })?
                    },
                )
                .await;
                if !capability.is_finished() {
                    capability.abort();
                }
                let validated = execution.and_then(|value| {
                    let actual = serde_json::to_value(&value).map_err(|_| {
                        if matches!(
                            op,
                            HostOp::Publish | HostOp::Measure | HostOp::ChannelTargetExecute
                        ) {
                            HostOpError::unknown_result(
                                op,
                                "external capability response cannot be encoded",
                            )
                        } else {
                            HostOpError::internal(op, "capability response cannot be encoded")
                        }
                    })?;
                    let unknown = validate_recorded_return(op, &scope, &request_value, &actual)?;
                    Ok((value, unknown))
                });
                let outcome = match &validated {
                    Ok((_, true)) => ToolCallOutcome::Unknown,
                    Ok((_, false)) => ToolCallOutcome::Succeeded,
                    Err(error)
                        if matches!(
                            error.code,
                            HostOpErrorCode::UnknownResult
                                | HostOpErrorCode::DeadlineExceeded
                                | HostOpErrorCode::Cancelled
                        ) =>
                    {
                        ToolCallOutcome::Unknown
                    }
                    Err(_) => ToolCallOutcome::Failed,
                };
                // The capability's deadline must not consume the time needed
                // to persist Unknown after an attempted external effect.
                tokio::time::timeout(limits.timeout(), recorder.finish(&identity, outcome))
                    .await
                    .map_err(|_| {
                        HostOpError::unknown_result(op, "tool-call outcome write timed out")
                    })??;
                validated.map(|(value, _)| value)
            })
            .await
            .map_err(|_| HostOpError::unknown_result(op, "tool-call recorder task stopped"))?
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

    #[test]
    fn standalone_measurement_surface_rejects_protocol_injection_and_false_success() {
        let account_id = Uuid::new_v4();
        let plan_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let command = MeasurementPlanCreateRequest {
            account_id,
            question: "How do rain gauges work?".into(),
            idempotency_key: "stable-question".into(),
            model: None,
        };
        assert!(command.validate().is_ok());
        let mut injected = serde_json::to_value(&command).unwrap();
        injected["cycle_id"] = serde_json::json!(Uuid::new_v4());
        assert!(serde_json::from_value::<MeasurementPlanCreateRequest>(injected).is_err());
        let receipt = MeasurementPlanReceipt {
            plan_id,
            target_id,
            account_id,
            model: "observed-model".into(),
            state: "accepted".into(),
        };
        assert!(receipt.validate_for(&command).is_ok());
        let mut forged = receipt.clone();
        forged.state = "succeeded".into();
        assert!(forged.validate_for(&command).is_err());
        let requested = MeasurementPlanReadRequest { plan_id };
        let status = MeasurementPlanStatus {
            plan_id,
            targets: vec![MeasurementTargetStatus {
                target_id,
                state: "queued".into(),
                surface: "consumer_web".into(),
                outcome_status: None,
                fixture: None,
                received_at: None,
                answer: None,
                answer_available: false,
                citations: vec![],
                citations_available: false,
            }],
        };
        assert!(status.validate_for(&requested).is_ok());
        let mut forged_status = status.clone();
        forged_status.targets[0].outcome_status = Some(geo_domain::ChannelOutcomeStatus::Observed);
        assert!(forged_status.validate_for(&requested).is_err());
        let mut evidence_pointer = status.clone();
        evidence_pointer.targets[0].state = "completed".into();
        evidence_pointer.targets[0].outcome_status =
            Some(geo_domain::ChannelOutcomeStatus::Observed);
        evidence_pointer.targets[0].fixture = Some(false);
        evidence_pointer.targets[0].received_at = Some(Utc::now());
        evidence_pointer.targets[0].answer_available = true;
        assert!(evidence_pointer.validate_for(&requested).is_ok());
        evidence_pointer.targets[0].fixture = Some(true);
        assert!(evidence_pointer.validate_for(&requested).is_err());
        let mut leaked = serde_json::to_value(status).unwrap();
        leaked["targets"][0]["raw_answer"] = serde_json::json!("evaluation text");
        assert!(serde_json::from_value::<MeasurementPlanStatus>(leaked).is_err());
    }

    #[test]
    fn import_progress_never_admits_unready_release_or_incomplete_success() {
        let job = Uuid::new_v4();
        let mut progress = geo_domain::KnowledgeImportProgress {
            import_job_id: Some(job),
            status: ImportStatus::Queued,
            stage: None,
            source_id: Some(Uuid::new_v4()),
            source_version_id: None,
            knowledge_release_id: None,
            completed_units: 0,
            failed_units: 0,
            error_count: 0,
            errors: vec![],
        };
        assert!(validate_import_status(&progress, job).is_ok());
        assert!(validate_import_status(&progress, Uuid::new_v4()).is_err());
        progress.knowledge_release_id = Some(Uuid::new_v4());
        for status in [
            ImportStatus::Queued,
            ImportStatus::Running,
            ImportStatus::Failed,
            ImportStatus::Cancelled,
        ] {
            progress.status = status;
            assert!(validate_import_status(&progress, job).is_err());
        }
        progress.status = ImportStatus::Partial;
        assert!(validate_import_status(&progress, job).is_err());
        progress.source_version_id = Some(Uuid::new_v4());
        assert!(validate_import_status(&progress, job).is_ok());
    }
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
    fn expanded_host_budgets_round_trip_and_reject_missing_slots() {
        let budgets = HostOpBudgets::default();
        let mut value = serde_json::to_value(budgets).unwrap();
        let restored: HostOpBudgets = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(restored.limits(HostOp::ProjectStart).max_calls, 4);
        value["limits"].as_array_mut().unwrap().pop();
        assert!(serde_json::from_value::<HostOpBudgets>(value).is_err());
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
    fn bound_plan_rejects_forged_question_text_purpose_and_scope() {
        let reference = serde_json::json!({
            "question_set_id": Uuid::new_v4(),
            "question_set_version_id": Uuid::new_v4(),
            "question_id": Uuid::new_v4(),
            "question_revision_id": Uuid::new_v4(),
        });
        let item = serde_json::json!({
            "account_id": Uuid::new_v4(),
            "provider": "kimi",
            "model": "model",
            "surface": "consumer_web",
            "search_mode": "web_search",
            "protocol_version": "v1",
            "question": reference,
            "scheduled_at": "2026-10-06T00:00:00Z",
            "sample_ordinal": 0,
        });
        let request = serde_json::json!({
            "publications": [], "measurements": [], "bound_measurements": [item],
        });
        assert!(
            serde_json::from_value::<ChannelPlanRequest>(request.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for key in [
            "text",
            "market",
            "language",
            "purpose",
            "evaluation_split",
            "project_id",
            "tenant_id",
        ] {
            let mut forged = request.clone();
            forged["bound_measurements"][0][key] = serde_json::json!("forged");
            assert!(
                serde_json::from_value::<ChannelPlanRequest>(forged).is_err(),
                "{key} must be rejected"
            );
        }
        let mut forged = request.clone();
        forged["bound_measurements"][0]["question"]["purpose"] = serde_json::json!("optimization");
        assert!(serde_json::from_value::<ChannelPlanRequest>(forged).is_err());
        let mut forged = request;
        forged["bound_measurements"][0]["question"]["question_id"] = serde_json::json!(Uuid::nil());
        assert!(
            serde_json::from_value::<ChannelPlanRequest>(forged)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn heldout_discovery_cannot_claim_optimization_text() {
        let set = Uuid::new_v4();
        let version = Uuid::new_v4();
        let request = QuestionDiscoverRequest {
            question_set_id: Some(set),
            question_set_version_id: Some(version),
            cursor: None,
            limit: None,
        };
        let mut page = QuestionDiscoveryPage {
            sets: vec![],
            versions: vec![],
            questions: vec![QuestionDiscoveryItem {
                reference: QuestionReference {
                    question_set_id: set,
                    question_set_version_id: version,
                    question_id: Uuid::new_v4(),
                    question_revision_id: Uuid::new_v4(),
                },
                purpose: geo_domain::QuestionPurpose::FrozenEvaluation,
                optimization_text: None,
            }],
            next_cursor: None,
        };
        assert!(page.validate_for(&request).is_ok());
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("optimization_text")
        );
        page.questions[0].optimization_text = Some("HELDOUT_CANARY".into());
        assert!(page.validate_for(&request).is_err());
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
