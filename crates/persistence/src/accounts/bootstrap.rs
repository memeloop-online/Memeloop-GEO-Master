//! Explicit, local-operator bootstrap of the first durable login identity.
//! This is not an HTTP repository operation and is never run by migrations.

use geo_domain::{Operator, OperatorId, Role, Tenant, TenantId, User, UserId, normalize_host};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BootstrapError {
    #[error("invalid bootstrap setting: {0}")]
    Invalid(&'static str),
    #[error("bootstrap identity conflicts with existing data")]
    Conflict,
    #[error("bootstrap database operation failed")]
    Database,
}

/// All identifiers are deployer-supplied and stable across retries. Debug
/// deliberately omits names, host, login, and the secret.
pub struct BootstrapIdentity {
    operator_id: OperatorId,
    operator_slug: String,
    operator_name: String,
    host: String,
    tenant_id: TenantId,
    tenant_slug: String,
    tenant_name: String,
    user_id: UserId,
    email: String,
    user_name: String,
    password: String,
    role: Role,
}

impl std::fmt::Debug for BootstrapIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BootstrapIdentity(<redacted>)")
    }
}

impl BootstrapIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operator_id: &str,
        operator_slug: &str,
        operator_name: &str,
        host: &str,
        tenant_id: &str,
        tenant_slug: &str,
        tenant_name: &str,
        user_id: &str,
        email: &str,
        user_name: &str,
        password: String,
        role: &str,
    ) -> Result<Self, BootstrapError> {
        fn id(value: &str) -> Result<Uuid, BootstrapError> {
            value
                .parse::<Uuid>()
                .ok()
                .filter(|id| !id.is_nil())
                .ok_or(BootstrapError::Invalid("UUID must be non-nil"))
        }
        let operator_id = OperatorId::from(id(operator_id)?);
        let tenant_id = TenantId::from(id(tenant_id)?);
        let user_id = UserId::from(id(user_id)?);
        let operator = Operator::new(operator_id, operator_slug, operator_name)
            .map_err(|_| BootstrapError::Invalid("operator name or slug"))?;
        let tenant = Tenant::new(tenant_id, operator_id, tenant_slug, tenant_name)
            .map_err(|_| BootstrapError::Invalid("tenant name or slug"))?;
        // Slugs are stable routing identifiers, not arbitrary display text.
        if !valid_slug(&operator.slug) || !valid_slug(&tenant.slug) {
            return Err(BootstrapError::Invalid("slug"));
        }
        if !valid_host(host) || normalize_host(host) != host {
            return Err(BootstrapError::Invalid("host"));
        }
        let role = match role {
            "resource_admin" => Role::ResourceAdmin,
            "customer_admin" => Role::CustomerAdmin,
            _ => return Err(BootstrapError::Invalid("role")),
        };
        // Hashing is deferred until insertion, but use the domain constructor
        // for validation and hashing rather than constructing SQL credentials.
        if password.len() < 12 || password.len() > 1024 {
            return Err(BootstrapError::Invalid("password length"));
        }
        let email = geo_domain::normalize_email(email)
            .map_err(|_| BootstrapError::Invalid("login name"))?;
        if email != email.trim().to_ascii_lowercase() {
            return Err(BootstrapError::Invalid("login name"));
        }
        if user_name.trim().is_empty() || user_name.chars().count() > 200 {
            return Err(BootstrapError::Invalid("user name"));
        }
        Ok(Self {
            operator_id,
            operator_slug: operator.slug,
            operator_name: operator.display_name,
            host: host.to_owned(),
            tenant_id,
            tenant_slug: tenant.slug,
            tenant_name: tenant.display_name,
            user_id,
            email,
            user_name: user_name.trim().to_owned(),
            password,
            role,
        })
    }
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 100
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.contains("://") || host.contains('@') {
        return false;
    }
    let (name, port) = match host.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    if port.is_some_and(|port| port.parse::<u16>().ok().filter(|p| *p > 0).is_none()) {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapOutcome {
    Created,
    Unchanged,
}

/// One transaction and a database-wide advisory lock serialize concurrent
/// invocations. Existing partial rows never receive new credentials or roles.
pub async fn bootstrap_identity(
    pool: &PgPool,
    input: &BootstrapIdentity,
) -> Result<BootstrapOutcome, BootstrapError> {
    let mut tx = pool.begin().await.map_err(|_| BootstrapError::Database)?;
    sqlx::query("SELECT pg_advisory_xact_lock(68204701)")
        .execute(&mut *tx)
        .await
        .map_err(|_| BootstrapError::Database)?;

    let operators = sqlx::query(
        "SELECT operator_id, slug, display_name FROM operators WHERE operator_id = $1 OR slug = $2",
    )
    .bind(input.operator_id.as_uuid())
    .bind(&input.operator_slug)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| BootstrapError::Database)?;
    let tenants = sqlx::query(
        "SELECT tenant_id, operator_id, slug, display_name FROM tenants WHERE tenant_id = $1 OR (operator_id = $2 AND slug = $3)",
    )
    .bind(input.tenant_id.as_uuid())
    .bind(input.operator_id.as_uuid())
    .bind(&input.tenant_slug)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| BootstrapError::Database)?;
    let hosts = sqlx::query("SELECT operator_id FROM operator_hosts WHERE host = $1")
        .bind(&input.host)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| BootstrapError::Database)?;
    let users = sqlx::query(
        "SELECT user_id, operator_id, email, display_name, password_hash, active FROM users WHERE user_id = $1 OR (operator_id = $2 AND email = $3)",
    )
    .bind(input.user_id.as_uuid())
    .bind(input.operator_id.as_uuid())
    .bind(&input.email)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| BootstrapError::Database)?;
    let memberships = sqlx::query(
        "SELECT membership_id, user_id, operator_id, tenant_id, role, active FROM memberships WHERE user_id = $1",
    )
    .bind(input.user_id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| BootstrapError::Database)?;

    let counts = [
        operators.len(),
        tenants.len(),
        hosts.len(),
        users.len(),
        memberships.len(),
    ];
    if counts == [0; 5] {
        let user = User::new(
            input.user_id,
            input.operator_id,
            &input.email,
            &input.user_name,
            &input.password,
        )
        .map_err(|_| BootstrapError::Invalid("user identity"))?;
        sqlx::query("INSERT INTO operators (operator_id, slug, display_name) VALUES ($1,$2,$3)")
            .bind(input.operator_id.as_uuid())
            .bind(&input.operator_slug)
            .bind(&input.operator_name)
            .execute(&mut *tx)
            .await
            .map_err(|_| BootstrapError::Database)?;
        sqlx::query("INSERT INTO operator_hosts (host, operator_id) VALUES ($1,$2)")
            .bind(&input.host)
            .bind(input.operator_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|_| BootstrapError::Database)?;
        sqlx::query(
            "INSERT INTO tenants (tenant_id, operator_id, slug, display_name) VALUES ($1,$2,$3,$4)",
        )
        .bind(input.tenant_id.as_uuid())
        .bind(input.operator_id.as_uuid())
        .bind(&input.tenant_slug)
        .bind(&input.tenant_name)
        .execute(&mut *tx)
        .await
        .map_err(|_| BootstrapError::Database)?;
        sqlx::query("INSERT INTO users (user_id, operator_id, email, display_name, password_hash) VALUES ($1,$2,$3,$4,$5)")
            .bind(input.user_id.as_uuid())
            .bind(input.operator_id.as_uuid())
            .bind(&input.email)
            .bind(&input.user_name)
            .bind(user.password_hash())
            .execute(&mut *tx)
            .await
            .map_err(|_| BootstrapError::Database)?;
        sqlx::query("INSERT INTO memberships (membership_id, user_id, operator_id, tenant_id, role) VALUES ($1,$2,$3,$4,$5)")
            .bind(Uuid::new_v4())
            .bind(input.user_id.as_uuid())
            .bind(input.operator_id.as_uuid())
            .bind(input.tenant_id.as_uuid())
            .bind(input.role.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|_| BootstrapError::Database)?;
        tx.commit().await.map_err(|_| BootstrapError::Database)?;
        return Ok(BootstrapOutcome::Created);
    }
    if counts != [1; 5] {
        return Err(BootstrapError::Conflict);
    }
    let operator = &operators[0];
    let tenant = &tenants[0];
    let host = &hosts[0];
    let user = &users[0];
    let membership = &memberships[0];
    macro_rules! col {
        ($row:expr, $name:literal, $ty:ty) => {
            $row.try_get::<$ty, _>($name)
                .map_err(|_| BootstrapError::Database)?
        };
    }
    if col!(operator, "operator_id", Uuid) != input.operator_id.as_uuid()
        || col!(operator, "slug", String) != input.operator_slug
        || col!(operator, "display_name", String) != input.operator_name
        || col!(tenant, "tenant_id", Uuid) != input.tenant_id.as_uuid()
        || col!(tenant, "operator_id", Uuid) != input.operator_id.as_uuid()
        || col!(tenant, "slug", String) != input.tenant_slug
        || col!(tenant, "display_name", String) != input.tenant_name
        || col!(host, "operator_id", Uuid) != input.operator_id.as_uuid()
        || col!(user, "user_id", Uuid) != input.user_id.as_uuid()
        || col!(user, "operator_id", Uuid) != input.operator_id.as_uuid()
        || col!(user, "email", String) != input.email
        || col!(user, "display_name", String) != input.user_name
        || !col!(user, "active", bool)
        || col!(membership, "user_id", Uuid) != input.user_id.as_uuid()
        || col!(membership, "operator_id", Uuid) != input.operator_id.as_uuid()
        || col!(membership, "tenant_id", Uuid) != input.tenant_id.as_uuid()
        || col!(membership, "role", String) != input.role.as_str()
        || !col!(membership, "active", bool)
    {
        return Err(BootstrapError::Conflict);
    }
    let stored = User::from_password_hash(
        input.user_id,
        input.operator_id,
        &input.email,
        &input.user_name,
        true,
        col!(user, "password_hash", String),
        chrono::Utc::now(),
        chrono::Utc::now(),
    )
    .map_err(|_| BootstrapError::Conflict)?;
    if !stored.verify_password(&input.password) {
        return Err(BootstrapError::Conflict);
    }
    tx.commit().await.map_err(|_| BootstrapError::Database)?;
    Ok(BootstrapOutcome::Unchanged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(host: &str, role: &str, password: &str) -> Result<BootstrapIdentity, BootstrapError> {
        BootstrapIdentity::new(
            "11111111-1111-4111-8111-111111111111",
            "operator",
            "Operator",
            host,
            "22222222-2222-4222-8222-222222222222",
            "tenant",
            "Tenant",
            "33333333-3333-4333-8333-333333333333",
            "admin@example.invalid",
            "Administrator",
            password.to_owned(),
            role,
        )
    }

    #[test]
    fn validates_exact_host_role_and_secret_without_debug_leaks() {
        let password = format!("{}{}", Uuid::new_v4(), Uuid::new_v4());
        let input = make("geo.example.invalid:8443", "resource_admin", &password).unwrap();
        assert_eq!(format!("{input:?}"), "BootstrapIdentity(<redacted>)");
        assert!(!format!("{input:?}").contains(&password));
        for host in [
            "GEO.example.invalid",
            "geo.example.invalid/",
            "https://geo.example.invalid",
            "geo.example.invalid:0",
            "geo.example.invalid:8443:9",
            " geo.example.invalid",
        ] {
            assert!(make(host, "resource_admin", &password).is_err());
        }
        assert!(make("geo.example.invalid", "operator", &password).is_err());
        assert!(make("geo.example.invalid", "customer_admin", &password).is_ok());
        assert!(make("geo.example.invalid", "customer_admin", "short").is_err());
    }
}
