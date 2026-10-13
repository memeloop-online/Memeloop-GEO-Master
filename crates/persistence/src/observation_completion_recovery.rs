use async_trait::async_trait;
use geo_domain::{
    AppError, ErrorCode, ObservationCapture, ObservationCaptureReceipt,
    ObservationCompletionRecovery, ObservationCompletionRecoveryInput,
    ObservationCompletionRecoveryReceipt, ObservationCompletionRecoveryRepository, TenantScope,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgObservationCompletionRecoveryRepository {
    pool: PgPool,
}

impl PgObservationCompletionRecoveryRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

fn unavailable(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "completion recovery store unavailable",
    )
}

fn invalid() -> AppError {
    AppError::new(ErrorCode::Internal, "stored completion recovery invalid")
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

async fn source(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    capture_id: Uuid,
) -> Result<ObservationCapture, AppError> {
    let row = sqlx::query(
        "SELECT * FROM observation_captures WHERE operator_id=$1 AND tenant_id=$2 \
         AND project_id=$3 AND capture_id=$4 AND phase='source' FOR SHARE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(capture_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(unavailable)?
    .ok_or_else(|| AppError::forbidden("completion recovery source unavailable"))?;
    let input: geo_domain::ObservationCaptureInput =
        serde_json::from_value(row.get("input")).map_err(|_| invalid())?;
    // Do not trust duplicated JSON identity over authoritative indexed bindings.
    if input.capture_id != capture_id
        || input.target_id != row.get::<Uuid, _>("target_id")
        || input.attempt_id != row.get::<Uuid, _>("attempt_id")
        || input.account_id != row.get::<Uuid, _>("account_id")
        || input.runner_session_id != row.get::<Uuid, _>("runner_session_id")
        || input.ordinal != 0
        || row.get::<i32, _>("ordinal") != 0
    {
        return Err(invalid());
    }
    Ok(ObservationCapture {
        receipt: ObservationCaptureReceipt {
            capture_id,
            schema_version: 1,
            digest_sha256: row.get("input_hash"),
            stored_at: row.get("stored_at"),
        },
        input,
    })
}

async fn read(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    recovery_id: Uuid,
) -> Result<Option<ObservationCompletionRecovery>, AppError> {
    let Some(row) = sqlx::query(
        "SELECT * FROM observation_completion_recoveries WHERE operator_id=$1 \
         AND tenant_id=$2 AND project_id=$3 AND recovery_id=$4",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(recovery_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(unavailable)?
    else {
        return Ok(None);
    };
    let input: ObservationCompletionRecoveryInput =
        serde_json::from_value(row.get("input")).map_err(|_| invalid())?;
    if input.recovery_id != recovery_id
        || input.source_capture_id != row.get::<Uuid, _>("source_capture_id")
        || input.recovery_execution_id != row.get::<Uuid, _>("recovery_execution_id")
    {
        return Err(invalid());
    }
    let original = source(tx, scope, input.source_capture_id).await?;
    let evidence = input.validate(scope, &original).map_err(|_| invalid())?;
    let digest: String = row.get("input_hash");
    if evidence.digest_sha256() != digest {
        return Err(invalid());
    }
    Ok(Some(ObservationCompletionRecovery {
        receipt: ObservationCompletionRecoveryReceipt {
            recovery_id,
            schema_version: 1,
            digest_sha256: digest,
            stored_at: row.get("stored_at"),
        },
        input,
    }))
}

pub(crate) async fn read_by_source(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    source_capture_id: Uuid,
) -> Result<Option<ObservationCompletionRecovery>, AppError> {
    // The existing unique index starts with these scoped source columns.
    // Multiple executions may retain evidence; never silently skip an invalid
    // latest record in favour of an older record.
    let recovery_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT recovery_id FROM observation_completion_recoveries \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_capture_id=$4 \
         ORDER BY stored_at DESC,recovery_id DESC LIMIT 1",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project(scope)?)
    .bind(source_capture_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(unavailable)?;
    match recovery_id {
        Some(id) => read(tx, scope, id).await,
        None => Ok(None),
    }
}

#[async_trait]
impl ObservationCompletionRecoveryRepository for PgObservationCompletionRecoveryRepository {
    async fn save(
        &self,
        scope: &TenantScope,
        input: ObservationCompletionRecoveryInput,
    ) -> Result<ObservationCompletionRecoveryReceipt, AppError> {
        let project_id = project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        if let Some(stored) = read(&mut tx, scope, input.recovery_id).await? {
            if stored.input != input {
                return Err(AppError::conflict("completion recovery identity differs"));
            }
            tx.commit().await.map_err(unavailable)?;
            return Ok(stored.receipt);
        }
        let original = source(&mut tx, scope, input.source_capture_id).await?;
        let evidence = input.validate(scope, &original)?;
        sqlx::query(
            "INSERT INTO observation_completion_recoveries \
             (recovery_id,operator_id,tenant_id,project_id,source_capture_id,recovery_execution_id,input_hash,input) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING",
        )
        .bind(input.recovery_id).bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid()).bind(project_id).bind(input.source_capture_id)
        .bind(input.recovery_execution_id).bind(evidence.digest_sha256())
        .bind(serde_json::to_value(&input).map_err(|_| invalid())?)
        .execute(&mut *tx).await.map_err(unavailable)?;
        let stored = read(&mut tx, scope, input.recovery_id)
            .await?
            .ok_or_else(|| AppError::conflict("completion recovery identity differs"))?;
        if stored.input != input || stored.receipt.digest_sha256 != evidence.digest_sha256() {
            return Err(AppError::conflict("completion recovery identity differs"));
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(stored.receipt)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        recovery_id: Uuid,
    ) -> Result<Option<ObservationCompletionRecovery>, AppError> {
        project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let stored = read(&mut tx, scope, recovery_id).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(stored)
    }

    async fn get_by_source(
        &self,
        scope: &TenantScope,
        source_capture_id: Uuid,
    ) -> Result<Option<ObservationCompletionRecovery>, AppError> {
        project(scope)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let stored = read_by_source(&mut tx, scope, source_capture_id).await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(stored)
    }
}
