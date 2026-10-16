use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, ObservationCompletionRecoveryBackfillItem,
    ObservationCompletionRecoveryClaim, ObservationCompletionRecoveryDueItem,
    ObservationCompletionRecoveryJobOutcome, ObservationCompletionRecoveryJobRepository,
    TenantScope, observation_completion_recovery_backoff,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use super::completion_recovery::{read_by_source, source};

#[derive(Clone)]
pub struct PgObservationCompletionRecoveryJobRepository {
    pool: PgPool,
}

impl PgObservationCompletionRecoveryJobRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

fn db(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "completion recovery queue unavailable",
    )
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

fn limit(value: usize) -> Result<i64, AppError> {
    if !(1..=100).contains(&value) {
        return Err(AppError::invalid_request(
            "recovery scan limit must be between 1 and 100",
        ));
    }
    Ok(value as i64)
}

fn row_scope(row: &PgRow) -> TenantScope {
    TenantScope::new(
        row.get::<Uuid, _>("operator_id").into(),
        row.get::<Uuid, _>("tenant_id").into(),
        Some(row.get::<Uuid, _>("project_id").into()),
    )
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    job: Uuid,
    lease: Uuid,
    attempt: i32,
    event: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO observation_completion_recovery_job_attempts \
         (event_id,operator_id,tenant_id,project_id,job_id,lease_id,attempt_number,event) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (job_id,lease_id,event) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(job)
    .bind(lease)
    .bind(attempt)
    .bind(event)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

#[async_trait]
impl ObservationCompletionRecoveryJobRepository for PgObservationCompletionRecoveryJobRepository {
    async fn scan_unqueued(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        page_size: usize,
    ) -> Result<Vec<ObservationCompletionRecoveryBackfillItem>, AppError> {
        // Coarse indexed discovery includes sources with missing immutable IDs:
        // enqueue records those once as retained rather than looping forever.
        let rows = sqlx::query(
            "SELECT o.operator_id,o.tenant_id,o.project_id,o.capture_id FROM observation_captures o \
             WHERE o.phase='source' AND o.ordinal=0 AND o.stored_at<=$1 \
             AND ($2::uuid IS NULL OR o.capture_id>$2) \
             AND o.input #>> '{owned_conversation,purpose}'='measurement' \
             AND o.input #>> '{owned_conversation,correlation}'='create_response' \
             AND (o.input->'completion' IS NULL OR o.input->'completion'='null'::jsonb) \
             AND NOT EXISTS (SELECT 1 FROM observation_completion_recovery_jobs j \
               WHERE j.operator_id=o.operator_id AND j.tenant_id=o.tenant_id \
               AND j.project_id=o.project_id AND j.source_capture_id=o.capture_id) \
             ORDER BY o.capture_id LIMIT $3",
        ).bind(as_of).bind(after).bind(limit(page_size)?)
            .fetch_all(&self.pool).await.map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ObservationCompletionRecoveryBackfillItem {
                scope: row_scope(&row),
                capture_id: row.get("capture_id"),
            })
            .collect())
    }

    async fn scan_due(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        page_size: usize,
    ) -> Result<Vec<ObservationCompletionRecoveryDueItem>, AppError> {
        let rows = sqlx::query(
            "SELECT operator_id,tenant_id,project_id,job_id FROM observation_completion_recovery_jobs \
             WHERE state NOT IN ('recovered','retained') AND created_at<=$1 AND next_attempt_at<=$1 \
             AND (lease_until IS NULL OR lease_until<=$1) \
             AND ($2::uuid IS NULL OR job_id>$2) ORDER BY job_id LIMIT $3",
        ).bind(as_of).bind(after).bind(limit(page_size)?)
            .fetch_all(&self.pool).await.map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ObservationCompletionRecoveryDueItem {
                scope: row_scope(&row),
                job_id: row.get("job_id"),
            })
            .collect())
    }

    async fn enqueue(&self, scope: &TenantScope, capture_id: Uuid) -> Result<Uuid, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        let original = source(&mut tx, scope, capture_id).await?;
        let state = if original.recovery_request_binding(scope).is_err() {
            "retained"
        } else if read_by_source(&mut tx, scope, capture_id).await?.is_some() {
            "recovered"
        } else {
            "pending"
        };
        sqlx::query(
            "INSERT INTO observation_completion_recovery_jobs \
             (job_id,operator_id,tenant_id,project_id,source_capture_id,source_receipt_sha256,state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (operator_id,tenant_id,project_id,source_capture_id) DO NOTHING",
        ).bind(Uuid::new_v4()).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(capture_id).bind(&original.receipt.digest_sha256).bind(state)
            .execute(&mut *tx).await.map_err(db)?;
        let id = sqlx::query_scalar(
            "SELECT job_id FROM observation_completion_recovery_jobs \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_capture_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(capture_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(id)
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
    ) -> Result<Option<ObservationCompletionRecoveryClaim>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        let Some(row) = sqlx::query(
            "SELECT * FROM observation_completion_recovery_jobs \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND job_id=$4 \
             AND state NOT IN ('recovered','retained') AND next_attempt_at<=clock_timestamp() \
             AND (lease_until IS NULL OR lease_until<=clock_timestamp()) FOR UPDATE SKIP LOCKED",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        else {
            return Ok(None);
        };
        let attempts: i32 = row.get("attempt_count");
        if let Some(old_lease) = row.get::<Option<Uuid>, _>("lease_id") {
            audit(&mut tx, scope, job_id, old_lease, attempts, "expired").await?;
        }
        let original = source(&mut tx, scope, row.get("source_capture_id")).await?;
        let binding = original.recovery_request_binding(scope);
        let state = if binding.is_err()
            || original.receipt.digest_sha256 != row.get::<String, _>("source_receipt_sha256")
        {
            Some("retained")
        } else if read_by_source(&mut tx, scope, original.input.capture_id)
            .await?
            .is_some()
        {
            Some("recovered")
        } else {
            None
        };
        if let Some(state) = state {
            sqlx::query(
                "UPDATE observation_completion_recovery_jobs SET state=$2,lease_id=NULL,lease_until=NULL, \
                 updated_at=clock_timestamp() WHERE job_id=$1",
            ).bind(job_id).bind(state).execute(&mut *tx).await.map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(None);
        }
        let request_binding = binding?;
        let lease_id = Uuid::new_v4();
        let lease_until = sqlx::query_scalar(
            "UPDATE observation_completion_recovery_jobs SET state='running',lease_id=$2, \
             lease_until=clock_timestamp()+interval '180 seconds',attempt_count=LEAST(attempt_count+1,1000000), \
             updated_at=clock_timestamp() WHERE job_id=$1 RETURNING lease_until",
        ).bind(job_id).bind(lease_id).fetch_one(&mut *tx).await.map_err(db)?;
        audit(
            &mut tx,
            scope,
            job_id,
            lease_id,
            (attempts + 1).min(1_000_000),
            "claimed",
        )
        .await?;
        tx.commit().await.map_err(db)?;
        Ok(Some(ObservationCompletionRecoveryClaim {
            job_id,
            lease_id,
            lease_until,
            original,
            request_binding,
        }))
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
        lease_id: Uuid,
        outcome: ObservationCompletionRecoveryJobOutcome,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        let row = sqlx::query(
            "SELECT * FROM observation_completion_recovery_jobs \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND job_id=$4 \
             AND state='running' AND lease_id=$5 AND lease_until>clock_timestamp() FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(job_id)
        .bind(lease_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("completion recovery lease is not current"))?;
        let original = source(&mut tx, scope, row.get("source_capture_id")).await?;
        // Every stored outcome revalidates authoritative immutable prerequisites.
        let valid = original.recovery_request_binding(scope).is_ok()
            && original.receipt.digest_sha256 == row.get::<String, _>("source_receipt_sha256");
        let state = if !valid {
            "retained"
        } else {
            match outcome {
                ObservationCompletionRecoveryJobOutcome::Recovered => {
                    read_by_source(&mut tx, scope, original.input.capture_id)
                        .await?
                        .ok_or_else(|| {
                            AppError::conflict("validated completion recovery evidence required")
                        })?;
                    "recovered"
                }
                ObservationCompletionRecoveryJobOutcome::Retry => "retry",
                ObservationCompletionRecoveryJobOutcome::NeedsLogin => "needs_login",
                ObservationCompletionRecoveryJobOutcome::Retained => "retained",
            }
        };
        let attempt: i32 = row.get("attempt_count");
        let backoff =
            observation_completion_recovery_backoff(attempt as u32, state == "needs_login");
        let changed = sqlx::query(
            "UPDATE observation_completion_recovery_jobs SET state=$3,lease_id=NULL,lease_until=NULL, \
             next_attempt_at=clock_timestamp()+($4::bigint * interval '1 second'),updated_at=clock_timestamp() \
             WHERE job_id=$1 AND lease_id=$2 AND lease_until>clock_timestamp()",
        ).bind(job_id).bind(lease_id).bind(state).bind(backoff)
            .execute(&mut *tx).await.map_err(db)?.rows_affected();
        if changed != 1 {
            return Err(AppError::conflict(
                "completion recovery lease is not current",
            ));
        }
        audit(&mut tx, scope, job_id, lease_id, attempt, state).await?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }
}
