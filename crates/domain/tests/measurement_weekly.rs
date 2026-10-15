use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    ReportSchedule, ReportWeekday, TenantScope, first_measurement_week, next_measurement_week,
};
use uuid::Uuid;

fn at(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}
fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

#[test]
fn first_discovery_freezes_next_schedule_without_historical_backfill() {
    let week = first_measurement_week(
        &scope(),
        "UTC",
        &ReportSchedule::default(),
        at("2026-10-15T12:00:00Z"),
    )
    .unwrap();
    assert_eq!(week.window.start_at, at("2026-10-12T00:00:00Z"));
    assert_eq!(week.window.end_at, at("2026-10-19T00:00:00Z"));
    assert_eq!(week.evidence_as_of, week.window.end_at);
    assert_eq!(week.report_at, at("2026-10-19T09:00:00Z"));
    assert_eq!(week.due_at, week.report_at);
    assert!(week.report_id.is_none());
}

#[test]
fn cutoff_after_report_time_controls_actual_due_time() {
    let schedule = ReportSchedule {
        cutoff_weekday: ReportWeekday::Tuesday,
        cutoff_local_time: "12:00".into(),
        ..ReportSchedule::default()
    };
    let week =
        first_measurement_week(&scope(), "UTC", &schedule, at("2026-10-15T12:00:00Z")).unwrap();
    assert_eq!(week.report_at, at("2026-10-19T09:00:00Z"));
    assert_eq!(week.due_at, at("2026-10-20T12:00:00Z"));
    assert_eq!(week.due_at, week.evidence_as_of);
}

#[test]
fn recovery_successor_is_adjacent_and_does_not_use_recovery_clock() {
    let week = first_measurement_week(
        &scope(),
        "UTC",
        &ReportSchedule::default(),
        at("2026-10-15T12:00:00Z"),
    )
    .unwrap();
    let next = next_measurement_week(&week, "UTC", &week.report_schedule).unwrap();
    assert_eq!(next.window.start_at, week.window.end_at);
    assert_eq!(next.window.end_at, week.window.end_at + Duration::days(7));
    assert_eq!(next.evidence_as_of, week.evidence_as_of + Duration::days(7));
}

#[test]
fn timezone_changes_use_whole_new_natural_weeks_with_overlap_or_gap() {
    let week = first_measurement_week(
        &scope(),
        "UTC",
        &ReportSchedule::default(),
        at("2026-10-15T12:00:00Z"),
    )
    .unwrap();
    let west = next_measurement_week(&week, "America/New_York", &week.report_schedule).unwrap();
    assert_eq!(west.window.start_at, at("2026-10-12T04:00:00Z"));
    assert_eq!(west.window.end_at, at("2026-10-19T04:00:00Z"));
    assert!(west.window.start_at < week.window.end_at);
    let east = next_measurement_week(&week, "Asia/Shanghai", &week.report_schedule).unwrap();
    assert_eq!(east.window.start_at, at("2026-10-18T16:00:00Z"));
    assert_eq!(east.window.end_at, at("2026-10-25T16:00:00Z"));
    let western = first_measurement_week(
        &scope(),
        "America/New_York",
        &ReportSchedule::default(),
        at("2026-10-15T12:00:00Z"),
    )
    .unwrap();
    let utc = next_measurement_week(&western, "UTC", &western.report_schedule).unwrap();
    assert_eq!(utc.window.start_at, at("2026-10-19T00:00:00Z"));
    // The new week remains Monday-to-Monday even across DST.
    let fall = first_measurement_week(
        &scope(),
        "America/New_York",
        &ReportSchedule::default(),
        at("2026-10-29T12:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        fall.window.end_at - fall.window.start_at,
        Duration::hours(169)
    );
}

#[test]
fn report_and_cutoff_schedule_changes_apply_only_to_successor() {
    let original = first_measurement_week(
        &scope(),
        "UTC",
        &ReportSchedule::default(),
        at("2026-10-15T12:00:00Z"),
    )
    .unwrap();
    let changed = ReportSchedule {
        report_weekday: ReportWeekday::Wednesday,
        cutoff_weekday: ReportWeekday::Tuesday,
        ..ReportSchedule::default()
    };
    let next = next_measurement_week(&original, "UTC", &changed).unwrap();
    assert_eq!(next.report_at, at("2026-10-28T09:00:00Z"));
    assert_eq!(next.evidence_as_of, at("2026-10-27T00:00:00Z"));
    assert_eq!(original.report_schedule, ReportSchedule::default());
}

#[test]
fn invalid_scope_timezone_and_clock_are_rejected() {
    let now = at("2026-10-15T12:00:00Z");
    let scope = scope();
    assert!(first_measurement_week(&scope, "unknown", &ReportSchedule::default(), now).is_err());
    let invalid = ReportSchedule {
        report_local_time: "25:00".into(),
        ..ReportSchedule::default()
    };
    assert!(first_measurement_week(&scope, "UTC", &invalid, now).is_err());
    let tenant = TenantScope {
        project_id: None,
        ..scope
    };
    assert!(first_measurement_week(&tenant, "UTC", &ReportSchedule::default(), now).is_err());
}
