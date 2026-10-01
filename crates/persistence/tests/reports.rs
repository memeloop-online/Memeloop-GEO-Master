use chrono::{Duration, Utc};
use geo_domain::{
    ErrorCode, InitialSource, InitialSourceKind, InitialSourceVisibility, ProjectCreate,
    ProjectRepository, ProjectSettings, ProjectStartCommand, ReportManifestKind, ReportManifestRef,
    ReportReduceInput, ReportRepository, TenantScope, hash_idempotency_key, reduce_report,
    settings_hash, start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgProjectRepository, PgReportRepository};
use std::collections::HashSet;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn immutable_scoped_report_replay_and_correction_in_postgres() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL required"),
    )
    .expect("valid database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,$3)")
        .bind(operator_id)
        .bind(format!("report-{operator_id}"))
        .bind("Test operator")
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,$4)",
    )
    .bind(tenant_id)
    .bind(operator_id)
    .bind(format!("report-{tenant_id}"))
    .bind("Test tenant")
    .execute(database.pool())
    .await
    .unwrap();
    let tenant_scope = TenantScope::new(operator_id.into(), tenant_id.into(), None);
    let projects = PgProjectRepository::from_database(&database);
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: None,
                display_name: "Report fixture".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Fixture".to_owned(),
                    market: "US".to_owned(),
                    language: "en".to_owned(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.com".to_owned(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let start = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("report-fixture"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(
        tenant_scope.operator_id,
        tenant_scope.tenant_id,
        Some(project.id),
    );
    let started_cycle = projects
        .get_report_cycle(&scope, project.id, start.cycle_id)
        .await
        .unwrap()
        .expect("started cycle is readable");
    let start_view = projects
        .get_start(&scope, project.id)
        .await
        .unwrap()
        .expect("start record is readable");
    assert_eq!(started_cycle.report_timezone, start_view.report_timezone);
    assert_eq!(
        started_cycle.report_window_start_at,
        start_view.report_window_start_at
    );
    assert_eq!(
        started_cycle.report_window_end_at,
        start_view.report_window_end_at
    );
    assert_eq!(started_cycle.cutoff_at, start_view.cutoff_at);
    assert_eq!(
        started_cycle.document_manifest,
        Some(start.document_manifest.clone())
    );
    assert_eq!(
        started_cycle.distribution_manifest,
        Some(start.distribution_manifest.clone())
    );
    let other = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert!(
        projects
            .get_report_cycle(&other, project.id, start.cycle_id)
            .await
            .unwrap()
            .is_none()
    );
    // PostgreSQL timestamptz stores microseconds; use its precision for the
    // directly seeded cycle timestamps asserted after an actual SQL read.
    let now = chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let input = ReportReduceInput {
        project_id: project.id,
        cycle_id: start.cycle_id,
        report_window_start_at: now - Duration::days(8),
        report_window_end_at: now - Duration::days(1),
        report_timezone: "UTC".to_owned(),
        cutoff_at: now - Duration::hours(1),
        input_temporal_provenance_verified: false,
        input_manifest_versions: vec![
            ReportManifestRef {
                kind: ReportManifestKind::Document,
                manifest_id: start.document_manifest.manifest_id,
                revision: start.document_manifest.revision,
                sealed: false,
                expected_count: None,
            },
            ReportManifestRef {
                kind: ReportManifestKind::Distribution,
                manifest_id: start.distribution_manifest.manifest_id,
                revision: start.distribution_manifest.revision,
                sealed: false,
                expected_count: None,
            },
        ],
        document_manifest: None,
        publication_targets: None,
        measurement_targets: None,
    };
    let reports = PgReportRepository::from_database(&database);
    let scan_at = now + Duration::days(30);
    let mut cursor = None;
    let mut found_due = false;
    loop {
        let page = reports.due_scopes_after(scan_at, cursor).await.unwrap();
        if page.is_empty() {
            break;
        }
        found_due |= page.iter().any(|(_, _, cycle)| *cycle == start.cycle_id);
        cursor = page.last().map(|(cutoff, _, cycle)| (*cutoff, *cycle));
        if page.len() < 100 {
            break;
        }
    }
    assert!(found_due);
    let proposed = reduce_report(&scope, &input, 1, None, now).unwrap();
    let (first, parallel_replay) = tokio::join!(
        reports.create(&scope, proposed.clone()),
        reports.create(&scope, proposed)
    );
    let first = first.unwrap();
    assert_eq!(first, parallel_replay.unwrap());
    let replay = reports
        .create(
            &scope,
            reduce_report(&scope, &input, 1, None, now + Duration::seconds(1)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first, replay);
    let after = reports.due_scopes_after(scan_at, None).await.unwrap();
    assert!(!after.iter().any(|(_, _, cycle)| *cycle == start.cycle_id));
    let proposed_correction = reduce_report(
        &scope,
        &input,
        2,
        Some(first.report_id),
        now + Duration::hours(1),
    )
    .unwrap();
    let (correction, correction_replay) = tokio::join!(
        reports.create(&scope, proposed_correction.clone()),
        reports.create(&scope, proposed_correction)
    );
    let correction = correction.unwrap();
    assert_eq!(correction, correction_replay.unwrap());
    assert_eq!(correction.correction_of, Some(first.report_id));
    let mut conflicting_input = input.clone();
    conflicting_input.report_timezone = "Etc/UTC".to_owned();
    let conflict = reports
        .create(
            &scope,
            reduce_report(
                &scope,
                &conflicting_input,
                2,
                Some(first.report_id),
                now + Duration::hours(1),
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::Conflict);
    assert_eq!(reports.list(&scope, project.id).await.unwrap().len(), 2);
    assert_eq!(
        reports.get(&other, first.report_id).await.unwrap_err().code,
        ErrorCode::NotFound
    );

    // Later cycles have the same frozen configuration but no project-start record.
    // Leave every early candidate without a report to model failed source reads:
    // the scanner must still advance its keyset cursor to the later cycle.
    let early_cutoff = now - Duration::days(90);
    let mut early_cycles = HashSet::new();
    for offset in 0..101 {
        let cycle_id = Uuid::new_v4();
        early_cycles.insert(cycle_id);
        let cutoff = early_cutoff + Duration::seconds(offset);
        sqlx::query(
            "INSERT INTO optimization_cycles
             (cycle_id,operator_id,tenant_id,project_id,config_revision_id,state,
              report_timezone,report_window_start_at,report_window_end_at,cutoff_at)
             VALUES ($1,$2,$3,$4,$5,'awaiting_knowledge','UTC',$6,$7,$8)",
        )
        .bind(cycle_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.id.as_uuid())
        .bind(start.config_revision_id)
        .bind(cutoff - Duration::days(8))
        .bind(cutoff - Duration::days(1))
        .bind(cutoff)
        .execute(database.pool())
        .await
        .unwrap();
    }
    let later_cycle = Uuid::new_v4();
    let later_cutoff = now - Duration::days(89);
    sqlx::query(
        "INSERT INTO optimization_cycles
         (cycle_id,operator_id,tenant_id,project_id,config_revision_id,state,
          report_timezone,report_window_start_at,report_window_end_at,cutoff_at)
         VALUES ($1,$2,$3,$4,$5,'running','UTC',$6,$7,$8)",
    )
    .bind(later_cycle)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.id.as_uuid())
    .bind(start.config_revision_id)
    .bind(later_cutoff - Duration::days(8))
    .bind(later_cutoff - Duration::days(1))
    .bind(later_cutoff)
    .execute(database.pool())
    .await
    .unwrap();
    let subsequent = projects
        .get_report_cycle(&scope, project.id, later_cycle)
        .await
        .unwrap()
        .expect("cycle without a start record is readable");
    assert_eq!(subsequent.report_timezone, "UTC");
    assert_eq!(subsequent.cutoff_at, later_cutoff);
    assert_eq!(
        subsequent.report_window_start_at,
        later_cutoff - Duration::days(8)
    );
    assert_eq!(
        subsequent.report_window_end_at,
        later_cutoff - Duration::days(1)
    );
    assert!(subsequent.document_manifest.is_none());
    assert!(subsequent.distribution_manifest.is_none());
    assert!(
        projects
            .get_report_cycle(&other, project.id, later_cycle)
            .await
            .unwrap()
            .is_none()
    );
    let start_record_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM project_start_records WHERE cycle_id=$1")
            .bind(later_cycle)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(start_record_count, 0);

    let first_page = reports.due_scopes_after(scan_at, None).await.unwrap();
    assert_eq!(first_page.len(), 100);
    assert!(!first_page.iter().any(|(_, _, cycle)| *cycle == later_cycle));
    let mut seen = HashSet::new();
    let mut cursor = None;
    let mut found_later = false;
    loop {
        let page = reports.due_scopes_after(scan_at, cursor).await.unwrap();
        if page.is_empty() {
            break;
        }
        for (_, page_scope, cycle) in &page {
            assert!(seen.insert(*cycle), "keyset page duplicated cycle {cycle}");
            if *cycle == later_cycle {
                assert_eq!(page_scope.operator_id, scope.operator_id);
                assert_eq!(page_scope.tenant_id, scope.tenant_id);
                assert_eq!(page_scope.project_id, scope.project_id);
                found_later = true;
            }
        }
        cursor = page.last().map(|(cutoff, _, cycle)| (*cutoff, *cycle));
        if page.len() < 100 {
            break;
        }
    }
    assert!(early_cycles.is_subset(&seen));
    assert!(
        found_later,
        "later due cycle must survive failed earlier candidates"
    );
}
