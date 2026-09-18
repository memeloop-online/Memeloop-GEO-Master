//! PostgreSQL persistence primitives shared by the API and background roles.
//!
//! The application currently uses development-only in-memory stores. This
//! crate keeps the durable boundary explicit and can be adopted by a role when
//! that role is ready to persist state. Database credentials are supplied at
//! runtime through configuration; they are never embedded in this crate.

mod config;
mod database;
mod error;
mod migrations;

pub use config::{DatabaseConfig, DatabaseConfigError};
pub use database::{Database, HealthCheck};
pub use error::PersistenceError;
pub use migrations::{MIGRATOR, MigrationMetadata, embedded_migrations, migration_metadata};
