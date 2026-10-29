use geo_persistence::{Database, DatabaseConfig, PgChannelJobRepository, PgChannelRepository};
use uuid::Uuid;
#[path = "../../domain/tests/support/customer_reconnect.rs"]
mod contract;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_customer_reconnect_is_atomic_and_retains_destination() {
    let db = Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL required"),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic reconnect operator')")
        .bind(operator).bind(format!("reconnect-{operator}")).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic reconnect tenant')")
        .bind(tenant).bind(operator).bind(format!("reconnect-{tenant}")).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic reconnect project')")
        .bind(project).bind(operator).bind(tenant).bind(format!("reconnect-{project}")).execute(db.pool()).await.unwrap();
    let scope = geo_domain::TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let operation = contract::verify(
        &PgChannelJobRepository::from_database(&db),
        &PgChannelRepository::from_database(&db),
        &scope,
    )
    .await;
    let persisted: (Uuid, Uuid) = sqlx::query_as(
        "SELECT account_id,login_verified_account_id FROM browser_operations WHERE operation_id=$1",
    )
    .bind(operation)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_ne!(persisted.0, persisted.1);
}
