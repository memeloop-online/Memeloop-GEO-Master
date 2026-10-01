//! Scoped report reads and an explicit, reusable due-cycle fan-in entry point.
//! Only server-owned repository snapshots are accepted as reducer inputs.

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ProjectId, ReportManifestKind, ReportManifestRef, ReportReduceInput, ReportSnapshot,
    TenantScope, reduce_report,
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
                    .map(|reference| ReportManifestRef {
                        kind: ReportManifestKind::Distribution,
                        manifest_id: reference.manifest_id,
                        revision: reference.revision,
                        sealed: reference.sealed,
                        expected_count: reference.expected_count.and_then(|n| n.try_into().ok()),
                    }),
            )
            .collect(),
        document_manifest,
        publication_targets: None,
        measurement_targets: None,
    };
    let snapshot = reduce_report(scope, &input, revision, correction_of, now)?;
    state.report_repository().create(scope, snapshot).await
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
