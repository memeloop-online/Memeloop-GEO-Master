use async_trait::async_trait;
use geo_domain::{
    AppError, ChannelTarget, ChannelTargetInput, ErrorCode, ObservationCapture,
    ObservationCaptureInput, ObservationCaptureReceipt, ObservationCaptureRepository,
    ObservationCaptureSnapshot, TenantScope,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgObservationCaptureRepository {
    pool: PgPool,
}

impl PgObservationCaptureRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

fn unavailable(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "observation capture store unavailable",
    )
}

fn scope_project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

fn decode(row: sqlx::postgres::PgRow) -> Result<ObservationCapture, AppError> {
    let input: ObservationCaptureInput = serde_json::from_value(row.get("input"))
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored observation capture invalid"))?;
    Ok(ObservationCapture {
        receipt: ObservationCaptureReceipt {
            capture_id: input.capture_id,
            schema_version: 1,
            digest_sha256: row.get("input_hash"),
            stored_at: row.get("stored_at"),
        },
        input,
    })
}

#[async_trait]
impl ObservationCaptureRepository for PgObservationCaptureRepository {
    async fn list_sources_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Vec<ObservationCapture>, AppError> {
        let project = scope_project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let rows = sqlx::query(
            "SELECT input,input_hash,stored_at FROM observation_captures \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND target_id=$4 AND attempt_id=$5 AND phase='source' \
             ORDER BY (input->>'observed_at')::timestamptz DESC,capture_id LIMIT 100",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project)
        .bind(target_id)
        .bind(attempt_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(unavailable)?;
        let items = rows
            .into_iter()
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(items)
    }
    async fn save(
        &self,
        scope: &TenantScope,
        input: ObservationCaptureInput,
    ) -> Result<ObservationCaptureReceipt, AppError> {
        let digest = input.validate(scope)?;
        let project_id = scope_project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let frozen_input: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT targets.frozen_input FROM channel_execution_attempts attempts \
             JOIN channel_execution_targets targets ON targets.operator_id=attempts.operator_id \
               AND targets.tenant_id=attempts.tenant_id AND targets.project_id=attempts.project_id \
               AND targets.target_id=attempts.target_id \
             WHERE attempts.operator_id=$1 AND attempts.tenant_id=$2 AND attempts.project_id=$3 \
               AND attempts.target_id=$4 AND attempts.attempt_id=$5 AND attempts.account_id=$6 \
               AND targets.kind='measure' FOR SHARE OF attempts",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(input.target_id)
        .bind(input.attempt_id)
        .bind(input.account_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        let Some(frozen_input) = frozen_input else {
            return Err(AppError::forbidden(
                "observation attempt or account mismatch",
            ));
        };
        // Channel jobs persist the complete target envelope, not its input.
        let target: ChannelTarget = serde_json::from_value(frozen_input)
            .map_err(|_| AppError::new(ErrorCode::Internal, "stored measurement target invalid"))?;
        if target.target_id != input.target_id
            || !matches!(&target.input, ChannelTargetInput::Measure { account_id, .. } if *account_id == input.account_id)
        {
            return Err(AppError::forbidden("observation target binding mismatch"));
        }
        if let Some(conversation) = &input.owned_conversation
            && !matches!(&target.input, ChannelTargetInput::Measure { provider, .. } if provider == &conversation.provider)
        {
            return Err(AppError::forbidden("observation provider mismatch"));
        }
        let (phase, source_capture_id) = match &input.snapshot {
            ObservationCaptureSnapshot::Source { .. } => ("source", None),
            ObservationCaptureSnapshot::Candidate {
                source_capture_id, ..
            }
            | ObservationCaptureSnapshot::Extraction {
                source_capture_id, ..
            } => {
                let source: Option<(Uuid, Uuid, String)> = sqlx::query_as(
                    "SELECT attempt_id,runner_session_id,phase FROM observation_captures \
                     WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND capture_id=$4",
                )
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project_id)
                .bind(source_capture_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(unavailable)?;
                if source.as_ref()
                    != Some(&(input.attempt_id, input.runner_session_id, "source".into()))
                {
                    return Err(AppError::conflict("candidate source capture mismatch"));
                }
                let phase = if matches!(
                    &input.snapshot,
                    ObservationCaptureSnapshot::Extraction { .. }
                ) {
                    "extraction"
                } else {
                    "candidate"
                };
                (phase, Some(*source_capture_id))
            }
        };
        let body = serde_json::to_value(&input)
            .map_err(|_| AppError::invalid_request("invalid observation capture"))?;
        sqlx::query(
            "INSERT INTO observation_captures \
             (capture_id,operator_id,tenant_id,project_id,target_id,attempt_id,account_id, \
              runner_session_id,ordinal,phase,source_capture_id,input_hash,input) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) \
             ON CONFLICT DO NOTHING",
        )
        .bind(input.capture_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(input.target_id)
        .bind(input.attempt_id)
        .bind(input.account_id)
        .bind(input.runner_session_id)
        .bind(i64::from(input.ordinal))
        .bind(phase)
        .bind(source_capture_id)
        .bind(&digest)
        .bind(body)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
        let row = sqlx::query(
            "SELECT input,input_hash,stored_at FROM observation_captures \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND capture_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(input.capture_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| AppError::conflict("observation capture identity differs"))?;
        let capture = decode(row)?;
        if capture.input != input || capture.receipt.digest_sha256 != digest {
            return Err(AppError::conflict("observation capture identity differs"));
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(capture.receipt)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        capture_id: Uuid,
    ) -> Result<Option<ObservationCapture>, AppError> {
        let row = sqlx::query(
            "SELECT input,input_hash,stored_at FROM observation_captures \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND capture_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope_project(scope)?)
        .bind(capture_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(unavailable)?;
        row.map(decode).transpose()
    }
}
