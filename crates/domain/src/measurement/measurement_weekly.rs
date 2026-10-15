//! Frozen standalone weekly reports. Discovery never backfills pre-install weeks.

use chrono::{DateTime, Datelike, Duration, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::project::{cutoff_after, resolve_local_datetime};
use crate::{AppError, MeasurementPeriodWindow, ReportSchedule, TenantScope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasurementWeeklySchedule {
    pub id: Uuid,
    pub scope: TenantScope,
    pub window: MeasurementPeriodWindow,
    pub evidence_as_of: DateTime<Utc>,
    pub report_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub report_schedule: ReportSchedule,
    pub report_id: Option<Uuid>,
}

/// Freeze the next scheduled prior natural week, rather than arbitrary historical weeks.
pub fn first_measurement_week(
    scope: &TenantScope,
    timezone: &str,
    schedule: &ReportSchedule,
    now: DateTime<Utc>,
) -> Result<MeasurementWeeklySchedule, AppError> {
    let (start, end, _) = crate::previous_calendar_week_window(timezone, schedule, now)?;
    freeze_week(scope, timezone, schedule, start, end)
}

/// Pending schedules retain their settings. After completion, the current settings
/// govern the next natural week. A timezone change starts at the local Monday of
/// the week containing the predecessor end; an overlap or gap is intentional.
pub fn next_measurement_week(
    predecessor: &MeasurementWeeklySchedule,
    timezone: &str,
    schedule: &ReportSchedule,
) -> Result<MeasurementWeeklySchedule, AppError> {
    let timezone_value = parse_timezone(timezone)?;
    let date = predecessor
        .window
        .end_at
        .with_timezone(&timezone_value)
        .date_naive();
    let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
    let start = resolve_local_datetime(timezone_value, monday.and_hms_opt(0, 0, 0).unwrap());
    let end = resolve_local_datetime(
        timezone_value,
        (monday + Duration::days(7)).and_hms_opt(0, 0, 0).unwrap(),
    );
    freeze_week(&predecessor.scope, timezone, schedule, start, end)
}

fn parse_timezone(timezone: &str) -> Result<Tz, AppError> {
    timezone
        .parse()
        .map_err(|_| AppError::invalid_request("report_timezone must be a valid IANA timezone"))
}

fn freeze_week(
    scope: &TenantScope,
    timezone: &str,
    schedule: &ReportSchedule,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<MeasurementWeeklySchedule, AppError> {
    if scope.project_id.is_none() {
        return Err(AppError::forbidden("project scope required"));
    }
    let zone = parse_timezone(timezone)?;
    let report_time =
        NaiveTime::parse_from_str(&schedule.report_local_time, "%H:%M").map_err(|_| {
            AppError::invalid_request("report_schedule.report_local_time must be HH:MM")
        })?;
    let cutoff_time =
        NaiveTime::parse_from_str(&schedule.cutoff_local_time, "%H:%M").map_err(|_| {
            AppError::invalid_request("report_schedule.cutoff_local_time must be HH:MM")
        })?;
    let report_at = cutoff_after(zone, schedule.report_weekday, report_time, end)?;
    let evidence_as_of = cutoff_after(zone, schedule.cutoff_weekday, cutoff_time, end)?;
    Ok(MeasurementWeeklySchedule {
        id: Uuid::new_v4(),
        scope: scope.clone(),
        window: MeasurementPeriodWindow {
            start_at: start,
            end_at: end,
            report_timezone: timezone.to_owned(),
        },
        evidence_as_of,
        report_at,
        due_at: report_at.max(evidence_as_of),
        report_schedule: schedule.clone(),
        report_id: None,
    })
}
