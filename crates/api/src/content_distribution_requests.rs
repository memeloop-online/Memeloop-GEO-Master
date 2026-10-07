//! A scoped HTTP entry to accept one frozen article for distribution.
//! Acceptance is not a publication attempt or a promise of delivery.
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
};
use geo_domain::{
    AcceptContentDistributionRequest, AppError, ChannelOutcomeStatus, ContentDistributionRequest,
    ErrorCode, ProjectId, TenantScope,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthContext, IDEMPOTENCY_KEY_HEADER, RequestContext, api_error,
    require_project_writer,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SingleArticleDistributionRequest {
    pub content_asset_id: Uuid,
    pub content_revision_id: Uuid,
    pub account_id: Uuid,
    pub placement_slot: String,
    pub format: String,
}

async fn replay_request(
    state: &AppState,
    scope: &TenantScope,
    request: &SingleArticleDistributionRequest,
    key: &str,
) -> Result<Option<ContentDistributionRequest>, AppError> {
    let Some(saved) = state
        .content_distribution_request_repository()
        .get_by_idempotency_key(scope, key)
        .await?
    else {
        return Ok(None);
    };
    if saved.content_asset_id != request.content_asset_id
        || saved.content_revision_id != request.content_revision_id
        || saved.account_id != request.account_id
        || saved.placement_slot != request.placement_slot
        || saved.format != request.format
    {
        return Err(AppError::conflict(
            "idempotency key reused for another request",
        ));
    }
    Ok(Some(saved))
}

/// Shared business entry for HTTP and the AI operating surface. The caller
/// supplies a server-authenticated project scope, not a client scope selector.
pub async fn accept_content_distribution_request(
    state: &AppState,
    scope: &TenantScope,
    request: SingleArticleDistributionRequest,
    idempotency_key: String,
) -> Result<ContentDistributionRequest, AppError> {
    if let Some(saved) = replay_request(state, scope, &request, &idempotency_key).await? {
        return Ok(saved);
    }
    let revision_result = state
        .content_service()
        .repository()
        .get_revision(scope, request.content_asset_id, request.content_revision_id)
        .await
        .and_then(|revision| {
            revision
                .filter(|revision| {
                    revision.asset_id == request.content_asset_id
                        && revision.revision_id == request.content_revision_id
                })
                .ok_or_else(|| AppError::not_found("content revision not found"))
        });
    let revision = match revision_result {
        Ok(revision) => revision,
        Err(error) => {
            return match replay_request(state, scope, &request, &idempotency_key).await? {
                Some(saved) => Ok(saved),
                None => Err(error),
            };
        }
    };
    let account_result = state
        .channel_service()
        .resolve_available_account(scope, request.account_id)
        .await;
    let account = match account_result {
        Ok(account) => account,
        Err(error) => {
            return match replay_request(state, scope, &request, &idempotency_key).await? {
                Some(saved) => Ok(saved),
                None => Err(error),
            };
        }
    };
    let accepted_result = state
        .content_distribution_request_repository()
        .accept(
            scope,
            AcceptContentDistributionRequest {
                revision,
                account,
                placement_slot: request.placement_slot.clone(),
                format: request.format.clone(),
                idempotency_key: idempotency_key.clone(),
            },
        )
        .await;
    let accepted = match accepted_result {
        Ok(accepted) => accepted,
        Err(error) => {
            return match replay_request(state, scope, &request, &idempotency_key).await? {
                Some(saved) => Ok(saved),
                None => Err(error),
            };
        }
    };
    // The acceptance is durable even when a connector or prerequisite is
    // unavailable. Materialization may create one existing-ledger outbox
    // command but never sends externally; a scanner can retry an unlinked row.
    // Return the stored acceptance ID in all cases after the first commit.
    match state
        .content_distribution_request_repository()
        .materialize(scope, accepted.request_id)
        .await
    {
        Ok(linked) => Ok(linked),
        Err(error) => {
            // Only the stable class is recorded: repository error text can
            // contain operational details unsuitable for public logs.
            tracing::warn!(
                error_code = ?error.code,
                "single-article request accepted but not yet materialized"
            );
            Ok(accepted)
        }
    }
}

pub async fn read_content_distribution_request(
    state: &AppState,
    scope: &TenantScope,
    request_id: Uuid,
) -> Result<ContentDistributionRequest, AppError> {
    state
        .content_distribution_request_repository()
        .get(scope, request_id)
        .await
}

/// A narrow ledger-derived read. Outbox existence alone is not a send attempt;
/// only an actual attempt/outcome can be presented as such.
#[derive(Debug, Clone, Serialize)]
pub struct SingleArticlePublication {
    pub request_id: Uuid,
    pub publication_intent_id: Option<Uuid>,
    pub channel_target_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub outcome: Option<ChannelOutcomeStatus>,
    pub fixture: Option<bool>,
    pub public_url: Option<String>,
}

pub async fn read_content_distribution_publication(
    state: &AppState,
    scope: &TenantScope,
    request_id: Uuid,
) -> Result<SingleArticlePublication, AppError> {
    let request = read_content_distribution_request(state, scope, request_id).await?;
    let mut view = SingleArticlePublication {
        request_id,
        publication_intent_id: request.publication_intent_id,
        channel_target_id: None,
        attempt_id: None,
        outcome: None,
        fixture: None,
        public_url: None,
    };
    let Some(intent_id) = request.publication_intent_id else {
        return Ok(view);
    };
    let bundle = state
        .distribution_repository()
        .get_publication_bundle(scope, intent_id)
        .await?;
    bundle.validate_origin()?;
    geo_domain::validate_distribution_request_intent(&request, &bundle.intent, &bundle.variant)?;
    // Materialized channel targets use command_id. The intent's historical
    // channel_target_id can be nil for independent request origins.
    match state
        .channel_job_repository()
        .get_target(scope, bundle.command.command_id)
        .await
    {
        Ok(target) => {
            if target.target.target_id != bundle.command.command_id {
                return Err(AppError::conflict(
                    "publication channel target differs from command",
                ));
            }
            view.channel_target_id = Some(target.target.target_id);
            if let Some(attempt) = target.attempts.last() {
                view.attempt_id = Some(attempt.attempt_id);
                view.outcome = attempt.outcome.as_ref().map(|outcome| outcome.status);
                view.fixture = attempt.outcome.as_ref().map(|outcome| outcome.fixture);
                view.public_url = attempt
                    .outcome
                    .as_ref()
                    .and_then(|outcome| outcome.public_url.clone());
            }
        }
        Err(error) if error.code == ErrorCode::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(view)
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, AppError> {
    let key = headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .ok_or_else(|| AppError::invalid_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| AppError::invalid_request("invalid Idempotency-Key header"))?;
    if key.is_empty() || key.len() > 256 {
        return Err(AppError::invalid_request("invalid Idempotency-Key header"));
    }
    Ok(key.to_owned())
}

pub(crate) async fn accept(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Json(request): Json<SingleArticleDistributionRequest>,
) -> Result<(StatusCode, Json<ContentDistributionRequest>), ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let scope = crate::content::scoped(&state, &auth.scope, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    let key = idempotency_key(&headers).map_err(|error| api_error(error, context.request_id))?;
    let accepted = accept_content_distribution_request(&state, &scope, request, key)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

pub(crate) async fn get_request(
    State(state): State<AppState>,
    Path((project_id, request_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ContentDistributionRequest>, ApiError> {
    let scope = crate::content::scoped(&state, &tenant, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    read_content_distribution_request(&state, &scope, request_id)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

pub(crate) async fn get_publication(
    State(state): State<AppState>,
    Path((project_id, request_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SingleArticlePublication>, ApiError> {
    let scope = crate::content::scoped(&state, &tenant, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    read_content_distribution_publication(&state, &scope, request_id)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}
