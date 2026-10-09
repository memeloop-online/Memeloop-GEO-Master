//! PostgreSQL persistence primitives shared by the API and background roles.
//!
//! The application currently uses development-only in-memory stores. This
//! crate keeps the durable boundary explicit and can be adopted by a role when
//! that role is ready to persist state. Database credentials are supplied at
//! runtime through configuration; they are never embedded in this crate.

mod agent;
mod appearance;
mod auth;
pub mod bootstrap;
mod channel_jobs;
mod channels;
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
mod migrations;
mod model_routes;
pub use model_routes::PgModelRouteRepository;
mod observation_capture;
pub use observation_capture::PgObservationCaptureRepository;
mod observation_analysis;
pub use observation_analysis::PgObservationAnalysisRepository;
mod provider_conversation_cleanup;
pub use provider_conversation_cleanup::PgProviderConversationCleanupRepository;
mod projects;
mod publication_lookup;
pub use publication_lookup::{PgPublicationLookupRepository, PublicationLookupDiscoveryCandidate};
mod publication_send_authorization;
pub use publication_send_authorization::PgPublicationSendAuthorizationRepository;
mod questions;
pub use questions::PgQuestionRepository;
mod measurement_report;
mod report;
mod serp;
pub use serp::{MemorySerpRepository, PgSerpRepository};
mod scope;

pub use agent::{PgAgentRepository, QueuedAgentRun};
pub use auth::PgAuthRepository;
pub use config::{DatabaseConfig, DatabaseConfigError};
pub use database::{Database, HealthCheck};
pub use error::PersistenceError;
pub use idempotency::PgIdempotencyStore;
pub use knowledge::PgKnowledgeRepository;
pub use migrations::{MIGRATOR, MigrationMetadata, embedded_migrations, migration_metadata};
pub use projects::{ContentBootstrapLease, PendingContentCycle, PgProjectRepository};
pub use report::PgReportRepository;
pub use scope::set_local_scope;
mod project_ai_settings;
pub use project_ai_settings::PgProjectAiSettingsRepository;
