//! Project-scoped versioned question-set HTTP and shared business services.
//! Identity, split assignment and idempotency are owned by the repository.

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use geo_domain::{
    AppError, CreateQuestionSet, KnowledgePurpose, ProductState, ProjectId, QuestionDraft,
    QuestionSetPage, QuestionSetVersion, QuestionSetVersionPage, QuestionSourceKind,
    ReviseQuestionSet, SourceState, TenantScope, create_question_request_hash,
    revise_question_request_hash,
};
use serde::Deserialize;
use std::collections::HashSet;
use uuid::Uuid;

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPageQuery {
    pub after: Option<Uuid>,
    pub limit: Option<u32>,
    /// The authentication middleware consumes this shared tenant selector.
    pub tenant_id: Option<String>,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionPageQuery {
    pub after_revision: Option<u32>,
    pub limit: Option<u32>,
    pub tenant_id: Option<String>,
    pub project_id: Option<ProjectId>,
}

async fn validate_drafts(
    state: &AppState,
    scope: &TenantScope,
    drafts: &[QuestionDraft],
) -> Result<(), AppError> {
    // Validation is scoped to the authenticated project, never client-provided
    // project/tenant selectors or guesses from source names.
    let products: HashSet<_> = state
        .knowledge_repository()
        .list_products(scope)
        .await?
        .into_iter()
        .filter(|product| product.state == ProductState::Active)
        .map(|product| product.product_id)
        .collect();
    for draft in drafts {
        if draft.product_refs.iter().any(|id| !products.contains(id)) {
            return Err(AppError::invalid_request(
                "question references an unavailable project product",
            ));
        }
        if let Some(id) = draft.source.reference_id {
            let source = state
                .knowledge_repository()
                .get_source(scope, id)
                .await?
                .ok_or_else(|| AppError::invalid_request("question source is unavailable"))?;
            if draft.source.kind == QuestionSourceKind::UserProvided
                || source.state != SourceState::Active
                || source.purpose != KnowledgePurpose::Public
            {
                return Err(AppError::invalid_request(
                    "question source must be a current public project source",
                ));
            }
        }
    }
    Ok(())
}

pub async fn list_sets(
    state: &AppState,
    scope: &TenantScope,
    after: Option<Uuid>,
    limit: u32,
) -> Result<QuestionSetPage, AppError> {
    state
        .question_repository()
        .list_sets(scope, after, limit)
        .await
}

pub async fn create_set(
    state: &AppState,
    scope: &TenantScope,
    command: CreateQuestionSet,
) -> Result<QuestionSetVersion, AppError> {
    let hash = create_question_request_hash(&command)?;
    if let Some(version) = state
        .question_repository()
        .replay(scope, &command.idempotency_key, &hash)
        .await?
    {
        return Ok(version);
    }
    validate_drafts(state, scope, &command.questions).await?;
    state.question_repository().create_set(scope, command).await
}

pub async fn list_versions(
    state: &AppState,
    scope: &TenantScope,
    set_id: Uuid,
    after_revision: Option<u32>,
    limit: u32,
) -> Result<QuestionSetVersionPage, AppError> {
    state
        .question_repository()
        .list_versions(scope, set_id, after_revision, limit)
        .await
}

pub async fn get_version(
    state: &AppState,
    scope: &TenantScope,
    set_id: Uuid,
    version_id: Uuid,
) -> Result<QuestionSetVersion, AppError> {
    state
        .question_repository()
        .get_version(scope, set_id, version_id)
        .await
}

pub async fn revise_set(
    state: &AppState,
    scope: &TenantScope,
    set_id: Uuid,
    command: ReviseQuestionSet,
) -> Result<QuestionSetVersion, AppError> {
    let hash = revise_question_request_hash(set_id, &command)?;
    if let Some(version) = state
        .question_repository()
        .replay(scope, &command.idempotency_key, &hash)
        .await?
    {
        return Ok(version);
    }
    validate_drafts(state, scope, &command.questions).await?;
    state
        .question_repository()
        .revise_set(scope, set_id, command)
        .await
}

pub async fn list_question_sets(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<SetPageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<QuestionSetPage>, ApiError> {
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let _ = (query.tenant_id, query.project_id);
    list_sets(&state, &scope, query.after, query.limit.unwrap_or(50))
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

pub async fn create_question_set(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(command): Json<CreateQuestionSet>,
) -> Result<Json<QuestionSetVersion>, ApiError> {
    require_project_writer(&auth).map_err(|e| api_error(e, context.request_id))?;
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    create_set(&state, &scope, command)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

pub async fn list_question_set_versions(
    State(state): State<AppState>,
    Path((project_id, set_id)): Path<(ProjectId, Uuid)>,
    Query(query): Query<VersionPageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<QuestionSetVersionPage>, ApiError> {
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let _ = (query.tenant_id, query.project_id);
    list_versions(
        &state,
        &scope,
        set_id,
        query.after_revision,
        query.limit.unwrap_or(50),
    )
    .await
    .map(Json)
    .map_err(|e| api_error(e, context.request_id))
}

pub async fn get_question_set_version(
    State(state): State<AppState>,
    Path((project_id, set_id, version_id)): Path<(ProjectId, Uuid, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<QuestionSetVersion>, ApiError> {
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    get_version(&state, &scope, set_id, version_id)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

pub async fn revise_question_set(
    State(state): State<AppState>,
    Path((project_id, set_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(command): Json<ReviseQuestionSet>,
) -> Result<Json<QuestionSetVersion>, ApiError> {
    require_project_writer(&auth).map_err(|e| api_error(e, context.request_id))?;
    let scope = crate::channel_jobs::scope(&state, &tenant, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    revise_set(&state, &scope, set_id, command)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
