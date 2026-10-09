//! Scoped queued intents and write-once results for saved-evidence analysis.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelJobRepository, ErrorCode, ObservationAnalysisClaim,
    ObservationAnalysisRepository, ObservationAnalysisRequest, ObservationAnalysisResult,
    ObservationAnalysisRevision, ObservationAnalysisSource, ObservationAnalysisState,
    ObservationCaptureRepository, TenantScope, observation_analysis_source_json, sha256_hex,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgObservationAnalysisRepository {
    pool: PgPool,
    jobs: crate::PgChannelJobRepository,
    captures: crate::PgObservationCaptureRepository,
}

impl PgObservationAnalysisRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
            jobs: crate::PgChannelJobRepository::from_database(database),
            captures: crate::PgObservationCaptureRepository::from_database(database),
        }
    }
}

fn unavailable(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "observation analysis store unavailable",
    )
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<ObservationAnalysisRevision, AppError> {
    let invalid = || AppError::new(ErrorCode::Internal, "stored observation analysis invalid");
    let request = serde_json::from_value(row.get("request")).map_err(|_| invalid())?;
    let state = match row.get::<String, _>("state").as_str() {
        "queued" => ObservationAnalysisState::Queued,
        "running" => ObservationAnalysisState::Running,
        "completed" => ObservationAnalysisState::Completed,
        _ => return Err(invalid()),
    };
    let result = row
        .get::<Option<serde_json::Value>, _>("result")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| invalid())?;
    Ok(ObservationAnalysisRevision {
        request,
        request_digest: row.get("request_digest"),
        state,
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        analyzed_at: row.get("analyzed_at"),
        result,
    })
}

#[async_trait]
impl ObservationAnalysisRepository for PgObservationAnalysisRepository {
    async fn interrupt_stale(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        cutoff: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let project = project(scope)?;
        if cutoff > now {
            return Err(AppError::invalid_request(
                "invalid analysis interruption time",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        // Rotate rather than clear the token to satisfy the completed-row
        // invariant while rejecting a late completion from the previous owner.
        sqlx::query(
            "UPDATE observation_analyses SET state='completed',claim_token=$8,\
             started_at=COALESCE(started_at,created_at),analyzed_at=$7,\
             result='{\"actual_model\":null,\"candidate_json\":null,\"outcome\":{\"status\":\"failed\",\"code\":\"analysis_interrupted\"},\"prompt_tokens\":0,\"completion_tokens\":0}'::jsonb \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 \
             AND attempt_id=$5 AND state IN ('queued','running') \
             AND COALESCE(started_at,created_at)<=$6",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project)
            .bind(target_id).bind(attempt_id).bind(cutoff).bind(now).bind(Uuid::new_v4())
            .execute(&mut *tx).await.map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }
    async fn create(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        created_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError> {
        request.validate(scope, idempotency_key, request_digest, created_at)?;
        let project = project(scope)?;
        // Both sources are immutable. The original attempt is already terminal;
        // no read/write race with inference may change its underlying evidence.
        let target = self.jobs.get_target(scope, request.target_id).await?;
        let capture = match request.source {
            ObservationAnalysisSource::Capture { capture_id } => {
                self.captures.get(scope, capture_id).await?
            }
            ObservationAnalysisSource::AttemptEvidence { .. } => None,
        };
        observation_analysis_source_json(scope, &request, &target, capture.as_ref())?;
        let key_hash = sha256_hex(idempotency_key.as_bytes());
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let inserted = sqlx::query(
            "INSERT INTO observation_analyses \
             (revision_id,operator_id,tenant_id,project_id,target_id,attempt_id,idempotency_key_hash,\
              request_digest,request,state,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'queued',$10) \
             ON CONFLICT DO NOTHING RETURNING *",
        )
        .bind(request.revision_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project).bind(request.target_id).bind(request.attempt_id).bind(&key_hash)
        .bind(request_digest).bind(serde_json::to_value(&request).map_err(|_| AppError::invalid_request("invalid analysis request"))?)
        .bind(created_at).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let revision = match inserted {
            Some(row) => decode(&row)?,
            None => {
                let row = sqlx::query(
                    "SELECT * FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 \
                     AND project_id=$3 AND idempotency_key_hash=$4",
                )
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project)
                .bind(key_hash)
                .fetch_optional(&mut *tx)
                .await
                .map_err(unavailable)?
                .ok_or_else(|| AppError::conflict("analysis revision identity already exists"))?;
                let prior = decode(&row)?;
                if prior.request_digest != request_digest || !prior.request.same_input(&request) {
                    return Err(AppError::conflict("analysis idempotency request differs"));
                }
                prior
            }
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(revision)
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> Result<Option<ObservationAnalysisClaim>, AppError> {
        let project = project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let row = sqlx::query(
            "SELECT * FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND revision_id=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(revision_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| AppError::not_found("analysis revision not found"))?;
        let revision = decode(&row)?;
        if revision.state != ObservationAnalysisState::Queued {
            return Ok(None);
        }
        if started_at < revision.created_at {
            return Err(AppError::invalid_request(
                "analysis start precedes acceptance",
            ));
        }
        let claim_token = Uuid::new_v4();
        let row = sqlx::query(
            "UPDATE observation_analyses SET state='running',started_at=$5,claim_token=$6 \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND revision_id=$4 \
             AND state='queued' RETURNING *",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(revision_id)
        .bind(started_at)
        .bind(claim_token)
        .fetch_one(&mut *tx)
        .await
        .map_err(unavailable)?;
        let revision = decode(&row)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(Some(ObservationAnalysisClaim {
            revision,
            claim_token,
        }))
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
        claim_token: Uuid,
        result: ObservationAnalysisResult,
        analyzed_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError> {
        let project = project(scope)?;
        result.validate()?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let row = sqlx::query(
            "SELECT * FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND revision_id=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(revision_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| AppError::not_found("analysis revision not found"))?;
        let revision = decode(&row)?;
        if row.get::<Option<Uuid>, _>("claim_token") != Some(claim_token)
            || revision
                .started_at
                .is_none_or(|started| analyzed_at < started)
        {
            return Err(AppError::conflict(
                "analysis claim or completion time differs",
            ));
        }
        if revision.state == ObservationAnalysisState::Completed {
            return if revision.result.as_ref() == Some(&result)
                && revision
                    .analyzed_at
                    .is_some_and(|at| at.timestamp_micros() == analyzed_at.timestamp_micros())
            {
                Ok(revision)
            } else {
                Err(AppError::conflict("analysis result already recorded"))
            };
        }
        if revision.state != ObservationAnalysisState::Running {
            return Err(AppError::conflict("analysis revision not running"));
        }
        let row = sqlx::query(
            "UPDATE observation_analyses SET state='completed',analyzed_at=$6,result=$7 \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND revision_id=$4 \
             AND claim_token=$5 AND state='running' RETURNING *",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(revision_id)
        .bind(claim_token)
        .bind(analyzed_at)
        .bind(
            serde_json::to_value(result)
                .map_err(|_| AppError::invalid_request("invalid analysis result"))?,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(unavailable)?;
        let revision = decode(&row)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(revision)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Option<ObservationAnalysisRevision>, AppError> {
        let project = project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let row = sqlx::query(
            "SELECT * FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND revision_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(revision_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        let result = row.as_ref().map(decode).transpose()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }

    async fn list_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        after_revision_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError> {
        let project = project(scope)?;
        if !(1..=100).contains(&limit) {
            return Err(AppError::invalid_request("invalid analysis page size"));
        }
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        if let Some(cursor) = after_revision_id {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM observation_analyses WHERE operator_id=$1 \
                 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 AND attempt_id=$5 AND revision_id=$6)",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project)
                .bind(target_id).bind(attempt_id).bind(cursor).fetch_one(&mut *tx).await.map_err(unavailable)?;
            if !exists {
                return Err(AppError::invalid_request("invalid analysis cursor"));
            }
        }
        let rows = sqlx::query(
            "SELECT * FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND target_id=$4 AND attempt_id=$5 \
             AND ($6::uuid IS NULL OR (created_at,revision_id)<(\
                 SELECT created_at,revision_id FROM observation_analyses \
                 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
                 AND target_id=$4 AND attempt_id=$5 AND revision_id=$6)) \
             ORDER BY created_at DESC,revision_id DESC LIMIT $7",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(target_id)
        .bind(attempt_id)
        .bind(after_revision_id)
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(unavailable)?;
        let revisions = rows.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(revisions)
    }
}
