//! Write-once inference responses with scoped atomic claims and no re-inference.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, AutomaticExtractionAdmission, ChannelJobRepository, ErrorCode,
    ManualExtractionAdmission, ObservationAnalysisOutcome, ObservationAnalysisRequest,
    ObservationAnalysisResult, ObservationAnalysisSource, ObservationCaptureRepository,
    ObservationExtractionBackfillItem, ObservationExtractionClaim, ObservationExtractionIntent,
    ObservationExtractionInterruption, ObservationExtractionReadyItem,
    ObservationExtractionRepository, ObservationExtractionRequest, ObservationExtractionResponse,
    ObservationExtractionState, ObservationExtractionStatus, TenantScope,
    observation_analysis_source_json, sha256_hex, validate_observation_extraction_page_size,
    validate_observation_extraction_scope,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgObservationExtractionRepository {
    pool: PgPool,
    captures: crate::PgObservationCaptureRepository,
    jobs: crate::PgChannelJobRepository,
}

impl PgObservationExtractionRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
            captures: crate::PgObservationCaptureRepository::from_database(database),
            jobs: crate::PgChannelJobRepository::from_database(database),
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
        let binding = sqlx::query("SELECT intent FROM observation_extraction_intents WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND intent_id=$4")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(id)
            .fetch_optional(&mut **tx).await.map_err(db)?.ok_or_else(|| AppError::not_found("extraction intent not found"))?;
        let intent = decode(&binding)?;
        super::analysis::lock_admission_sources(
            tx,
            scope,
            project(scope),
            intent.request.target_id,
            intent.request.attempt_id,
        )
        .await?;
        if let Some(revision_id) = intent.request.manual_revision_id {
            sqlx::query("SELECT revision_id FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND revision_id=$4 FOR UPDATE")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(revision_id)
                .fetch_one(&mut **tx).await.map_err(db)?;
        }
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
        interruption_result: Option<(ObservationAnalysisResult, bool)>,
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
            sync_manual(&mut tx, scope, &intent, token, interruption_result).await?;
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
fn automatic_capture(request: &ObservationExtractionRequest) -> Result<Uuid, AppError> {
    if request.manual_revision_id.is_some() || request.source_evidence_index.is_some() {
        return Err(AppError::invalid_request(
            "automatic extraction requires capture",
        ));
    }
    request
        .source_capture_id
        .ok_or_else(|| AppError::invalid_request("automatic extraction requires capture"))
}

async fn sync_manual(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    intent: &ObservationExtractionIntent,
    token: Uuid,
    interruption_result: Option<(ObservationAnalysisResult, bool)>,
) -> Result<(), AppError> {
    let Some(revision_id) = intent.request.manual_revision_id else {
        return Ok(());
    };
    let (analysis_state, result, resolved) = match intent.state {
        ObservationExtractionState::Running => ("running", None, false),
        ObservationExtractionState::ResponseReceived => ("running", None, true),
        ObservationExtractionState::Completed => ("completed", intent.completion.clone(), true),
        ObservationExtractionState::Interrupted => {
            let (mut result, proof) = interruption_result
                .ok_or_else(|| AppError::conflict("interruption result required"))?;
            if let Some(response) = &intent.response {
                result.actual_model = Some(response.actual_model.clone());
                result.config_revision = response.config_revision;
                result.prompt_tokens = response.prompt_tokens;
                result.completion_tokens = response.completion_tokens;
            }
            (
                "completed",
                Some(result),
                proof || intent.response.is_some(),
            )
        }
        ObservationExtractionState::Queued => return Ok(()),
    };
    let result = result
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| AppError::invalid_request("invalid analysis result"))?;
    let updated = sqlx::query("UPDATE observation_analyses SET state=$5,started_at=$6,analyzed_at=$7,result=$8,inference_resolved=inference_resolved OR $9,claim_token=$10 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND revision_id=$4 AND (state='queued' OR (state='running' AND claim_token=$10))")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(revision_id)
        .bind(analysis_state).bind(intent.started_at).bind(intent.finished_at).bind(result).bind(resolved).bind(token)
        .execute(&mut **tx).await.map_err(db)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("linked analysis lifecycle differs"));
    }
    Ok(())
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
    async fn admit_manual(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        created_at: DateTime<Utc>,
    ) -> Result<ManualExtractionAdmission, AppError> {
        request.validate(scope, idempotency_key, request_digest, created_at)?;
        let target = self.jobs.get_target(scope, request.target_id).await?;
        let capture = match request.source {
            ObservationAnalysisSource::Capture { capture_id } => {
                self.captures.get(scope, capture_id).await?
            }
            ObservationAnalysisSource::AttemptEvidence { .. } => None,
        };
        observation_analysis_source_json(scope, &request, &target, capture.as_ref())?;
        let mut tx = self.transaction(scope).await?;
        super::analysis::lock_admission_sources(
            &mut tx,
            scope,
            project(scope),
            request.target_id,
            request.attempt_id,
        )
        .await?;
        let key_hash = sha256_hex(idempotency_key.as_bytes());
        let inserted = sqlx::query("INSERT INTO observation_analyses (revision_id,operator_id,tenant_id,project_id,target_id,attempt_id,idempotency_key_hash,request_digest,request,state,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'queued',$10) ON CONFLICT DO NOTHING RETURNING *")
            .bind(request.revision_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope))
            .bind(request.target_id).bind(request.attempt_id).bind(&key_hash).bind(request_digest)
            .bind(serde_json::to_value(&request).map_err(|_| AppError::invalid_request("invalid analysis request"))?)
            .bind(created_at).fetch_optional(&mut *tx).await.map_err(db)?;
        let admission = if let Some(row) = inserted {
            let revision = super::analysis::decode(&row)?;
            let extraction = ObservationExtractionRequest::for_manual(&revision.request);
            let key = extraction.intent_key(scope)?;
            let intent = ObservationExtractionIntent::queued(extraction, key, created_at);
            sqlx::query("INSERT INTO observation_extraction_intents (intent_id,operator_id,tenant_id,project_id,source_capture_id,source_evidence_index,manual_revision_id,intent_key,state,created_at,intent) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'queued',$9,$10)")
                .bind(intent.intent_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope))
                .bind(intent.request.source_capture_id).bind(intent.request.source_evidence_index.map(i64::from)).bind(intent.request.manual_revision_id)
                .bind(&intent.intent_key).bind(created_at).bind(value(&intent)?).execute(&mut *tx).await.map_err(db)?;
            ManualExtractionAdmission {
                revision,
                intent,
            }
        } else {
            let row = sqlx::query("SELECT * FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND idempotency_key_hash=$4")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(key_hash)
                .fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(|| AppError::conflict("analysis revision identity already exists"))?;
            let revision = super::analysis::decode(&row)?;
            if revision.request_digest != request_digest || !revision.request.same_input(&request) {
                return Err(AppError::conflict("analysis idempotency request differs"));
            }
            let row = sqlx::query("SELECT * FROM observation_extraction_intents WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND manual_revision_id=$4")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)).bind(revision.request.revision_id)
                .fetch_optional(&mut *tx).await.map_err(db)?;
            ManualExtractionAdmission {
                revision,
                intent: decode(&row.ok_or_else(|| {
                    AppError::conflict("analysis has no linked extraction intent")
                })?)?,
            }
        };
        tx.commit().await.map_err(db)?;
        Ok(admission)
    }
    async fn admit_automatic(
        &self,
        scope: &TenantScope,
        request: ObservationExtractionRequest,
        created_at: DateTime<Utc>,
    ) -> Result<AutomaticExtractionAdmission, AppError> {
        let key = request.intent_key(scope)?;
        let capture = self
            .captures
            .get(scope, automatic_capture(&request)?)
            .await?
            .ok_or_else(|| AppError::not_found("extraction source not found"))?;
        request.validate_source(scope, &capture)?;
        let mut tx = self.transaction(scope).await?;
        super::analysis::lock_admission_sources(
            &mut tx,
            scope,
            project(scope),
            request.target_id,
            request.attempt_id,
        )
        .await?;
        let existing = sqlx::query("SELECT * FROM observation_extraction_intents WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_capture_id=$4 ORDER BY created_at,intent_id LIMIT 1")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope))
            .bind(request.source_capture_id).fetch_optional(&mut *tx).await.map_err(db)?;
        if let Some(row) = existing {
            let intent = decode(&row)?;
            tx.commit().await.map_err(db)?;
            return Ok(AutomaticExtractionAdmission::Intent(Box::new(intent)));
        }
        if super::analysis::unresolved_for_attempt(
            &mut tx,
            scope,
            project(scope),
            request.target_id,
            request.attempt_id,
        )
        .await?
        {
            tx.commit().await.map_err(db)?;
            return Ok(AutomaticExtractionAdmission::DeferredManual);
        }
        let intent = ObservationExtractionIntent::queued(request, key, created_at);
        sqlx::query("INSERT INTO observation_extraction_intents (intent_id,operator_id,tenant_id,project_id,source_capture_id,intent_key,state,created_at,intent) VALUES ($1,$2,$3,$4,$5,$6,'queued',$7,$8)")
            .bind(intent.intent_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope))
            .bind(intent.request.source_capture_id).bind(&intent.intent_key).bind(created_at).bind(value(&intent)?)
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(AutomaticExtractionAdmission::Intent(Box::new(intent)))
    }
    async fn scan_unqueued_sources(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationExtractionBackfillItem>, AppError> {
        validate_observation_extraction_page_size(limit)?;
        let rows = sqlx::query(
            "SELECT c.operator_id,c.tenant_id,c.project_id,c.capture_id \
             FROM observation_captures c \
             WHERE c.phase='source' AND c.ordinal=0 AND c.stored_at<=$1 \
             AND ($2::uuid IS NULL OR c.capture_id>$2) \
             AND NOT EXISTS (SELECT 1 FROM observation_extraction_intents e \
                 WHERE e.operator_id=c.operator_id AND e.tenant_id=c.tenant_id \
                 AND e.project_id=c.project_id AND e.source_capture_id=c.capture_id) \
             ORDER BY c.capture_id LIMIT $3",
        )
        .bind(as_of)
        .bind(after)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| ObservationExtractionBackfillItem {
                scope: TenantScope::new(
                    row.get::<Uuid, _>("operator_id").into(),
                    row.get::<Uuid, _>("tenant_id").into(),
                    Some(row.get::<Uuid, _>("project_id").into()),
                ),
                capture_id: row.get("capture_id"),
            })
            .collect())
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
        let row = sqlx::query("SELECT jsonb_build_object('state',e.state,'created_at',e.created_at,'started_at',e.intent->'started_at','finished_at',e.intent->'finished_at','interruption',e.intent->'interruption') AS status FROM observation_extraction_intents e WHERE e.operator_id=$1 AND e.tenant_id=$2 AND e.project_id=$3 AND e.intent->'request'->>'target_id'=$4::uuid::text AND e.intent->'request'->>'attempt_id'=$5::uuid::text ORDER BY (e.state IN ('queued','running','response_received')) DESC,e.created_at DESC,e.intent_id DESC LIMIT 1")
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
        sync_manual(&mut tx, scope, &intent, token, None).await?;
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
            None,
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
            None,
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
        self.interrupt_with_result(
            scope,
            intent_id,
            claim_token,
            reason,
            ObservationAnalysisResult {
                config_revision: None,
                actual_model: None,
                candidate_json: None,
                outcome: ObservationAnalysisOutcome::Failed {
                    code: "analysis_interrupted".into(),
                },
                prompt_tokens: 0,
                completion_tokens: 0,
            },
            false,
            finished_at,
        )
        .await
    }
    async fn interrupt_with_result(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
        claim_token: Uuid,
        reason: ObservationExtractionInterruption,
        result: ObservationAnalysisResult,
        inference_resolved: bool,
        finished_at: DateTime<Utc>,
    ) -> Result<ObservationExtractionIntent, AppError> {
        result.validate()?;
        if !matches!(&result.outcome, ObservationAnalysisOutcome::Failed { .. }) {
            return Err(AppError::invalid_request(
                "interruption requires failure result",
            ));
        }
        self.update(
            scope,
            intent_id,
            claim_token,
            |intent| intent.interrupt(reason, finished_at),
            true,
            Some((result, inference_resolved)),
        )
        .await
    }
}
