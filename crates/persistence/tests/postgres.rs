use geo_persistence::{Database, DatabaseConfig};

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
