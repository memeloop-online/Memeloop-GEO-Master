use std::{env, net::SocketAddr, str::FromStr};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    /// Development-only header scope extraction is intentionally explicit.
    pub dev_scope_headers: bool,
    pub ready_on_start: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            dev_scope_headers: true,
            ready_on_start: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid {name}: {value}")]
    Invalid { name: &'static str, value: String },
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
        let dev_scope_headers = env_bool("GEO_DEV_SCOPE_HEADERS", defaults.dev_scope_headers)?;
        let ready_on_start = env_bool("GEO_READY_ON_START", defaults.ready_on_start)?;
        Ok(Self {
            bind_addr,
            dev_scope_headers,
            ready_on_start,
        })
    }
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
        assert!(config.dev_scope_headers);
        assert!(config.ready_on_start);
    }
}
