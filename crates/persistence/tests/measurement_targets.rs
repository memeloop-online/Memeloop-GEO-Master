use geo_domain::{
    ErrorCode, MeasurementTarget, MeasurementTargetKind, ProjectCreate, ProjectPatch,
    ProjectRepository, ProjectSettings, TenantScope,
};
use geo_persistence::{Database, DatabaseConfig, PgProjectRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn explicit_targets_persist_across_repository_restart_and_clear_with_revision_check() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("targets-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("targets-{tenant}")).execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), None);
    let repository = PgProjectRepository::from_database(&database);
    let project = repository
        .create(
            &scope,
            ProjectCreate {
                slug: None,
                display_name: "Measurement workspace".to_owned(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    assert!(project.settings.measurement_targets.is_empty());
    let project_scope = project.scope();
    let targets = vec![
        MeasurementTarget {
            name: "Model A".to_owned(),
            kind: MeasurementTargetKind::Product,
            aliases: vec!["A".to_owned()],
        },
        MeasurementTarget {
            name: "Model A Pro".to_owned(),
            kind: MeasurementTargetKind::Product,
            aliases: vec![],
        },
    ];
    let updated = repository
        .update(
            &project_scope,
            project.id,
            project.revision,
            ProjectPatch {
                measurement_targets: Some(targets.clone()),
                ..ProjectPatch::default()
            },
        )
        .await
        .unwrap();
    let restarted = Database::connect_and_migrate(&config).await.unwrap();
    let restarted_repository = PgProjectRepository::from_database(&restarted);
    let reloaded = restarted_repository
        .get(&project_scope, project.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.settings.measurement_targets, targets);
    assert!(reloaded.settings.brand_name.is_empty());
    assert!(reloaded.settings.product_name.is_none());
    assert_eq!(
        restarted_repository
            .update(
                &project_scope,
                project.id,
                project.revision,
                ProjectPatch {
                    measurement_targets: Some(vec![]),
                    ..ProjectPatch::default()
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let cleared = restarted_repository
        .update(
            &project_scope,
            project.id,
            updated.project.revision,
            ProjectPatch {
                measurement_targets: Some(vec![]),
                ..ProjectPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(cleared.project.settings.measurement_targets.is_empty());
    assert!(
        restarted_repository
            .get(&project_scope, project.id)
            .await
            .unwrap()
            .unwrap()
            .settings
            .measurement_targets
            .is_empty()
    );
}
