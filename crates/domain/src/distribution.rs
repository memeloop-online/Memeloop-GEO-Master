//! Frozen second-stage coverage. The legacy channel plan is not a distribution manifest.
//! Persistence must commit targets, variants, intents and commands in ONE transaction.
use crate::{
    AppError, ContentExecution, ContentExecutionStatus, ContentHandoff, ContentItemStatus,
    ContentRevision, DocumentManifest, EvidenceRef, ProjectId, ReportEvidenceReference,
    ReportPublicationStatus, TenantScope,
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;
use uuid::Uuid;

pub const CHANNEL_VARIANT_POLICY: &str = "deterministic-markdown-v1";
pub const RICH_CHANNEL_VARIANT_POLICY: &str = "deterministic-rich-markdown-v2";
pub const RICH_MARKDOWN_FORMAT: &str = "rich_markdown.v2";

fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn identity(parts: &[&str]) -> Uuid {
    let hash = digest(parts);
    let bytes = hex::decode(&hash[..32]).expect("sha256 hex");
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes);
    id[6] = (id[6] & 0x0f) | 0x50;
    id[8] = (id[8] & 0x3f) | 0x80;
    Uuid::from_bytes(id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformPlacement {
    pub platform_id: String,
    /// Only the primary placement is covered. Extra accounts do not multiply coverage.
    pub placement_slot: String,
    pub capability_version: String,
    pub supported_formats: Vec<String>,
    pub unavailable_reason: Option<String>,
    /// A fixture is never evidence of a real connector.
    pub fixture: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionManifest {
    pub manifest_id: Uuid,
    pub project_id: ProjectId,
    pub cycle_id: Uuid,
    pub revision: i32,
    pub document_manifest_id: Uuid,
    pub document_manifest_revision: i32,
    pub content_execution_id: Uuid,
    pub content_handoff_id: Uuid,
    pub platform_scope: Vec<PlatformPlacement>,
    pub document_roster: Vec<DistributionDocument>,
    pub input_hash: String,
    pub expected_count: u64,
    pub sealed_at: DateTime<Utc>,
    /// Number of materialized coverage cells, not the number of publishable cells.
    pub expansion_cursor: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionDocument {
    pub document_item_id: Uuid,
    pub document_key: String,
    pub content_type: String,
    pub status: ContentItemStatus,
    pub reason: Option<String>,
    pub content_revision_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistributionTargetStatus {
    Pending,
    Blocked,
    Deferred,
    NotApplicable,
    Cancelled,
    Ready,
    ReusedVerified,
    ReusedUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionTarget {
    pub target_id: Uuid,
    pub manifest_id: Uuid,
    pub ordinal: u64,
    pub document_item_id: Uuid,
    pub content_revision_id: Option<Uuid>,
    pub platform_id: String,
    pub placement_slot: String,
    pub variant_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub publication_intent_id: Option<Uuid>,
    pub status: DistributionTargetStatus,
    pub reason: Option<String>,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelVariant {
    pub variant_id: Uuid,
    pub content_revision_id: Uuid,
    pub platform_id: String,
    pub placement_slot: String,
    pub policy_version: String,
    pub title: String,
    pub markdown: String,
    pub payload_hash: String,
    pub evidence: Vec<EvidenceRef>,
}

/// A logical project-scoped asset; cycle and coverage target are deliberately
/// absent from its identity. An unknown earlier send must not be retried as new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationIntent {
    pub intent_id: Uuid,
    pub project_id: ProjectId,
    pub channel_target_id: Uuid,
    pub variant_id: Uuid,
    pub content_revision_id: Uuid,
    pub platform_id: String,
    pub placement_slot: String,
    pub account_id: Uuid,
    pub payload_hash: String,
    pub logical_key: String,
    pub verification: IntentVerification,
    pub verification_evidence_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentVerification {
    Unverified,
    Unknown,
    Verified,
}

/// This is a durable outbox record, not proof of a send. The dispatcher must
/// claim it and write an attempt before performing any external side effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationCommand {
    pub command_id: Uuid,
    pub intent_id: Uuid,
    pub target_id: Uuid,
    pub payload_hash: String,
    pub fixture: bool,
}

/// Trusted read of the immutable payload and its authoritative dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationBundle {
    pub revision: ContentRevision,
    pub variant: ChannelVariant,
    pub intent: PublicationIntent,
    pub target: DistributionTarget,
    pub command: PublicationCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreezeDistribution {
    pub cycle_id: Uuid,
    pub revision: i32,
    pub document_manifest: DocumentManifest,
    pub content_execution: ContentExecution,
    pub content_handoff: ContentHandoff,
    pub placements: Vec<PlatformPlacement>,
    pub sealed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionExpansionPage {
    pub manifest_id: Uuid,
    pub cursor: u64,
    pub next_cursor: u64,
    pub rows: Vec<DistributionTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionTargetPage {
    pub manifest_id: Uuid,
    pub rows: Vec<DistributionTarget>,
    pub next_ordinal: Option<u64>,
    pub expected_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedDistribution {
    pub manifest_id: Uuid,
    pub target_id: Uuid,
    /// Required to bind an exact immutable variant; not needed for blocked cells.
    pub revision: Option<ContentRevision>,
    pub account_id: Option<Uuid>,
    /// Current source/format eligibility is checked by the trusted service,
    /// independently of the frozen coverage denominator.
    #[serde(default)]
    pub defer_reason: Option<DistributionDeferralReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistributionDeferralReason {
    SourceUnavailable,
    SourceChanged,
    ContentUnsupported,
}

impl DistributionDeferralReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "source_unavailable",
            Self::SourceChanged => "source_changed",
            Self::ContentUnsupported => "content_unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedDistribution {
    pub target: DistributionTarget,
    pub variant: Option<ChannelVariant>,
    pub intent: Option<PublicationIntent>,
    pub publication_commands: Vec<PublicationCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionTargetVersion {
    pub recorded_at: DateTime<Utc>,
    pub target: DistributionTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionSnapshot {
    pub manifest: DistributionManifest,
    pub targets: Vec<DistributionTarget>,
    pub as_of: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionCycleInputs {
    pub manifest: Option<DistributionManifest>,
    pub targets: Vec<DistributionTarget>,
    /// The reducer must not call missing cells successful. True only if all
    /// expected cells were committed at or before the requested cutoff.
    pub temporally_complete: bool,
}

/// A report result associated with one frozen coverage cell, not merely the
/// logical intent (which may be reused by several cells across cycles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributionPublicationResult {
    pub target_id: Uuid,
    pub status: ReportPublicationStatus,
    pub reason: Option<String>,
    pub evidence: Vec<ReportEvidenceReference>,
}

/// The receipt keeps its original timestamps, but each target has a distinct
/// evidence identity so the report reducer cannot merge two coverage cells.
pub fn target_publication_evidence(
    target_id: Uuid,
    intent_id: Uuid,
    source_evidence_id: Uuid,
    kind: &str,
    occurred_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> ReportEvidenceReference {
    ReportEvidenceReference {
        evidence_id: identity(&[
            "distribution-target-evidence-v1",
            &target_id.to_string(),
            &intent_id.to_string(),
            &source_evidence_id.to_string(),
            kind,
        ]),
        kind: kind.into(),
        resource_id: target_id,
        resource_version: Some(format!("intent:{intent_id}:receipt:{source_evidence_id}")),
        occurred_at: Some(occurred_at),
        received_at: Some(received_at),
        summary: if kind == "public_verification" {
            "Verified public readback of the associated publication intent".into()
        } else {
            "Published receipt of the associated publication intent".into()
        },
    }
}

pub fn prepare_variant(
    revision: &ContentRevision,
    placement: &PlatformPlacement,
) -> Result<ChannelVariant, AppError> {
    if revision.document.schema_version == Some(2) {
        return Err(AppError::invalid_request(
            "existing publication outbox cannot publish rich content",
        ));
    }
    prepare_versioned_variant(revision, placement, CHANNEL_VARIANT_POLICY)
}

/// Prepares only the rich-format payload; callers must separately authorize
/// publication format, media and the entire downstream connector/send path.
/// The existing outbox intentionally does not call this function.
pub fn prepare_rich_variant(
    revision: &ContentRevision,
    placement: &PlatformPlacement,
) -> Result<ChannelVariant, AppError> {
    if revision.document.schema_version != Some(2)
        || !placement
            .supported_formats
            .iter()
            .any(|f| f == RICH_MARKDOWN_FORMAT)
    {
        return Err(AppError::invalid_request(
            "rich content requires an explicitly supported rich publication format",
        ));
    }
    prepare_versioned_variant(revision, placement, RICH_CHANNEL_VARIANT_POLICY)
}
fn prepare_versioned_variant(
    revision: &ContentRevision,
    placement: &PlatformPlacement,
    policy: &str,
) -> Result<ChannelVariant, AppError> {
    revision.document.validate(&revision.evidence)?;
    if !revision.document.media_references().is_empty() {
        return Err(AppError::invalid_request(
            "media publication requires a media-capable channel adapter",
        ));
    }
    if revision.markdown != revision.document.markdown() {
        return Err(AppError::invalid_request(
            "content revision markdown is inconsistent",
        ));
    }
    let title = revision.document.title.clone();
    let markdown = revision.markdown.clone();
    let payload_hash = digest(&[&title, &markdown]);
    let variant_id = identity(&[
        &revision.revision_id.to_string(),
        &placement.platform_id,
        &placement.placement_slot,
        policy,
        &payload_hash,
    ]);
    Ok(ChannelVariant {
        variant_id,
        content_revision_id: revision.revision_id,
        platform_id: placement.platform_id.clone(),
        placement_slot: placement.placement_slot.clone(),
        policy_version: policy.into(),
        title,
        markdown,
        payload_hash,
        evidence: revision.evidence.clone(),
    })
}

fn check_scope(scope: &TenantScope, project: ProjectId) -> Result<(), AppError> {
    if scope.project_id == Some(project) {
        Ok(())
    } else {
        Err(AppError::forbidden("distribution requires project scope"))
    }
}

pub fn freeze_distribution(
    scope: &TenantScope,
    input: FreezeDistribution,
) -> Result<DistributionManifest, AppError> {
    let doc = &input.document_manifest;
    let execution = &input.content_execution;
    let handoff = &input.content_handoff;
    check_scope(scope, doc.project_id)?;
    if scope.operator_id != doc.operator_id || scope.tenant_id != doc.tenant_id {
        return Err(AppError::forbidden(
            "document manifest is outside tenant scope",
        ));
    }
    if !doc.sealed
        || doc.expected_count != Some(doc.items.len() as i64)
        || input.revision <= 0
        || input.placements.is_empty()
        || execution.project_id != doc.project_id
        || execution.cycle_id != input.cycle_id
        || execution.manifest_id != doc.manifest_id
        || execution.manifest_revision != doc.revision
        || execution.status != ContentExecutionStatus::Closed
        || execution.expected_count != doc.items.len() as u64
        || execution.handoff_id != Some(handoff.handoff_id)
        || handoff.execution_id != execution.execution_id
        || handoff.coverage.total != doc.items.len() as u64
        || handoff.items.len() != doc.items.len()
    {
        return Err(AppError::invalid_request(
            "distribution inputs are not a closed matching roster",
        ));
    }
    let counts = (
        handoff
            .items
            .iter()
            .filter(|item| item.status == ContentItemStatus::Ready)
            .count() as u64,
        handoff
            .items
            .iter()
            .filter(|item| item.status == ContentItemStatus::Blocked)
            .count() as u64,
        handoff
            .items
            .iter()
            .filter(|item| item.status == ContentItemStatus::Deferred)
            .count() as u64,
        handoff
            .items
            .iter()
            .filter(|item| item.status == ContentItemStatus::NotApplicable)
            .count() as u64,
        handoff
            .items
            .iter()
            .filter(|item| item.status == ContentItemStatus::Cancelled)
            .count() as u64,
    );
    if handoff.coverage != execution.coverage
        || handoff.coverage.incomplete != 0
        || counts
            != (
                handoff.coverage.ready,
                handoff.coverage.blocked,
                handoff.coverage.deferred,
                handoff.coverage.not_applicable,
                handoff.coverage.cancelled,
            )
    {
        return Err(AppError::invalid_request(
            "handoff coverage does not match terminal item states",
        ));
    }
    let mut placements = input.placements;
    let mut placement_keys = HashSet::new();
    for place in &placements {
        if place.platform_id.trim().is_empty()
            || place.placement_slot.trim().is_empty()
            || place.capability_version.trim().is_empty()
            || !placement_keys.insert((place.platform_id.clone(), place.placement_slot.clone()))
        {
            return Err(AppError::invalid_request(
                "invalid or duplicate platform placement",
            ));
        }
    }
    placements.sort_by(|a, b| {
        (&a.platform_id, &a.placement_slot).cmp(&(&b.platform_id, &b.placement_slot))
    });
    let mut roster = Vec::with_capacity(doc.items.len());
    let mut handoff_by_id = HashMap::with_capacity(handoff.items.len());
    for item in &handoff.items {
        if handoff_by_id.insert(item.item_id, item).is_some() {
            return Err(AppError::invalid_request(
                "handoff repeats document identity",
            ));
        }
    }
    for planned in &doc.items {
        let item = handoff_by_id
            .remove(&planned.document_manifest_item_id)
            .ok_or_else(|| AppError::invalid_request("handoff misses planned document"))?;
        if item.document_key != planned.document_key
            || (item.status == ContentItemStatus::Ready) != item.revision_id.is_some()
        {
            return Err(AppError::invalid_request("handoff item is inconsistent"));
        }
        roster.push(DistributionDocument {
            document_item_id: item.item_id,
            document_key: item.document_key.clone(),
            content_type: planned.content_type.clone(),
            status: item.status,
            reason: item.reason.clone(),
            content_revision_id: item.revision_id,
        });
    }
    let expected_count = (roster.len() as u64)
        .checked_mul(placements.len() as u64)
        .ok_or_else(|| AppError::invalid_request("distribution coverage exceeds capacity"))?;
    let manifest_id = identity(&[
        &scope.storage_key(),
        &input.cycle_id.to_string(),
        &input.revision.to_string(),
        &doc.manifest_id.to_string(),
        &doc.revision.to_string(),
    ]);
    let frozen =
        serde_json::to_string(&(doc, &roster, &placements, &execution.input_hash, handoff))
            .map_err(|_| AppError::invalid_request("distribution inputs cannot be serialized"))?;
    Ok(DistributionManifest {
        manifest_id,
        project_id: doc.project_id,
        cycle_id: input.cycle_id,
        revision: input.revision,
        document_manifest_id: doc.manifest_id,
        document_manifest_revision: doc.revision,
        content_execution_id: execution.execution_id,
        content_handoff_id: handoff.handoff_id,
        platform_scope: placements,
        document_roster: roster,
        input_hash: digest(&[&scope.storage_key(), &frozen]),
        expected_count,
        sealed_at: input.sealed_at,
        expansion_cursor: 0,
        complete: expected_count == 0,
    })
}

/// Pure, deterministic covered-cell identity. Callers must check
/// `ordinal < manifest.expected_count` before invoking.
pub fn distribution_cell(manifest: &DistributionManifest, ordinal: u64) -> DistributionTarget {
    let width = manifest.platform_scope.len();
    let document = &manifest.document_roster[ordinal as usize / width];
    let placement = &manifest.platform_scope[ordinal as usize % width];
    let (status, reason) = match document.status {
        ContentItemStatus::Blocked => (DistributionTargetStatus::Blocked, document.reason.clone()),
        ContentItemStatus::Deferred => {
            (DistributionTargetStatus::Deferred, document.reason.clone())
        }
        ContentItemStatus::NotApplicable => (
            DistributionTargetStatus::NotApplicable,
            document.reason.clone(),
        ),
        ContentItemStatus::Cancelled => {
            (DistributionTargetStatus::Cancelled, document.reason.clone())
        }
        ContentItemStatus::Ready => {
            if let Some(reason) = &placement.unavailable_reason {
                (DistributionTargetStatus::Deferred, Some(reason.clone()))
            } else if !placement.supported_formats.contains(&document.content_type) {
                (
                    DistributionTargetStatus::NotApplicable,
                    Some("unsupported_format".into()),
                )
            } else {
                (DistributionTargetStatus::Pending, None)
            }
        }
        _ => (
            DistributionTargetStatus::Deferred,
            Some("content_not_ready".into()),
        ),
    };
    DistributionTarget {
        target_id: identity(&[
            &manifest.manifest_id.to_string(),
            &document.document_item_id.to_string(),
            &placement.platform_id,
            &placement.placement_slot,
        ]),
        manifest_id: manifest.manifest_id,
        ordinal,
        document_item_id: document.document_item_id,
        content_revision_id: document.content_revision_id,
        platform_id: placement.platform_id.clone(),
        placement_slot: placement.placement_slot.clone(),
        variant_id: None,
        account_id: None,
        publication_intent_id: None,
        status,
        reason,
        version: 1,
    }
}

/// Stable across cycles for identical project, content, payload, placement
/// and account. A persisted prior intent always takes precedence over this
/// candidate (including when an external result is unknown).
pub fn prepare_publication_intent(
    scope: &TenantScope,
    manifest: &DistributionManifest,
    target: &DistributionTarget,
    variant: &ChannelVariant,
    account_id: Uuid,
    at: DateTime<Utc>,
) -> (PublicationIntent, PublicationCommand) {
    let logical_key = digest(&[
        &scope.storage_key(),
        &variant.content_revision_id.to_string(),
        &variant.variant_id.to_string(),
        &target.platform_id,
        &target.placement_slot,
        &account_id.to_string(),
        &variant.payload_hash,
    ]);
    let intent_id = identity(&[&logical_key]);
    (
        PublicationIntent {
            intent_id,
            project_id: manifest.project_id,
            channel_target_id: target.target_id,
            variant_id: variant.variant_id,
            content_revision_id: variant.content_revision_id,
            platform_id: target.platform_id.clone(),
            placement_slot: target.placement_slot.clone(),
            account_id,
            payload_hash: variant.payload_hash.clone(),
            logical_key,
            verification: IntentVerification::Unverified,
            verification_evidence_id: None,
            created_at: at,
        },
        PublicationCommand {
            command_id: identity(&[&intent_id.to_string(), "publish"]),
            intent_id,
            target_id: target.target_id,
            payload_hash: variant.payload_hash.clone(),
            fixture: manifest.platform_scope.iter().any(|placement| {
                placement.platform_id == target.platform_id
                    && placement.placement_slot == target.placement_slot
                    && placement.fixture
            }),
        },
    )
}

#[async_trait]
pub trait DistributionRepository: Send + Sync {
    async fn get_publication_bundle(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<PublicationBundle, AppError>;
    async fn freeze(
        &self,
        scope: &TenantScope,
        input: FreezeDistribution,
    ) -> Result<DistributionManifest, AppError>;
    async fn get(&self, scope: &TenantScope, id: Uuid) -> Result<DistributionManifest, AppError>;
    async fn get_target(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        target_id: Uuid,
    ) -> Result<DistributionTarget, AppError>;
    async fn latest_for_cycle(
        &self,
        scope: &TenantScope,
        cycle: Uuid,
    ) -> Result<Option<DistributionManifest>, AppError>;
    async fn as_of(
        &self,
        scope: &TenantScope,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<DistributionSnapshot, AppError>;
    async fn expansion_page(
        &self,
        scope: &TenantScope,
        id: Uuid,
        cursor: u64,
        limit: usize,
    ) -> Result<DistributionExpansionPage, AppError>;
    /// Transactional compare-and-swap. Replaying the exact committed rows at
    /// an earlier cursor is safe; changed rows or future cursors conflict.
    async fn commit_expansion_page(
        &self,
        scope: &TenantScope,
        id: Uuid,
        expected_cursor: u64,
        rows: Vec<DistributionTarget>,
    ) -> Result<DistributionManifest, AppError>;
    /// One atomic storage operation: target transition, immutable variant and
    /// project-scoped intent reuse, and durable outbox command insertion.
    async fn materialize(
        &self,
        scope: &TenantScope,
        prepared: PreparedDistribution,
    ) -> Result<MaterializedDistribution, AppError>;
    /// Trusted receipt adapter only: supply the evidence ID of a persisted
    /// external check before changing the reuse classification.
    async fn record_intent_verification(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        verification: IntentVerification,
        evidence_id: Uuid,
    ) -> Result<PublicationIntent, AppError>;
    async fn list_targets(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after_ordinal: Option<u64>,
        limit: usize,
    ) -> Result<DistributionTargetPage, AppError>;
    async fn cycle_inputs(
        &self,
        scope: &TenantScope,
        cycle: Uuid,
        at: DateTime<Utc>,
    ) -> Result<DistributionCycleInputs, AppError>;
    /// Read server-owned attempt and receipt records at a strict report cutoff.
    /// The passed targets are historical snapshots; never look up their mutable
    /// current binding when associating a reused intent.
    async fn publication_results(
        &self,
        scope: &TenantScope,
        targets: &[DistributionTarget],
        at: DateTime<Utc>,
    ) -> Result<Vec<DistributionPublicationResult>, AppError>;
}

#[derive(Default)]
pub struct MemoryDistributionRepository {
    state: RwLock<MemoryDistributionState>,
}

#[derive(Default)]
struct MemoryDistributionState {
    manifests: HashMap<(String, Uuid), DistributionManifest>,
    histories: HashMap<(String, Uuid), Vec<DistributionTargetVersion>>,
    intents: HashMap<(String, String), PublicationIntent>,
    variants: HashMap<(String, Uuid), ChannelVariant>,
    revisions: HashMap<(String, Uuid), ContentRevision>,
    commands: HashMap<(String, Uuid), PublicationCommand>,
}

impl MemoryDistributionRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// In-memory diagnostic view. Production dispatch must consume a durable
    /// outbox transactionally; this view makes no delivery guarantee.
    pub async fn publication_commands(&self, scope: &TenantScope) -> Vec<PublicationCommand> {
        let state = self.state.read().await;
        let mut commands: Vec<_> = state
            .commands
            .iter()
            .filter(|((key, _), _)| key == &scope.storage_key())
            .map(|(_, value)| value.clone())
            .collect();
        commands.sort_by_key(|command| command.command_id);
        commands
    }
}

fn scoped_manifest<'a>(
    state: &'a MemoryDistributionState,
    scope: &TenantScope,
    id: Uuid,
) -> Result<&'a DistributionManifest, AppError> {
    let manifest = state
        .manifests
        .get(&(scope.storage_key(), id))
        .ok_or_else(|| AppError::not_found("distribution manifest not found"))?;
    check_scope(scope, manifest.project_id)?;
    Ok(manifest)
}

fn recorded_at(history: &[DistributionTargetVersion]) -> DateTime<Utc> {
    let now = Utc::now();
    history.last().map_or(now, |last| {
        now.max(last.recorded_at + Duration::nanoseconds(1))
    })
}

#[async_trait]
impl DistributionRepository for MemoryDistributionRepository {
    async fn publication_results(
        &self,
        _scope: &TenantScope,
        _targets: &[DistributionTarget],
        _at: DateTime<Utc>,
    ) -> Result<Vec<DistributionPublicationResult>, AppError> {
        // Memory intent verification has no trusted persisted receipt ledger.
        // Never upgrade it to a report success from an intent marker alone.
        Ok(vec![])
    }
    async fn get_publication_bundle(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<PublicationBundle, AppError> {
        let state = self.state.read().await;
        let key = scope.storage_key();
        let intent = state
            .intents
            .iter()
            .find(|((owner, _), value)| owner == &key && value.intent_id == intent_id)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| AppError::not_found("publication intent not found"))?;
        let variant = state
            .variants
            .get(&(key.clone(), intent.variant_id))
            .cloned()
            .ok_or_else(|| AppError::not_found("publication variant not found"))?;
        let revision = state
            .revisions
            .get(&(key.clone(), intent.content_revision_id))
            .cloned()
            .ok_or_else(|| AppError::not_found("content revision not found"))?;
        let command = state
            .commands
            .iter()
            .find(|((owner, _), command)| owner == &key && command.intent_id == intent_id)
            .map(|(_, command)| command.clone())
            .ok_or_else(|| AppError::not_found("publication command not found"))?;
        let target = state
            .histories
            .get(&(key, command.target_id))
            .and_then(|history| history.last())
            .map(|version| version.target.clone())
            .ok_or_else(|| AppError::not_found("distribution target not found"))?;
        Ok(PublicationBundle {
            revision,
            variant,
            intent,
            target,
            command,
        })
    }

    async fn freeze(
        &self,
        scope: &TenantScope,
        input: FreezeDistribution,
    ) -> Result<DistributionManifest, AppError> {
        let mut manifest = freeze_distribution(scope, input)?;
        let key = (scope.storage_key(), manifest.manifest_id);
        let mut state = self.state.write().await;
        if let Some(old) = state.manifests.get(&key) {
            if old.input_hash != manifest.input_hash {
                return Err(AppError::conflict("frozen distribution inputs differ"));
            }
            return Ok(old.clone());
        }
        // A caller cannot backdate a freeze to rewrite an earlier report cutoff.
        manifest.sealed_at = Utc::now();
        state.manifests.insert(key, manifest.clone());
        Ok(manifest)
    }

    async fn get(&self, scope: &TenantScope, id: Uuid) -> Result<DistributionManifest, AppError> {
        let state = self.state.read().await;
        Ok(scoped_manifest(&state, scope, id)?.clone())
    }

    async fn get_target(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        target_id: Uuid,
    ) -> Result<DistributionTarget, AppError> {
        let state = self.state.read().await;
        scoped_manifest(&state, scope, manifest_id)?;
        state
            .histories
            .get(&(scope.storage_key(), target_id))
            .and_then(|history| history.last())
            .filter(|record| record.target.manifest_id == manifest_id)
            .map(|record| record.target.clone())
            .ok_or_else(|| AppError::not_found("distribution target not found"))
    }

    async fn latest_for_cycle(
        &self,
        scope: &TenantScope,
        cycle: Uuid,
    ) -> Result<Option<DistributionManifest>, AppError> {
        let state = self.state.read().await;
        Ok(state
            .manifests
            .iter()
            .filter(|((key, _), manifest)| {
                key == &scope.storage_key()
                    && scope.project_id == Some(manifest.project_id)
                    && manifest.cycle_id == cycle
            })
            .map(|(_, manifest)| manifest)
            .max_by_key(|manifest| manifest.revision)
            .cloned())
    }

    async fn as_of(
        &self,
        scope: &TenantScope,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<DistributionSnapshot, AppError> {
        let state = self.state.read().await;
        let mut manifest = scoped_manifest(&state, scope, id)?.clone();
        if at < manifest.sealed_at {
            return Err(AppError::not_found("manifest was not frozen at cutoff"));
        }
        let mut targets: Vec<_> = state
            .histories
            .iter()
            .filter(|((key, _), _)| key == &scope.storage_key())
            .filter_map(|(_, versions)| {
                versions
                    .iter()
                    .rev()
                    .find(|version| version.recorded_at <= at)
            })
            .filter(|version| version.target.manifest_id == id)
            .map(|version| version.target.clone())
            .collect();
        targets.sort_by_key(|target| target.ordinal);
        manifest.expansion_cursor = targets
            .iter()
            .take_while(|target| target.ordinal < manifest.expected_count)
            .enumerate()
            .take_while(|(index, target)| *index as u64 == target.ordinal)
            .count() as u64;
        manifest.complete = manifest.expansion_cursor == manifest.expected_count;
        Ok(DistributionSnapshot {
            manifest,
            targets,
            as_of: at,
        })
    }

    async fn expansion_page(
        &self,
        scope: &TenantScope,
        id: Uuid,
        cursor: u64,
        limit: usize,
    ) -> Result<DistributionExpansionPage, AppError> {
        let manifest = self.get(scope, id).await?;
        if limit == 0 || cursor > manifest.expected_count {
            return Err(AppError::invalid_request(
                "invalid expansion cursor or page limit",
            ));
        }
        let end = cursor
            .saturating_add(limit as u64)
            .min(manifest.expected_count);
        Ok(DistributionExpansionPage {
            manifest_id: id,
            cursor,
            next_cursor: end,
            rows: (cursor..end)
                .map(|ordinal| distribution_cell(&manifest, ordinal))
                .collect(),
        })
    }

    async fn commit_expansion_page(
        &self,
        scope: &TenantScope,
        id: Uuid,
        expected_cursor: u64,
        rows: Vec<DistributionTarget>,
    ) -> Result<DistributionManifest, AppError> {
        if rows.is_empty() {
            return Err(AppError::invalid_request("expansion page is empty"));
        }
        let key = (scope.storage_key(), id);
        let mut state = self.state.write().await;
        let manifest = scoped_manifest(&state, scope, id)?.clone();
        let end = expected_cursor
            .checked_add(rows.len() as u64)
            .ok_or_else(|| AppError::invalid_request("expansion cursor overflow"))?;
        if end > manifest.expected_count
            || rows.iter().enumerate().any(|(offset, row)| {
                row != &distribution_cell(&manifest, expected_cursor + offset as u64)
            })
        {
            return Err(AppError::conflict(
                "expansion rows do not match frozen roster",
            ));
        }
        if manifest.expansion_cursor != expected_cursor {
            if end <= manifest.expansion_cursor
                && rows.iter().all(|row| {
                    state
                        .histories
                        .get(&(scope.storage_key(), row.target_id))
                        .and_then(|history| history.first())
                        .is_some_and(|version| version.target == *row)
                })
            {
                return Ok(manifest);
            }
            return Err(AppError::conflict("expansion cursor changed"));
        }
        for row in rows {
            let history = state
                .histories
                .entry((scope.storage_key(), row.target_id))
                .or_default();
            history.push(DistributionTargetVersion {
                recorded_at: recorded_at(history),
                target: row,
            });
        }
        let manifest = state.manifests.get_mut(&key).expect("manifest checked");
        manifest.expansion_cursor = end;
        manifest.complete = end == manifest.expected_count;
        Ok(manifest.clone())
    }

    async fn materialize(
        &self,
        scope: &TenantScope,
        prepared: PreparedDistribution,
    ) -> Result<MaterializedDistribution, AppError> {
        let mut state = self.state.write().await;
        let manifest = scoped_manifest(&state, scope, prepared.manifest_id)?.clone();
        let key = (scope.storage_key(), prepared.target_id);
        let history = state
            .histories
            .get(&key)
            .ok_or_else(|| AppError::not_found("distribution target not found"))?;
        let old = history.last().expect("nonempty history").target.clone();
        if old.manifest_id != manifest.manifest_id {
            return Err(AppError::not_found("target belongs to another manifest"));
        }
        if old.status != DistributionTargetStatus::Pending
            && old.status != DistributionTargetStatus::Deferred
            && old.status != DistributionTargetStatus::Ready
            && old.status != DistributionTargetStatus::ReusedUnknown
            && old.status != DistributionTargetStatus::ReusedVerified
        {
            return Err(AppError::conflict("target is not publishable"));
        }
        let placement = manifest
            .platform_scope
            .iter()
            .find(|placement| {
                placement.platform_id == old.platform_id
                    && placement.placement_slot == old.placement_slot
            })
            .expect("frozen placement");
        if let Some(reason) = prepared.defer_reason {
            let mut next = old.clone();
            next.status = DistributionTargetStatus::Deferred;
            next.reason = Some(reason.code().into());
            if next != old {
                next.version += 1;
                let history = state.histories.get_mut(&key).expect("checked");
                history.push(DistributionTargetVersion {
                    recorded_at: recorded_at(history),
                    target: next.clone(),
                });
            }
            let variant = old
                .variant_id
                .and_then(|id| state.variants.get(&(scope.storage_key(), id)).cloned());
            let intent = old.publication_intent_id.and_then(|id| {
                state
                    .intents
                    .iter()
                    .find(|((key, _), intent)| {
                        key == &scope.storage_key() && intent.intent_id == id
                    })
                    .map(|(_, intent)| intent.clone())
            });
            return Ok(MaterializedDistribution {
                target: next,
                variant,
                intent,
                publication_commands: vec![],
            });
        }
        if old.status == DistributionTargetStatus::Deferred
            && !matches!(
                old.reason.as_deref(),
                Some(
                    "account_unassigned"
                        | "source_unavailable"
                        | "source_changed"
                        | "content_unsupported"
                )
            )
        {
            return Err(AppError::conflict(
                "target is deferred by capability or content",
            ));
        }
        let mut next = old.clone();
        let Some(account_id) = prepared.account_id else {
            if old.publication_intent_id.is_some() {
                return Err(AppError::conflict("assigned intent cannot be unassigned"));
            }
            next.status = DistributionTargetStatus::Deferred;
            next.reason = Some("account_unassigned".into());
            if next != old {
                next.version += 1;
                let history = state.histories.get_mut(&key).expect("checked");
                history.push(DistributionTargetVersion {
                    recorded_at: recorded_at(history),
                    target: next.clone(),
                });
            }
            return Ok(MaterializedDistribution {
                target: next,
                variant: None,
                intent: None,
                publication_commands: vec![],
            });
        };
        let revision = prepared
            .revision
            .as_ref()
            .ok_or_else(|| AppError::invalid_request("content revision required"))?;
        if Some(revision.revision_id) != old.content_revision_id {
            return Err(AppError::conflict(
                "content revision differs from frozen handoff",
            ));
        }
        let variant = prepare_variant(revision, placement)?;
        if state
            .revisions
            .get(&(scope.storage_key(), revision.revision_id))
            .is_some_and(|prior| prior != revision)
            || state
                .variants
                .get(&(scope.storage_key(), variant.variant_id))
                .is_some_and(|prior| prior != &variant)
        {
            return Err(AppError::conflict(
                "immutable content revision or channel variant changed",
            ));
        }
        let (candidate, command) =
            prepare_publication_intent(scope, &manifest, &old, &variant, account_id, Utc::now());
        let intent_key = (scope.storage_key(), candidate.logical_key.clone());
        let previous = state.intents.get(&intent_key).cloned();
        let intent = previous.clone().unwrap_or(candidate);
        if let Some(bound) = old.publication_intent_id
            && (bound != intent.intent_id || old.account_id != Some(account_id))
        {
            return Err(AppError::conflict(
                "target is already bound to another intent",
            ));
        }
        let mut commands = Vec::new();
        if previous.is_none() {
            state
                .commands
                .insert((scope.storage_key(), command.command_id), command.clone());
            state.intents.insert(intent_key, intent.clone());
            commands.push(command);
        }
        state
            .variants
            .entry((scope.storage_key(), variant.variant_id))
            .or_insert(variant.clone());
        state
            .revisions
            .entry((scope.storage_key(), revision.revision_id))
            .or_insert(revision.clone());
        next.variant_id = Some(variant.variant_id);
        next.account_id = Some(account_id);
        next.publication_intent_id = Some(intent.intent_id);
        next.status = match previous.as_ref() {
            Some(prior) if prior.verification == IntentVerification::Verified => {
                DistributionTargetStatus::ReusedVerified
            }
            Some(prior) if prior.verification == IntentVerification::Unknown => {
                DistributionTargetStatus::ReusedUnknown
            }
            Some(prior) if prior.channel_target_id == old.target_id => {
                if old.status == DistributionTargetStatus::Deferred {
                    DistributionTargetStatus::Ready
                } else {
                    old.status
                }
            }
            Some(_) => DistributionTargetStatus::ReusedUnknown,
            None => DistributionTargetStatus::Ready,
        };
        next.reason = None;
        if next != old {
            next.version += 1;
            let history = state.histories.get_mut(&key).expect("checked");
            history.push(DistributionTargetVersion {
                recorded_at: recorded_at(history),
                target: next.clone(),
            });
        }
        Ok(MaterializedDistribution {
            target: next,
            variant: Some(variant),
            intent: Some(intent),
            publication_commands: commands,
        })
    }

    async fn record_intent_verification(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        verification: IntentVerification,
        evidence_id: Uuid,
    ) -> Result<PublicationIntent, AppError> {
        if evidence_id.is_nil() || verification == IntentVerification::Unverified {
            return Err(AppError::invalid_request(
                "receipt evidence and a resolved verification state are required",
            ));
        }
        let mut state = self.state.write().await;
        let (_, intent) = state
            .intents
            .iter_mut()
            .find(|((key, _), intent)| key == &scope.storage_key() && intent.intent_id == intent_id)
            .ok_or_else(|| AppError::not_found("publication intent not found"))?;
        check_scope(scope, intent.project_id)?;
        if intent.verification == IntentVerification::Verified
            && verification == IntentVerification::Unknown
        {
            return Err(AppError::conflict(
                "verified asset cannot revert to unknown",
            ));
        }
        intent.verification = verification;
        intent.verification_evidence_id = Some(evidence_id);
        Ok(intent.clone())
    }

    async fn list_targets(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after_ordinal: Option<u64>,
        limit: usize,
    ) -> Result<DistributionTargetPage, AppError> {
        if limit == 0 {
            return Err(AppError::invalid_request(
                "target page limit must be positive",
            ));
        }
        let state = self.state.read().await;
        let manifest = scoped_manifest(&state, scope, id)?;
        let mut targets: Vec<_> = state
            .histories
            .iter()
            .filter(|((key, _), _)| key == &scope.storage_key())
            .filter_map(|(_, history)| history.last())
            .filter(|record| {
                record.target.manifest_id == id
                    && after_ordinal.is_none_or(|cursor| record.target.ordinal > cursor)
            })
            .map(|record| record.target.clone())
            .collect();
        targets.sort_by_key(|target| target.ordinal);
        targets.truncate(limit);
        let next_ordinal = targets
            .last()
            .map(|target| target.ordinal)
            .filter(|ordinal| *ordinal + 1 < manifest.expansion_cursor);
        Ok(DistributionTargetPage {
            manifest_id: id,
            rows: targets,
            next_ordinal,
            expected_count: manifest.expected_count,
        })
    }

    async fn cycle_inputs(
        &self,
        scope: &TenantScope,
        cycle: Uuid,
        at: DateTime<Utc>,
    ) -> Result<DistributionCycleInputs, AppError> {
        let state = self.state.read().await;
        let manifest = state
            .manifests
            .iter()
            .filter(|((key, _), manifest)| {
                key == &scope.storage_key()
                    && scope.project_id == Some(manifest.project_id)
                    && manifest.cycle_id == cycle
                    && manifest.sealed_at <= at
            })
            .map(|(_, manifest)| manifest)
            .max_by_key(|manifest| manifest.revision)
            .cloned();
        drop(state);
        if let Some(manifest) = manifest {
            let snapshot = self.as_of(scope, manifest.manifest_id, at).await?;
            let temporally_complete = snapshot.targets.len() as u64 == manifest.expected_count;
            Ok(DistributionCycleInputs {
                manifest: Some(snapshot.manifest),
                targets: snapshot.targets,
                temporally_complete,
            })
        } else {
            Ok(DistributionCycleInputs {
                manifest: None,
                targets: vec![],
                temporally_complete: false,
            })
        }
    }
}

#[cfg(test)]
mod report_evidence_tests {
    use super::*;

    #[test]
    fn reused_receipt_retains_times_but_has_distinct_target_associations() {
        let intent = Uuid::new_v4();
        let receipt = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let observed = Utc::now() - Duration::hours(2);
        let received = Utc::now() - Duration::hours(1);
        let left = target_publication_evidence(
            first,
            intent,
            receipt,
            "public_verification",
            observed,
            received,
        );
        let right = target_publication_evidence(
            second,
            intent,
            receipt,
            "public_verification",
            observed,
            received,
        );
        assert_eq!(
            left,
            target_publication_evidence(
                first,
                intent,
                receipt,
                "public_verification",
                observed,
                received
            )
        );
        assert_ne!(left.evidence_id, right.evidence_id);
        assert_eq!(left.resource_id, first);
        assert_eq!(right.resource_id, second);
        assert_eq!(left.occurred_at, right.occurred_at);
        assert_eq!(left.received_at, right.received_at);
    }
}
