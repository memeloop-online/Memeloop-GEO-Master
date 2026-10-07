use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelAttempt, ChannelCycleInputs, ChannelDispatchCandidate, ChannelJobRepository,
    ChannelOutcome, ChannelPlan, ChannelSecret, ChannelTarget, ChannelTargetInput,
    ChannelTargetView, ErrorCode, OperatorId, ProjectId, StandaloneMeasurementPlan, TenantId,
    TenantScope, frozen_cycle_inputs,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
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

async fn bind_generated_claim(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    target: &ChannelTarget,
    attempt_id: Uuid,
    at: DateTime<Utc>,
) -> Result<(), AppError> {
    let ChannelTargetInput::GeneratedPublish {
        publication_intent_id,
        ..
    } = &target.input
    else {
        return Ok(());
    };
    let command: Option<(Uuid, bool)> = sqlx::query_as(
        "UPDATE distribution_publication_commands SET status='claimed' \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4 \
           AND command_id=$5 AND materialized_target_id=$5 AND status='pending' \
         RETURNING command_id,fixture",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(publication_intent_id)
    .bind(target.target_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    let (command_id, fixture) =
        command.ok_or_else(|| AppError::conflict("generated publication already claimed"))?;
    sqlx::query(
        "INSERT INTO distribution_publication_attempts \
          (attempt_id,operator_id,tenant_id,project_id,intent_id,command_id,claimed_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(attempt_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(publication_intent_id)
    .bind(command_id)
    .bind(at)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    // The pre-send record is explicitly unknown. A timeout/crash after claim
    // cannot be interpreted as safe-to-retry or as verified publication.
    sqlx::query(
        "INSERT INTO distribution_intent_evidence \
          (evidence_id,operator_id,tenant_id,project_id,intent_id,result,attempt_id,fixture,observed_at) \
         VALUES ($1,$2,$3,$4,$5,'unknown',$1,$6,$7)",
    )
    .bind(attempt_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(publication_intent_id)
    .bind(fixture)
    .bind(at)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    let body: serde_json::Value = sqlx::query_scalar(
        "SELECT body FROM distribution_publication_intents \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(publication_intent_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    let mut intent: geo_domain::PublicationIntent = decode(body)?;
    if intent.verification != geo_domain::IntentVerification::Unverified {
        return Err(AppError::conflict(
            "publication intent has already been attempted",
        ));
    }
    intent.verification = geo_domain::IntentVerification::Unknown;
    intent.verification_evidence_id = Some(attempt_id);
    sqlx::query(
        "UPDATE distribution_publication_intents SET verification='unknown', \
          verification_evidence_id=$1,body=$2 \
         WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND intent_id=$6",
    )
    .bind(attempt_id)
    .bind(encode(&intent)?)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(publication_intent_id)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

fn owned_verified_readback(input: &ChannelTargetInput, outcome: &ChannelOutcome) -> bool {
    let ChannelTargetInput::GeneratedPublish {
        platform,
        title,
        body,
        ..
    } = input
    else {
        return false;
    };
    let Some(url) = outcome.public_url.as_deref() else {
        return false;
    };
    let Some(post_id) = url
        .strip_prefix("https://www.zhihu.com/p/")
        .or_else(|| url.strip_prefix("https://zhuanlan.zhihu.com/p/"))
    else {
        return false;
    };
    if platform != "zhihu"
        || post_id.is_empty()
        || !post_id.bytes().all(|byte| byte.is_ascii_digit())
        || outcome.fixture
        || outcome.status != geo_domain::ChannelOutcomeStatus::Verified
    {
        return false;
    }
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let hash = hex::encode(Sha256::digest(
        format!("{}\n{}", normalize(title), normalize(body)).as_bytes(),
    ));
    outcome.runner_evidence.iter().any(|proof| {
        proof.get("kind").and_then(|v| v.as_str()) == Some("public_readback")
            && proof.get("url").and_then(|v| v.as_str()) == Some(url)
            && proof.get("content_matched").and_then(|v| v.as_bool()) == Some(true)
            && proof.get("owned_by_account").and_then(|v| v.as_bool()) == Some(true)
            && proof.get("expected_sha256").and_then(|v| v.as_str()) == Some(hash.as_str())
            && proof.get("readback_sha256").and_then(|v| v.as_str()) == Some(hash.as_str())
    })
}

#[async_trait]
impl ChannelJobRepository for PgChannelJobRepository {
    async fn replay_measurement_plan(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<StandaloneMeasurementPlan>, AppError> {
        let prior: Option<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT request_hash,plan FROM measurement_execution_plans WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND idempotency_key=$4"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(key).fetch_optional(&self.pool).await.map_err(db)?;
        match prior {
            Some((hash, json)) if hash == request_hash => decode(json).map(Some),
            Some(_) => Err(AppError::conflict("measurement idempotency key differs")),
            None => Ok(None),
        }
    }

    async fn create_measurement_plan(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
        plan: StandaloneMeasurementPlan,
    ) -> Result<StandaloneMeasurementPlan, AppError> {
        plan.validate(scope)?;
        if key.trim().is_empty() || request_hash.is_empty() {
            return Err(AppError::invalid_request(
                "measurement idempotency identity required",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        // Serialize first inserts as well as retries; locking a missing plan row
        // cannot protect concurrent uses of the same idempotency key.
        let exists: Option<Uuid> = sqlx::query_scalar(
            "SELECT project_id FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        if exists.is_none() {
            return Err(AppError::not_found("project not found"));
        }
        let prior: Option<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT request_hash,plan FROM measurement_execution_plans WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND idempotency_key=$4"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(key).fetch_optional(&mut *tx).await.map_err(db)?;
        if let Some((hash, json)) = prior {
            return if hash == request_hash {
                decode(json)
            } else {
                Err(AppError::conflict("measurement idempotency key differs"))
            };
        }
        sqlx::query(
            "INSERT INTO measurement_execution_plans (plan_id,operator_id,tenant_id,project_id,idempotency_key,request_hash,input_hash,revision,plan,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)"
        ).bind(plan.plan_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(key).bind(request_hash).bind(&plan.input_hash)
            .bind(plan.revision).bind(encode(&plan)?).bind(plan.created_at)
            .execute(&mut *tx).await.map_err(db)?;
        for (ordinal, target) in plan.targets.iter().enumerate() {
            let ordinal = i32::try_from(ordinal)
                .map_err(|_| AppError::invalid_request("too many targets"))?;
            sqlx::query(
                "INSERT INTO channel_execution_targets (target_id,operator_id,tenant_id,project_id,measurement_plan_id,kind,frozen_input,ordinal) VALUES ($1,$2,$3,$4,$5,'measure',$6,$7)"
            ).bind(target.target_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?).bind(plan.plan_id).bind(encode(target)?).bind(ordinal)
                .execute(&mut *tx).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(plan)
    }

    async fn get_measurement_plan(
        &self,
        scope: &TenantScope,
        plan_id: Uuid,
    ) -> Result<Option<StandaloneMeasurementPlan>, AppError> {
        let json: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT plan FROM measurement_execution_plans WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND plan_id=$4"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(plan_id).fetch_optional(&self.pool).await.map_err(db)?;
        json.map(decode).transpose()
    }

    async fn list_measurement_plans(
        &self,
        scope: &TenantScope,
        after_plan_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<StandaloneMeasurementPlan>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid measurement page size"));
        }
        let cursor_at: Option<DateTime<Utc>> = if let Some(cursor) = after_plan_id {
            Some(
                sqlx::query_scalar(
                    "SELECT created_at FROM measurement_execution_plans \
                 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND plan_id=$4",
                )
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?)
                .bind(cursor)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?
                .ok_or_else(|| AppError::invalid_request("invalid measurement cursor"))?,
            )
        } else {
            None
        };
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT plan FROM measurement_execution_plans WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND ($4::timestamptz IS NULL OR (created_at, plan_id) < ($4, $5::uuid)) \
             ORDER BY created_at DESC, plan_id DESC LIMIT $6"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(cursor_at).bind(after_plan_id).bind(limit as i64).fetch_all(&self.pool).await.map_err(db)?;
        rows.into_iter().map(decode).collect()
    }

    async fn list_optimization_measurement_plans(
        &self,
        scope: &TenantScope,
        after_plan_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<StandaloneMeasurementPlan>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid measurement page size"));
        }
        // An excluded-only plan cannot become an AI cursor or reveal its position.
        let cursor_at: Option<DateTime<Utc>> = if let Some(cursor) = after_plan_id {
            Some(sqlx::query_scalar(
                "SELECT created_at FROM measurement_execution_plans \
                 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND plan_id=$4 \
                 AND EXISTS (SELECT 1 FROM jsonb_array_elements(plan->'targets') AS target \
                   WHERE target->'input'->>'kind'='measure' AND target->'input'->'question_binding'->>'purpose'='optimization')",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?)
            .bind(cursor)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::invalid_request("invalid measurement cursor"))?)
        } else {
            None
        };
        // Filter eligibility in SQL before LIMIT, even when many newer plans
        // contain only frozen/unknown questions.
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT plan FROM measurement_execution_plans WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND ($4::timestamptz IS NULL OR (created_at, plan_id) < ($4, $5::uuid)) \
             AND EXISTS (SELECT 1 FROM jsonb_array_elements(plan->'targets') AS target \
               WHERE target->'input'->>'kind'='measure' AND target->'input'->'question_binding'->>'purpose'='optimization') \
             ORDER BY created_at DESC, plan_id DESC LIMIT $6"
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(cursor_at).bind(after_plan_id).bind(limit as i64).fetch_all(&self.pool).await.map_err(db)?;
        rows.into_iter().map(decode).collect()
    }

    async fn materialize_pending_commands(
        &self,
        after_command_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ChannelDispatchCandidate>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid command page size"));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let rows = sqlx::query(
            "SELECT c.command_id,c.operator_id,c.tenant_id,c.project_id,c.intent_id,\
                    c.origin_target_id,c.origin_request_id,c.payload_hash,\
                    i.body AS intent_body,v.body AS variant_body,r.body AS revision_body,\
                    t.current_body AS distribution_body,m.cycle_id, \
                    q.publication_intent_id AS request_intent_id,q.content_revision_id AS request_revision_id, \
                    q.platform_id AS request_platform_id,q.placement_slot AS request_slot, \
                    q.account_id AS request_account_id,q.format AS request_format, \
                    i.origin_target_id AS intent_origin_target_id,i.origin_request_id AS intent_origin_request_id \
             FROM distribution_publication_commands c \
             JOIN distribution_publication_intents i ON (i.operator_id,i.tenant_id,i.project_id,i.intent_id) = \
                (c.operator_id,c.tenant_id,c.project_id,c.intent_id) \
             JOIN distribution_channel_variants v ON (v.operator_id,v.tenant_id,v.project_id,v.variant_id) = \
                (i.operator_id,i.tenant_id,i.project_id,i.variant_id) \
             JOIN content_revisions r ON (r.operator_id,r.tenant_id,r.project_id,r.revision_id) = \
                (v.operator_id,v.tenant_id,v.project_id,v.content_revision_id) \
             LEFT JOIN distribution_execution_targets t ON (t.operator_id,t.tenant_id,t.project_id,t.target_id) = \
                (c.operator_id,c.tenant_id,c.project_id,c.origin_target_id) \
             LEFT JOIN distribution_execution_manifests m ON (m.operator_id,m.tenant_id,m.project_id,m.manifest_id) = \
                (t.operator_id,t.tenant_id,t.project_id,t.manifest_id) \
             LEFT JOIN content_distribution_requests q ON (q.operator_id,q.tenant_id,q.project_id,q.request_id) = \
                (c.operator_id,c.tenant_id,c.project_id,c.origin_request_id) \
             WHERE c.materialized_target_id IS NULL AND c.status='pending' \
                AND ($1::uuid IS NULL OR c.command_id>$1) \
             ORDER BY c.command_id LIMIT $2 FOR UPDATE OF c SKIP LOCKED",
        )
        .bind(after_command_id)
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let mut created = Vec::with_capacity(rows.len());
        for row in rows {
            let command_id: Uuid = row.get("command_id");
            let intent_id: Uuid = row.get("intent_id");
            let origin_target_id: Option<Uuid> = row.get("origin_target_id");
            let origin_request_id: Option<Uuid> = row.get("origin_request_id");
            let operator: Uuid = row.get("operator_id");
            let tenant: Uuid = row.get("tenant_id");
            let project_id: Uuid = row.get("project_id");
            let scope = TenantScope::new(
                OperatorId::new(operator),
                TenantId::new(tenant),
                Some(ProjectId::new(project_id)),
            );
            let intent: geo_domain::PublicationIntent = decode(row.get("intent_body"))?;
            let variant: geo_domain::ChannelVariant = decode(row.get("variant_body"))?;
            let revision: geo_domain::ContentRevision = decode(row.get("revision_body"))?;
            let payload_hash: String = row.get("payload_hash");
            if intent.intent_id != intent_id
                || origin_target_id.is_some() == origin_request_id.is_some()
                || row.get::<Option<Uuid>, _>("intent_origin_target_id") != origin_target_id
                || row.get::<Option<Uuid>, _>("intent_origin_request_id") != origin_request_id
                || intent.channel_target_id != origin_target_id.unwrap_or(Uuid::nil())
                || intent.project_id.as_uuid() != project_id
                || intent.variant_id != variant.variant_id
                || intent.content_revision_id != revision.revision_id
                || variant.content_revision_id != revision.revision_id
                || variant.platform_id != intent.platform_id
                || variant.payload_hash != payload_hash
                || intent.payload_hash != payload_hash
            {
                return Err(AppError::conflict(
                    "distribution command dependencies differ",
                ));
            }
            if let Some(target_id) = origin_target_id {
                let coverage: geo_domain::DistributionTarget =
                    decode(row.get::<serde_json::Value, _>("distribution_body"))?;
                if coverage.target_id != target_id
                    || coverage.publication_intent_id != Some(intent_id)
                    || coverage.variant_id != Some(variant.variant_id)
                    || coverage.account_id != Some(intent.account_id)
                    || coverage.content_revision_id != Some(revision.revision_id)
                    || coverage.platform_id != variant.platform_id
                    || row.get::<Option<Uuid>, _>("cycle_id").is_none()
                {
                    return Err(AppError::conflict("covered publication origin differs"));
                }
            } else if row.get::<Option<Uuid>, _>("request_intent_id") != Some(intent_id)
                || row.get::<Option<Uuid>, _>("request_revision_id") != Some(revision.revision_id)
                || row.get::<Option<Uuid>, _>("request_account_id") != Some(intent.account_id)
                || row
                    .get::<Option<String>, _>("request_platform_id")
                    .as_deref()
                    != Some(variant.platform_id.as_str())
                || row.get::<Option<String>, _>("request_slot").as_deref()
                    != Some(variant.placement_slot.as_str())
                || row.get::<Option<String>, _>("request_format").as_deref()
                    != Some(geo_domain::TEXT_DISTRIBUTION_FORMAT)
                || row.get::<Option<Uuid>, _>("cycle_id").is_some()
            {
                return Err(AppError::conflict("requested publication origin differs"));
            }
            let target = ChannelTarget {
                target_id: command_id,
                input: ChannelTargetInput::GeneratedPublish {
                    content_revision_id: revision.revision_id,
                    variant_id: variant.variant_id,
                    publication_intent_id: intent_id,
                    distribution_target_id: origin_target_id.unwrap_or(Uuid::nil()),
                    origin_request_id,
                    platform: variant.platform_id,
                    account_id: intent.account_id,
                    title: variant.title,
                    body_sha256: hex::encode(Sha256::digest(variant.markdown.as_bytes())),
                    body: variant.markdown,
                    payload_hash,
                    evidence: variant.evidence,
                },
            };
            sqlx::query(
                "INSERT INTO channel_execution_targets \
                    (target_id,operator_id,tenant_id,project_id,cycle_id,plan_id,publication_intent_id,kind,frozen_input,ordinal) \
                 VALUES ($1,$2,$3,$4,$5,NULL,$6,'publish',$7,NULL)",
            )
            .bind(command_id)
            .bind(operator)
            .bind(tenant)
            .bind(project_id)
            .bind(row.get::<Option<Uuid>, _>("cycle_id"))
            .bind(intent_id)
            .bind(encode(&target)?)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            let updated = sqlx::query(
                "UPDATE distribution_publication_commands SET materialized_target_id=$1,materialized_at=clock_timestamp() \
                 WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND command_id=$5 \
                    AND intent_id=$6 AND materialized_target_id IS NULL",
            )
            .bind(command_id)
            .bind(operator)
            .bind(tenant)
            .bind(project_id)
            .bind(command_id)
            .bind(intent_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            if updated.rows_affected() != 1 {
                return Err(AppError::conflict(
                    "distribution command already materialized",
                ));
            }
            created.push(ChannelDispatchCandidate {
                scope,
                target_id: command_id,
            });
        }
        tx.commit().await.map_err(db)?;
        Ok(created)
    }

    async fn insert_generated_target(
        &self,
        _scope: &TenantScope,
        _cycle_id: Uuid,
        _command_id: Uuid,
        _target: ChannelTarget,
    ) -> Result<ChannelTarget, AppError> {
        Err(AppError::invalid_request(
            "generated PostgreSQL targets require atomic outbox materialization",
        ))
    }
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
            ChannelTargetInput::Publish { .. } | ChannelTargetInput::GeneratedPublish { .. } => {
                "publish"
            }
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
        bind_generated_claim(&mut tx, scope, &target, attempt_id, at).await?;
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
        if plan
            .targets
            .iter()
            .any(|target| matches!(target.input, ChannelTargetInput::GeneratedPublish { .. }))
        {
            return Err(AppError::invalid_request(
                "generated publication cannot be frozen into a legacy plan",
            ));
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
                ChannelTargetInput::Publish { .. }
                | ChannelTargetInput::GeneratedPublish { .. } => "publish",
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
            ChannelTargetInput::Publish { .. } | ChannelTargetInput::GeneratedPublish { .. } => {
                "publish"
            }
            ChannelTargetInput::Measure { .. } => "measure",
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        let result = sqlx::query(
            "INSERT INTO channel_execution_attempts (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"
        ).bind(attempt_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(target_id).bind(target.input.account_id()).bind(kind).bind(at)
            .execute(&mut *tx).await.map_err(db)?;
        bind_generated_claim(&mut tx, scope, &target, attempt_id, at).await?;
        tx.commit().await.map_err(db)?;
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
        let target = self.get_target(scope, target_id).await?.target;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let updated = sqlx::query(
            "UPDATE channel_execution_attempts SET outcome=$1,received_at=$2 WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND target_id=$6 AND attempt_id=$7 AND outcome IS NULL"
        ).bind(encode(&outcome)?).bind(received_at).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(target_id).bind(attempt_id).execute(&mut *tx).await.map_err(db)?;
        if updated.rows_affected() != 0
            && let ChannelTargetInput::GeneratedPublish {
                publication_intent_id,
                ..
            } = &target.input
        {
            let fixture: bool = sqlx::query_scalar(
                    "SELECT fixture FROM distribution_publication_commands \
                     WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
                       AND intent_id=$4 AND materialized_target_id=$5 AND status='claimed' FOR UPDATE",
                )
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?)
                .bind(publication_intent_id)
                .bind(target_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .ok_or_else(|| AppError::conflict("generated command was not claimed"))?;
            let verified = !fixture && owned_verified_readback(&target.input, &outcome);
            if verified {
                let receipt = outcome
                    .public_url
                    .as_deref()
                    .expect("verified readback URL");
                let evidence_id = Uuid::new_v4();
                sqlx::query(
                        "INSERT INTO distribution_intent_evidence \
                          (evidence_id,operator_id,tenant_id,project_id,intent_id,result,attempt_id, \
                           external_receipt,public_readback,fixture,observed_at) \
                         VALUES ($1,$2,$3,$4,$5,'verified',$6,$7,$8,false,$9)",
                    )
                    .bind(evidence_id)
                    .bind(scope.operator_id.as_uuid())
                    .bind(scope.tenant_id.as_uuid())
                    .bind(project(scope)?)
                    .bind(publication_intent_id)
                    .bind(attempt_id)
                    .bind(serde_json::json!({"external_receipt_id":receipt}))
                    .bind(serde_json::json!({"public_url":receipt,"verified":true,
                        "runner_evidence":outcome.runner_evidence}))
                    .bind(outcome.occurred_at)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                let body: serde_json::Value = sqlx::query_scalar(
                        "SELECT body FROM distribution_publication_intents \
                         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4 FOR UPDATE",
                    )
                    .bind(scope.operator_id.as_uuid())
                    .bind(scope.tenant_id.as_uuid())
                    .bind(project(scope)?)
                    .bind(publication_intent_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db)?;
                let mut intent: geo_domain::PublicationIntent = decode(body)?;
                intent.verification = geo_domain::IntentVerification::Verified;
                intent.verification_evidence_id = Some(evidence_id);
                sqlx::query(
                    "UPDATE distribution_publication_intents SET verification='verified', \
                          verification_evidence_id=$1,body=$2 \
                         WHERE operator_id=$3 AND tenant_id=$4 AND project_id=$5 AND intent_id=$6",
                )
                .bind(evidence_id)
                .bind(encode(&intent)?)
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?)
                .bind(publication_intent_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
            if !fixture
                && !outcome.fixture
                && (outcome.status == geo_domain::ChannelOutcomeStatus::Published || verified)
            {
                sqlx::query(
                    "UPDATE distribution_publication_commands SET status='delivered' \
                         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
                           AND intent_id=$4 AND materialized_target_id=$5",
                )
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?)
                .bind(publication_intent_id)
                .bind(target_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
        }
        tx.commit().await.map_err(db)?;
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

    async fn store_publication_binding(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        binding: ChannelSecret,
    ) -> Result<(), AppError> {
        if binding.encrypted_bytes().is_empty() {
            return Err(AppError::invalid_request(
                "empty encrypted publication binding",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        let row: Option<(String, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT targets.kind,attempts.outcome \
             FROM channel_execution_attempts attempts \
             JOIN channel_execution_targets targets \
               ON targets.operator_id=attempts.operator_id \
              AND targets.tenant_id=attempts.tenant_id \
              AND targets.project_id=attempts.project_id \
              AND targets.target_id=attempts.target_id \
             WHERE attempts.operator_id=$1 AND attempts.tenant_id=$2 \
               AND attempts.project_id=$3 AND attempts.target_id=$4 \
               AND attempts.attempt_id=$5 \
             FOR UPDATE OF attempts",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(target_id)
        .bind(attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let (kind, outcome) = row.ok_or_else(|| AppError::not_found("attempt not found"))?;
        if kind != "publish" {
            return Err(AppError::invalid_request(
                "binding requires a publication target",
            ));
        }
        let previous: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT encrypted_binding FROM publication_execution_bindings \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
               AND target_id=$4 AND attempt_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(target_id)
        .bind(attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if let Some(previous) = previous {
            return if previous == binding.encrypted_bytes() {
                Ok(())
            } else {
                Err(AppError::conflict("publication binding already recorded"))
            };
        }
        if outcome.is_some() {
            return Err(AppError::conflict("publication attempt already finished"));
        }
        sqlx::query(
            "INSERT INTO publication_execution_bindings \
             (attempt_id,operator_id,tenant_id,project_id,target_id,encrypted_binding) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(attempt_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(target_id)
        .bind(binding.encrypted_bytes())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn get_publication_binding(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<ChannelSecret>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT targets.kind \
             FROM channel_execution_attempts attempts \
             JOIN channel_execution_targets targets \
               ON targets.operator_id=attempts.operator_id \
              AND targets.tenant_id=attempts.tenant_id \
              AND targets.project_id=attempts.project_id \
              AND targets.target_id=attempts.target_id \
             WHERE attempts.operator_id=$1 AND attempts.tenant_id=$2 \
               AND attempts.project_id=$3 AND attempts.target_id=$4 \
               AND attempts.attempt_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(target_id)
        .bind(attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if kind.as_deref() != Some("publish") {
            return match kind {
                Some(_) => Err(AppError::invalid_request(
                    "binding requires a publication target",
                )),
                None => Err(AppError::not_found("attempt not found")),
            };
        }
        let encrypted: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT encrypted_binding FROM publication_execution_bindings \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
               AND target_id=$4 AND attempt_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(target_id)
        .bind(attempt_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        Ok(encrypted.map(ChannelSecret::new))
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

#[cfg(test)]
mod tests {
    use super::*;
    use geo_domain::ChannelOutcomeStatus;

    #[test]
    fn generated_verification_requires_owned_matching_non_fixture_readback() {
        let input = ChannelTargetInput::GeneratedPublish {
            content_revision_id: Uuid::new_v4(),
            variant_id: Uuid::new_v4(),
            publication_intent_id: Uuid::new_v4(),
            distribution_target_id: Uuid::new_v4(),
            origin_request_id: None,
            platform: "zhihu".into(),
            account_id: Uuid::new_v4(),
            title: "Frozen title".into(),
            body: "Frozen body".into(),
            body_sha256: hex::encode(Sha256::digest(b"Frozen body")),
            payload_hash: "immutable".into(),
            evidence: vec![],
        };
        let hash = hex::encode(Sha256::digest(b"Frozen title\nFrozen body"));
        let url = "https://zhuanlan.zhihu.com/p/12345";
        let mut outcome = ChannelOutcome {
            status: ChannelOutcomeStatus::Verified,
            detail: None,
            occurred_at: Utc::now(),
            raw_answer: None,
            citations: vec![],
            public_url: Some(url.into()),
            screenshot_ref: None,
            connector_version: None,
            runner_evidence: vec![serde_json::json!({
                "kind":"public_readback", "url":url, "content_matched":true,
                "owned_by_account":true,"expected_sha256":hash,"readback_sha256":hash
            })],
            fixture: false,
        };
        assert!(owned_verified_readback(&input, &outcome));
        outcome.fixture = true;
        assert!(!owned_verified_readback(&input, &outcome));
        outcome.fixture = false;
        outcome.runner_evidence[0]["owned_by_account"] = serde_json::json!(false);
        assert!(!owned_verified_readback(&input, &outcome));
        outcome.runner_evidence[0]["owned_by_account"] = serde_json::json!(true);
        outcome.runner_evidence[0]["readback_sha256"] = serde_json::json!("different");
        assert!(!owned_verified_readback(&input, &outcome));
    }
}
