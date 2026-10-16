//! Administration adapter: existing audited mutations and scoped metadata reads.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, PartnerAdminConnection, PartnerAdminConnectionPage, PartnerAdminIssuedKey,
    PartnerAdminKey, PartnerAdminKeyPage, PartnerAdminRepository, PartnerIssuedKey, TenantScope,
};
use sqlx::{
    PgPool, Postgres, Row,
    postgres::{PgArguments, PgRow},
    query::Query,
};
use uuid::Uuid;

use crate::{PgPartnerRepository, set_local_scope};

#[derive(Clone)]
pub struct PgPartnerAdminRepository {
    pool: PgPool,
    inner: PgPartnerRepository,
}

fn db(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "partner administration storage unavailable",
    )
}

fn unavailable() -> AppError {
    AppError::not_found("partner resource not found")
}

fn mutation_error(error: AppError) -> AppError {
    if matches!(error.code, ErrorCode::Forbidden | ErrorCode::NotFound) {
        unavailable()
    } else {
        error
    }
}

fn scoped<'q>(
    sql: &'q str,
    scope: &TenantScope,
) -> Result<Query<'q, Postgres, PgArguments>, AppError> {
    let project = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    Ok(sqlx::query(sql)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid()))
}

fn page_limit(limit: u32) -> Result<i64, AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request(
            "page size must be between 1 and 100",
        ));
    }
    Ok(i64::from(limit))
}

fn connection(row: &PgRow) -> PartnerAdminConnection {
    PartnerAdminConnection {
        connection_id: row.get("connection_id"),
        knowledge_write: row.get("knowledge_write"),
        created_at: row.get("created_at"),
        revoked_at: row.get("revoked_at"),
    }
}

fn key(row: &PgRow) -> PartnerAdminKey {
    PartnerAdminKey {
        key_id: row.get("key_id"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
        revoked_at: row.get("revoked_at"),
    }
}

impl PgPartnerAdminRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
            inner: PgPartnerRepository::from_database(database),
        }
    }

    async fn read_connection(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<PartnerAdminConnection, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        let row = scoped(
            "SELECT connection_id,knowledge_write,created_at,revoked_at FROM partner_connections \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4",
            scope,
        )?
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(unavailable)?;
        tx.commit().await.map_err(db)?;
        Ok(connection(&row))
    }

    async fn issued(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        issued: PartnerIssuedKey,
    ) -> Result<PartnerAdminIssuedKey, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        let row = scoped(
            "SELECT key_id,created_at,expires_at,revoked_at FROM partner_keys \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 AND key_id=$5",
            scope,
        )?
        .bind(connection)
        .bind(issued.key_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(unavailable)?;
        tx.commit().await.map_err(db)?;
        Ok(PartnerAdminIssuedKey {
            key: key(&row),
            bearer: issued.bearer(),
        })
    }
}

#[async_trait]
impl PartnerAdminRepository for PgPartnerAdminRepository {
    async fn list_connections(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<PartnerAdminConnectionPage, AppError> {
        let limit = page_limit(limit)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        let rows = scoped(
            "SELECT connection_id,knowledge_write,created_at,revoked_at FROM partner_connections \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND ($4::uuid IS NULL OR connection_id>$4) ORDER BY connection_id LIMIT $5",
            scope,
        )?
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let has_more = rows.len() > limit as usize;
        let items: Vec<_> = rows.iter().take(limit as usize).map(connection).collect();
        let next_cursor = if has_more {
            items.last().map(|item| item.connection_id)
        } else {
            None
        };
        tx.commit().await.map_err(db)?;
        Ok(PartnerAdminConnectionPage { items, next_cursor })
    }

    async fn create_connection(
        &self,
        scope: &TenantScope,
    ) -> Result<PartnerAdminConnection, AppError> {
        let id = self.inner.create_connection(scope).await?;
        self.read_connection(scope, id).await
    }

    async fn list_keys(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<PartnerAdminKeyPage, AppError> {
        let limit = page_limit(limit)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        // A nonexistent or foreign connection must not masquerade as an empty list.
        scoped(
            "SELECT connection_id FROM partner_connections \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4",
            scope,
        )?
        .bind(connection)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(unavailable)?;
        let rows = scoped(
            "SELECT key_id,created_at,expires_at,revoked_at FROM partner_keys \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
             AND ($5::uuid IS NULL OR key_id>$5) ORDER BY key_id LIMIT $6",
            scope,
        )?
        .bind(connection)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let has_more = rows.len() > limit as usize;
        let items: Vec<_> = rows.iter().take(limit as usize).map(key).collect();
        let next_cursor = if has_more {
            items.last().map(|item| item.key_id)
        } else {
            None
        };
        tx.commit().await.map_err(db)?;
        Ok(PartnerAdminKeyPage { items, next_cursor })
    }

    async fn issue_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<PartnerAdminIssuedKey, AppError> {
        let issued = self
            .inner
            .issue_key(scope, connection, expires_at)
            .await
            .map_err(mutation_error)?;
        self.issued(scope, connection, issued).await
    }

    async fn rotate_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        old_key: Uuid,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<PartnerAdminIssuedKey, AppError> {
        let issued = self
            .inner
            .rotate_key(scope, connection, old_key, expires_at)
            .await
            .map_err(mutation_error)?;
        self.issued(scope, connection, issued).await
    }

    async fn revoke_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        key: Uuid,
    ) -> Result<(), AppError> {
        self.inner
            .revoke_key(scope, connection, key)
            .await
            .map_err(mutation_error)
    }

    async fn revoke_connection(
        &self,
        scope: &TenantScope,
        connection: Uuid,
    ) -> Result<(), AppError> {
        self.inner
            .revoke_connection(scope, connection)
            .await
            .map_err(mutation_error)
    }
}
