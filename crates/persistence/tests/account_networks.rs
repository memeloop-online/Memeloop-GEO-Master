use geo_domain::AccountNetworkBindingRepository;
use geo_persistence::{Database, DatabaseConfig, PgChannelJobRepository, PgChannelRepository};
use uuid::Uuid;
#[path = "../../domain/tests/support/account_networks.rs"]
mod contract;
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_account_network_contract_and_immutability() {
    let db = Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL required"),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic operator')",
    )
    .bind(operator)
    .bind(format!("network-{operator}"))
    .execute(db.pool())
    .await
    .unwrap();
    let jobs = PgChannelJobRepository::from_database(&db);
    let channels = PgChannelRepository::from_database(&db);
    let scope = contract::verify(&jobs, &channels, operator.into()).await;
    contract::verify_busy_legacy_bootstrap(&jobs, &channels, operator.into()).await;
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic tenant')")
        .bind(tenant).bind(operator).bind(format!("network-{tenant}")).execute(db.pool()).await.unwrap();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic network project')")
        .bind(project).bind(operator).bind(tenant).bind(format!("network-{project}")).execute(db.pool()).await.unwrap();
    let project_scope =
        geo_domain::TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    contract::verify_project_defaults(&jobs, &channels, &project_scope).await;
    let restored = PgChannelJobRepository::from_database(&db);
    assert!(
        restored
            .get_account_network(&scope)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        sqlx::query(
            "UPDATE account_network_bindings SET effective_network=$1 WHERE operator_id=$2"
        )
        .bind(vec![0u8])
        .bind(operator)
        .execute(db.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM account_network_bindings WHERE operator_id=$1")
            .bind(operator)
            .execute(db.pool())
            .await
            .is_err()
    );
}
