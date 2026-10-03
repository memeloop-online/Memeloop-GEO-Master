use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelAttempt, ChannelCycleInputs, ChannelDispatchCandidate, ChannelJobRepository,
    ChannelOutcome, ChannelPlan, ChannelTarget, ChannelTargetInput, ChannelTargetView, ErrorCode,
    OperatorId, ProjectId, TenantId, TenantScope, frozen_cycle_inputs,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgChannelJobRepository {
    pool: PgPool,
}

impl PgChannelJobRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

fn db(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .is_some_and(|db| db.is_unique_violation())
    {
        AppError::conflict("channel target or attempt already exists")
    } else {
        AppError::new(
            ErrorCode::DependencyUnavailable,
            "channel ledger unavailable",
        )
    }
}

fn decode<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(json)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored channel ledger invalid"))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "channel ledger encoding failed"))
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

#[async_trait]
impl ChannelJobRepository for PgChannelJobRepository {
    async fn reserve_account(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        reservation_id: Uuid,
        at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        project(scope)?;
        if expires_at <= at || expires_at - at > chrono::Duration::minutes(5) {
            return Err(AppError::invalid_request(
                "invalid account reservation duration",
            ));
        }
        let reserved = sqlx::query(
            "INSERT INTO channel_account_preflight_reservations \
             (operator_id,account_id,reservation_id,expires_at) VALUES ($1,$2,$3,$4) \
             ON CONFLICT (operator_id,account_id) DO UPDATE \
               SET reservation_id=EXCLUDED.reservation_id,expires_at=EXCLUDED.expires_at \
             WHERE channel_account_preflight_reservations.expires_at <= $5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(account_id)
        .bind(reservation_id)
        .bind(expires_at)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if reserved.rows_affected() == 0 {
            return Err(AppError::conflict("channel account preflight busy"));
        }
        Ok(())
    }

    async fn release_account(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        reservation_id: Uuid,
    ) -> Result<(), AppError> {
        project(scope)?;
        sqlx::query(
            "DELETE FROM channel_account_preflight_reservations \
             WHERE operator_id=$1 AND account_id=$2 AND reservation_id=$3",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(account_id)
        .bind(reservation_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn claim_reserved(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        reservation_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<(ChannelTarget, ChannelAttempt), AppError> {
        let target = self.get_target(scope, target_id).await?.target;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let lease: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
            "SELECT reservation_id,expires_at FROM channel_account_preflight_reservations \
             WHERE operator_id=$1 AND account_id=$2 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(target.input.account_id())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if !lease.is_some_and(|(owner, expiry)| owner == reservation_id && expiry > at) {
            return Err(AppError::conflict("account preflight reservation expired"));
        }
        let kind = match target.input {
            ChannelTargetInput::Publish { .. } => "publish",
            ChannelTargetInput::Measure { .. } => "measure",
        };
        sqlx::query(
            "INSERT INTO channel_execution_attempts \
             (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(attempt_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(target_id)
        .bind(target.input.account_id())
        .bind(kind)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok((
            target,
            ChannelAttempt {
                attempt_id,
                target_id,
                claimed_at: at,
                outcome: None,
                received_at: None,
            },
        ))
    }
    async fn scan_pending(
        &self,
        after_target_id: Option<Uuid>,
        as_of: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ChannelDispatchCandidate>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request(
                "invalid channel dispatch page size",
            ));
        }
        let rows = sqlx::query(
            "SELECT targets.operator_id,targets.tenant_id,targets.project_id,targets.target_id \
             FROM channel_execution_targets targets \
             JOIN projects ON projects.operator_id=targets.operator_id \
               AND projects.tenant_id=targets.tenant_id AND projects.project_id=targets.project_id \
             WHERE ($1::uuid IS NULL OR targets.target_id > $1) \
               AND projects.status NOT IN ('paused','archived') \
               AND (targets.kind='publish' OR (targets.frozen_input->'input'->>'scheduled_at')::timestamptz <= $2) \
               AND NOT EXISTS (SELECT 1 FROM channel_execution_attempts attempts \
                 WHERE attempts.operator_id=targets.operator_id AND attempts.tenant_id=targets.tenant_id \
                   AND attempts.project_id=targets.project_id AND attempts.target_id=targets.target_id) \
             ORDER BY targets.target_id LIMIT $3",
        )
        .bind(after_target_id)
        .bind(as_of)
        .bind(i64::try_from(limit).map_err(|_| AppError::invalid_request("invalid page size"))?)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ChannelDispatchCandidate {
                scope: TenantScope::new(
                    OperatorId::new(row.get("operator_id")),
                    TenantId::new(row.get("tenant_id")),
                    Some(ProjectId::new(row.get("project_id"))),
                ),
                target_id: row.get("target_id"),
            })
            .collect())
    }
    async fn create_plan(
        &self,
        scope: &TenantScope,
        plan: ChannelPlan,
    ) -> Result<ChannelPlan, AppError> {
        if scope.project_id != Some(plan.project_id) {
            return Err(AppError::forbidden("plan outside project"));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let prior: Option<(String,serde_json::Value)> = sqlx::query_as(
            "SELECT input_hash,plan FROM channel_execution_plans WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 FOR UPDATE"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(plan.cycle_id).fetch_optional(&mut *tx).await.map_err(db)?;
        if let Some((hash, json)) = prior {
            return if hash == plan.input_hash {
                decode(json)
            } else {
                Err(AppError::conflict("channel plan already frozen"))
            };
        }
        sqlx::query(
            "INSERT INTO channel_execution_plans (plan_id,operator_id,tenant_id,project_id,cycle_id,input_hash,revision,plan,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)"
        ).bind(plan.plan_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(plan.cycle_id).bind(&plan.input_hash).bind(plan.revision)
            .bind(encode(&plan)?).bind(plan.created_at).execute(&mut *tx).await.map_err(db)?;
        for (ordinal, target) in plan.targets.iter().enumerate() {
            let ordinal = i32::try_from(ordinal)
                .map_err(|_| AppError::invalid_request("too many targets"))?;
            let kind = match target.input {
                ChannelTargetInput::Publish { .. } => "publish",
                ChannelTargetInput::Measure { .. } => "measure",
            };
            sqlx::query(
                "INSERT INTO channel_execution_targets (target_id,operator_id,tenant_id,project_id,cycle_id,plan_id,kind,frozen_input,ordinal) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)"
            ).bind(target.target_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?).bind(plan.cycle_id).bind(plan.plan_id).bind(kind)
                .bind(encode(target)?).bind(ordinal).execute(&mut *tx).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(plan)
    }

    async fn get_plan(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<Option<ChannelPlan>, AppError> {
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT plan FROM channel_execution_plans WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(cycle_id).fetch_optional(&self.pool).await.map_err(db)?;
        json.map(decode).transpose()
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<(ChannelTarget, ChannelAttempt), AppError> {
        let target = self.get_target(scope, target_id).await?.target;
        let kind = match target.input {
            ChannelTargetInput::Publish { .. } => "publish",
            ChannelTargetInput::Measure { .. } => "measure",
        };
        let result = sqlx::query(
            "INSERT INTO channel_execution_attempts (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"
        ).bind(attempt_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(target_id).bind(target.input.account_id()).bind(kind).bind(at)
            .execute(&self.pool).await.map_err(db)?;
        debug_assert_eq!(result.rows_affected(), 1);
        Ok((
            target,
            ChannelAttempt {
                attempt_id,
                target_id,
                claimed_at: at,
                outcome: None,
                received_at: None,
            },
        ))
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        outcome: ChannelOutcome,
        received_at: DateTime<Utc>,
    ) -> Result<ChannelTargetView, AppError> {
        let updated = sqlx::query(
            "UPDATE channel_execution_attempts SET outcome=$1,received_at=$2 WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND target_id=$6 AND attempt_id=$7 AND outcome IS NULL"
        ).bind(encode(&outcome)?).bind(received_at).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(target_id).bind(attempt_id).execute(&self.pool).await.map_err(db)?;
        let view = self.get_target(scope, target_id).await?;
        let attempt = view
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == attempt_id)
            .ok_or_else(|| AppError::not_found("attempt not found"))?;
        if updated.rows_affected() == 0 && attempt.outcome.as_ref() != Some(&outcome) {
            return Err(AppError::conflict("attempt outcome already recorded"));
        }
        Ok(view)
    }

    async fn get_target(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
    ) -> Result<ChannelTargetView, AppError> {
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT frozen_input FROM channel_execution_targets WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(target_id).fetch_optional(&self.pool).await.map_err(db)?;
        let target: ChannelTarget =
            decode(json.ok_or_else(|| AppError::not_found("target not found"))?)?;
        let rows = sqlx::query(
            "SELECT attempt_id,claimed_at,outcome,received_at FROM channel_execution_attempts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 ORDER BY claimed_at,attempt_id"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(target_id).fetch_all(&self.pool).await.map_err(db)?;
        let attempts = rows
            .into_iter()
            .map(|row| {
                let json: Option<serde_json::Value> = row.get("outcome");
                Ok(ChannelAttempt {
                    attempt_id: row.get("attempt_id"),
                    target_id,
                    claimed_at: row.get("claimed_at"),
                    outcome: json.map(decode).transpose()?,
                    received_at: row.get("received_at"),
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        Ok(ChannelTargetView { target, attempts })
    }

    async fn cycle_inputs(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        as_of: DateTime<Utc>,
    ) -> Result<ChannelCycleInputs, AppError> {
        let Some(plan) = self.get_plan(scope, cycle_id).await? else {
            return Ok(ChannelCycleInputs {
                manifests: vec![],
                publications: None,
                measurements: None,
            });
        };
        if plan.created_at > as_of {
            return Ok(ChannelCycleInputs {
                manifests: vec![],
                publications: None,
                measurements: None,
            });
        }
        let rows = sqlx::query(
            "SELECT attempts.target_id,attempts.attempt_id,attempts.claimed_at,attempts.outcome,attempts.received_at FROM channel_execution_attempts attempts JOIN channel_execution_targets targets ON targets.target_id=attempts.target_id AND targets.operator_id=attempts.operator_id AND targets.tenant_id=attempts.tenant_id AND targets.project_id=attempts.project_id WHERE attempts.operator_id=$1 AND attempts.tenant_id=$2 AND attempts.project_id=$3 AND targets.cycle_id=$4 AND attempts.claimed_at<=$5 ORDER BY attempts.claimed_at,attempts.attempt_id"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(cycle_id).bind(as_of).fetch_all(&self.pool).await.map_err(db)?;
        let mut attempts: HashMap<Uuid, Vec<ChannelAttempt>> = HashMap::new();
        for row in rows {
            let target_id: Uuid = row.get("target_id");
            let json: Option<serde_json::Value> = row.get("outcome");
            attempts.entry(target_id).or_default().push(ChannelAttempt {
                target_id,
                attempt_id: row.get("attempt_id"),
                claimed_at: row.get("claimed_at"),
                outcome: json.map(decode).transpose()?,
                received_at: row.get("received_at"),
            });
        }
        Ok(frozen_cycle_inputs(&plan, &attempts, as_of))
    }
}
