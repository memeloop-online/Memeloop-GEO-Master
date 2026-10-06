//! Scoped, read-only and explicitly page-local independent-measurement citation insights.
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use geo_domain::{
    AppError, ChannelAttempt, ChannelOutcomeStatus, ChannelTargetView, CitationInsightPage,
    ProjectId, StandaloneMeasurementPlan, TenantScope, summarize_citations,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{ApiError, AppState, RequestContext, api_error};

/// Verify the persisted Rust-owned receipt and re-run the SAME official-search
/// acceptance validator that originally admitted this outcome. The normalized
/// result fields are derived from the accepted outcome; they do not reintroduce
/// a second parser or trust a consumer-supplied search flag.
fn accepted_live_search(view: &ChannelTargetView, attempt: &ChannelAttempt) -> bool {
    let Some(outcome) = &attempt.outcome else {
        return false;
    };
    if outcome.fixture || outcome.status != ChannelOutcomeStatus::Observed {
        return false;
    }
    let mut markers = outcome.runner_evidence.iter().filter(|evidence| {
        evidence.get("kind").and_then(serde_json::Value::as_str) == Some("runner_receipt")
    });
    let Some(marker) = markers.next() else {
        return false;
    };
    if markers.next().is_some()
        || marker
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("geo.runner.receipt.v1")
        || marker.get("provenance").and_then(serde_json::Value::as_str) != Some("live")
        || marker
            .get("execution_id")
            .and_then(serde_json::Value::as_str)
            != Some(attempt.attempt_id.to_string().as_str())
        || marker
            .get("connector_version")
            .and_then(serde_json::Value::as_str)
            != outcome.connector_version.as_deref()
        || marker.get("occurred_at") != serde_json::to_value(outcome.occurred_at).ok().as_ref()
    {
        return false;
    }
    let Some(received_at) = attempt.received_at else {
        return false;
    };
    let result = crate::browser_bridge::BrowserExecution {
        execution_id: attempt.attempt_id,
        provenance: Some(crate::browser_bridge::BrowserReceiptProvenance::Live),
        status: "completed".into(),
        stage: Some("official_search_observation".into()),
        reason: None,
        evidence: outcome
            .runner_evidence
            .iter()
            .filter(|evidence| {
                evidence.get("kind").and_then(serde_json::Value::as_str) != Some("runner_receipt")
            })
            .cloned()
            .collect(),
        public_url: outcome.public_url.clone(),
        occurred_at: Some(outcome.occurred_at),
        connector_version: outcome.connector_version.clone(),
    };
    crate::channel_jobs::measurement_observation(
        &result,
        &view.target,
        attempt.claimed_at,
        received_at,
    )
    .is_some_and(|(status, answer, citations)| {
        status == outcome.status
            && Some(answer.as_str()) == outcome.raw_answer.as_deref()
            && citations == outcome.citations
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitationInsightsQuery {
    pub after: Option<Uuid>,
    pub limit: Option<usize>,
    /// Authentication middleware may use a tenant selector; it never changes
    /// the project scope established by `channel_jobs::scope`.
    pub tenant_id: Option<String>,
    pub project_id: Option<ProjectId>,
}

/// The repository is the authority for both plan ownership and persisted attempts.
/// Each response covers exactly the returned plans, never the entire project.
pub async fn get_citation_insights(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<CitationInsightsQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<CitationInsightPage>, ApiError> {
    let result = async {
        let _ = (query.tenant_id, query.project_id);
        let scope = crate::channel_jobs::scope(&state, &tenant, project_id).await?;
        read_citation_page(&state, &scope, query.after, query.limit).await
    }
    .await;
    result
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

pub(crate) async fn read_citation_page(
    state: &AppState,
    scope: &TenantScope,
    after: Option<Uuid>,
    limit: Option<usize>,
) -> Result<CitationInsightPage, AppError> {
    read_page(state, scope, after, limit, false).await
}

pub(crate) async fn read_optimization_citation_page(
    state: &AppState,
    scope: &TenantScope,
    after: Option<Uuid>,
    limit: Option<usize>,
) -> Result<CitationInsightPage, AppError> {
    read_page(state, scope, after, limit, true).await
}

async fn read_page(
    state: &AppState,
    scope: &TenantScope,
    after: Option<Uuid>,
    limit: Option<usize>,
    optimization_only: bool,
) -> Result<CitationInsightPage, AppError> {
    let limit = limit.unwrap_or(5);
    if !(1..=10).contains(&limit) {
        return Err(AppError::invalid_request("limit must be 1 to 10"));
    }
    let repository = state.channel_job_repository();
    let mut plans = if optimization_only {
        repository
            .list_optimization_measurement_plans(scope, after, limit + 1)
            .await?
    } else {
        repository
            .list_measurement_plans(scope, after, limit + 1)
            .await?
    };
    let has_more = plans.len() > limit;
    plans.truncate(limit);
    let next_after = has_more.then(|| plans.last().expect("nonempty page").plan_id);
    let mut inputs: Vec<(StandaloneMeasurementPlan, Vec<ChannelTargetView>)> =
        Vec::with_capacity(plans.len());
    for plan in plans {
        let mut views = Vec::with_capacity(plan.targets.len());
        for target in &plan.targets {
            views.push(repository.get_target(scope, target.target_id).await?);
        }
        inputs.push((plan, views));
    }
    summarize_citations(scope, &inputs, next_after, accepted_live_search)
}
