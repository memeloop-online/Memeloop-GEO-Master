use geo_domain::ProxyPoolRepository;
use geo_persistence::{Database, DatabaseConfig, PgChannelRepository};
use uuid::Uuid;
#[path = "../../domain/tests/support/proxy_pools.rs"]
mod contract;
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_proxy_inventory_is_scoped_versioned_and_immutable() {
    let database = Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL required"),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    let other = Uuid::new_v4();
    for id in [operator, other] {
        sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,$3)")
            .bind(id)
            .bind(format!("proxy-{id}"))
            .bind("Synthetic proxy operator")
            .execute(database.pool())
            .await
            .unwrap();
    }
    let repo = PgChannelRepository::from_database(&database);
    let (pool, member, revision) = contract::verify(&repo, operator.into(), other.into()).await;
    let restored = PgChannelRepository::from_database(&database);
    assert_eq!(
        restored
            .get_proxy_member(operator.into(), pool, member)
            .await
            .unwrap()
            .version,
        3
    );
    assert_eq!(
        restored
            .get_proxy_revision(operator.into(), pool, member, revision)
            .await
            .unwrap()
            .encrypted_bytes(),
        &[1]
    );
    assert!(
        sqlx::query("UPDATE operator_proxy_revisions SET encrypted_proxy=$1 WHERE operator_id=$2")
            .bind(vec![9u8])
            .bind(operator)
            .execute(database.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM operator_proxy_revisions WHERE operator_id=$1")
            .bind(operator)
            .execute(database.pool())
            .await
            .is_err()
    );
}
