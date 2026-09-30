use std::{env, fmt, net::SocketAddr, str::FromStr};
use thiserror::Error;

const AI_ENV_NAMES: [&str; 5] = [
    "GEO_AI_BASE_URL",
    "GEO_AI_API_KEY",
    "GEO_AI_MODEL",
    "GEO_AGENT_BUNDLE_PATH",
    "GEO_AGENT_BUNDLE_SHA256",
];

#[derive(Clone)]
pub struct DevelopmentAiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub bundle_path: String,
    pub bundle_sha256: String,
}

impl fmt::Debug for DevelopmentAiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DevelopmentAiConfig(***)")
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub ready_on_start: bool,
    /// Enables startup reconciliation of persisted `running` runs. This is
    /// valid only when this process is the sole executor for the database.
    pub single_process_executor: bool,
    /// The in-memory adapter is only valid for an explicitly supplied local
    /// development password.  It is never a production fallback.
    pub dev_password: Option<String>,
    /// Exact browser origins accepted for login and state-changing requests.
    pub allowed_origins: Vec<String>,
    /// Process-wide credential is permitted only in loopback, in-memory dev.
    pub development_ai: Option<DevelopmentAiConfig>,
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppConfig")
            .field("bind_addr", &self.bind_addr)
            .field("ready_on_start", &self.ready_on_start)
            .field("single_process_executor", &self.single_process_executor)
            .field("dev_password_configured", &self.dev_password.is_some())
            .field("allowed_origins", &self.allowed_origins)
            .field("development_ai_configured", &self.development_ai.is_some())
            .finish()
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            ready_on_start: true,
            single_process_executor: false,
            dev_password: None,
            allowed_origins: vec![
                "http://localhost:5173".to_owned(),
                "http://127.0.0.1:5173".to_owned(),
                "http://localhost:8080".to_owned(),
                "http://127.0.0.1:8080".to_owned(),
            ],
            development_ai: None,
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
    #[error("AI runtime configuration requires all five GEO_AI_* and GEO_AGENT_BUNDLE_* variables")]
    PartialAiConfiguration,
    #[error("invalid AI runtime configuration: {0}")]
    InvalidAiConfiguration(&'static str),
    #[error("process-wide AI credentials require in-memory loopback development mode")]
    DevelopmentAiRequiresMemoryMode,
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
        let single_process_executor = env_bool(
            "GEO_SINGLE_PROCESS_EXECUTOR",
            defaults.single_process_executor,
        )?;
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
        let mut ai_values = Vec::with_capacity(AI_ENV_NAMES.len());
        for name in AI_ENV_NAMES {
            ai_values.push(match env::var(name) {
                Ok(value) => Some(value),
                Err(env::VarError::NotPresent) => None,
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(ConfigError::InvalidAiConfiguration(
                        "environment values must be Unicode",
                    ));
                }
            });
        }
        let development_ai = parse_development_ai(|name| {
            AI_ENV_NAMES
                .iter()
                .position(|candidate| *candidate == name)
                .and_then(|index| ai_values[index].clone())
        })?;
        Ok(Self {
            bind_addr,
            ready_on_start,
            single_process_executor,
            dev_password,
            allowed_origins,
            development_ai,
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

    pub fn validate_ai_mode(&self, durable_storage: bool) -> Result<(), ConfigError> {
        if self.development_ai.is_some() && (durable_storage || !self.bind_addr.ip().is_loopback())
        {
            return Err(ConfigError::DevelopmentAiRequiresMemoryMode);
        }
        Ok(())
    }
}

fn parse_development_ai(
    get: impl FnMut(&'static str) -> Option<String>,
) -> Result<Option<DevelopmentAiConfig>, ConfigError> {
    let values = AI_ENV_NAMES.map(get);
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    if values
        .iter()
        .any(|value| value.as_deref().is_none_or(str::is_empty))
    {
        return Err(ConfigError::PartialAiConfiguration);
    }
    let [
        Some(base_url),
        Some(api_key),
        Some(model),
        Some(bundle_path),
        Some(bundle_sha256),
    ] = values
    else {
        return Err(ConfigError::PartialAiConfiguration);
    };
    if bundle_sha256.len() != 64 || hex::decode(&bundle_sha256).is_err() {
        return Err(ConfigError::InvalidAiConfiguration(
            "bundle SHA-256 must be 64 hexadecimal characters",
        ));
    }
    if model.trim().is_empty() || model.len() > 256 || model.contains("://") {
        return Err(ConfigError::InvalidAiConfiguration(
            "model must be a routing identifier",
        ));
    }
    Ok(Some(DevelopmentAiConfig {
        base_url,
        api_key,
        model,
        bundle_path,
        bundle_sha256,
    }))
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
    use super::{AppConfig, ConfigError, parse_development_ai};

    #[test]
    fn defaults_are_local_and_development_safe() {
        let config = AppConfig::default();
        assert_eq!(config.bind_addr.port(), 8080);
        assert!(config.ready_on_start);
        assert!(
            !config.single_process_executor,
            "startup reconciliation must be opt-in so multi-replica deployments are safe by default"
        );
        assert!(config.dev_password.is_none());
        assert!(config.bind_addr.ip().is_loopback());
        assert!(config.development_ai.is_none());
    }

    #[test]
    fn ai_settings_are_all_or_nothing_and_redacted() {
        let names = [
            "GEO_AI_BASE_URL",
            "GEO_AI_API_KEY",
            "GEO_AI_MODEL",
            "GEO_AGENT_BUNDLE_PATH",
            "GEO_AGENT_BUNDLE_SHA256",
        ];
        assert!(parse_development_ai(|_| None).unwrap().is_none());
        for omitted in 0..names.len() {
            let result = parse_development_ai(|name| {
                names
                    .iter()
                    .position(|candidate| *candidate == name)
                    .and_then(|i| {
                        (i != omitted).then(|| {
                            if i == 4 {
                                "a".repeat(64)
                            } else {
                                "sensitive".into()
                            }
                        })
                    })
            });
            assert!(matches!(result, Err(ConfigError::PartialAiConfiguration)));
        }
        let config = parse_development_ai(|name| {
            Some(if name == names[4] {
                "a".repeat(64)
            } else {
                "sensitive".into()
            })
        })
        .unwrap()
        .unwrap();
        assert!(!format!("{config:?}").contains("sensitive"));
        let app = AppConfig {
            dev_password: Some("private-development-password".into()),
            development_ai: Some(config),
            ..AppConfig::default()
        };
        let debug = format!("{app:?}");
        assert!(!debug.contains("private-development-password"));
        assert!(!debug.contains("sensitive"));
    }

    #[test]
    fn process_wide_ai_key_cannot_run_with_durable_storage_or_public_bind() {
        let mut config = AppConfig {
            development_ai: parse_development_ai(|name| {
                Some(if name == "GEO_AGENT_BUNDLE_SHA256" {
                    "a".repeat(64)
                } else {
                    "test".into()
                })
            })
            .unwrap(),
            ..AppConfig::default()
        };
        assert!(config.validate_ai_mode(false).is_ok());
        assert!(matches!(
            config.validate_ai_mode(true),
            Err(ConfigError::DevelopmentAiRequiresMemoryMode)
        ));
        config.bind_addr = "0.0.0.0:8080".parse().unwrap();
        assert!(matches!(
            config.validate_ai_mode(false),
            Err(ConfigError::DevelopmentAiRequiresMemoryMode)
        ));
    }
}
