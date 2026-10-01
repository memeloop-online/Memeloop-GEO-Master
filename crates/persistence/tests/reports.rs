use chrono::{Duration, Utc};
use geo_domain::{
    ErrorCode, InitialSource, InitialSourceKind, InitialSourceVisibility, ProjectCreate,
    ProjectRepository, ProjectSettings, ProjectStartCommand, ReportManifestKind, ReportManifestRef,
    ReportReduceInput, ReportRepository, TenantScope, hash_idempotency_key, reduce_report,
    settings_hash, start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgProjectRepository, PgReportRepository};
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
    let now = Utc::now();
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
    let other = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert_eq!(
        reports.get(&other, first.report_id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}
