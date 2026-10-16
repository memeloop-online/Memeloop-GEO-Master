use sqlx::migrate::Migrator;

/// SQL migrations embedded in the service artifact.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");
