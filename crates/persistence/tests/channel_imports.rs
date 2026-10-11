//! Requires a disposable database, never a live/shared deployment.
use geo_domain::ChannelRepository;
use geo_persistence::{Database, DatabaseConfig, PgChannelRepository};
use uuid::Uuid;

#[path = "../../domain/tests/support/channel_imports.rs"]
mod contract;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_import_rows_are_atomic_scoped_and_replayable() {
    let database = Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL required"),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    let other_operator = Uuid::new_v4();
    for id in [operator, other_operator] {
        sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,$3)")
            .bind(id)
            .bind(format!("import-{id}"))
            .bind("Synthetic import operator")
            .execute(database.pool())
            .await
            .unwrap();
    }
    let repository = PgChannelRepository::from_database(&database);
    let key = contract::verify(&repository, operator.into(), other_operator.into()).await;
    let restored = PgChannelRepository::from_database(&database);
    let rows = restored
        .list_import_rows(operator.into(), &key)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let (cipher, version, outcome): (Vec<u8>, i32, serde_json::Value) = sqlx::query_as(
        "SELECT encrypted_input,credential_version,outcome FROM operator_channel_import_rows WHERE operator_id=$1 AND import_key=$2",
    ).bind(operator).bind(&key).fetch_one(database.pool()).await.unwrap();
    assert_eq!(version, 1);
    assert_eq!(cipher, vec![42; 64]);
    assert!(outcome.get("password").is_none());
    assert!(outcome.get("username").is_none());
}
