//! Write-once inference responses with scoped atomic claims and no re-inference.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, ObservationAnalysisResult, ObservationCaptureRepository,
    ObservationExtractionClaim, ObservationExtractionIntent, ObservationExtractionInterruption,
    ObservationExtractionReadyItem, ObservationExtractionRepository, ObservationExtractionRequest,
    ObservationExtractionResponse, ObservationExtractionState, ObservationExtractionStatus,
    TenantScope, validate_observation_extraction_page_size, validate_observation_extraction_scope,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgObservationExtractionRepository {
    pool: PgPool,
    captures: crate::PgObservationCaptureRepository,
}

impl PgObservationExtractionRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
            captures: crate::PgObservationCaptureRepository::from_database(database),
        }
    }
    async fn transaction(
        &self,
        scope: &TenantScope,
    ) -> Result<Transaction<'_, Postgres>, AppError> {
        validate_observation_extraction_scope(scope)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        Ok(tx)
    }
    async fn locked(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<PgRow, AppError> {
        sqlx::query("SELECT * FROM observation_extraction_intents WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4 FOR UPDATE")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(id)
            .fetch_optional(&mut **tx).await.map_err(db)?.ok_or_else(|| AppError::not_found("extraction intent not found"))
    }
    async fn update(
        &self,
        scope: &TenantScope,
        id: Uuid,
        token: Uuid,
        apply: impl FnOnce(&mut ObservationExtractionIntent) -> Result<(), AppError> + Send,
        fence: bool,
    ) -> Result<ObservationExtractionIntent, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = self.locked(&mut tx, scope, id).await?;
        if token.is_nil() || row.get::<Option<Uuid>, _>("claim_token") != Some(token) {
            return Err(AppError::conflict("extraction claim is not current"));
        }
        let mut intent = decode(&row)?;
        let original = intent.clone();
        apply(&mut intent)?;
        // Idempotent terminal replay must not issue UPDATE: the database also
        // enforces terminal immutability for callers bypassing this repository.
        if intent != original {
            write(&mut tx, &intent, if fence { Uuid::new_v4() } else { token }).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(intent)
    }
}

fn db(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "observation extraction store unavailable",
    )
}
fn project(scope: &TenantScope) -> Uuid {
    scope
        .project_id
        .expect("validated extraction scope")
        .as_uuid()
}
fn decode(row: &PgRow) -> Result<ObservationExtractionIntent, AppError> {
    serde_json::from_value(row.get("intent"))
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored observation extraction invalid"))
}
fn state(state: ObservationExtractionState) -> &'static str {
    match state {
        ObservationExtractionState::Queued => "queued",
        ObservationExtractionState::Running => "running",
        ObservationExtractionState::ResponseReceived => "response_received",
        ObservationExtractionState::Completed => "completed",
        ObservationExtractionState::Interrupted => "interrupted",
    }
}
fn value(intent: &ObservationExtractionIntent) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(intent)
        .map_err(|_| AppError::new(ErrorCode::Internal, "invalid observation extraction"))
}
async fn write(
    tx: &mut Transaction<'_, Postgres>,
    intent: &ObservationExtractionIntent,
    token: Uuid,
) -> Result<(), AppError> {
    sqlx::query("UPDATE observation_extraction_intents SET state=$2,intent=$3,claim_token=$4 WHERE intent_id=$1")
        .bind(intent.intent_id).bind(state(intent.state)).bind(value(intent)?).bind(token).execute(&mut **tx).await.map_err(db)?;
    Ok(())
}

#[async_trait]
impl ObservationExtractionRepository for PgObservationExtractionRepository {
    async fn enqueue(
        &self,
        scope: &TenantScope,
        request: ObservationExtractionRequest,
        created_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        let key = request.intent_key(scope)?;
        let capture = self
            .captures
            .get(scope, request.source_capture_id)
            .await?
            .ok_or_else(|| AppError::not_found("extraction source not found"))?;
        request.validate_source(scope, &capture)?;
        let intent = ObservationExtractionIntent::queued(request, key, created_at);
        let mut tx = self.transaction(scope).await?;
        let inserted = sqlx::query("INSERT INTO observation_extraction_intents (intent_id,operator_id,tenant_id,project_id,source_capture_id,intent_key,state,created_at,intent) VALUES ($1,$2,$3,$4,$5,$6,'queued',$7,$8) ON CONFLICT (operator_id,tenant_id,project_id,intent_key) DO NOTHING RETURNING *")
            .bind(intent.intent_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope))
            .bind(intent.request.source_capture_id).bind(&intent.intent_key).bind(created_at).bind(value(&intent)?)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let stored = match inserted {
            Some(row) => decode(&row)?,
            None => {
                let row = sqlx::query("SELECT * FROM observation_extraction_intents WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_key=$4")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(&intent.intent_key)
                .fetch_one(&mut *tx).await.map_err(db)?;
                let prior = decode(&row)?;
                if prior.request != intent.request {
                    return Err(AppError::conflict("extraction intent binding differs"));
                }
                prior
            }
        };
        tx.commit().await.map_err(db)?;
        Ok(stored)
    }
    async fn get(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<Option<ObservationExtractionIntent>, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query("SELECT * FROM observation_extraction_intents WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(intent_id)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let result = row.as_ref().map(decode).transpose()?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    async fn scan_ready(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationExtractionReadyItem>, AppError> {
        validate_observation_extraction_page_size(limit)?;
        let rows = sqlx::query("SELECT operator_id,tenant_id,project_id,intent_id,state FROM observation_extraction_intents WHERE state IN ('queued','response_received') AND created_at<=$1 AND ($2::uuid IS NULL OR intent_id>$2) ORDER BY intent_id LIMIT $3")
            .bind(as_of).bind(after).bind(limit as i64).fetch_all(&self.pool).await.map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|r| ObservationExtractionReadyItem {
                scope: TenantScope::new(
                    r.get::<Uuid, _>("operator_id").into(),
                    r.get::<Uuid, _>("tenant_id").into(),
                    Some(r.get::<Uuid, _>("project_id").into()),
                ),
                intent_id: r.get("intent_id"),
                state: if r.get::<String, _>("state") == "queued" {
                    ObservationExtractionState::Queued
                } else {
                    ObservationExtractionState::ResponseReceived
                },
            })
            .collect())
    }
    async fn status_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<ObservationExtractionStatus>, AppError> {
        let mut tx = self.transaction(scope).await?;
        // Read only the public status fields. Large model responses and source
        // identities never cross the database boundary for this lookup.
        let row = sqlx::query("SELECT jsonb_build_object('state',e.state,'created_at',e.created_at,'started_at',e.intent->'started_at','finished_at',e.intent->'finished_at','interruption',e.intent->'interruption') AS status FROM observation_captures c JOIN observation_extraction_intents e ON e.operator_id=c.operator_id AND e.tenant_id=c.tenant_id AND e.project_id=c.project_id AND e.source_capture_id=c.capture_id WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3 AND c.target_id=$4 AND c.attempt_id=$5 AND c.phase='source' ORDER BY (e.state IN ('queued','running','response_received')) DESC,e.created_at DESC,e.intent_id DESC LIMIT 1")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope))
            .bind(target_id).bind(attempt_id).fetch_optional(&mut *tx).await.map_err(db)?;
        let status = row
            .map(|row| {
                serde_json::from_value(row.get("status")).map_err(|_| {
                    AppError::new(ErrorCode::Internal, "stored extraction status invalid")
                })
            })
            .transpose()?;
        tx.commit().await.map_err(db)?;
        Ok(status)
    }
    async fn claim(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> Result<Option<ObservationExtractionClaim>, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = self.locked(&mut tx, scope, intent_id).await?;
        let mut intent = decode(&row)?;
        if intent.state != ObservationExtractionState::Queued {
            return Ok(None);
        }
        if started_at < intent.created_at {
            return Err(AppError::invalid_request("invalid extraction start time"));
        }
        let token = Uuid::new_v4();
        intent.state = ObservationExtractionState::Running;
        intent.started_at = Some(started_at);
        write(&mut tx, &intent, token).await?;
        tx.commit().await.map_err(db)?;
        Ok(Some(ObservationExtractionClaim {
            intent,
            claim_token: token,
        }))
    }
    async fn claim_response(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<Option<ObservationExtractionClaim>, AppError> {
        let mut tx = self.transaction(scope).await?;
        let row = self.locked(&mut tx, scope, intent_id).await?;
        let intent = decode(&row)?;
        let result = (intent.state == ObservationExtractionState::ResponseReceived).then(|| {
            ObservationExtractionClaim {
                intent,
                claim_token: row.get("claim_token"),
            }
        });
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    async fn record_response(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        response: ObservationExtractionResponse,
        received_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        self.update(
            scope,
            intent_id,
            claim_token,
            |intent| intent.record_response(response, received_at),
            false,
        )
        .await
    }
    async fn complete(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        result: ObservationAnalysisResult,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        self.update(
            scope,
            intent_id,
            claim_token,
            |intent| intent.complete(result, finished_at),
            false,
        )
        .await
    }
    async fn interrupt(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        reason: ObservationExtractionInterruption,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        self.update(
            scope,
            intent_id,
            claim_token,
            |intent| intent.interrupt(reason, finished_at),
            true,
        )
        .await
    }
}
