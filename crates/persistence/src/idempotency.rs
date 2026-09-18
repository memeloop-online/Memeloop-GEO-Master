use async_trait::async_trait;
use geo_domain::{
    AppError, IdempotencyDecision, IdempotencyStore, IdempotencyToken, StoredResponse, TenantScope,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Transactional PostgreSQL idempotency store backed by the W01
/// `idempotency_records` table. The unique scope/key index and row lock make
/// concurrent first requests resolve to exactly one reservation.
#[derive(Clone)]
pub struct PgIdempotencyStore {
    pool: PgPool,
}

impl PgIdempotencyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl IdempotencyStore for PgIdempotencyStore {
    async fn begin(
        &self,
        scope: &TenantScope,
        key: &str,
        body_hash: &str,
    ) -> Result<IdempotencyDecision, AppError> {
        if key.trim().is_empty() {
            return Err(AppError::invalid_request(
                "Idempotency-Key must not be empty",
            ));
        }
        if body_hash.trim().is_empty() {
            return Err(AppError::invalid_request(
                "request body hash must not be empty",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(database_unavailable)?;
        crate::scope::set_local_scope(&mut transaction, scope)
            .await
            .map_err(database_unavailable)?;
        let project_id = scope.project_id.map(|id| id.as_uuid());
        let record_id = Uuid::new_v4();
        let inserted = sqlx::query(
            r#"INSERT INTO idempotency_records
                (idempotency_record_id, operator_id, tenant_id, project_id,
                 idempotency_key, request_hash, state)
               VALUES ($1, $2, $3, $4, $5, $6, 'in_flight')
               ON CONFLICT DO NOTHING"#,
        )
        .bind(record_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(key)
        .bind(body_hash)
        .execute(&mut *transaction)
        .await
        .map_err(database_unavailable)?
        .rows_affected()
            == 1;
        if inserted {
            transaction.commit().await.map_err(database_unavailable)?;
            return Ok(IdempotencyDecision::New(IdempotencyToken {
                scope: scope.storage_key(),
                key: key.to_owned(),
                body_hash: body_hash.to_owned(),
            }));
        }

        let row = sqlx::query(
            r#"SELECT request_hash, state, response_status, response_content_type,
                      response_body
               FROM idempotency_records
               WHERE operator_id = $1 AND tenant_id = $2
                 AND project_id IS NOT DISTINCT FROM $3
                 AND idempotency_key = $4
               FOR UPDATE"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_unavailable)?
        .ok_or_else(|| {
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "idempotency reservation disappeared",
            )
        })?;
        let existing_hash: String = row.try_get("request_hash").map_err(database_unavailable)?;
        if existing_hash != body_hash {
            return Err(AppError::conflict(
                "Idempotency-Key was already used with a different request body",
            ));
        }
        let state: String = row.try_get("state").map_err(database_unavailable)?;
        let result = match state.as_str() {
            "in_flight" => IdempotencyDecision::InFlight,
            "completed" => {
                let status: Option<i16> = row
                    .try_get("response_status")
                    .map_err(database_unavailable)?;
                let content_type: Option<String> = row
                    .try_get("response_content_type")
                    .map_err(database_unavailable)?;
                let body: Option<Vec<u8>> =
                    row.try_get("response_body").map_err(database_unavailable)?;
                let (Some(status), Some(content_type), Some(body)) = (status, content_type, body)
                else {
                    return Err(AppError::new(
                        geo_domain::ErrorCode::Internal,
                        "completed idempotency record has no response",
                    ));
                };
                IdempotencyDecision::Replay(StoredResponse {
                    status: u16::try_from(status).map_err(database_unavailable)?,
                    content_type,
                    body,
                })
            }
            _ => {
                return Err(AppError::new(
                    geo_domain::ErrorCode::Internal,
                    "invalid idempotency record state",
                ));
            }
        };
        transaction.commit().await.map_err(database_unavailable)?;
        Ok(result)
    }

    async fn complete(
        &self,
        token: &IdempotencyToken,
        response: StoredResponse,
    ) -> Result<(), AppError> {
        let (operator_id, tenant_id, project_id) = parse_scope(&token.scope)?;
        let mut transaction = self.pool.begin().await.map_err(database_unavailable)?;
        crate::scope::set_local_scope(
            &mut transaction,
            &TenantScope::new(
                operator_id.into(),
                tenant_id.into(),
                project_id.map(Into::into),
            ),
        )
        .await
        .map_err(database_unavailable)?;
        let row = sqlx::query(
            r#"SELECT request_hash, state
               FROM idempotency_records
               WHERE operator_id = $1 AND tenant_id = $2
                 AND project_id IS NOT DISTINCT FROM $3
                 AND idempotency_key = $4
               FOR UPDATE"#,
        )
        .bind(operator_id)
        .bind(tenant_id)
        .bind(project_id)
        .bind(&token.key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_unavailable)?
        .ok_or_else(|| {
            AppError::conflict("idempotency reservation is missing or belongs to another request")
        })?;
        let existing_hash: String = row.try_get("request_hash").map_err(database_unavailable)?;
        if existing_hash != token.body_hash {
            return Err(AppError::conflict(
                "idempotency reservation belongs to another request",
            ));
        }
        let state: String = row.try_get("state").map_err(database_unavailable)?;
        if state == "completed" {
            transaction.commit().await.map_err(database_unavailable)?;
            return Ok(());
        }
        if state != "in_flight" {
            return Err(AppError::new(
                geo_domain::ErrorCode::Internal,
                "invalid idempotency record state",
            ));
        }
        sqlx::query(
            r#"UPDATE idempotency_records
               SET state = 'completed', response_status = $1,
                   response_content_type = $2, response_body = $3,
                   completed_at = now(), updated_at = now()
               WHERE operator_id = $4 AND tenant_id = $5
                 AND project_id IS NOT DISTINCT FROM $6
                 AND idempotency_key = $7 AND request_hash = $8"#,
        )
        .bind(i16::try_from(response.status).map_err(database_unavailable)?)
        .bind(response.content_type)
        .bind(response.body)
        .bind(operator_id)
        .bind(tenant_id)
        .bind(project_id)
        .bind(&token.key)
        .bind(&token.body_hash)
        .execute(&mut *transaction)
        .await
        .map_err(database_unavailable)?;
        transaction.commit().await.map_err(database_unavailable)?;
        Ok(())
    }
}

fn parse_scope(scope: &str) -> Result<(Uuid, Uuid, Option<Uuid>), AppError> {
    let mut parts = scope.split(':');
    let operator_id = parts
        .next()
        .ok_or_else(|| AppError::invalid_request("invalid idempotency scope"))?
        .parse()
        .map_err(|_| AppError::invalid_request("invalid idempotency scope"))?;
    let tenant_id = parts
        .next()
        .ok_or_else(|| AppError::invalid_request("invalid idempotency scope"))?
        .parse()
        .map_err(|_| AppError::invalid_request("invalid idempotency scope"))?;
    let project_id = parts
        .next()
        .map(|value| {
            value
                .parse()
                .map_err(|_| AppError::invalid_request("invalid idempotency scope"))
        })
        .transpose()?;
    if parts.next().is_some() {
        return Err(AppError::invalid_request("invalid idempotency scope"));
    }
    Ok((operator_id, tenant_id, project_id))
}

fn database_unavailable(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        format!("idempotency persistence is unavailable: {error}"),
    )
}
