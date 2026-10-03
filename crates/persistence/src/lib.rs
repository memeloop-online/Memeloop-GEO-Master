//! PostgreSQL persistence primitives shared by the API and background roles.
//!
//! The application currently uses development-only in-memory stores. This
//! crate keeps the durable boundary explicit and can be adopted by a role when
//! that role is ready to persist state. Database credentials are supplied at
//! runtime through configuration; they are never embedded in this crate.

mod agent;
mod auth;
pub mod bootstrap;
mod channel_jobs;
mod channels;
pub use channel_jobs::PgChannelJobRepository;
pub use channels::PgChannelRepository;
mod config;
mod content;
pub use content::PgContentRepository;
mod database;
mod error;
mod idempotency;
mod knowledge;
mod migrations;
mod model_routes;
pub use model_routes::PgModelRouteRepository;
mod projects;
mod report;
mod scope;

pub use agent::PgAgentRepository;
pub use auth::PgAuthRepository;
pub use config::{DatabaseConfig, DatabaseConfigError};
pub use database::{Database, HealthCheck};
pub use error::PersistenceError;
pub use idempotency::PgIdempotencyStore;
pub use knowledge::PgKnowledgeRepository;
pub use migrations::{MIGRATOR, MigrationMetadata, embedded_migrations, migration_metadata};
pub use projects::PgProjectRepository;
pub use report::PgReportRepository;
pub use scope::set_local_scope;
