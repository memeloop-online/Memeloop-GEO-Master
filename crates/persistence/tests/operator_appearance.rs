//! Disposable PostgreSQL regression: host resolution, revision conflicts,
//! operator isolation, and durable readback across repository reconnection.

use geo_domain::{AuthRepository, ErrorCode, UpdateOperatorAppearance};
use geo_persistence::{Database, DatabaseConfig, PgAuthRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn operator_appearance_is_host_scoped_and_survives_reconnection() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database URL");
    let config = DatabaseConfig::from_url(url).expect("database config");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("database connection");
    let pool = database.pool();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let first_host = format!("appearance-{first}.test");
    let second_host = format!("appearance-{second}.test");
    for (operator, host, name) in [
        (first, &first_host, "First Operator"),
        (second, &second_host, "Second Operator"),
    ] {
        sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,$3)")
            .bind(operator)
            .bind(format!("appearance-{operator}"))
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO operator_hosts(host,operator_id) VALUES($1,$2)")
            .bind(host)
            .bind(operator)
            .execute(pool)
            .await
            .unwrap();
    }
    let repo = PgAuthRepository::new(pool.clone());
    assert!(
        repo.operator_for_host("unknown-appearance.test")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.operator_for_host(&first_host)
            .await
            .unwrap()
            .unwrap()
            .id
            .as_uuid(),
        first
    );
    assert_eq!(
        repo.operator_for_host(&second_host)
            .await
            .unwrap()
            .unwrap()
            .id
            .as_uuid(),
        second
    );
    let before = repo
        .operator_appearance(first.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.display_name, "First Operator");
    assert_eq!(before.revision, 1);
    assert_eq!(before.logo_url, None);
    let update = UpdateOperatorAppearance {
        display_name: "Renamed Operator".to_owned(),
        primary_color: "#123abc".to_owned(),
        default_locale: "en".to_owned(),
    };
    let after = repo
        .update_operator_appearance(first.into(), 1, update.clone())
        .await
        .unwrap();
    assert_eq!(after.revision, 2);
    assert_eq!(after.primary_color, "#123ABC");
    assert_eq!(after.default_locale, "en");
    assert_eq!(
        repo.update_operator_appearance(first.into(), 1, update)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    drop(repo);
    drop(database);

    let restarted = Database::connect_and_migrate(&config)
        .await
        .expect("reconnect");
    let repo = PgAuthRepository::from_database(&restarted);
    assert_eq!(
        repo.operator_appearance(first.into())
            .await
            .unwrap()
            .unwrap(),
        after
    );
    assert_eq!(
        repo.operator_for_host(&first_host)
            .await
            .unwrap()
            .unwrap()
            .display_name,
        "Renamed Operator"
    );
    let other = repo
        .operator_appearance(second.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.display_name, "Second Operator");
    assert_eq!(other.revision, 1);
    assert_eq!(other.default_locale, "zh-CN");
    assert_eq!(
        repo.operator_for_host(&second_host)
            .await
            .unwrap()
            .unwrap()
            .display_name,
        "Second Operator"
    );
}
