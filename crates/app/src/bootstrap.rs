//! One-shot, local command entry point. No HTTP route or server listener.

use geo_persistence::{
    Database, DatabaseConfig,
    bootstrap::{BootstrapError, BootstrapIdentity, BootstrapOutcome, bootstrap_identity},
};
use std::env;

/// All values must be explicitly supplied by deployment environment or secret
/// manager. The password is only accepted from the process environment.
pub async fn run_from_env() -> Result<BootstrapOutcome, BootstrapError> {
    fn required(name: &'static str) -> Result<String, BootstrapError> {
        env::var(name).map_err(|_| BootstrapError::Invalid(name))
    }
    let operator_id = required("GEO_BOOTSTRAP_OPERATOR_ID")?;
    let operator_slug = required("GEO_BOOTSTRAP_OPERATOR_SLUG")?;
    let operator_name = required("GEO_BOOTSTRAP_OPERATOR_NAME")?;
    let host = required("GEO_BOOTSTRAP_HOST")?;
    let tenant_id = required("GEO_BOOTSTRAP_TENANT_ID")?;
    let tenant_slug = required("GEO_BOOTSTRAP_TENANT_SLUG")?;
    let tenant_name = required("GEO_BOOTSTRAP_TENANT_NAME")?;
    let user_id = required("GEO_BOOTSTRAP_USER_ID")?;
    let email = required("GEO_BOOTSTRAP_LOGIN_NAME")?;
    let user_name = required("GEO_BOOTSTRAP_USER_NAME")?;
    let password = required("GEO_BOOTSTRAP_PASSWORD")?;
    let role = required("GEO_BOOTSTRAP_ROLE")?;
    let identity = BootstrapIdentity::new(
        &operator_id,
        &operator_slug,
        &operator_name,
        &host,
        &tenant_id,
        &tenant_slug,
        &tenant_name,
        &user_id,
        &email,
        &user_name,
        password,
        &role,
    )?;
    let config = DatabaseConfig::from_env().map_err(|_| BootstrapError::Database)?;
    let database = Database::connect_and_migrate(&config)
        .await
        .map_err(|_| BootstrapError::Database)?;
    bootstrap_identity(database.pool(), &identity).await
}
