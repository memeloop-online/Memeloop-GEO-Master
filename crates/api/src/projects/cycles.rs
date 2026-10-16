//! Current and successor cycles are independent of immutable start acceptance.

use axum::{
    Json,
    extract::{Extension, Path, State},
};
use chrono::Utc;
use geo_domain::{AppError, CycleReportView, ProjectId, TenantScope};
use serde::Deserialize;
use uuid::Uuid;

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SuccessorRequest {
    predecessor_cycle_id: Uuid,
}

async fn scope(
    state: &AppState,
    tenant: &TenantScope,
    project_id: ProjectId,
) -> Result<TenantScope, AppError> {
    state
        .project_repository()
        .get(tenant, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(TenantScope::new(
        tenant.operator_id,
        tenant.tenant_id,
        Some(project_id),
    ))
}

pub(crate) async fn current(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<CycleReportView>, ApiError> {
    let scope = scope(&state, &tenant, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .project_repository()
        .get_current_cycle(&scope, project_id)
        .await
        .and_then(|cycle| cycle.ok_or_else(|| AppError::not_found("current cycle not found")))
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

pub(crate) async fn schedule_successor(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<SuccessorRequest>,
) -> Result<Json<CycleReportView>, ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let scope = scope(&state, &tenant, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    let reports = state
        .report_repository()
        .list(&scope, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    if !reports
        .iter()
        .any(|report| report.cycle_id == request.predecessor_cycle_id && report.revision == 1)
    {
        return Err(api_error(
            AppError::conflict("predecessor report must be saved before scheduling its successor"),
            context.request_id,
        ));
    }
    state
        .project_repository()
        .schedule_next_cycle(&scope, project_id, request.predecessor_cycle_id, Utc::now())
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}
