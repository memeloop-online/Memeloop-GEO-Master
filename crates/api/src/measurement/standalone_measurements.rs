//! Immutable, project-owned measurement plans with no optimization-cycle dependency.
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use chrono::Utc;
use geo_domain::{
    AppError, ChannelTarget, ChannelTargetInput, MeasurementReportConditions,
    ObservationAnalysisOutcome, ProjectId, ProjectStatus, QuestionPurpose,
    StandaloneMeasurementPlan, TenantScope, effective_observation, sha256_hex,
};
use geo_worker::{
    MeasurementModelOption, MeasurementOptionsRequest, MeasurementOptionsResult,
    MeasurementPlanCreateRequest, MeasurementPlanReadRequest, MeasurementPlanReceipt,
    MeasurementPlanStatus, MeasurementTargetStatus,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthContext, RequestContext, api_error,
    channel_jobs::{BoundMeasurementRequest, MeasurementRequest},
    require_project_writer,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementPlanRequest {
    pub idempotency_key: String,
    pub title: String,
    #[serde(default)]
    pub measurements: Vec<MeasurementRequest>,
    #[serde(default)]
    pub bound_measurements: Vec<BoundMeasurementRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanPageQuery {
    pub after: Option<Uuid>,
    pub limit: Option<usize>,
    pub tenant_id: Option<String>,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Serialize)]
pub struct MeasurementPlanPage {
    pub items: Vec<StandaloneMeasurementPlan>,
    pub next_after: Option<Uuid>,
}

fn request_hash(request: &MeasurementPlanRequest) -> Result<String, AppError> {
    // Explicit schedule is caller-owned. Generated plan/target IDs and creation
    // time must never participate in replay identity.
    let bytes = serde_json::to_vec(&(
        &request.title,
        &request.measurements,
        &request.bound_measurements,
    ))
    .map_err(|_| AppError::invalid_request("invalid measurement plan"))?;
    Ok(sha256_hex(&bytes))
}

fn target_id(plan_id: Uuid, input: &ChannelTargetInput) -> Result<Uuid, AppError> {
    let mut hash = Sha256::new();
    hash.update(b"geo.standalone_measurement.target.v1\0");
    hash.update(plan_id.as_bytes());
    hash.update(
        serde_json::to_vec(input)
            .map_err(|_| AppError::invalid_request("invalid measurement target"))?,
    );
    let bytes: [u8; 16] = hash.finalize()[..16].try_into().expect("sha256 length");
    Ok(Uuid::from_bytes(bytes))
}

/// Caller supplies an authenticated project scope. Knowledge and cycle state
/// are intentionally absent from the ad-hoc measurement path.
pub async fn create_measurement_plan(
    state: &AppState,
    scope: &TenantScope,
    request: MeasurementPlanRequest,
) -> Result<StandaloneMeasurementPlan, AppError> {
    let hash = request_hash(&request)?;
    create_measurement_plan_with_hash(state, scope, request, hash).await
}

async fn create_measurement_plan_with_hash(
    state: &AppState,
    scope: &TenantScope,
    request: MeasurementPlanRequest,
    input_hash: String,
) -> Result<StandaloneMeasurementPlan, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let project = state
        .project_repository()
        .get(scope, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    if matches!(
        project.status,
        ProjectStatus::Paused | ProjectStatus::Archived
    ) {
        return Err(AppError::conflict("project is inactive"));
    }
    if request.title.trim().is_empty() || request.title.len() > 200 {
        return Err(AppError::invalid_request("title is missing or too long"));
    }
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
        return Err(AppError::invalid_request(
            "idempotency key is missing or too long",
        ));
    }
    let count = request.measurements.len() + request.bound_measurements.len();
    if !(1..=100).contains(&count) {
        return Err(AppError::invalid_request(
            "measurement plan requires 1 to 100 targets",
        ));
    }
    let repository = state.channel_job_repository();
    if let Some(plan) = repository
        .replay_measurement_plan(scope, &request.idempotency_key, &input_hash)
        .await?
    {
        return Ok(plan);
    }
    let plan_id = Uuid::new_v4();
    let mut targets = Vec::with_capacity(count);
    for measurement in request.measurements {
        let input = crate::channel_jobs::measurement_input(state, scope, measurement).await?;
        targets.push(ChannelTarget {
            target_id: target_id(plan_id, &input)?,
            input,
        });
    }
    for measurement in request.bound_measurements {
        let input = crate::channel_jobs::bound_measurement_input(state, scope, measurement).await?;
        targets.push(ChannelTarget {
            target_id: target_id(plan_id, &input)?,
            input,
        });
    }
    let plan = StandaloneMeasurementPlan {
        plan_id,
        project_id,
        title: request.title,
        input_hash: input_hash.clone(),
        revision: 1,
        created_at: Utc::now(),
        targets,
    };
    plan.validate(scope)?;
    repository
        .create_measurement_plan(scope, &request.idempotency_key, &input_hash, plan)
        .await
}

/// An AI tool never supplies protocol or timing internals. The original
/// command, not a newly generated timestamp or a changing website default,
/// owns replay identity; the existing repository enforces that identity even
/// across concurrent requests and process restarts.
pub async fn create_agent_plan(
    state: &AppState,
    scope: &TenantScope,
    request: MeasurementPlanCreateRequest,
) -> Result<MeasurementPlanReceipt, AppError> {
    request.validate().map_err(AppError::invalid_request)?;
    let raw = serde_json::to_vec(&(&request.account_id, &request.question, &request.model))
        .map_err(|_| AppError::invalid_request("invalid measurement command"))?;
    let input_hash = sha256_hex(&raw);
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    state
        .project_repository()
        .get(scope, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    if let Some(plan) = state
        .channel_job_repository()
        .replay_measurement_plan(scope, &request.idempotency_key, &input_hash)
        .await?
    {
        return agent_receipt(&plan, request.account_id);
    }
    let account = state
        .channel_service()
        .resolve_available_account(scope, request.account_id)
        .await?;
    let observed = crate::measurement_options::discover(state, scope, request.account_id).await?;
    let model = if let Some(model) = request.model.as_ref() {
        if !observed.models.iter().any(|item| &item.id == model) {
            return Err(AppError::invalid_request(
                "model is not in the observed website menu",
            ));
        }
        model.clone()
    } else {
        observed
            .selected_model
            .filter(|id| observed.models.iter().any(|item| &item.id == id))
            .or_else(|| observed.models.first().map(|item| item.id.clone()))
            .ok_or_else(|| AppError::capability_missing("website model menu is empty"))?
    };
    let plan = create_measurement_plan_with_hash(
        state,
        scope,
        MeasurementPlanRequest {
            idempotency_key: request.idempotency_key,
            title: "Ad-hoc question measurement".into(),
            measurements: vec![MeasurementRequest {
                account_id: request.account_id,
                provider: account.platform,
                model,
                surface: "consumer_web".into(),
                search_mode: "web_search".into(),
                protocol_version: "v1".into(),
                question_set_version: "ad_hoc.v1".into(),
                question: request.question,
                market: "CN".into(),
                language: "zh-CN".into(),
                scheduled_at: Utc::now(),
                sample_ordinal: 0,
            }],
            bound_measurements: vec![],
        },
        input_hash,
    )
    .await?;
    agent_receipt(&plan, request.account_id)
}

fn agent_receipt(
    plan: &StandaloneMeasurementPlan,
    account_id: Uuid,
) -> Result<MeasurementPlanReceipt, AppError> {
    let target = plan
        .targets
        .first()
        .ok_or_else(|| AppError::invalid_request("measurement plan has no target"))?;
    let ChannelTargetInput::Measure {
        account_id: persisted,
        model,
        ..
    } = &target.input
    else {
        return Err(AppError::invalid_request(
            "measurement plan target is not a measurement",
        ));
    };
    if *persisted != account_id || plan.targets.len() != 1 {
        return Err(AppError::conflict(
            "measurement idempotency key belongs to a different command",
        ));
    }
    Ok(MeasurementPlanReceipt {
        plan_id: plan.plan_id,
        target_id: target.target_id,
        account_id: *persisted,
        model: model.clone(),
        state: "accepted".into(),
    })
}

pub async fn agent_options(
    state: &AppState,
    scope: &TenantScope,
    request: MeasurementOptionsRequest,
) -> Result<MeasurementOptionsResult, AppError> {
    if request.account_id.is_nil() {
        return Err(AppError::invalid_request("account ID must be non-zero"));
    }
    let options = crate::measurement_options::discover(state, scope, request.account_id).await?;
    Ok(MeasurementOptionsResult {
        account_id: request.account_id,
        models: options
            .models
            .into_iter()
            .map(|model| MeasurementModelOption {
                id: model.id,
                label: model.label,
            })
            .collect(),
        selected_model: options.selected_model,
    })
}

/// Read the exact scoped plan's current saved evidence. Frozen evaluation and
/// unclassified historical questions remain metadata-only.
pub async fn read_agent_plan(
    state: &AppState,
    scope: &TenantScope,
    request: MeasurementPlanReadRequest,
) -> Result<MeasurementPlanStatus, AppError> {
    if request.plan_id.is_nil() {
        return Err(AppError::invalid_request("plan ID must be non-zero"));
    }
    let plan = state
        .channel_job_repository()
        .get_measurement_plan(scope, request.plan_id)
        .await?
        .ok_or_else(|| AppError::not_found("measurement plan not found"))?;
    let as_of = Utc::now();
    let repository = state.channel_job_repository();
    let mut views = Vec::with_capacity(plan.targets.len());
    for target in &plan.targets {
        views.push(repository.get_target(scope, target.target_id).await?);
    }
    let readable = |input: &ChannelTargetInput| {
        matches!(
            input,
            ChannelTargetInput::Measure { question_binding, question_set_version, .. }
                if question_binding.as_ref().map_or(
                    question_set_version == "ad_hoc.v1",
                    |binding| binding.purpose == QuestionPurpose::Optimization,
                )
        )
    };
    let eligible_views = views
        .iter()
        .filter(|view| readable(&view.target.input))
        .cloned()
        .collect::<Vec<_>>();
    let analyses = if let Some(resolver) = state.observation_evidence_resolver() {
        resolver.resolve(scope, &eligible_views, as_of).await?
    } else {
        vec![]
    };
    let mut targets = Vec::with_capacity(plan.targets.len());
    for view in &views {
        let target = &view.target;
        let attempt = view.attempts.last();
        let outcome = attempt.and_then(|item| item.outcome.as_ref());
        let eligible = readable(&target.input);
        let observation = (eligible && outcome.is_some_and(|item| !item.fixture))
            .then(|| {
                effective_observation(
                    view,
                    &analyses,
                    as_of,
                    crate::citation_insights::accepted_live_search,
                )
            })
            .flatten();
        // A refusal is also useful to explain, but is not a successful observation.
        let refusal = outcome.filter(|item| {
            eligible && !item.fixture && item.status == geo_domain::ChannelOutcomeStatus::Refused
        });
        let answer = observation
            .as_ref()
            .map(|item| &item.raw_answer)
            .or_else(|| refusal.and_then(|item| item.raw_answer.as_ref()));
        let citations = observation
            .as_ref()
            .map(|item| item.citations.as_slice())
            .or_else(|| refusal.map(|item| item.citations.as_slice()))
            .unwrap_or_default();
        let analysis_provenance = observation
            .as_ref()
            .and_then(|item| item.provenance.clone());
        let recommendations = analysis_provenance
            .as_ref()
            .and_then(|source| {
                analyses
                    .iter()
                    .find(|revision| revision.request.revision_id == source.revision_id)
            })
            .and_then(|revision| revision.result.as_ref())
            .and_then(|result| match &result.outcome {
                ObservationAnalysisOutcome::Grounded { audit, .. } => audit
                    .get("recommendations")
                    .filter(|value| !value.is_null())
                    .cloned(),
                _ => None,
            });
        let (surface, conditions) = match &target.input {
            ChannelTargetInput::Measure {
                question,
                provider,
                model,
                surface,
                search_mode,
                protocol_version,
                question_set_version,
                market,
                language,
                sample_ordinal,
                ..
            } => (
                surface.clone(),
                eligible.then(|| MeasurementReportConditions {
                    question: question.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    surface: surface.clone(),
                    search_mode: search_mode.clone(),
                    protocol_version: protocol_version.clone(),
                    question_set_version: question_set_version.clone(),
                    market: market.clone(),
                    language: language.clone(),
                    sample_ordinal: *sample_ordinal,
                }),
            ),
            _ => {
                return Err(AppError::invalid_request(
                    "standalone plan contains a non-measurement target",
                ));
            }
        };
        targets.push(MeasurementTargetStatus {
            target_id: target.target_id,
            state: if outcome.is_some() {
                "completed"
            } else if attempt.is_some() {
                "attempting"
            } else {
                "queued"
            }
            .into(),
            outcome_status: outcome.map(|item| item.status),
            fixture: outcome.map(|item| item.fixture),
            received_at: outcome.and_then(|_| attempt.and_then(|item| item.received_at)),
            surface,
            conditions,
            analysis_provenance,
            recommendations,
            answer: answer.filter(|text| text.len() <= 16 * 1024).cloned(),
            answer_available: answer.is_some(),
            citations: if citations.len() <= 50 && citations.iter().all(|url| url.len() <= 2048) {
                citations.to_vec()
            } else {
                vec![]
            },
            citations_available: !citations.is_empty(),
        });
    }
    Ok(MeasurementPlanStatus {
        plan_id: plan.plan_id,
        targets,
    })
}

pub async fn submit_plan(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<MeasurementPlanRequest>,
) -> Result<Json<StandaloneMeasurementPlan>, ApiError> {
    require_project_writer(&auth).map_err(|e| api_error(e, context.request_id))?;
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    create_measurement_plan(&state, &scope, request)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

pub async fn list_plans(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<PlanPageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<MeasurementPlanPage>, ApiError> {
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let _ = (query.tenant_id, query.project_id);
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(api_error(
            AppError::invalid_request("limit must be 1 to 100"),
            context.request_id,
        ));
    }
    let mut items = state
        .channel_job_repository()
        .list_measurement_plans(&scope, query.after, limit + 1)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_after = if has_more {
        items.last().map(|plan| plan.plan_id)
    } else {
        None
    };
    Ok(Json(MeasurementPlanPage { items, next_after }))
}

pub async fn get_plan(
    State(state): State<AppState>,
    Path((project_id, plan_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<StandaloneMeasurementPlan>, ApiError> {
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    state
        .channel_job_repository()
        .get_measurement_plan(&scope, plan_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?
        .map(Json)
        .ok_or_else(|| {
            api_error(
                AppError::not_found("measurement plan not found"),
                context.request_id,
            )
        })
}
