//! Axum HTTP boundary for the GEO modular monolith.

mod context;
mod error;
mod idempotency;
mod storage;

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    middleware,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use futures_util::StreamExt;
use geo_domain::{AppError, EventEnvelope, Operation, TenantScope};
use serde::Serialize;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_stream::wrappers::BroadcastStream;
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

pub use context::{
    CORRELATION_ID_HEADER, OPERATOR_ID_HEADER, PROJECT_ID_HEADER, REQUEST_ID_HEADER,
    RequestContext, TENANT_ID_HEADER, dev_scope_middleware, scope_from_headers,
};
pub use error::{ApiError, ErrorResponse, api_error, error_response};
pub use idempotency::{
    IDEMPOTENCY_KEY_HEADER, IdempotencyDecision, IdempotencyStore, IdempotencyToken,
    MAX_IDEMPOTENCY_REQUEST_BYTES, MAX_IDEMPOTENCY_RESPONSE_BYTES, MemoryIdempotencyStore,
    SharedIdempotencyStore, StoredResponse, body_hash, json_command_idempotency_middleware,
};
pub use storage::{EventBus, MemoryOperationStore, OperationStore};

#[derive(Clone)]
pub struct AppState {
    operation_store: Arc<dyn OperationStore>,
    idempotency_store: Arc<dyn IdempotencyStore>,
    events: EventBus,
    ready: Arc<AtomicBool>,
}

impl AppState {
    /// Construct the explicitly non-durable development state.
    pub fn development() -> Self {
        Self {
            operation_store: Arc::new(MemoryOperationStore::default()),
            idempotency_store: Arc::new(MemoryIdempotencyStore::default()),
            events: EventBus::default(),
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_stores(
        operation_store: Arc<dyn OperationStore>,
        idempotency_store: Arc<dyn IdempotencyStore>,
        events: EventBus,
    ) -> Self {
        Self {
            operation_store,
            idempotency_store,
            events,
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn operation_store(&self) -> Arc<dyn OperationStore> {
        Arc::clone(&self.operation_store)
    }

    pub fn idempotency_store(&self) -> Arc<dyn IdempotencyStore> {
        Arc::clone(&self.idempotency_store)
    }

    pub fn events(&self) -> EventBus {
        self.events.clone()
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn publish_event(&self, event: EventEnvelope) -> usize {
        self.events.publish(event)
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub durable_storage: bool,
}

impl HealthResponse {
    fn live() -> Self {
        Self {
            status: "ok",
            service: "geo-api",
            durable_storage: false,
        }
    }

    fn ready(state: &AppState) -> Self {
        Self {
            status: if state.is_ready() { "ok" } else { "not_ready" },
            service: "geo-api",
            durable_storage: false,
        }
    }
}

#[utoipa::path(
    get,
    path = "/health/live",
    responses((status = 200, description = "Process is alive", body = HealthResponse))
)]
async fn health_live() -> impl IntoResponse {
    Json(HealthResponse::live())
}

#[utoipa::path(
    get,
    path = "/health/ready",
    responses(
        (status = 200, description = "Service is ready", body = HealthResponse),
        (status = 503, description = "Service is not ready", body = HealthResponse)
    )
)]
async fn health_ready(State(state): State<AppState>) -> Response {
    let status = if state.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(HealthResponse::ready(&state))).into_response()
}

#[utoipa::path(
    get,
    path = "/api/v1/operations/{id}",
    params(("id" = Uuid, Path, description = "Operation ID")),
    responses(
        (status = 200, description = "Operation", body = Operation),
        (status = 404, description = "Operation does not exist in this tenant scope", body = ErrorResponse)
    )
)]
async fn get_operation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Operation>, ApiError> {
    let operation = state
        .operation_store
        .get(id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .filter(|operation| scope.contains(&operation.scope));
    operation.map(Json).ok_or_else(|| {
        api_error(
            AppError::not_found("operation not found"),
            context.request_id,
        )
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/events",
    responses((status = 200, description = "Server-sent event stream", content_type = "text/event-stream"))
)]
async fn events(
    State(state): State<AppState>,
    Extension(scope): Extension<TenantScope>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.events.subscribe()).filter_map(move |item| {
        let scope = scope.clone();
        async move {
            let event = item.ok()?;
            if !scope.contains(&event.scope()) {
                return None;
            }
            let payload = serde_json::to_string(&event).ok()?;
            Some(Ok(Event::default()
                .id(event.event_id.to_string())
                .event(event.event_type)
                .data(payload)))
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Memeloop GEO API",
        version = "0.1.0",
        description = "W01 modular-monolith HTTP contract; persistence is not implemented yet."
    ),
    paths(health_live, health_ready, get_operation, events),
    components(schemas(
        HealthResponse,
        ErrorResponse,
        Operation,
        geo_domain::OperationStatus,
        geo_domain::TenantScope,
        geo_domain::OperatorId,
        geo_domain::TenantId,
        geo_domain::ProjectId,
        geo_domain::EventEnvelope,
        geo_domain::AppError,
        geo_domain::ErrorCode
    ))
)]
pub struct ApiDoc;

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

/// Build the HTTP router. Scoped routes use the development header adapter;
/// replace that middleware with authenticated server-side identity resolution
/// before production deployment.
pub fn router(state: AppState) -> Router {
    let idempotency_store = state.idempotency_store();
    let scoped = Router::new()
        .route("/operations/{id}", get(get_operation))
        .route("/events", get(events))
        .route_layer(middleware::from_fn_with_state(
            idempotency_store,
            json_command_idempotency_middleware,
        ))
        .route_layer(middleware::from_fn(dev_scope_middleware));

    Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .nest(
            "/api/v1",
            Router::new()
                .route("/openapi.json", get(openapi_json))
                .merge(scoped),
        )
        .layer(middleware::from_fn(context::request_context_middleware))
        .with_state(state)
}
