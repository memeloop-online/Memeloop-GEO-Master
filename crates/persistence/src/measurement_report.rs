use geo_domain::{
    AppError, ErrorCode, MeasurementPeriodReport, TenantScope,
    validate_measurement_period_correction,
};
use uuid::Uuid;

use crate::PgReportRepository;

fn database_error(error: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        format!("measurement report database operation failed: {error}"),
    )
}

fn decode(value: serde_json::Value) -> Result<MeasurementPeriodReport, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored measurement report is invalid"))
}

impl PgReportRepository {
    pub(crate) async fn save_measurement_period(
        &self,
        scope: &TenantScope,
        snapshot: MeasurementPeriodReport,
    ) -> Result<MeasurementPeriodReport, AppError> {
        if scope.project_id != Some(snapshot.project_id) {
            return Err(AppError::forbidden("measurement report outside project"));
        }
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(database_error)?;
        // A project exists before any cycle or enterprise setup. This lock
        // fences first-create and correction races across replicas.
        let project: Option<Uuid> = sqlx::query_scalar(
            "SELECT project_id FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(snapshot.project_id.as_uuid()).fetch_optional(&mut *tx).await.map_err(database_error)?;
        if project.is_none() {
            return Err(AppError::not_found("project not found"));
        }
        let values: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT snapshot FROM measurement_period_reports WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND report_window_start_at=$4 AND report_window_end_at=$5 AND report_timezone=$6 ORDER BY revision",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(snapshot.project_id.as_uuid()).bind(snapshot.report_window_start_at)
            .bind(snapshot.report_window_end_at).bind(&snapshot.report_timezone)
            .fetch_all(&mut *tx).await.map_err(database_error)?;
        let rows = values
            .into_iter()
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(row) = rows.iter().find(|row| {
            row.revision == snapshot.revision && row.correction_of == snapshot.correction_of
        }) {
            return Ok(row.clone());
        }
        validate_measurement_period_correction(&rows, &snapshot)?;
        let value = serde_json::to_value(&snapshot).map_err(|_| {
            AppError::new(ErrorCode::Internal, "cannot serialize measurement report")
        })?;
        sqlx::query(
            "INSERT INTO measurement_period_reports (report_id,operator_id,tenant_id,project_id,revision,correction_of,report_window_start_at,report_window_end_at,report_timezone,evidence_as_of,snapshot) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        ).bind(snapshot.report_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(snapshot.project_id.as_uuid()).bind(snapshot.revision as i32).bind(snapshot.correction_of)
            .bind(snapshot.report_window_start_at).bind(snapshot.report_window_end_at)
            .bind(&snapshot.report_timezone).bind(snapshot.evidence_as_of).bind(value)
            .execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }

    pub(crate) async fn read_measurement_periods(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<MeasurementPeriodReport>, AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(database_error)?;
        let values: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT snapshot FROM measurement_period_reports WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY report_window_end_at DESC,revision DESC,report_id",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .fetch_all(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        values.into_iter().map(decode).collect()
    }
}
