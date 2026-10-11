use chrono::{Duration, Utc};
use geo_domain::*;
use uuid::Uuid;

pub async fn verify_login_commit(
    repo: &impl ChannelJobRepository,
    channels: &impl ChannelRepository,
    operator: OperatorId,
) {
    let now = Utc::now();
    let account_id = Uuid::new_v4();
    let scope = BrowserOperationScope {
        operator_id: operator,
        tenant_id: None,
        project_id: None,
        owner_id: Uuid::new_v4(),
    };
    let account = PoolAccount {
        account_id,
        platform: "synthetic".into(),
        group_id: None,
        status: ChannelStatus::NeedsLogin,
        display_name: None,
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: true,
        proxy_server: Some("http://example.invalid".into()),
        created_at: now,
        updated_at: now,
    };
    channels
        .save_pool_account(
            operator,
            PoolAccountRecord {
                account,
                session: None,
                proxy: Some(ChannelSecret::new(vec![9])),
            },
        )
        .await
        .unwrap();
    let operation = repo
        .begin_browser_operation(
            &scope,
            &BeginBrowserOperation {
                operation_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                account_id,
                purpose: BrowserOperationPurpose::Login,
                runner_generation: Uuid::new_v4(),
                effective_network: ChannelSecret::new(vec![1]),
                reservation_id: None,
                expires_at: now + Duration::minutes(5),
            },
            now,
        )
        .await
        .unwrap();
    let storage = VerifiedLoginStorage {
        account_id,
        secret_scope: TenantScope::new(operator, Uuid::new_v4().into(), None),
        owner_kind: ChannelOwnerKind::OperatorPool,
        platform: "synthetic".into(),
        expected_platform_account_id: None,
        expected_session: None,
        platform_account_id: format!("synthetic-{account_id}"),
        display_name: "Verified".into(),
        avatar_url: None,
        session: ChannelSecret::new(vec![3]),
    };
    assert!(
        repo.save_owned_login_session(&scope, operation.operation_id, &storage, channels, now)
            .await
            .is_err()
    );
    repo.mark_browser_start_requested(&scope, operation.operation_id, now)
        .await
        .unwrap();
    repo.save_owned_login_session(&scope, operation.operation_id, &storage, channels, now)
        .await
        .unwrap();
    let readback = channels
        .get_pool_account(operator, account_id)
        .await
        .unwrap();
    assert_eq!(readback.session.unwrap().encrypted_bytes(), &[3]);
    assert_eq!(readback.proxy.unwrap().encrypted_bytes(), &[9]);
    assert_eq!(readback.account.status, ChannelStatus::Ready);
    // Snapshot CAS rejects a stale verifier even while its operation is active.
    assert!(
        repo.save_owned_login_session(&scope, operation.operation_id, &storage, channels, now)
            .await
            .is_err()
    );
    let mut current = storage;
    current.expected_session = Some(ChannelSecret::new(vec![3]));
    current.expected_platform_account_id = Some(current.platform_account_id.clone());
    repo.request_browser_close(&scope, operation.operation_id, now)
        .await
        .unwrap();
    assert!(
        repo.save_owned_login_session(&scope, operation.operation_id, &current, channels, now)
            .await
            .is_err()
    );
    assert_eq!(
        channels
            .get_pool_account(operator, account_id)
            .await
            .unwrap()
            .session
            .unwrap()
            .encrypted_bytes(),
        &[3]
    );
}

pub async fn verify(
    repo: &impl ChannelJobRepository,
    operator_id: OperatorId,
) -> (BrowserOperationScope, Uuid) {
    let now = Utc::now();
    let scope = BrowserOperationScope {
        operator_id,
        tenant_id: None,
        project_id: None,
        owner_id: Uuid::new_v4(),
    };
    let legacy = TenantScope::new(
        operator_id,
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let make = |account_id| BeginBrowserOperation {
        operation_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        account_id,
        purpose: BrowserOperationPurpose::Login,
        runner_generation: Uuid::new_v4(),
        effective_network: ChannelSecret::new(vec![1, 2, 3]),
        reservation_id: None,
        expires_at: now + Duration::minutes(1),
    };
    let first = make(Uuid::new_v4());
    let rival = make(first.account_id);
    let (a, b) = tokio::join!(
        repo.begin_browser_operation(&scope, &first, now),
        repo.begin_browser_operation(&scope, &rival, now)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let chosen = if a.is_ok() { first } else { rival };
    let id = chosen.operation_id;
    assert_eq!(
        repo.get_active_browser_operation(&scope, chosen.account_id)
            .await
            .unwrap()
            .unwrap()
            .operation_id,
        id
    );
    assert_eq!(
        repo.begin_browser_operation(&scope, &chosen, now)
            .await
            .unwrap()
            .operation_id,
        id
    );
    let later = now + Duration::hours(1);
    assert!(
        repo.reserve_account(
            &legacy,
            chosen.account_id,
            Uuid::new_v4(),
            later,
            later + Duration::minutes(1)
        )
        .await
        .is_err()
    );
    repo.release_account(&legacy, chosen.account_id, id)
        .await
        .unwrap();
    assert!(
        repo.begin_browser_operation(&scope, &make(chosen.account_id), now)
            .await
            .is_err()
    );
    repo.mark_browser_start_requested(&scope, id, now)
        .await
        .unwrap();
    assert!(
        repo.release_browser_unstarted(&scope, id, now)
            .await
            .is_err()
    );
    let mut wrong = scope.clone();
    wrong.owner_id = Uuid::new_v4();
    assert!(
        repo.get_active_browser_operation(&wrong, chosen.account_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(repo.request_browser_close(&wrong, id, now).await.is_err());
    assert!(
        repo.get_browser_operation(&wrong, id)
            .await
            .unwrap()
            .is_none()
    );
    wrong = scope.clone();
    wrong.project_id = Some(Uuid::new_v4().into());
    assert!(
        repo.confirm_browser_disposed(&wrong, id, chosen.session_id, chosen.runner_generation, now)
            .await
            .is_err()
    );
    assert!(
        repo.confirm_browser_disposed(&scope, id, chosen.session_id, Uuid::new_v4(), now)
            .await
            .is_err()
    );
    assert!(
        repo.confirm_browser_disposed(&scope, id, Uuid::new_v4(), chosen.runner_generation, now)
            .await
            .is_err()
    );
    repo.request_browser_close(&scope, id, now).await.unwrap();
    assert!(
        repo.mark_browser_start_requested(&scope, id, now)
            .await
            .is_err()
    );
    assert!(
        repo.list_due_browser_operations(now, 1000)
            .await
            .unwrap()
            .iter()
            .any(|op| op.operation_id == id)
    );
    repo.defer_browser_reconciliation(&scope, id, later + Duration::hours(1), later)
        .await
        .unwrap();
    assert!(
        !repo
            .list_due_browser_operations(later, 1000)
            .await
            .unwrap()
            .iter()
            .any(|op| op.operation_id == id)
    );
    assert!(repo.list_due_browser_operations(later, 0).await.is_err());
    repo.confirm_browser_disposed(
        &scope,
        id,
        chosen.session_id,
        chosen.runner_generation,
        later,
    )
    .await
    .unwrap();
    repo.confirm_browser_disposed(
        &scope,
        id,
        chosen.session_id,
        chosen.runner_generation,
        later,
    )
    .await
    .unwrap();
    assert!(
        repo.mark_browser_start_requested(&scope, id, later)
            .await
            .is_err()
    );
    assert_eq!(
        repo.begin_browser_operation(&scope, &chosen, later)
            .await
            .unwrap()
            .state,
        BrowserOperationState::Disposed
    );
    // A terminal replay cannot delete a new owner's gate.
    repo.reserve_account(
        &legacy,
        chosen.account_id,
        id,
        now,
        now + Duration::minutes(1),
    )
    .await
    .unwrap();
    repo.confirm_browser_disposed(
        &scope,
        id,
        chosen.session_id,
        chosen.runner_generation,
        later,
    )
    .await
    .unwrap();
    assert!(
        repo.begin_browser_operation(&scope, &make(chosen.account_id), now)
            .await
            .is_err()
    );
    let mut next = make(chosen.account_id);
    next.reservation_id = Some(id);
    repo.begin_browser_operation(&scope, &next, now)
        .await
        .unwrap();
    repo.confirm_browser_disposed(
        &scope,
        id,
        chosen.session_id,
        chosen.runner_generation,
        later,
    )
    .await
    .unwrap();
    assert!(
        repo.begin_browser_operation(&scope, &make(chosen.account_id), now)
            .await
            .is_err()
    );
    repo.release_browser_unstarted(&scope, next.operation_id, now)
        .await
        .unwrap();
    repo.release_browser_unstarted(&scope, next.operation_id, now)
        .await
        .unwrap();
    assert!(
        repo.mark_browser_start_requested(&scope, next.operation_id, now)
            .await
            .is_err()
    );
    // Legacy live gate blocks durable acquisition; only exact live adoption works.
    let mut adopted = make(Uuid::new_v4());
    let reservation = Uuid::new_v4();
    repo.reserve_account(
        &legacy,
        adopted.account_id,
        reservation,
        now,
        now + Duration::minutes(2),
    )
    .await
    .unwrap();
    assert!(
        repo.begin_browser_operation(&scope, &adopted, now)
            .await
            .is_err()
    );
    adopted.reservation_id = Some(Uuid::new_v4());
    assert!(
        repo.begin_browser_operation(&scope, &adopted, now)
            .await
            .is_err()
    );
    adopted.reservation_id = Some(reservation);
    repo.begin_browser_operation(&scope, &adopted, now)
        .await
        .unwrap();
    repo.release_account(&legacy, adopted.account_id, reservation)
        .await
        .unwrap();
    assert!(
        repo.reserve_account(
            &legacy,
            adopted.account_id,
            Uuid::new_v4(),
            later,
            later + Duration::minutes(1)
        )
        .await
        .is_err()
    );
    (scope, adopted.operation_id)
}
