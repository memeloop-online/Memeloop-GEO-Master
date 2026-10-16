//! PostgreSQL persistence primitives shared by the API and background roles.
//!
//! Database credentials are supplied at runtime through configuration.

mod accounts;
mod agent;
mod config;
mod content;
mod database;
mod error;
mod idempotency;
mod knowledge;
mod measurement;
mod migrations;
mod partners;
mod projects;
mod publishing;
mod scope;

pub use accounts::{PgAuthRepository, PgChannelJobRepository, PgChannelRepository, bootstrap};
pub use agent::{PgAgentRepository, QueuedAgentRun};
pub use config::{DatabaseConfig, DatabaseConfigError};
pub use content::{
    ContentDispatchCandidate, ContentDispatchLease, PgContentMediaRepository, PgContentRepository,
};
pub use database::{Database, HealthCheck};
pub use error::PersistenceError;
pub use geo_domain::MeasurementWeeklySchedule;
pub use idempotency::PgIdempotencyStore;
pub use knowledge::PgKnowledgeRepository;
pub use measurement::{
    MemorySerpRepository, PgObservationAnalysisRepository, PgObservationCaptureRepository,
    PgObservationCompletionRecoveryJobRepository, PgObservationCompletionRecoveryRepository,
    PgObservationExtractionRepository, PgProjectSerpSettingsRepository,
    PgProviderConversationCleanupRepository, PgQuestionRepository, PgReportRepository,
    PgSerpRepository,
};
pub use migrations::MIGRATOR;
pub use partners::{PgPartnerAdminRepository, PgPartnerRepository};
pub use projects::{
    ContentBootstrapLease, PendingContentCycle, PgModelRouteRepository,
    PgProjectAiSettingsRepository, PgProjectRepository,
};
pub use publishing::{
    PgConnectorCapabilityRepository, PgContentDistributionRequestRepository,
    PgDistributionRepository, PgPublicationLookupRepository,
    PgPublicationSendAuthorizationRepository, PublicationLookupDiscoveryCandidate,
};
pub use scope::set_local_scope;
