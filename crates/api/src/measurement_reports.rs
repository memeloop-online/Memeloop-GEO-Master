//! Cycle-free reporting over persisted independent measurement records.
//! No sampler, provider, model, optimizer or successor-cycle calls.

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, header::CACHE_CONTROL},
};
use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    AppError, ChannelOutcomeStatus, ChannelTargetInput, MeasurementPeriodPreview,
    MeasurementPeriodReport, MeasurementPeriodSample, MeasurementPeriodSearchSample,
    MeasurementPeriodWindow, ProjectId, TenantScope, build_measurement_period_search_sample,
    effective_observation, freeze_measurement_period, preview_measurement_period_with_search,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthContext, ErrorResponse, RequestContext, api_error,
    require_project_writer,
};

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPeriodQuery {
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub report_timezone: Option<String>,
    pub project_id: Option<ProjectId>,
    pub tenant_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPeriodRequest {
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    #[serde(default = "utc")]
    pub report_timezone: String,
    pub correction_of: Option<Uuid>,
}

fn utc() -> String {
    "UTC".to_owned()
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeasurementPeriodList {
    pub items: Vec<MeasurementPeriodReport>,
}

fn microseconds(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(at.timestamp_micros()).expect("supported timestamp")
}

pub fn default_measurement_period_window(now: DateTime<Utc>) -> MeasurementPeriodWindow {
    let now = microseconds(now);
    MeasurementPeriodWindow {
        start_at: now - Duration::days(7),
        end_at: now,
        report_timezone: utc(),
    }
}

/// UI and P00 share this read-only service and the exact returned window.
pub async fn preview_project_measurements(
    state: &AppState,
    scope: &TenantScope,
    window: Option<MeasurementPeriodWindow>,
    now: DateTime<Utc>,
) -> Result<MeasurementPeriodPreview, AppError> {
    let now = microseconds(now);
    let window = window.unwrap_or_else(|| default_measurement_period_window(now));
    let window = MeasurementPeriodWindow {
        start_at: microseconds(window.start_at),
        end_at: microseconds(window.end_at),
        ..window
    };
    window.validate(now)?;
    ensure_project(state, scope).await?;
    let samples = collect_samples(state, scope, &window, now, None).await?;
    let search = collect_search_samples(state, scope, &window, now, None).await?;
    preview_measurement_period_with_search(scope, &window, samples, search, now)
}

/// Window identity is the initial-create idempotency boundary. A correction
/// names its parent; either replay returns before touching mutable sources.
pub async fn save_project_measurement_report(
    state: &AppState,
    scope: &TenantScope,
    request: MeasurementPeriodRequest,
    now: DateTime<Utc>,
) -> Result<MeasurementPeriodReport, AppError> {
    ensure_project(state, scope).await?;
    let now = microseconds(now);
    let window = MeasurementPeriodWindow {
        start_at: microseconds(request.start_at),
        end_at: microseconds(request.end_at),
        report_timezone: request.report_timezone,
    };
    window.validate(now)?;
    let repository = state.report_repository();
    let rows = repository.list_measurement_periods(scope).await?;
    let matching: Vec<_> = rows.iter().filter(|row| row.window() == window).collect();
    let parent = if let Some(parent_id) = request.correction_of {
        if let Some(replay) = matching
            .iter()
            .find(|row| row.correction_of == Some(parent_id))
        {
            return Ok((*replay).clone());
        }
        let parent = matching
            .iter()
            .find(|row| row.report_id == parent_id)
            .ok_or_else(|| AppError::conflict("measurement report correction parent not found"))?;
        if matching.iter().any(|row| row.revision > parent.revision) {
            return Err(AppError::conflict(
                "measurement report correction parent is not latest",
            ));
        }
        Some(*parent)
    } else {
        if let Some(replay) = matching.iter().find(|row| row.revision == 1) {
            return Ok((*replay).clone());
        }
        None
    };
    let samples = collect_samples(state, scope, &window, now, parent).await?;
    let search = collect_search_samples(state, scope, &window, now, parent).await?;
    let preview = preview_measurement_period_with_search(scope, &window, samples, search, now)?;
    let report = freeze_measurement_period(
        scope,
        preview,
        parent.map_or(1, |row| row.revision + 1),
        request.correction_of,
    )?;
    repository.create_measurement_period(scope, report).await
}

async fn ensure_project(state: &AppState, scope: &TenantScope) -> Result<(), AppError> {
    let id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    state
        .project_repository()
        .get(scope, id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(())
}

async fn collect_samples(
    state: &AppState,
    scope: &TenantScope,
    window: &MeasurementPeriodWindow,
    as_of: DateTime<Utc>,
    parent: Option<&MeasurementPeriodReport>,
) -> Result<Vec<MeasurementPeriodSample>, AppError> {
    let repository = state.channel_job_repository();
    let mut identities = Vec::new();
    if let Some(parent) = parent {
        // Corrections preserve the first report's frozen sample denominator.
        identities.extend(
            parent
                .samples
                .iter()
                .map(|sample| (sample.plan_id, sample.target_id)),
        );
    } else {
        let mut after = None;
        loop {
            let page = repository.list_measurement_plans(scope, after, 100).await?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|plan| plan.plan_id);
            for plan in page {
                if Some(plan.project_id) != scope.project_id || plan.created_at > as_of {
                    continue;
                }
                for target in plan.targets {
                    if let ChannelTargetInput::Measure { scheduled_at, .. } = target.input
                        && scheduled_at >= window.start_at
                        && scheduled_at < window.end_at
                    {
                        identities.push((plan.plan_id, target.target_id));
                    }
                }
            }
        }
    }
    let mut samples = Vec::new();
    for chunk in identities.chunks(64) {
        let mut views = Vec::new();
        for (_, target_id) in chunk {
            let mut view = repository.get_target(scope, *target_id).await?;
            view.attempts.retain(|attempt| attempt.claimed_at <= as_of);
            views.push(view);
        }
        let analyses = if let Some(resolver) = state.observation_evidence_resolver() {
            resolver.resolve(scope, &views, as_of).await?
        } else {
            vec![]
        };
        for (view, (plan_id, _)) in views.iter().zip(chunk) {
            let ChannelTargetInput::Measure {
                scheduled_at,
                question_binding,
                ..
            } = &view.target.input
            else {
                return Err(AppError::conflict("measurement report target changed kind"));
            };
            let attempt = view.attempts.last();
            let outcome = attempt
                .filter(|attempt| attempt.received_at.is_some_and(|at| at <= as_of))
                .and_then(|attempt| attempt.outcome.as_ref())
                .filter(|outcome| outcome.occurred_at <= as_of);
            let original_status = match outcome.map(|outcome| outcome.status) {
                None => "pending",
                Some(ChannelOutcomeStatus::Unknown) => "unknown",
                Some(ChannelOutcomeStatus::Observed) => "observed",
                Some(ChannelOutcomeStatus::Refused) => "refused",
                Some(ChannelOutcomeStatus::Missing) => "missing",
                Some(ChannelOutcomeStatus::Failed) => "failed",
                Some(ChannelOutcomeStatus::LoginRequired) => "login_required",
                Some(ChannelOutcomeStatus::Unsupported) => "unsupported",
                _ => {
                    return Err(AppError::conflict(
                        "publication outcome on measurement report target",
                    ));
                }
            };
            let live = effective_observation(
                view,
                &[],
                as_of,
                crate::citation_insights::accepted_live_search,
            )
            .filter(|observation| {
                observation.observed_at >= window.start_at
                    && observation.observed_at < window.end_at
            });
            let observation = effective_observation(
                view,
                &analyses,
                as_of,
                crate::citation_insights::accepted_live_search,
            );
            samples.push(MeasurementPeriodSample {
                plan_id: *plan_id,
                target_id: view.target.target_id,
                attempt_id: attempt.map(|attempt| attempt.attempt_id),
                comparison_key: view
                    .target
                    .input
                    .comparison_key()
                    .expect("measurement protocol"),
                question_binding: question_binding.clone(),
                scheduled_at: *scheduled_at,
                original_status: original_status.to_owned(),
                observed_live: live.is_some(),
                observation,
            });
        }
    }
    Ok(samples)
}

async fn collect_search_samples(
    state: &AppState,
    scope: &TenantScope,
    window: &MeasurementPeriodWindow,
    as_of: DateTime<Utc>,
    parent: Option<&MeasurementPeriodReport>,
) -> Result<Option<Vec<MeasurementPeriodSearchSample>>, AppError> {
    use std::collections::BTreeMap;
    // An old snapshot did not freeze a search cohort. A correction must not
    // retroactively add one or represent unavailable history as zero samples.
    if parent.is_some_and(|report| report.search.is_none()) {
        return Ok(None);
    }
    let Some(service) = state.serp_service() else {
        return if parent.is_some() {
            Err(AppError::capability_missing(
                "saved search report evidence is unavailable",
            ))
        } else {
            Ok(None)
        };
    };
    let repository = service.report_repository();
    let mut cohort = BTreeMap::new();
    if let Some(section) = parent.and_then(|report| report.search.as_ref()) {
        for chunk in section.samples.chunks(100) {
            let ids: Vec<_> = chunk
                .iter()
                .map(|sample| sample.cohort.measurement_id)
                .collect();
            let current: BTreeMap<_, _> = repository
                .get_report_measurements(scope, &ids)
                .await?
                .into_iter()
                .map(|identity| (identity.measurement_id, identity))
                .collect();
            if current.len() != chunk.len()
                || chunk.iter().any(|sample| {
                    current.get(&sample.cohort.measurement_id) != Some(&sample.cohort)
                })
            {
                return Err(AppError::conflict("search report cohort identity changed"));
            }
            cohort.extend(current);
        }
    } else {
        let mut after = None;
        loop {
            let page = repository
                .list_report_measurements(scope, window, as_of, after, 100)
                .await?;
            if page.is_empty() {
                break;
            }
            for identity in page {
                if after.is_some_and(|cursor| identity.measurement_id <= cursor) {
                    return Err(AppError::conflict("search report cohort cursor invalid"));
                }
                after = Some(identity.measurement_id);
                cohort.insert(identity.measurement_id, identity);
            }
        }
    }
    let identities: Vec<_> = cohort.into_values().collect();
    let mut samples = Vec::with_capacity(identities.len());
    for chunk in identities.chunks(100) {
        let ids: Vec<_> = chunk
            .iter()
            .map(|identity| identity.measurement_id)
            .collect();
        let mut observations = BTreeMap::<_, Vec<_>>::new();
        let mut after = None;
        loop {
            let page = repository
                .list_report_observations(scope, &ids, as_of, after, 100)
                .await?;
            if page.is_empty() {
                break;
            }
            for candidate in page {
                let id = candidate.observation.observation_id;
                if after.is_some_and(|cursor| id <= cursor)
                    || !ids.contains(&candidate.observation.measurement_id)
                {
                    return Err(AppError::conflict(
                        "search report observation cursor or identity invalid",
                    ));
                }
                after = Some(id);
                observations
                    .entry(candidate.observation.measurement_id)
                    .or_default()
                    .push(candidate);
            }
        }
        for identity in chunk {
            samples.push(build_measurement_period_search_sample(
                identity.clone(),
                observations
                    .remove(&identity.measurement_id)
                    .unwrap_or_default(),
                window,
                as_of,
            )?);
        }
    }
    Ok(Some(samples))
}

#[utoipa::path(get, path="/api/v1/projects/{id}/measurement-report-preview",
    security(("sessionCookie"=[])), params(("id"=ProjectId,Path)),
    responses((status=200,body=MeasurementPeriodPreview),(status=404,body=ErrorResponse)))]
pub(crate) async fn preview(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Query(query): Query<MeasurementPeriodQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<(HeaderMap, Json<MeasurementPeriodPreview>), ApiError> {
    let result = async {
        let _ = (query.project_id, query.tenant_id);
        let scope = crate::channel_jobs::scope(&state, &tenant, id).await?;
        let now = Utc::now();
        let window = match (query.start_at, query.end_at) {
            (None, None) => Some(MeasurementPeriodWindow {
                start_at: now - Duration::days(7),
                end_at: now,
                report_timezone: query.report_timezone.unwrap_or_else(utc),
            }),
            (Some(start_at), Some(end_at)) => Some(MeasurementPeriodWindow {
                start_at,
                end_at,
                report_timezone: query.report_timezone.unwrap_or_else(utc),
            }),
            _ => {
                return Err(AppError::invalid_request(
                    "both report window bounds are required",
                ));
            }
        };
        preview_project_measurements(&state, &scope, window, now).await
    }
    .await
    .map_err(|error| api_error(error, context.request_id))?;
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((headers, Json(result)))
}

#[utoipa::path(get, path="/api/v1/projects/{id}/measurement-reports",
    security(("sessionCookie"=[])), params(("id"=ProjectId,Path)),
    responses((status=200,body=MeasurementPeriodList),(status=404,body=ErrorResponse)))]
pub(crate) async fn list(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<MeasurementPeriodList>, ApiError> {
    let result = async {
        let scope = crate::channel_jobs::scope(&state, &tenant, id).await?;
        state
            .report_repository()
            .list_measurement_periods(&scope)
            .await
    }
    .await;
    result
        .map(|items| Json(MeasurementPeriodList { items }))
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(get, path="/api/v1/measurement-reports/{id}",
    security(("sessionCookie"=[])), params(("id"=Uuid,Path),("project_id"=ProjectId,Query)),
    responses((status=200,body=MeasurementPeriodReport),(status=404,body=ErrorResponse)))]
pub(crate) async fn get(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<crate::reports::ReportProjectQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<MeasurementPeriodReport>, ApiError> {
    let result = async {
        let scope = crate::channel_jobs::scope(&state, &tenant, query.project_id).await?;
        state
            .report_repository()
            .get_measurement_period(&scope, id)
            .await
    }
    .await;
    result
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(post, path="/api/v1/projects/{id}/measurement-reports",
    security(("sessionCookie"=[])), params(("id"=ProjectId,Path)), request_body=MeasurementPeriodRequest,
    responses((status=200,body=MeasurementPeriodReport),(status=403,body=ErrorResponse)))]
pub(crate) async fn create(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<MeasurementPeriodRequest>,
) -> Result<Json<MeasurementPeriodReport>, ApiError> {
    let result = async {
        require_project_writer(&auth)?;
        let scope = crate::channel_jobs::scope(&state, &auth.scope, id).await?;
        save_project_measurement_report(&state, &scope, request, Utc::now()).await
    }
    .await;
    result
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}
