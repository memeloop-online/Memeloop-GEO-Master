use std::{env, time::Duration};
use thiserror::Error;

const DEFAULT_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_MIN_CONNECTIONS: u32 = 0;
const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 5;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 5;

/// Runtime settings for a PostgreSQL connection pool.
///
/// `database_url` is intentionally opaque and is never included in `Debug` or
/// error output. Supply it using `DATABASE_URL` (or construct this value from
/// a secret manager at the application boundary).
#[derive(Clone)]
pub struct DatabaseConfig {
    database_url: String,
    max_connections: u32,
    min_connections: u32,
    acquire_timeout: Duration,
    connect_timeout: Duration,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseConfig")
            .field("database_url", &"<redacted>")
            .field("max_connections", &self.max_connections)
            .field("min_connections", &self.min_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

impl DatabaseConfig {
    /// Builds settings from the process environment without providing a
    /// development fallback that could accidentally target the wrong database.
    pub fn from_env() -> Result<Self, DatabaseConfigError> {
        let database_url = env::var("DATABASE_URL")
            .map_err(|_| DatabaseConfigError::MissingEnvironment("DATABASE_URL"))?;
        Self::from_url_and_env(database_url)
    }

    /// Builds settings from a URL and the optional pool environment variables.
    ///
    /// This constructor is useful when a deployment obtains the URL from a
    /// secret manager rather than exposing it as a process environment value.
    pub fn from_url(database_url: impl Into<String>) -> Result<Self, DatabaseConfigError> {
        Self::from_url_with_settings(
            database_url.into(),
            DEFAULT_MAX_CONNECTIONS,
            DEFAULT_MIN_CONNECTIONS,
            Duration::from_secs(DEFAULT_ACQUIRE_TIMEOUT_SECS),
            Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS),
        )
    }

    fn from_url_and_env(database_url: String) -> Result<Self, DatabaseConfigError> {
        let max_connections = optional_u32("DATABASE_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;
        let min_connections = optional_u32("DATABASE_MIN_CONNECTIONS", DEFAULT_MIN_CONNECTIONS)?;
        if min_connections > max_connections {
            return Err(DatabaseConfigError::InvalidConnectionBounds {
                min: min_connections,
                max: max_connections,
            });
        }

        let acquire_timeout = optional_duration(
            "DATABASE_ACQUIRE_TIMEOUT_SECS",
            DEFAULT_ACQUIRE_TIMEOUT_SECS,
        )?;
        let connect_timeout = optional_duration(
            "DATABASE_CONNECT_TIMEOUT_SECS",
            DEFAULT_CONNECT_TIMEOUT_SECS,
        )?;

        Self::from_url_with_settings(
            database_url,
            max_connections,
            min_connections,
            acquire_timeout,
            connect_timeout,
        )
    }

    fn from_url_with_settings(
        database_url: String,
        max_connections: u32,
        min_connections: u32,
        acquire_timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self, DatabaseConfigError> {
        if database_url.trim().is_empty() {
            return Err(DatabaseConfigError::EmptyDatabaseUrl);
        }
        if min_connections > max_connections {
            return Err(DatabaseConfigError::InvalidConnectionBounds {
                min: min_connections,
                max: max_connections,
            });
        }

        Ok(Self {
            database_url,
            max_connections,
            min_connections,
            acquire_timeout,
            connect_timeout,
        })
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub const fn max_connections(&self) -> u32 {
        self.max_connections
    }

    pub const fn min_connections(&self) -> u32 {
        self.min_connections
    }

    pub const fn acquire_timeout(&self) -> Duration {
        self.acquire_timeout
    }

    pub const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DatabaseConfigError {
    #[error("required database setting {0} is missing")]
    MissingEnvironment(&'static str),
    #[error("DATABASE_URL must not be empty")]
    EmptyDatabaseUrl,
    #[error("invalid {name}: {value}")]
    InvalidInteger { name: &'static str, value: String },
    #[error("database pool minimum connections ({min}) exceed maximum ({max})")]
    InvalidConnectionBounds { min: u32, max: u32 },
}

fn optional_u32(name: &'static str, default: u32) -> Result<u32, DatabaseConfigError> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u32>()
            .map_err(|_| invalid_integer(name, value)),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(DatabaseConfigError::InvalidInteger {
            name,
            value: "<non-unicode>".to_owned(),
        }),
    }
}

fn optional_duration(
    name: &'static str,
    default_seconds: u64,
) -> Result<Duration, DatabaseConfigError> {
    let seconds = match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| invalid_integer(name, value))?,
        Err(env::VarError::NotPresent) => default_seconds,
        Err(env::VarError::NotUnicode(_)) => {
            return Err(DatabaseConfigError::InvalidInteger {
                name,
                value: "<non-unicode>".to_owned(),
            });
        }
    };
    Ok(Duration::from_secs(seconds))
}

fn invalid_integer(name: &'static str, value: String) -> DatabaseConfigError {
    DatabaseConfigError::InvalidInteger { name, value }
}

#[cfg(test)]
mod tests {
    use super::{DatabaseConfig, DatabaseConfigError};

    #[test]
    fn config_defaults_are_explicit_and_credentials_are_redacted() {
        let config = DatabaseConfig::from_url("postgres://example.invalid/db").unwrap();

        assert_eq!(config.max_connections(), 10);
        assert_eq!(config.min_connections(), 0);
        assert_eq!(config.acquire_timeout().as_secs(), 5);
        assert_eq!(config.connect_timeout().as_secs(), 5);
        assert!(!format!("{config:?}").contains("example.invalid"));
    }

    #[test]
    fn empty_url_is_rejected() {
        assert_eq!(
            DatabaseConfig::from_url("  ").unwrap_err(),
            DatabaseConfigError::EmptyDatabaseUrl
        );
    }
}
