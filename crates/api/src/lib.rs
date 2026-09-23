//! Axum HTTP boundary for the GEO modular monolith.

mod agent;
mod agent_runtime;
mod context;
mod error;
mod idempotency;
mod knowledge;
mod run_executor;
mod storage;

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{IF_MATCH, SET_COOKIE},
    },
    middleware,
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use geo_domain::{
    AgentRepository, AgentRuntime, AppError, DEFAULT_SESSION_TTL_SECS, DistributionScope,
    DocumentScope, EventEnvelope, InitialSource, KnowledgeRepository, Membership,
    MemoryAgentRepository, MemoryAuthRepository, MemoryKnowledgeRepository, MissingAgentRuntime,
    Operation, Operator, Project, ProjectCreate, ProjectId, ProjectOverview, ProjectPage,
    ProjectPatch, ProjectRepository, ProjectSettings, ProjectStartAcceptance, ProjectStartCommand,
    ReportSchedule, ResourceMode, Role, TenantId, TenantScope, User, hash_idempotency_key,
    settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, PgAuthRepository, PgIdempotencyStore, PgKnowledgeRepository, PgProjectRepository,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

pub use agent_runtime::{EmbeddedAgentRuntime, RepositoryHostOps};
pub use context::{
    AuthContext, AuthMiddlewareState, CORRELATION_ID_HEADER, CSRF_HEADER, DEV_SESSION_COOKIE_NAME,
    OPERATOR_ID_HEADER, OriginConfig, PROJECT_ID_HEADER, REQUEST_ID_HEADER, RequestContext,
    SESSION_COOKIE_NAME, SessionCookieConfig, SharedAuthRepository, TENANT_ID_HEADER,
    TENANT_SELECTOR_HEADER, auth_scope_from_extension, auth_scope_from_extensions,
    auth_scope_from_middleware_state, auth_scope_from_request, auth_scope_middleware,
    auth_scope_with_repository, auth_scope_with_repository_and_cookie,
    csrf_origin_from_config_extension, csrf_origin_from_middleware_state, csrf_origin_from_request,
    csrf_origin_middleware, csrf_origin_with_config, csrf_origin_with_scheme, dev_scope_middleware,
    host_from_headers, no_store_middleware, request_host, resolve_auth_context,
    resolve_auth_context_with_cookie, scope_from_headers, session_auth_from_extension,
    session_auth_from_extensions, session_auth_from_middleware_state, session_auth_from_request,
    session_auth_middleware, session_auth_with_repository_and_cookie, validate_origin,
    validate_origin_headers, validate_origin_headers_with_config, validate_origin_with_config,
    validate_origin_with_scheme,
};
pub use error::{ApiError, ErrorResponse, api_error, error_response};
pub use idempotency::{
    IDEMPOTENCY_KEY_HEADER, IdempotencyDecision, IdempotencyStore, IdempotencyToken,
    MAX_IDEMPOTENCY_REQUEST_BYTES, MAX_IDEMPOTENCY_RESPONSE_BYTES, MemoryIdempotencyStore,
    SharedIdempotencyStore, StoredResponse, body_hash, json_command_idempotency_middleware,
};
pub use storage::{EventBus, MemoryOperationStore, OperationStore, PgOperationStore};

#[derive(Clone)]
pub struct AppState {
    operation_store: Arc<dyn OperationStore>,
    idempotency_store: Arc<dyn IdempotencyStore>,
    agent_repository: Arc<dyn AgentRepository>,
    agent_runtime: Arc<dyn AgentRuntime>,
    auth_repository: SharedAuthRepository,
    project_repository: Arc<dyn ProjectRepository>,
    knowledge_repository: Arc<dyn KnowledgeRepository>,
    events: EventBus,
    ready: Arc<AtomicBool>,
    durable_storage: bool,
    origin_scheme: Arc<str>,
    origin_config: OriginConfig,
}

impl AppState {
    /// Construct the explicitly non-durable development state.
    pub fn development() -> Self {
        // Tests and in-process callers must opt into a password explicitly;
        // the default state gets an unpredictable bootstrap secret.
        Self::development_with_password(&Uuid::new_v4().to_string())
    }

    pub fn development_with_password(password: &str) -> Self {
        Self {
            operation_store: Arc::new(MemoryOperationStore::default()),
            idempotency_store: Arc::new(MemoryIdempotencyStore::default()),
            agent_repository: Arc::new(MemoryAgentRepository::default()),
            agent_runtime: Arc::new(MissingAgentRuntime),
            auth_repository: Arc::new(MemoryAuthRepository::development_with_password(password)),
            project_repository: Arc::new(geo_domain::MemoryProjectRepository::default()),
            knowledge_repository: Arc::new(MemoryKnowledgeRepository::default()),
            events: EventBus::default(),
            ready: Arc::new(AtomicBool::new(false)),
            durable_storage: false,
            origin_scheme: Arc::from("http"),
            origin_config: OriginConfig::local_http(),
        }
    }

    pub fn with_stores(
        operation_store: Arc<dyn OperationStore>,
        idempotency_store: Arc<dyn IdempotencyStore>,
        events: EventBus,
    ) -> Self {
        Self::with_stores_and_auth_and_projects(
            operation_store,
            idempotency_store,
            Arc::new(MemoryAuthRepository::development_with_password(
                &Uuid::new_v4().to_string(),
            )),
            Arc::new(geo_domain::MemoryProjectRepository::default()),
            events,
            false,
        )
    }

    pub fn with_stores_and_auth(
        operation_store: Arc<dyn OperationStore>,
        idempotency_store: Arc<dyn IdempotencyStore>,
        auth_repository: SharedAuthRepository,
        events: EventBus,
        durable_storage: bool,
    ) -> Self {
        Self::with_stores_and_auth_and_projects(
            operation_store,
            idempotency_store,
            auth_repository,
            Arc::new(geo_domain::MemoryProjectRepository::default()),
            events,
            durable_storage,
        )
    }

    pub fn with_stores_and_auth_and_projects(
        operation_store: Arc<dyn OperationStore>,
        idempotency_store: Arc<dyn IdempotencyStore>,
        auth_repository: SharedAuthRepository,
        project_repository: Arc<dyn ProjectRepository>,
        events: EventBus,
        durable_storage: bool,
    ) -> Self {
        Self::with_stores_and_auth_and_projects_and_knowledge(
            operation_store,
            idempotency_store,
            auth_repository,
            project_repository,
            Arc::new(MemoryKnowledgeRepository::default()),
            events,
            durable_storage,
        )
    }

    pub fn with_stores_and_auth_and_projects_and_knowledge(
        operation_store: Arc<dyn OperationStore>,
        idempotency_store: Arc<dyn IdempotencyStore>,
        auth_repository: SharedAuthRepository,
        project_repository: Arc<dyn ProjectRepository>,
        knowledge_repository: Arc<dyn KnowledgeRepository>,
        events: EventBus,
        durable_storage: bool,
    ) -> Self {
        Self {
            operation_store,
            idempotency_store,
            agent_repository: Arc::new(MemoryAgentRepository::default()),
            agent_runtime: Arc::new(MissingAgentRuntime),
            auth_repository,
            project_repository,
            knowledge_repository,
            events,
            ready: Arc::new(AtomicBool::new(false)),
            durable_storage,
            origin_scheme: Arc::from("http"),
            origin_config: OriginConfig::local_http(),
        }
    }

    pub fn with_origin_scheme(mut self, scheme: impl Into<Arc<str>>) -> Self {
        self.origin_scheme = scheme.into();
        self.origin_config = OriginConfig::new(self.origin_scheme.clone());
        self
    }

    pub fn with_allowed_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.origin_config = OriginConfig::from_allowed_origins(origins);
        self
    }

    pub fn with_origin_config(mut self, origin_config: OriginConfig) -> Self {
        self.origin_scheme = origin_config.scheme.clone();
        self.origin_config = origin_config;
        self
    }

    pub fn from_database(database: &Database) -> Self {
        Self::with_stores_and_auth_and_projects_and_knowledge(
            Arc::new(PgOperationStore::from_database(database)),
            Arc::new(PgIdempotencyStore::from_database(database)),
            Arc::new(PgAuthRepository::from_database(database)),
            Arc::new(PgProjectRepository::from_database(database)),
            Arc::new(PgKnowledgeRepository::from_database(database)),
            EventBus::default(),
            true,
        )
        .with_agent_repository(Arc::new(
            geo_persistence::PgAgentRepository::from_database(database),
        ))
    }

    pub fn operation_store(&self) -> Arc<dyn OperationStore> {
        Arc::clone(&self.operation_store)
    }

    pub fn agent_repository(&self) -> Arc<dyn AgentRepository> {
        Arc::clone(&self.agent_repository)
    }

    pub fn agent_runtime(&self) -> Arc<dyn AgentRuntime> {
        Arc::clone(&self.agent_runtime)
    }

    pub fn with_agent_repository(mut self, repository: Arc<dyn AgentRepository>) -> Self {
        self.agent_repository = repository;
        self
    }

    pub fn with_agent_runtime(mut self, runtime: Arc<dyn AgentRuntime>) -> Self {
        self.agent_runtime = runtime;
        self
    }

    pub fn idempotency_store(&self) -> Arc<dyn IdempotencyStore> {
        Arc::clone(&self.idempotency_store)
    }

    pub fn auth_repository(&self) -> SharedAuthRepository {
        Arc::clone(&self.auth_repository)
    }

    pub fn project_repository(&self) -> Arc<dyn ProjectRepository> {
        Arc::clone(&self.project_repository)
    }

    pub fn knowledge_repository(&self) -> Arc<dyn KnowledgeRepository> {
        Arc::clone(&self.knowledge_repository)
    }

    pub fn durable_storage(&self) -> bool {
        self.durable_storage
    }

    pub fn origin_scheme(&self) -> &str {
        &self.origin_scheme
    }

    pub fn origin_config(&self) -> &OriginConfig {
        &self.origin_config
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub login_name: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuthConfigResponse {
    pub mode: &'static str,
    pub session_cookie_name: &'static str,
    pub csrf_header: &'static str,
    pub tenant_selector_query: &'static str,
    pub tenant_selector_header: &'static str,
    pub same_origin_required: bool,
    pub session_ttl_seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub development_login_name: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MembershipView {
    pub tenant_id: TenantId,
    pub tenant_slug: String,
    pub tenant_display_name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserView {
    pub id: geo_domain::UserId,
    pub login_name: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperatorView {
    pub id: geo_domain::OperatorId,
    pub slug: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuthSessionResponse {
    pub user: UserView,
    pub operator: OperatorView,
    pub memberships: Vec<MembershipView>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub csrf_token: String,
}

#[derive(Debug, Deserialize, ToSchema)]
struct ProjectListQuery {
    /// Maximum number of projects to return. The API accepts 1 through 100;
    /// omitted values use the conservative default of 50.
    limit: Option<String>,
    /// Opaque cursor returned by the previous page.
    cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ProjectSettingsPatchRequest {
    pub brand_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub product_name: Option<Option<String>>,
    pub market: Option<String>,
    pub language: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub target_audience: Option<Option<String>>,
    pub competitors: Option<Vec<String>>,
    pub initial_sources: Option<Vec<InitialSource>>,
    pub resource_mode: Option<ResourceMode>,
    pub budget_currency: Option<String>,
    pub monthly_budget_minor: Option<i64>,
    pub monitoring_reserve_percent: Option<u8>,
    pub report_timezone: Option<String>,
    pub report_schedule: Option<ReportSchedule>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub objective: Option<Option<String>>,
    pub document_scope: Option<DocumentScope>,
    pub distribution_scope: Option<DistributionScope>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ProjectPatchRequest {
    /// The revision echoed by clients. If present it must match If-Match.
    pub revision: Option<i64>,
    pub slug: Option<String>,
    pub display_name: Option<String>,
    pub brand_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub product_name: Option<Option<String>>,
    pub market: Option<String>,
    pub language: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub target_audience: Option<Option<String>>,
    pub competitors: Option<Vec<String>>,
    pub initial_sources: Option<Vec<InitialSource>>,
    pub resource_mode: Option<ResourceMode>,
    pub budget_currency: Option<String>,
    pub monthly_budget_minor: Option<i64>,
    pub monitoring_reserve_percent: Option<u8>,
    pub report_timezone: Option<String>,
    pub report_schedule: Option<ReportSchedule>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub objective: Option<Option<String>>,
    pub document_scope: Option<DocumentScope>,
    pub distribution_scope: Option<DistributionScope>,
    pub settings: Option<ProjectSettingsPatchRequest>,
}

impl ProjectPatchRequest {
    fn into_domain(self) -> (Option<i64>, ProjectPatch) {
        let settings = self.settings.unwrap_or_default();
        let product_name = self.product_name.or(settings.product_name);
        let target_audience = self.target_audience.or(settings.target_audience);
        let objective = self.objective.or(settings.objective);
        let patch = ProjectPatch {
            slug: self.slug,
            display_name: self.display_name,
            brand_name: self.brand_name.or(settings.brand_name),
            product_name: product_name.clone().flatten(),
            clear_product_name: matches!(product_name, Some(None)),
            market: self.market.or(settings.market),
            language: self.language.or(settings.language),
            target_audience: target_audience.clone().flatten(),
            clear_target_audience: matches!(target_audience, Some(None)),
            competitors: self.competitors.or(settings.competitors),
            initial_sources: self.initial_sources.or(settings.initial_sources),
            resource_mode: self.resource_mode.or(settings.resource_mode),
            budget_currency: self.budget_currency.or(settings.budget_currency),
            monthly_budget_minor: self.monthly_budget_minor.or(settings.monthly_budget_minor),
            monitoring_reserve_percent: self
                .monitoring_reserve_percent
                .or(settings.monitoring_reserve_percent),
            report_timezone: self.report_timezone.or(settings.report_timezone),
            report_schedule: self.report_schedule.or(settings.report_schedule),
            objective: objective.clone().flatten(),
            clear_objective: matches!(objective, Some(None)),
            document_scope: self.document_scope.or(settings.document_scope),
            distribution_scope: self.distribution_scope.or(settings.distribution_scope),
            // Lifecycle transitions are commands (for example /start), not
            // ordinary configuration patches.
            status: None,
        };
        (self.revision, patch)
    }
}

/// Serde represents both a missing `Option<Option<T>>` field and an explicit
/// JSON null as `None` by default. PATCH needs three states, so wrapping the
/// parsed inner option lets callers distinguish missing from present-null.
fn deserialize_present_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EstimateState {
    Unknown,
    Estimated,
    Frozen,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CountEstimate {
    pub state: EstimateState,
    pub value: Option<u64>,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub basis_refs: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MoneyEstimate {
    pub state: EstimateState,
    pub value_minor: Option<i64>,
    pub min_minor: Option<i64>,
    pub max_minor: Option<i64>,
    pub basis_refs: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EstimateCoverage {
    pub documents: CountEstimate,
    pub document_platform_targets: CountEstimate,
    pub measurement_samples: CountEstimate,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EstimateCosts {
    pub phase_one_documents: MoneyEstimate,
    pub phase_two_distribution: MoneyEstimate,
    pub measurement: MoneyEstimate,
    pub total: MoneyEstimate,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EstimateBudget {
    pub currency: String,
    pub monthly_limit_minor: i64,
    pub measurement_reserve_minor: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EstimateBlocker {
    pub code: String,
    pub scope: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectEstimateResponse {
    pub settings_hash: String,
    pub estimator_version: String,
    pub pricing_snapshot_id: Option<String>,
    pub capability_snapshot_id: Option<String>,
    pub coverage: EstimateCoverage,
    pub costs: EstimateCosts,
    pub budget: EstimateBudget,
    pub blockers: Vec<EstimateBlocker>,
    pub assumptions: Vec<String>,
}

fn user_view(user: &User) -> UserView {
    UserView {
        id: user.id,
        login_name: user.email.clone(),
        display_name: user.display_name.clone(),
    }
}

fn operator_view(operator: &Operator) -> OperatorView {
    OperatorView {
        id: operator.id,
        slug: operator.slug.clone(),
        display_name: operator.display_name.clone(),
    }
}

fn membership_view(membership: Membership) -> MembershipView {
    MembershipView {
        tenant_id: membership.tenant_id,
        tenant_slug: membership.tenant_slug,
        tenant_display_name: membership.tenant_display_name,
        role: match membership.role {
            geo_domain::Role::CustomerAdmin => "tenant_admin",
            geo_domain::Role::CustomerMember => "member",
            geo_domain::Role::CustomerReadOnly => "viewer",
            geo_domain::Role::Operator => "operator_agent",
            geo_domain::Role::ResourceAdmin => "resource_admin",
            geo_domain::Role::OemAdmin => "operator_admin",
        }
        .to_owned(),
    }
}

impl HealthResponse {
    fn live(state: &AppState) -> Self {
        Self {
            status: "ok",
            service: "geo-api",
            durable_storage: state.durable_storage(),
        }
    }

    fn ready(state: &AppState) -> Self {
        Self {
            status: if state.is_ready() { "ok" } else { "not_ready" },
            service: "geo-api",
            durable_storage: state.durable_storage(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/health/live",
    responses((status = 200, description = "Process is alive", body = HealthResponse))
)]
async fn health_live(State(state): State<AppState>) -> impl IntoResponse {
    // The live endpoint is intentionally independent from database readiness,
    // but still reports whether this process was assembled with durable stores.
    Json(HealthResponse::live(&state))
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
    path = "/api/v1/auth/config",
    responses((status = 200, description = "Authentication configuration", body = AuthConfigResponse))
)]
async fn auth_config(State(state): State<AppState>) -> Response {
    let mut response = Json(AuthConfigResponse {
        mode: if state.durable_storage() {
            "persistent"
        } else {
            "development"
        },
        session_cookie_name: if state.durable_storage() {
            SESSION_COOKIE_NAME
        } else {
            DEV_SESSION_COOKIE_NAME
        },
        csrf_header: CSRF_HEADER,
        tenant_selector_query: "tenant_id",
        tenant_selector_header: TENANT_SELECTOR_HEADER,
        same_origin_required: true,
        session_ttl_seconds: DEFAULT_SESSION_TTL_SECS,
        development_login_name: (!state.durable_storage())
            .then_some(geo_domain::DEVELOPMENT_USER_EMAIL),
    })
    .into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated session", body = AuthSessionResponse),
        (status = 401, description = "Invalid credentials", body = ErrorResponse)
    )
)]
async fn auth_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(request_context): Extension<RequestContext>,
    Json(input): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    validate_origin_headers_with_config(&headers, state.origin_config())
        .map_err(|error| api_error(error, request_context.request_id))?;
    let host = host_from_headers(&headers)
        .map_err(|error| api_error(error, request_context.request_id))?;
    let operator = state
        .auth_repository()
        .operator_for_host(&host)
        .await
        .map_err(|error| api_error(error, request_context.request_id))?
        .ok_or_else(|| {
            api_error(
                AppError::unauthorized("request host is not configured"),
                request_context.request_id,
            )
        })?;
    let identity = state
        .auth_repository()
        .authenticate(operator.id, &input.login_name, &input.password)
        .await
        .map_err(|error| api_error(error, request_context.request_id))?
        .ok_or_else(|| {
            api_error(
                AppError::unauthorized("invalid email or password"),
                request_context.request_id,
            )
        })?;
    let credentials = state
        .auth_repository()
        .create_session(
            identity.operator.id,
            identity.user.id,
            chrono::Duration::seconds(DEFAULT_SESSION_TTL_SECS),
        )
        .await
        .map_err(|error| api_error(error, request_context.request_id))?;
    let cookie_name = if state.durable_storage() {
        SESSION_COOKIE_NAME
    } else {
        DEV_SESSION_COOKIE_NAME
    };
    let secure = state.durable_storage();
    let cookie = format!(
        "{cookie_name}={}; Path=/; Max-Age={DEFAULT_SESSION_TTL_SECS}; HttpOnly; SameSite=Lax{}",
        credentials.token,
        if secure { "; Secure" } else { "" }
    );
    let mut response = Json(AuthSessionResponse {
        user: user_view(&identity.user),
        operator: operator_view(&identity.operator),
        memberships: identity
            .memberships
            .into_iter()
            .map(membership_view)
            .collect(),
        expires_at: credentials.session.expires_at,
        csrf_token: credentials.session.csrf_token().to_owned(),
    })
    .into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| {
            api_error(
                AppError::new(geo_domain::ErrorCode::Internal, "invalid session cookie"),
                request_context.request_id,
            )
        })?,
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/api/v1/auth/session",
    security(("sessionCookie" = [])),
    responses((status = 200, description = "Current authenticated session", body = AuthSessionResponse))
)]
async fn auth_session(Extension(auth): Extension<AuthContext>) -> Response {
    let mut response = Json(AuthSessionResponse {
        user: user_view(&auth.user),
        operator: operator_view(&auth.operator),
        memberships: auth.memberships.into_iter().map(membership_view).collect(),
        expires_at: auth.session.expires_at,
        csrf_token: auth.session.csrf_token().to_owned(),
    })
    .into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

#[utoipa::path(
    delete,
    path = "/api/v1/auth/session",
    security(("sessionCookie" = [])),
    responses((status = 204, description = "Session revoked"))
)]
async fn auth_logout(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(request_context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    state
        .auth_repository()
        .revoke_session(auth.operator.id, auth.session.id)
        .await
        .map_err(|error| api_error(error, request_context.request_id))?;
    let clear_dev =
        format!("{DEV_SESSION_COOKIE_NAME}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax");
    let clear_prod =
        format!("{SESSION_COOKIE_NAME}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax; Secure");
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_str(&clear_dev).map_err(|_| {
            api_error(
                AppError::new(geo_domain::ErrorCode::Internal, "invalid session cookie"),
                request_context.request_id,
            )
        })?,
    );
    response.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_str(&clear_prod).map_err(|_| {
            api_error(
                AppError::new(geo_domain::ErrorCode::Internal, "invalid session cookie"),
                request_context.request_id,
            )
        })?,
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

fn selected_membership(auth: &AuthContext) -> Option<&Membership> {
    auth.memberships
        .iter()
        .find(|membership| membership.tenant_id == auth.scope.tenant_id && membership.active)
}

fn require_project_writer(auth: &AuthContext) -> Result<(), AppError> {
    match selected_membership(auth).map(|membership| membership.role) {
        Some(Role::CustomerAdmin | Role::CustomerMember) => Ok(()),
        Some(Role::CustomerReadOnly) => Err(AppError::forbidden(
            "viewer membership cannot change projects",
        )),
        Some(_) => Err(AppError::forbidden(
            "membership role cannot change customer projects",
        )),
        None => Err(AppError::forbidden(
            "user is not a member of the selected tenant",
        )),
    }
}

fn parse_project_limit(value: Option<&str>) -> Result<usize, AppError> {
    let limit = value
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| AppError::invalid_request("limit must be an integer"))
        })
        .transpose()?
        .unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request("limit must be between 1 and 100"));
    }
    Ok(limit)
}

fn parse_if_match(headers: &HeaderMap) -> Result<i64, AppError> {
    let value = headers
        .get(IF_MATCH)
        .ok_or_else(|| AppError::invalid_request("If-Match header is required"))?
        .to_str()
        .map_err(|_| AppError::invalid_request("invalid If-Match header"))?
        .trim();
    let value = value.strip_prefix("W/").unwrap_or(value).trim();
    let value = value.trim_matches('"');
    if value.is_empty() || value == "*" {
        return Err(AppError::invalid_request(
            "If-Match must contain a numeric project revision",
        ));
    }
    value
        .parse::<i64>()
        .map_err(|_| AppError::invalid_request("If-Match must contain a numeric project revision"))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects",
    security(("sessionCookie" = [])),
    params(
        ("limit" = Option<String>, Query, description = "Page size from 1 to 100"),
        ("cursor" = Option<String>, Query, description = "Opaque cursor from the previous page")
    ),
    responses(
        (status = 200, description = "Tenant-scoped project page", body = ProjectPage),
        (status = 400, description = "Invalid pagination parameters", body = ErrorResponse)
    )
)]
async fn list_projects(
    State(state): State<AppState>,
    Query(query): Query<ProjectListQuery>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ProjectPage>, ApiError> {
    let limit = parse_project_limit(query.limit.as_deref())
        .map_err(|error| api_error(error, context.request_id))?;
    let page = state
        .project_repository
        .list_page(&scope, limit, query.cursor.as_deref())
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    Ok(Json(page))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects",
    security(("sessionCookie" = [])),
    request_body = ProjectCreate,
    responses(
        (status = 201, description = "Project created", body = Project),
        (status = 403, description = "Membership cannot create projects", body = ErrorResponse),
        (status = 409, description = "Project slug already exists", body = ErrorResponse)
    )
)]
async fn create_project(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<ProjectCreate>,
) -> Result<Response, ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let project = state
        .project_repository
        .create(&auth.scope, input)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    Ok((StatusCode::CREATED, Json(project)).into_response())
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}",
    security(("sessionCookie" = [])),
    params(("id" = ProjectId, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Project", body = Project),
        (status = 404, description = "Project does not exist in this tenant", body = ErrorResponse)
    )
)]
async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Project>, ApiError> {
    state
        .project_repository
        .get(&scope, id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .map(Json)
        .ok_or_else(|| api_error(AppError::not_found("project not found"), context.request_id))
}

#[utoipa::path(
    patch,
    path = "/api/v1/projects/{id}",
    security(("sessionCookie" = [])),
    params(("id" = ProjectId, Path, description = "Project ID")),
    request_body = ProjectPatchRequest,
    responses(
        (status = 200, description = "Updated project", body = Project),
        (status = 400, description = "Missing or invalid If-Match", body = ErrorResponse),
        (status = 403, description = "Membership cannot update projects", body = ErrorResponse),
        (status = 409, description = "Project revision conflict", body = ErrorResponse)
    )
)]
async fn patch_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<ProjectPatchRequest>,
) -> Result<Json<Project>, ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let expected_revision =
        parse_if_match(&headers).map_err(|error| api_error(error, context.request_id))?;
    let (body_revision, patch) = input.into_domain();
    if let Some(body_revision) = body_revision
        && body_revision != expected_revision
    {
        return Err(api_error(
            AppError::conflict("request revision does not match If-Match"),
            context.request_id,
        ));
    }
    if patch.status.is_some() {
        return Err(api_error(
            AppError::invalid_request("project status changes must use the start operation"),
            context.request_id,
        ));
    }
    let updated = state
        .project_repository
        .update(&auth.scope, id, expected_revision, patch)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    Ok(Json(updated.project))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/overview",
    security(("sessionCookie" = [])),
    params(("id" = ProjectId, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Project overview", body = ProjectOverview),
        (status = 404, description = "Project does not exist in this tenant", body = ErrorResponse)
    )
)]
async fn get_project_overview(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ProjectOverview>, ApiError> {
    let project = state
        .project_repository
        .get(&scope, id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .ok_or_else(|| api_error(AppError::not_found("project not found"), context.request_id))?;
    let start = state
        .project_repository
        .get_start(&scope, id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    let mut overview = ProjectOverview::from_start(project, start);
    let knowledge_scope = TenantScope::new(scope.operator_id, scope.tenant_id, Some(id));
    let knowledge = state
        .knowledge_repository()
        .overview(&knowledge_scope)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    overview.knowledge.source_count = knowledge.source_count;
    overview.knowledge.fact_count = knowledge.fact_count;
    overview.knowledge.status = if knowledge.current_release_id.is_some() {
        geo_domain::OverviewKnowledgeStatus::Ready
    } else if knowledge.importing_count > 0 {
        geo_domain::OverviewKnowledgeStatus::Importing
    } else {
        geo_domain::OverviewKnowledgeStatus::Empty
    };
    if knowledge.current_release_id.is_some() {
        overview.cycle.awaiting_knowledge = false;
    }
    Ok(Json(overview))
}

fn estimate_project(
    input: ProjectCreate,
    _scope: &TenantScope,
) -> Result<ProjectEstimateResponse, AppError> {
    // Estimation has no writes and intentionally does not pretend that source
    // locators determine documents, platform targets, samples, or pricing.
    let settings = input.settings.validate_draft()?;
    let reserve = ((settings.monthly_budget_minor as i128)
        .saturating_mul(settings.monitoring_reserve_percent as i128)
        / 100) as i64;
    let unknown_count = |reason: &str| CountEstimate {
        state: EstimateState::Unknown,
        value: None,
        min: None,
        max: None,
        basis_refs: Vec::new(),
        reason: Some(reason.to_owned()),
    };
    let unknown_money = |reason: &str| MoneyEstimate {
        state: EstimateState::Unknown,
        value_minor: None,
        min_minor: None,
        max_minor: None,
        basis_refs: Vec::new(),
        reason: Some(reason.to_owned()),
    };
    Ok(ProjectEstimateResponse {
        settings_hash: settings_hash(&settings)?,
        estimator_version: "w02-prerequisites-unknown-v1".to_owned(),
        pricing_snapshot_id: None,
        capability_snapshot_id: None,
        coverage: EstimateCoverage {
            documents: unknown_count(
                "KnowledgeRelease is not available; document manifest is not frozen.",
            ),
            document_platform_targets: unknown_count(
                "CapabilitySnapshot is not available; distribution targets are not expanded.",
            ),
            measurement_samples: unknown_count(
                "MeasurementProtocol is not available; measurement samples are not planned.",
            ),
        },
        costs: EstimateCosts {
            phase_one_documents: unknown_money(
                "PricingSnapshot and frozen document denominator are unavailable.",
            ),
            phase_two_distribution: unknown_money(
                "PricingSnapshot, CapabilitySnapshot, and distribution denominator are unavailable.",
            ),
            measurement: unknown_money("PricingSnapshot and MeasurementProtocol are unavailable."),
            total: unknown_money("Component costs are not known."),
        },
        budget: EstimateBudget {
            currency: settings.budget_currency,
            monthly_limit_minor: settings.monthly_budget_minor,
            measurement_reserve_minor: reserve,
        },
        blockers: vec![
            EstimateBlocker {
                code: "knowledge_release_unavailable".to_owned(),
                scope: "documents".to_owned(),
                reason: "W02 has not resolved immutable knowledge inputs.".to_owned(),
            },
            EstimateBlocker {
                code: "capability_snapshot_unavailable".to_owned(),
                scope: "document_platform_targets".to_owned(),
                reason: "No eligible platform/account capability snapshot is frozen.".to_owned(),
            },
            EstimateBlocker {
                code: "measurement_protocol_unavailable".to_owned(),
                scope: "measurement_samples".to_owned(),
                reason: "No measurement protocol or sample plan is frozen.".to_owned(),
            },
            EstimateBlocker {
                code: "pricing_snapshot_unavailable".to_owned(),
                scope: "costs".to_owned(),
                reason: "No applicable price list snapshot is frozen.".to_owned(),
            },
        ],
        assumptions: vec![
            "Estimate is side-effect free: it creates no project, reservation, or task.".to_owned(),
            "Zero budget permits later free knowledge work but must block paid actions.".to_owned(),
        ],
    })
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/estimate",
    security(("sessionCookie" = [])),
    request_body = ProjectCreate,
    responses(
        (status = 200, description = "Deterministic resource and budget range", body = ProjectEstimateResponse),
        (status = 403, description = "Membership cannot estimate projects", body = ErrorResponse)
    )
)]
async fn estimate_project_handler(
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<ProjectCreate>,
) -> Result<Json<ProjectEstimateResponse>, ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    estimate_project(input, &auth.scope)
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

/// Stable operation identity for project starts. The id binds the server-side
/// tenant scope, project, and idempotency key without storing the client key
/// as business data in the operation payload.
pub fn project_start_operation_id(scope: &TenantScope, idempotency_key: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"geo.project.start.v1\0");
    digest.update(scope.storage_key().as_bytes());
    digest.update([0]);
    digest.update(idempotency_key.as_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // UUID version 5 / RFC 4122 variant bits make the deterministic value
    // recognizable as a UUID while retaining the hash-derived identity.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/start",
    security(("sessionCookie" = [])),
    params(("id" = ProjectId, Path, description = "Project ID")),
    request_body = ProjectStartRequest,
    responses(
        (status = 202, description = "Atomic project start acceptance", body = ProjectStartAcceptance),
        (status = 403, description = "Membership cannot start projects", body = ErrorResponse),
        (status = 409, description = "Project is already started or revision changed", body = ErrorResponse)
    )
)]
async fn start_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<ProjectStartRequest>,
) -> Result<Response, ApiError> {
    require_project_writer(&auth).map_err(|error| api_error(error, context.request_id))?;
    let idempotency_key = headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .ok_or_else(|| {
            api_error(
                AppError::invalid_request("missing Idempotency-Key header"),
                context.request_id,
            )
        })?
        .to_str()
        .map_err(|_| {
            api_error(
                AppError::invalid_request("invalid Idempotency-Key header"),
                context.request_id,
            )
        })?
        .trim()
        .to_owned();
    if idempotency_key.is_empty() {
        return Err(api_error(
            AppError::invalid_request("Idempotency-Key must not be empty"),
            context.request_id,
        ));
    }
    let operation_scope = TenantScope::new(auth.scope.operator_id, auth.scope.tenant_id, Some(id));
    let operation_id = project_start_operation_id(&operation_scope, &idempotency_key);
    let project = state
        .project_repository
        .get(&auth.scope, id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .ok_or_else(|| api_error(AppError::not_found("project not found"), context.request_id))?;

    let normalized_settings = project
        .settings
        .clone()
        .validate_draft()
        .map_err(|error| api_error(error, context.request_id))?;
    let frozen_settings_hash = settings_hash(&normalized_settings)
        .map_err(|error| api_error(error, context.request_id))?;
    let command = ProjectStartCommand {
        expected_revision: input.expected_revision,
        idempotency_key_hash: hash_idempotency_key(&idempotency_key),
        request_hash: start_request_hash(id, input.expected_revision, &frozen_settings_hash),
        settings_hash: frozen_settings_hash,
        operation_id,
    };
    let acceptance = state
        .project_repository
        .start(&auth.scope, id, command)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    // PostgreSQL writes this operation in the same start transaction. The
    // in-memory adapter mirrors it in the existing operation store so normal
    // operation lookup remains available in development and tests.
    if !state.durable_storage() {
        let mut operation = Operation::queued("project.start", operation_scope.clone());
        operation.id = acceptance.operation_id;
        operation.result = Some(serde_json::to_value(&acceptance).map_err(|error| {
            api_error(
                AppError::new(
                    geo_domain::ErrorCode::Internal,
                    format!("start acceptance cannot be serialized: {error}"),
                ),
                context.request_id,
            )
        })?);
        state
            .operation_store
            .save(operation)
            .await
            .map_err(|error| api_error(error, context.request_id))?;
        state.publish_event(EventEnvelope::new(
            "cycle.created",
            operation_scope,
            acceptance.cycle_id,
            1,
            acceptance.operation_id,
        ));
    }
    Ok((StatusCode::ACCEPTED, Json(acceptance)).into_response())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectStartRequest {
    pub expected_revision: i64,
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/start",
    security(("sessionCookie" = [])),
    params(("id" = ProjectId, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Persisted project start acceptance", body = ProjectStartAcceptance),
        (status = 404, description = "Project has not been started in this tenant", body = ErrorResponse)
    )
)]
async fn get_project_start(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ProjectStartAcceptance>, ApiError> {
    state
        .project_repository
        .get_start(&scope, id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .map(|view| Json(view.acceptance))
        .ok_or_else(|| {
            api_error(
                AppError::not_found("project start not found"),
                context.request_id,
            )
        })
}

#[utoipa::path(
    get,
    path = "/api/v1/operations/{id}",
    security(("sessionCookie" = [])),
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
        .get(&scope, id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
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
    security(("sessionCookie" = [])),
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
        description = "W02 project setup and overview API with server-side sessions and tenant scopes."
    ),
    paths(
        health_live,
        health_ready,
        auth_config,
        auth_login,
        auth_session,
        auth_logout,
        list_projects,
        create_project,
        get_project,
        patch_project,
        get_project_overview,
        estimate_project_handler,
        start_project,
        get_project_start,
        knowledge::capabilities,
        knowledge::create_upload_session,
        knowledge::put_upload_content,
        knowledge::complete_upload,
        knowledge::import_batch,
        knowledge::materialize_initial_sources,
        knowledge::list_sources,
        knowledge::get_source,
        knowledge::get_source_version,
        knowledge::list_products,
        knowledge::list_facts,
        knowledge::current_release,
        knowledge::search,
        knowledge::ask,
        get_operation,
        events,
        agent::create_conversation,
        agent::list_conversations,
        agent::get_conversation,
        agent::append_message,
        agent::cancel_turn,
        agent::conversation_events
    ),
    components(schemas(
        HealthResponse,
        LoginRequest,
        AuthConfigResponse,
        AuthSessionResponse,
        UserView,
        OperatorView,
        MembershipView,
        Project,
        ProjectCreate,
        ProjectSettings,
        ProjectPage,
        ProjectPatchRequest,
        ProjectSettingsPatchRequest,
        ProjectOverview,
        EstimateState,
        CountEstimate,
        EstimateCoverage,
        EstimateCosts,
        MoneyEstimate,
        EstimateBudget,
        EstimateBlocker,
        ProjectEstimateResponse,
        ProjectStartRequest,
        ProjectStartAcceptance,
        knowledge::KnowledgeProjectQuery,
        knowledge::ImportBatchRequest,
        geo_domain::KnowledgeCapability,
        geo_domain::UploadSessionCommand,
        geo_domain::UploadSession,
        geo_domain::ImportItem,
        geo_domain::ImportAcceptance,
        geo_domain::ImportBatchAcceptance,
        geo_domain::Source,
        geo_domain::SourceDetail,
        geo_domain::SourceVersion,
        geo_domain::Chunk,
        geo_domain::ChunkLocator,
        geo_domain::ImportJob,
        geo_domain::Product,
        geo_domain::Fact,
        geo_domain::KnowledgeRelease,
        geo_domain::CurrentKnowledgeRelease,
        geo_domain::KnowledgeSearchRequest,
        geo_domain::KnowledgeEvidence,
        geo_domain::KnowledgeSearchResult,
        geo_domain::KnowledgeAnswerStatus,
        geo_domain::KnowledgeAskResult,
        ErrorResponse,
        Operation,
        geo_domain::OperationStatus,
        geo_domain::TenantScope,
        geo_domain::OperatorId,
        geo_domain::TenantId,
        geo_domain::ProjectId,
        geo_domain::EventEnvelope,
        geo_domain::AppError,
        geo_domain::ErrorCode,
        geo_domain::Conversation,
        geo_domain::ConversationDetail,
        geo_domain::ConversationEvent,
        geo_domain::ConversationId,
        geo_domain::ConversationStatus,
        geo_domain::CreateConversation,
        geo_domain::AppendMessage,
        geo_domain::SubmitAcceptance,
        geo_domain::Message,
        geo_domain::MessageRole,
        geo_domain::AttachmentReference,
        geo_domain::AttachmentId,
        geo_domain::ObjectRef,
        geo_domain::Turn,
        geo_domain::TurnId,
        geo_domain::TurnStatus,
        geo_domain::Run,
        geo_domain::RunId,
        geo_domain::RunStatus,
        geo_domain::RuntimeCapability,
        geo_domain::RuntimeCapabilityStatus,
        agent::ConversationPage,
        agent::AgentSubmitResponse
    )),
    modifiers(&SecurityModifier)
)]
pub struct ApiDoc;

struct SecurityModifier;

impl utoipa::Modify for SecurityModifier {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "sessionCookie",
                SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("__Host-geo_session"))),
            );
        }
    }
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

/// Build the HTTP router. All business routes use server-resolved identity;
/// the legacy header adapter remains exported only for isolated compatibility
/// tests and is intentionally not installed here.
pub fn router(state: AppState) -> Router {
    let idempotency_store = state.idempotency_store();
    let middleware_state = state.clone();
    let scoped: Router<AppState> = Router::new()
        .route("/operations/{id}", get(get_operation))
        .route("/events", get(events))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", get(get_project).patch(patch_project))
        .route("/projects/{id}/overview", get(get_project_overview))
        .layer(middleware::from_fn_with_state(
            idempotency_store.clone(),
            json_command_idempotency_middleware,
        ))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(auth_scope_from_request));

    let agent_routes: Router<AppState> = Router::new()
        .route(
            "/agent/conversations",
            get(agent::list_conversations).post(agent::create_conversation),
        )
        .route(
            "/agent/conversations/{conversation_id}",
            get(agent::get_conversation),
        )
        .route(
            "/agent/conversations/{conversation_id}/messages",
            post(agent::append_message),
        )
        .route(
            "/agent/conversations/{conversation_id}/events",
            get(agent::conversation_events),
        )
        .route("/agent/turns/{turn_id}/cancel", post(agent::cancel_turn))
        .layer(middleware::from_fn_with_state(
            idempotency_store.clone(),
            json_command_idempotency_middleware,
        ))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(agent::project_scope_middleware))
        .layer(middleware::from_fn(auth_scope_from_request));

    // Project start owns its durable idempotency boundary.  It must not be
    // intercepted by the generic HTTP idempotency cache, whose in-flight
    // state cannot represent a committed business start transaction.
    let start_routes: Router<AppState> = Router::new()
        .route(
            "/projects/{id}/start",
            get(get_project_start).post(start_project),
        )
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(auth_scope_from_request));

    // Estimation only validates and computes a range. It intentionally stays
    // outside the idempotency middleware because it has no external side
    // effect or durable reservation to protect.
    let estimate_routes: Router<AppState> = Router::new()
        .route("/projects/estimate", post(estimate_project_handler))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(auth_scope_from_request));

    // Knowledge uploads carry raw bytes and completion has its own durable
    // idempotency boundary, so this router intentionally stays outside the
    // JSON idempotency middleware.
    let knowledge_routes = knowledge::routes()
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(auth_scope_from_request));

    let auth_session_routes: Router<AppState> = Router::new()
        .route("/auth/session", get(auth_session).delete(auth_logout))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(session_auth_from_request));

    let auth_routes: Router<AppState> = Router::new()
        .route("/auth/config", get(auth_config))
        .route("/auth/login", post(auth_login))
        .merge(auth_session_routes)
        .layer(middleware::from_fn(no_store_middleware));

    Router::new()
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .nest(
            "/api/v1",
            Router::new()
                .route("/openapi.json", get(openapi_json))
                .merge(auth_routes)
                .merge(estimate_routes)
                .merge(start_routes)
                .merge(knowledge_routes)
                .merge(agent_routes)
                .merge(scoped),
        )
        .layer(Extension(middleware_state))
        .layer(middleware::from_fn(context::request_context_middleware))
        .with_state(state)
}
