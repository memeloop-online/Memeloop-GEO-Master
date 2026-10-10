//! Opt-in PostgreSQL model assembly. Only trusted deployment environment and
//! provisioned database grants select a gateway and credential.

use std::{env, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use geo_api::{
    ProviderRoute, ProviderRouteResolver, RoutedProviderClientBridge, SharedModelProvider,
};
use geo_domain::{ModelRouteGrant, TenantScope};
use geo_persistence::{Database, PgModelRouteRepository};
use geo_provider::{
    HttpTokenCenter, HttpTransport, ProviderError, ResolvedToken, SecretRef, TokenCenter,
    TokenCenterKeyMapping,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const NAMES: [&str; 5] = [
    "GEO_PRODUCTION_AI_BASE_URL",
    "GEO_TOKEN_CENTER_URL",
    "GEO_TOKEN_CENTER_TOKEN",
    "GEO_PRODUCTION_AGENT_BUNDLE_PATH",
    "GEO_PRODUCTION_AGENT_BUNDLE_SHA256",
];
const ROUTE_PREFIX: &str = "tenant-model-route";

#[derive(Clone)]
pub struct ProductionAiConfig {
    pub gateway_url: String,
    pub token_center_url: String,
    pub service_token: String,
    pub bundle_path: String,
    pub bundle_sha256: String,
}

impl fmt::Debug for ProductionAiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProductionAiConfig(***)")
    }
}

#[derive(Debug, Error)]
pub enum ProductionAiError {
    #[error("production model runtime requires all five production AI variables")]
    PartialConfiguration,
    #[error("invalid production model runtime configuration")]
    InvalidConfiguration,
}

impl ProductionAiConfig {
    pub fn from_env() -> Result<Option<Self>, ProductionAiError> {
        let mut values = Vec::with_capacity(NAMES.len());
        for name in NAMES {
            values.push(match env::var(name) {
                Ok(value) => Some(value),
                Err(env::VarError::NotPresent) => None,
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(ProductionAiError::InvalidConfiguration);
                }
            });
        }
        Self::parse(|name| {
            NAMES
                .iter()
                .position(|candidate| *candidate == name)
                .and_then(|index| values[index].clone())
        })
    }

    fn parse(
        mut get: impl FnMut(&'static str) -> Option<String>,
    ) -> Result<Option<Self>, ProductionAiError> {
        let values = NAMES.map(&mut get);
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        if values
            .iter()
            .any(|value| value.as_deref().is_none_or(str::is_empty))
        {
            return Err(ProductionAiError::PartialConfiguration);
        }
        let [
            Some(gateway_url),
            Some(token_center_url),
            Some(service_token),
            Some(bundle_path),
            Some(bundle_sha256),
        ] = values
        else {
            return Err(ProductionAiError::PartialConfiguration);
        };
        if bundle_sha256.len() != 64 || hex::decode(&bundle_sha256).is_err() {
            return Err(ProductionAiError::InvalidConfiguration);
        }
        // Constructors validate both endpoints without ever sending a token.
        let _ = geo_provider::ProviderClient::new(
            &gateway_url,
            SecretRef::new("configuration-probe")
                .map_err(|_| ProductionAiError::InvalidConfiguration)?,
            Arc::new(HttpTransport::new().map_err(|_| ProductionAiError::InvalidConfiguration)?),
            Arc::new(ConfigurationProbe),
        )
        .map_err(|_| ProductionAiError::InvalidConfiguration)?;
        let _ = HttpTokenCenter::new(&token_center_url, service_token.clone(), [])
            .map_err(|_| ProductionAiError::InvalidConfiguration)?;
        Ok(Some(Self {
            gateway_url,
            token_center_url,
            service_token,
            bundle_path,
            bundle_sha256,
        }))
    }
}

struct ConfigurationProbe;
#[async_trait]
impl TokenCenter for ConfigurationProbe {
    async fn resolve(&self, _: &SecretRef) -> Result<ResolvedToken, ProviderError> {
        Err(unavailable())
    }
}

fn unavailable() -> ProviderError {
    ProviderError::TokenUnavailable("model grant or credential unavailable".into())
}

/// Fingerprints every meaningful grant field. A concurrent edit between route
/// selection and credential resolution invalidates the in-flight selection.
fn reference(grant: &ModelRouteGrant) -> Result<SecretRef, ProviderError> {
    let mut hasher = Sha256::new();
    for field in [
        grant.scope.operator_id.to_string(),
        grant.scope.tenant_id.to_string(),
        grant
            .scope
            .project_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        grant.model.clone(),
        grant.is_default.to_string(),
        grant.enabled.to_string(),
        grant.tenant_external_id.clone(),
        grant.principal_external_id.clone(),
        grant.key_id.to_string(),
        grant.credential_generation.to_string(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    SecretRef::new(format!(
        "{ROUTE_PREFIX}:{}:{}",
        grant.route_id,
        hex::encode(hasher.finalize())
    ))
}

fn route_id(reference: &SecretRef) -> Result<Uuid, ProviderError> {
    let text = reference.as_str();
    let (prefix, rest) = text.split_once(':').ok_or_else(unavailable)?;
    let (id, fingerprint) = rest.split_once(':').ok_or_else(unavailable)?;
    if prefix != ROUTE_PREFIX
        || fingerprint.len() != 64
        || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(unavailable());
    }
    id.parse().map_err(|_| unavailable())
}

struct DurableRoutes {
    grants: PgModelRouteRepository,
    gateway_url: String,
}

struct DurableModelMetadata {
    grants: PgModelRouteRepository,
}

/// Metadata uses the same uncached default-grant query as inference. It does
/// not resolve Token Center credentials or probe an upstream model.
#[async_trait]
impl geo_api::InheritedModelMetadata for DurableModelMetadata {
    async fn default_model(
        &self,
        scope: &TenantScope,
    ) -> Result<Option<String>, geo_domain::AppError> {
        let grant = self
            .grants
            .resolve(scope, None)
            .await
            .map_err(|_| geo_domain::AppError::not_ready("model grant metadata unavailable"))?;
        Ok(default_model_metadata(scope, grant.as_ref()))
    }
}

fn default_model_metadata(scope: &TenantScope, grant: Option<&ModelRouteGrant>) -> Option<String> {
    grant
        .filter(|grant| {
            grant.scope.contains(scope) && grant.enabled && grant.is_default && grant.valid()
        })
        .map(|grant| grant.model.clone())
}

pub fn configure_model_metadata(state: &geo_api::AppState, database: &Database) {
    let metadata: Arc<dyn geo_api::InheritedModelMetadata> = Arc::new(DurableModelMetadata {
        grants: PgModelRouteRepository::from_database(database),
    });
    for usage in geo_domain::ProjectAiUsage::ALL {
        state
            .project_ai_settings()
            .with_inherited_metadata(usage, Arc::clone(&metadata));
    }
}

#[async_trait]
impl ProviderRouteResolver for DurableRoutes {
    async fn resolve(
        &self,
        scope: &TenantScope,
        requested_model: Option<&str>,
    ) -> Result<ProviderRoute, ProviderError> {
        let grant = self
            .grants
            .resolve(scope, requested_model)
            .await
            .map_err(|_| unavailable())?
            .filter(|grant| grant.enabled && grant.valid())
            .ok_or_else(unavailable)?;
        Ok(ProviderRoute {
            base_url: self.gateway_url.clone(),
            secret_ref: reference(&grant)?,
            model: grant.model,
        })
    }
}

struct DurableTokenCenter {
    grants: PgModelRouteRepository,
    token_center_url: String,
    service_token: String,
}

impl fmt::Debug for DurableTokenCenter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DurableTokenCenter(***)")
    }
}

#[async_trait]
impl TokenCenter for DurableTokenCenter {
    async fn resolve(&self, secret_ref: &SecretRef) -> Result<ResolvedToken, ProviderError> {
        let id = route_id(secret_ref)?;
        let grant = self
            .grants
            .current(id)
            .await
            .map_err(|_| unavailable())?
            .filter(|grant| grant.enabled && grant.valid())
            .ok_or_else(unavailable)?;
        if reference(&grant)?.as_str() != secret_ref.as_str() {
            return Err(unavailable());
        }
        // The HTTP adapter has only this single, DB-revalidated mapping for
        // this invocation; it cannot resolve an adjacent tenant's reference.
        let mapping = TokenCenterKeyMapping::new(
            secret_ref.clone(),
            grant.tenant_external_id,
            grant.principal_external_id,
            grant.key_id.to_string(),
        )?
        .with_generation(grant.credential_generation)?;
        HttpTokenCenter::new(&self.token_center_url, &self.service_token, [mapping])?
            .resolve(secret_ref)
            .await
    }
}

/// Share this scoped provider between chat runtime and any server-owned
/// content generation capability. An absent DB grant is never a success path.
pub fn build_model_provider(
    database: &Database,
    config: &ProductionAiConfig,
) -> Result<SharedModelProvider, ProductionAiError> {
    let grants = PgModelRouteRepository::from_database(database);
    let bridge = RoutedProviderClientBridge::new(
        Arc::new(HttpTransport::new().map_err(|_| ProductionAiError::InvalidConfiguration)?),
        Arc::new(DurableTokenCenter {
            grants: grants.clone(),
            token_center_url: config.token_center_url.clone(),
            service_token: config.service_token.clone(),
        }),
        Arc::new(DurableRoutes {
            grants,
            gateway_url: config.gateway_url.clone(),
        }),
        Duration::from_secs(60),
    )
    .map_err(|_| ProductionAiError::InvalidConfiguration)?;
    Ok(Arc::new(bridge))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_requires_a_valid_enabled_default_grant_in_the_requested_scope() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let mut grant = ModelRouteGrant {
            route_id: Uuid::new_v4(),
            scope: scope.clone(),
            model: "approved-default-model".into(),
            is_default: true,
            enabled: true,
            tenant_external_id: "synthetic-tenant".into(),
            principal_external_id: "synthetic-principal".into(),
            key_id: Uuid::new_v4(),
            credential_generation: 1,
        };
        assert_eq!(default_model_metadata(&scope, None), None);
        assert_eq!(
            default_model_metadata(&scope, Some(&grant)).as_deref(),
            Some("approved-default-model")
        );
        grant.enabled = false;
        assert_eq!(default_model_metadata(&scope, Some(&grant)), None);
        grant.enabled = true;
        grant.is_default = false;
        assert_eq!(default_model_metadata(&scope, Some(&grant)), None);
        grant.is_default = true;
        for forbidden in [
            TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
            TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
            TenantScope::new(
                scope.operator_id,
                scope.tenant_id,
                Some(Uuid::new_v4().into()),
            ),
            TenantScope::new(scope.operator_id, scope.tenant_id, None),
        ] {
            assert_eq!(default_model_metadata(&forbidden, Some(&grant)), None);
        }
        grant.scope.project_id = None;
        assert!(default_model_metadata(&scope, Some(&grant)).is_some());
        grant.credential_generation = 0;
        assert_eq!(default_model_metadata(&scope, Some(&grant)), None);
    }

    #[test]
    fn config_is_all_or_nothing_and_redacted() {
        assert!(ProductionAiConfig::parse(|_| None).unwrap().is_none());
        for omitted in NAMES {
            let result = ProductionAiConfig::parse(|name| {
                (name != omitted).then(|| match name {
                    "GEO_PRODUCTION_AI_BASE_URL" | "GEO_TOKEN_CENTER_URL" => {
                        "http://127.0.0.1:1/v1".into()
                    }
                    "GEO_PRODUCTION_AGENT_BUNDLE_SHA256" => "a".repeat(64),
                    _ => "test-only-secret".into(),
                })
            });
            assert!(matches!(
                result,
                Err(ProductionAiError::PartialConfiguration)
            ));
        }
        let config = ProductionAiConfig::parse(|name| {
            Some(match name {
                "GEO_PRODUCTION_AI_BASE_URL" | "GEO_TOKEN_CENTER_URL" => {
                    "http://127.0.0.1:1/v1".into()
                }
                "GEO_PRODUCTION_AGENT_BUNDLE_SHA256" => "a".repeat(64),
                _ => "test-only-secret".into(),
            })
        })
        .unwrap()
        .unwrap();
        assert!(!format!("{config:?}").contains("test-only-secret"));
    }

    #[test]
    fn edited_route_invalidates_inflight_reference() {
        let mut grant = ModelRouteGrant {
            route_id: Uuid::new_v4(),
            scope: TenantScope::new(Uuid::new_v4().into(), Uuid::new_v4().into(), None),
            model: "model-one".into(),
            is_default: true,
            enabled: true,
            tenant_external_id: "tenant".into(),
            principal_external_id: "subject".into(),
            key_id: Uuid::new_v4(),
            credential_generation: 1,
        };
        let before = reference(&grant).unwrap();
        grant.credential_generation = 2;
        assert_ne!(before, reference(&grant).unwrap());
        grant.credential_generation = 1;
        grant.enabled = false;
        assert_ne!(before, reference(&grant).unwrap());
        grant.enabled = true;
        grant.scope.tenant_id = Uuid::new_v4().into();
        assert_ne!(before, reference(&grant).unwrap());
    }
}
