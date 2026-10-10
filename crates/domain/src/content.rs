//! Durable first-stage execution. Planning manifests are immutable inputs;
//! all mutable progress lives in this separate aggregate.
use crate::rich_content::{MediaReference, RichContent};
use crate::{
    AppError, ContentReuseBinding, ContentReuseCandidate, ContentReuseDecision,
    ContentReuseRequest, ContentSemanticDescriptor, DocumentManifest, DocumentManifestItemState,
    EvidenceRef, MediaObjectKey, MemoryContentMediaRepository, ProjectId, TenantScope,
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub const RICH_GENERATION_POLICY_VERSION: &str = "rich-content-generation-v2";
pub const RICH_CHECK_POLICY_VERSION: &str = "rich-content-check-v2";
pub const RICH_REPAIR_POLICY_VERSION: &str = "rich-content-repair-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentExecutionStatus {
    Running,
    Closed,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentItemStatus {
    Pending,
    Prepared,
    Drafted,
    NeedsRepair,
    Ready,
    Blocked,
    Deferred,
    NotApplicable,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentStep {
    Prepare,
    Generate,
    Check,
    Repair,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlockKind {
    Heading,
    Paragraph,
    List,
    Rich,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlock {
    pub block_id: Uuid,
    pub kind: ContentBlockKind,
    pub text: String,
    #[serde(default)]
    pub citation_ids: Vec<Uuid>,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich: Option<RichContent>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredDocument {
    pub title: String,
    pub blocks: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u8>,
}
impl StructuredDocument {
    pub fn media_references(&self) -> Vec<&MediaReference> {
        self.blocks
            .iter()
            .filter_map(|b| b.rich.as_ref())
            .flat_map(RichContent::media_references)
            .collect()
    }
    /// Text visible to factual checks, preserving the title and one evidence
    /// envelope per top-level block. Includes nested tables, code and media
    /// alternative text/captions; authorization remains separate.
    pub fn check_sections(&self) -> Vec<(Option<Uuid>, String)> {
        let mut sections = Vec::with_capacity(self.blocks.len() + 1);
        sections.push((None, self.title.clone()));
        sections.extend(self.blocks.iter().map(|block| {
            let text = match &block.rich {
                Some(rich) => rich.plain_text(),
                None if block.kind == ContentBlockKind::List => {
                    std::iter::once(block.text.as_str())
                        .chain(block.items.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                None => block.text.clone(),
            };
            (Some(block.block_id), text)
        }));
        sections
    }
    pub fn validate(&self, evidence: &[EvidenceRef]) -> Result<(), AppError> {
        self.validate_structure(evidence)
    }
    /// Structural validation alone does not authorize referenced media objects.
    pub fn validate_structure(&self, evidence: &[EvidenceRef]) -> Result<(), AppError> {
        if !matches!(self.schema_version, None | Some(2)) {
            return Err(AppError::invalid_request(
                "unsupported content schema version",
            ));
        }
        if self.title.trim().is_empty() || self.blocks.is_empty() {
            return Err(AppError::invalid_request(
                "document title and blocks are required",
            ));
        }
        if self.schema_version == Some(2)
            && (self.title.len() > 4096 || self.blocks.len() > crate::rich_content::MAX_NODES)
        {
            return Err(AppError::invalid_request(
                "rich document title or block count limit exceeded",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let (mut node_count, mut text_bytes) = (0usize, self.title.len());
        for block in &self.blocks {
            if !seen.insert(block.block_id) {
                return Err(AppError::invalid_request("duplicate block id"));
            }
            if block.kind == ContentBlockKind::Rich {
                if self.schema_version != Some(2)
                    || !block.text.is_empty()
                    || !block.items.is_empty()
                {
                    return Err(AppError::invalid_request(
                        "rich block requires schema v2 and empty legacy text/items",
                    ));
                }
                block
                    .rich
                    .as_ref()
                    .ok_or_else(|| AppError::invalid_request("rich block requires a node"))?
                    .validate()?;
                let (nodes, bytes) = block
                    .rich
                    .as_ref()
                    .expect("rich node is present")
                    .complexity();
                node_count = node_count.saturating_add(nodes);
                text_bytes = text_bytes.saturating_add(bytes);
            } else if block.rich.is_some() {
                return Err(AppError::invalid_request(
                    "only rich blocks contain rich nodes",
                ));
            }
            if block.kind != ContentBlockKind::List && !block.items.is_empty() {
                return Err(AppError::invalid_request("only list blocks contain items"));
            }
            if block.kind == ContentBlockKind::List && block.items.is_empty() {
                return Err(AppError::invalid_request("list blocks require items"));
            }
            if block.kind != ContentBlockKind::Rich
                && block.text.trim().is_empty()
                && block.items.is_empty()
            {
                return Err(AppError::invalid_request("empty block"));
            }
            if self.schema_version == Some(2) {
                node_count = node_count.saturating_add(1);
                text_bytes = text_bytes.saturating_add(block.text.len());
                for item in &block.items {
                    text_bytes = text_bytes.saturating_add(item.len());
                }
                if node_count > crate::rich_content::MAX_NODES
                    || text_bytes > crate::rich_content::MAX_TEXT_BYTES
                {
                    return Err(AppError::invalid_request(
                        "rich document node or text limit exceeded",
                    ));
                }
            }
            if block
                .citation_ids
                .iter()
                .any(|id| !evidence.iter().any(|e| e.chunk_id == Some(*id)))
            {
                return Err(AppError::invalid_request(
                    "citation is outside the prepared evidence set",
                ));
            }
        }
        Ok(())
    }
    pub fn markdown(&self) -> String {
        if self.schema_version == Some(2) {
            return self.rich_markdown();
        }
        let mut result = format!("# {}\n\n", self.title);
        for block in &self.blocks {
            match block.kind {
                ContentBlockKind::Heading => result.push_str(&format!("## {}\n\n", block.text)),
                ContentBlockKind::Paragraph => result.push_str(&format!("{}\n\n", block.text)),
                ContentBlockKind::List => {
                    if !block.text.is_empty() {
                        result.push_str(&format!("{}\n\n", block.text));
                    }
                    for item in &block.items {
                        result.push_str(&format!("- {item}\n"));
                    }
                    result.push('\n');
                }
                ContentBlockKind::Rich => unreachable!("v1 rich blocks are rejected by validation"),
            }
        }
        result.trim_end().to_owned()
    }
    fn rich_markdown(&self) -> String {
        self.render_rich_markdown(None)
            .expect("legacy rich rendering does not resolve media paths")
    }
    fn render_rich_markdown(
        &self,
        paths: Option<&HashMap<MediaObjectKey, String>>,
    ) -> Result<String, AppError> {
        let mut result = format!(
            "# {}\n\n",
            crate::rich_content::escape_markdown(&self.title)
        );
        for block in &self.blocks {
            match block.kind {
                ContentBlockKind::Rich => {
                    let rich = block.rich.as_ref().expect("validated rich block");
                    result.push_str(&match paths {
                        Some(paths) => rich.markdown_with_media_paths(paths)?,
                        None => rich.markdown(),
                    });
                }
                ContentBlockKind::Heading => result.push_str(&format!(
                    "## {}\n\n",
                    crate::rich_content::escape_markdown(&block.text)
                )),
                ContentBlockKind::Paragraph => result.push_str(&format!(
                    "{}\n\n",
                    crate::rich_content::escape_markdown(&block.text)
                )),
                ContentBlockKind::List => {
                    if !block.text.is_empty() {
                        result.push_str(&format!(
                            "{}\n\n",
                            crate::rich_content::escape_markdown(&block.text)
                        ));
                    }
                    for item in &block.items {
                        result.push_str(&format!(
                            "- {}\n",
                            crate::rich_content::escape_markdown(item)
                        ));
                    }
                    result.push('\n');
                }
            }
        }
        Ok(result.trim_end().to_owned())
    }
    /// Rendering requires valid structure. Authorization of media references remains a service responsibility.
    pub fn checked_markdown(&self, evidence: &[EvidenceRef]) -> Result<String, AppError> {
        self.validate(evidence)?;
        Ok(self.markdown())
    }
    /// Resolve image paths from an authorized, immutable media snapshot. The
    /// mapping is applied only to typed media nodes, never to document text.
    pub fn markdown_with_media_paths(
        &self,
        evidence: &[EvidenceRef],
        paths: &HashMap<MediaObjectKey, String>,
    ) -> Result<String, AppError> {
        self.validate(evidence)?;
        self.validate_export_media_identities()?;
        if self.schema_version == Some(2) {
            self.render_rich_markdown(Some(paths))
        } else {
            Ok(self.markdown())
        }
    }
    /// HTML contains relative media paths only. A caller must authorize and package all media before export.
    pub fn html(&self, evidence: &[EvidenceRef]) -> Result<String, AppError> {
        self.validate(evidence)?;
        self.render_html(None)
    }
    /// Render typed media references against the authorized bundle paths.
    pub fn html_with_media_paths(
        &self,
        evidence: &[EvidenceRef],
        paths: &HashMap<MediaObjectKey, String>,
    ) -> Result<String, AppError> {
        self.validate(evidence)?;
        self.validate_export_media_identities()?;
        self.render_html(Some(paths))
    }
    fn validate_export_media_identities(&self) -> Result<(), AppError> {
        let mut seen = HashMap::new();
        for reference in self.media_references() {
            let identity = (reference.object_id, reference.object_version);
            if seen
                .insert(identity, reference.sha256.as_str())
                .is_some_and(|digest| digest != reference.sha256.as_str())
            {
                return Err(AppError::invalid_request(
                    "media object has conflicting digests",
                ));
            }
        }
        Ok(())
    }
    fn render_html(
        &self,
        paths: Option<&HashMap<MediaObjectKey, String>>,
    ) -> Result<String, AppError> {
        let mut output = format!("<h1>{}</h1>", crate::rich_content::escape_html(&self.title));
        for block in &self.blocks {
            match block.kind {
                ContentBlockKind::Rich => output.push_str(&match paths {
                    Some(paths) => block
                        .rich
                        .as_ref()
                        .expect("validated rich block")
                        .html_with_media_paths(paths)?,
                    None => block.rich.as_ref().expect("validated rich block").html(),
                }),
                ContentBlockKind::Heading => output.push_str(&format!(
                    "<h2>{}</h2>",
                    crate::rich_content::escape_html(&block.text)
                )),
                ContentBlockKind::Paragraph => output.push_str(&format!(
                    "<p>{}</p>",
                    crate::rich_content::escape_html(&block.text)
                )),
                ContentBlockKind::List => {
                    if !block.text.is_empty() {
                        output.push_str(&format!(
                            "<p>{}</p>",
                            crate::rich_content::escape_html(&block.text)
                        ));
                    }
                    output.push_str("<ul>");
                    for item in &block.items {
                        output.push_str(&format!(
                            "<li><p>{}</p></li>",
                            crate::rich_content::escape_html(item)
                        ));
                    }
                    output.push_str("</ul>");
                }
            }
        }
        Ok(output)
    }
}

/// Only new revisions and revisions newly selected for readiness or a new
/// handoff need fresh media authorization. Historical revisions remain intact
/// after withdrawal so deleting withdrawn images is always possible.
fn affected_media_keys(previous: &ContentState, next: &ContentState) -> Vec<MediaObjectKey> {
    let old_ids: std::collections::HashSet<_> = previous
        .revisions
        .iter()
        .map(|revision| revision.revision_id)
        .collect();
    let mut selected: std::collections::HashSet<Uuid> = next
        .revisions
        .iter()
        .filter(|revision| !old_ids.contains(&revision.revision_id))
        .map(|revision| revision.revision_id)
        .collect();
    for item in &next.items {
        let prior = previous
            .items
            .iter()
            .find(|old| old.item_id == item.item_id);
        if item.status == ContentItemStatus::Ready
            && item.ready_revision_id.is_some()
            && prior.is_none_or(|old| {
                old.status != ContentItemStatus::Ready
                    || old.ready_revision_id != item.ready_revision_id
            })
        {
            selected.extend(item.ready_revision_id);
        }
    }
    for handoff in next.handoffs.iter().skip(previous.handoffs.len()) {
        selected.extend(
            handoff
                .items
                .iter()
                .filter(|item| item.status == ContentItemStatus::Ready)
                .filter_map(|item| item.revision_id),
        );
    }
    media_keys_for_revisions(&next.revisions, &selected)
}

fn media_keys_for_revisions(
    revisions: &[ContentRevision],
    selected: &std::collections::HashSet<Uuid>,
) -> Vec<MediaObjectKey> {
    let mut keys: Vec<_> = revisions
        .iter()
        .filter(|revision| selected.contains(&revision.revision_id))
        .flat_map(|revision| revision.document.media_references())
        .map(|media| MediaObjectKey {
            object_id: media.object_id,
            object_version: media.object_version,
            sha256: media.sha256.clone(),
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

fn media_keys_for_revision(revision: &ContentRevision) -> Vec<MediaObjectKey> {
    media_keys_for_revisions(
        std::slice::from_ref(revision),
        &std::collections::HashSet::from([revision.revision_id]),
    )
}
fn selected_media_keys(
    states: &HashMap<Uuid, (TenantScope, ContentState)>,
    scope: &TenantScope,
    state: &ContentState,
    item: &ContentItem,
) -> Result<Vec<MediaObjectKey>, AppError> {
    let revision_id = item
        .ready_revision_id
        .ok_or_else(|| AppError::conflict("ready revision is missing"))?;
    if let Some(revision) = state
        .revisions
        .iter()
        .find(|r| r.revision_id == revision_id)
    {
        return Ok(media_keys_for_revision(revision));
    }
    let origin_id = item
        .reuse_binding
        .as_ref()
        .or_else(|| {
            item.reuse_history
                .iter()
                .find(|r| r.revision_id == revision_id)
        })
        .filter(|binding| binding.revision_id == revision_id)
        .map(|binding| binding.origin_execution_id)
        .ok_or_else(|| AppError::conflict("ready revision is unavailable"))?;
    let (_, origin) = states
        .get(&origin_id)
        .filter(|(stored_scope, _)| stored_scope == scope)
        .ok_or_else(|| AppError::conflict("reuse origin is outside project scope"))?;
    let revision = origin
        .revisions
        .iter()
        .find(|r| r.revision_id == revision_id)
        .ok_or_else(|| AppError::conflict("reuse origin revision is unavailable"))?;
    Ok(media_keys_for_revision(revision))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBrief {
    pub brief_id: Uuid,
    pub title: String,
    pub objective: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub quotes: Vec<ContentEvidence>,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEvidence {
    pub reference: EvidenceRef,
    pub exact_quote: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentFinding {
    pub finding_id: Uuid,
    pub code: String,
    pub block_id: Option<Uuid>,
    pub evidence: Vec<EvidenceRef>,
    pub detail: String,
    pub blocking: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRevision {
    pub revision_id: Uuid,
    pub asset_id: Uuid,
    pub revision: i32,
    pub base_revision_id: Option<Uuid>,
    #[serde(default)]
    pub derived_from_revision_id: Option<Uuid>,
    pub document: StructuredDocument,
    pub markdown: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub quotes: Vec<ContentEvidence>,
    pub findings: Vec<ContentFinding>,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCheck {
    pub check_id: Uuid,
    pub revision_id: Uuid,
    pub findings: Vec<ContentFinding>,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentAsset {
    pub asset_id: Uuid,
    pub execution_id: Uuid,
    pub item_id: Uuid,
    pub current_revision_id: Uuid,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepLease {
    pub execution_id: Uuid,
    pub item_id: Uuid,
    pub step: ContentStep,
    pub owner: String,
    pub token: Uuid,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub revision_id: Option<Uuid>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentAttemptOutcome {
    Claimed,
    Completed,
    Released,
    Failed,
    Expired,
    Cancelled,
    Superseded,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentStepAttempt {
    pub step: ContentStep,
    pub owner: String,
    pub token: Uuid,
    pub claimed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub outcome: ContentAttemptOutcome,
    pub finished_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCoverage {
    pub total: u64,
    pub ready: u64,
    pub blocked: u64,
    pub deferred: u64,
    pub not_applicable: u64,
    pub cancelled: u64,
    pub incomplete: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHandoffItem {
    pub item_id: Uuid,
    pub document_key: String,
    pub status: ContentItemStatus,
    pub reason: Option<String>,
    pub revision_id: Option<Uuid>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHandoff {
    pub handoff_id: Uuid,
    pub execution_id: Uuid,
    pub revision: i32,
    pub supersedes_handoff_id: Option<Uuid>,
    pub coverage: ContentCoverage,
    pub items: Vec<ContentHandoffItem>,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentExecution {
    pub execution_id: Uuid,
    pub project_id: ProjectId,
    pub cycle_id: Uuid,
    pub manifest_id: Uuid,
    pub manifest_revision: i32,
    pub policy_version: String,
    pub input_hash: String,
    pub status: ContentExecutionStatus,
    pub expected_count: u64,
    pub coverage: ContentCoverage,
    pub handoff_id: Option<Uuid>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentItem {
    pub item_id: Uuid,
    pub execution_id: Uuid,
    pub document_key: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub product_id: Option<Uuid>,
    #[serde(default)]
    pub market: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub planner_version: String,
    pub branch_key: String,
    pub input_hash: String,
    pub planning_state: DocumentManifestItemState,
    pub planning_reason: Option<String>,
    pub status: ContentItemStatus,
    pub reason: Option<String>,
    pub source_version_refs: Vec<Uuid>,
    pub brief: Option<ContentBrief>,
    pub asset_id: Option<Uuid>,
    pub current_revision_id: Option<Uuid>,
    pub ready_revision_id: Option<Uuid>,
    #[serde(default)]
    pub reuse_binding: Option<ContentReuseBinding>,
    /// Pinned bindings retained for already sealed handoffs after copy-on-write.
    #[serde(default)]
    pub reuse_history: Vec<ContentReuseBinding>,
    #[serde(default)]
    pub semantic_descriptor: Option<ContentSemanticDescriptor>,
    #[serde(default)]
    pub semantic_fingerprint: Option<String>,
    #[serde(default)]
    pub reuse_reservation_token: Option<Uuid>,
    #[serde(default)]
    pub automatic_repair_count: u8,
    pub steps: Vec<StepLease>,
    #[serde(default)]
    pub attempts: Vec<ContentStepAttempt>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentState {
    pub execution: ContentExecution,
    pub items: Vec<ContentItem>,
    pub assets: Vec<ContentAsset>,
    pub revisions: Vec<ContentRevision>,
    #[serde(default)]
    pub checks: Vec<ContentCheck>,
    pub handoff: Option<ContentHandoff>,
    #[serde(default)]
    pub handoffs: Vec<ContentHandoff>,
}
fn hash(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    hex::encode(digest.finalize())
}
fn stable_id(key: &str) -> Uuid {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
pub fn start_content_state(
    scope: &TenantScope,
    cycle_id: Uuid,
    manifest: &DocumentManifest,
    policy_version: &str,
) -> Result<ContentState, AppError> {
    if scope.project_id != Some(manifest.project_id)
        || scope.operator_id != manifest.operator_id
        || scope.tenant_id != manifest.tenant_id
    {
        return Err(AppError::forbidden("manifest is outside project scope"));
    }
    if !manifest.sealed
        || manifest.expected_count != Some(manifest.items.len() as i64)
        || policy_version.trim().is_empty()
    {
        return Err(AppError::invalid_request(
            "sealed manifest and policy version required",
        ));
    }
    let root = format!(
        "{}:{}:{}:{}:{}:{}",
        scope.operator_id,
        scope.tenant_id,
        manifest.project_id,
        cycle_id,
        manifest.manifest_id,
        manifest.revision
    );
    let execution_id = stable_id(&format!("content:{root}:{}", policy_version));
    let mut items = Vec::with_capacity(manifest.items.len());
    let mut item_ids = std::collections::HashSet::new();
    let mut keys = std::collections::HashSet::new();
    for planned in &manifest.items {
        if planned.manifest_id != manifest.manifest_id
            || planned.knowledge_release_id != manifest.knowledge_release_id
            || !item_ids.insert(planned.document_manifest_item_id)
            || !keys.insert(planned.document_key.as_str())
        {
            return Err(AppError::invalid_request(
                "manifest contains inconsistent or repeated items",
            ));
        }
        let branch_key = format!("{cycle_id}:{}:{}", manifest.revision, planned.document_key);
        let frozen_item = serde_json::to_string(planned)
            .map_err(|_| AppError::invalid_request("manifest item cannot be serialized"))?;
        let input_hash = hash(&[&branch_key, &frozen_item, policy_version]);
        let (status, reason) = match planned.state {
            DocumentManifestItemState::Planned => (ContentItemStatus::Pending, None),
            DocumentManifestItemState::Blocked => {
                (ContentItemStatus::Blocked, planned.block_reason.clone())
            }
            DocumentManifestItemState::Deferred => {
                (ContentItemStatus::Deferred, planned.block_reason.clone())
            }
            DocumentManifestItemState::NotApplicable => (
                ContentItemStatus::NotApplicable,
                planned.block_reason.clone(),
            ),
        };
        items.push(ContentItem {
            item_id: planned.document_manifest_item_id,
            execution_id,
            document_key: planned.document_key.clone(),
            content_type: planned.content_type.clone(),
            product_id: planned.product_id,
            market: planned.market.clone(),
            language: planned.language.clone(),
            planner_version: manifest.planner_version.clone(),
            branch_key,
            input_hash,
            planning_state: planned.state.clone(),
            planning_reason: planned.block_reason.clone(),
            status,
            reason,
            source_version_refs: planned.source_version_refs.clone(),
            brief: None,
            asset_id: None,
            current_revision_id: None,
            ready_revision_id: None,
            reuse_binding: None,
            reuse_history: Vec::new(),
            semantic_descriptor: None,
            semantic_fingerprint: None,
            reuse_reservation_token: None,
            automatic_repair_count: 0,
            steps: Vec::new(),
            attempts: Vec::new(),
        });
    }
    let mut state = ContentState {
        execution: ContentExecution {
            execution_id,
            project_id: manifest.project_id,
            cycle_id,
            manifest_id: manifest.manifest_id,
            manifest_revision: manifest.revision,
            policy_version: policy_version.to_owned(),
            input_hash: hash(&[&root, &manifest.scope_hash, policy_version]),
            status: ContentExecutionStatus::Running,
            expected_count: items.len() as u64,
            coverage: ContentCoverage {
                total: 0,
                ready: 0,
                blocked: 0,
                deferred: 0,
                not_applicable: 0,
                cancelled: 0,
                incomplete: 0,
            },
            handoff_id: None,
        },
        items,
        assets: Vec::new(),
        revisions: Vec::new(),
        checks: Vec::new(),
        handoff: None,
        handoffs: Vec::new(),
    };
    state.recount();
    Ok(state)
}
impl ContentState {
    /// Bind a frozen semantic input to the current fenced prepare attempt.
    pub fn set_semantic_descriptor(
        &mut self,
        lease: &StepLease,
        descriptor: ContentSemanticDescriptor,
        fingerprint: &str,
    ) -> Result<ContentItem, AppError> {
        self.running()?;
        let canonical = descriptor.canonical()?;
        if descriptor.fingerprint()? != fingerprint
            || lease.execution_id != self.execution.execution_id
            || lease.step != ContentStep::Prepare
            || lease.expires_at <= Utc::now()
            || self.execution.project_id != descriptor.scope.project_id.unwrap()
            || self.execution.policy_version != descriptor.generation_policy_version
        {
            return Err(AppError::conflict(
                "stale or mismatched content reservation",
            ));
        }
        let item = self.item_mut(lease.item_id)?;
        let mut refs = item.source_version_refs.clone();
        refs.sort_unstable();
        refs.dedup();
        if item.document_key != canonical.document_key
            || item.content_type != canonical.content_type
            || item.product_id != canonical.product_id
            || item.market != canonical.market
            || item.language != canonical.language
            || item.planner_version != canonical.planner_version
            || item.status != ContentItemStatus::Pending
            || !item.steps.contains(lease)
            || refs != canonical.source_version_ids
            || item
                .semantic_fingerprint
                .as_deref()
                .is_some_and(|old| old != fingerprint)
            || item
                .semantic_descriptor
                .as_ref()
                .is_some_and(|old| old != &canonical)
        {
            return Err(AppError::conflict(
                "semantic input differs from frozen branch",
            ));
        }
        item.semantic_descriptor = Some(canonical);
        item.semantic_fingerprint = Some(fingerprint.to_owned());
        item.reuse_reservation_token = Some(lease.token);
        Ok(item.clone())
    }

    /// The destination keeps its own manifest and coverage but references the
    /// original immutable asset/revision/check; no records are copied locally.
    pub fn apply_reuse(
        &mut self,
        item_id: Uuid,
        binding: ContentReuseBinding,
        descriptor: ContentSemanticDescriptor,
        fingerprint: &str,
    ) -> Result<ContentItem, AppError> {
        self.running()?;
        let canonical = descriptor.canonical()?;
        if canonical.fingerprint()? != fingerprint
            || binding.fingerprint != fingerprint
            || self.execution.project_id != canonical.scope.project_id.unwrap()
            || self.execution.policy_version != canonical.generation_policy_version
            || binding.origin_execution_id == self.execution.execution_id
        {
            return Err(AppError::conflict("invalid content reuse binding"));
        }
        let item = self.item_mut(item_id)?;
        let mut refs = item.source_version_refs.clone();
        refs.sort_unstable();
        refs.dedup();
        if item.document_key != canonical.document_key
            || item.content_type != canonical.content_type
            || item.product_id != canonical.product_id
            || item.market != canonical.market
            || item.language != canonical.language
            || item.planner_version != canonical.planner_version
            || refs != canonical.source_version_ids
            || item.status != ContentItemStatus::Pending
            || item.steps.iter().any(|lease| lease.expires_at > Utc::now())
        {
            return Err(AppError::conflict("destination is not eligible for reuse"));
        }
        item.steps.clear();
        item.status = ContentItemStatus::Ready;
        item.reason = None;
        item.brief = Some(ContentBrief {
            brief_id: Uuid::new_v4(),
            title: canonical.brief_title.clone(),
            objective: canonical.brief_objective.clone(),
            evidence: canonical
                .evidence
                .iter()
                .map(|e| e.reference.clone())
                .collect(),
            quotes: canonical.evidence.clone(),
            created_at: binding.reused_at,
        });
        item.asset_id = Some(binding.asset_id);
        item.current_revision_id = Some(binding.revision_id);
        item.ready_revision_id = Some(binding.revision_id);
        item.semantic_descriptor = Some(canonical);
        item.semantic_fingerprint = Some(fingerprint.to_owned());
        item.reuse_history.push(binding.clone());
        item.reuse_binding = Some(binding);
        item.reuse_reservation_token = None;
        let result = item.clone();
        self.recount();
        Ok(result)
    }

    /// Editing a reused item forks the destination's own asset, preserving the
    /// origin's revision chain and all previously sealed handoffs.
    pub fn fork_reused_item(
        &mut self,
        item_id: Uuid,
        base_revision_id: Uuid,
        origin: &ContentRevision,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        if self.execution.status == ContentExecutionStatus::Cancelled {
            return Err(AppError::conflict("cancelled content cannot be edited"));
        }
        let item = self
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .ok_or_else(|| AppError::not_found("content item not found"))?;
        let binding = item
            .reuse_binding
            .as_ref()
            .ok_or_else(|| AppError::conflict("item is not a reused revision"))?;
        if item.status != ContentItemStatus::Ready
            || binding.revision_id != base_revision_id
            || origin.revision_id != binding.revision_id
            || origin.asset_id != binding.asset_id
        {
            return Err(AppError::conflict("reused base revision changed"));
        }
        document.validate(&origin.evidence)?;
        let now = Utc::now();
        let asset_id = stable_id(&format!(
            "content-asset:{}:{}",
            self.execution.execution_id, item.branch_key
        ));
        if self.assets.iter().any(|asset| asset.asset_id == asset_id) {
            return Err(AppError::conflict("destination asset already exists"));
        }
        let next = ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id,
            revision: 1,
            base_revision_id: None,
            derived_from_revision_id: Some(base_revision_id),
            markdown: document.markdown(),
            document,
            evidence: origin.evidence.clone(),
            quotes: origin.quotes.clone(),
            findings: Vec::new(),
            created_at: now,
        };
        if self.execution.status == ContentExecutionStatus::Closed {
            self.execution.status = ContentExecutionStatus::Running;
            self.execution.handoff_id = None;
            self.handoff = None;
        }
        let item = self.item_mut(item_id)?;
        item.asset_id = Some(asset_id);
        item.current_revision_id = Some(next.revision_id);
        item.ready_revision_id = None;
        item.reuse_binding = None;
        item.reuse_reservation_token = None;
        item.status = ContentItemStatus::Drafted;
        item.reason = None;
        self.assets.push(ContentAsset {
            asset_id,
            execution_id: self.execution.execution_id,
            item_id,
            current_revision_id: next.revision_id,
            created_at: now,
        });
        self.revisions.push(next.clone());
        self.recount();
        Ok(next)
    }

    pub fn recount(&mut self) {
        let mut c = ContentCoverage {
            total: self.items.len() as u64,
            ready: 0,
            blocked: 0,
            deferred: 0,
            not_applicable: 0,
            cancelled: 0,
            incomplete: 0,
        };
        for item in &self.items {
            match item.status {
                ContentItemStatus::Ready => c.ready += 1,
                ContentItemStatus::Blocked => c.blocked += 1,
                ContentItemStatus::Deferred => c.deferred += 1,
                ContentItemStatus::NotApplicable => c.not_applicable += 1,
                ContentItemStatus::Cancelled => c.cancelled += 1,
                _ => c.incomplete += 1,
            }
        }
        self.execution.coverage = c;
    }
    fn item_mut(&mut self, id: Uuid) -> Result<&mut ContentItem, AppError> {
        self.items
            .iter_mut()
            .find(|i| i.item_id == id)
            .ok_or_else(|| AppError::not_found("content item not found"))
    }
    fn running(&self) -> Result<(), AppError> {
        if self.execution.status != ContentExecutionStatus::Running {
            return Err(AppError::conflict("content execution is not running"));
        }
        Ok(())
    }
    pub fn claim(
        &mut self,
        item_id: Uuid,
        step: ContentStep,
        owner: &str,
        now: DateTime<Utc>,
        ttl_seconds: i64,
    ) -> Result<StepLease, AppError> {
        self.running()?;
        if owner.trim().is_empty() || !(1..=3600).contains(&ttl_seconds) {
            return Err(AppError::invalid_request("invalid lease owner or lifetime"));
        }
        if matches!(step, ContentStep::Check | ContentStep::Repair) {
            let item = self
                .items
                .iter()
                .find(|item| item.item_id == item_id)
                .ok_or_else(|| AppError::not_found("content item not found"))?;
            if item.current_revision_id.is_some_and(|id| {
                self.revisions
                    .iter()
                    .any(|r| r.revision_id == id && r.document.schema_version == Some(2))
            }) && owner
                != match step {
                    ContentStep::Check => RICH_CHECK_POLICY_VERSION,
                    ContentStep::Repair => RICH_REPAIR_POLICY_VERSION,
                    _ => unreachable!(),
                }
            {
                return Err(AppError::conflict(
                    "rich content requires a versioned check and repair workflow",
                ));
            }
        }
        if step == ContentStep::Repair {
            let item = self
                .items
                .iter()
                .find(|item| item.item_id == item_id)
                .ok_or_else(|| AppError::not_found("content item not found"))?;
            if item.automatic_repair_count >= 2
                || !item.current_revision_id.is_some_and(|revision_id| {
                    self.checks.iter().any(|check| {
                        check.revision_id == revision_id
                            && check.findings.iter().any(|finding| finding.blocking)
                    })
                })
            {
                return Err(AppError::conflict(
                    "current revision is not eligible for factual repair",
                ));
            }
        }
        let execution_id = self.execution.execution_id;
        let item = self.item_mut(item_id)?;
        let expected = match step {
            ContentStep::Prepare => ContentItemStatus::Pending,
            ContentStep::Generate => ContentItemStatus::Prepared,
            ContentStep::Check => ContentItemStatus::Drafted,
            ContentStep::Repair => ContentItemStatus::NeedsRepair,
        };
        if item.status != expected {
            return Err(AppError::conflict("step is not claimable for item status"));
        }
        if matches!(step, ContentStep::Check | ContentStep::Repair)
            && item.current_revision_id.is_none()
        {
            return Err(AppError::conflict("draft revision is missing"));
        }
        if item
            .steps
            .iter()
            .any(|lease| lease.step == step && lease.expires_at > now)
        {
            return Err(AppError::conflict("step is already leased"));
        }
        let expired: Vec<Uuid> = item
            .steps
            .iter()
            .filter(|lease| lease.step == step)
            .map(|lease| lease.token)
            .collect();
        for attempt in &mut item.attempts {
            if expired.contains(&attempt.token) && attempt.outcome == ContentAttemptOutcome::Claimed
            {
                attempt.outcome = ContentAttemptOutcome::Expired;
                attempt.finished_at = Some(now);
            }
        }
        item.steps.retain(|lease| lease.step != step);
        let lease = StepLease {
            execution_id,
            item_id,
            step,
            owner: owner.to_owned(),
            token: Uuid::new_v4(),
            expires_at: now + Duration::seconds(ttl_seconds),
            revision_id: matches!(step, ContentStep::Check | ContentStep::Repair)
                .then_some(item.current_revision_id)
                .flatten(),
        };
        item.steps.push(lease.clone());
        item.attempts.push(ContentStepAttempt {
            step,
            owner: owner.to_owned(),
            token: lease.token,
            claimed_at: now,
            expires_at: lease.expires_at,
            outcome: ContentAttemptOutcome::Claimed,
            finished_at: None,
        });
        Ok(lease)
    }
    fn consume(
        &mut self,
        lease: &StepLease,
        step: ContentStep,
    ) -> Result<&mut ContentItem, AppError> {
        self.running()?;
        if lease.execution_id != self.execution.execution_id
            || lease.step != step
            || lease.expires_at <= Utc::now()
        {
            return Err(AppError::conflict("stale step lease"));
        }
        let item = self.item_mut(lease.item_id)?;
        if !item.steps.iter().any(|stored| stored == lease) {
            return Err(AppError::conflict("stale step lease"));
        }
        if matches!(step, ContentStep::Check | ContentStep::Repair)
            && lease.revision_id.is_some()
            && item.current_revision_id != lease.revision_id
        {
            return Err(AppError::conflict("step revision changed"));
        }
        item.steps.retain(|stored| stored != lease);
        if let Some(attempt) = item.attempts.iter_mut().find(|a| a.token == lease.token) {
            attempt.outcome = ContentAttemptOutcome::Completed;
            attempt.finished_at = Some(Utc::now());
        }
        Ok(item)
    }
    pub fn complete_prepare(
        &mut self,
        lease: &StepLease,
        brief: ContentBrief,
    ) -> Result<ContentItem, AppError> {
        if brief.title.trim().is_empty()
            || brief.objective.trim().is_empty()
            || brief.evidence.is_empty()
        {
            return Err(AppError::invalid_request(
                "brief requires title, objective and evidence",
            ));
        }
        // Check before consuming so invalid provider output does not lose its lease.
        let item = self
            .items
            .iter()
            .find(|i| i.item_id == lease.item_id)
            .ok_or_else(|| AppError::not_found("item not found"))?;
        if brief.evidence.iter().any(|e| {
            e.chunk_id.is_none() || !item.source_version_refs.contains(&e.source_version_id)
        }) || brief
            .quotes
            .iter()
            .any(|q| q.exact_quote.trim().is_empty() || !brief.evidence.contains(&q.reference))
        {
            return Err(AppError::invalid_request(
                "brief evidence must be located in frozen public sources",
            ));
        }
        if let Some(descriptor) = &item.semantic_descriptor
            && (descriptor.brief_title != brief.title
                || descriptor.brief_objective != brief.objective
                || descriptor.evidence != brief.quotes)
        {
            return Err(AppError::conflict(
                "prepared brief differs from frozen semantic input",
            ));
        }
        let item = self.consume(lease, ContentStep::Prepare)?;
        if item.status != ContentItemStatus::Pending {
            return Err(AppError::conflict("prepare already completed"));
        }
        item.brief = Some(brief);
        item.status = ContentItemStatus::Prepared;
        Ok(item.clone())
    }
    pub fn complete_generate(
        &mut self,
        lease: &StepLease,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        if document.schema_version == Some(2)
            && self.execution.policy_version != RICH_GENERATION_POLICY_VERSION
        {
            return Err(AppError::invalid_request(
                "rich generation requires a versioned content policy",
            ));
        }
        let item = self
            .items
            .iter()
            .find(|i| i.item_id == lease.item_id)
            .ok_or_else(|| AppError::not_found("item not found"))?;
        let brief = item
            .brief
            .as_ref()
            .ok_or_else(|| AppError::conflict("brief is missing"))?;
        let evidence = brief.evidence.clone();
        let quotes = brief.quotes.clone();
        document.validate(&evidence)?;
        let item = self.consume(lease, ContentStep::Generate)?;
        if item.status != ContentItemStatus::Prepared {
            return Err(AppError::conflict("draft already generated"));
        }
        let now = Utc::now();
        // Each frozen execution owns its revision chain. A new generation
        // policy must not restart revision 1 on another execution's asset.
        let asset_id = stable_id(&format!(
            "content-asset:{}:{}",
            item.execution_id, item.branch_key
        ));
        let revision = ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id,
            revision: 1,
            base_revision_id: None,
            derived_from_revision_id: None,
            markdown: document.markdown(),
            document,
            evidence,
            quotes,
            findings: Vec::new(),
            created_at: now,
        };
        item.status = ContentItemStatus::Drafted;
        item.asset_id = Some(asset_id);
        item.current_revision_id = Some(revision.revision_id);
        self.assets.push(ContentAsset {
            asset_id,
            execution_id: lease.execution_id,
            item_id: lease.item_id,
            current_revision_id: revision.revision_id,
            created_at: now,
        });
        self.revisions.push(revision.clone());
        Ok(revision)
    }
    pub fn complete_check(
        &mut self,
        lease: &StepLease,
        findings: Vec<ContentFinding>,
    ) -> Result<ContentItem, AppError> {
        let item = self
            .items
            .iter()
            .find(|i| i.item_id == lease.item_id)
            .ok_or_else(|| AppError::not_found("item not found"))?;
        let revision_id = item
            .current_revision_id
            .ok_or_else(|| AppError::conflict("draft is missing"))?;
        let revision = self
            .revisions
            .iter()
            .find(|r| r.revision_id == revision_id)
            .ok_or_else(|| AppError::conflict("draft is missing"))?;
        if revision.document.schema_version == Some(2) && lease.owner != RICH_CHECK_POLICY_VERSION {
            return Err(AppError::conflict(
                "rich content requires a versioned check workflow",
            ));
        }
        if lease.revision_id.is_some() && lease.revision_id != Some(revision_id) {
            return Err(AppError::conflict("check revision changed"));
        }
        if findings.iter().any(|f| {
            f.code.trim().is_empty()
                || f.detail.trim().is_empty()
                || f.block_id
                    .is_some_and(|id| !revision.document.blocks.iter().any(|b| b.block_id == id))
                || f.evidence.iter().any(|e| !revision.evidence.contains(e))
        }) {
            return Err(AppError::invalid_request(
                "finding must cite a draft block and prepared evidence",
            ));
        }
        self.consume(lease, ContentStep::Check)?;
        self.checks.push(ContentCheck {
            check_id: Uuid::new_v4(),
            revision_id,
            findings: findings.clone(),
            created_at: Utc::now(),
        });
        let item = self.item_mut(lease.item_id)?;
        item.status = if findings.iter().any(|f| f.blocking) {
            if item.automatic_repair_count < 2 {
                ContentItemStatus::NeedsRepair
            } else {
                ContentItemStatus::Blocked
            }
        } else {
            ContentItemStatus::Ready
        };
        item.reason = findings
            .iter()
            .find(|f| f.blocking)
            .map(|f| f.detail.clone());
        item.ready_revision_id = (item.status == ContentItemStatus::Ready).then_some(revision_id);
        let result = item.clone();
        self.recount();
        Ok(result)
    }
    pub fn complete_repair(
        &mut self,
        lease: &StepLease,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        self.running()?;
        if lease.step != ContentStep::Repair || lease.execution_id != self.execution.execution_id {
            return Err(AppError::conflict("stale repair lease"));
        }
        let item = self
            .items
            .iter()
            .find(|i| i.item_id == lease.item_id)
            .ok_or_else(|| AppError::not_found("item not found"))?;
        if item.status != ContentItemStatus::NeedsRepair || item.automatic_repair_count >= 2 {
            return Err(AppError::conflict(
                "item is not eligible for automatic repair",
            ));
        }
        let revision_id = item
            .current_revision_id
            .ok_or_else(|| AppError::conflict("repair base revision is missing"))?;
        if lease.revision_id != Some(revision_id) {
            return Err(AppError::conflict("repair base revision changed"));
        }
        let previous = self
            .revisions
            .iter()
            .find(|r| r.revision_id == revision_id)
            .ok_or_else(|| AppError::conflict("repair base revision is missing"))?;
        if previous.document.schema_version != document.schema_version {
            return Err(AppError::conflict(
                "repair cannot change content schema version",
            ));
        }
        if previous.document.schema_version == Some(2) {
            if lease.owner != RICH_REPAIR_POLICY_VERSION {
                return Err(AppError::conflict(
                    "rich content requires a versioned repair workflow",
                ));
            }
            let check = self
                .checks
                .iter()
                .rev()
                .find(|c| c.revision_id == revision_id && c.findings.iter().any(|f| f.blocking))
                .ok_or_else(|| AppError::conflict("current revision has no blocking check"))?;
            let title_flagged = check
                .findings
                .iter()
                .any(|f| f.blocking && f.block_id.is_none());
            let flagged: std::collections::HashSet<_> = check
                .findings
                .iter()
                .filter(|f| f.blocking)
                .filter_map(|f| f.block_id)
                .collect();
            let old_positions: std::collections::HashMap<_, _> = previous
                .document
                .blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (block.block_id, index))
                .collect();
            let mut next_position = 0;
            let changed_illegally = document.blocks.iter().any(|new| {
                let Some(&index) = old_positions.get(&new.block_id) else { return true; };
                if index < next_position || previous.document.blocks[next_position..index].iter().any(|old| !flagged.contains(&old.block_id)) {
                    return true;
                }
                next_position = index + 1;
                let old = &previous.document.blocks[index];
                old.kind != new.kind || old.citation_ids != new.citation_ids
                    || (!flagged.contains(&old.block_id) && old != new)
                    || matches!((&old.rich, &new.rich), (Some(before), Some(after)) if !before.preserves_survivor_structure(after))
            });
            if (!title_flagged && previous.document.title != document.title)
                || changed_illegally
                || previous.document.blocks[next_position..]
                    .iter()
                    .any(|old| !flagged.contains(&old.block_id))
            {
                return Err(AppError::invalid_request(
                    "rich repair changed an unaffected block or evidence identity",
                ));
            }
        }
        if !self
            .checks
            .iter()
            .any(|c| c.revision_id == revision_id && c.findings.iter().any(|f| f.blocking))
        {
            return Err(AppError::conflict("current revision has no blocking check"));
        }
        let asset_id = item
            .asset_id
            .ok_or_else(|| AppError::conflict("repair asset is missing"))?;
        if !self
            .assets
            .iter()
            .any(|a| a.asset_id == asset_id && a.current_revision_id == revision_id)
        {
            return Err(AppError::conflict("repair base revision changed"));
        }
        document.validate(&previous.evidence)?;
        let next = ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id,
            revision: previous
                .revision
                .checked_add(1)
                .ok_or_else(|| AppError::conflict("too many content revisions"))?,
            base_revision_id: Some(revision_id),
            derived_from_revision_id: None,
            markdown: document.markdown(),
            document,
            evidence: previous.evidence.clone(),
            quotes: previous.quotes.clone(),
            findings: Vec::new(),
            created_at: Utc::now(),
        };
        self.consume(lease, ContentStep::Repair)?;
        let item = self.item_mut(lease.item_id)?;
        item.automatic_repair_count += 1;
        item.current_revision_id = Some(next.revision_id);
        item.ready_revision_id = None;
        item.status = ContentItemStatus::Drafted;
        item.reason = None;
        self.assets
            .iter_mut()
            .find(|a| a.asset_id == asset_id)
            .expect("asset was verified above")
            .current_revision_id = next.revision_id;
        self.revisions.push(next.clone());
        self.recount();
        Ok(next)
    }
    pub fn edit(
        &mut self,
        asset_id: Uuid,
        base_revision_id: Uuid,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        if self.execution.status == ContentExecutionStatus::Cancelled {
            return Err(AppError::conflict("cancelled content cannot be edited"));
        }
        let asset = self
            .assets
            .iter()
            .find(|a| a.asset_id == asset_id)
            .ok_or_else(|| AppError::not_found("content asset not found"))?
            .clone();
        if asset.current_revision_id != base_revision_id {
            return Err(AppError::conflict("base revision changed"));
        }
        let previous = self
            .revisions
            .iter()
            .find(|r| r.revision_id == base_revision_id)
            .ok_or_else(|| AppError::conflict("base revision is missing"))?;
        document.validate(&previous.evidence)?;
        let next = ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id,
            revision: previous.revision + 1,
            base_revision_id: Some(base_revision_id),
            derived_from_revision_id: None,
            markdown: document.markdown(),
            document,
            evidence: previous.evidence.clone(),
            quotes: previous.quotes.clone(),
            findings: Vec::new(),
            created_at: Utc::now(),
        };
        if self.execution.status == ContentExecutionStatus::Closed {
            if let Some(previous_handoff) = self.handoff.take()
                && !self
                    .handoffs
                    .iter()
                    .any(|h| h.handoff_id == previous_handoff.handoff_id)
            {
                self.handoffs.push(previous_handoff);
            }
            self.execution.status = ContentExecutionStatus::Running;
            self.execution.handoff_id = None;
        }
        self.assets
            .iter_mut()
            .find(|a| a.asset_id == asset_id)
            .unwrap()
            .current_revision_id = next.revision_id;
        let item = self.item_mut(asset.item_id)?;
        item.current_revision_id = Some(next.revision_id);
        item.ready_revision_id = None;
        item.reuse_reservation_token = None;
        item.status = ContentItemStatus::Drafted;
        item.reason = None;
        for attempt in &mut item.attempts {
            if item.steps.iter().any(|lease| lease.token == attempt.token)
                && attempt.outcome == ContentAttemptOutcome::Claimed
            {
                attempt.outcome = ContentAttemptOutcome::Superseded;
                attempt.finished_at = Some(Utc::now());
            }
        }
        item.steps.clear();
        self.revisions.push(next.clone());
        self.recount();
        Ok(next)
    }
    pub fn classify(
        &mut self,
        item_id: Uuid,
        status: ContentItemStatus,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.running()?;
        if !matches!(
            status,
            ContentItemStatus::Blocked
                | ContentItemStatus::Deferred
                | ContentItemStatus::NotApplicable
                | ContentItemStatus::Cancelled
        ) || reason.trim().is_empty()
        {
            return Err(AppError::invalid_request(
                "classification requires terminal status and reason",
            ));
        }
        let item = self.item_mut(item_id)?;
        if matches!(
            item.status,
            ContentItemStatus::Ready | ContentItemStatus::Cancelled
        ) {
            return Err(AppError::conflict("terminal item cannot be reclassified"));
        }
        if item.steps.iter().any(|lease| lease.expires_at > Utc::now()) {
            return Err(AppError::conflict("active step lease must be fenced"));
        }
        item.status = status;
        item.reason = Some(reason.to_owned());
        for attempt in &mut item.attempts {
            if item.steps.iter().any(|lease| lease.token == attempt.token)
                && attempt.outcome == ContentAttemptOutcome::Claimed
            {
                attempt.outcome = ContentAttemptOutcome::Expired;
                attempt.finished_at = Some(Utc::now());
            }
        }
        item.steps.clear();
        let result = item.clone();
        self.recount();
        Ok(result)
    }
    pub fn fail_step(&mut self, lease: &StepLease, reason: &str) -> Result<ContentItem, AppError> {
        if reason.trim().is_empty() {
            return Err(AppError::invalid_request("failure reason required"));
        }
        let item = self.consume(lease, lease.step)?;
        if let Some(attempt) = item.attempts.iter_mut().find(|a| a.token == lease.token) {
            attempt.outcome = ContentAttemptOutcome::Failed;
        }
        item.steps.clear();
        item.status = ContentItemStatus::Blocked;
        item.reason = Some(reason.to_owned());
        let result = item.clone();
        self.recount();
        Ok(result)
    }
    pub fn release_step(&mut self, lease: &StepLease) -> Result<ContentItem, AppError> {
        let item = self.consume(lease, lease.step)?;
        if let Some(attempt) = item.attempts.iter_mut().find(|a| a.token == lease.token) {
            attempt.outcome = ContentAttemptOutcome::Released;
        }
        if lease.step == ContentStep::Prepare {
            item.semantic_descriptor = None;
            item.semantic_fingerprint = None;
            item.reuse_reservation_token = None;
        }
        Ok(item.clone())
    }
    pub fn invalidate_ready(
        &mut self,
        item_id: Uuid,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.running()?;
        if reason.trim().is_empty() {
            return Err(AppError::invalid_request("invalidation reason required"));
        }
        let item = self.item_mut(item_id)?;
        if item.status != ContentItemStatus::Ready {
            return Err(AppError::conflict("only ready content can be invalidated"));
        }
        item.status = ContentItemStatus::Blocked;
        item.ready_revision_id = None;
        item.reason = Some(reason.to_owned());
        item.reuse_reservation_token = None;
        let result = item.clone();
        self.recount();
        Ok(result)
    }
    pub fn close(&mut self) -> Result<ContentHandoff, AppError> {
        if let Some(handoff) = &self.handoff {
            return Ok(handoff.clone());
        }
        self.running()?;
        self.recount();
        if self.execution.coverage.incomplete != 0 {
            return Err(AppError::conflict(
                "all denominator items need a terminal result",
            ));
        }
        let handoff = ContentHandoff {
            handoff_id: stable_id(&format!(
                "content-handoff:{}:{}",
                self.execution.execution_id,
                self.handoffs.len() + 1
            )),
            execution_id: self.execution.execution_id,
            revision: i32::try_from(self.handoffs.len() + 1)
                .map_err(|_| AppError::conflict("too many handoff revisions"))?,
            supersedes_handoff_id: self.handoffs.last().map(|h| h.handoff_id),
            coverage: self.execution.coverage.clone(),
            items: self
                .items
                .iter()
                .map(|i| ContentHandoffItem {
                    item_id: i.item_id,
                    document_key: i.document_key.clone(),
                    status: i.status,
                    reason: i.reason.clone(),
                    revision_id: i.ready_revision_id,
                })
                .collect(),
            created_at: Utc::now(),
        };
        self.execution.status = ContentExecutionStatus::Closed;
        self.execution.handoff_id = Some(handoff.handoff_id);
        self.handoff = Some(handoff.clone());
        self.handoffs.push(handoff.clone());
        Ok(handoff)
    }
    pub fn cancel(&mut self) -> Result<ContentExecution, AppError> {
        if self.execution.status == ContentExecutionStatus::Closed {
            return Err(AppError::conflict("closed execution cannot be cancelled"));
        }
        self.execution.status = ContentExecutionStatus::Cancelled;
        for item in &mut self.items {
            for attempt in &mut item.attempts {
                if item.steps.iter().any(|lease| lease.token == attempt.token)
                    && attempt.outcome == ContentAttemptOutcome::Claimed
                {
                    attempt.outcome = ContentAttemptOutcome::Cancelled;
                    attempt.finished_at = Some(Utc::now());
                }
            }
            item.steps.clear();
            if matches!(
                item.status,
                ContentItemStatus::Pending
                    | ContentItemStatus::Prepared
                    | ContentItemStatus::Drafted
                    | ContentItemStatus::NeedsRepair
            ) {
                item.status = ContentItemStatus::Cancelled;
                item.reason = Some("execution_cancelled".into());
            }
        }
        self.recount();
        Ok(self.execution.clone())
    }
}

#[async_trait]
pub trait ContentRepository: Send + Sync {
    async fn start(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        manifest: DocumentManifest,
        policy_version: &str,
    ) -> Result<ContentExecution, AppError>;
    async fn prepare_or_reuse(
        &self,
        scope: &TenantScope,
        request: ContentReuseRequest,
    ) -> Result<ContentReuseDecision, AppError>;
    async fn resolve_checked_revision(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<ContentRevision>, AppError>;
    async fn fork_reused_item(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        base_revision_id: Uuid,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError>;
    async fn get_execution(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<ContentExecution>, AppError>;
    async fn get_handoff(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
    ) -> Result<Option<ContentHandoff>, AppError>;
    async fn list_handoffs(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
    ) -> Result<Vec<ContentHandoff>, AppError>;
    async fn list_executions(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<Vec<ContentExecution>, AppError>;
    async fn get_item(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
    ) -> Result<Option<ContentItem>, AppError>;
    async fn list_items(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
    ) -> Result<Vec<ContentItem>, AppError>;
    async fn get_asset(
        &self,
        scope: &TenantScope,
        asset_id: Uuid,
    ) -> Result<Option<ContentAsset>, AppError>;
    async fn list_assets(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
    ) -> Result<Vec<ContentAsset>, AppError>;
    async fn list_project_assets(&self, scope: &TenantScope)
    -> Result<Vec<ContentAsset>, AppError>;
    async fn list_revisions(
        &self,
        scope: &TenantScope,
        asset_id: Uuid,
    ) -> Result<Vec<ContentRevision>, AppError>;
    async fn get_revision(
        &self,
        scope: &TenantScope,
        asset_id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<ContentRevision>, AppError>;
    async fn find_exact_child_revision(
        &self,
        scope: &TenantScope,
        asset_id: Uuid,
        base_revision_id: Uuid,
        document: &StructuredDocument,
    ) -> Result<Option<ContentRevision>, AppError>;
    async fn list_checks(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Vec<ContentCheck>, AppError>;
    #[allow(clippy::too_many_arguments)] // Explicit scope, branch, step and fencing clock.
    async fn claim(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        step: ContentStep,
        owner: &str,
        now: DateTime<Utc>,
        ttl_seconds: i64,
    ) -> Result<StepLease, AppError>;
    async fn complete_prepare(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        brief: ContentBrief,
    ) -> Result<ContentItem, AppError>;
    async fn complete_generate(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError>;
    async fn complete_check(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        findings: Vec<ContentFinding>,
    ) -> Result<ContentItem, AppError>;
    async fn complete_repair(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError>;
    async fn edit(
        &self,
        scope: &TenantScope,
        asset_id: Uuid,
        base_revision_id: Uuid,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError>;
    async fn classify(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        status: ContentItemStatus,
        reason: &str,
    ) -> Result<ContentItem, AppError>;
    async fn fail_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        reason: &str,
    ) -> Result<ContentItem, AppError>;
    async fn release_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
    ) -> Result<ContentItem, AppError>;
    async fn invalidate_ready(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        reason: &str,
    ) -> Result<ContentItem, AppError>;
    async fn close(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
    ) -> Result<ContentHandoff, AppError>;
    async fn cancel(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
    ) -> Result<ContentExecution, AppError>;
}

pub struct MemoryContentRepository {
    state: RwLock<HashMap<Uuid, (TenantScope, ContentState)>>,
    candidates: std::sync::Mutex<HashMap<(TenantScope, String), ContentReuseCandidate>>,
    reservations: std::sync::Mutex<HashMap<(TenantScope, String), ContentProducerReservation>>,
    media_repository: Arc<MemoryContentMediaRepository>,
}

impl Default for MemoryContentRepository {
    fn default() -> Self {
        Self::with_media_repository(Arc::new(MemoryContentMediaRepository::default()))
    }
}

#[derive(Clone)]
struct ContentProducerReservation {
    execution_id: Uuid,
    item_id: Uuid,
    token: Uuid,
    expires_at: DateTime<Utc>,
}
impl MemoryContentRepository {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_media_repository(media_repository: Arc<MemoryContentMediaRepository>) -> Self {
        Self {
            state: RwLock::new(HashMap::new()),
            candidates: std::sync::Mutex::new(HashMap::new()),
            reservations: std::sync::Mutex::new(HashMap::new()),
            media_repository,
        }
    }
    async fn mutate<T>(
        &self,
        scope: &TenantScope,
        id: Uuid,
        f: impl FnOnce(&mut ContentState) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let media = self.media_repository.read_guard().await;
        let mut guard = self.state.write().await;
        let (stored_scope, state) = guard
            .get(&id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let mut candidate = state.clone();
        let result = f(&mut candidate)?;
        media.validate(stored_scope, &affected_media_keys(state, &candidate))?;
        // A reused Ready item points at the origin execution's immutable
        // revision, not a revision copied into this execution.
        for item in candidate.items.iter().filter(|item| {
            item.status == ContentItemStatus::Ready
                && item.reuse_binding.is_some()
                && (state
                    .items
                    .iter()
                    .find(|prior| prior.item_id == item.item_id)
                    .is_none_or(|prior| {
                        prior.status != ContentItemStatus::Ready
                            || prior.ready_revision_id != item.ready_revision_id
                    })
                    || candidate.handoffs.len() > state.handoffs.len())
        }) {
            media.validate(
                stored_scope,
                &selected_media_keys(&guard, stored_scope, &candidate, item)?,
            )?;
        }
        guard.get_mut(&id).expect("previously checked").1 = candidate;
        Ok(result)
    }
}
#[async_trait]
impl ContentRepository for MemoryContentRepository {
    async fn start(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        manifest: DocumentManifest,
        policy_version: &str,
    ) -> Result<ContentExecution, AppError> {
        let state = start_content_state(scope, cycle_id, &manifest, policy_version)?;
        let mut guard = self.state.write().await;
        if let Some((_, existing)) = guard.get(&state.execution.execution_id) {
            if existing.execution.input_hash != state.execution.input_hash
                || existing
                    .items
                    .iter()
                    .map(|i| &i.input_hash)
                    .collect::<Vec<_>>()
                    != state
                        .items
                        .iter()
                        .map(|i| &i.input_hash)
                        .collect::<Vec<_>>()
            {
                return Err(AppError::conflict("content execution replay inputs differ"));
            }
            return Ok(existing.execution.clone());
        }
        let result = state.execution.clone();
        guard.insert(result.execution_id, (scope.clone(), state));
        Ok(result)
    }
    async fn prepare_or_reuse(
        &self,
        scope: &TenantScope,
        request: ContentReuseRequest,
    ) -> Result<ContentReuseDecision, AppError> {
        let media = self.media_repository.read_guard().await;
        if scope != &request.descriptor.scope {
            return Err(AppError::forbidden(
                "semantic input is outside project scope",
            ));
        }
        let descriptor = request.descriptor.canonical()?;
        let fingerprint = descriptor.fingerprint()?;
        let key = (scope.clone(), fingerprint.clone());
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get(&request.execution_id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if stored_scope != scope {
            return Err(AppError::not_found("content execution not found"));
        }
        let dest = state
            .items
            .iter()
            .find(|i| i.item_id == request.item_id)
            .ok_or_else(|| AppError::not_found("content item not found"))?;
        if state.execution.project_id != descriptor.scope.project_id.unwrap()
            || state.execution.policy_version != descriptor.generation_policy_version
            || dest.document_key != descriptor.document_key
            || dest.content_type != descriptor.content_type
            || dest.product_id != descriptor.product_id
            || dest.market != descriptor.market
            || dest.language != descriptor.language
            || dest.planner_version != descriptor.planner_version
            || {
                let mut refs = dest.source_version_refs.clone();
                refs.sort_unstable();
                refs.dedup();
                refs != descriptor.source_version_ids
            }
        {
            return Err(AppError::conflict(
                "semantic input differs from execution branch",
            ));
        }
        if !(1..=3600).contains(&request.ttl_seconds)
            || request.now + Duration::seconds(request.ttl_seconds) <= Utc::now()
        {
            return Err(AppError::invalid_request(
                "valid current reservation lifetime required",
            ));
        }
        if let Some(old) = &dest.semantic_descriptor
            && old != &descriptor
        {
            return Err(AppError::conflict(
                "semantic descriptor changed for reserved branch",
            ));
        }
        if dest.status == ContentItemStatus::Ready {
            return if dest.semantic_fingerprint.as_deref() == Some(&fingerprint) {
                media.validate(scope, &selected_media_keys(&states, scope, state, dest)?)?;
                Ok(ContentReuseDecision::Ready(dest.clone()))
            } else {
                Err(AppError::conflict("ready content input differs"))
            };
        }
        if dest.status != ContentItemStatus::Pending {
            return Ok(ContentReuseDecision::Busy(dest.clone()));
        }
        if dest
            .steps
            .iter()
            .any(|lease| lease.expires_at > request.now)
        {
            return Ok(ContentReuseDecision::Busy(dest.clone()));
        }
        let candidate = self
            .candidates
            .lock()
            .map_err(|_| AppError::conflict("candidate registry unavailable"))?
            .get(&key)
            .cloned();
        if let Some(candidate) = candidate {
            if candidate.descriptor != descriptor {
                return Err(AppError::conflict(
                    "semantic digest collides with another input",
                ));
            }
            let origin = states
                .get(&candidate.origin_execution_id)
                .filter(|(stored_scope, _)| stored_scope == scope)
                .map(|(_, state)| state)
                .ok_or_else(|| AppError::conflict("reuse origin is missing"))?;
            let valid = origin.execution.status != ContentExecutionStatus::Cancelled
                && origin.items.iter().any(|item| {
                    item.item_id == candidate.origin_item_id
                        && item.status == ContentItemStatus::Ready
                        && item.ready_revision_id == Some(candidate.revision_id)
                        && item.asset_id == Some(candidate.asset_id)
                })
                && origin.assets.iter().any(|asset| {
                    asset.asset_id == candidate.asset_id
                        && asset.item_id == candidate.origin_item_id
                        && asset.current_revision_id == candidate.revision_id
                })
                && origin.revisions.iter().any(|r| {
                    r.revision_id == candidate.revision_id && r.asset_id == candidate.asset_id
                })
                && origin.checks.iter().any(|c| {
                    c.check_id == candidate.check_id
                        && c.revision_id == candidate.revision_id
                        && !c.findings.iter().any(|f| f.blocking)
                });
            if !valid {
                return Err(AppError::conflict("reuse origin is no longer current"));
            }
            let origin_revision = origin
                .revisions
                .iter()
                .find(|revision| revision.revision_id == candidate.revision_id)
                .expect("validated origin revision");
            media.validate(scope, &media_keys_for_revision(origin_revision))?;
            let binding = ContentReuseBinding {
                origin_execution_id: candidate.origin_execution_id,
                origin_item_id: candidate.origin_item_id,
                asset_id: candidate.asset_id,
                revision_id: candidate.revision_id,
                check_id: candidate.check_id,
                fingerprint: fingerprint.clone(),
                reused_at: request.now,
            };
            let item = &mut states
                .get_mut(&request.execution_id)
                .expect("destination was verified")
                .1;
            return item
                .apply_reuse(request.item_id, binding, descriptor, &fingerprint)
                .map(ContentReuseDecision::Ready);
        }
        let mut refs = dest.source_version_refs.clone();
        refs.sort_unstable();
        refs.dedup();
        if states.values().any(|(stored_scope, previous)| {
            stored_scope == scope
                && previous.execution.execution_id != request.execution_id
                && previous.items.iter().any(|item| {
                    item.document_key == descriptor.document_key
                        && item.status == ContentItemStatus::Ready
                        && item.semantic_descriptor.is_none()
                        && {
                            let mut previous_refs = item.source_version_refs.clone();
                            previous_refs.sort_unstable();
                            previous_refs.dedup();
                            previous_refs == refs
                        }
                })
        }) {
            let state = &mut states
                .get_mut(&request.execution_id)
                .expect("destination was verified")
                .1;
            let blocked = state.classify(
                request.item_id,
                ContentItemStatus::Blocked,
                "reuse_provenance_insufficient",
            )?;
            return Ok(ContentReuseDecision::InsufficientEvidence(blocked));
        }
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
        if let Some(reservation) = reservations.get(&key).cloned() {
            let previous_item = states
                .get(&reservation.execution_id)
                .filter(|(stored_scope, _)| stored_scope == scope)
                .and_then(|(_, state)| {
                    state
                        .items
                        .iter()
                        .find(|item| item.item_id == reservation.item_id)
                });
            let effective_expiry = previous_item
                .into_iter()
                .flat_map(|item| item.steps.iter().map(|lease| lease.expires_at))
                .fold(reservation.expires_at, DateTime::<Utc>::max);
            if effective_expiry > request.now {
                return Ok(ContentReuseDecision::Busy(dest.clone()));
            }
            if (reservation.execution_id, reservation.item_id)
                != (request.execution_id, request.item_id)
                && let Some((_, previous)) = states.get_mut(&reservation.execution_id)
                && previous.execution.status == ContentExecutionStatus::Running
                && previous.items.iter().any(|item| {
                    item.item_id == reservation.item_id
                        && matches!(
                            item.status,
                            ContentItemStatus::Pending
                                | ContentItemStatus::Prepared
                                | ContentItemStatus::Drafted
                                | ContentItemStatus::NeedsRepair
                        )
                })
            {
                previous.classify(
                    reservation.item_id,
                    ContentItemStatus::Blocked,
                    "reuse_reservation_superseded",
                )?;
            }
            reservations.remove(&key);
        }
        let state = &mut states
            .get_mut(&request.execution_id)
            .expect("destination was verified")
            .1;
        let lease = state.claim(
            request.item_id,
            ContentStep::Prepare,
            &request.owner,
            request.now,
            request.ttl_seconds,
        )?;
        let item = state.set_semantic_descriptor(&lease, descriptor, &fingerprint)?;
        reservations.insert(
            key,
            ContentProducerReservation {
                execution_id: request.execution_id,
                item_id: request.item_id,
                token: lease.token,
                expires_at: lease.expires_at,
            },
        );
        Ok(ContentReuseDecision::Reserved { item, lease })
    }
    async fn resolve_checked_revision(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<ContentRevision>, AppError> {
        let states = self.state.read().await;
        let (stored_scope, state) = match states.get(&execution_id) {
            Some(entry) if scope.contains(&entry.0) => entry,
            _ => return Ok(None),
        };
        let item = match state.items.iter().find(|item| item.item_id == item_id) {
            Some(item) => item,
            None => return Ok(None),
        };
        let (origin, owner_item_id, check_id, asset_id, reused) = if let Some(binding) = item
            .reuse_history
            .iter()
            .find(|binding| binding.revision_id == revision_id)
        {
            let origin = states
                .get(&binding.origin_execution_id)
                .filter(|(origin_scope, _)| origin_scope == stored_scope)
                .map(|(_, state)| state)
                .ok_or_else(|| AppError::conflict("reuse origin is outside project scope"))?;
            (
                origin,
                binding.origin_item_id,
                Some(binding.check_id),
                binding.asset_id,
                true,
            )
        } else {
            let asset_id = state
                .revisions
                .iter()
                .find(|revision| revision.revision_id == revision_id)
                .map(|revision| revision.asset_id);
            let Some(asset_id) = asset_id else {
                return Ok(None);
            };
            (state, item_id, None, asset_id, false)
        };
        if !origin.items.iter().any(|owner| {
            owner.item_id == owner_item_id
                && owner.asset_id == Some(asset_id)
                && (reused
                    || owner.ready_revision_id == Some(revision_id)
                    || origin.handoffs.iter().any(|handoff| {
                        handoff.items.iter().any(|handed| {
                            handed.item_id == owner_item_id
                                && handed.status == ContentItemStatus::Ready
                                && handed.revision_id == Some(revision_id)
                        })
                    }))
        }) || !origin.assets.iter().any(|asset| {
            asset.asset_id == asset_id
                && asset.item_id == owner_item_id
                && (reused
                    || asset.current_revision_id == revision_id
                    || origin.handoffs.iter().any(|handoff| {
                        handoff.items.iter().any(|handed| {
                            handed.item_id == owner_item_id
                                && handed.status == ContentItemStatus::Ready
                                && handed.revision_id == Some(revision_id)
                        })
                    }))
        }) || !origin.checks.iter().any(|check| {
            check.revision_id == revision_id
                && check_id.is_none_or(|id| check.check_id == id)
                && !check.findings.iter().any(|finding| finding.blocking)
        }) {
            return Err(AppError::conflict("origin checked revision changed"));
        }
        let check = origin
            .checks
            .iter()
            .find(|check| {
                check.revision_id == revision_id
                    && check_id.is_none_or(|id| check.check_id == id)
                    && !check.findings.iter().any(|finding| finding.blocking)
            })
            .ok_or_else(|| AppError::conflict("successful origin check is missing"))?;
        let revision = origin
            .revisions
            .iter()
            .find(|revision| revision.revision_id == revision_id && revision.asset_id == asset_id)
            .ok_or_else(|| AppError::conflict("checked revision is missing"))?;
        let mut revision = revision.clone();
        revision.findings = check.findings.clone();
        Ok(Some(revision))
    }
    async fn fork_reused_item(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        base_revision_id: Uuid,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        let media = self.media_repository.read_guard().await;
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get(&execution_id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let binding = state
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .and_then(|item| item.reuse_binding.as_ref())
            .ok_or_else(|| AppError::conflict("checked reused revision required"))?;
        let origin_state = states
            .get(&binding.origin_execution_id)
            .filter(|(origin_scope, _)| origin_scope == stored_scope)
            .map(|(_, state)| state)
            .ok_or_else(|| AppError::conflict("reuse origin is outside project scope"))?;
        let origin = origin_state
            .revisions
            .iter()
            .find(|revision| {
                revision.revision_id == binding.revision_id && revision.asset_id == binding.asset_id
            })
            .ok_or_else(|| AppError::conflict("reused revision is missing"))?
            .clone();
        if !origin_state.checks.iter().any(|check| {
            check.check_id == binding.check_id
                && check.revision_id == origin.revision_id
                && !check.findings.iter().any(|finding| finding.blocking)
        }) {
            return Err(AppError::conflict("reused check is missing"));
        }
        let (_, state) = states
            .get_mut(&execution_id)
            .expect("destination was verified");
        let mut next = state.clone();
        let result = next.fork_reused_item(item_id, base_revision_id, &origin, document)?;
        media.validate(scope, &affected_media_keys(state, &next))?;
        *state = next;
        Ok(result)
    }
    async fn get_execution(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<ContentExecution>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .get(&id)
            .filter(|(s, _)| scope.contains(s))
            .map(|(_, s)| s.execution.clone()))
    }
    async fn get_handoff(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<ContentHandoff>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .get(&id)
            .filter(|(s, _)| scope.contains(s))
            .and_then(|(_, v)| v.handoff.clone()))
    }
    async fn list_handoffs(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Vec<ContentHandoff>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .get(&id)
            .filter(|(s, _)| scope.contains(s))
            .map(|(_, v)| v.handoffs.clone())
            .unwrap_or_default())
    }
    async fn list_executions(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<Vec<ContentExecution>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .values()
            .filter(|(s, v)| scope.contains(s) && v.execution.cycle_id == cycle_id)
            .map(|(_, v)| v.execution.clone())
            .collect())
    }
    async fn get_item(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
    ) -> Result<Option<ContentItem>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .get(&id)
            .filter(|(s, _)| scope.contains(s))
            .and_then(|(_, v)| v.items.iter().find(|i| i.item_id == item).cloned()))
    }
    async fn list_items(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Vec<ContentItem>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .get(&id)
            .filter(|(s, _)| scope.contains(s))
            .map(|(_, v)| v.items.clone())
            .unwrap_or_default())
    }
    async fn get_asset(
        &self,
        scope: &TenantScope,
        asset: Uuid,
    ) -> Result<Option<ContentAsset>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .values()
            .filter(|(s, _)| scope.contains(s))
            .flat_map(|(_, v)| &v.assets)
            .find(|a| a.asset_id == asset)
            .cloned())
    }
    async fn list_assets(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Vec<ContentAsset>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .get(&id)
            .filter(|(s, _)| scope.contains(s))
            .map(|(_, v)| v.assets.clone())
            .unwrap_or_default())
    }
    async fn list_project_assets(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<ContentAsset>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .values()
            .filter(|(s, _)| scope.contains(s))
            .flat_map(|(_, v)| v.assets.clone())
            .collect())
    }
    async fn list_revisions(
        &self,
        scope: &TenantScope,
        asset: Uuid,
    ) -> Result<Vec<ContentRevision>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .values()
            .filter(|(s, _)| scope.contains(s))
            .flat_map(|(_, v)| {
                v.revisions
                    .iter()
                    .filter(move |r| r.asset_id == asset)
                    .map(move |r| {
                        let mut result = r.clone();
                        if let Some(check) =
                            v.checks.iter().find(|c| c.revision_id == r.revision_id)
                        {
                            result.findings = check.findings.clone();
                        }
                        result
                    })
            })
            .collect())
    }
    async fn get_revision(
        &self,
        scope: &TenantScope,
        asset_id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<ContentRevision>, AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::invalid_request("project scope required"));
        }
        let states = self.state.read().await;
        Ok(states
            .values()
            .filter(|(stored_scope, state)| {
                scope == stored_scope && state.assets.iter().any(|a| a.asset_id == asset_id)
            })
            .find_map(|(_, state)| {
                state
                    .revisions
                    .iter()
                    .find(|r| r.asset_id == asset_id && r.revision_id == revision_id)
                    .map(|revision| {
                        let mut result = revision.clone();
                        if let Some(check) = state
                            .checks
                            .iter()
                            .find(|check| check.revision_id == revision_id)
                        {
                            result.findings = check.findings.clone();
                        }
                        result
                    })
            }))
    }
    async fn find_exact_child_revision(
        &self,
        scope: &TenantScope,
        asset_id: Uuid,
        base_revision_id: Uuid,
        document: &StructuredDocument,
    ) -> Result<Option<ContentRevision>, AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::invalid_request("project scope required"));
        }
        let states = self.state.read().await;
        let matched = states
            .values()
            .filter(|(stored_scope, state)| {
                scope == stored_scope && state.assets.iter().any(|a| a.asset_id == asset_id)
            })
            .flat_map(|(_, state)| {
                state
                    .revisions
                    .iter()
                    .filter(move |revision| {
                        revision.asset_id == asset_id
                            && (revision.base_revision_id == Some(base_revision_id)
                                || revision.derived_from_revision_id == Some(base_revision_id))
                            && &revision.document == document
                    })
                    .map(move |revision| (state, revision))
            })
            .min_by_key(|(_, revision)| (revision.revision, revision.revision_id));
        Ok(matched.map(|(state, revision)| {
            let mut result = revision.clone();
            if let Some(check) = state
                .checks
                .iter()
                .find(|check| check.revision_id == revision.revision_id)
            {
                result.findings = check.findings.clone();
            }
            result
        }))
    }
    async fn list_checks(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Vec<ContentCheck>, AppError> {
        Ok(self
            .state
            .read()
            .await
            .values()
            .filter(|(stored_scope, state)| {
                scope.contains(stored_scope)
                    && state.revisions.iter().any(|r| r.revision_id == revision_id)
            })
            .flat_map(|(_, state)| {
                state
                    .checks
                    .iter()
                    .filter(move |c| c.revision_id == revision_id)
            })
            .cloned()
            .collect())
    }
    async fn claim(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
        step: ContentStep,
        owner: &str,
        now: DateTime<Utc>,
        ttl: i64,
    ) -> Result<StepLease, AppError> {
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let reservation = state
            .items
            .iter()
            .find(|current| current.item_id == item)
            .and_then(|current| {
                current
                    .semantic_fingerprint
                    .as_ref()
                    .zip(current.reuse_reservation_token)
            })
            .map(|(fingerprint, token)| ((stored_scope.clone(), fingerprint.clone()), token));
        let fresh_key = (step == ContentStep::Check)
            .then(|| {
                state
                    .items
                    .iter()
                    .find(|current| current.item_id == item)
                    .filter(|current| {
                        current.semantic_descriptor.is_some()
                            && current.reuse_reservation_token.is_none()
                    })
                    .and_then(|current| current.semantic_fingerprint.as_ref())
                    .map(|fingerprint| (stored_scope.clone(), fingerprint.clone()))
            })
            .flatten();
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
        if let Some((key, token)) = &reservation
            && !reservations.get(key).is_some_and(|held| {
                held.execution_id == id && held.item_id == item && held.token == *token
            })
        {
            return Err(AppError::conflict(
                "content producer reservation was superseded",
            ));
        }
        if let Some(key) = &fresh_key {
            if reservations
                .get(key)
                .is_some_and(|held| held.expires_at > now)
            {
                return Err(AppError::conflict("content producer is already reserved"));
            }
            reservations.remove(key);
        }
        let lease = state.claim(item, step, owner, now, ttl)?;
        if let Some((key, _)) = reservation
            && let Some(held) = reservations.get_mut(&key)
        {
            held.expires_at = lease.expires_at;
        }
        if let Some(key) = fresh_key {
            let token = Uuid::new_v4();
            state.item_mut(item)?.reuse_reservation_token = Some(token);
            reservations.insert(
                key,
                ContentProducerReservation {
                    execution_id: id,
                    item_id: item,
                    token,
                    expires_at: lease.expires_at,
                },
            );
        }
        Ok(lease)
    }
    async fn complete_prepare(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        brief: ContentBrief,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_prepare(lease, brief)
        })
        .await
    }
    async fn complete_generate(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        doc: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_generate(lease, doc)
        })
        .await
    }
    async fn complete_check(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        findings: Vec<ContentFinding>,
    ) -> Result<ContentItem, AppError> {
        let media = self.media_repository.read_guard().await;
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&lease.execution_id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let reserving = state
            .items
            .iter()
            .find(|item| item.item_id == lease.item_id)
            .and_then(|item| {
                item.semantic_fingerprint
                    .as_ref()
                    .zip(item.reuse_reservation_token)
            })
            .map(|(fingerprint, token)| ((stored_scope.clone(), fingerprint.clone()), token));
        if state.items.iter().any(|item| {
            item.item_id == lease.item_id
                && item.semantic_descriptor.is_some()
                && reserving.is_none()
        }) {
            return Err(AppError::conflict(
                "checked semantic input lacks producer reservation",
            ));
        }
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
        if let Some((key, token)) = &reserving
            && !reservations.get(key).is_some_and(|held| {
                held.execution_id == lease.execution_id
                    && held.item_id == lease.item_id
                    && held.token == *token
            })
        {
            return Err(AppError::conflict(
                "content producer reservation was superseded",
            ));
        }
        let mut next = state.clone();
        let result = next.complete_check(lease, findings)?;
        media.validate(scope, &affected_media_keys(state, &next))?;
        if result.status == ContentItemStatus::Ready
            && let (Some(descriptor), Some(fingerprint), Some(asset_id), Some(revision_id)) = (
                result.semantic_descriptor.clone(),
                result.semantic_fingerprint.clone(),
                result.asset_id,
                result.ready_revision_id,
            )
        {
            let check = next
                .checks
                .iter()
                .find(|check| {
                    check.revision_id == revision_id && !check.findings.iter().any(|f| f.blocking)
                })
                .ok_or_else(|| AppError::conflict("successful check is missing"))?;
            let candidate = ContentReuseCandidate {
                descriptor,
                fingerprint: fingerprint.clone(),
                origin_execution_id: lease.execution_id,
                origin_item_id: lease.item_id,
                asset_id,
                revision_id,
                check_id: check.check_id,
                created_at: check.created_at,
            };
            let mut registry = self
                .candidates
                .lock()
                .map_err(|_| AppError::conflict("candidate registry unavailable"))?;
            let key = (stored_scope.clone(), fingerprint);
            if registry
                .get(&key)
                .is_some_and(|prior| prior.descriptor != candidate.descriptor)
            {
                return Err(AppError::conflict("semantic digest collision"));
            }
            registry.insert(key, candidate);
        }
        if matches!(
            result.status,
            ContentItemStatus::Ready | ContentItemStatus::Blocked
        ) && let Some((key, _)) = reserving
        {
            reservations.remove(&key);
        }
        *state = next;
        Ok(result)
    }
    async fn complete_repair(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        document: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_repair(lease, document)
        })
        .await
    }
    async fn edit(
        &self,
        scope: &TenantScope,
        asset: Uuid,
        base: Uuid,
        doc: StructuredDocument,
    ) -> Result<ContentRevision, AppError> {
        let id = self
            .state
            .read()
            .await
            .values()
            .find(|(sc, s)| scope.contains(sc) && s.assets.iter().any(|a| a.asset_id == asset))
            .map(|(_, s)| s.execution.execution_id)
            .ok_or_else(|| AppError::not_found("asset not found"))?;
        let media = self.media_repository.read_guard().await;
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let previous_candidate = state
            .items
            .iter()
            .find(|item| item.asset_id == Some(asset))
            .and_then(|item| {
                item.semantic_fingerprint
                    .as_ref()
                    .map(|fingerprint| ((stored_scope.clone(), fingerprint.clone()), item.item_id))
            });
        let mut next = state.clone();
        let result = next.edit(asset, base, doc)?;
        media.validate(scope, &affected_media_keys(state, &next))?;
        if let Some((key, edited_item_id)) = previous_candidate {
            let mut registry = self
                .candidates
                .lock()
                .map_err(|_| AppError::conflict("candidate registry unavailable"))?;
            if registry
                .get(&key)
                .is_some_and(|candidate| candidate.asset_id == asset)
            {
                registry.remove(&key);
            }
            let mut reservations = self
                .reservations
                .lock()
                .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
            if reservations.get(&key).is_some_and(|reservation| {
                reservation.execution_id == id && reservation.item_id == edited_item_id
            }) {
                reservations.remove(&key);
            }
        }
        *state = next;
        Ok(result)
    }
    async fn classify(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
        status: ContentItemStatus,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let key = state
            .items
            .iter()
            .find(|current| current.item_id == item)
            .and_then(|current| current.semantic_fingerprint.as_ref())
            .map(|fingerprint| (stored_scope.clone(), fingerprint.clone()));
        let result = state.classify(item, status, reason)?;
        if let Some(key) = key {
            let mut reservations = self
                .reservations
                .lock()
                .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
            if reservations.get(&key).is_some_and(|reservation| {
                reservation.execution_id == id && reservation.item_id == item
            }) {
                reservations.remove(&key);
            }
        }
        Ok(result)
    }
    async fn fail_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&lease.execution_id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let key = state
            .items
            .iter()
            .find(|item| item.item_id == lease.item_id)
            .and_then(|item| item.semantic_fingerprint.as_ref())
            .map(|fingerprint| (stored_scope.clone(), fingerprint.clone()));
        let result = state.fail_step(lease, reason)?;
        if let Some(key) = key {
            let mut reservations = self
                .reservations
                .lock()
                .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
            if reservations.get(&key).is_some_and(|reservation| {
                reservation.execution_id == lease.execution_id
                    && reservation.item_id == lease.item_id
            }) {
                reservations.remove(&key);
            }
        }
        Ok(result)
    }
    async fn release_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
    ) -> Result<ContentItem, AppError> {
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&lease.execution_id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let key = (lease.step == ContentStep::Prepare)
            .then(|| {
                state
                    .items
                    .iter()
                    .find(|item| item.item_id == lease.item_id)
                    .and_then(|item| item.semantic_fingerprint.as_ref())
                    .map(|fingerprint| (stored_scope.clone(), fingerprint.clone()))
            })
            .flatten();
        let result = state.release_step(lease)?;
        if let Some(key) = key {
            let mut reservations = self
                .reservations
                .lock()
                .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
            if reservations.get(&key).is_some_and(|reservation| {
                reservation.execution_id == lease.execution_id
                    && reservation.item_id == lease.item_id
            }) {
                reservations.remove(&key);
            }
        }
        Ok(result)
    }
    async fn invalidate_ready(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&execution_id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let candidate_key = state
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .and_then(|item| item.semantic_fingerprint.clone())
            .map(|fingerprint| (stored_scope.clone(), fingerprint));
        let result = state.invalidate_ready(item_id, reason)?;
        if let Some(key) = candidate_key {
            let mut registry = self
                .candidates
                .lock()
                .map_err(|_| AppError::conflict("candidate registry unavailable"))?;
            if registry.get(&key).is_some_and(|candidate| {
                candidate.origin_execution_id == execution_id && candidate.origin_item_id == item_id
            }) {
                registry.remove(&key);
            }
        }
        Ok(result)
    }
    async fn close(&self, scope: &TenantScope, id: Uuid) -> Result<ContentHandoff, AppError> {
        self.mutate(scope, id, ContentState::close).await
    }
    async fn cancel(&self, scope: &TenantScope, id: Uuid) -> Result<ContentExecution, AppError> {
        let mut states = self.state.write().await;
        let (stored_scope, state) = states
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        let keys: Vec<_> = state
            .items
            .iter()
            .filter_map(|item| item.semantic_fingerprint.as_ref())
            .map(|fingerprint| (stored_scope.clone(), fingerprint.clone()))
            .collect();
        let result = state.cancel()?;
        self.candidates
            .lock()
            .map_err(|_| AppError::conflict("candidate registry unavailable"))?
            .retain(|_, candidate| candidate.origin_execution_id != id);
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| AppError::conflict("content reservation registry unavailable"))?;
        for key in keys {
            if reservations
                .get(&key)
                .is_some_and(|reservation| reservation.execution_id == id)
            {
                reservations.remove(&key);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod revision_lookup_tests {
    use super::*;
    use crate::ErrorCode;

    fn scope() -> TenantScope {
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        )
    }

    fn document(text: &str) -> StructuredDocument {
        StructuredDocument {
            title: "Title".into(),
            blocks: vec![ContentBlock {
                block_id: Uuid::new_v4(),
                kind: ContentBlockKind::Paragraph,
                text: text.into(),
                citation_ids: vec![],
                items: vec![],
                rich: None,
            }],
            schema_version: Some(2),
        }
    }

    fn revision(
        asset_id: Uuid,
        revision: i32,
        base_revision_id: Option<Uuid>,
        derived_from_revision_id: Option<Uuid>,
        document: StructuredDocument,
    ) -> ContentRevision {
        ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id,
            revision,
            base_revision_id,
            derived_from_revision_id,
            markdown: document.markdown(),
            document,
            evidence: vec![],
            quotes: vec![],
            findings: vec![],
            created_at: Utc::now(),
        }
    }

    async fn insert(
        repo: &MemoryContentRepository,
        scope: TenantScope,
        asset_id: Uuid,
        revisions: Vec<ContentRevision>,
        checks: Vec<ContentCheck>,
    ) {
        let execution_id = Uuid::new_v4();
        let coverage = ContentCoverage {
            total: 0,
            ready: 0,
            blocked: 0,
            deferred: 0,
            not_applicable: 0,
            cancelled: 0,
            incomplete: 0,
        };
        let state = ContentState {
            execution: ContentExecution {
                execution_id,
                project_id: scope.project_id.expect("test scope has a project"),
                cycle_id: Uuid::new_v4(),
                manifest_id: Uuid::new_v4(),
                manifest_revision: 1,
                policy_version: "test".into(),
                input_hash: "test".into(),
                status: ContentExecutionStatus::Running,
                expected_count: 0,
                coverage,
                handoff_id: None,
            },
            items: vec![],
            assets: vec![ContentAsset {
                asset_id,
                execution_id,
                item_id: Uuid::new_v4(),
                current_revision_id: revisions.last().expect("test has revisions").revision_id,
                created_at: Utc::now(),
            }],
            revisions,
            checks,
            handoff: None,
            handoffs: vec![],
        };
        repo.state
            .write()
            .await
            .insert(execution_id, (scope, state));
    }

    #[tokio::test]
    async fn get_revision_requires_owned_asset_and_exact_project_and_overlays_findings() {
        let repo = MemoryContentRepository::new();
        let scope = scope();
        let foreign_project = TenantScope {
            project_id: Some(Uuid::new_v4().into()),
            ..scope.clone()
        };
        let foreign_tenant = TenantScope {
            tenant_id: Uuid::new_v4().into(),
            ..scope.clone()
        };
        let asset_id = Uuid::new_v4();
        let foreign_asset = Uuid::new_v4();
        let target = revision(asset_id, 1, None, None, document("target"));
        let finding = ContentFinding {
            finding_id: Uuid::new_v4(),
            code: "checked".into(),
            block_id: None,
            evidence: vec![],
            detail: "finding".into(),
            blocking: false,
        };
        let checks = vec![ContentCheck {
            check_id: Uuid::new_v4(),
            revision_id: target.revision_id,
            findings: vec![finding.clone()],
            created_at: Utc::now(),
        }];
        let mut history = vec![target.clone()];
        for index in 2..=1_000 {
            history.push(revision(asset_id, index, None, None, document("history")));
        }
        insert(&repo, scope.clone(), asset_id, history, checks).await;
        insert(
            &repo,
            foreign_project.clone(),
            foreign_asset,
            vec![revision(foreign_asset, 1, None, None, document("foreign"))],
            vec![],
        )
        .await;
        let found = repo
            .get_revision(&scope, asset_id, target.revision_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.revision_id, target.revision_id);
        assert_eq!(found.findings, vec![finding]);
        assert!(
            repo.get_revision(&scope, foreign_asset, target.revision_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.get_revision(&foreign_project, asset_id, target.revision_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.get_revision(&foreign_tenant, asset_id, target.revision_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.get_revision(&scope, asset_id, Uuid::new_v4())
                .await
                .unwrap()
                .is_none()
        );
        let tenant_wide = TenantScope {
            project_id: None,
            ..scope.clone()
        };
        assert_eq!(
            repo.get_revision(&tenant_wide, asset_id, target.revision_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }

    #[tokio::test]
    async fn exact_child_matches_whole_document_and_immediate_lineage_in_revision_order() {
        let repo = MemoryContentRepository::new();
        let scope = scope();
        let foreign_scope = TenantScope {
            project_id: Some(Uuid::new_v4().into()),
            ..scope.clone()
        };
        let asset_id = Uuid::new_v4();
        let wrong_asset_id = Uuid::new_v4();
        let base = Uuid::new_v4();
        let target_document = document("identical");
        let mut changed_block = target_document.clone();
        changed_block.blocks[0].block_id = Uuid::new_v4();
        let mut changed_schema = target_document.clone();
        changed_schema.schema_version = None;
        let mut changed_citation = target_document.clone();
        changed_citation.blocks[0].citation_ids.push(Uuid::new_v4());
        let first = revision(asset_id, 12, Some(base), None, target_document.clone());
        let derived = revision(asset_id, 13, None, Some(base), target_document.clone());
        let derived_only_base = Uuid::new_v4();
        let derived_only = revision(
            asset_id,
            14,
            None,
            Some(derived_only_base),
            target_document.clone(),
        );
        let descendant = revision(
            asset_id,
            2,
            Some(derived.revision_id),
            None,
            target_document.clone(),
        );
        let finding = ContentFinding {
            finding_id: Uuid::new_v4(),
            code: "checked".into(),
            block_id: None,
            evidence: vec![],
            detail: "found".into(),
            blocking: false,
        };
        let revisions = vec![
            descendant,
            revision(asset_id, 3, Some(base), None, changed_block.clone()),
            revision(asset_id, 4, Some(base), None, changed_schema.clone()),
            revision(asset_id, 5, Some(base), None, changed_citation.clone()),
            derived.clone(),
            first.clone(),
            derived_only.clone(),
        ];
        insert(
            &repo,
            scope.clone(),
            asset_id,
            revisions,
            vec![ContentCheck {
                check_id: Uuid::new_v4(),
                revision_id: first.revision_id,
                findings: vec![finding.clone()],
                created_at: Utc::now(),
            }],
        )
        .await;
        insert(
            &repo,
            scope.clone(),
            wrong_asset_id,
            vec![revision(
                wrong_asset_id,
                1,
                Some(base),
                None,
                target_document.clone(),
            )],
            vec![],
        )
        .await;
        insert(
            &repo,
            foreign_scope.clone(),
            asset_id,
            vec![revision(
                asset_id,
                1,
                Some(base),
                None,
                target_document.clone(),
            )],
            vec![],
        )
        .await;
        let found = repo
            .find_exact_child_revision(&scope, asset_id, base, &target_document)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.revision_id, first.revision_id);
        assert_eq!(found.findings, vec![finding]);
        assert_eq!(
            repo.find_exact_child_revision(&scope, asset_id, derived_only_base, &target_document)
                .await
                .unwrap()
                .unwrap()
                .revision_id,
            derived_only.revision_id
        );
        for altered in [&changed_block, &changed_schema, &changed_citation] {
            assert!(
                repo.find_exact_child_revision(&scope, asset_id, base, altered)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        let mut changed_title = target_document.clone();
        changed_title.title.push('!');
        assert!(
            repo.find_exact_child_revision(&scope, asset_id, base, &changed_title)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.find_exact_child_revision(&scope, asset_id, Uuid::new_v4(), &target_document)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.find_exact_child_revision(&foreign_scope, asset_id, base, &target_document)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            repo.find_exact_child_revision(
                &TenantScope {
                    tenant_id: Uuid::new_v4().into(),
                    ..scope.clone()
                },
                asset_id,
                base,
                &target_document
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            repo.find_exact_child_revision(&scope, wrong_asset_id, base, &target_document)
                .await
                .unwrap()
                .is_some()
        );
        let tenant_wide = TenantScope {
            project_id: None,
            ..scope.clone()
        };
        assert_eq!(
            repo.find_exact_child_revision(&tenant_wide, asset_id, base, &target_document)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            repo.find_exact_child_revision(&scope, asset_id, derived.revision_id, &target_document)
                .await
                .unwrap()
                .unwrap()
                .revision,
            2
        );
    }
}
