//! Scoped write-once results for saved-evidence analysis.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelJobRepository, ErrorCode, MAX_ANALYSIS_PROJECTION_ATTEMPTS,
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

pub(crate) fn decode(row: &sqlx::postgres::PgRow) -> Result<ObservationAnalysisRevision, AppError> {
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
        inference_resolved: row.get("inference_resolved"),
    })
}

#[async_trait]
impl ObservationAnalysisRepository for PgObservationAnalysisRepository {
    async fn has_unresolved_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<bool, AppError> {
        let project = project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let blocked =
            unresolved_for_attempt(&mut tx, scope, project, target_id, attempt_id).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(blocked)
    }
    async fn project_response(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        result: ObservationAnalysisResult,
        analyzed_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError> {
        request.validate(scope, idempotency_key, request_digest, analyzed_at)?;
        result.validate()?;
        let project = project(scope)?;
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
        lock_admission_sources(
            &mut tx,
            scope,
            project,
            request.target_id,
            request.attempt_id,
        )
        .await?;
        let inserted = sqlx::query(
            "INSERT INTO observation_analyses \
             (revision_id,operator_id,tenant_id,project_id,target_id,attempt_id,idempotency_key_hash,\
              request_digest,request,state,created_at,started_at,analyzed_at,claim_token,result,inference_resolved) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'completed',$10,$10,$10,$11,$12,TRUE) \
             ON CONFLICT DO NOTHING RETURNING *",
        )
        .bind(request.revision_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project).bind(request.target_id).bind(request.attempt_id).bind(&key_hash)
        .bind(request_digest)
        .bind(serde_json::to_value(&request).map_err(|_| AppError::invalid_request("invalid analysis request"))?)
        .bind(analyzed_at).bind(Uuid::new_v4())
        .bind(serde_json::to_value(&result).map_err(|_| AppError::invalid_request("invalid analysis result"))?)
        .fetch_optional(&mut *tx).await.map_err(unavailable)?;
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
                if prior.request_digest != request_digest
                    || !prior.request.same_input(&request)
                    || prior.state != ObservationAnalysisState::Completed
                    || prior.result.as_ref() != Some(&result)
                {
                    return Err(AppError::conflict("analysis projection already differs"));
                }
                prior
            }
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(revision)
    }
    async fn latest_grounded_for_attempts(
        &self,
        scope: &TenantScope,
        attempts: &[(Uuid, Uuid)],
        as_of: DateTime<Utc>,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError> {
        let project = project(scope)?;
        if attempts.len() > MAX_ANALYSIS_PROJECTION_ATTEMPTS
            || attempts
                .iter()
                .any(|(target, attempt)| target.is_nil() || attempt.is_nil())
        {
            return Err(AppError::invalid_request(
                "invalid analysis projection batch",
            ));
        }
        if attempts.is_empty() {
            return Ok(Vec::new());
        }
        let (targets, attempts): (Vec<_>, Vec<_>) = attempts.iter().copied().unzip();
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        // One indexed seek per distinct requested pair. Apply eligibility before
        // LIMIT: newer failures and post-cutoff completions cannot hide evidence.
        let rows = sqlx::query(
            "SELECT revision.* FROM (\
               SELECT DISTINCT target_id,attempt_id \
               FROM unnest($4::uuid[],$5::uuid[]) AS pairs(target_id,attempt_id)\
             ) requested CROSS JOIN LATERAL (\
               SELECT a.* FROM observation_analyses a \
               WHERE a.operator_id=$1 AND a.tenant_id=$2 AND a.project_id=$3 \
               AND a.target_id=requested.target_id AND a.attempt_id=requested.attempt_id \
               AND a.state='completed' AND a.result->'outcome'->>'status'='grounded' \
               AND a.created_at<=$6 AND a.analyzed_at<=$6 \
               AND (a.request->>'observed_at')::timestamptz<=$6 \
               ORDER BY a.created_at DESC,a.revision_id DESC LIMIT 1\
             ) revision ORDER BY revision.target_id,revision.attempt_id",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(targets)
        .bind(attempts)
        .bind(as_of)
        .fetch_all(&mut *tx)
        .await
        .map_err(unavailable)?;
        let revisions = rows.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(revisions)
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

/// Shared order with automatic admission; lock all immutable source rows before
/// inspecting either ledger, including manual attempt-evidence requests.
pub(crate) async fn lock_admission_sources(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &TenantScope,
    project: Uuid,
    target_id: Uuid,
    attempt_id: Uuid,
) -> Result<(), AppError> {
    // Stable parent exists even before a first source capture arrives.
    sqlx::query("SELECT attempt_id FROM channel_execution_attempts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 AND attempt_id=$5 FOR UPDATE")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project)
        .bind(target_id).bind(attempt_id).fetch_optional(&mut **tx).await.map_err(unavailable)?;
    sqlx::query("SELECT capture_id FROM observation_captures WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 AND attempt_id=$5 AND phase='source' ORDER BY capture_id FOR UPDATE")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project)
        .bind(target_id).bind(attempt_id).fetch_all(&mut **tx).await.map_err(unavailable)?;
    Ok(())
}

pub(crate) async fn unresolved_for_attempt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &TenantScope,
    project: Uuid,
    target_id: Uuid,
    attempt_id: Uuid,
) -> Result<bool, AppError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM observation_analyses WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4 AND attempt_id=$5 AND (state <> 'completed' OR (NOT inference_resolved AND COALESCE(result->'outcome'->>'status','') NOT IN ('grounded','unverified'))))")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project)
        .bind(target_id).bind(attempt_id).fetch_one(&mut **tx).await.map_err(unavailable)
}
