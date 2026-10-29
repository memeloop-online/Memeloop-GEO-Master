use chrono::Utc;
use geo_domain::*;
use uuid::Uuid;

pub async fn verify(
    jobs: &impl ChannelJobRepository,
    channels: &impl ChannelRepository,
    scope: &TenantScope,
) -> Uuid {
    let now = Utc::now();
    let source = Uuid::new_v4();
    let target = Uuid::new_v4();
    let identity = format!("synthetic-{target}");
    for id in [source, target] {
        channels
            .save_account(
                scope,
                ChannelAccountRecord {
                    account: ChannelAccount {
                        account_id: id,
                        project_id: scope.project_id.unwrap(),
                        owner_kind: ChannelOwnerKind::Customer,
                        platform: "synthetic".into(),
                        group_id: None,
                        status: if id == target {
                            ChannelStatus::Ready
                        } else {
                            ChannelStatus::NeedsLogin
                        },
                        display_name: Some("Synthetic account".into()),
                        platform_account_id: (id == target).then(|| identity.clone()),
                        avatar_url: None,
                        enabled: true,
                        proxy_configured: false,
                        proxy_server: None,
                        created_at: now,
                        updated_at: now,
                    },
                    session: (id == target).then(|| ChannelSecret::new(vec![1])),
                    proxy: None,
                },
            )
            .await
            .unwrap();
    }
    let source_scope = AccountNetworkScope {
        operator_id: scope.operator_id,
        tenant_id: Some(scope.tenant_id),
        project_id: scope.project_id,
        account_id: source,
    };
    let target_scope = AccountNetworkScope {
        account_id: target,
        ..source_scope.clone()
    };
    let mut target_binding = Uuid::nil();
    for network_scope in [&source_scope, &target_scope] {
        let binding = AccountNetworkBinding {
            binding_id: Uuid::new_v4(),
            scope: network_scope.clone(),
            source: AccountNetworkSource::LegacySnapshot,
            effective_network: ChannelSecret::new(vec![2]),
            created_at: now,
        };
        jobs.set_account_network(network_scope, None, &binding, channels, now)
            .await
            .unwrap();
        if network_scope.account_id == target {
            target_binding = binding.binding_id;
        }
    }
    let operation_scope = BrowserOperationScope {
        operator_id: scope.operator_id,
        tenant_id: Some(scope.tenant_id),
        project_id: scope.project_id,
        owner_id: Uuid::new_v4(),
    };
    let operation = jobs
        .begin_browser_operation(
            &operation_scope,
            &BeginBrowserOperation {
                operation_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                account_id: source,
                purpose: BrowserOperationPurpose::Login,
                runner_generation: Uuid::new_v4(),
                effective_network: ChannelSecret::new(vec![2]),
                reservation_id: None,
                expires_at: now + chrono::Duration::minutes(20),
                network_selection: Some(BrowserNetworkSelection {
                    scope: source_scope,
                    historical_binding_id: None,
                }),
            },
            now,
        )
        .await
        .unwrap();
    jobs.mark_browser_start_requested(&operation_scope, operation.operation_id, now)
        .await
        .unwrap();
    let reservation = Uuid::new_v4();
    let mut storage = VerifiedLoginStorage {
        reconnect: Some(VerifiedLoginReconnect {
            source_account_id: source,
            reservation_id: reservation,
            network_binding_id: target_binding,
        }),
        account_id: target,
        secret_scope: scope.clone(),
        owner_kind: ChannelOwnerKind::Customer,
        platform: "synthetic".into(),
        expected_platform_account_id: Some(identity.clone()),
        expected_session: Some(ChannelSecret::new(vec![1])),
        platform_account_id: identity,
        display_name: "Verified account".into(),
        avatar_url: None,
        session: ChannelSecret::new(vec![3]),
    };
    // No destination reservation, another reservation owner, changed network,
    // another project and a stale credential snapshot cannot alter either row.
    assert!(
        jobs.save_owned_login_session(
            &operation_scope,
            operation.operation_id,
            &storage,
            channels,
            now,
        )
        .await
        .is_err()
    );
    jobs.reserve_account(
        scope,
        target,
        reservation,
        now,
        now + CHANNEL_EXECUTION_LEASE,
    )
    .await
    .unwrap();
    for invalid in 0..4 {
        match invalid {
            0 => storage.reconnect.as_mut().unwrap().reservation_id = Uuid::new_v4(),
            1 => storage.reconnect.as_mut().unwrap().network_binding_id = Uuid::new_v4(),
            2 => storage.secret_scope.project_id = Some(Uuid::new_v4().into()),
            _ => storage.expected_session = Some(ChannelSecret::new(vec![9])),
        }
        assert!(
            jobs.save_owned_login_session(
                &operation_scope,
                operation.operation_id,
                &storage,
                channels,
                now,
            )
            .await
            .is_err()
        );
        storage.reconnect.as_mut().unwrap().reservation_id = reservation;
        storage.reconnect.as_mut().unwrap().network_binding_id = target_binding;
        storage.secret_scope = scope.clone();
        storage.expected_session = Some(ChannelSecret::new(vec![1]));
        assert!(
            channels
                .get_account(scope, source)
                .await
                .unwrap()
                .account
                .enabled
        );
        assert_eq!(
            channels
                .get_account(scope, target)
                .await
                .unwrap()
                .session
                .unwrap()
                .encrypted_bytes(),
            &[1]
        );
        assert!(
            jobs.get_browser_operation(&operation_scope, operation.operation_id)
                .await
                .unwrap()
                .unwrap()
                .login_verified_at
                .is_none()
        );
    }
    jobs.save_owned_login_session(
        &operation_scope,
        operation.operation_id,
        &storage,
        channels,
        now,
    )
    .await
    .unwrap();
    let saved = channels.get_account(scope, target).await.unwrap();
    assert_eq!(saved.session.unwrap().encrypted_bytes(), &[3]);
    assert_eq!(saved.account.account_id, target);
    let placeholder = channels.get_account(scope, source).await.unwrap();
    assert!(!placeholder.account.enabled);
    assert_eq!(placeholder.account.status, ChannelStatus::Disabled);
    assert!(placeholder.session.is_none());
    assert!(placeholder.account.platform_account_id.is_none());
    assert_eq!(channels.list_accounts(scope).await.unwrap().len(), 2);
    let verified = jobs
        .get_browser_operation(&operation_scope, operation.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(verified.login_verified_account_id, Some(target));
    assert_eq!(verified.account_id, source);
    assert_eq!(verified.network_binding_id, operation.network_binding_id);
    jobs.release_account(scope, target, reservation)
        .await
        .unwrap();
    let later = now + chrono::Duration::hours(1);
    assert!(
        jobs.reserve_account(
            scope,
            target,
            Uuid::new_v4(),
            later,
            later + CHANNEL_EXECUTION_LEASE,
        )
        .await
        .is_err()
    );
    assert_eq!(
        jobs.get_account_browser_operation(scope.operator_id, target)
            .await
            .unwrap()
            .unwrap()
            .operation_id,
        operation.operation_id
    );
    let head = jobs
        .get_account_network(&target_scope)
        .await
        .unwrap()
        .unwrap();
    let pending = AccountNetworkBinding {
        binding_id: Uuid::new_v4(),
        scope: target_scope.clone(),
        source: AccountNetworkSource::LegacySnapshot,
        effective_network: ChannelSecret::new(vec![4]),
        created_at: now,
    };
    let head = jobs
        .set_account_network(&target_scope, Some(head.version), &pending, channels, now)
        .await
        .unwrap();
    assert_eq!(head.active.binding_id, target_binding);
    assert_eq!(head.pending.unwrap().binding_id, pending.binding_id);
    jobs.transition_browser_operation(
        &operation_scope,
        operation.operation_id,
        BrowserOperationTransition::Close,
        now,
    )
    .await
    .unwrap();
    assert!(
        jobs.reserve_account(
            scope,
            target,
            Uuid::new_v4(),
            later,
            later + CHANNEL_EXECUTION_LEASE,
        )
        .await
        .is_err()
    );
    jobs.transition_browser_operation(
        &operation_scope,
        operation.operation_id,
        BrowserOperationTransition::Disposed {
            session_id: operation.session_id,
            runner_generation: operation.runner_generation,
        },
        now,
    )
    .await
    .unwrap();
    let head = jobs
        .get_account_network(&target_scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.active.binding_id, pending.binding_id);
    assert!(head.pending.is_none());
    jobs.reserve_account(
        scope,
        target,
        Uuid::new_v4(),
        now,
        now + CHANNEL_EXECUTION_LEASE,
    )
    .await
    .unwrap();
    operation.operation_id
}
