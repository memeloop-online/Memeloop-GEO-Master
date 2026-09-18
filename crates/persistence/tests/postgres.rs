use geo_domain::{
    InitialSource, InitialSourceKind, InitialSourceVisibility, ProjectCreate, ProjectRepository,
    ProjectSettings, ProjectStartCommand, TenantScope, hash_idempotency_key, settings_hash,
    start_request_hash,
};
use geo_persistence::{Database, DatabaseConfig, PgProjectRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn embedded_migrations_apply_to_postgres_when_configured() {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");

    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("connect and apply embedded migrations");

    // Running the embedded migration set again must be safe.
    database.migrate().await.expect("re-run migration set");

    let required_tables = [
        "operators",
        "tenants",
        "projects",
        "operations",
        "idempotency_records",
        "outbox_events",
        "project_config_revisions",
        "project_start_records",
        "optimization_cycles",
        "document_manifests",
        "distribution_manifests",
        "workflow_runs",
    ];

    for table in required_tables {
        let relation: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(format!("public.{table}"))
            .fetch_one(database.pool())
            .await
            .expect("query migration result");
        assert_eq!(relation.as_deref(), Some(table));
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn atomic_start_and_scope_visibility_hold_when_postgres_is_configured() {
    let database_url =
        std::env::var("GEO_TEST_DATABASE_URL").expect("GEO_TEST_DATABASE_URL is required");
    let config = DatabaseConfig::from_url(database_url).expect("valid test database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let operator_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let other_tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id, slug, display_name) VALUES ($1,$2,$3)")
        .bind(operator_id)
        .bind(format!("atomic-{operator_id}"))
        .bind("Atomic test operator")
        .execute(database.pool())
        .await
        .expect("operator");
    for tenant_id in [tenant_id, other_tenant_id] {
        sqlx::query(
            "INSERT INTO tenants (tenant_id, operator_id, slug, display_name) VALUES ($1,$2,$3,$4)",
        )
        .bind(tenant_id)
        .bind(operator_id)
        .bind(format!("tenant-{tenant_id}"))
        .bind("Atomic test tenant")
        .execute(database.pool())
        .await
        .expect("tenant");
    }
    let scope = TenantScope::new(operator_id.into(), tenant_id.into(), None);
    let repository = PgProjectRepository::from_database(&database);
    let project = repository
        .create(
            &scope,
            ProjectCreate {
                slug: Some(format!("atomic-{tenant_id}")),
                display_name: "Atomic start".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Acme".to_owned(),
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
        .expect("draft");
    let frozen = project
        .settings
        .clone()
        .validate_start()
        .expect("startable");
    let frozen_hash = settings_hash(&frozen).expect("settings hash");
    let command = ProjectStartCommand {
        expected_revision: project.revision,
        idempotency_key_hash: hash_idempotency_key("atomic-start"),
        request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
        settings_hash: frozen_hash,
        operation_id: Uuid::new_v4(),
    };
    let acceptance = repository
        .start(&scope, project.id, command.clone())
        .await
        .expect("atomic start");
    assert!(!acceptance.document_manifest.sealed);
    assert_eq!(acceptance.document_manifest.expected_count, None);
    assert_eq!(acceptance.document_manifest.state, "awaiting_knowledge");
    assert_eq!(acceptance.distribution_manifest.state, "awaiting_documents");
    assert_eq!(
        repository
            .start(&scope, project.id, command)
            .await
            .expect("same-key replay"),
        acceptance
    );
    let other_scope = TenantScope::new(operator_id.into(), other_tenant_id.into(), None);
    assert!(
        repository
            .get_start(&other_scope, project.id)
            .await
            .expect("scoped lookup")
            .is_none()
    );
}
