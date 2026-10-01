use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, ProjectId, ReportRepository, ReportSnapshot, TenantScope,
    validate_correction,
};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgReportRepository {
    pool: PgPool,
}

impl PgReportRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }

    /// A bounded, trusted scheduler scan. This global selector returns only
    /// scope identifiers; all subsequent reads and writes still use that
    /// explicit scope, and a concurrent replica is serialized on cycle write.
    pub async fn due_scopes(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<(TenantScope, Uuid)>, AppError> {
        Ok(self
            .due_scopes_after(now, None)
            .await?
            .into_iter()
            .map(|(_, scope, cycle)| (scope, cycle))
            .collect())
    }

    /// Keyset cursor makes later due cycles reachable even if earlier ones
    /// repeatedly fail their source reads. Advance the cursor after every
    /// returned page regardless of per-cycle outcome.
    pub async fn due_scopes_after(
        &self,
        now: DateTime<Utc>,
        after: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<(DateTime<Utc>, TenantScope, Uuid)>, AppError> {
        let rows: Vec<(DateTime<Utc>, Uuid, Uuid, Uuid, Uuid)> = sqlx::query_as(
            r#"SELECT c.cutoff_at, c.operator_id, c.tenant_id, c.project_id, c.cycle_id
               FROM optimization_cycles c
               WHERE c.cutoff_at <= $1
                 AND ($2::timestamptz IS NULL OR (c.cutoff_at,c.cycle_id) > ($2,$3))
                 AND NOT EXISTS (
                   SELECT 1 FROM report_snapshots r
                   WHERE r.operator_id=c.operator_id AND r.tenant_id=c.tenant_id
                     AND r.project_id=c.project_id AND r.cycle_id=c.cycle_id
                     AND r.revision=1
               )
               ORDER BY c.cutoff_at, c.cycle_id
               LIMIT 100"#,
        )
        .bind(now)
        .bind(after.map(|(cutoff, _)| cutoff))
        .bind(after.map(|(_, cycle)| cycle))
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(rows
            .into_iter()
            .map(|(cutoff, operator, tenant, project, cycle)| {
                (
                    cutoff,
                    TenantScope::new(operator.into(), tenant.into(), Some(project.into())),
                    cycle,
                )
            })
            .collect())
    }
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        format!("report database operation failed: {error}"),
    )
}

fn decode(value: serde_json::Value) -> Result<ReportSnapshot, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored report snapshot is invalid"))
}

#[async_trait]
impl ReportRepository for PgReportRepository {
    async fn create(
        &self,
        scope: &TenantScope,
        snapshot: ReportSnapshot,
    ) -> Result<ReportSnapshot, AppError> {
        if scope.project_id != Some(snapshot.project_id) {
            return Err(AppError::forbidden(
                "report project is outside tenant scope",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(database_error)?;
        // Serialize corrections/replay against the owning cycle without
        // holding an in-process lock (multiple API/worker replicas may reduce).
        let cycle: Option<Uuid> = sqlx::query_scalar(
            "SELECT cycle_id FROM optimization_cycles WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(snapshot.project_id.as_uuid())
        .bind(snapshot.cycle_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        if cycle.is_none() {
            return Err(AppError::not_found("report cycle was not found"));
        }
        let existing: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT snapshot FROM report_snapshots WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND cycle_id=$4 AND report_window_start_at=$5 AND report_window_end_at=$6 ORDER BY revision",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(snapshot.project_id.as_uuid())
        .bind(snapshot.cycle_id)
        .bind(snapshot.report_window_start_at)
        .bind(snapshot.report_window_end_at)
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
        let existing = existing
            .into_iter()
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(previous) = existing
            .iter()
            .find(|item| item.report_id == snapshot.report_id)
        {
            if previous.input_hash != snapshot.input_hash
                || previous.correction_of != snapshot.correction_of
            {
                return Err(AppError::conflict("report revision inputs differ"));
            }
            return Ok(previous.clone());
        }
        validate_correction(&existing, &snapshot)?;
        let snapshot_json = serde_json::to_value(&snapshot)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot serialize report"))?;
        let manifests_json = serde_json::to_value(&snapshot.input_manifest_versions)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot serialize report manifests"))?;
        sqlx::query(
            "INSERT INTO report_snapshots (report_id,operator_id,tenant_id,project_id,cycle_id,revision,correction_of,report_window_start_at,report_window_end_at,cutoff_at,reducer_version,input_manifest_versions,input_hash,snapshot) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        )
        .bind(snapshot.report_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(snapshot.project_id.as_uuid())
        .bind(snapshot.cycle_id)
        .bind(i32::try_from(snapshot.revision).map_err(|_| AppError::invalid_request("revision exceeds supported range"))?)
        .bind(snapshot.correction_of)
        .bind(snapshot.report_window_start_at)
        .bind(snapshot.report_window_end_at)
        .bind(snapshot.cutoff_at)
        .bind(&snapshot.reducer_version)
        .bind(manifests_json)
        .bind(&snapshot.input_hash)
        .bind(snapshot_json)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }

    async fn list(
        &self,
        scope: &TenantScope,
        project_id: ProjectId,
    ) -> Result<Vec<ReportSnapshot>, AppError> {
        if scope.project_id != Some(project_id) {
            return Err(AppError::forbidden(
                "report project is outside tenant scope",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(database_error)?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT snapshot FROM report_snapshots WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY report_window_end_at DESC, revision DESC",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        rows.into_iter().map(decode).collect()
    }

    async fn get(&self, scope: &TenantScope, report_id: Uuid) -> Result<ReportSnapshot, AppError> {
        let project_id = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(database_error)?;
        let row: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT snapshot FROM report_snapshots WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND report_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(report_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        decode(row.ok_or_else(|| AppError::not_found("report was not found"))?)
    }
}
