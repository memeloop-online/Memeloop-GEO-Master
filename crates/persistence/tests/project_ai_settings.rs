use geo_domain::{
    ErrorCode, ProjectAiMode, ProjectAiSettingsRecord, ProjectAiSettingsRepository, ProjectAiUsage,
    TenantScope,
};
use geo_persistence::{Database, DatabaseConfig, PgProjectAiSettingsRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn settings_restart_cas_and_project_usage_isolation() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required"),
    )
    .unwrap();
    let db = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("settings-{operator}"))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("settings-{tenant}")).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
        .bind(project).bind(operator).bind(tenant).bind(format!("settings-{project}")).execute(db.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let repo = PgProjectAiSettingsRepository::from_database(&db);
    let usage = ProjectAiUsage::WorkbenchContent;
    assert_eq!(repo.get(&scope, usage).await.unwrap().revision, 0);
    let mut row = ProjectAiSettingsRecord::inherited(usage);
    row.mode = ProjectAiMode::Custom;
    row.model = Some("synthetic-model".into());
    row.base_url = Some("https://example.invalid/v1".into());
    row.encrypted_api_key = Some(vec![1, 2, 3, 4]);
    let saved = repo.save(&scope, 0, row.clone()).await.unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(
        repo.save(&scope, 0, row.clone()).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let (first, second) = tokio::join!(
        repo.save(&scope, 1, row.clone()),
        repo.save(&scope, 1, row.clone())
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let reboot = Database::connect_and_migrate(&config).await.unwrap();
    let reloaded = PgProjectAiSettingsRepository::from_database(&reboot)
        .get(&scope, usage)
        .await
        .unwrap();
    assert_eq!(reloaded.revision, 2);
    assert_eq!(reloaded.encrypted_api_key, Some(vec![1, 2, 3, 4]));
    assert_eq!(
        repo.get(&scope, ProjectAiUsage::ObservationAnalysis)
            .await
            .unwrap()
            .revision,
        0
    );
    let other = TenantScope::new(operator.into(), Uuid::new_v4().into(), Some(project.into()));
    assert_eq!(repo.get(&other, usage).await.unwrap().revision, 0);
    let cleared = repo
        .save(&scope, 2, ProjectAiSettingsRecord::inherited(usage))
        .await
        .unwrap();
    assert_eq!(cleared.revision, 3);
    assert!(
        repo.get(&scope, usage)
            .await
            .unwrap()
            .encrypted_api_key
            .is_none()
    );
    sqlx::query("DELETE FROM project_ai_settings WHERE operator_id=$1")
        .bind(operator)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM projects WHERE operator_id=$1")
        .bind(operator)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM tenants WHERE operator_id=$1")
        .bind(operator)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM operators WHERE operator_id=$1")
        .bind(operator)
        .execute(db.pool())
        .await
        .unwrap();
}
