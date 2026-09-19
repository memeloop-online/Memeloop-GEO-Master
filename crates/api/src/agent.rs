use axum::{
    Json,
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use geo_domain::{
    AppError, AppendMessage, Conversation, ConversationDetail, ConversationEvent, ConversationId,
    CreateConversation, ProjectId, Run, SubmitAcceptance, TenantScope, TurnId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{convert::Infallible, time::Duration};
use tokio_stream::wrappers::BroadcastStream;
use utoipa::ToSchema;

use crate::{
    ApiError, AppState, IDEMPOTENCY_KEY_HEADER, PROJECT_ID_HEADER, RequestContext, api_error,
    context, error_response, require_project_writer,
};

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub(crate) struct AgentScopeQuery {
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub(crate) struct AgentEventsQuery {
    pub project_id: Option<ProjectId>,
    pub after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConversationPage {
    pub items: Vec<Conversation>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentSubmitResponse {
    pub status: &'static str,
    pub conversation_id: ConversationId,
    pub message_id: geo_domain::MessageId,
    pub turn_id: TurnId,
    pub run_id: geo_domain::RunId,
    pub events_url: String,
    pub run_status: geo_domain::RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<geo_domain::AppError>,
}

impl From<SubmitAcceptance> for AgentSubmitResponse {
    fn from(value: SubmitAcceptance) -> Self {
        Self {
            status: "accepted",
            conversation_id: value.conversation.id,
            message_id: value.message.id,
            turn_id: value.turn.id,
            run_id: value.run.id,
            events_url: value.events_url,
            run_status: value.run.status,
            error: value.run.error,
        }
    }
}

fn project_from_headers(headers: &HeaderMap) -> Result<Option<ProjectId>, AppError> {
    let Some(value) = headers.get(PROJECT_ID_HEADER) else {
        return Ok(None);
    };
    value
        .to_str()
        .map_err(|_| AppError::invalid_request("invalid x-project-id header"))?
        .parse()
        .map(Some)
        .map_err(|_| AppError::invalid_request("invalid x-project-id header"))
}

fn scoped_project(
    tenant_scope: &TenantScope,
    headers: &HeaderMap,
    query_project_id: Option<ProjectId>,
    required: bool,
) -> Result<TenantScope, AppError> {
    let project_id = query_project_id
        .or(project_from_headers(headers)?)
        .or(tenant_scope.project_id);
    if required && project_id.is_none() {
        return Err(AppError::invalid_request("project selector is required"));
    }
    Ok(TenantScope::new(
        tenant_scope.operator_id,
        tenant_scope.tenant_id,
        project_id,
    ))
}

/// Narrow the auth-resolved tenant scope for agent routes before the generic
/// idempotency middleware runs.  This keeps the same key independent per
/// project without changing shared middleware semantics for older routes.
pub(crate) async fn project_scope_middleware(mut request: Request, next: Next) -> Response {
    let Some(base_scope) = request.extensions().get::<TenantScope>().cloned() else {
        return error_response(
            AppError::invalid_request("tenant scope is required"),
            request.extensions().get::<RequestContext>().copied(),
        );
    };
    let project_id = match project_from_headers(request.headers()) {
        Ok(project_id) => project_id.or_else(|| {
            request.uri().query().and_then(|query| {
                query.split('&').find_map(|part| {
                    let (key, value) = part.split_once('=')?;
                    (key == "project_id").then(|| value.parse().ok()).flatten()
                })
            })
        }),
        Err(error) => {
            return error_response(error, request.extensions().get::<RequestContext>().copied());
        }
    };
    if let Some(project_id) = project_id {
        request.extensions_mut().insert(TenantScope::new(
            base_scope.operator_id,
            base_scope.tenant_id,
            Some(project_id),
        ));
    }
    next.run(request).await
}

fn request_id(context: RequestContext) -> uuid::Uuid {
    context.request_id
}

fn idempotency_key_hash(headers: &HeaderMap) -> Result<String, AppError> {
    let key = headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .ok_or_else(|| AppError::invalid_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| AppError::invalid_request("invalid Idempotency-Key header"))?;
    Ok(hex::encode(Sha256::digest(key.trim().as_bytes())))
}

fn request_hash(input: &AppendMessage) -> Result<String, AppError> {
    let body = serde_json::to_vec(input).map_err(|error| {
        AppError::new(
            geo_domain::ErrorCode::Internal,
            format!("message request cannot be hashed: {error}"),
        )
    })?;
    Ok(hex::encode(Sha256::digest(body)))
}

#[utoipa::path(
    post,
    path = "/api/v1/agent/conversations",
    security(("sessionCookie" = [])),
    request_body = CreateConversation,
    responses((status = 201, body = Conversation))
)]
pub(crate) async fn create_conversation(
    State(state): State<AppState>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(auth): Extension<context::AuthContext>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Query(query): Query<AgentScopeQuery>,
    Json(input): Json<CreateConversation>,
) -> Result<(StatusCode, Json<Conversation>), ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, request_id(context)))?;
    let scope = scoped_project(&tenant_scope, &headers, query.project_id, true)
        .map_err(|error| api_error(error, request_id(context)))?;
    let conversation = state
        .agent_repository()
        .create_conversation(&scope, Some(auth.user.id), input)
        .await
        .map_err(|error| api_error(error, request_id(context)))?;
    Ok((StatusCode::CREATED, Json(conversation)))
}

#[utoipa::path(
    get,
    path = "/api/v1/agent/conversations",
    security(("sessionCookie" = [])),
    responses((status = 200, body = ConversationPage))
)]
pub(crate) async fn list_conversations(
    State(state): State<AppState>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Query(query): Query<AgentScopeQuery>,
) -> Result<Json<ConversationPage>, ApiError> {
    let scope = scoped_project(&tenant_scope, &headers, query.project_id, true)
        .map_err(|error| api_error(error, request_id(context)))?;
    let items = state
        .agent_repository()
        .list_conversations(&scope)
        .await
        .map_err(|error| api_error(error, request_id(context)))?;
    Ok(Json(ConversationPage {
        items,
        next_cursor: None,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/agent/conversations/{conversation_id}",
    security(("sessionCookie" = [])),
    params(("conversation_id" = ConversationId, Path)),
    responses((status = 200, body = ConversationDetail))
)]
pub(crate) async fn get_conversation(
    State(state): State<AppState>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Query(query): Query<AgentScopeQuery>,
    Path(conversation_id): Path<ConversationId>,
) -> Result<Json<ConversationDetail>, ApiError> {
    let scope = scoped_project(&tenant_scope, &headers, query.project_id, true)
        .map_err(|error| api_error(error, request_id(context)))?;
    let detail = state
        .agent_repository()
        .get_conversation(&scope, conversation_id)
        .await
        .map_err(|error| api_error(error, request_id(context)))?
        .ok_or_else(|| {
            api_error(
                AppError::not_found("conversation not found"),
                request_id(context),
            )
        })?;
    Ok(Json(detail))
}

#[utoipa::path(
    post,
    path = "/api/v1/agent/conversations/{conversation_id}/messages",
    security(("sessionCookie" = [])),
    params(("conversation_id" = ConversationId, Path)),
    request_body = AppendMessage,
    responses((status = 202, body = AgentSubmitResponse))
)]
pub(crate) async fn append_message(
    State(state): State<AppState>,
    Extension(auth): Extension<context::AuthContext>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Query(query): Query<AgentScopeQuery>,
    Path(conversation_id): Path<ConversationId>,
    Json(input): Json<AppendMessage>,
) -> Result<(StatusCode, Json<AgentSubmitResponse>), ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, request_id(context)))?;
    let scope = scoped_project(&auth.scope, &headers, query.project_id, true)
        .map_err(|error| api_error(error, request_id(context)))?;
    let request_hash =
        request_hash(&input).map_err(|error| api_error(error, request_id(context)))?;
    let acceptance = state
        .agent_repository()
        .append_message(
            &scope,
            conversation_id,
            input,
            idempotency_key_hash(&headers)
                .map_err(|error| api_error(error, request_id(context)))?,
            request_hash,
            state.agent_runtime().capability().await,
        )
        .await
        .map_err(|error| api_error(error, request_id(context)))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(AgentSubmitResponse::from(acceptance)),
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/agent/turns/{turn_id}/cancel",
    security(("sessionCookie" = [])),
    params(("turn_id" = TurnId, Path)),
    responses((status = 202, body = Run))
)]
pub(crate) async fn cancel_turn(
    State(state): State<AppState>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(auth): Extension<context::AuthContext>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Query(query): Query<AgentScopeQuery>,
    Path(turn_id): Path<TurnId>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, request_id(context)))?;
    let scope = scoped_project(&tenant_scope, &headers, query.project_id, true)
        .map_err(|error| api_error(error, request_id(context)))?;
    let run = state
        .agent_repository()
        .cancel_turn(&scope, turn_id)
        .await
        .map_err(|error| api_error(error, request_id(context)))?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

fn sse_event(event: ConversationEvent) -> Result<Event, Infallible> {
    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_owned());
    Ok(Event::default()
        .id(event.sequence.to_string())
        .data(payload))
}

#[utoipa::path(
    get,
    path = "/api/v1/agent/conversations/{conversation_id}/events",
    security(("sessionCookie" = [])),
    params(("conversation_id" = ConversationId, Path)),
    responses((status = 200, description = "Conversation event stream", content_type = "text/event-stream"))
)]
pub(crate) async fn conversation_events(
    State(state): State<AppState>,
    Extension(tenant_scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Query(query): Query<AgentEventsQuery>,
    Path(conversation_id): Path<ConversationId>,
) -> Result<Response, ApiError> {
    let scope = scoped_project(&tenant_scope, &headers, query.project_id, true)
        .map_err(|error| api_error(error, request_id(context)))?;
    let after = query
        .after
        .or_else(|| {
            headers
                .get("last-event-id")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(0);
    let repository = state.agent_repository();
    // Subscribe before reading the durable replay so events published between
    // the snapshot and stream construction are still available to the tail.
    let receiver = repository.subscribe_events();
    let replay = repository
        .replay_events(&scope, conversation_id, Some(after))
        .await
        .map_err(|error| api_error(error, request_id(context)))?;
    let replay_last = replay.last().map(|event| event.sequence).unwrap_or(after);
    let scope_for_stream = scope.clone();
    let live = BroadcastStream::new(receiver).filter_map(move |item| {
        let scope = scope_for_stream.clone();
        async move {
            let event = item.ok()?;
            if event.conversation_id != conversation_id
                || !scope.contains(&event.scope())
                || event.sequence <= replay_last
            {
                return None;
            }
            Some(sse_event(event))
        }
    });
    let replay_stream = futures_util::stream::iter(replay.into_iter().map(sse_event));
    let stream = Box::pin(replay_stream.chain(live));
    Ok(Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response())
}
