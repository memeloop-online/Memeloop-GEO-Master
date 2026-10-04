//! Scoped report reads and an explicit, reusable due-cycle fan-in entry point.
//! Only server-owned repository snapshots are accepted as reducer inputs.

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, DistributionPublicationResult, DistributionTarget, DistributionTargetStatus,
    ErrorCode, ProjectId, ReportManifestKind, ReportManifestRef, ReportPublicationStatus,
    ReportPublicationTarget, ReportReduceInput, ReportSnapshot, TenantScope, reduce_report,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthContext, ErrorResponse, RequestContext, api_error,
    require_project_writer,
};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportProjectQuery {
    pub project_id: ProjectId,
    /// Tenant selection is consumed by authentication middleware.
    #[serde(default)]
    #[allow(dead_code)]
    pub tenant_id: Option<String>,
}

#[derive(Debug, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReduceRequest {
    /// A correction must explicitly name the immutable report it supersedes.
    pub correction_of: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReportList {
    pub items: Vec<ReportSnapshot>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReportEvidenceList {
    pub items: Vec<geo_domain::ReportEvidenceReference>,
}

fn formal_publication_target(
    target: DistributionTarget,
    outcome: Option<DistributionPublicationResult>,
) -> ReportPublicationTarget {
    let status = if let Some(result) = &outcome {
        // A later source deferral does not undo an earlier external send.
        result.status
    } else {
        match target.status {
            DistributionTargetStatus::Blocked => ReportPublicationStatus::Blocked,
            DistributionTargetStatus::Deferred => ReportPublicationStatus::Deferred,
            DistributionTargetStatus::NotApplicable => ReportPublicationStatus::NotApplicable,
            DistributionTargetStatus::Cancelled => ReportPublicationStatus::Cancelled,
            DistributionTargetStatus::ReusedUnknown | DistributionTargetStatus::ReusedVerified => {
                ReportPublicationStatus::Unknown
            }
            DistributionTargetStatus::Pending | DistributionTargetStatus::Ready => {
                ReportPublicationStatus::Planned
            }
        }
    };
    ReportPublicationTarget {
        target_id: target.target_id,
        platform_id: target.platform_id,
        status,
        reason: target
            .reason
            .or_else(|| outcome.as_ref().and_then(|result| result.reason.clone()))
            .or_else(|| {
                (target.status == DistributionTargetStatus::ReusedVerified
                    && status == ReportPublicationStatus::Unknown)
                    .then(|| {
                        "Reused intent has no target-associated public verification evidence"
                            .to_owned()
                    })
            }),
        evidence: outcome.map_or_else(Vec::new, |result| result.evidence),
    }
}

async fn project_scope(
    state: &AppState,
    tenant_scope: &TenantScope,
    project_id: ProjectId,
) -> Result<TenantScope, AppError> {
    state
        .project_repository()
        .get(tenant_scope, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(TenantScope::new(
        tenant_scope.operator_id,
        tenant_scope.tenant_id,
        Some(project_id),
    ))
}

/// Shared service for HTTP, trusted agent tools and a future scoped scheduler.
/// It never takes observations or publication results from an API request.
pub async fn reduce_cycle_report(
    state: &AppState,
    scope: &TenantScope,
    cycle_id: Uuid,
    correction_of: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<ReportSnapshot, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let cycle = state
        .project_repository()
        .get_report_cycle(scope, project_id, cycle_id)
        .await?
        .ok_or_else(|| AppError::not_found("cycle not found"))?;
    let existing = state.report_repository().list(scope, project_id).await?;
    let revisions: Vec<_> = existing
        .iter()
        .filter(|report| {
            report.cycle_id == cycle_id
                && report.report_window_start_at == cycle.report_window_start_at
                && report.report_window_end_at == cycle.report_window_end_at
                && report.cutoff_at == cycle.cutoff_at
        })
        .collect();
    let revision = if let Some(parent_id) = correction_of {
        if let Some(replay) = revisions
            .iter()
            .find(|r| r.correction_of == Some(parent_id))
        {
            return Ok((*replay).clone());
        }
        let parent = revisions
            .iter()
            .find(|r| r.report_id == parent_id)
            .ok_or_else(|| AppError::conflict("correction parent was not found in this cycle"))?;
        if revisions.iter().any(|r| r.revision > parent.revision) {
            return Err(AppError::conflict(
                "correction parent is not the latest revision",
            ));
        }
        parent.revision + 1
    } else {
        if let Some(replay) = revisions.iter().find(|r| r.revision == 1) {
            schedule_successor_after_report(state, scope, replay, now).await;
            return Ok((*replay).clone());
        }
        1
    };
    // A replay returns above even if sources changed or their adapter is down.
    // A new snapshot must respect the frozen due time; no HTTP caller can
    // accelerate a weekly cutoff or supply synthetic "successful" samples.
    if now < cycle.cutoff_at {
        return Err(AppError::new(
            geo_domain::ErrorCode::NotReady,
            "report cutoff has not arrived",
        ));
    }
    let document_manifest = if let Some(reference) = &cycle.document_manifest {
        state
            .knowledge_repository()
            .get_document_manifest(scope, reference.manifest_id)
            .await?
    } else {
        None
    };
    let doc = document_manifest.as_ref();
    if doc.is_some_and(|manifest| {
        cycle.document_manifest.as_ref().is_none_or(|reference| {
            manifest.manifest_id != reference.manifest_id || manifest.revision != reference.revision
        }) || manifest.project_id != project_id
    }) {
        return Err(AppError::conflict(
            "document manifest does not match the frozen start revision",
        ));
    }
    // Manifest identity is always fixed at the original cutoff. Corrections
    // can see later versions of those targets, not manifests frozen later or
    // a newer manifest revision that would rewrite the original denominator.
    let distribution_repository = state.distribution_repository();
    let formal = if let Some(parent_id) = correction_of {
        let parent = revisions
            .iter()
            .find(|report| report.report_id == parent_id)
            .expect("correction parent checked above");
        let reference = parent.input_manifest_versions.iter().find(|reference| {
            reference.kind == ReportManifestKind::Distribution && reference.sealed
        });
        if let Some(reference) = reference {
            match distribution_repository
                .get(scope, reference.manifest_id)
                .await
            {
                Ok(manifest)
                    if manifest.revision == reference.revision
                        && manifest.sealed_at <= cycle.cutoff_at =>
                {
                    let snapshot = distribution_repository
                        .as_of(scope, manifest.manifest_id, now)
                        .await?;
                    Some((manifest, snapshot.targets))
                }
                Ok(_) => None,
                Err(error) if error.code == ErrorCode::NotFound => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        }
    } else {
        let formal_at_cutoff = distribution_repository
            .cycle_inputs(scope, cycle_id, cycle.cutoff_at)
            .await?;
        formal_at_cutoff
            .manifest
            .map(|manifest| (manifest, formal_at_cutoff.targets))
    };
    if let Some((manifest, targets)) = &formal
        && (manifest.project_id != project_id
            || manifest.cycle_id != cycle_id
            || manifest.sealed_at > cycle.cutoff_at
            || cycle.document_manifest.as_ref().is_some_and(|reference| {
                manifest.document_manifest_id != reference.manifest_id
                    || manifest.document_manifest_revision != reference.revision
            })
            || targets.len() as u64 > manifest.expected_count
            || targets.iter().any(|target| {
                target.manifest_id != manifest.manifest_id
                    || target.ordinal >= manifest.expected_count
            }))
    {
        return Err(AppError::conflict(
            "distribution inputs do not match the frozen cycle and cutoff",
        ));
    }
    // Legacy publication plans only fill a gap when no formal distribution
    // manifest existed at cutoff. Independent measurement inputs remain.
    let channel_inputs = state
        .channel_job_repository()
        .cycle_inputs(
            scope,
            cycle_id,
            if correction_of.is_some() {
                now
            } else {
                cycle.cutoff_at
            },
        )
        .await?;
    let has_distribution = formal.is_some()
        || channel_inputs
            .manifests
            .iter()
            .any(|reference| reference.kind == ReportManifestKind::Distribution);
    let formal_reference = formal.as_ref().map(|(manifest, _)| ReportManifestRef {
        kind: ReportManifestKind::Distribution,
        manifest_id: manifest.manifest_id,
        revision: manifest.revision,
        sealed: true,
        expected_count: Some(manifest.expected_count),
    });
    let formal_publications = if let Some((_, targets)) = formal {
        let evidence_at = if correction_of.is_some() {
            now
        } else {
            cycle.cutoff_at
        };
        let results = distribution_repository
            .publication_results(scope, &targets, evidence_at)
            .await?;
        let mut outcomes: std::collections::HashMap<_, _> = results
            .into_iter()
            .map(|result| (result.target_id, result))
            .collect();
        Some(
            targets
                .into_iter()
                .map(|target| {
                    let outcome = outcomes.remove(&target.target_id);
                    formal_publication_target(target, outcome)
                })
                .collect(),
        )
    } else {
        None
    };
    let input = ReportReduceInput {
        project_id,
        cycle_id,
        report_window_start_at: cycle.report_window_start_at,
        report_window_end_at: cycle.report_window_end_at,
        report_timezone: cycle.report_timezone,
        cutoff_at: cycle.cutoff_at,
        input_temporal_provenance_verified: false,
        input_manifest_versions: cycle
            .document_manifest
            .iter()
            .map(|reference| ReportManifestRef {
                kind: ReportManifestKind::Document,
                manifest_id: reference.manifest_id,
                revision: reference.revision,
                sealed: doc.is_some_and(|m| m.sealed),
                expected_count: doc
                    .and_then(|m| m.expected_count)
                    .and_then(|n| n.try_into().ok()),
            })
            .chain(
                cycle
                    .distribution_manifest
                    .iter()
                    .filter(|_| !has_distribution)
                    .map(|reference| ReportManifestRef {
                        kind: ReportManifestKind::Distribution,
                        manifest_id: reference.manifest_id,
                        revision: reference.revision,
                        sealed: reference.sealed,
                        expected_count: reference.expected_count.and_then(|n| n.try_into().ok()),
                    }),
            )
            .chain(formal_reference)
            .chain(channel_inputs.manifests.into_iter().filter(|reference| {
                formal_publications.is_none() || reference.kind != ReportManifestKind::Distribution
            }))
            .collect(),
        document_manifest,
        publication_targets: formal_publications.or(channel_inputs.publications),
        measurement_targets: channel_inputs.measurements,
    };
    let snapshot = reduce_report(scope, &input, revision, correction_of, now)?;
    let snapshot = state.report_repository().create(scope, snapshot).await?;
    schedule_successor_after_report(state, scope, &snapshot, now).await;
    Ok(snapshot)
}

async fn schedule_successor_after_report(
    state: &AppState,
    scope: &TenantScope,
    report: &ReportSnapshot,
    now: DateTime<Utc>,
) {
    if report.revision != 1 {
        return;
    }
    // Saving the report and scheduling are separate durable operations.
    // The database recovery scanner also finds reports without a successor.
    // Scheduling failure must never hide a successfully persisted report.
    if let Err(error) = state
        .project_repository()
        .schedule_next_cycle(scope, report.project_id, report.cycle_id, now)
        .await
    {
        tracing::warn!(code = ?error.code, "report successor not scheduled");
    }
}

#[utoipa::path(
    get, path = "/api/v1/projects/{id}/reports",
    security(("sessionCookie" = [])),
    params(("id" = ProjectId, Path)),
    responses((status = 200, body = ReportList), (status = 404, body = ErrorResponse))
)]
pub(crate) async fn list_reports(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ReportList>, ApiError> {
    let scope = project_scope(&state, &tenant_scope, id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let items = state
        .report_repository()
        .list(&scope, id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    Ok(Json(ReportList { items }))
}

#[utoipa::path(
    get, path = "/api/v1/reports/{id}",
    security(("sessionCookie" = [])),
    params(("id" = Uuid, Path), ("project_id" = ProjectId, Query)),
    responses((status = 200, body = ReportSnapshot), (status = 404, body = ErrorResponse))
)]
pub(crate) async fn get_report(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ReportProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ReportSnapshot>, ApiError> {
    let scope = project_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    state
        .report_repository()
        .get(&scope, id)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

#[utoipa::path(
    get, path = "/api/v1/reports/{id}/evidence",
    security(("sessionCookie" = [])),
    params(("id" = Uuid, Path), ("project_id" = ProjectId, Query)),
    responses((status = 200, body = ReportEvidenceList), (status = 404, body = ErrorResponse))
)]
pub(crate) async fn get_report_evidence(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ReportProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ReportEvidenceList>, ApiError> {
    let scope = project_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let report = state
        .report_repository()
        .get(&scope, id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    Ok(Json(ReportEvidenceList {
        items: report.evidence,
    }))
}

#[utoipa::path(
    post, path = "/api/v1/cycles/{id}/reductions",
    security(("sessionCookie" = [])),
    params(("id" = Uuid, Path), ("project_id" = ProjectId, Query)),
    request_body = ReduceRequest,
    responses((status = 200, body = ReportSnapshot), (status = 403, body = ErrorResponse))
)]
pub(crate) async fn create_reduction(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ReportProjectQuery>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<ReduceRequest>,
) -> Result<Json<ReportSnapshot>, ApiError> {
    require_project_writer(&auth).map_err(|e| api_error(e, context.request_id))?;
    let scope = project_scope(&state, &auth.scope, query.project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    reduce_cycle_report(&state, &scope, id, request.correction_of, Utc::now())
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

#[cfg(test)]
mod projection_tests {
    use super::*;

    #[test]
    fn attempted_publication_is_not_hidden_by_later_source_deferral() {
        let target_id = Uuid::new_v4();
        let target = DistributionTarget {
            target_id,
            manifest_id: Uuid::new_v4(),
            ordinal: 0,
            document_item_id: Uuid::new_v4(),
            content_revision_id: Some(Uuid::new_v4()),
            platform_id: "test-platform".into(),
            placement_slot: "primary".into(),
            variant_id: Some(Uuid::new_v4()),
            account_id: Some(Uuid::new_v4()),
            publication_intent_id: Some(Uuid::new_v4()),
            status: DistributionTargetStatus::Deferred,
            reason: Some("source_changed".into()),
            version: 3,
        };
        let verified = formal_publication_target(
            target,
            Some(DistributionPublicationResult {
                target_id,
                status: ReportPublicationStatus::Verified,
                reason: None,
                evidence: vec![],
            }),
        );
        assert_eq!(verified.status, ReportPublicationStatus::Verified);
        assert_eq!(verified.reason.as_deref(), Some("source_changed"));
    }
}
