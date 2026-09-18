use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::timeout;

use crate::{DatabaseConfig, PersistenceError, migrations::MIGRATOR};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthCheck {
    pub reachable: bool,
}

impl HealthCheck {
    const REACHABLE: Self = Self { reachable: true };
}

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let connect = PgPoolOptions::new()
            .min_connections(config.min_connections())
            .max_connections(config.max_connections())
            .acquire_timeout(config.acquire_timeout())
            .connect(config.database_url());
        let pool = timeout(config.connect_timeout(), connect)
            .await
            .map_err(|_| PersistenceError::ConnectionTimeout(config.connect_timeout()))??;
        Ok(Self { pool })
    }

    pub async fn connect_from_env() -> Result<Self, PersistenceError> {
        Self::connect(&DatabaseConfig::from_env()?).await
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn health_check(&self) -> Result<HealthCheck, PersistenceError> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(HealthCheck::REACHABLE)
    }

    pub async fn migrate(&self) -> Result<(), PersistenceError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    pub async fn connect_and_migrate(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let database = Self::connect(config).await?;
        database.migrate().await?;
        Ok(database)
    }

    pub async fn connect_and_migrate_from_env() -> Result<Self, PersistenceError> {
        Self::connect_and_migrate(&DatabaseConfig::from_env()?).await
    }
}
