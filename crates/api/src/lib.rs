//! Axum HTTP boundary for the GEO modular monolith.

mod agent;
mod agent_runtime;
mod browser_bridge;
mod channel_jobs;
mod channel_tools;
mod channels;
mod connector_capabilities;
mod content;
pub mod content_runtime;
mod content_tools;
pub mod distribution;
pub use content::ContentService;
mod context;
mod cycles;
mod error;
mod idempotency;
mod knowledge;
mod pdf_parse;
pub use pdf_parse::{
    PDF_PARSER_PROFILE, PdfParserClient, dispatch_pdf_parse_job, spawn_pdf_parse_scanner,
};
mod provider_bridge;
mod publication_lookup;
pub use publication_lookup::dispatch_publication_lookup;
mod reports;
mod run_executor;
pub use run_executor::dispatch_queued;
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
    AgentRepository, AgentRuntime, AppError, ConnectorCapabilityRepository,
    DEFAULT_SESSION_TTL_SECS, DistributionScope, DocumentScope, EventEnvelope, InitialSource,
    KnowledgeRepository, Membership, MemoryAgentRepository, MemoryAuthRepository,
    MemoryConnectorCapabilityRepository, MemoryKnowledgeRepository, MissingAgentRuntime, Operation,
    Operator, Project, ProjectCreate, ProjectId, ProjectOverview, ProjectPage, ProjectPatch,
    ProjectRepository, ProjectSettings, ProjectStartAcceptance, ProjectStartCommand,
    ReportRepository, ReportSchedule, ResourceMode, Role, TenantId, TenantScope, User,
    hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, PgAuthRepository, PgIdempotencyStore, PgKnowledgeRepository, PgProjectRepository,
    PgReportRepository,
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
pub use browser_bridge::BrowserBridge;
pub use channel_jobs::{
    ChannelDispatchDeferred, ChannelDispatchResult, MeasurementRequest, PlanRequest,
    PublicationRequest, create_channel_plan, execute_channel_target,
};
pub use channels::ChannelService;
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
pub use provider_bridge::{
    ModelProviderBridge, ProviderClientBridge, ProviderRoute, ProviderRouteResolver,
    RoutedProviderClientBridge, SharedModelProvider,
};
pub use reports::{preview_cycle_report, reduce_cycle_report};
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
    report_repository: Arc<dyn ReportRepository>,
    connector_capability_repository: Arc<dyn ConnectorCapabilityRepository>,
    channel_service: ChannelService,
    channel_job_repository: Arc<dyn geo_domain::ChannelJobRepository>,
    publication_lookup_repository: Option<Arc<dyn geo_domain::PublicationLookupRepository>>,
    content_repository: Arc<dyn geo_domain::ContentRepository>,
    distribution_repository: Arc<dyn geo_domain::DistributionRepository>,
    content_dispatch_repository: Option<geo_persistence::PgContentRepository>,
    content_model: Arc<std::sync::RwLock<Option<SharedModelProvider>>>,
    content_executor:
        Arc<std::sync::RwLock<Option<Arc<dyn content_runtime::ContentWorkflowExecutor>>>>,
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
            report_repository: Arc::new(geo_domain::MemoryReportRepository::default()),
            connector_capability_repository: Arc::new(
                MemoryConnectorCapabilityRepository::default(),
            ),
            channel_service: ChannelService::development(),
            channel_job_repository: Arc::new(geo_domain::MemoryChannelJobRepository::default()),
            publication_lookup_repository: None,
            content_repository: Arc::new(geo_domain::MemoryContentRepository::default()),
            distribution_repository: Arc::new(geo_domain::MemoryDistributionRepository::default()),
            content_dispatch_repository: None,
            content_model: Arc::new(std::sync::RwLock::new(None)),
            content_executor: Arc::new(std::sync::RwLock::new(None)),
            events: EventBus::default(),
            ready: Arc::new(AtomicBool::new(false)),
            durable_storage: false,
            origin_scheme: Arc::from("http"),
            origin_config: OriginConfig::local_http(),
        }
    }

    pub fn development_with_pdf_parser_profile(password: &str, profile: String) -> Self {
        let mut state = Self::development_with_password(password);
        state.knowledge_repository =
            Arc::new(MemoryKnowledgeRepository::with_pdf_parser_profile(profile));
        state
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
            report_repository: Arc::new(geo_domain::MemoryReportRepository::default()),
            connector_capability_repository: Arc::new(
                MemoryConnectorCapabilityRepository::default(),
            ),
            channel_service: ChannelService::development(),
            channel_job_repository: Arc::new(geo_domain::MemoryChannelJobRepository::default()),
            publication_lookup_repository: None,
            content_repository: Arc::new(geo_domain::MemoryContentRepository::default()),
            distribution_repository: Arc::new(geo_domain::MemoryDistributionRepository::default()),
            content_dispatch_repository: None,
            content_model: Arc::new(std::sync::RwLock::new(None)),
            content_executor: Arc::new(std::sync::RwLock::new(None)),
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
        .with_agent_repository(Arc::new(geo_persistence::PgAgentRepository::from_database(
            database,
        )))
        .with_report_repository(Arc::new(PgReportRepository::from_database(database)))
        .with_connector_capability_repository(Arc::new(
            geo_persistence::PgConnectorCapabilityRepository::from_database(database),
        ))
        .with_content_repository(Arc::new(
            geo_persistence::PgContentRepository::from_database(database),
        ))
        .with_distribution_repository(Arc::new(
            geo_persistence::PgDistributionRepository::from_database(database),
        ))
        .with_content_dispatch_repository(geo_persistence::PgContentRepository::from_database(
            database,
        ))
        .with_channel_job_repository(Arc::new(
            geo_persistence::PgChannelJobRepository::from_database(database),
        ))
        .with_publication_lookup_repository(Arc::new(
            geo_persistence::PgPublicationLookupRepository::from_database(database),
        ))
        .with_channel_service(ChannelService::unconfigured(Arc::new(
            geo_persistence::PgChannelRepository::from_database(database),
        )))
    }

    pub fn from_database_with_pdf_parser_profile(database: &Database, profile: String) -> Self {
        let mut state = Self::from_database(database);
        state.knowledge_repository = Arc::new(
            PgKnowledgeRepository::from_database(database).with_pdf_parser_profile(profile),
        );
        state
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

    pub fn report_repository(&self) -> Arc<dyn ReportRepository> {
        Arc::clone(&self.report_repository)
    }

    pub fn connector_capability_repository(&self) -> Arc<dyn ConnectorCapabilityRepository> {
        Arc::clone(&self.connector_capability_repository)
    }

    pub fn with_connector_capability_repository(
        mut self,
        repository: Arc<dyn ConnectorCapabilityRepository>,
    ) -> Self {
        self.connector_capability_repository = repository;
        self
    }

    pub fn with_report_repository(mut self, repository: Arc<dyn ReportRepository>) -> Self {
        self.report_repository = repository;
        self
    }

    pub fn durable_storage(&self) -> bool {
        self.durable_storage
    }

    pub fn channel_service(&self) -> &ChannelService {
        &self.channel_service
    }

    pub fn content_service(&self) -> ContentService {
        let service = ContentService::new(
            Arc::clone(&self.content_repository),
            self.knowledge_repository(),
            self.project_repository(),
        );
        match self
            .content_model
            .read()
            .expect("content model lock")
            .clone()
        {
            Some(provider) => service.with_model_provider(provider),
            None => service,
        }
    }

    pub fn distribution_repository(&self) -> Arc<dyn geo_domain::DistributionRepository> {
        Arc::clone(&self.distribution_repository)
    }

    pub fn distribution_service(&self) -> distribution::DistributionService {
        distribution::DistributionService::new(
            Arc::clone(&self.distribution_repository),
            Arc::clone(&self.content_repository),
            self.knowledge_repository(),
            self.project_repository(),
            Arc::clone(&self.channel_service.repository),
        )
        .with_connector_registry(
            self.connector_capability_repository(),
            self.channel_service.browser.clone(),
        )
    }

    pub fn with_distribution_repository(
        mut self,
        repository: Arc<dyn geo_domain::DistributionRepository>,
    ) -> Self {
        self.distribution_repository = repository;
        self
    }

    pub fn with_content_repository(
        mut self,
        repository: Arc<dyn geo_domain::ContentRepository>,
    ) -> Self {
        self.content_repository = repository;
        self
    }

    fn with_content_dispatch_repository(
        mut self,
        repository: geo_persistence::PgContentRepository,
    ) -> Self {
        self.content_dispatch_repository = Some(repository);
        self
    }

    /// Startup-only assembly; shared with the already constructed host bridge.
    pub fn configure_content_model(&self, provider: SharedModelProvider) {
        *self.content_model.write().expect("content model lock") = Some(provider);
    }

    pub fn content_model_available(&self) -> bool {
        self.content_model
            .read()
            .expect("content model lock")
            .is_some()
    }

    pub fn configure_content_executor(
        &self,
        executor: Arc<dyn content_runtime::ContentWorkflowExecutor>,
    ) {
        *self
            .content_executor
            .write()
            .expect("content executor lock") = Some(executor);
    }

    pub fn content_executor_available(&self) -> bool {
        self.content_executor
            .read()
            .expect("content executor lock")
            .is_some()
    }

    pub fn dispatch_content_execution(
        &self,
        scope: TenantScope,
        execution_id: Uuid,
    ) -> Result<(), AppError> {
        let executor = self
            .content_executor
            .read()
            .expect("content executor lock")
            .clone()
            .ok_or_else(|| {
                AppError::capability_missing("content workflow engine is not configured")
            })?;
        let Some(repository) = self.content_dispatch_repository.clone() else {
            return executor.dispatch(scope, execution_id);
        };
        tokio::runtime::Handle::try_current()
            .map_err(|_| AppError::capability_missing("content workflow requires an application runtime"))?
            .spawn(async move {
                let now = chrono::Utc::now();
                let lease = match repository
                    .try_claim_dispatch(&scope, execution_id, now, chrono::Duration::seconds(90))
                    .await
                {
                    Ok(Some(lease)) => lease,
                    Ok(None) => return,
                    Err(error) => {
                        tracing::warn!(code = ?error.code, "content dispatch claim failed");
                        return;
                    }
                };
                let cancellation = Arc::new(AtomicBool::new(false));
                let mut run = Box::pin(executor.run_supervised(scope, execution_id, Arc::clone(&cancellation)));
                let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // The engine may be in a non-cancellable provider operation if
                // renewal fails. Never claim exactly-once external calls:
                // persisted step tokens fence late result writes instead.
                let outcome = loop {
                    tokio::select! {
                        result = &mut run => break Some(result),
                        _ = heartbeat.tick() => {
                            match repository.renew_dispatch(&lease, chrono::Utc::now(), chrono::Duration::seconds(90)).await {
                                Ok(true) => {}
                                Ok(false) => {
                                    cancellation.store(true, Ordering::SeqCst);
                                    tracing::warn!("content dispatch lease lost; waiting for engine to finish");
                                    break None;
                                }
                                Err(error) => {
                                    cancellation.store(true, Ordering::SeqCst);
                                    tracing::warn!(code = ?error.code, "content dispatch heartbeat failed");
                                    break None;
                                }
                            }
                        }
                    }
                };
                if outcome.is_none() {
                    let _ = run.await;
                }
                if let Some(Err(error)) = &outcome {
                    tracing::warn!(code = ?error.code, "content workflow interrupted; durable item state remains resumable");
                }
                // A completed preparation pass may still have deferred account
                // or source dependencies. Retry those with backoff; a fully
                // prepared execution is excluded by the durable scanner.
                let backoff = if matches!(outcome, Some(Ok(()))) { 300 } else { 30 };
                if let Err(error) = repository.release_dispatch(&lease, chrono::Utc::now(), chrono::Duration::seconds(backoff)).await {
                    tracing::warn!(code = ?error.code, "content dispatch release failed");
                }
            });
        Ok(())
    }

    pub fn channel_job_repository(&self) -> Arc<dyn geo_domain::ChannelJobRepository> {
        Arc::clone(&self.channel_job_repository)
    }

    pub fn with_publication_lookup_repository(
        mut self,
        repository: Arc<dyn geo_domain::PublicationLookupRepository>,
    ) -> Self {
        self.publication_lookup_repository = Some(repository);
        self
    }

    pub fn with_channel_job_repository(
        mut self,
        repository: Arc<dyn geo_domain::ChannelJobRepository>,
    ) -> Self {
        self.channel_job_repository = repository;
        self
    }

    pub fn with_channel_service(mut self, service: ChannelService) -> Self {
        self.channel_service = service;
        self
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

    /// Reconcile abandoned in-flight runs before this process accepts work.
    ///
    /// The repository operation is deliberately not started by `router` or a
    /// request handler. It is a startup-only hook and must be enabled only
    /// when this process is the sole executor for the database.
    pub async fn reconcile_running_runs(&self) -> Result<u64, AppError> {
        self.agent_repository.reconcile_running_runs().await
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
        reports::list_reports,
        reports::get_report,
        reports::get_report_evidence,
        reports::get_report_preview,
        reports::create_reduction,
        knowledge::capabilities,
        knowledge::create_upload_session,
        knowledge::put_upload_content,
        knowledge::complete_upload,
        knowledge::import_batch,
        knowledge::materialize_initial_sources,
        knowledge::list_sources,
        knowledge::get_source,
        knowledge::get_import_job,
        knowledge::retry_import_job,
        knowledge::get_source_version,
        knowledge::list_products,
        knowledge::list_facts,
        knowledge::current_release,
        knowledge::get_document_manifest,
        knowledge::plan_document_manifest,
        knowledge::search,
        knowledge::ask,
        get_operation,
        events,
        agent::create_conversation,
        agent::list_conversations,
        agent::get_conversation,
        agent::append_message,
        agent::create_attachment_upload,
        agent::put_attachment_content,
        agent::complete_attachment_upload,
        agent::get_attachment,
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
        reports::ReportProjectQuery,
        reports::ReduceRequest,
        reports::ReportList,
        reports::ReportEvidenceList,
        geo_domain::ReportSnapshot,
        geo_domain::ReportPreview,
        geo_domain::ReportPreviewKind,
        geo_domain::ReportStatus,
        geo_domain::ReportAvailability,
        geo_domain::ReportCoverage,
        geo_domain::ReportManifestKind,
        geo_domain::ReportManifestRef,
        geo_domain::ReportEvidenceReference,
        geo_domain::ReportFinding,
        geo_domain::ReportPublicationGroup,
        geo_domain::ReportMeasurementGroup,
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
        geo_domain::DocumentManifest,
        geo_domain::DocumentManifestCoverage,
        geo_domain::DocumentManifestItem,
        geo_domain::DocumentManifestItemState,
        geo_domain::DocumentManifestPlanRequest,
        geo_domain::DocumentManifestState,
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
        agent::AgentSubmitResponse,
        agent::AttachmentUploadCommand
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
        .route(
            "/agent/attachments/upload-sessions",
            post(agent::create_attachment_upload),
        )
        .route(
            "/agent/attachments/upload-sessions/{id}/complete",
            post(agent::complete_attachment_upload),
        )
        .route("/agent/attachments/{id}", get(agent::get_attachment))
        .layer(middleware::from_fn_with_state(
            idempotency_store.clone(),
            json_command_idempotency_middleware,
        ))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(agent::project_scope_middleware))
        .layer(middleware::from_fn(auth_scope_from_request));

    // Raw upload bytes exceed the JSON command cache's 1 MiB buffer. The
    // knowledge repository verifies bytes and scopes the session itself.
    let agent_attachment_bytes: Router<AppState> = Router::new()
        .route(
            "/agent/attachments/upload-sessions/{id}/content",
            axum::routing::put(agent::put_attachment_content),
        )
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

    // The repository enforces immutable revision/replay semantics. The
    // generic JSON idempotency cache is deliberately not the report authority.
    let report_routes: Router<AppState> = Router::new()
        .route("/projects/{id}/cycles/current", get(cycles::current))
        .route("/projects/{id}/cycles", post(cycles::schedule_successor))
        .route("/projects/{id}/reports", get(reports::list_reports))
        .route("/reports/{id}", get(reports::get_report))
        .route("/reports/{id}/evidence", get(reports::get_report_evidence))
        .route(
            "/cycles/{id}/report-preview",
            get(reports::get_report_preview),
        )
        .route("/cycles/{id}/reductions", post(reports::create_reduction))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(auth_scope_from_request));

    // Login actions can contain transient credentials. Never put these
    // requests through the generic idempotency cache.
    let channel_routes = channels::customer_routes()
        .route(
            "/projects/{project_id}/cycles/{cycle_id}/channel-plan",
            get(channel_jobs::get_plan).post(channel_jobs::submit_plan),
        )
        .route(
            "/projects/{project_id}/channel-targets/{target_id}",
            get(channel_jobs::get_target),
        )
        .route(
            "/projects/{project_id}/channel-targets/{target_id}/publication-lookup",
            get(publication_lookup::get_publication_lookup),
        )
        .route(
            "/projects/{project_id}/channel-targets/{target_id}/execute",
            post(channel_jobs::execute_target),
        )
        .layer(middleware::from_fn(no_store_middleware))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(auth_scope_from_request));

    // Operator pool scope comes from deployment configuration plus trusted
    // membership, not a customer tenant selector. The catalogue is session-only.
    let operator_channel_routes = channels::operator_routes()
        .route(
            "/operator/connector-capabilities",
            get(connector_capabilities::list_operator),
        )
        .route(
            "/operator/connector-capabilities/{platform_id}/{placement_slot}",
            axum::routing::patch(connector_capabilities::configure_operator),
        )
        .layer(middleware::from_fn(no_store_middleware))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(session_auth_from_request));

    let content_routes: Router<AppState> = Router::new()
        .route(
            "/projects/{id}/connector-capabilities",
            get(connector_capabilities::list_project),
        )
        .route(
            "/projects/{id}/cycles/{cycle_id}/distribution-manifest",
            get(distribution::cycle_manifest).post(distribution::freeze),
        )
        .route(
            "/projects/{id}/distribution-manifests/{manifest_id}",
            get(distribution::manifest),
        )
        .route(
            "/projects/{id}/distribution-manifests/{manifest_id}/targets",
            get(distribution::targets),
        )
        .route(
            "/projects/{id}/distribution-manifests/{manifest_id}/targets/{target_id}",
            get(distribution::target),
        )
        .route(
            "/projects/{id}/distribution-manifests/{manifest_id}/targets/{target_id}/publication-target",
            get(distribution::publication_target),
        )
        .route(
            "/projects/{id}/distribution-manifests/{manifest_id}/resume",
            post(distribution::resume),
        )
        .route(
            "/projects/{id}/cycles/{cycle_id}/document-executions",
            get(content::executions).post(content::start),
        )
        .route(
            "/projects/{id}/document-executions/{execution_id}",
            get(content::execution),
        )
        .route(
            "/projects/{id}/document-executions/{execution_id}/items",
            get(content::items),
        )
        .route(
            "/projects/{id}/document-executions/{execution_id}/resume",
            post(content::resume),
        )
        .route(
            "/projects/{id}/document-executions/{execution_id}/cancel",
            post(content::cancel),
        )
        .route("/projects/{id}/contents", get(content::contents))
        .route("/projects/{id}/contents/{asset_id}", get(content::asset))
        .route(
            "/projects/{id}/contents/{asset_id}/revisions",
            get(content::revisions).post(content::edit),
        )
        .layer(middleware::from_fn(no_store_middleware))
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
                .merge(report_routes)
                .merge(channel_routes)
                .merge(content_routes)
                .merge(operator_channel_routes)
                .merge(knowledge_routes)
                .merge(agent_attachment_bytes)
                .merge(agent_routes)
                .merge(scoped),
        )
        .layer(Extension(middleware_state))
        .layer(middleware::from_fn(context::request_context_middleware))
        .with_state(state)
}
