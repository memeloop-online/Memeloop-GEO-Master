use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use geo_domain::{
    AppError, CurrentKnowledgeRelease, ImportAcceptance, ImportBatchAcceptance, ImportItem,
    InitialSourceKind, KnowledgeAskResult, KnowledgeCapability, KnowledgeSearchRequest,
    KnowledgeSearchResult, MAX_UPLOAD_BYTES, Product, ProjectId, Source, SourceDetail,
    SourceVersion, TenantScope, UploadSession, UploadSessionCommand,
};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthContext, ErrorResponse, IDEMPOTENCY_KEY_HEADER, RequestContext,
    api_error, require_project_writer,
};

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeProjectQuery {
    pub project_id: ProjectId,
    /// Consumed by the authentication selector middleware before this route;
    /// retained here so the strict query decoder accepts the shared URL.
    #[serde(default)]
    #[allow(dead_code)]
    pub tenant_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportBatchRequest {
    pub items: Vec<ImportItem>,
}

async fn knowledge_scope(
    state: &AppState,
    tenant_scope: &TenantScope,
    project_id: ProjectId,
) -> Result<TenantScope, AppError> {
    // Project IDs are always resolved under the authenticated tenant.  A
    // cross-tenant ID deliberately looks absent, rather than leaking its
    // existence through knowledge reads or writes.
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

fn required_idempotency_key(headers: &HeaderMap) -> Result<String, AppError> {
    let value = headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .ok_or_else(|| AppError::invalid_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| AppError::invalid_request("invalid Idempotency-Key header"))?
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(AppError::invalid_request(
            "Idempotency-Key must not be empty",
        ));
    }
    Ok(value)
}

async fn record_acceptance(
    state: &AppState,
    scope: &TenantScope,
    acceptance: &ImportAcceptance,
) -> Result<(), AppError> {
    // PostgreSQL persists the operation and outbox in its import transaction.
    // Memory mode mirrors the operation in the normal operation store and
    // publishes only after the synchronous repository mutation completed.
    if !state.durable_storage()
        && let Some(operation) = &acceptance.operation
    {
        state.operation_store().save(operation.clone()).await?;
    }
    if let Some(release) = &acceptance.release {
        state.publish_event(geo_domain::EventEnvelope::new(
            "knowledge.release.created",
            scope.clone(),
            release.knowledge_release_id,
            release.sequence as u64,
            acceptance
                .operation
                .as_ref()
                .map(|operation| operation.id)
                .unwrap_or(release.knowledge_release_id),
        ));
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/v1/knowledge/capabilities",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query, description = "Project resource selector")),
    responses((status = 200, body = KnowledgeCapability), (status = 404, body = ErrorResponse))
)]
pub(crate) async fn capabilities(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<KnowledgeCapability>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .capabilities(&scope)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    post,
    path = "/api/v1/knowledge/upload-sessions",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query, description = "Project resource selector")),
    request_body = UploadSessionCommand,
    responses((status = 201, body = UploadSession), (status = 400, body = ErrorResponse))
)]
pub(crate) async fn create_upload_session(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(command): Json<UploadSessionCommand>,
) -> Result<(StatusCode, Json<UploadSession>), ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let scope = knowledge_scope(&state, &auth.scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .create_upload_session(&scope, command)
        .await
        .map(|session| (StatusCode::CREATED, Json(session)))
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    put,
    path = "/api/v1/knowledge/upload-sessions/{id}/content",
    security(("sessionCookie" = [])),
    params(
        ("id" = Uuid, Path, description = "Server-generated upload session ID"),
        ("project_id" = ProjectId, Query, description = "Project resource selector")
    ),
    request_body(content = String, content_type = "application/octet-stream"),
    responses((status = 200, body = UploadSession), (status = 400, body = ErrorResponse))
)]
pub(crate) async fn put_upload_content(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    request: Request,
) -> Result<Json<UploadSession>, ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let scope = knowledge_scope(&state, &auth.scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    let content = to_bytes(request.into_body(), MAX_UPLOAD_BYTES as usize + 1)
        .await
        .map_err(|_| {
            api_error(
                AppError::invalid_request("uploaded content exceeds maximum upload size"),
                context.request_id,
            )
        })?;
    state
        .knowledge_repository()
        .put_upload_content(&scope, id, content.to_vec())
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    post,
    path = "/api/v1/knowledge/upload-sessions/{id}/complete",
    security(("sessionCookie" = [])),
    params(
        ("id" = Uuid, Path, description = "Server-generated upload session ID"),
        ("project_id" = ProjectId, Query, description = "Project resource selector")
    ),
    responses((status = 202, body = ImportAcceptance), (status = 400, body = ErrorResponse))
)]
pub(crate) async fn complete_upload(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<KnowledgeProjectQuery>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let key =
        required_idempotency_key(&headers).map_err(|error| api_error(error, context.request_id))?;
    let scope = knowledge_scope(&state, &auth.scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    let acceptance = state
        .knowledge_repository()
        .complete_upload(&scope, id, &key)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    record_acceptance(&state, &scope, &acceptance)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    Ok((StatusCode::ACCEPTED, Json(acceptance)).into_response())
}

#[utoipa::path(
    post,
    path = "/api/v1/knowledge/imports",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query, description = "Project resource selector")),
    request_body = ImportBatchRequest,
    responses((status = 202, body = ImportBatchAcceptance), (status = 400, body = ErrorResponse))
)]
pub(crate) async fn import_batch(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<ImportBatchRequest>,
) -> Result<(StatusCode, Json<ImportBatchAcceptance>), ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let scope = knowledge_scope(&state, &auth.scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    let response = state
        .knowledge_repository()
        .import_batch(&scope, request.items)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    for acceptance in &response.items {
        record_acceptance(&state, &scope, acceptance)
            .await
            .map_err(|error| api_error(error, context.request_id))?;
    }
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Explicit, idempotent conversion of W02 setup inputs into W03 imports.
#[utoipa::path(
    post,
    path = "/api/v1/knowledge/materialize-initial-sources",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query)),
    responses((status = 202, body = ImportBatchAcceptance))
)]
pub(crate) async fn materialize_initial_sources(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<(StatusCode, Json<ImportBatchAcceptance>), ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let project = state
        .project_repository()
        .get(&auth.scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .ok_or_else(|| api_error(AppError::not_found("project not found"), context.request_id))?;
    let scope = TenantScope::new(
        auth.scope.operator_id,
        auth.scope.tenant_id,
        Some(query.project_id),
    );
    let mut items = Vec::with_capacity(project.settings.initial_sources.len());
    for source in &project.settings.initial_sources {
        let encoded = serde_json::to_vec(source).map_err(|error| {
            api_error(
                AppError::new(
                    geo_domain::ErrorCode::Internal,
                    format!("cannot hash initial source: {error}"),
                ),
                context.request_id,
            )
        })?;
        let (kind, text, url, object_id, knowledge_release_id) = match source.kind {
            InitialSourceKind::Text => (
                geo_domain::SourceKind::Text,
                Some(source.value.clone()),
                None,
                None,
                None,
            ),
            InitialSourceKind::Url => (
                geo_domain::SourceKind::Url,
                None,
                Some(source.value.clone()),
                None,
                None,
            ),
            InitialSourceKind::Object => (
                geo_domain::SourceKind::Object,
                None,
                None,
                source.value.parse().ok(),
                None,
            ),
            InitialSourceKind::KnowledgeCollection => (
                geo_domain::SourceKind::KnowledgeCollection,
                None,
                None,
                None,
                source
                    .version_ref
                    .as_deref()
                    .and_then(|value| value.parse().ok()),
            ),
        };
        items.push(ImportItem {
            client_item_id: format!("initial:{}", geo_domain::sha256_hex(&encoded)),
            kind,
            name: source.value.chars().take(120).collect(),
            purpose: match source.visibility {
                geo_domain::InitialSourceVisibility::Public => geo_domain::KnowledgePurpose::Public,
                geo_domain::InitialSourceVisibility::Internal => {
                    geo_domain::KnowledgePurpose::Internal
                }
            },
            text,
            url,
            object_id,
            knowledge_release_id,
        });
    }
    // P01 stores a successful upload as an InitialSourceKind::Object carrying
    // the already-created Source ID.  That is a reference to processed
    // project knowledge, not a request for an object-store adapter; reuse it
    // exactly once without creating another version or release.
    let mut imports = Vec::new();
    for item in items {
        if item.kind == geo_domain::SourceKind::Object
            && let Some(source_id) = item.object_id
            && let Some(source) = state
                .knowledge_repository()
                .get_source(&scope, source_id)
                .await
                .map_err(|error| api_error(error, context.request_id))?
        {
            // This configured source has already been materialized by the
            // upload flow; do not manufacture another import acceptance.
            let _ = source;
        } else {
            imports.push(item);
        }
    }
    let response = if imports.is_empty() {
        ImportBatchAcceptance { items: Vec::new() }
    } else {
        state
            .knowledge_repository()
            .import_batch(&scope, imports)
            .await
            .map_err(|error| api_error(error, context.request_id))?
    };
    for acceptance in &response.items {
        record_acceptance(&state, &scope, acceptance)
            .await
            .map_err(|error| api_error(error, context.request_id))?;
    }
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/knowledge/sources",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query, description = "Project resource selector")),
    responses((status = 200, body = Vec<Source>))
)]
pub(crate) async fn list_sources(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Vec<Source>>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .list_sources(&scope)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    get,
    path = "/api/v1/knowledge/sources/{id}",
    security(("sessionCookie" = [])),
    params(("id" = Uuid, Path), ("project_id" = ProjectId, Query)),
    responses((status = 200, body = SourceDetail), (status = 404, body = ErrorResponse))
)]
pub(crate) async fn get_source(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SourceDetail>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .get_source_detail(&scope, id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .map(Json)
        .ok_or_else(|| api_error(AppError::not_found("source not found"), context.request_id))
}

#[utoipa::path(
    get,
    path = "/api/v1/knowledge/sources/{id}/versions/{version_id}",
    security(("sessionCookie" = [])),
    params(("id" = Uuid, Path), ("version_id" = Uuid, Path), ("project_id" = ProjectId, Query)),
    responses((status = 200, body = SourceVersion), (status = 404, body = ErrorResponse))
)]
pub(crate) async fn get_source_version(
    State(state): State<AppState>,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SourceVersion>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .get_source_version(&scope, id, version_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .map(Json)
        .ok_or_else(|| {
            api_error(
                AppError::not_found("source version not found"),
                context.request_id,
            )
        })
}

#[utoipa::path(
    get,
    path = "/api/v1/knowledge/products",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query)),
    responses((status = 200, body = Vec<Product>))
)]
pub(crate) async fn list_products(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Vec<Product>>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .list_products(&scope)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    get,
    path = "/api/v1/knowledge/facts",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query)),
    responses((status = 200, description = "Scoped facts"))
)]
pub(crate) async fn list_facts(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Vec<geo_domain::Fact>>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .list_facts(&scope)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    get,
    path = "/api/v1/knowledge/releases/current",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query)),
    responses((status = 200, body = CurrentKnowledgeRelease))
)]
pub(crate) async fn current_release(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<CurrentKnowledgeRelease>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .current_release(&scope)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    post,
    path = "/api/v1/knowledge/search",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query)),
    request_body = KnowledgeSearchRequest,
    responses((status = 200, body = KnowledgeSearchResult))
)]
pub(crate) async fn search(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> Result<Json<KnowledgeSearchResult>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .search(&scope, request)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

#[utoipa::path(
    post,
    path = "/api/v1/knowledge/ask",
    security(("sessionCookie" = [])),
    params(("project_id" = ProjectId, Query)),
    request_body = KnowledgeSearchRequest,
    responses((status = 200, body = KnowledgeAskResult))
)]
pub(crate) async fn ask(
    State(state): State<AppState>,
    Query(query): Query<KnowledgeProjectQuery>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> Result<Json<KnowledgeAskResult>, ApiError> {
    let scope = knowledge_scope(&state, &tenant_scope, query.project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    state
        .knowledge_repository()
        .ask(&scope, request)
        .await
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/knowledge/capabilities", get(capabilities))
        .route("/knowledge/upload-sessions", post(create_upload_session))
        .route(
            "/knowledge/upload-sessions/{id}/content",
            put(put_upload_content),
        )
        .route(
            "/knowledge/upload-sessions/{id}/complete",
            post(complete_upload),
        )
        .route("/knowledge/imports", post(import_batch))
        .route(
            "/knowledge/materialize-initial-sources",
            post(materialize_initial_sources),
        )
        .route("/knowledge/sources", get(list_sources))
        .route("/knowledge/sources/{id}", get(get_source))
        .route(
            "/knowledge/sources/{id}/versions/{version_id}",
            get(get_source_version),
        )
        .route("/knowledge/products", get(list_products))
        .route("/knowledge/facts", get(list_facts))
        .route("/knowledge/releases/current", get(current_release))
        .route("/knowledge/search", post(search))
        .route("/knowledge/ask", post(ask))
}
