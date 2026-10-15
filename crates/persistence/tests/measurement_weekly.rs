use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    ErrorCode, MeasurementWeeklySchedule, ProjectSettings, ReportRepository, TenantScope,
    freeze_measurement_period, preview_measurement_period_at_cutoff,
};
use geo_persistence::{Database, DatabaseConfig, PgReportRepository};
use uuid::Uuid;

fn at(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

async fn discover(repo: &PgReportRepository, project: Uuid, now: DateTime<Utc>) {
    let mut after = None;
    loop {
        let page = repo
            .discover_weekly_projects_after(now, after)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|(id, _)| *id);
        for (id, outcome) in page {
            if id == project {
                outcome.unwrap();
            }
        }
    }
}

async fn due(
    repo: &PgReportRepository,
    scope: &TenantScope,
    now: DateTime<Utc>,
) -> Vec<MeasurementWeeklySchedule> {
    let mut after = None;
    let mut found = Vec::new();
    loop {
        let page = repo.due_measurement_weeks_after(now, after).await.unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|s| (s.due_at, s.id));
        found.extend(page.into_iter().filter(|s| &s.scope == scope));
    }
    found
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn draft_weekly_reports_persist_cutoff_replay_config_and_scope_without_cycles() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database required"),
    )
    .unwrap();
    let db = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("weekly-{operator}"))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("weekly-{tenant}")).execute(db.pool()).await.unwrap();
    let settings = ProjectSettings {
        report_timezone: "UTC".into(),
        ..ProjectSettings::default()
    };
    for id in [project, other] {
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name,project_settings) VALUES ($1,$2,$3,$4,'Synthetic',$5)")
            .bind(id).bind(operator).bind(tenant).bind(format!("weekly-{id}"))
            .bind(serde_json::to_value(&settings).unwrap()).execute(db.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(other.into()));
    let repo = PgReportRepository::from_database(&db);
    let discovered = at("2026-10-15T12:00:00Z");
    let ((), ()) = tokio::join!(
        discover(&repo, project, discovered),
        discover(&repo, project, discovered)
    );
    assert!(
        due(&repo, &scope, at("2026-10-19T08:59:59Z"))
            .await
            .is_empty()
    );
    let schedule = due(&repo, &scope, at("2026-10-19T09:00:00Z"))
        .await
        .remove(0);
    assert_eq!(schedule.window.start_at, at("2026-10-12T00:00:00Z"));
    assert_eq!(schedule.evidence_as_of, at("2026-10-19T00:00:00Z"));
    let generated = at("2026-11-10T12:00:00Z");
    let restarted = PgReportRepository::from_database(&db);
    discover(&restarted, project, generated).await;
    assert_eq!(
        due(&restarted, &scope, generated).await,
        vec![schedule.clone()]
    );
    let changed = ProjectSettings {
        report_timezone: "America/New_York".into(),
        ..settings.clone()
    };
    sqlx::query("UPDATE projects SET project_settings=$2 WHERE project_id=$1")
        .bind(project)
        .bind(serde_json::to_value(&changed).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    discover(&repo, project, generated).await;
    assert_eq!(due(&repo, &scope, generated).await, vec![schedule.clone()]);
    let report = freeze_measurement_period(
        &scope,
        preview_measurement_period_at_cutoff(
            &scope,
            &schedule.window,
            vec![],
            None,
            schedule.evidence_as_of,
            generated,
        )
        .unwrap(),
        1,
        None,
    )
    .unwrap();
    let saved = repo
        .create_measurement_period(&scope, report)
        .await
        .unwrap();
    assert_eq!(saved.coverage.planned, 0);
    assert_eq!(
        repo.complete_measurement_week(&other_scope, schedule.id, saved.report_id, generated)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    // An already selected, saved read-only report may complete after archive.
    sqlx::query("UPDATE projects SET status='archived' WHERE project_id=$1")
        .bind(project)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(due(&repo, &scope, generated).await.is_empty());
    repo.complete_measurement_week(&scope, schedule.id, saved.report_id, generated)
        .await
        .unwrap();
    restarted
        .complete_measurement_week(
            &scope,
            schedule.id,
            saved.report_id,
            generated + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert!(due(&repo, &scope, generated).await.is_empty());
    sqlx::query("UPDATE projects SET status='draft' WHERE project_id=$1")
        .bind(project)
        .execute(db.pool())
        .await
        .unwrap();
    discover(&repo, project, generated).await;
    let next = due(&repo, &scope, generated).await.remove(0);
    assert_eq!(next.window.report_timezone, "America/New_York");
    assert_eq!(next.window.start_at, at("2026-10-12T04:00:00Z"));
    assert_eq!(next.evidence_as_of, at("2026-10-19T04:00:00Z"));
    assert_eq!(
        repo.get_measurement_period(&scope, saved.report_id)
            .await
            .unwrap(),
        saved
    );
    // A manually saved revision-one with a different cutoff must never be linked.
    let mismatch = freeze_measurement_period(
        &scope,
        preview_measurement_period_at_cutoff(
            &scope,
            &next.window,
            vec![],
            None,
            next.evidence_as_of + Duration::seconds(1),
            generated,
        )
        .unwrap(),
        1,
        None,
    )
    .unwrap();
    let mismatch = repo
        .create_measurement_period(&scope, mismatch)
        .await
        .unwrap();
    assert_eq!(
        repo.complete_measurement_week(&scope, next.id, mismatch.report_id, generated)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(due(&repo, &scope, generated).await, vec![next]);
    let cycles: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM optimization_cycles WHERE project_id=$1")
            .bind(project)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(cycles, 0);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM measurement_weekly_schedules WHERE project_id=$1")
            .bind(project)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 2);
}
