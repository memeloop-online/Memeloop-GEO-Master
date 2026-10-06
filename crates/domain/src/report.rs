use std::collections::{BTreeMap, BTreeSet, HashMap};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppError, DocumentManifest, DocumentManifestItemState, ErrorCode, ProjectId, TenantScope,
};

pub const REPORT_REDUCER_VERSION: &str = "geo-report-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportManifestKind {
    Document,
    Distribution,
    Measurement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportManifestRef {
    pub kind: ReportManifestKind,
    pub manifest_id: Uuid,
    pub revision: i32,
    pub sealed: bool,
    pub expected_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportEvidenceReference {
    pub evidence_id: Uuid,
    pub kind: String,
    pub resource_id: Uuid,
    pub resource_version: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub received_at: Option<DateTime<Utc>>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportPublicationStatus {
    Planned,
    Published,
    Verified,
    Unknown,
    Failed,
    Blocked,
    Deferred,
    NotApplicable,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportPublicationTarget {
    pub target_id: Uuid,
    pub platform_id: String,
    pub status: ReportPublicationStatus,
    pub reason: Option<String>,
    pub evidence: Vec<ReportEvidenceReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportMeasurementStatus {
    Observed,
    NotMentioned,
    Refused,
    Missing,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportMeasurementTarget {
    /// Stable scheduled sample ID, not a retry or request ID.
    pub target_id: Uuid,
    /// Exact immutable protocol/question-set/provider/model/surface/market/language key.
    pub comparison_key: String,
    pub scheduled_at: DateTime<Utc>,
    pub status: ReportMeasurementStatus,
    pub missing_reason: Option<String>,
    pub evidence: Vec<ReportEvidenceReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportReduceInput {
    pub project_id: ProjectId,
    pub cycle_id: Uuid,
    pub report_window_start_at: DateTime<Utc>,
    pub report_window_end_at: DateTime<Utc>,
    pub report_timezone: String,
    pub cutoff_at: DateTime<Utc>,
    /// True only when the manifest/outcome state was captured at or before
    /// the report's evidence_as_of; current mutable source adapters pass false.
    #[serde(default)]
    pub input_temporal_provenance_verified: bool,
    pub input_manifest_versions: Vec<ReportManifestRef>,
    pub document_manifest: Option<DocumentManifest>,
    /// None means the branch has no materialized source; Some(empty) requires a sealed zero manifest.
    pub publication_targets: Option<Vec<ReportPublicationTarget>>,
    pub measurement_targets: Option<Vec<ReportMeasurementTarget>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportAvailability {
    Available,
    Unavailable,
    Unsealed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportCoverage {
    pub availability: ReportAvailability,
    pub expected_count: Option<u64>,
    pub observed_count: u64,
    pub counts: BTreeMap<String, u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportPublicationGroup {
    pub platform_id: String,
    pub coverage: ReportCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportMeasurementGroup {
    pub comparison_key: String,
    pub coverage: ReportCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportFinding {
    pub finding_id: Uuid,
    pub kind: String,
    pub summary: String,
    pub evidence_ids: Vec<Uuid>,
    pub insufficient_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportSnapshot {
    pub report_id: Uuid,
    pub project_id: ProjectId,
    pub cycle_id: Uuid,
    pub revision: u32,
    pub correction_of: Option<Uuid>,
    pub report_window_start_at: DateTime<Utc>,
    pub report_window_end_at: DateTime<Utc>,
    pub report_timezone: String,
    pub cutoff_at: DateTime<Utc>,
    pub evidence_as_of: DateTime<Utc>,
    pub generated_at: DateTime<Utc>,
    pub reducer_version: String,
    pub input_hash: String,
    pub status: ReportStatus,
    pub input_manifest_versions: Vec<ReportManifestRef>,
    pub documents: ReportCoverage,
    pub publications: ReportCoverage,
    pub measurements: ReportCoverage,
    pub publication_groups: Vec<ReportPublicationGroup>,
    pub measurement_groups: Vec<ReportMeasurementGroup>,
    pub findings: Vec<ReportFinding>,
    pub evidence: Vec<ReportEvidenceReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportPreviewKind {
    Preview,
}

/// An ephemeral read-only projection. It cannot be stored as an official
/// snapshot or referenced as a correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportPreview {
    pub kind: ReportPreviewKind,
    pub project_id: ProjectId,
    pub cycle_id: Uuid,
    pub report_window_start_at: DateTime<Utc>,
    pub report_window_end_at: DateTime<Utc>,
    pub report_timezone: String,
    pub cutoff_at: DateTime<Utc>,
    pub evidence_as_of: DateTime<Utc>,
    pub generated_at: DateTime<Utc>,
    pub reducer_version: String,
    pub input_hash: String,
    pub status: ReportStatus,
    pub input_manifest_versions: Vec<ReportManifestRef>,
    pub documents: ReportCoverage,
    pub publications: ReportCoverage,
    pub measurements: ReportCoverage,
    pub publication_groups: Vec<ReportPublicationGroup>,
    pub measurement_groups: Vec<ReportMeasurementGroup>,
    pub findings: Vec<ReportFinding>,
    pub evidence: Vec<ReportEvidenceReference>,
}

struct ReportProjection {
    finding_namespace: Uuid,
    evidence_as_of: DateTime<Utc>,
    input_hash: String,
    status: ReportStatus,
    input_manifest_versions: Vec<ReportManifestRef>,
    documents: ReportCoverage,
    publications: ReportCoverage,
    measurements: ReportCoverage,
    publication_groups: Vec<ReportPublicationGroup>,
    measurement_groups: Vec<ReportMeasurementGroup>,
    findings: Vec<ReportFinding>,
    evidence: Vec<ReportEvidenceReference>,
}

#[derive(Clone, Copy)]
enum ProjectionMode {
    Official {
        revision: u32,
        correction_of: Option<Uuid>,
    },
    Preview,
}

#[async_trait]
pub trait ReportRepository: Send + Sync {
    async fn create(
        &self,
        scope: &TenantScope,
        snapshot: ReportSnapshot,
    ) -> Result<ReportSnapshot, AppError>;
    async fn list(
        &self,
        scope: &TenantScope,
        project_id: ProjectId,
    ) -> Result<Vec<ReportSnapshot>, AppError>;
    async fn get(&self, scope: &TenantScope, report_id: Uuid) -> Result<ReportSnapshot, AppError>;
}

#[derive(Default)]
pub struct MemoryReportRepository {
    reports: RwLock<HashMap<(Uuid, Uuid, Uuid), Vec<ReportSnapshot>>>,
}

impl MemoryReportRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ReportRepository for MemoryReportRepository {
    async fn create(
        &self,
        scope: &TenantScope,
        snapshot: ReportSnapshot,
    ) -> Result<ReportSnapshot, AppError> {
        if scope.project_id != Some(snapshot.project_id) {
            return Err(AppError::forbidden(
                "report project is outside tenant scope",
            ));
        }
        let key = (
            scope.operator_id.as_uuid(),
            scope.tenant_id.as_uuid(),
            snapshot.project_id.as_uuid(),
        );
        let mut guard = self.reports.write().await;
        let reports = guard.entry(key).or_default();
        if let Some(existing) = reports
            .iter()
            .find(|report| report.report_id == snapshot.report_id)
        {
            if existing.input_hash != snapshot.input_hash
                || existing.correction_of != snapshot.correction_of
            {
                return Err(AppError::conflict("report revision inputs differ"));
            }
            return Ok(existing.clone());
        }
        validate_correction(reports, &snapshot)?;
        reports.push(snapshot.clone());
        Ok(snapshot)
    }

    async fn list(
        &self,
        scope: &TenantScope,
        project_id: ProjectId,
    ) -> Result<Vec<ReportSnapshot>, AppError> {
        if scope.project_id != Some(project_id) {
            return Err(AppError::forbidden(
                "report project is outside tenant scope",
            ));
        }
        let guard = self.reports.read().await;
        let mut reports = guard
            .get(&(
                scope.operator_id.as_uuid(),
                scope.tenant_id.as_uuid(),
                project_id.as_uuid(),
            ))
            .cloned()
            .unwrap_or_default();
        reports.sort_by_key(|report| {
            (
                std::cmp::Reverse(report.report_window_end_at),
                std::cmp::Reverse(report.revision),
            )
        });
        Ok(reports)
    }

    async fn get(&self, scope: &TenantScope, report_id: Uuid) -> Result<ReportSnapshot, AppError> {
        let project_id = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        self.list(scope, project_id)
            .await?
            .into_iter()
            .find(|report| report.report_id == report_id)
            .ok_or_else(|| AppError::not_found("report was not found"))
    }
}

pub fn validate_correction(
    existing: &[ReportSnapshot],
    snapshot: &ReportSnapshot,
) -> Result<(), AppError> {
    if snapshot.revision == 1 && snapshot.correction_of.is_none() {
        return Ok(());
    }
    let Some(previous) = existing
        .iter()
        .find(|r| Some(r.report_id) == snapshot.correction_of)
    else {
        return Err(AppError::conflict("correction parent was not found"));
    };
    if previous.revision + 1 != snapshot.revision
        || previous.cycle_id != snapshot.cycle_id
        || previous.report_window_start_at != snapshot.report_window_start_at
        || previous.report_window_end_at != snapshot.report_window_end_at
        || previous.cutoff_at != snapshot.cutoff_at
        || previous.project_id != snapshot.project_id
        || previous.input_manifest_versions != snapshot.input_manifest_versions
        || previous.reducer_version != snapshot.reducer_version
        || existing.iter().any(|report| {
            report.report_window_start_at == snapshot.report_window_start_at
                && report.report_window_end_at == snapshot.report_window_end_at
                && report.cutoff_at == snapshot.cutoff_at
                && report.input_manifest_versions == snapshot.input_manifest_versions
                && report.revision >= snapshot.revision
        })
    {
        return Err(AppError::conflict(
            "correction parent does not match report window",
        ));
    }
    Ok(())
}

fn stable_uuid(value: &str) -> Uuid {
    let digest = Sha256::digest(value.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// A lookup establishes an independently observed public asset, not the
/// provenance of the original send. One reused original send gets a distinct
/// reference for each frozen report coverage cell.
pub fn publication_lookup_asset_evidence(
    frozen_target_id: Uuid,
    execution_id: Uuid,
    observed_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> ReportEvidenceReference {
    ReportEvidenceReference {
        evidence_id: stable_uuid(&format!(
            "publication-lookup-asset-v1:{frozen_target_id}:{execution_id}"
        )),
        kind: "publication_lookup_asset_observed".to_owned(),
        resource_id: frozen_target_id,
        resource_version: None,
        occurred_at: Some(observed_at),
        received_at: Some(received_at),
        summary: "Independent lookup observed a public asset; original send remains unproven"
            .to_owned(),
    }
}

fn unavailable(reason: &str) -> ReportCoverage {
    ReportCoverage {
        availability: ReportAvailability::Unavailable,
        expected_count: None,
        observed_count: 0,
        counts: BTreeMap::new(),
        reason: Some(reason.to_owned()),
    }
}

fn unsealed(reason: &str) -> ReportCoverage {
    ReportCoverage {
        availability: ReportAvailability::Unsealed,
        expected_count: None,
        observed_count: 0,
        counts: BTreeMap::new(),
        reason: Some(reason.to_owned()),
    }
}

fn bump(counts: &mut BTreeMap<String, u64>, key: &str) {
    *counts.entry(key.to_owned()).or_default() += 1;
}

fn error(message: &str) -> AppError {
    AppError::invalid_request(message)
}

/// Pure, deterministic fan-in over frozen plan denominators. No source is
/// inferred from a project setting, and a missing branch is never a zero.
pub fn reduce_report(
    scope: &TenantScope,
    input: &ReportReduceInput,
    revision: u32,
    correction_of: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<ReportSnapshot, AppError> {
    let projection = project_report(
        scope,
        input,
        now,
        ProjectionMode::Official {
            revision,
            correction_of,
        },
    )?;
    Ok(ReportSnapshot {
        report_id: projection.finding_namespace,
        project_id: input.project_id,
        cycle_id: input.cycle_id,
        revision,
        correction_of,
        report_window_start_at: input.report_window_start_at,
        report_window_end_at: input.report_window_end_at,
        report_timezone: input.report_timezone.clone(),
        cutoff_at: input.cutoff_at,
        evidence_as_of: projection.evidence_as_of,
        generated_at: now,
        reducer_version: REPORT_REDUCER_VERSION.to_owned(),
        input_hash: projection.input_hash,
        status: projection.status,
        input_manifest_versions: projection.input_manifest_versions,
        documents: projection.documents,
        publications: projection.publications,
        measurements: projection.measurements,
        publication_groups: projection.publication_groups,
        measurement_groups: projection.measurement_groups,
        findings: projection.findings,
        evidence: projection.evidence,
    })
}

/// Project the frozen full-window denominators against evidence known by now.
/// No report identity or revision is allocated, and this is never persisted.
pub fn preview_report(
    scope: &TenantScope,
    input: &ReportReduceInput,
    now: DateTime<Utc>,
) -> Result<ReportPreview, AppError> {
    let projection = project_report(scope, input, now, ProjectionMode::Preview)?;
    Ok(ReportPreview {
        kind: ReportPreviewKind::Preview,
        project_id: input.project_id,
        cycle_id: input.cycle_id,
        report_window_start_at: input.report_window_start_at,
        report_window_end_at: input.report_window_end_at,
        report_timezone: input.report_timezone.clone(),
        cutoff_at: input.cutoff_at,
        evidence_as_of: projection.evidence_as_of,
        generated_at: now,
        reducer_version: REPORT_REDUCER_VERSION.to_owned(),
        input_hash: projection.input_hash,
        status: projection.status,
        input_manifest_versions: projection.input_manifest_versions,
        documents: projection.documents,
        publications: projection.publications,
        measurements: projection.measurements,
        publication_groups: projection.publication_groups,
        measurement_groups: projection.measurement_groups,
        findings: projection.findings,
        evidence: projection.evidence,
    })
}

fn project_report(
    scope: &TenantScope,
    input: &ReportReduceInput,
    now: DateTime<Utc>,
    mode: ProjectionMode,
) -> Result<ReportProjection, AppError> {
    if scope.project_id != Some(input.project_id) {
        return Err(AppError::forbidden(
            "report project is outside tenant scope",
        ));
    }
    if input.report_window_start_at >= input.report_window_end_at
        || input.cutoff_at < input.report_window_end_at
        || matches!(mode, ProjectionMode::Official { revision: 0, .. })
        || matches!(
            mode,
            ProjectionMode::Official {
                revision,
                correction_of
            } if (revision == 1) != correction_of.is_none()
        )
        || input.report_timezone.trim().is_empty()
    {
        return Err(error("invalid report window, timezone or revision"));
    }
    let mut canonical = input.clone();
    if let Some(docs) = &mut canonical.document_manifest {
        docs.items
            .sort_by_key(|item| item.document_manifest_item_id);
    }
    canonical
        .input_manifest_versions
        .sort_by_key(|item| (item.kind, item.manifest_id, item.revision));
    let mut kinds = BTreeSet::new();
    for manifest in &canonical.input_manifest_versions {
        if !kinds.insert(manifest.kind)
            || manifest.revision <= 0
            || (manifest.sealed && manifest.expected_count.is_none())
            || (!manifest.sealed && manifest.expected_count.is_some())
        {
            return Err(error("invalid or duplicate manifest reference"));
        }
    }
    let manifest = |kind| {
        canonical
            .input_manifest_versions
            .iter()
            .find(|item| item.kind == kind)
    };
    let doc_ref = manifest(ReportManifestKind::Document);
    let dist_ref = manifest(ReportManifestKind::Distribution);
    let measure_ref = manifest(ReportManifestKind::Measurement);
    if let Some(docs) = &canonical.document_manifest
        && (docs.operator_id != scope.operator_id
            || docs.tenant_id != scope.tenant_id
            || docs.project_id != input.project_id
            || doc_ref.is_none_or(|reference| {
                reference.manifest_id != docs.manifest_id
                    || reference.revision != docs.revision
                    || reference.sealed != docs.sealed
                    || reference.expected_count
                        != docs.expected_count.and_then(|n| n.try_into().ok())
            }))
    {
        return Err(error(
            "document manifest does not match frozen scope and version",
        ));
    }
    for (targets_present, reference, name) in [
        (
            canonical.publication_targets.is_some(),
            dist_ref,
            "distribution",
        ),
        (
            canonical.measurement_targets.is_some(),
            measure_ref,
            "measurement",
        ),
    ] {
        if targets_present && !reference.is_some_and(|reference| reference.sealed) {
            return Err(error(&format!("{name} outcomes require a sealed manifest")));
        }
    }
    if let Some(targets) = &mut canonical.publication_targets {
        targets.sort_by_key(|target| target.target_id);
        for target in targets.iter_mut() {
            target.evidence.sort_by_key(|e| e.evidence_id);
        }
    }
    if let Some(targets) = &mut canonical.measurement_targets {
        targets.sort_by_key(|target| target.target_id);
        for target in targets.iter_mut() {
            target.evidence.sort_by_key(|e| e.evidence_id);
        }
    }
    let input_bytes = serde_json::to_vec(&canonical)
        .map_err(|_| AppError::new(ErrorCode::Internal, "report input cannot be serialized"))?;
    let input_hash = hex::encode(Sha256::digest(input_bytes));
    let evidence_as_of = match mode {
        ProjectionMode::Official {
            correction_of: Some(_),
            ..
        } => now,
        _ => std::cmp::min(now, input.cutoff_at),
    };
    let finding_namespace = match mode {
        ProjectionMode::Official { revision, .. } => stable_uuid(&format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            scope.operator_id,
            scope.tenant_id,
            input.project_id,
            input.cycle_id,
            input.report_window_start_at,
            input.report_window_end_at,
            input.cutoff_at,
            serde_json::to_string(&canonical.input_manifest_versions).unwrap_or_default(),
            REPORT_REDUCER_VERSION,
            revision,
        )),
        ProjectionMode::Preview => stable_uuid(&format!(
            "report-preview-v1:{}:{}:{}:{}:{}",
            scope.operator_id, scope.tenant_id, input.project_id, input.cycle_id, evidence_as_of,
        )),
    };
    let mut findings = Vec::new();
    let mut evidence_map = BTreeMap::<Uuid, ReportEvidenceReference>::new();
    let documents = match (&canonical.document_manifest, doc_ref) {
        (Some(docs), Some(reference)) if docs.sealed => {
            let mut counts = BTreeMap::new();
            let mut document_ids = BTreeSet::new();
            for item in &docs.items {
                if !document_ids.insert(item.document_manifest_item_id)
                    || item.manifest_id != docs.manifest_id
                {
                    return Err(error("duplicate or mismatched document manifest item"));
                }
                let status = match item.state {
                    DocumentManifestItemState::Planned => "planned",
                    DocumentManifestItemState::Blocked => "blocked",
                    DocumentManifestItemState::Deferred => "deferred",
                    DocumentManifestItemState::NotApplicable => "not_applicable",
                };
                bump(&mut counts, status);
                record_evidence(
                    &mut evidence_map,
                    ReportEvidenceReference {
                        evidence_id: item.document_manifest_item_id,
                        kind: "document_manifest_item".to_owned(),
                        resource_id: item.document_manifest_item_id,
                        resource_version: Some(docs.revision.to_string()),
                        occurred_at: None,
                        received_at: None,
                        summary: format!("Planned document branch: {status}"),
                    },
                )?;
                for source_id in &item.source_version_refs {
                    record_evidence(
                        &mut evidence_map,
                        ReportEvidenceReference {
                            evidence_id: *source_id,
                            kind: "source_version".to_owned(),
                            resource_id: *source_id,
                            resource_version: None,
                            occurred_at: None,
                            received_at: None,
                            summary: "Source version referenced by the planned document branch"
                                .to_owned(),
                        },
                    )?;
                }
                if status != "not_applicable" {
                    findings.push(ReportFinding {
                        finding_id: stable_uuid(&format!(
                            "{finding_namespace}:document:{}",
                            item.document_manifest_item_id
                        )),
                        kind: format!("document_{status}"),
                        summary: format!(
                            "Document branch is {status}; no generated content is inferred."
                        ),
                        evidence_ids: std::iter::once(item.document_manifest_item_id)
                            .chain(item.source_version_refs.iter().copied())
                            .collect(),
                        insufficient_reason: item.block_reason.clone().or_else(|| {
                            Some(
                                "Document content and completion evidence are not available."
                                    .to_owned(),
                            )
                        }),
                    });
                }
            }
            if docs.items.len() as u64 > reference.expected_count.unwrap_or(0) {
                return Err(error("document items exceed frozen denominator"));
            }
            ReportCoverage {
                availability: ReportAvailability::Available,
                expected_count: reference.expected_count,
                observed_count: docs.items.len() as u64,
                counts,
                reason: None,
            }
        }
        (_, Some(reference)) if !reference.sealed => unsealed("document manifest is not sealed"),
        _ => unavailable("document manifest is not available"),
    };
    let mut publication_groups = BTreeMap::<String, ReportCoverage>::new();
    let publications = match (&canonical.publication_targets, dist_ref) {
        (Some(targets), Some(reference)) => {
            let expected = reference.expected_count.unwrap_or(0);
            if targets.len() as u64 > expected {
                return Err(error("publication targets exceed frozen denominator"));
            }
            let mut ids = BTreeSet::new();
            let mut counts = BTreeMap::new();
            for target in targets {
                if !ids.insert(target.target_id) || target.platform_id.trim().is_empty() {
                    return Err(error("duplicate publication target or empty platform"));
                }
                let expected_kind = if target.status == ReportPublicationStatus::Verified {
                    "public_verification"
                } else {
                    "publication_receipt"
                };
                let valid = collect_evidence(
                    &target.evidence,
                    evidence_as_of,
                    target.target_id,
                    expected_kind,
                    None,
                    &mut evidence_map,
                )?;
                let mut lookup_evidence = BTreeSet::new();
                for reference in &target.evidence {
                    if reference.kind == "publication_lookup_asset_observed"
                        && reference.resource_id == target.target_id
                        && reference
                            .occurred_at
                            .zip(reference.received_at)
                            .is_some_and(|(occurred, received)| {
                                occurred <= received && received <= evidence_as_of
                            })
                    {
                        record_evidence(&mut evidence_map, reference.clone())?;
                        lookup_evidence.insert(reference.evidence_id);
                    }
                }
                let status = match target.status {
                    ReportPublicationStatus::Verified if valid => "verified",
                    ReportPublicationStatus::Published if valid => "published",
                    ReportPublicationStatus::Verified | ReportPublicationStatus::Published => {
                        "unknown"
                    }
                    ReportPublicationStatus::Unknown => "unknown",
                    ReportPublicationStatus::Failed => "failed",
                    ReportPublicationStatus::Blocked => "blocked",
                    ReportPublicationStatus::Deferred => "deferred",
                    ReportPublicationStatus::NotApplicable => "not_applicable",
                    ReportPublicationStatus::Cancelled => "cancelled",
                    ReportPublicationStatus::Planned => "pending",
                };
                bump(&mut counts, status);
                if !lookup_evidence.is_empty() {
                    findings.push(ReportFinding {
                        finding_id: stable_uuid(&format!(
                            "{finding_namespace}:publication:asset_observed:{}",
                            target.target_id
                        )),
                        kind: "publication_asset_observed".to_owned(),
                        summary: "A public asset was observed independently; this does not prove the original publication send succeeded.".to_owned(),
                        evidence_ids: lookup_evidence.iter().copied().collect(),
                        insufficient_reason: Some(
                            "No trusted causal link between the original send and the observed asset is available.".to_owned(),
                        ),
                    });
                }
                record_evidence(
                    &mut evidence_map,
                    ReportEvidenceReference {
                        evidence_id: target.target_id,
                        kind: "publication_target".to_owned(),
                        resource_id: target.target_id,
                        resource_version: Some(reference.revision.to_string()),
                        occurred_at: None,
                        received_at: None,
                        summary: format!("Publication target: {status}"),
                    },
                )?;
                if matches!(
                    status,
                    "unknown" | "failed" | "blocked" | "deferred" | "cancelled" | "pending"
                ) {
                    findings.push(ReportFinding {
                        finding_id: stable_uuid(&format!("{finding_namespace}:publication:{}", target.target_id)),
                        kind: format!("publication_{status}"),
                        summary: format!("Publication target remains {status}; no public verification is inferred."),
                        evidence_ids: std::iter::once(target.target_id)
                            .chain(target.evidence.iter().filter(|e| evidence_map.contains_key(&e.evidence_id)).map(|e| e.evidence_id))
                            .collect(),
                        insufficient_reason: target.reason.clone().or_else(|| Some(
                            "Public verification or a terminal receipt is not available.".to_owned(),
                        )),
                    });
                }
                let group = publication_groups
                    .entry(target.platform_id.clone())
                    .or_insert_with(|| ReportCoverage {
                        availability: ReportAvailability::Available,
                        expected_count: None,
                        observed_count: 0,
                        counts: BTreeMap::new(),
                        reason: Some("per-platform planned denominator is unavailable".to_owned()),
                    });
                group.observed_count += 1;
                bump(&mut group.counts, status);
            }
            if expected > targets.len() as u64 {
                counts.insert("unmaterialized".to_owned(), expected - targets.len() as u64);
            }
            ReportCoverage {
                availability: ReportAvailability::Available,
                expected_count: Some(expected),
                observed_count: targets.len() as u64,
                counts,
                reason: None,
            }
        }
        (_, Some(reference)) if !reference.sealed => {
            unsealed("distribution manifest is not sealed")
        }
        _ => unavailable("publication targets are not available"),
    };
    let mut measurement_groups = BTreeMap::<String, ReportCoverage>::new();
    let measurements = match (&canonical.measurement_targets, measure_ref) {
        (Some(targets), Some(reference)) => {
            let expected = reference.expected_count.unwrap_or(0);
            if targets.len() as u64 > expected {
                return Err(error("measurement targets exceed frozen denominator"));
            }
            let mut ids = BTreeSet::new();
            let mut counts = BTreeMap::new();
            for target in targets {
                if !ids.insert(target.target_id) || target.comparison_key.trim().is_empty() {
                    return Err(error(
                        "duplicate measurement sample or empty comparison key",
                    ));
                }
                let valid = collect_evidence(
                    &target.evidence,
                    evidence_as_of,
                    target.target_id,
                    "observation",
                    Some((input.report_window_start_at, input.report_window_end_at)),
                    &mut evidence_map,
                )? && target.scheduled_at >= input.report_window_start_at
                    && target.scheduled_at < input.report_window_end_at;
                let status = match target.status {
                    ReportMeasurementStatus::Observed if valid => "observed",
                    ReportMeasurementStatus::NotMentioned if valid => "not_mentioned",
                    ReportMeasurementStatus::Refused if valid => "refused",
                    ReportMeasurementStatus::Observed
                    | ReportMeasurementStatus::NotMentioned
                    | ReportMeasurementStatus::Refused
                    | ReportMeasurementStatus::Missing => "missing",
                    ReportMeasurementStatus::Pending => "pending",
                };
                bump(&mut counts, status);
                record_evidence(
                    &mut evidence_map,
                    ReportEvidenceReference {
                        evidence_id: target.target_id,
                        kind: "measurement_target".to_owned(),
                        resource_id: target.target_id,
                        resource_version: Some(reference.revision.to_string()),
                        occurred_at: Some(target.scheduled_at),
                        received_at: None,
                        summary: format!("Measurement sample: {status}"),
                    },
                )?;
                if matches!(status, "missing" | "pending" | "refused") {
                    findings.push(ReportFinding {
                        finding_id: stable_uuid(&format!("{finding_namespace}:measurement:{}", target.target_id)),
                        kind: format!("measurement_{status}"),
                        summary: format!("Independent measurement sample is {status}; missing is not counted as not-mentioned."),
                        evidence_ids: std::iter::once(target.target_id)
                            .chain(target.evidence.iter().filter(|e| evidence_map.contains_key(&e.evidence_id)).map(|e| e.evidence_id))
                            .collect(),
                        insufficient_reason: target.missing_reason.clone().or_else(|| {
                            if status == "missing" || status == "pending" {
                                Some("A timely, timestamped observation is unavailable.".to_owned())
                            } else { None }
                        }),
                    });
                }
                let group = measurement_groups
                    .entry(target.comparison_key.clone())
                    .or_insert_with(|| ReportCoverage {
                        availability: ReportAvailability::Available,
                        expected_count: None,
                        observed_count: 0,
                        counts: BTreeMap::new(),
                        reason: Some("per-protocol planned denominator is unavailable".to_owned()),
                    });
                group.observed_count += 1;
                bump(&mut group.counts, status);
            }
            if expected > targets.len() as u64 {
                counts.insert("unmaterialized".to_owned(), expected - targets.len() as u64);
            }
            ReportCoverage {
                availability: ReportAvailability::Available,
                expected_count: Some(expected),
                observed_count: targets.len() as u64,
                counts,
                reason: None,
            }
        }
        (_, Some(reference)) if !reference.sealed => unsealed("measurement manifest is not sealed"),
        _ => unavailable("independent measurement targets are not available"),
    };
    if canonical.document_manifest.is_some() && !input.input_temporal_provenance_verified {
        findings.push(ReportFinding {
            finding_id: stable_uuid(&format!("{finding_namespace}:temporal_provenance")),
            kind: "temporal_provenance_unavailable".to_owned(),
            summary: match mode {
                ProjectionMode::Preview => "Current document manifest state is visible, but its state at the preview evidence time cannot be verified.".to_owned(),
                ProjectionMode::Official { .. } => "Current document manifest state is visible, but its state at the report cutoff cannot be verified.".to_owned(),
            },
            evidence_ids: canonical.document_manifest.as_ref().map(|docs| docs.items.iter().map(|item| item.document_manifest_item_id).collect()).unwrap_or_default(),
            insufficient_reason: Some("The source has no recorded seal or item transition timestamps.".to_owned()),
        });
    }
    let complete = [&documents, &publications, &measurements].iter().all(|c| {
        c.availability == ReportAvailability::Available
            && c.expected_count == Some(c.observed_count)
    }) && input.input_temporal_provenance_verified
        && !documents.counts.contains_key("planned")
        && !documents.counts.contains_key("blocked")
        && !documents.counts.contains_key("deferred")
        && !publications.counts.contains_key("unknown")
        && !publications.counts.contains_key("pending")
        && !publications.counts.contains_key("failed")
        && !publications.counts.contains_key("blocked")
        && !publications.counts.contains_key("deferred")
        && !publications.counts.contains_key("cancelled")
        && !measurements.counts.contains_key("missing")
        && !measurements.counts.contains_key("pending")
        && !measurements.counts.contains_key("refused");
    if matches!(mode, ProjectionMode::Official { .. }) && now < input.cutoff_at && !complete {
        return Err(AppError::new(
            ErrorCode::NotReady,
            "report inputs remain incomplete before cutoff",
        ));
    }
    if !complete {
        findings.push(ReportFinding {
            finding_id: stable_uuid(&format!("{finding_namespace}:coverage_gap")),
            kind: "coverage_gap".to_owned(),
            summary: match mode {
                ProjectionMode::Preview => {
                    "Planned inputs are incomplete or unavailable at the preview evidence time."
                        .to_owned()
                }
                ProjectionMode::Official { .. } => {
                    "Planned inputs are incomplete or unavailable at the reporting cutoff."
                        .to_owned()
                }
            },
            evidence_ids: Vec::new(),
            insufficient_reason: Some(
                "No unsupported effectiveness or trend inference is made from incomplete inputs."
                    .to_owned(),
            ),
        });
    }
    Ok(ReportProjection {
        finding_namespace,
        evidence_as_of,
        input_hash,
        status: if complete {
            ReportStatus::Complete
        } else {
            ReportStatus::Partial
        },
        input_manifest_versions: canonical.input_manifest_versions,
        documents,
        publications,
        measurements,
        publication_groups: publication_groups
            .into_iter()
            .map(|(platform_id, coverage)| ReportPublicationGroup {
                platform_id,
                coverage,
            })
            .collect(),
        measurement_groups: measurement_groups
            .into_iter()
            .map(|(comparison_key, coverage)| ReportMeasurementGroup {
                comparison_key,
                coverage,
            })
            .collect(),
        findings,
        evidence: evidence_map.into_values().collect(),
    })
}

fn collect_evidence(
    refs: &[ReportEvidenceReference],
    as_of: DateTime<Utc>,
    target_id: Uuid,
    expected_kind: &str,
    window: Option<(DateTime<Utc>, DateTime<Utc>)>,
    collected: &mut BTreeMap<Uuid, ReportEvidenceReference>,
) -> Result<bool, AppError> {
    let mut valid = false;
    for reference in refs {
        // A raw identifier without occurrence and receipt times is not proof
        // of a publication or independent measurement; retries cannot supply
        // the same observation to a different scheduled sample.
        if let (Some(occurred), Some(received)) = (reference.occurred_at, reference.received_at)
            && occurred <= received
            && received <= as_of
            && reference.kind == expected_kind
            && reference.resource_id == target_id
            && window.is_none_or(|(start, end)| occurred >= start && occurred < end)
        {
            valid = true;
            record_evidence(collected, reference.clone())?;
        }
    }
    Ok(valid)
}

fn record_evidence(
    collected: &mut BTreeMap<Uuid, ReportEvidenceReference>,
    reference: ReportEvidenceReference,
) -> Result<(), AppError> {
    if let Some(previous) = collected.get(&reference.evidence_id)
        && previous != &reference
    {
        return Err(error("conflicting evidence identities"));
    }
    collected.insert(reference.evidence_id, reference);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DocumentManifestCoverage, DocumentManifestItem, DocumentManifestState, OperatorId, TenantId,
    };
    use chrono::TimeZone;

    fn instant(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
    }

    fn setup() -> (TenantScope, ReportReduceInput) {
        let operator_id = OperatorId(Uuid::new_v4());
        let tenant_id = TenantId(Uuid::new_v4());
        let project_id = ProjectId(Uuid::new_v4());
        let scope = TenantScope::new(operator_id, tenant_id, Some(project_id));
        let doc_id = Uuid::new_v4();
        let dist_id = Uuid::new_v4();
        let input = ReportReduceInput {
            project_id,
            cycle_id: Uuid::new_v4(),
            report_window_start_at: instant(20),
            report_window_end_at: instant(27),
            report_timezone: "UTC".to_owned(),
            cutoff_at: instant(28),
            input_temporal_provenance_verified: false,
            input_manifest_versions: vec![
                ReportManifestRef {
                    kind: ReportManifestKind::Document,
                    manifest_id: doc_id,
                    revision: 1,
                    sealed: true,
                    expected_count: Some(2),
                },
                ReportManifestRef {
                    kind: ReportManifestKind::Distribution,
                    manifest_id: dist_id,
                    revision: 1,
                    sealed: false,
                    expected_count: None,
                },
            ],
            document_manifest: Some(DocumentManifest {
                manifest_id: doc_id,
                operator_id,
                tenant_id,
                project_id,
                revision: 1,
                knowledge_release_id: Uuid::new_v4(),
                planner_version: "test".to_owned(),
                state: DocumentManifestState::Ready,
                sealed: true,
                expected_count: Some(2),
                scope_hash: "test".to_owned(),
                items: vec![
                    DocumentManifestItem {
                        document_manifest_item_id: Uuid::new_v4(),
                        manifest_id: doc_id,
                        knowledge_release_id: Uuid::new_v4(),
                        document_key: "one".to_owned(),
                        content_type: "faq".to_owned(),
                        product_id: None,
                        market: "test".to_owned(),
                        language: "en".to_owned(),
                        state: DocumentManifestItemState::Planned,
                        block_reason: None,
                        dependency_hash: "one".to_owned(),
                        source_version_refs: vec![],
                    },
                    DocumentManifestItem {
                        document_manifest_item_id: Uuid::new_v4(),
                        manifest_id: doc_id,
                        knowledge_release_id: Uuid::new_v4(),
                        document_key: "two".to_owned(),
                        content_type: "faq".to_owned(),
                        product_id: None,
                        market: "test".to_owned(),
                        language: "en".to_owned(),
                        state: DocumentManifestItemState::Blocked,
                        block_reason: Some("insufficient source".to_owned()),
                        dependency_hash: "two".to_owned(),
                        source_version_refs: vec![],
                    },
                ],
                coverage: DocumentManifestCoverage {
                    total: 2,
                    planned: 1,
                    blocked: 1,
                    ..Default::default()
                },
            }),
            publication_targets: None,
            measurement_targets: None,
        };
        (scope, input)
    }

    fn evidence(kind: &str, resource_id: Uuid) -> ReportEvidenceReference {
        ReportEvidenceReference {
            evidence_id: Uuid::new_v4(),
            kind: kind.to_owned(),
            resource_id,
            resource_version: Some("1".to_owned()),
            occurred_at: Some(instant(25)),
            received_at: Some(instant(26)),
            summary: "Recorded external observation".to_owned(),
        }
    }

    #[test]
    fn preview_before_cutoff_keeps_full_window_and_does_not_create_snapshot_identity() {
        let (scope, mut input) = setup();
        input.input_manifest_versions.push(ReportManifestRef {
            kind: ReportManifestKind::Measurement,
            manifest_id: Uuid::new_v4(),
            revision: 1,
            sealed: true,
            expected_count: Some(3),
        });
        let observed_id = Uuid::new_v4();
        let future_id = Uuid::new_v4();
        input.measurement_targets = Some(vec![
            ReportMeasurementTarget {
                target_id: observed_id,
                comparison_key: "api/protocol".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::Observed,
                missing_reason: None,
                evidence: vec![evidence("observation", observed_id)],
            },
            ReportMeasurementTarget {
                target_id: future_id,
                comparison_key: "api/protocol".to_owned(),
                scheduled_at: instant(27),
                status: ReportMeasurementStatus::Pending,
                missing_reason: None,
                evidence: vec![],
            },
        ]);
        let preview = preview_report(&scope, &input, instant(26)).unwrap();
        assert_eq!(preview.kind, ReportPreviewKind::Preview);
        assert_eq!(preview.generated_at, instant(26));
        assert_eq!(preview.evidence_as_of, instant(26));
        assert_eq!(preview.report_window_end_at, instant(27));
        assert_eq!(preview.measurements.expected_count, Some(3));
        assert_eq!(preview.measurements.observed_count, 2);
        assert_eq!(preview.measurements.counts["observed"], 1);
        assert_eq!(preview.measurements.counts["pending"], 1);
        assert_eq!(preview.measurements.counts["unmaterialized"], 1);
        assert_eq!(preview.status, ReportStatus::Partial);
        let json = serde_json::to_value(&preview).unwrap();
        assert_eq!(json["kind"], "preview");
        for identity in ["report_id", "revision", "correction_of"] {
            assert!(json.get(identity).is_none(), "unexpected {identity}");
        }
        assert!(preview.findings.iter().any(|finding| {
            finding.kind == "coverage_gap" && finding.summary.contains("preview evidence time")
        }));
        assert!(preview.findings.iter().any(|finding| {
            finding.kind == "temporal_provenance_unavailable"
                && finding.summary.contains("preview evidence time")
        }));
        let official = reduce_report(&scope, &input, 1, None, instant(26)).unwrap_err();
        assert_eq!(official.code, ErrorCode::NotReady);
        let replay = preview_report(&scope, &input, instant(26)).unwrap();
        assert_eq!(preview, replay);
    }

    #[test]
    fn preview_clamps_evidence_after_cutoff_without_treating_missing_as_not_mentioned() {
        let (scope, mut input) = setup();
        let distribution = input
            .input_manifest_versions
            .iter_mut()
            .find(|reference| reference.kind == ReportManifestKind::Distribution)
            .unwrap();
        distribution.sealed = true;
        distribution.expected_count = Some(1);
        input.input_manifest_versions.push(ReportManifestRef {
            kind: ReportManifestKind::Measurement,
            manifest_id: Uuid::new_v4(),
            revision: 1,
            sealed: true,
            expected_count: Some(1),
        });
        let publication_id = Uuid::new_v4();
        input.publication_targets = Some(vec![ReportPublicationTarget {
            target_id: publication_id,
            platform_id: "platform".to_owned(),
            status: ReportPublicationStatus::Unknown,
            reason: None,
            evidence: vec![],
        }]);
        let sample_id = Uuid::new_v4();
        let mut late = evidence("observation", sample_id);
        late.received_at = Some(instant(29));
        input.measurement_targets = Some(vec![ReportMeasurementTarget {
            target_id: sample_id,
            comparison_key: "api/protocol".to_owned(),
            scheduled_at: instant(25),
            status: ReportMeasurementStatus::NotMentioned,
            missing_reason: None,
            evidence: vec![late.clone()],
        }]);
        let preview = preview_report(&scope, &input, instant(30)).unwrap();
        assert_eq!(preview.generated_at, instant(30));
        assert_eq!(preview.evidence_as_of, input.cutoff_at);
        assert_eq!(preview.publications.counts["unknown"], 1);
        assert_eq!(preview.measurements.counts["missing"], 1);
        assert!(!preview.measurements.counts.contains_key("not_mentioned"));
        assert!(!preview.evidence.contains(&late));
        let official = reduce_report(&scope, &input, 1, None, instant(30)).unwrap();
        assert_eq!(official.measurements, preview.measurements);
        assert_ne!(
            official.findings[0].finding_id,
            preview.findings[0].finding_id
        );
        let corrected =
            reduce_report(&scope, &input, 2, Some(official.report_id), instant(30)).unwrap();
        assert_eq!(corrected.measurements.counts["not_mentioned"], 1);
    }

    #[test]
    fn unavailable_is_not_zero_and_planned_is_not_generated() {
        let (scope, input) = setup();
        let report = reduce_report(&scope, &input, 1, None, instant(29)).unwrap();
        assert_eq!(report.status, ReportStatus::Partial);
        assert_eq!(report.documents.expected_count, Some(2));
        assert_eq!(report.documents.counts["planned"], 1);
        assert_eq!(report.documents.counts["blocked"], 1);
        assert_eq!(
            report.publications.availability,
            ReportAvailability::Unsealed
        );
        assert_eq!(
            report.measurements.availability,
            ReportAvailability::Unavailable
        );
        assert_eq!(report.measurements.expected_count, None);
        assert_eq!(report.evidence.len(), 2);
        assert!(
            report
                .evidence
                .iter()
                .all(|e| e.kind == "document_manifest_item")
        );
    }

    #[test]
    fn real_fan_in_keeps_distinct_statuses_groups_and_evidence() {
        let (scope, mut input) = setup();
        let distribution = input
            .input_manifest_versions
            .iter_mut()
            .find(|r| r.kind == ReportManifestKind::Distribution)
            .unwrap();
        distribution.sealed = true;
        distribution.expected_count = Some(4);
        input.input_manifest_versions.push(ReportManifestRef {
            kind: ReportManifestKind::Measurement,
            manifest_id: Uuid::new_v4(),
            revision: 1,
            sealed: true,
            expected_count: Some(5),
        });
        let published_id = Uuid::new_v4();
        let receipt = evidence("public_verification", published_id);
        input.publication_targets = Some(vec![
            ReportPublicationTarget {
                target_id: published_id,
                platform_id: "first".to_owned(),
                status: ReportPublicationStatus::Verified,
                reason: None,
                evidence: vec![receipt.clone()],
            },
            ReportPublicationTarget {
                target_id: Uuid::new_v4(),
                platform_id: "first".to_owned(),
                status: ReportPublicationStatus::Unknown,
                reason: Some("receipt pending".to_owned()),
                evidence: vec![],
            },
            ReportPublicationTarget {
                target_id: Uuid::new_v4(),
                platform_id: "second".to_owned(),
                status: ReportPublicationStatus::Failed,
                reason: None,
                evidence: vec![],
            },
        ]);
        let observed_id = Uuid::new_v4();
        let not_mentioned_id = Uuid::new_v4();
        let refused_id = Uuid::new_v4();
        let sample = evidence("observation", observed_id);
        let other_sample = evidence("observation", not_mentioned_id);
        input.measurement_targets = Some(vec![
            ReportMeasurementTarget {
                target_id: observed_id,
                comparison_key: "api/model-a".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::Observed,
                missing_reason: None,
                evidence: vec![sample.clone()],
            },
            ReportMeasurementTarget {
                target_id: not_mentioned_id,
                comparison_key: "api/model-a".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::NotMentioned,
                missing_reason: None,
                evidence: vec![other_sample],
            },
            ReportMeasurementTarget {
                target_id: refused_id,
                comparison_key: "web/model-b".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::Refused,
                missing_reason: None,
                evidence: vec![evidence("observation", refused_id)],
            },
            ReportMeasurementTarget {
                target_id: Uuid::new_v4(),
                comparison_key: "web/model-b".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::Missing,
                missing_reason: Some("technical".to_owned()),
                evidence: vec![],
            },
        ]);
        let report = reduce_report(&scope, &input, 1, None, instant(29)).unwrap();
        assert_eq!(report.publications.counts["verified"], 1);
        assert_eq!(report.publications.counts["unknown"], 1);
        assert_eq!(report.publications.counts["failed"], 1);
        assert_eq!(report.publications.counts["unmaterialized"], 1);
        assert_eq!(report.measurements.counts["observed"], 1);
        assert_eq!(report.measurements.counts["not_mentioned"], 1);
        assert_eq!(report.measurements.counts["refused"], 1);
        assert_eq!(report.measurements.counts["missing"], 1);
        assert_eq!(report.measurements.counts["unmaterialized"], 1);
        assert_eq!(report.publication_groups.len(), 2);
        assert_eq!(report.measurement_groups.len(), 2);
        assert_eq!(
            report
                .evidence
                .iter()
                .filter(|r| r.evidence_id == sample.evidence_id)
                .count(),
            1
        );
        assert_eq!(report.measurement_groups[0].coverage.observed_count, 2);
    }

    #[tokio::test]
    async fn cutoff_replay_and_explicit_late_correction_preserve_first_snapshot() {
        let (scope, mut input) = setup();
        input.input_manifest_versions.push(ReportManifestRef {
            kind: ReportManifestKind::Measurement,
            manifest_id: Uuid::new_v4(),
            revision: 1,
            sealed: true,
            expected_count: Some(1),
        });
        let sample_id = Uuid::new_v4();
        let mut late = evidence("observation", sample_id);
        late.received_at = Some(instant(29));
        input.measurement_targets = Some(vec![ReportMeasurementTarget {
            target_id: sample_id,
            comparison_key: "api/model-a".to_owned(),
            scheduled_at: instant(25),
            status: ReportMeasurementStatus::Observed,
            missing_reason: None,
            evidence: vec![late],
        }]);
        let repo = MemoryReportRepository::new();
        let first = repo
            .create(
                &scope,
                reduce_report(&scope, &input, 1, None, instant(29)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.measurements.counts["missing"], 1);
        let replay = repo
            .create(
                &scope,
                reduce_report(&scope, &input, 1, None, instant(30)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first, replay);
        let correction = repo
            .create(
                &scope,
                reduce_report(&scope, &input, 2, Some(first.report_id), instant(30)).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(correction.measurements.counts["observed"], 1);
        assert_eq!(correction.correction_of, Some(first.report_id));
        assert_eq!(repo.get(&scope, first.report_id).await.unwrap(), first);
        assert_eq!(repo.list(&scope, input.project_id).await.unwrap().len(), 2);
        let mut changed = input.clone();
        changed.measurement_targets = Some(vec![]);
        let conflict = repo
            .create(
                &scope,
                reduce_report(&scope, &changed, 1, None, instant(30)).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code, ErrorCode::Conflict);
        let alien = TenantScope::new(
            scope.operator_id,
            TenantId(Uuid::new_v4()),
            scope.project_id,
        );
        assert_eq!(
            repo.get(&alien, first.report_id).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn canonical_order_and_missing_proof_are_not_observed() {
        let (scope, mut input) = setup();
        let distribution = input
            .input_manifest_versions
            .iter_mut()
            .find(|r| r.kind == ReportManifestKind::Distribution)
            .unwrap();
        distribution.sealed = true;
        distribution.expected_count = Some(2);
        let published_id = Uuid::new_v4();
        let receipt = evidence("publication_receipt", published_id);
        input.publication_targets = Some(vec![
            ReportPublicationTarget {
                target_id: Uuid::new_v4(),
                platform_id: "first".to_owned(),
                status: ReportPublicationStatus::Verified,
                reason: None,
                evidence: vec![ReportEvidenceReference {
                    received_at: None,
                    ..receipt.clone()
                }],
            },
            ReportPublicationTarget {
                target_id: published_id,
                platform_id: "second".to_owned(),
                status: ReportPublicationStatus::Published,
                reason: None,
                evidence: vec![receipt],
            },
        ]);
        let first = reduce_report(&scope, &input, 1, None, instant(29)).unwrap();
        assert_eq!(first.publications.counts["unknown"], 1);
        assert_eq!(first.publications.counts["published"], 1);
        input.publication_targets.as_mut().unwrap().reverse();
        input.document_manifest.as_mut().unwrap().items.reverse();
        input.input_manifest_versions.reverse();
        let reordered = reduce_report(&scope, &input, 1, None, instant(29)).unwrap();
        assert_eq!(first.input_hash, reordered.input_hash);
        assert_eq!(first.report_id, reordered.report_id);
    }

    #[test]
    fn independent_lookup_asset_is_reported_without_changing_unknown_status() {
        let (scope, mut input) = setup();
        let distribution = input
            .input_manifest_versions
            .iter_mut()
            .find(|r| r.kind == ReportManifestKind::Distribution)
            .unwrap();
        distribution.sealed = true;
        distribution.expected_count = Some(1);
        let target_id = Uuid::new_v4();
        let asset =
            publication_lookup_asset_evidence(target_id, Uuid::new_v4(), instant(25), instant(26));
        input.publication_targets = Some(vec![ReportPublicationTarget {
            target_id,
            platform_id: "first".to_owned(),
            status: ReportPublicationStatus::Unknown,
            reason: None,
            evidence: vec![asset.clone()],
        }]);
        let report = reduce_report(&scope, &input, 1, None, instant(29)).unwrap();
        assert_eq!(report.publications.counts["unknown"], 1);
        assert!(!report.publications.counts.contains_key("verified"));
        assert!(report.findings.iter().any(|finding| {
            finding.kind == "publication_asset_observed"
                && finding.evidence_ids == [asset.evidence_id]
                && finding
                    .insufficient_reason
                    .as_deref()
                    .unwrap()
                    .contains("causal")
        }));
        assert!(report.evidence.contains(&asset));
        let mut misbound = input.clone();
        misbound.publication_targets.as_mut().unwrap()[0].evidence[0].resource_id = Uuid::new_v4();
        let wrong_target = reduce_report(&scope, &misbound, 1, None, instant(29)).unwrap();
        assert!(!wrong_target.evidence.contains(&asset));
        assert!(
            !wrong_target
                .findings
                .iter()
                .any(|finding| finding.kind == "publication_asset_observed")
        );
        let mut late = input.clone();
        late.publication_targets.as_mut().unwrap()[0].evidence[0].received_at = Some(instant(29));
        let initial = reduce_report(&scope, &late, 1, None, instant(29)).unwrap();
        assert!(
            !initial
                .findings
                .iter()
                .any(|finding| finding.kind == "publication_asset_observed")
        );
        let correction =
            reduce_report(&scope, &late, 2, Some(initial.report_id), instant(30)).unwrap();
        assert_eq!(correction.publications.counts["unknown"], 1);
        assert!(
            correction
                .findings
                .iter()
                .any(|finding| finding.kind == "publication_asset_observed")
        );
        assert_eq!(initial.publications, correction.publications);
    }

    #[test]
    fn wrong_kind_reused_or_noncausal_evidence_does_not_prove_independent_samples() {
        let (scope, mut input) = setup();
        input.input_manifest_versions.push(ReportManifestRef {
            kind: ReportManifestKind::Measurement,
            manifest_id: Uuid::new_v4(),
            revision: 1,
            sealed: true,
            expected_count: Some(3),
        });
        let first_id = Uuid::new_v4();
        let observed = evidence("observation", first_id);
        let second_id = Uuid::new_v4();
        let third_id = Uuid::new_v4();
        input.measurement_targets = Some(vec![
            ReportMeasurementTarget {
                target_id: first_id,
                comparison_key: "api/protocol-1".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::Observed,
                missing_reason: None,
                evidence: vec![observed.clone()],
            },
            ReportMeasurementTarget {
                target_id: second_id,
                comparison_key: "api/protocol-1".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::NotMentioned,
                missing_reason: None,
                evidence: vec![observed],
            },
            ReportMeasurementTarget {
                target_id: third_id,
                comparison_key: "web/protocol-2".to_owned(),
                scheduled_at: instant(25),
                status: ReportMeasurementStatus::Refused,
                missing_reason: None,
                evidence: vec![ReportEvidenceReference {
                    kind: "publication_receipt".to_owned(),
                    occurred_at: Some(instant(26)),
                    received_at: Some(instant(25)),
                    ..evidence("observation", third_id)
                }],
            },
        ]);
        let report = reduce_report(&scope, &input, 1, None, instant(29)).unwrap();
        assert_eq!(report.measurements.counts["observed"], 1);
        assert_eq!(report.measurements.counts["missing"], 2);
        assert!(!report.measurements.counts.contains_key("not_mentioned"));
        assert_eq!(report.measurement_groups[0].coverage.expected_count, None);
    }
}
