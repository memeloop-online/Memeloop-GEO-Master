use geo_domain::*;
use geo_persistence::{Database, DatabaseConfig, PgChannelJobRepository, PgChannelRepository};
use uuid::Uuid;
#[path = "../../domain/tests/support/operator_login_queue.rs"]
mod contract;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_operator_login_queue_contract_and_restart() {
    let db = Database::connect_and_migrate(
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
            .bind(format!("login-queue-{id}"))
            .bind("Synthetic login queue operator")
            .execute(db.pool())
            .await
            .unwrap();
    }
    let jobs = PgChannelJobRepository::from_database(&db);
    let channels = PgChannelRepository::from_database(&db);
    let (scope, id) = contract::verify(&jobs, &channels, operator.into(), other.into()).await;
    drop(jobs);
    let restored = PgChannelJobRepository::from_database(&db)
        .get_login_queue(&scope, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.status, OperatorLoginQueueStatus::Completed);
    assert_eq!(restored.items.len(), 3);
    let verified:i64=sqlx::query_scalar("SELECT count(*) FROM browser_operations WHERE operator_id=$1 AND login_verified_at IS NOT NULL").bind(operator).fetch_one(db.pool()).await.unwrap();
    assert_eq!(verified, 1);
}

async fn single_queue_fixture(
    db: &Database,
) -> (
    OperatorLoginQueueScope,
    OperatorLoginQueue,
    BrowserOperationScope,
    BeginBrowserOperation,
) {
    let now = chrono::Utc::now();
    let operator = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,$3)")
        .bind(operator)
        .bind(format!("queue-recovery-{operator}"))
        .bind("Synthetic queue recovery operator")
        .execute(db.pool())
        .await
        .unwrap();
    let account_id = Uuid::new_v4();
    let key = Uuid::new_v4().to_string();
    let channels = PgChannelRepository::from_database(db);
    channels
        .save_import_row(
            operator.into(),
            &key,
            None,
            ChannelImportRecord {
                row: ChannelImportRow {
                    row_key: "synthetic-row".into(),
                    ordinal: 0,
                    account_id: Some(account_id),
                    status: ChannelImportStatus::NeedsLogin,
                    error_code: None,
                    masked_username: None,
                    display_alias: None,
                },
                secret_id: account_id,
                encrypted_input: ChannelSecret::new(vec![8; 64]),
            },
            Some(PoolAccount {
                account_id,
                platform: "synthetic".into(),
                group_id: None,
                status: ChannelStatus::NeedsLogin,
                display_name: None,
                platform_account_id: None,
                avatar_url: None,
                enabled: true,
                proxy_configured: false,
                proxy_server: None,
                created_at: now,
                updated_at: now,
            }),
        )
        .await
        .unwrap();
    let jobs = PgChannelJobRepository::from_database(db);
    let network_scope = AccountNetworkScope {
        operator_id: operator.into(),
        account_id,
        tenant_id: None,
        project_id: None,
    };
    let binding = AccountNetworkBinding {
        binding_id: Uuid::new_v4(),
        scope: network_scope.clone(),
        source: AccountNetworkSource::Direct,
        effective_network: ChannelSecret::new(vec![5]),
        created_at: now,
    };
    jobs.set_account_network(&network_scope, None, &binding, &channels, now)
        .await
        .unwrap();
    let scope = OperatorLoginQueueScope {
        operator_id: operator.into(),
        creator_user_id: Uuid::new_v4(),
    };
    let queue = jobs
        .create_login_queue(
            &scope,
            Uuid::new_v4(),
            &key,
            &[OperatorLoginQueueSelection {
                row_key: "synthetic-row".into(),
                account_id,
            }],
            &channels,
            now,
        )
        .await
        .unwrap();
    let browser_scope = BrowserOperationScope {
        operator_id: operator.into(),
        tenant_id: None,
        project_id: None,
        owner_id: Uuid::new_v4(),
    };
    let input = BeginBrowserOperation {
        network_selection: Some(BrowserNetworkSelection {
            scope: network_scope,
            historical_binding_id: None,
        }),
        operation_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        account_id,
        purpose: BrowserOperationPurpose::Login,
        runner_generation: Uuid::new_v4(),
        effective_network: ChannelSecret::new(vec![99]),
        reservation_id: None,
        expires_at: now + chrono::Duration::minutes(20),
    };
    (scope, queue, browser_scope, input)
}

fn verified_storage(operator: OperatorId, account: Uuid) -> VerifiedLoginStorage {
    VerifiedLoginStorage {
        account_id: account,
        secret_scope: TenantScope::new(operator, Uuid::new_v4().into(), None),
        owner_kind: ChannelOwnerKind::OperatorPool,
        platform: "synthetic".into(),
        expected_platform_account_id: None,
        expected_session: None,
        platform_account_id: format!("synthetic-{account}"),
        display_name: "Synthetic verified account".into(),
        avatar_url: None,
        session: ChannelSecret::new(vec![7]),
    }
}

async fn disposable_database() -> Database {
    Database::connect_and_migrate(
        &DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL required"),
        )
        .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_login_queue_restores_active_operation_at_each_recovery_boundary() {
    let db = disposable_database().await;
    let (scope, queue, owner, input) = single_queue_fixture(&db).await;
    let channels = PgChannelRepository::from_database(&db);
    let jobs = PgChannelJobRepository::from_database(&db);
    let (claimed, prepared) = jobs
        .begin_login_queue_item(
            &scope,
            queue.queue_id,
            queue.version,
            owner.owner_id,
            &input,
            &channels,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    drop(jobs);
    let restored = PgChannelJobRepository::from_database(&db);
    let read = restored.active_login_queue(&scope).await.unwrap().unwrap();
    assert_eq!(
        read.current().unwrap().operation_id,
        Some(prepared.operation_id)
    );
    assert_eq!(
        read.current().unwrap().owner_session_id,
        Some(owner.owner_id)
    );
    let read_operation = restored
        .get_browser_operation(&owner, prepared.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_operation.state, BrowserOperationState::Prepared);
    assert_eq!(read_operation.session_id, input.session_id);
    assert_eq!(read_operation.runner_generation, input.runner_generation);
    assert_eq!(
        read_operation.network_binding_id,
        prepared.network_binding_id
    );
    assert_eq!(read_operation.effective_network.encrypted_bytes(), &[5]);
    assert_eq!(
        read_operation.expires_at.timestamp_micros(),
        input.expires_at.timestamp_micros()
    );
    assert_eq!(
        channels
            .get_pool_login(scope.operator_id, input.session_id)
            .await
            .unwrap()
            .account_id,
        input.account_id
    );
    restored
        .claim_browser_start(&owner, input.operation_id, chrono::Utc::now())
        .await
        .unwrap()
        .unwrap();
    drop(restored);
    let restored = PgChannelJobRepository::from_database(&db);
    let read = restored
        .get_login_queue(&scope, queue.queue_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.version, claimed.version);
    assert_eq!(
        read.current().unwrap().operation_id,
        Some(input.operation_id)
    );
    assert_eq!(
        restored
            .get_browser_operation(&owner, input.operation_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        BrowserOperationState::StartRequested
    );
    assert!(
        restored
            .claim_browser_start(&owner, input.operation_id, chrono::Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    restored
        .save_owned_login_session(
            &owner,
            input.operation_id,
            &verified_storage(scope.operator_id, input.account_id),
            &channels,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    restored
        .request_browser_close(&owner, input.operation_id, chrono::Utc::now())
        .await
        .unwrap();
    drop(restored);
    let restored = PgChannelJobRepository::from_database(&db);
    let read = restored.active_login_queue(&scope).await.unwrap().unwrap();
    assert_eq!(
        read.current().unwrap().operation_id,
        Some(input.operation_id)
    );
    let read_operation = restored
        .get_browser_operation(&owner, input.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_operation.state, BrowserOperationState::Closing);
    assert!(read_operation.login_verified_at.is_some());
    assert_eq!(
        channels
            .get_pool_account(scope.operator_id, input.account_id)
            .await
            .unwrap()
            .session
            .unwrap()
            .encrypted_bytes(),
        &[7]
    );
    let reconciled = restored
        .apply_login_queue_action(
            &scope,
            queue.queue_id,
            read.version,
            OperatorLoginQueueAction::ReconcileCurrent,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(reconciled.version, read.version);
    assert_eq!(
        reconciled.current().unwrap().operation_id,
        Some(input.operation_id)
    );
    restored
        .confirm_browser_disposed(
            &owner,
            input.operation_id,
            input.session_id,
            input.runner_generation,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let completed = restored
        .apply_login_queue_action(
            &scope,
            queue.queue_id,
            read.version,
            OperatorLoginQueueAction::ReconcileCurrent,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(completed.status, OperatorLoginQueueStatus::Completed);
    assert_eq!(
        completed.items[0].status,
        OperatorLoginQueueItemStatus::Succeeded
    );
}

async fn wait_for_gate_waiter(pool: &sqlx::PgPool, blocker: i32, prefix: &str) -> i32 {
    // Observe the real lock dependency rather than relying on a timing delay.
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let pid:Option<i32> = sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) AND strpos(query,$2)=1 LIMIT 1")
                .bind(blocker).bind(prefix).fetch_optional(pool).await.unwrap();
            if let Some(pid) = pid { return pid; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("expected gate waiter did not appear")
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn postgres_queue_claim_cannot_deadlock_current_login_verification() {
    let db = disposable_database().await;
    let (scope, queue, owner, input) = single_queue_fixture(&db).await;
    let jobs = PgChannelJobRepository::from_database(&db);
    let channels = PgChannelRepository::from_database(&db);
    // Another session already owns this account. Its verification must commit,
    // while a queue attempting the account returns a normal busy conflict.
    let operation = jobs
        .begin_browser_operation(&owner, &input, chrono::Utc::now())
        .await
        .unwrap();
    jobs.claim_browser_start(&owner, operation.operation_id, chrono::Utc::now())
        .await
        .unwrap()
        .unwrap();
    let mut gate_blocker = db.pool().begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate_blocker)
        .await
        .unwrap();
    sqlx::query("SELECT browser_operation_id FROM channel_account_preflight_reservations WHERE operator_id=$1 AND account_id=$2 FOR UPDATE")
        .bind(scope.operator_id.as_uuid()).bind(input.account_id).fetch_one(&mut *gate_blocker).await.unwrap();
    let verification = tokio::spawn({
        let jobs = jobs.clone();
        let channels = channels.clone();
        let owner = owner.clone();
        let account = input.account_id;
        async move {
            jobs.save_owned_login_session(
                &owner,
                operation.operation_id,
                &verified_storage(owner.operator_id, account),
                &channels,
                chrono::Utc::now(),
            )
            .await
        }
    });
    let verifier_pid = wait_for_gate_waiter(
        db.pool(),
        blocker_pid,
        "SELECT browser_operation_id FROM channel_account_preflight_reservations",
    )
    .await;
    let contender_input = BeginBrowserOperation {
        operation_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        ..input.clone()
    };
    let contender_session = contender_input.session_id;
    let contender = tokio::spawn({
        let jobs = jobs.clone();
        let channels = channels.clone();
        let scope = scope.clone();
        let queue = queue.clone();
        async move {
            jobs.begin_login_queue_item(
                &scope,
                queue.queue_id,
                queue.version,
                Uuid::new_v4(),
                &contender_input,
                &channels,
                chrono::Utc::now(),
            )
            .await
        }
    });
    wait_for_gate_waiter(
        db.pool(),
        verifier_pid,
        "INSERT INTO channel_account_preflight_reservations",
    )
    .await;
    gate_blocker.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), verification)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), contender)
        .await
        .unwrap()
        .unwrap();
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("busy account claim unexpectedly succeeded"),
    };
    assert_eq!(error.code, ErrorCode::Conflict);
    let retained = jobs
        .get_login_queue(&scope, queue.queue_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.version, queue.version);
    assert!(retained.current().is_none());
    assert!(
        !jobs
            .browser_operation_exists(scope.operator_id, contender_session)
            .await
            .unwrap()
    );
    assert!(
        jobs.get_browser_operation(&owner, input.operation_id)
            .await
            .unwrap()
            .unwrap()
            .login_verified_at
            .is_some()
    );
    assert_eq!(
        channels
            .get_pool_account(scope.operator_id, input.account_id)
            .await
            .unwrap()
            .session
            .unwrap()
            .encrypted_bytes(),
        &[7]
    );
}
