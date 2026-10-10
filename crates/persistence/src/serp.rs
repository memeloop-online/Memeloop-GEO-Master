//! Scoped SERP state, write-once send intents, and immutable raw/parsed evidence.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::*;
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgSerpRepository {
    pool: PgPool,
}

fn unavailable(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "search observation store unavailable",
    )
}
fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}
fn encode(value: &impl Serialize) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value).map_err(|_| AppError::invalid_request("invalid search record"))
}
fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored search record invalid"))
}
fn page_limit(limit: usize) -> Result<i64, AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request("invalid search page size"));
    }
    Ok(limit as i64)
}
fn state_name(state: SerpTaskState) -> &'static str {
    match state {
        SerpTaskState::Queued => "queued",
        SerpTaskState::Claimed => "claimed",
        SerpTaskState::Sending => "sending",
        SerpTaskState::AwaitingResult => "awaiting_result",
        SerpTaskState::Completed => "completed",
        SerpTaskState::Unknown => "unknown",
        SerpTaskState::Failed => "failed",
        SerpTaskState::Cancelled => "cancelled",
    }
}

#[derive(Clone)]
struct Stored {
    measurement: SerpMeasurement,
    claim: Option<SerpClaim>,
    intent: Option<SerpSendingIntent>,
    task: Option<SerpProviderTask>,
    next_poll_at: Option<DateTime<Utc>>,
}

fn validate_binding_evidence(
    stored: &Stored,
    intent: &SerpSendingIntent,
    task: &SerpProviderTask,
    raw: &SerpStoredRaw,
    recovery: bool,
) -> Result<(), AppError> {
    stored.intent(intent)?;
    task.validate_binding(intent)?;
    raw.evidence.validate(intent)?;
    let expected = if recovery {
        SerpEvidenceOperation::RecoveryRead
    } else {
        SerpEvidenceOperation::Submission
    };
    if raw.evidence.operation != expected
        || !raw.evidence.body_complete
        || raw.evidence.send_certainty != SerpSendCertainty::ResponseReceived
        || !raw
            .evidence
            .http_status
            .is_some_and(|code| (200..300).contains(&code))
        || (recovery
            && raw.evidence.provider_task_id.as_deref() != Some(task.provider_task_id.as_str()))
    {
        return Err(AppError::conflict("search binding evidence differs"));
    }
    if stored.task.as_ref().is_some_and(|prior| {
        if recovery {
            prior.provider_task_id != task.provider_task_id
                || prior.correlation_tag != task.correlation_tag
                || prior.attempt_id != task.attempt_id
                || prior.measurement_id != task.measurement_id
        } else {
            prior != task
        }
    }) {
        return Err(AppError::conflict("search provider task already bound"));
    }
    Ok(())
}

fn validate_observation_evidence(
    stored: &Stored,
    observation: &SerpObservation,
    raw: &SerpStoredRaw,
) -> Result<(), AppError> {
    observation.validate_source(&stored.measurement, raw)?;
    if !matches!(
        raw.evidence.operation,
        SerpEvidenceOperation::ResultRead | SerpEvidenceOperation::RecoveryRead
    ) || stored
        .task
        .as_ref()
        .map(|task| task.provider_task_id.as_str())
        != raw.evidence.provider_task_id.as_deref()
    {
        return Err(AppError::conflict("search observation evidence differs"));
    }
    Ok(())
}
impl Stored {
    fn execution(&self) -> SerpExecution {
        SerpExecution {
            measurement_id: self.measurement.measurement_id,
            claim: self.claim.clone(),
            intent: self.intent.clone(),
            provider_task: self.task.clone(),
            next_poll_at: self.next_poll_at,
        }
    }
    fn due(&self, now: DateTime<Utc>) -> bool {
        self.measurement.created_at <= now
            && self
                .claim
                .as_ref()
                .is_none_or(|claim| claim.lease_expires_at <= now)
            && match self.measurement.state {
                SerpTaskState::Queued => self.measurement.scheduled_at <= now,
                SerpTaskState::AwaitingResult | SerpTaskState::Unknown => {
                    self.next_poll_at.is_none_or(|due| due <= now)
                }
                _ => false,
            }
    }
    fn from_row(row: PgRow) -> Result<Self, AppError> {
        Ok(Self {
            measurement: decode(row.get("measurement"))?,
            claim: row
                .get::<Option<serde_json::Value>, _>("claim")
                .map(decode)
                .transpose()?,
            intent: row
                .get::<Option<serde_json::Value>, _>("sending_intent")
                .map(decode)
                .transpose()?,
            task: row
                .get::<Option<serde_json::Value>, _>("provider_task")
                .map(decode)
                .transpose()?,
            next_poll_at: row.get("next_poll_at"),
        })
    }
    fn fence(&self, supplied: &SerpClaim, now: DateTime<Utc>) -> Result<&SerpClaim, AppError> {
        let stored = self
            .claim
            .as_ref()
            .ok_or_else(|| AppError::conflict("search claim missing"))?;
        if stored.attempt_id != supplied.attempt_id
            || stored.claim_token != supplied.claim_token
            || supplied.measurement.measurement_id != self.measurement.measurement_id
        {
            return Err(AppError::conflict("search claim differs"));
        }
        stored.validate_live(now)?;
        Ok(stored)
    }
    fn intent(&self, supplied: &SerpSendingIntent) -> Result<(), AppError> {
        supplied.validate()?;
        if self.intent.as_ref() != Some(supplied) {
            return Err(AppError::conflict("search sending intent differs"));
        }
        Ok(())
    }
}

impl PgSerpRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
    async fn tx(&self, scope: &TenantScope) -> Result<Transaction<'_, Postgres>, AppError> {
        project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        Ok(tx)
    }
    async fn locked(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Stored, AppError> {
        let row = sqlx::query("SELECT * FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4 FOR UPDATE")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
            .fetch_optional(&mut **tx).await.map_err(unavailable)?
            .ok_or_else(|| AppError::not_found("search measurement not found"))?;
        Stored::from_row(row)
    }
    async fn save(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        stored: &Stored,
    ) -> Result<(), AppError> {
        if let Some(claim) = &stored.claim {
            sqlx::query("INSERT INTO serp_claims (claim_token,operator_id,tenant_id,project_id,measurement_id,attempt_id,claim) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (claim_token) DO NOTHING")
                .bind(claim.claim_token).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project(scope)?).bind(claim.measurement.measurement_id).bind(claim.attempt_id)
                .bind(encode(claim)?).execute(&mut **tx).await.map_err(unavailable)?;
        }
        sqlx::query("UPDATE serp_measurements SET measurement=$5,state=$6,claim=$7,sending_intent=$8,provider_task=$9,lease_expires_at=$10,next_poll_at=$11 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(stored.measurement.measurement_id).bind(encode(&stored.measurement)?)
            .bind(state_name(stored.measurement.state))
            .bind(stored.claim.as_ref().map(encode).transpose()?)
            .bind(stored.intent.as_ref().map(encode).transpose()?)
            .bind(stored.task.as_ref().map(encode).transpose()?)
            .bind(stored.claim.as_ref().map(|claim| claim.lease_expires_at))
            .bind(stored.next_poll_at)
            .execute(&mut **tx).await.map_err(unavailable)?;
        Ok(())
    }
    async fn raw(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SerpStoredRaw>, AppError> {
        let row = sqlx::query("SELECT metadata,body,stored_at FROM serp_raw_evidence WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND evidence_id=$4")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
            .fetch_optional(&mut **tx).await.map_err(unavailable)?;
        row.map(|row| {
            let mut evidence: SerpRawEvidence = decode(row.get("metadata"))?;
            evidence.body = row.get("body");
            Ok(SerpStoredRaw {
                evidence,
                stored_at: row.get("stored_at"),
            })
        })
        .transpose()
    }
    async fn binding(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        stored: &Stored,
        intent: &SerpSendingIntent,
        task: &SerpProviderTask,
        recovery: bool,
    ) -> Result<(), AppError> {
        let raw = Self::raw(tx, scope, task.binding_evidence_id)
            .await?
            .ok_or_else(|| AppError::not_found("search binding evidence not found"))?;
        validate_binding_evidence(stored, intent, task, &raw, recovery)
    }
}

#[async_trait]
impl SerpRepository for PgSerpRepository {
    async fn get_execution(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SerpExecution>, AppError> {
        let mut tx = self.tx(scope).await?;
        let row = sqlx::query("SELECT * FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
            .fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let result = row
            .map(Stored::from_row)
            .transpose()?
            .map(|stored| stored.execution());
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn get_observation(
        &self,
        scope: &TenantScope,
        id: Uuid,
        observation_id: Uuid,
    ) -> Result<Option<SerpObservation>, AppError> {
        let mut tx = self.tx(scope).await?;
        let row = sqlx::query("SELECT observation FROM serp_observations WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4 AND observation_id=$5")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id).bind(observation_id)
            .fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let result = row.map(|row| decode(row.get("observation"))).transpose()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn list_raw(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpRawReceipt>, AppError> {
        let limit = page_limit(limit)?;
        let mut tx = self.tx(scope).await?;
        Self::locked(&mut tx, scope, id).await?;
        let cursor: Option<DateTime<Utc>> = if let Some(after) = after {
            Some(sqlx::query_scalar("SELECT stored_at FROM serp_raw_evidence WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4 AND evidence_id=$5")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id).bind(after)
                .fetch_optional(&mut *tx).await.map_err(unavailable)?.ok_or_else(|| AppError::invalid_request("invalid search evidence cursor"))?)
        } else {
            None
        };
        // Metadata contains an empty body; byte length is computed by PostgreSQL
        // without returning BYTEA to this inventory endpoint.
        let rows = sqlx::query("SELECT metadata,octet_length(body) AS body_bytes,stored_at FROM serp_raw_evidence WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4 AND ($5::uuid IS NULL OR (stored_at,evidence_id)<($6,$5)) ORDER BY stored_at DESC,evidence_id DESC LIMIT $7")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
            .bind(after).bind(cursor).bind(limit).fetch_all(&mut *tx).await.map_err(unavailable)?;
        let result = rows
            .iter()
            .map(|row| {
                let stored = SerpStoredRaw {
                    evidence: decode(row.get("metadata"))?,
                    stored_at: row.get("stored_at"),
                };
                let mut receipt = stored.receipt();
                receipt.body_bytes = row.get::<i32, _>("body_bytes") as usize;
                Ok(receipt)
            })
            .collect::<Result<_, AppError>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn release_read(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        next_poll_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if next_poll_at < now {
            return Err(AppError::invalid_request("search poll precedes release"));
        }
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, claim.measurement.measurement_id).await?;
        stored.fence(claim, now)?;
        if stored.measurement.state != SerpTaskState::AwaitingResult || stored.task.is_none() {
            return Err(AppError::conflict("search read cannot release"));
        }
        stored.claim = None;
        stored.next_poll_at = Some(next_poll_at);
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    async fn defer_recovery(
        &self,
        scope: &TenantScope,
        id: Uuid,
        next_poll_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if next_poll_at < now {
            return Err(AppError::invalid_request("search poll precedes deferral"));
        }
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, id).await?;
        if stored.measurement.state != SerpTaskState::Unknown || now < stored.measurement.created_at
        {
            return Err(AppError::conflict("search recovery cannot defer"));
        }
        stored.next_poll_at = Some(
            stored
                .next_poll_at
                .map_or(next_poll_at, |prior| prior.max(next_poll_at)),
        );
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    async fn list_due(
        &self,
        scope: &TenantScope,
        now: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpMeasurement>, AppError> {
        let limit = page_limit(limit)?;
        let mut tx = self.tx(scope).await?;
        if let Some(after) = after {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4)")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(after)
                .fetch_one(&mut *tx).await.map_err(unavailable)?;
            if !exists {
                return Err(AppError::invalid_request("invalid search due cursor"));
            }
        }
        let rows = sqlx::query("SELECT measurement FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND state IN ('queued','awaiting_result','unknown') AND created_at<=$4 AND (lease_expires_at IS NULL OR lease_expires_at<=$4) AND ((state='queued' AND scheduled_at<=$4) OR (state IN ('awaiting_result','unknown') AND (next_poll_at IS NULL OR next_poll_at<=$4))) AND ($5::uuid IS NULL OR measurement_id>$5) ORDER BY measurement_id LIMIT $6")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(now).bind(after).bind(limit)
            .fetch_all(&mut *tx).await.map_err(unavailable)?;
        let result = rows
            .iter()
            .map(|row| decode(row.get("measurement")))
            .collect::<Result<_, _>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn accept(
        &self,
        scope: &TenantScope,
        key: &str,
        measurement: SerpMeasurement,
    ) -> Result<SerpMeasurement, AppError> {
        measurement.validate_accept(scope, key)?;
        let mut tx = self.tx(scope).await?;
        let row = sqlx::query("INSERT INTO serp_measurements (measurement_id,operator_id,tenant_id,project_id,idempotency_key_hash,measurement,state,created_at,scheduled_at) VALUES ($1,$2,$3,$4,$5,$6,'queued',$7,$8) ON CONFLICT DO NOTHING RETURNING measurement")
            .bind(measurement.measurement_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(sha256_hex(key.as_bytes())).bind(encode(&measurement)?).bind(measurement.created_at)
            .bind(measurement.scheduled_at)
            .fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let result = if let Some(row) = row {
            decode(row.get("measurement"))?
        } else {
            let row = sqlx::query("SELECT measurement FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND idempotency_key_hash=$4")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(sha256_hex(key.as_bytes()))
                .fetch_optional(&mut *tx).await.map_err(unavailable)?
                .ok_or_else(|| AppError::conflict("search measurement identity exists"))?;
            let prior: SerpMeasurement = decode(row.get("measurement"))?;
            if !prior.same_input(&measurement) {
                return Err(AppError::conflict("search idempotency input differs"));
            }
            prior
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        id: Uuid,
        now: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<Option<SerpClaim>, AppError> {
        if expires <= now {
            return Err(AppError::invalid_request("invalid search lease"));
        }
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, id).await?;
        if stored.measurement.state != SerpTaskState::Queued
            || stored.measurement.scheduled_at > now
        {
            return Ok(None);
        }
        if now < stored.measurement.created_at {
            return Err(AppError::invalid_request("claim precedes acceptance"));
        }
        stored.measurement.state = SerpTaskState::Claimed;
        let claim = SerpClaim {
            measurement: stored.measurement.clone(),
            attempt_id: Uuid::new_v4(),
            claim_token: Uuid::new_v4(),
            claimed_at: now,
            lease_expires_at: expires,
        };
        stored.claim = Some(claim.clone());
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(Some(claim))
    }

    async fn renew_claim(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        now: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<SerpClaim, AppError> {
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, claim.measurement.measurement_id).await?;
        let mut renewed = stored.fence(claim, now)?.clone();
        if expires <= renewed.lease_expires_at
            || !matches!(
                stored.measurement.state,
                SerpTaskState::Claimed | SerpTaskState::Sending | SerpTaskState::AwaitingResult
            )
        {
            return Err(AppError::conflict("search lease cannot renew"));
        }
        renewed.lease_expires_at = expires;
        renewed.measurement = stored.measurement.clone();
        stored.claim = Some(renewed.clone());
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(renewed)
    }

    async fn begin_send(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        request_sha256: &str,
        correlation_tag: &str,
        credential_revision: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<Option<SerpSendingIntent>, AppError> {
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, claim.measurement.measurement_id).await?;
        stored.fence(claim, now)?;
        if let Some(intent) = &stored.intent {
            if stored.measurement.state != SerpTaskState::Sending
                || intent.request_sha256 != request_sha256
                || intent.correlation_tag != correlation_tag
                || intent.credential_revision != credential_revision
            {
                return Err(AppError::conflict("search sending intent exists"));
            }
            return Ok(None);
        }
        let status: String = sqlx::query_scalar("SELECT status FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR SHARE")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .fetch_optional(&mut *tx).await.map_err(unavailable)?
            .ok_or_else(|| AppError::not_found("project not found"))?;
        if !matches!(status.as_str(), "draft" | "active") {
            return Err(AppError::conflict("project is unavailable for measurement"));
        }
        if let Some(revision) = credential_revision {
            let row = sqlx::query("SELECT * FROM project_serp_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_key=$4 FOR SHARE")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
                .bind(&stored.measurement.source_key).fetch_optional(&mut *tx).await.map_err(unavailable)?;
            let settings = row
                .as_ref()
                .map(crate::project_serp_settings::decode)
                .transpose()?;
            validate_project_serp_send(settings.as_ref(), &stored.measurement, revision)?;
        }
        validate_serp_task_transition(stored.measurement.state, SerpTaskState::Sending)?;
        let intent = SerpSendingIntent {
            measurement_id: stored.measurement.measurement_id,
            attempt_id: claim.attempt_id,
            send_token: Uuid::new_v4(),
            request_sha256: request_sha256.into(),
            correlation_tag: correlation_tag.into(),
            credential_revision,
            intended_at: now,
        };
        intent.validate()?;
        stored.measurement.state = SerpTaskState::Sending;
        stored.intent = Some(intent.clone());
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(Some(intent))
    }

    async fn get_sending_intent(
        &self,
        scope: &TenantScope,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Option<SerpSendingIntent>, AppError> {
        let mut tx = self.tx(scope).await?;
        let stored = Self::locked(&mut tx, scope, id).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(stored.intent.filter(|intent| intent.attempt_id == attempt))
    }

    async fn append_raw(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        evidence: SerpRawEvidence,
    ) -> Result<SerpStoredRaw, AppError> {
        evidence.validate(intent)?;
        let mut tx = self.tx(scope).await?;
        let stored = Self::locked(&mut tx, scope, intent.measurement_id).await?;
        stored.intent(intent)?;
        if evidence.operation == SerpEvidenceOperation::ResultRead
            && stored
                .task
                .as_ref()
                .map(|task| task.provider_task_id.as_str())
                != evidence.provider_task_id.as_deref()
        {
            return Err(AppError::conflict("search result task differs"));
        }
        if let Some(prior) = Self::raw(&mut tx, scope, evidence.evidence_id).await? {
            return if prior.evidence == evidence {
                Ok(prior)
            } else {
                Err(AppError::conflict("search raw evidence exists"))
            };
        }
        let mut metadata = evidence.clone();
        metadata.body.clear();
        let stored_at = sqlx::query_scalar("INSERT INTO serp_raw_evidence (evidence_id,operator_id,tenant_id,project_id,measurement_id,attempt_id,metadata,body,response_sha256) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING RETURNING stored_at")
            .bind(evidence.evidence_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(evidence.measurement_id).bind(evidence.attempt_id).bind(encode(&metadata)?).bind(&evidence.body).bind(&evidence.response_sha256)
            .fetch_optional(&mut *tx).await.map_err(unavailable)?
            .ok_or_else(|| AppError::conflict("search raw evidence identity exists"))?;
        tx.commit().await.map_err(unavailable)?;
        Ok(SerpStoredRaw {
            evidence,
            stored_at,
        })
    }

    async fn get_raw(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SerpStoredRaw>, AppError> {
        let mut tx = self.tx(scope).await?;
        let result = Self::raw(&mut tx, scope, id).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn bind_provider_task(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        task: SerpProviderTask,
    ) -> Result<SerpProviderTask, AppError> {
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, intent.measurement_id).await?;
        Self::binding(&mut tx, scope, &stored, intent, &task, false).await?;
        stored.task = Some(task.clone());
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(task)
    }

    async fn recover_provider_task(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        task: SerpProviderTask,
        now: DateTime<Utc>,
    ) -> Result<SerpProviderTask, AppError> {
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, intent.measurement_id).await?;
        validate_serp_read_recovery(stored.measurement.state, &task, intent)?;
        Self::binding(&mut tx, scope, &stored, intent, &task, true).await?;
        if now < intent.intended_at {
            return Err(AppError::invalid_request("recovery precedes sending"));
        }
        let task = stored.task.clone().unwrap_or(task);
        stored.task = Some(task.clone());
        stored.measurement.state = SerpTaskState::AwaitingResult;
        stored.claim = None;
        stored.next_poll_at = Some(now);
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(task)
    }

    async fn get_provider_task(
        &self,
        scope: &TenantScope,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Option<SerpProviderTask>, AppError> {
        let mut tx = self.tx(scope).await?;
        let stored = Self::locked(&mut tx, scope, id).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(stored.task.filter(|task| task.attempt_id == attempt))
    }

    async fn append_observation(
        &self,
        scope: &TenantScope,
        observation: SerpObservation,
    ) -> Result<SerpObservation, AppError> {
        let mut tx = self.tx(scope).await?;
        let stored = Self::locked(&mut tx, scope, observation.measurement_id).await?;
        observation.validate(&stored.measurement)?;
        let raw = Self::raw(&mut tx, scope, observation.raw_evidence_id)
            .await?
            .ok_or_else(|| AppError::not_found("search raw evidence not found"))?;
        validate_observation_evidence(&stored, &observation, &raw)?;
        let row = sqlx::query("INSERT INTO serp_observations (observation_id,operator_id,tenant_id,project_id,measurement_id,attempt_id,raw_evidence_id,observation,analyzed_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING RETURNING observation")
            .bind(observation.observation_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(observation.measurement_id).bind(observation.attempt_id).bind(observation.raw_evidence_id)
            .bind(encode(&observation)?).bind(observation.analyzed_at).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        if row.is_none() {
            let row = sqlx::query("SELECT observation FROM serp_observations WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND observation_id=$4")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(observation.observation_id)
                .fetch_optional(&mut *tx).await.map_err(unavailable)?
                .ok_or_else(|| AppError::conflict("search observation identity exists"))?;
            if decode::<SerpObservation>(row.get("observation"))? != observation {
                return Err(AppError::conflict("search observation exists"));
            }
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(observation)
    }

    async fn list_observations(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpObservation>, AppError> {
        let limit = page_limit(limit)?;
        let mut tx = self.tx(scope).await?;
        Self::locked(&mut tx, scope, id).await?;
        let cursor: Option<DateTime<Utc>> = if let Some(after) = after {
            Some(sqlx::query_scalar("SELECT analyzed_at FROM serp_observations WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4 AND observation_id=$5")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id).bind(after)
                .fetch_optional(&mut *tx).await.map_err(unavailable)?.ok_or_else(|| AppError::invalid_request("invalid search observation cursor"))?)
        } else {
            None
        };
        let rows = sqlx::query("SELECT observation FROM serp_observations WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4 AND ($5::uuid IS NULL OR (analyzed_at,observation_id)<($6,$5)) ORDER BY analyzed_at DESC,observation_id DESC LIMIT $7")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
            .bind(after).bind(cursor).bind(limit).fetch_all(&mut *tx).await.map_err(unavailable)?;
        let result = rows
            .iter()
            .map(|row| decode(row.get("observation")))
            .collect::<Result<_, _>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn claim_read(
        &self,
        scope: &TenantScope,
        id: Uuid,
        now: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<Option<SerpClaim>, AppError> {
        if expires <= now {
            return Err(AppError::invalid_request("invalid search lease"));
        }
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, id).await?;
        if stored.measurement.state != SerpTaskState::AwaitingResult
            || stored.next_poll_at.is_some_and(|due| due > now)
            || stored
                .claim
                .as_ref()
                .is_some_and(|claim| claim.lease_expires_at > now)
        {
            return Ok(None);
        }
        let task = stored
            .task
            .as_ref()
            .ok_or_else(|| AppError::conflict("search provider task missing"))?;
        if now
            < stored
                .intent
                .as_ref()
                .ok_or_else(|| AppError::conflict("search sending intent missing"))?
                .intended_at
        {
            return Err(AppError::invalid_request("read claim precedes sending"));
        }
        let claim = SerpClaim {
            measurement: stored.measurement.clone(),
            attempt_id: task.attempt_id,
            claim_token: Uuid::new_v4(),
            claimed_at: now,
            lease_expires_at: expires,
        };
        stored.claim = Some(claim.clone());
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(Some(claim))
    }

    async fn expire_claims(
        &self,
        scope: &TenantScope,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<Uuid>, AppError> {
        let limit = page_limit(limit)?;
        let mut tx = self.tx(scope).await?;
        let rows = sqlx::query("SELECT * FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND state IN ('claimed','sending','awaiting_result') AND lease_expires_at<=$4 ORDER BY lease_expires_at,measurement_id LIMIT $5 FOR UPDATE SKIP LOCKED")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(now).bind(limit)
            .fetch_all(&mut *tx).await.map_err(unavailable)?;
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let mut stored = Stored::from_row(row)?;
            stored.measurement.state =
                if stored.measurement.state == SerpTaskState::Claimed && stored.intent.is_none() {
                    SerpTaskState::Queued
                } else {
                    SerpTaskState::Unknown
                };
            stored.claim = None;
            stored.next_poll_at = Some(now);
            ids.push(stored.measurement.measurement_id);
            Self::save(&mut tx, scope, &stored).await?;
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(ids)
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        state: SerpTaskState,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, claim.measurement.measurement_id).await?;
        stored.fence(claim, now)?;
        validate_serp_task_transition(stored.measurement.state, state)?;
        if !matches!(
            state,
            SerpTaskState::AwaitingResult
                | SerpTaskState::Completed
                | SerpTaskState::Unknown
                | SerpTaskState::Failed
        ) {
            return Err(AppError::invalid_request("invalid search completion state"));
        }
        if state == SerpTaskState::AwaitingResult && stored.task.is_none() {
            return Err(AppError::conflict("search provider task missing"));
        }
        if state == SerpTaskState::Completed {
            let found: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM serp_observations WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4 AND attempt_id=$5)")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
                .bind(stored.measurement.measurement_id).bind(claim.attempt_id).fetch_one(&mut *tx).await.map_err(unavailable)?;
            if !found {
                return Err(AppError::conflict("search observation missing"));
            }
        }
        stored.measurement.state = state;
        stored.claim = None;
        stored.next_poll_at = matches!(
            state,
            SerpTaskState::AwaitingResult | SerpTaskState::Unknown
        )
        .then_some(now);
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    async fn cancel(
        &self,
        scope: &TenantScope,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let mut tx = self.tx(scope).await?;
        let mut stored = Self::locked(&mut tx, scope, id).await?;
        if now < stored.measurement.created_at {
            return Err(AppError::invalid_request(
                "cancellation precedes acceptance",
            ));
        }
        if stored.measurement.state == SerpTaskState::Cancelled {
            return Ok(());
        }
        validate_serp_task_transition(stored.measurement.state, SerpTaskState::Cancelled)?;
        stored.measurement.state = SerpTaskState::Cancelled;
        stored.claim = None;
        stored.next_poll_at = None;
        Self::save(&mut tx, scope, &stored).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    async fn get(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SerpMeasurement>, AppError> {
        let mut tx = self.tx(scope).await?;
        let row = sqlx::query("SELECT measurement FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
            .fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let result = row.map(|row| decode(row.get("measurement"))).transpose()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn list(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpMeasurement>, AppError> {
        let limit = page_limit(limit)?;
        let mut tx = self.tx(scope).await?;
        let cursor: Option<DateTime<Utc>> = if let Some(id) = after {
            Some(sqlx::query_scalar("SELECT created_at FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND measurement_id=$4")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(id)
                .fetch_optional(&mut *tx).await.map_err(unavailable)?
                .ok_or_else(|| AppError::invalid_request("invalid search cursor"))?)
        } else {
            None
        };
        let rows = sqlx::query("SELECT measurement FROM serp_measurements WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND ($4::uuid IS NULL OR (created_at,measurement_id)<($5,$4)) ORDER BY created_at DESC,measurement_id DESC LIMIT $6")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .bind(after).bind(cursor).bind(limit).fetch_all(&mut *tx).await.map_err(unavailable)?;
        let result = rows
            .iter()
            .map(|row| decode(row.get("measurement")))
            .collect::<Result<_, _>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }
}

#[derive(Default)]
pub struct MemorySerpRepository {
    data: tokio::sync::Mutex<MemoryData>,
    settings: Option<std::sync::Arc<MemoryProjectSerpSettingsRepository>>,
}
impl MemorySerpRepository {
    pub fn with_settings(settings: std::sync::Arc<MemoryProjectSerpSettingsRepository>) -> Self {
        Self {
            settings: Some(settings),
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct MemoryData {
    tasks: std::collections::HashMap<Uuid, (TenantScope, String, Stored)>,
    raw: std::collections::HashMap<Uuid, (TenantScope, SerpStoredRaw)>,
    observations: std::collections::HashMap<Uuid, (TenantScope, SerpObservation)>,
    claims: std::collections::HashMap<Uuid, (TenantScope, SerpClaim)>,
}
impl MemoryData {
    fn task(&self, scope: &TenantScope, id: Uuid) -> Result<&Stored, AppError> {
        project(scope)?;
        self.tasks
            .get(&id)
            .filter(|(owner, _, _)| owner == scope)
            .map(|(_, _, stored)| stored)
            .ok_or_else(|| AppError::not_found("search measurement not found"))
    }
    fn task_mut(&mut self, scope: &TenantScope, id: Uuid) -> Result<&mut Stored, AppError> {
        project(scope)?;
        self.tasks
            .get_mut(&id)
            .filter(|(owner, _, _)| owner == scope)
            .map(|(_, _, stored)| stored)
            .ok_or_else(|| AppError::not_found("search measurement not found"))
    }
    fn raw(&self, scope: &TenantScope, id: Uuid) -> Option<&SerpStoredRaw> {
        self.raw
            .get(&id)
            .filter(|(owner, _)| owner == scope)
            .map(|(_, raw)| raw)
    }
}

#[async_trait]
impl SerpRepository for MemorySerpRepository {
    async fn get_execution(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SerpExecution>, AppError> {
        project(scope)?;
        Ok(self
            .data
            .lock()
            .await
            .tasks
            .get(&id)
            .filter(|(owner, _, _)| owner == scope)
            .map(|(_, _, stored)| stored.execution()))
    }

    async fn get_observation(
        &self,
        scope: &TenantScope,
        id: Uuid,
        observation_id: Uuid,
    ) -> Result<Option<SerpObservation>, AppError> {
        project(scope)?;
        Ok(self
            .data
            .lock()
            .await
            .observations
            .get(&observation_id)
            .filter(|(owner, observation)| owner == scope && observation.measurement_id == id)
            .map(|(_, observation)| observation.clone()))
    }

    async fn list_raw(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpRawReceipt>, AppError> {
        page_limit(limit)?;
        let data = self.data.lock().await;
        data.task(scope, id)?;
        let cursor = after
            .map(|after| {
                data.raw(scope, after)
                    .filter(|raw| raw.evidence.measurement_id == id)
                    .map(|raw| (raw.stored_at, after))
                    .ok_or_else(|| AppError::invalid_request("invalid search evidence cursor"))
            })
            .transpose()?;
        let mut rows: Vec<_> = data
            .raw
            .values()
            .filter(|(owner, raw)| owner == scope && raw.evidence.measurement_id == id)
            .map(|(_, raw)| raw)
            .filter(|raw| {
                cursor.is_none_or(|cursor| (raw.stored_at, raw.evidence.evidence_id) < cursor)
            })
            .map(SerpStoredRaw::receipt)
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse((row.stored_at, row.evidence_id)));
        rows.truncate(limit);
        Ok(rows)
    }

    async fn release_read(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        next_poll_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if next_poll_at < now {
            return Err(AppError::invalid_request("search poll precedes release"));
        }
        let mut data = self.data.lock().await;
        let stored = data.task_mut(scope, claim.measurement.measurement_id)?;
        stored.fence(claim, now)?;
        if stored.measurement.state != SerpTaskState::AwaitingResult || stored.task.is_none() {
            return Err(AppError::conflict("search read cannot release"));
        }
        stored.claim = None;
        stored.next_poll_at = Some(next_poll_at);
        Ok(())
    }

    async fn defer_recovery(
        &self,
        scope: &TenantScope,
        id: Uuid,
        next_poll_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if next_poll_at < now {
            return Err(AppError::invalid_request("search poll precedes deferral"));
        }
        let mut data = self.data.lock().await;
        let stored = data.task_mut(scope, id)?;
        if stored.measurement.state != SerpTaskState::Unknown || now < stored.measurement.created_at
        {
            return Err(AppError::conflict("search recovery cannot defer"));
        }
        stored.next_poll_at = Some(
            stored
                .next_poll_at
                .map_or(next_poll_at, |prior| prior.max(next_poll_at)),
        );
        Ok(())
    }

    async fn list_due(
        &self,
        scope: &TenantScope,
        now: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpMeasurement>, AppError> {
        project(scope)?;
        page_limit(limit)?;
        let data = self.data.lock().await;
        if let Some(after) = after {
            data.task(scope, after)
                .map_err(|_| AppError::invalid_request("invalid search due cursor"))?;
        }
        let mut rows: Vec<_> = data
            .tasks
            .values()
            .filter(|(owner, _, stored)| owner == scope && stored.due(now))
            .map(|(_, _, stored)| &stored.measurement)
            .filter(|row| after.is_none_or(|after| row.measurement_id > after))
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.measurement_id);
        rows.truncate(limit);
        Ok(rows)
    }

    async fn accept(
        &self,
        scope: &TenantScope,
        key: &str,
        measurement: SerpMeasurement,
    ) -> Result<SerpMeasurement, AppError> {
        measurement.validate_accept(scope, key)?;
        let hash = sha256_hex(key.as_bytes());
        let mut data = self.data.lock().await;
        if let Some((_, _, prior)) = data
            .tasks
            .values()
            .find(|(owner, prior_key, _)| owner == scope && *prior_key == hash)
        {
            return if prior.measurement.same_input(&measurement) {
                Ok(prior.measurement.clone())
            } else {
                Err(AppError::conflict("search idempotency input differs"))
            };
        }
        if data.tasks.contains_key(&measurement.measurement_id) {
            return Err(AppError::conflict("search measurement identity exists"));
        }
        data.tasks.insert(
            measurement.measurement_id,
            (
                scope.clone(),
                hash,
                Stored {
                    measurement: measurement.clone(),
                    claim: None,
                    intent: None,
                    task: None,
                    next_poll_at: None,
                },
            ),
        );
        Ok(measurement)
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        id: Uuid,
        now: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<Option<SerpClaim>, AppError> {
        if expires <= now {
            return Err(AppError::invalid_request("invalid search lease"));
        }
        let mut data = self.data.lock().await;
        let stored = data.task_mut(scope, id)?;
        if stored.measurement.state != SerpTaskState::Queued
            || stored.measurement.scheduled_at > now
        {
            return Ok(None);
        }
        if now < stored.measurement.created_at {
            return Err(AppError::invalid_request("claim precedes acceptance"));
        }
        stored.measurement.state = SerpTaskState::Claimed;
        let claim = SerpClaim {
            measurement: stored.measurement.clone(),
            attempt_id: Uuid::new_v4(),
            claim_token: Uuid::new_v4(),
            claimed_at: now,
            lease_expires_at: expires,
        };
        stored.claim = Some(claim.clone());
        data.claims
            .insert(claim.claim_token, (scope.clone(), claim.clone()));
        Ok(Some(claim))
    }

    async fn renew_claim(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        now: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<SerpClaim, AppError> {
        let mut data = self.data.lock().await;
        let stored = data.task_mut(scope, claim.measurement.measurement_id)?;
        let mut renewed = stored.fence(claim, now)?.clone();
        if expires <= renewed.lease_expires_at
            || !matches!(
                stored.measurement.state,
                SerpTaskState::Claimed | SerpTaskState::Sending | SerpTaskState::AwaitingResult
            )
        {
            return Err(AppError::conflict("search lease cannot renew"));
        }
        renewed.lease_expires_at = expires;
        renewed.measurement = stored.measurement.clone();
        stored.claim = Some(renewed.clone());
        Ok(renewed)
    }

    async fn begin_send(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        request_sha256: &str,
        correlation_tag: &str,
        credential_revision: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<Option<SerpSendingIntent>, AppError> {
        let gate = if credential_revision.is_some() {
            Some(
                self.settings
                    .as_ref()
                    .ok_or_else(|| AppError::not_ready("search source unavailable"))?
                    .send_consistency_gate(),
            )
        } else {
            None
        };
        let _consistency = if let Some(gate) = &gate {
            Some(gate.read().await)
        } else {
            None
        };
        let mut data = self.data.lock().await;
        let stored = data.task_mut(scope, claim.measurement.measurement_id)?;
        stored.fence(claim, now)?;
        if let Some(intent) = &stored.intent {
            if stored.measurement.state != SerpTaskState::Sending
                || intent.request_sha256 != request_sha256
                || intent.correlation_tag != correlation_tag
                || intent.credential_revision != credential_revision
            {
                return Err(AppError::conflict("search sending intent exists"));
            }
            return Ok(None);
        }
        if let Some(revision) = credential_revision {
            let settings = self
                .settings
                .as_ref()
                .ok_or_else(|| AppError::not_ready("search source unavailable"))?
                .get(scope, &stored.measurement.source_key)
                .await?;
            validate_project_serp_send(settings.as_ref(), &stored.measurement, revision)?;
        }
        validate_serp_task_transition(stored.measurement.state, SerpTaskState::Sending)?;
        let intent = SerpSendingIntent {
            measurement_id: stored.measurement.measurement_id,
            attempt_id: claim.attempt_id,
            send_token: Uuid::new_v4(),
            request_sha256: request_sha256.into(),
            correlation_tag: correlation_tag.into(),
            credential_revision,
            intended_at: now,
        };
        intent.validate()?;
        stored.measurement.state = SerpTaskState::Sending;
        stored.intent = Some(intent.clone());
        Ok(Some(intent))
    }

    async fn get_sending_intent(
        &self,
        scope: &TenantScope,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Option<SerpSendingIntent>, AppError> {
        let data = self.data.lock().await;
        Ok(data
            .task(scope, id)?
            .intent
            .clone()
            .filter(|intent| intent.attempt_id == attempt))
    }

    async fn append_raw(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        evidence: SerpRawEvidence,
    ) -> Result<SerpStoredRaw, AppError> {
        evidence.validate(intent)?;
        let mut data = self.data.lock().await;
        let stored = data.task(scope, intent.measurement_id)?;
        stored.intent(intent)?;
        if evidence.operation == SerpEvidenceOperation::ResultRead
            && stored
                .task
                .as_ref()
                .map(|task| task.provider_task_id.as_str())
                != evidence.provider_task_id.as_deref()
        {
            return Err(AppError::conflict("search result task differs"));
        }
        if let Some(prior) = data.raw(scope, evidence.evidence_id) {
            return if prior.evidence == evidence {
                Ok(prior.clone())
            } else {
                Err(AppError::conflict("search raw evidence exists"))
            };
        }
        if data.raw.contains_key(&evidence.evidence_id) {
            return Err(AppError::conflict("search raw evidence identity exists"));
        }
        let raw = SerpStoredRaw {
            evidence,
            stored_at: Utc::now(),
        };
        data.raw
            .insert(raw.evidence.evidence_id, (scope.clone(), raw.clone()));
        Ok(raw)
    }

    async fn get_raw(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SerpStoredRaw>, AppError> {
        project(scope)?;
        Ok(self.data.lock().await.raw(scope, id).cloned())
    }

    async fn bind_provider_task(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        task: SerpProviderTask,
    ) -> Result<SerpProviderTask, AppError> {
        let mut data = self.data.lock().await;
        let stored = data.task(scope, intent.measurement_id)?;
        let raw = data
            .raw(scope, task.binding_evidence_id)
            .ok_or_else(|| AppError::not_found("search binding evidence not found"))?;
        validate_binding_evidence(stored, intent, &task, raw, false)?;
        data.task_mut(scope, intent.measurement_id)?.task = Some(task.clone());
        Ok(task)
    }

    async fn recover_provider_task(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        task: SerpProviderTask,
        now: DateTime<Utc>,
    ) -> Result<SerpProviderTask, AppError> {
        let mut data = self.data.lock().await;
        let stored = data.task(scope, intent.measurement_id)?;
        validate_serp_read_recovery(stored.measurement.state, &task, intent)?;
        let raw = data
            .raw(scope, task.binding_evidence_id)
            .ok_or_else(|| AppError::not_found("search binding evidence not found"))?;
        validate_binding_evidence(stored, intent, &task, raw, true)?;
        if now < intent.intended_at {
            return Err(AppError::invalid_request("recovery precedes sending"));
        }
        let stored = data.task_mut(scope, intent.measurement_id)?;
        let task = stored.task.clone().unwrap_or(task);
        stored.task = Some(task.clone());
        stored.measurement.state = SerpTaskState::AwaitingResult;
        stored.claim = None;
        stored.next_poll_at = Some(now);
        Ok(task)
    }

    async fn get_provider_task(
        &self,
        scope: &TenantScope,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Option<SerpProviderTask>, AppError> {
        let data = self.data.lock().await;
        Ok(data
            .task(scope, id)?
            .task
            .clone()
            .filter(|task| task.attempt_id == attempt))
    }

    async fn append_observation(
        &self,
        scope: &TenantScope,
        observation: SerpObservation,
    ) -> Result<SerpObservation, AppError> {
        let mut data = self.data.lock().await;
        let stored = data.task(scope, observation.measurement_id)?;
        let raw = data
            .raw(scope, observation.raw_evidence_id)
            .ok_or_else(|| AppError::not_found("search raw evidence not found"))?;
        validate_observation_evidence(stored, &observation, raw)?;
        if let Some((owner, prior)) = data.observations.get(&observation.observation_id) {
            return if owner == scope && prior == &observation {
                Ok(prior.clone())
            } else {
                Err(AppError::conflict("search observation exists"))
            };
        }
        data.observations.insert(
            observation.observation_id,
            (scope.clone(), observation.clone()),
        );
        Ok(observation)
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        claim: &SerpClaim,
        state: SerpTaskState,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let mut data = self.data.lock().await;
        let stored = data.task(scope, claim.measurement.measurement_id)?;
        stored.fence(claim, now)?;
        validate_serp_task_transition(stored.measurement.state, state)?;
        if !matches!(
            state,
            SerpTaskState::AwaitingResult
                | SerpTaskState::Completed
                | SerpTaskState::Unknown
                | SerpTaskState::Failed
        ) {
            return Err(AppError::invalid_request("invalid search completion state"));
        }
        if state == SerpTaskState::AwaitingResult && stored.task.is_none() {
            return Err(AppError::conflict("search provider task missing"));
        }
        if state == SerpTaskState::Completed
            && !data.observations.values().any(|(owner, observation)| {
                owner == scope
                    && observation.measurement_id == stored.measurement.measurement_id
                    && observation.attempt_id == claim.attempt_id
            })
        {
            return Err(AppError::conflict("search observation missing"));
        }
        let stored = data.task_mut(scope, claim.measurement.measurement_id)?;
        stored.measurement.state = state;
        stored.claim = None;
        stored.next_poll_at = matches!(
            state,
            SerpTaskState::AwaitingResult | SerpTaskState::Unknown
        )
        .then_some(now);
        Ok(())
    }

    async fn cancel(
        &self,
        scope: &TenantScope,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let mut data = self.data.lock().await;
        let stored = data.task_mut(scope, id)?;
        if now < stored.measurement.created_at {
            return Err(AppError::invalid_request(
                "cancellation precedes acceptance",
            ));
        }
        if stored.measurement.state == SerpTaskState::Cancelled {
            return Ok(());
        }
        validate_serp_task_transition(stored.measurement.state, SerpTaskState::Cancelled)?;
        stored.measurement.state = SerpTaskState::Cancelled;
        stored.claim = None;
        stored.next_poll_at = None;
        Ok(())
    }

    async fn claim_read(
        &self,
        scope: &TenantScope,
        id: Uuid,
        now: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Result<Option<SerpClaim>, AppError> {
        if expires <= now {
            return Err(AppError::invalid_request("invalid search lease"));
        }
        let mut data = self.data.lock().await;
        let stored = data.task_mut(scope, id)?;
        if stored.measurement.state != SerpTaskState::AwaitingResult
            || stored.next_poll_at.is_some_and(|due| due > now)
            || stored
                .claim
                .as_ref()
                .is_some_and(|claim| claim.lease_expires_at > now)
        {
            return Ok(None);
        }
        let task = stored
            .task
            .as_ref()
            .ok_or_else(|| AppError::conflict("search provider task missing"))?;
        if now
            < stored
                .intent
                .as_ref()
                .ok_or_else(|| AppError::conflict("search sending intent missing"))?
                .intended_at
        {
            return Err(AppError::invalid_request("read claim precedes sending"));
        }
        let claim = SerpClaim {
            measurement: stored.measurement.clone(),
            attempt_id: task.attempt_id,
            claim_token: Uuid::new_v4(),
            claimed_at: now,
            lease_expires_at: expires,
        };
        stored.claim = Some(claim.clone());
        data.claims
            .insert(claim.claim_token, (scope.clone(), claim.clone()));
        Ok(Some(claim))
    }

    async fn expire_claims(
        &self,
        scope: &TenantScope,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<Uuid>, AppError> {
        project(scope)?;
        page_limit(limit)?;
        let mut data = self.data.lock().await;
        let mut candidates: Vec<_> = data
            .tasks
            .values()
            .filter(|(owner, _, stored)| {
                owner == scope
                    && matches!(
                        stored.measurement.state,
                        SerpTaskState::Claimed
                            | SerpTaskState::Sending
                            | SerpTaskState::AwaitingResult
                    )
            })
            .filter_map(|(_, _, stored)| {
                stored
                    .claim
                    .as_ref()
                    .filter(|claim| claim.lease_expires_at <= now)
                    .map(|claim| (claim.lease_expires_at, stored.measurement.measurement_id))
            })
            .collect();
        candidates.sort();
        candidates.truncate(limit);
        let mut ids = Vec::with_capacity(candidates.len());
        for (_, id) in candidates {
            let stored = data.task_mut(scope, id)?;
            stored.measurement.state =
                if stored.measurement.state == SerpTaskState::Claimed && stored.intent.is_none() {
                    SerpTaskState::Queued
                } else {
                    SerpTaskState::Unknown
                };
            stored.claim = None;
            stored.next_poll_at = Some(now);
            ids.push(id);
        }
        Ok(ids)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SerpMeasurement>, AppError> {
        project(scope)?;
        Ok(self
            .data
            .lock()
            .await
            .tasks
            .get(&id)
            .filter(|(owner, _, _)| owner == scope)
            .map(|(_, _, stored)| stored.measurement.clone()))
    }

    async fn list(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpMeasurement>, AppError> {
        project(scope)?;
        page_limit(limit)?;
        let data = self.data.lock().await;
        let cursor = after
            .map(|id| {
                data.task(scope, id)
                    .map(|stored| (stored.measurement.created_at, id))
                    .map_err(|_| AppError::invalid_request("invalid search cursor"))
            })
            .transpose()?;
        let mut rows: Vec<_> = data
            .tasks
            .values()
            .filter(|(owner, _, _)| owner == scope)
            .map(|(_, _, stored)| &stored.measurement)
            .filter(|row| cursor.is_none_or(|cursor| (row.created_at, row.measurement_id) < cursor))
            .cloned()
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse((row.created_at, row.measurement_id)));
        rows.truncate(limit);
        Ok(rows)
    }

    async fn list_observations(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<SerpObservation>, AppError> {
        page_limit(limit)?;
        let data = self.data.lock().await;
        data.task(scope, id)?;
        let cursor = after
            .map(|after| {
                data.observations
                    .get(&after)
                    .filter(|(owner, observation)| {
                        owner == scope && observation.measurement_id == id
                    })
                    .map(|(_, observation)| (observation.analyzed_at, after))
                    .ok_or_else(|| AppError::invalid_request("invalid search observation cursor"))
            })
            .transpose()?;
        let mut rows: Vec<_> = data
            .observations
            .values()
            .filter(|(owner, observation)| owner == scope && observation.measurement_id == id)
            .map(|(_, observation)| observation)
            .filter(|row| {
                cursor.is_none_or(|cursor| (row.analyzed_at, row.observation_id) < cursor)
            })
            .cloned()
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse((row.analyzed_at, row.observation_id)));
        rows.truncate(limit);
        Ok(rows)
    }
}
