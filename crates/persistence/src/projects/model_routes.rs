//! Durable, uncached model grants. Trusted deployment SQL provisions rows;
//! ordinary inference has read-only access to these mappings.

use geo_domain::{ModelRouteGrant, TenantScope};
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
#[error("model route storage unavailable")]
pub struct ModelRouteStorageError;

#[derive(Clone)]
pub struct PgModelRouteRepository {
    pool: PgPool,
}

type RouteRow = (
    Uuid,         // route_id
    Uuid,         // operator_id
    Uuid,         // tenant_id
    Option<Uuid>, // project_id
    String,       // model
    bool,         // is_default
    bool,         // enabled
    String,       // tenant_external_id
    String,       // principal_external_id
    Uuid,         // key_id
    i64,          // credential_generation
);

fn grant(row: RouteRow) -> ModelRouteGrant {
    ModelRouteGrant {
        route_id: row.0,
        scope: TenantScope::new(row.1.into(), row.2.into(), row.3.map(Into::into)),
        model: row.4,
        is_default: row.5,
        enabled: row.6,
        tenant_external_id: row.7,
        principal_external_id: row.8,
        key_id: row.9,
        credential_generation: row.10,
    }
}

impl PgModelRouteRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    /// The project-specific row takes precedence even when disabled, so a
    /// revoked project grant can never fall through to a tenant-wide grant.
    /// A missing requested model is a hard denial, never an arbitrary default.
    pub async fn resolve(
        &self,
        scope: &TenantScope,
        requested_model: Option<&str>,
    ) -> Result<Option<ModelRouteGrant>, ModelRouteStorageError> {
        let row: Option<RouteRow> = sqlx::query_as(
            "SELECT route_id,operator_id,tenant_id,project_id,model,is_default,enabled,\
                    tenant_external_id,principal_external_id,key_id,credential_generation \
             FROM tenant_model_routes \
             WHERE operator_id=$1 AND tenant_id=$2 \
               AND (project_id=$3 OR project_id IS NULL) \
               AND (($4::text IS NOT NULL AND model=$4) \
                 OR ($4::text IS NULL AND is_default=true)) \
             ORDER BY project_id IS NOT NULL DESC LIMIT 1",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|id| id.as_uuid()))
        .bind(requested_model)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ModelRouteStorageError)?;
        Ok(row.map(grant))
    }

    /// Re-read immediately before credential resolution. Disabling, deleting,
    /// rotating or editing a grant must not leave an indefinitely usable
    /// process cache. The returned grant is not a credential.
    pub async fn current(
        &self,
        route_id: Uuid,
    ) -> Result<Option<ModelRouteGrant>, ModelRouteStorageError> {
        let row: Option<RouteRow> = sqlx::query_as(
            "SELECT route_id,operator_id,tenant_id,project_id,model,is_default,enabled,\
                    tenant_external_id,principal_external_id,key_id,credential_generation \
             FROM tenant_model_routes WHERE route_id=$1",
        )
        .bind(route_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ModelRouteStorageError)?;
        Ok(row.map(grant))
    }
}
