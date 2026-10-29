use chrono::{Duration, Utc};
use geo_domain::*;
use uuid::Uuid;

pub async fn verify(
    jobs: &impl ChannelJobRepository,
    channels: &impl ChannelRepository,
    operator: OperatorId,
    other_operator: OperatorId,
) -> (OperatorLoginQueueScope, Uuid) {
    let now = Utc::now();
    let scope = OperatorLoginQueueScope {
        operator_id: operator,
        creator_user_id: Uuid::new_v4(),
    };
    let key = Uuid::new_v4().to_string();
    let mut selections = Vec::new();
    for index in 0..3 {
        let account_id = Uuid::new_v4();
        let row_key = format!("synthetic-row-{index}");
        channels
            .save_import_row(
                operator,
                &key,
                None,
                ChannelImportRecord {
                    row: ChannelImportRow {
                        row_key: row_key.clone(),
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
        let binding = AccountNetworkBinding {
            binding_id: Uuid::new_v4(),
            scope: AccountNetworkScope {
                operator_id: operator,
                account_id,
                tenant_id: None,
                project_id: None,
            },
            source: AccountNetworkSource::Direct,
            effective_network: ChannelSecret::new(vec![index + 1]),
            created_at: now,
        };
        jobs.set_account_network(&binding.scope, None, &binding, channels, now)
            .await
            .unwrap();
        selections.push(OperatorLoginQueueSelection {
            row_key,
            account_id,
        });
    }
    selections.swap(0, 2);
    let id = Uuid::new_v4();
    let mut bad = selections.clone();
    bad[0].account_id = selections[1].account_id;
    assert!(
        jobs.create_login_queue(&scope, id, &key, &bad, channels, now)
            .await
            .is_err()
    );
    bad = selections.clone();
    bad[0].row_key = "missing-row".into();
    assert!(
        jobs.create_login_queue(&scope, id, &key, &bad, channels, now)
            .await
            .is_err()
    );
    let mut queue = jobs
        .create_login_queue(&scope, id, &key, &selections, channels, now)
        .await
        .unwrap();
    assert_eq!(
        queue.items.iter().map(|i| i.account_id).collect::<Vec<_>>(),
        selections.iter().map(|i| i.account_id).collect::<Vec<_>>()
    );
    assert_eq!(
        jobs.create_login_queue(&scope, id, &key, &selections, channels, now)
            .await
            .unwrap()
            .version,
        1
    );
    assert!(
        jobs.create_login_queue(&scope, Uuid::new_v4(), &key, &selections, channels, now)
            .await
            .is_err()
    );
    let mut changed = selections.clone();
    changed.swap(0, 1);
    assert!(
        jobs.create_login_queue(&scope, id, &key, &changed, channels, now)
            .await
            .is_err()
    );
    for foreign in [
        OperatorLoginQueueScope {
            creator_user_id: Uuid::new_v4(),
            ..scope.clone()
        },
        OperatorLoginQueueScope {
            operator_id: other_operator,
            ..scope.clone()
        },
    ] {
        assert!(jobs.get_login_queue(&foreign, id).await.unwrap().is_none());
        assert!(jobs.active_login_queue(&foreign).await.unwrap().is_none());
        assert!(
            jobs.apply_login_queue_action(&foreign, id, 1, OperatorLoginQueueAction::Pause, now)
                .await
                .is_err()
        );
        assert!(
            jobs.create_login_queue(&foreign, id, &key, &selections, channels, now)
                .await
                .is_err()
        );
    }
    let owner = Uuid::new_v4();
    let input = |account_id| BeginBrowserOperation {
        network_selection: Some(BrowserNetworkSelection {
            scope: AccountNetworkScope {
                operator_id: operator,
                account_id,
                tenant_id: None,
                project_id: None,
            },
            historical_binding_id: None,
        }),
        operation_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        account_id,
        purpose: BrowserOperationPurpose::Login,
        runner_generation: Uuid::new_v4(),
        effective_network: ChannelSecret::new(vec![99]),
        reservation_id: None,
        expires_at: now + Duration::minutes(20),
    };
    // Failure of the final reference write rolls back queue, gate and operation.
    let other_session = Uuid::new_v4();
    channels
        .save_pool_login(
            operator,
            PoolLoginSession {
                session_id: other_session,
                account_id: selections[1].account_id,
                created_at: now,
            },
        )
        .await
        .unwrap();
    let mut collision = input(selections[0].account_id);
    collision.session_id = other_session;
    assert!(
        jobs.begin_login_queue_item(&scope, id, 1, owner, &collision, channels, now)
            .await
            .is_err()
    );
    assert_eq!(
        jobs.get_login_queue(&scope, id)
            .await
            .unwrap()
            .unwrap()
            .version,
        1
    );
    assert!(
        !jobs
            .browser_operation_exists(operator, collision.session_id)
            .await
            .unwrap()
    );
    channels
        .delete_pool_login(operator, other_session)
        .await
        .unwrap();
    let a = input(selections[0].account_id);
    let b = input(selections[0].account_id);
    let (left, right) = tokio::join!(
        jobs.begin_login_queue_item(&scope, id, 1, owner, &a, channels, now),
        jobs.begin_login_queue_item(&scope, id, 1, owner, &b, channels, now)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let (claimed, first) = left.or(right).unwrap();
    queue = claimed;
    assert_eq!(
        queue.current().unwrap().account_id,
        selections[0].account_id
    );
    assert_eq!(first.state, BrowserOperationState::Prepared);
    assert!(first.login_verified_at.is_none());
    assert_eq!(first.effective_network.encrypted_bytes(), &[3]);
    assert_eq!(
        channels
            .get_pool_login(operator, first.session_id)
            .await
            .unwrap()
            .account_id,
        first.account_id
    );
    assert_eq!(
        jobs.apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::ReconcileCurrent,
            now
        )
        .await
        .unwrap()
        .version,
        queue.version
    );
    let mut wrong_owner = first.scope.clone();
    wrong_owner.owner_id = Uuid::new_v4();
    assert!(
        jobs.get_browser_operation(&wrong_owner, first.operation_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        jobs.claim_browser_start(&wrong_owner, first.operation_id, now)
            .await
            .is_err()
    );
    let (a, b) = tokio::join!(
        jobs.claim_browser_start(&first.scope, first.operation_id, now),
        jobs.claim_browser_start(&first.scope, first.operation_id, now)
    );
    assert_ne!(a.unwrap().is_some(), b.unwrap().is_some());
    assert!(
        jobs.claim_browser_start(&first.scope, first.operation_id, now)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        jobs.apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::RetryCurrent,
            now
        )
        .await
        .is_err()
    );
    jobs.request_browser_close(&first.scope, first.operation_id, now)
        .await
        .unwrap();
    assert!(
        jobs.apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::SkipCurrent,
            now
        )
        .await
        .is_err()
    );
    jobs.confirm_browser_disposed(
        &first.scope,
        first.operation_id,
        first.session_id,
        first.runner_generation,
        now,
    )
    .await
    .unwrap();
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::ReconcileCurrent,
            now,
        )
        .await
        .unwrap();
    assert_eq!(queue.status, OperatorLoginQueueStatus::Paused);
    assert_eq!(
        queue.current().unwrap().error_code.as_deref(),
        Some("login_not_completed")
    );
    assert!(
        jobs.apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::Resume,
            now
        )
        .await
        .is_err()
    );
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::RetryCurrent,
            now,
        )
        .await
        .unwrap();
    assert_eq!(queue.items[0].attempt, 1);
    assert!(queue.current().is_none());
    assert_eq!(queue.status, OperatorLoginQueueStatus::Paused);
    assert!(
        jobs.begin_login_queue_item(
            &scope,
            id,
            queue.version,
            Uuid::new_v4(),
            &input(selections[0].account_id),
            channels,
            now
        )
        .await
        .is_err()
    );
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::Resume,
            now,
        )
        .await
        .unwrap();
    let new_owner = Uuid::new_v4();
    let (next, retry) = jobs
        .begin_login_queue_item(
            &scope,
            id,
            queue.version,
            new_owner,
            &input(selections[0].account_id),
            channels,
            now,
        )
        .await
        .unwrap();
    queue = next;
    assert_ne!(retry.operation_id, first.operation_id);
    assert_eq!(retry.scope.owner_id, new_owner);
    assert_eq!(
        jobs.get_browser_operation(&first.scope, first.operation_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        BrowserOperationState::Disposed
    );
    jobs.release_browser_unstarted(&retry.scope, retry.operation_id, now)
        .await
        .unwrap();
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::SkipCurrent,
            now,
        )
        .await
        .unwrap();
    assert_eq!(queue.items[0].status, OperatorLoginQueueItemStatus::Skipped);
    // A stale failure (including a stale no-operation failure) cannot pause a successor.
    assert!(
        jobs.pause_login_queue_error(
            &scope,
            id,
            queue.version - 1,
            Some(first.operation_id),
            "login_failed",
            now
        )
        .await
        .is_err()
    );
    let (next, second) = jobs
        .begin_login_queue_item(
            &scope,
            id,
            queue.version,
            new_owner,
            &input(selections[1].account_id),
            channels,
            now,
        )
        .await
        .unwrap();
    queue = next;
    assert!(
        jobs.pause_login_queue_error(&scope, id, queue.version, None, "login_failed", now)
            .await
            .is_err()
    );
    assert!(
        jobs.pause_login_queue_error(
            &scope,
            id,
            queue.version,
            Some(second.operation_id),
            "upstream secret message",
            now
        )
        .await
        .is_err()
    );
    jobs.claim_browser_start(&second.scope, second.operation_id, now)
        .await
        .unwrap()
        .unwrap();
    let storage = VerifiedLoginStorage {
        reconnect: None,
        account_id: second.account_id,
        secret_scope: TenantScope::new(operator, Uuid::new_v4().into(), None),
        owner_kind: ChannelOwnerKind::OperatorPool,
        platform: "synthetic".into(),
        expected_platform_account_id: None,
        expected_session: None,
        platform_account_id: format!("synthetic-{}", second.account_id),
        display_name: "Synthetic verified account".into(),
        avatar_url: None,
        session: ChannelSecret::new(vec![7]),
    };
    jobs.save_owned_login_session(&second.scope, second.operation_id, &storage, channels, now)
        .await
        .unwrap();
    assert!(
        jobs.get_browser_operation(&second.scope, second.operation_id)
            .await
            .unwrap()
            .unwrap()
            .login_verified_at
            .is_some()
    );
    // A crash after verified storage does not imply disposal or allow advancement.
    let unchanged = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::ReconcileCurrent,
            now,
        )
        .await
        .unwrap();
    assert_eq!(unchanged.version, queue.version);
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::Pause,
            now,
        )
        .await
        .unwrap();
    jobs.request_browser_close(&second.scope, second.operation_id, now)
        .await
        .unwrap();
    assert!(
        jobs.apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::RetryCurrent,
            now
        )
        .await
        .is_err()
    );
    jobs.confirm_browser_disposed(
        &second.scope,
        second.operation_id,
        second.session_id,
        second.runner_generation,
        now,
    )
    .await
    .unwrap();
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::ReconcileCurrent,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        queue.items[1].status,
        OperatorLoginQueueItemStatus::Succeeded
    );
    assert_eq!(queue.status, OperatorLoginQueueStatus::Paused);
    assert!(queue.current().is_none());
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::Resume,
            now,
        )
        .await
        .unwrap();
    let mut disabled = channels
        .get_pool_account(operator, selections[2].account_id)
        .await
        .unwrap();
    disabled.account.enabled = false;
    channels
        .save_pool_account_metadata(operator, disabled.clone())
        .await
        .unwrap();
    assert!(
        jobs.begin_login_queue_item(
            &scope,
            id,
            queue.version,
            new_owner,
            &input(selections[2].account_id),
            channels,
            now
        )
        .await
        .is_err()
    );
    assert_eq!(
        jobs.get_login_queue(&scope, id)
            .await
            .unwrap()
            .unwrap()
            .version,
        queue.version
    );
    disabled.account.enabled = true;
    channels
        .save_pool_account_metadata(operator, disabled)
        .await
        .unwrap();
    let (next, last) = jobs
        .begin_login_queue_item(
            &scope,
            id,
            queue.version,
            new_owner,
            &input(selections[2].account_id),
            channels,
            now,
        )
        .await
        .unwrap();
    queue = next;
    queue = jobs
        .pause_login_queue_error(
            &scope,
            id,
            queue.version,
            Some(last.operation_id),
            "network_unavailable",
            now,
        )
        .await
        .unwrap();
    assert_eq!(queue.status, OperatorLoginQueueStatus::Paused);
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::Resume,
            now,
        )
        .await
        .unwrap();
    jobs.release_browser_unstarted(&last.scope, last.operation_id, now)
        .await
        .unwrap();
    queue = jobs
        .apply_login_queue_action(
            &scope,
            id,
            queue.version,
            OperatorLoginQueueAction::SkipCurrent,
            now,
        )
        .await
        .unwrap();
    assert_eq!(queue.status, OperatorLoginQueueStatus::Completed);
    assert!(queue.current().is_none());
    assert!(jobs.active_login_queue(&scope).await.unwrap().is_none());
    assert_eq!(
        jobs.get_login_queue(&scope, id)
            .await
            .unwrap()
            .unwrap()
            .version,
        queue.version
    );
    assert_eq!(
        jobs.create_login_queue(&scope, id, &key, &selections, channels, now)
            .await
            .unwrap()
            .status,
        OperatorLoginQueueStatus::Completed
    );
    (scope, id)
}
