use geo_domain::BrowserOperationRepository;
use geo_persistence::{Database, DatabaseConfig, PgChannelJobRepository};
use uuid::Uuid;
#[path = "../../domain/tests/support/browser_operations.rs"]
mod contract;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_browser_ownership_contract_and_restart() {
    let db = Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL required"),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let operator = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,$3)")
        .bind(operator)
        .bind(format!("ownership-{operator}"))
        .bind("Synthetic ownership operator")
        .execute(db.pool())
        .await
        .unwrap();
    let repo = PgChannelJobRepository::from_database(&db);
    // A retained pre-migration pool login must block new ownership, even though
    // it has no durable operation and its timestamp is arbitrarily old.
    use geo_domain::{
        BeginBrowserOperation, BrowserOperationPurpose, BrowserOperationScope, ChannelRepository,
        ChannelSecret, PoolLoginSession,
    };
    let legacy_account = Uuid::new_v4();
    sqlx::query("INSERT INTO operator_channel_accounts(operator_id,account_id,platform,metadata) VALUES($1,$2,'synthetic','{}')")
        .bind(operator).bind(legacy_account).execute(db.pool()).await.unwrap();
    let channels = geo_persistence::PgChannelRepository::from_database(&db);
    let legacy_login = PoolLoginSession {
        session_id: Uuid::new_v4(),
        account_id: legacy_account,
        created_at: chrono::Utc::now() - chrono::Duration::days(30),
    };
    channels
        .save_pool_login(operator.into(), legacy_login.clone())
        .await
        .unwrap();
    let mut replay = legacy_login.clone();
    replay.created_at = chrono::Utc::now();
    channels
        .save_pool_login(operator.into(), replay)
        .await
        .unwrap();
    assert_eq!(
        channels
            .get_pool_login(operator.into(), legacy_login.session_id)
            .await
            .unwrap()
            .created_at
            .timestamp_micros(),
        legacy_login.created_at.timestamp_micros()
    );
    let legacy_scope = BrowserOperationScope {
        operator_id: operator.into(),
        tenant_id: None,
        project_id: None,
        owner_id: Uuid::new_v4(),
    };
    assert!(
        repo.begin_browser_operation(
            &legacy_scope,
            &BeginBrowserOperation {
                operation_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                account_id: legacy_account,
                purpose: BrowserOperationPurpose::Login,
                runner_generation: Uuid::new_v4(),
                effective_network: ChannelSecret::new(vec![1]),
                reservation_id: None,
                expires_at: chrono::Utc::now() + chrono::Duration::minutes(5)
            },
            chrono::Utc::now()
        )
        .await
        .is_err()
    );
    assert!(
        !repo
            .browser_operation_exists(operator.into(), legacy_login.session_id)
            .await
            .unwrap()
    );
    let (scope, id) = contract::verify(&repo, operator.into()).await;
    contract::verify_login_commit(&repo, &channels, operator.into()).await;
    drop(repo);
    let restarted = PgChannelJobRepository::from_database(&db);
    let restored = restarted
        .get_browser_operation(&scope, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.effective_network.encrypted_bytes(), &[1, 2, 3]);
    assert_eq!(restored.state, geo_domain::BrowserOperationState::Prepared);
    assert!(
        restarted
            .list_due_browser_operations(chrono::Utc::now() + chrono::Duration::hours(24), 1000)
            .await
            .unwrap()
            .iter()
            .any(|op| op.operation_id == id)
    );
}
