use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    EffectiveObservation, ErrorCode, MeasurementPeriodSample, MeasurementPeriodWindow,
    ObservationAnalysisSource, ReportRepository, SavedAnalysisProvenance, TenantScope,
    freeze_measurement_period, preview_measurement_period,
};
use geo_persistence::{Database, DatabaseConfig, PgReportRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn cycle_free_reports_freeze_concurrent_create_correction_and_restart() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other_project) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("period-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("period-{tenant}")).execute(database.pool()).await.unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("period-{id}")).execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let other = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let window = MeasurementPeriodWindow {
        start_at: now - Duration::days(7),
        end_at: now,
        report_timezone: "UTC".into(),
    };
    let sample = MeasurementPeriodSample {
        plan_id: Uuid::new_v4(),
        target_id: Uuid::new_v4(),
        attempt_id: Some(Uuid::new_v4()),
        comparison_key: "provider|model|consumer_web|web_search".into(),
        question_binding: None,
        scheduled_at: now - Duration::hours(1),
        original_status: "unknown".into(),
        observed_live: false,
        observation: None,
    };
    let repository = PgReportRepository::from_database(&database);
    let proposed = freeze_measurement_period(
        &scope,
        preview_measurement_period(&scope, &window, vec![sample.clone()], now).unwrap(),
        1,
        None,
    )
    .unwrap();
    let later = freeze_measurement_period(
        &scope,
        preview_measurement_period(
            &scope,
            &window,
            vec![sample.clone()],
            now + Duration::seconds(1),
        )
        .unwrap(),
        1,
        None,
    )
    .unwrap();
    let (first, replay) = tokio::join!(
        repository.create_measurement_period(&scope, proposed),
        repository.create_measurement_period(&scope, later),
    );
    let first = first.unwrap();
    assert_eq!(first, replay.unwrap());
    let analyzed = now + Duration::minutes(1);
    let mut interpreted = sample.clone();
    interpreted.observation = Some(EffectiveObservation {
        raw_answer: "Saved answer".into(),
        citations: vec!["https://example.test/source".into()],
        observed_at: sample.scheduled_at,
        received_at: sample.scheduled_at + Duration::seconds(1),
        provenance: Some(SavedAnalysisProvenance {
            revision_id: Uuid::new_v4(),
            source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
            source_sha256: "a".repeat(64),
            observed_at: sample.scheduled_at,
            analyzed_at: analyzed,
            actual_model: "parser-model".into(),
            config_revision: Some(1),
            prompt_version: "v1".into(),
            parser_version: "v1".into(),
        }),
    });
    let proposed = freeze_measurement_period(
        &scope,
        preview_measurement_period(&scope, &window, vec![interpreted], analyzed).unwrap(),
        2,
        Some(first.report_id),
    )
    .unwrap();
    let (correction, replay) = tokio::join!(
        repository.create_measurement_period(&scope, proposed.clone()),
        repository.create_measurement_period(&scope, proposed),
    );
    let correction = correction.unwrap();
    assert_eq!(correction, replay.unwrap());
    assert_eq!(correction.coverage.planned, 1);
    assert_eq!(correction.coverage.counts["unknown"], 1);
    assert_eq!(correction.coverage.grounded_saved_analysis, 1);
    let restarted = PgReportRepository::from_database(&database);
    assert_eq!(
        restarted
            .get_measurement_period(&scope, first.report_id)
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        restarted
            .get_measurement_period(&scope, correction.report_id)
            .await
            .unwrap(),
        correction
    );
    assert_eq!(
        restarted
            .list_measurement_periods(&scope)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        restarted
            .get_measurement_period(&other, first.report_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        restarted
            .create_measurement_period(&other, first.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    let cycles: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM optimization_cycles WHERE project_id=$1")
            .bind(project)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(cycles, 0);
    assert!(
        restarted
            .list(&scope, project.into())
            .await
            .unwrap()
            .is_empty()
    );
}
