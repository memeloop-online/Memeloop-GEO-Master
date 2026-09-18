use std::{env, net::SocketAddr, str::FromStr};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub ready_on_start: bool,
    /// The in-memory adapter is only valid for an explicitly supplied local
    /// development password.  It is never a production fallback.
    pub dev_password: Option<String>,
    /// Exact browser origins accepted for login and state-changing requests.
    pub allowed_origins: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            ready_on_start: true,
            dev_password: None,
            allowed_origins: vec![
                "http://localhost:5173".to_owned(),
                "http://127.0.0.1:5173".to_owned(),
                "http://localhost:8080".to_owned(),
                "http://127.0.0.1:8080".to_owned(),
            ],
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid {name}: {value}")]
    Invalid { name: &'static str, value: String },
    #[error("in-memory development mode requires GEO_DEV_PASSWORD")]
    MissingDevelopmentPassword,
    #[error("in-memory development mode must bind to a loopback address")]
    DevelopmentMustBindLoopback,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let defaults = Self::default();
        let bind_addr = match env::var("GEO_BIND_ADDR") {
            Ok(value) => SocketAddr::from_str(&value).map_err(|_| ConfigError::Invalid {
                name: "GEO_BIND_ADDR",
                value,
            })?,
            Err(_) => defaults.bind_addr,
        };
        let ready_on_start = env_bool("GEO_READY_ON_START", defaults.ready_on_start)?;
        let dev_password = match env::var("GEO_DEV_PASSWORD") {
            Ok(value) => Some(value),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::Invalid {
                    name: "GEO_DEV_PASSWORD",
                    value: "<non-unicode>".to_owned(),
                });
            }
        };
        let allowed_origins = match env::var("GEO_ALLOWED_ORIGINS") {
            Ok(value) => parse_origins(value)?,
            Err(env::VarError::NotPresent) => defaults.allowed_origins,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::Invalid {
                    name: "GEO_ALLOWED_ORIGINS",
                    value: "<non-unicode>".to_owned(),
                });
            }
        };
        Ok(Self {
            bind_addr,
            ready_on_start,
            dev_password,
            allowed_origins,
        })
    }

    pub fn database_url_configured() -> bool {
        env::var_os("DATABASE_URL").is_some()
    }

    pub fn validate_for_memory_mode(&self) -> Result<&str, ConfigError> {
        if !self.bind_addr.ip().is_loopback() {
            return Err(ConfigError::DevelopmentMustBindLoopback);
        }
        let password = self
            .dev_password
            .as_deref()
            .filter(|password| !password.is_empty())
            .ok_or(ConfigError::MissingDevelopmentPassword)?;
        Ok(password)
    }
}

fn parse_origins(value: String) -> Result<Vec<String>, ConfigError> {
    let origins = value
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if origins.is_empty() {
        return Err(ConfigError::Invalid {
            name: "GEO_ALLOWED_ORIGINS",
            value,
        });
    }
    Ok(origins)
}

fn env_bool(name: &'static str, default: bool) -> Result<bool, ConfigError> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(ConfigError::Invalid { name, value }),
        },
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::AppConfig;

    #[test]
    fn defaults_are_local_and_development_safe() {
        let config = AppConfig::default();
        assert_eq!(config.bind_addr.port(), 8080);
        assert!(config.ready_on_start);
        assert!(config.dev_password.is_none());
        assert!(config.bind_addr.ip().is_loopback());
    }
}
