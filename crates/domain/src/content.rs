//! Durable first-stage execution. Planning manifests are immutable inputs;
//! all mutable progress lives in this separate aggregate.
use crate::{
    AppError, DocumentManifest, DocumentManifestItemState, EvidenceRef, ProjectId, TenantScope,
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::sync::RwLock;
use uuid::Uuid;

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
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredDocument {
    pub title: String,
    pub blocks: Vec<ContentBlock>,
}
impl StructuredDocument {
    pub fn validate(&self, evidence: &[EvidenceRef]) -> Result<(), AppError> {
        if self.title.trim().is_empty() || self.blocks.is_empty() {
            return Err(AppError::invalid_request(
                "document title and blocks are required",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for block in &self.blocks {
            if !seen.insert(block.block_id) {
                return Err(AppError::invalid_request("duplicate block id"));
            }
            if block.kind != ContentBlockKind::List && !block.items.is_empty() {
                return Err(AppError::invalid_request("only list blocks contain items"));
            }
            if block.kind == ContentBlockKind::List && block.items.is_empty() {
                return Err(AppError::invalid_request("list blocks require items"));
            }
            if block.text.trim().is_empty() && block.items.is_empty() {
                return Err(AppError::invalid_request("empty block"));
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
            }
        }
        result.trim_end().to_owned()
    }
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
        let asset_id = stable_id(&format!("content-asset:{}", item.branch_key));
        let revision = ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id,
            revision: 1,
            base_revision_id: None,
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

#[derive(Default)]
pub struct MemoryContentRepository {
    state: RwLock<HashMap<Uuid, (TenantScope, ContentState)>>,
}
impl MemoryContentRepository {
    pub fn new() -> Self {
        Self::default()
    }
    async fn mutate<T>(
        &self,
        scope: &TenantScope,
        id: Uuid,
        f: impl FnOnce(&mut ContentState) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let mut guard = self.state.write().await;
        let (stored_scope, state) = guard
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found("content execution not found"))?;
        if !scope.contains(stored_scope) {
            return Err(AppError::not_found("content execution not found"));
        }
        f(state)
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
        self.mutate(scope, id, |s| s.claim(item, step, owner, now, ttl))
            .await
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
        self.mutate(scope, lease.execution_id, |s| {
            s.complete_check(lease, findings)
        })
        .await
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
        self.mutate(scope, id, |s| s.edit(asset, base, doc)).await
    }
    async fn classify(
        &self,
        scope: &TenantScope,
        id: Uuid,
        item: Uuid,
        status: ContentItemStatus,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, id, |s| s.classify(item, status, reason))
            .await
    }
    async fn fail_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, lease.execution_id, |s| s.fail_step(lease, reason))
            .await
    }
    async fn release_step(
        &self,
        scope: &TenantScope,
        lease: &StepLease,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, lease.execution_id, |s| s.release_step(lease))
            .await
    }
    async fn invalidate_ready(
        &self,
        scope: &TenantScope,
        execution_id: Uuid,
        item_id: Uuid,
        reason: &str,
    ) -> Result<ContentItem, AppError> {
        self.mutate(scope, execution_id, |s| s.invalidate_ready(item_id, reason))
            .await
    }
    async fn close(&self, scope: &TenantScope, id: Uuid) -> Result<ContentHandoff, AppError> {
        self.mutate(scope, id, ContentState::close).await
    }
    async fn cancel(&self, scope: &TenantScope, id: Uuid) -> Result<ContentExecution, AppError> {
        self.mutate(scope, id, ContentState::cancel).await
    }
}
