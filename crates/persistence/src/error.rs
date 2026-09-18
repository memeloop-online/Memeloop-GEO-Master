use std::time::Duration;

use thiserror::Error;

use crate::DatabaseConfigError;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("database configuration error: {0}")]
    Configuration(#[from] DatabaseConfigError),
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("database connection timed out after {0:?}")]
    ConnectionTimeout(Duration),
    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}
