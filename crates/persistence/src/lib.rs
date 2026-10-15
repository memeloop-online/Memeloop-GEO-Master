//! PostgreSQL persistence primitives shared by the API and background roles.
//!
//! Database credentials are supplied at runtime through configuration.

mod account_networks;
mod agent;
mod appearance;
mod auth;
pub mod bootstrap;
mod browser_operations;
mod channel_jobs;
mod channels;
mod operator_login_queue;
mod proxy_pools;
pub use channel_jobs::PgChannelJobRepository;
pub use channels::PgChannelRepository;
mod connector_capabilities;
pub use connector_capabilities::PgConnectorCapabilityRepository;
mod config;
mod content;
pub use content::{ContentDispatchCandidate, ContentDispatchLease, PgContentRepository};
mod content_distribution_request;
pub use content_distribution_request::PgContentDistributionRequestRepository;
mod content_media;
pub use content_media::PgContentMediaRepository;
mod database;
mod distribution;
pub use distribution::PgDistributionRepository;
mod error;
mod idempotency;
mod knowledge;
mod partner;
pub use partner::PgPartnerRepository;
mod partner_admin;
pub use partner_admin::PgPartnerAdminRepository;
mod migrations;
mod model_routes;
pub use model_routes::PgModelRouteRepository;
mod measurement;
pub use measurement::{
    MemorySerpRepository, PgObservationAnalysisRepository, PgObservationCaptureRepository,
    PgObservationCompletionRecoveryJobRepository, PgObservationCompletionRecoveryRepository,
    PgObservationExtractionRepository, PgProjectSerpSettingsRepository,
    PgProviderConversationCleanupRepository, PgReportRepository, PgSerpRepository,
};
mod projects;
mod publication_lookup;
pub use publication_lookup::{PgPublicationLookupRepository, PublicationLookupDiscoveryCandidate};
mod publication_send_authorization;
pub use publication_send_authorization::PgPublicationSendAuthorizationRepository;
mod questions;
pub use geo_domain::MeasurementWeeklySchedule;
pub use questions::PgQuestionRepository;
mod scope;

pub use agent::{PgAgentRepository, QueuedAgentRun};
pub use auth::PgAuthRepository;
pub use config::{DatabaseConfig, DatabaseConfigError};
pub use database::{Database, HealthCheck};
pub use error::PersistenceError;
pub use idempotency::PgIdempotencyStore;
pub use knowledge::PgKnowledgeRepository;
pub use migrations::MIGRATOR;
pub use projects::{ContentBootstrapLease, PendingContentCycle, PgProjectRepository};
pub use scope::set_local_scope;
mod project_ai_settings;
pub use project_ai_settings::PgProjectAiSettingsRepository;
