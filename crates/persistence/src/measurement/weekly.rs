use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, MeasurementPeriodReport, MeasurementPeriodWindow,
    MeasurementWeeklySchedule, ProjectSettings, TenantScope, first_measurement_week,
    next_measurement_week,
};
use uuid::Uuid;

use crate::PgReportRepository;

fn database_error(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "weekly report database operation failed",
    )
}

#[derive(sqlx::FromRow)]
struct ScheduleRow {
    schedule_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    report_window_start_at: DateTime<Utc>,
    report_window_end_at: DateTime<Utc>,
    report_timezone: String,
    evidence_as_of: DateTime<Utc>,
    report_at: DateTime<Utc>,
    due_at: DateTime<Utc>,
    report_schedule: serde_json::Value,
    report_id: Option<Uuid>,
}

impl ScheduleRow {
    fn decode(self) -> Result<MeasurementWeeklySchedule, AppError> {
        Ok(MeasurementWeeklySchedule {
            id: self.schedule_id,
            scope: TenantScope::new(
                self.operator_id.into(),
                self.tenant_id.into(),
                Some(self.project_id.into()),
            ),
            window: MeasurementPeriodWindow {
                start_at: self.report_window_start_at,
                end_at: self.report_window_end_at,
                report_timezone: self.report_timezone,
            },
            evidence_as_of: self.evidence_as_of,
            report_at: self.report_at,
            due_at: self.due_at,
            report_schedule: serde_json::from_value(self.report_schedule).map_err(|_| {
                AppError::new(
                    ErrorCode::Internal,
                    "stored weekly report settings are invalid",
                )
            })?,
            report_id: self.report_id,
        })
    }
}

impl PgReportRepository {
    /// Trusted global selector returns scope IDs only; each materialization is
    /// tenant-scoped and serialized with report saves and project edits.
    /// Per-project failures do not block pagination through later projects.
    pub async fn discover_weekly_projects_after(
        &self,
        now: DateTime<Utc>,
        after: Option<Uuid>,
    ) -> Result<Vec<(Uuid, Result<(), AppError>)>, AppError> {
        let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT operator_id,tenant_id,project_id FROM projects WHERE status <> 'archived' AND ($1::uuid IS NULL OR project_id > $1) ORDER BY project_id LIMIT 100",
        ).bind(after).fetch_all(&self.pool).await.map_err(database_error)?;
        let mut results = Vec::with_capacity(rows.len());
        for (operator, tenant, project) in rows {
            let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
            results.push((project, self.discover_measurement_week(&scope, now).await));
        }
        Ok(results)
    }

    async fn discover_measurement_week(
        &self,
        scope: &TenantScope,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(database_error)?;
        let project_row: Option<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT status,project_settings FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .fetch_optional(&mut *tx).await.map_err(database_error)?;
        let Some((status, settings)) = project_row else {
            return Err(AppError::not_found("project not found"));
        };
        if status == "archived" {
            return Ok(());
        }
        let previous: Option<ScheduleRow> = sqlx::query_as(
            "SELECT * FROM measurement_weekly_schedules WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY sequence DESC LIMIT 1",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .fetch_optional(&mut *tx).await.map_err(database_error)?;
        let previous = previous.map(ScheduleRow::decode).transpose()?;
        if previous.as_ref().is_some_and(|s| s.report_id.is_none()) {
            return Ok(());
        }
        let settings: ProjectSettings = serde_json::from_value(settings).map_err(|_| {
            AppError::new(
                ErrorCode::Internal,
                "stored project report settings are invalid",
            )
        })?;
        let mut schedule = match previous {
            Some(previous) => next_measurement_week(
                &previous,
                &settings.report_timezone,
                &settings.report_schedule,
            )?,
            None => first_measurement_week(
                scope,
                &settings.report_timezone,
                &settings.report_schedule,
                now,
            )?,
        };
        // A timezone changed back to an earlier timezone can revisit an already
        // completed natural week. Advance to its latest completed boundary once.
        let existing: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM measurement_weekly_schedules WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND report_window_start_at=$4 AND report_window_end_at=$5 AND report_timezone=$6)",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(schedule.window.start_at).bind(schedule.window.end_at).bind(&schedule.window.report_timezone)
            .fetch_one(&mut *tx).await.map_err(database_error)?;
        if existing {
            let latest_end: DateTime<Utc> = sqlx::query_scalar(
                "SELECT MAX(report_window_end_at) FROM measurement_weekly_schedules WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND report_timezone=$4",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
                .bind(&schedule.window.report_timezone).fetch_one(&mut *tx).await.map_err(database_error)?;
            schedule.window.end_at = latest_end;
            schedule = next_measurement_week(
                &schedule,
                &settings.report_timezone,
                &settings.report_schedule,
            )?;
        }
        let schedule_json = serde_json::to_value(&schedule.report_schedule).map_err(|_| {
            AppError::new(
                ErrorCode::Internal,
                "cannot serialize weekly report settings",
            )
        })?;
        sqlx::query(
            "INSERT INTO measurement_weekly_schedules (schedule_id,operator_id,tenant_id,project_id,report_window_start_at,report_window_end_at,report_timezone,evidence_as_of,report_at,due_at,report_schedule) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        ).bind(schedule.id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(schedule.window.start_at).bind(schedule.window.end_at).bind(&schedule.window.report_timezone)
            .bind(schedule.evidence_as_of).bind(schedule.report_at).bind(schedule.due_at).bind(schedule_json)
            .execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)
    }

    /// The global due selector carries trusted scopes. Archived projects are
    /// excluded; drafts and paused projects can still summarize saved evidence.
    pub async fn due_measurement_weeks_after(
        &self,
        now: DateTime<Utc>,
        after: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<Vec<MeasurementWeeklySchedule>, AppError> {
        let rows: Vec<ScheduleRow> = sqlx::query_as(
            "SELECT s.* FROM measurement_weekly_schedules s JOIN projects p ON (p.operator_id,p.tenant_id,p.project_id)=(s.operator_id,s.tenant_id,s.project_id) WHERE s.report_id IS NULL AND s.due_at <= $1 AND p.status <> 'archived' AND ($2::timestamptz IS NULL OR (s.due_at,s.schedule_id)>($2,$3)) ORDER BY s.due_at,s.schedule_id LIMIT 100",
        ).bind(now).bind(after.map(|(at, _)| at)).bind(after.map(|(_, id)| id))
            .fetch_all(&self.pool).await.map_err(database_error)?;
        rows.into_iter().map(ScheduleRow::decode).collect()
    }

    /// Link a saved immutable revision-one report only at the frozen cutoff.
    /// A manual report with a different cutoff requires explicit resolution.
    pub async fn complete_measurement_week(
        &self,
        scope: &TenantScope,
        schedule_id: Uuid,
        report_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(database_error)?;
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM projects WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .fetch_optional(&mut *tx).await.map_err(database_error)?;
        status.ok_or_else(|| AppError::not_found("project not found"))?;
        let schedule: Option<ScheduleRow> = sqlx::query_as(
            "SELECT * FROM measurement_weekly_schedules WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND schedule_id=$4",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(schedule_id)
            .fetch_optional(&mut *tx).await.map_err(database_error)?;
        let schedule = schedule
            .ok_or_else(|| AppError::not_found("weekly report schedule not found"))?
            .decode()?;
        if let Some(completed) = schedule.report_id {
            return if completed == report_id {
                Ok(())
            } else {
                Err(AppError::conflict(
                    "weekly report already completed with another report",
                ))
            };
        }
        // Archiving prevents new discovery/selection, not completion of a
        // selected read-only report whose immutable snapshot is already saved.
        if now < schedule.due_at {
            return Err(AppError::conflict(
                "weekly report is not eligible for completion",
            ));
        }
        let report: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT snapshot FROM measurement_period_reports WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND report_id=$4",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(report_id)
            .fetch_optional(&mut *tx).await.map_err(database_error)?;
        let report: MeasurementPeriodReport = serde_json::from_value(
            report.ok_or_else(|| AppError::not_found("measurement report not found"))?,
        )
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored measurement report is invalid"))?;
        if report.report_id != report_id
            || report.project_id != project
            || report.revision != 1
            || report.correction_of.is_some()
            || report.window() != schedule.window
            || report.evidence_as_of != schedule.evidence_as_of
            || report.generated_at > now
        {
            return Err(AppError::conflict(
                "saved report does not match the weekly window and frozen evidence cutoff",
            ));
        }
        sqlx::query(
            "UPDATE measurement_weekly_schedules SET report_id=$5,completed_at=$6 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND schedule_id=$4 AND report_id IS NULL",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(schedule_id)
            .bind(report_id).bind(now).execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)
    }
}
