use chrono::{Duration, Utc};
use geo_domain::*;
use uuid::Uuid;

pub async fn verify_project_defaults(
    jobs: &impl ChannelJobRepository,
    channels: &impl ChannelRepository,
    scope: &TenantScope,
) {
    let now = Utc::now();
    let project = scope.project_id.unwrap();
    let old = ChannelSettingsRecord {
        settings: ChannelSettings {
            project_id: project,
            default_group_id: None,
            proxy_configured: true,
            proxy_server: Some("http://example.invalid".into()),
            updated_at: now,
        },
        proxy: Some(ChannelSecret::new(vec![1])),
    };
    channels.save_settings(scope, old.clone()).await.unwrap();
    let mut accounts = Vec::new();
    for _ in 0..6 {
        let account = ChannelAccount {
            account_id: Uuid::new_v4(),
            project_id: project,
            owner_kind: ChannelOwnerKind::Customer,
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
        };
        channels
            .save_account(
                scope,
                ChannelAccountRecord {
                    account: account.clone(),
                    session: None,
                    proxy: None,
                },
            )
            .await
            .unwrap();
        accounts.push(account.account_id);
    }
    let net_scope = |account_id| AccountNetworkScope {
        operator_id: scope.operator_id,
        account_id,
        tenant_id: Some(scope.tenant_id),
        project_id: Some(project),
    };
    let binding = |account_id, source, value| AccountNetworkBinding {
        binding_id: Uuid::new_v4(),
        scope: net_scope(account_id),
        source,
        effective_network: ChannelSecret::new(vec![value]),
        created_at: now,
    };
    let inherited = binding(
        accounts[0],
        AccountNetworkSource::ProjectDefault {
            settings_updated_at: Some(now),
        },
        1,
    );
    jobs.set_account_network(&inherited.scope, None, &inherited, channels, now)
        .await
        .unwrap();
    let explicit = binding(accounts[1], AccountNetworkSource::Direct, 9);
    jobs.set_account_network(&explicit.scope, None, &explicit, channels, now)
        .await
        .unwrap();
    let idle_inherited = binding(
        accounts[4],
        AccountNetworkSource::ProjectDefault {
            settings_updated_at: Some(now),
        },
        1,
    );
    jobs.set_account_network(&idle_inherited.scope, None, &idle_inherited, channels, now)
        .await
        .unwrap();
    let override_inherited = binding(
        accounts[5],
        AccountNetworkSource::ProjectDefault {
            settings_updated_at: Some(now),
        },
        1,
    );
    jobs.set_account_network(
        &override_inherited.scope,
        None,
        &override_inherited,
        channels,
        now,
    )
    .await
    .unwrap();
    let owner = BrowserOperationScope {
        operator_id: scope.operator_id,
        tenant_id: Some(scope.tenant_id),
        project_id: Some(project),
        owner_id: Uuid::new_v4(),
    };
    let begin = |account_id, network_selection| BeginBrowserOperation {
        network_selection,
        operation_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        account_id,
        purpose: BrowserOperationPurpose::Login,
        runner_generation: Uuid::new_v4(),
        effective_network: ChannelSecret::new(vec![1]),
        reservation_id: None,
        expires_at: now + Duration::minutes(5),
    };
    let running = jobs
        .begin_browser_operation(
            &owner,
            &begin(
                accounts[0],
                Some(BrowserNetworkSelection {
                    scope: inherited.scope.clone(),
                    historical_binding_id: None,
                }),
            ),
            now,
        )
        .await
        .unwrap();
    let legacy_op = jobs
        .begin_browser_operation(&owner, &begin(accounts[2], None), now)
        .await
        .unwrap();
    let override_op = jobs
        .begin_browser_operation(
            &owner,
            &begin(
                accounts[5],
                Some(BrowserNetworkSelection {
                    scope: override_inherited.scope.clone(),
                    historical_binding_id: None,
                }),
            ),
            now,
        )
        .await
        .unwrap();
    let explicit_pending = binding(accounts[5], AccountNetworkSource::Direct, 9);
    jobs.set_account_network(
        &explicit_pending.scope,
        Some(1),
        &explicit_pending,
        channels,
        now,
    )
    .await
    .unwrap();
    let mut new = old.clone();
    new.settings.updated_at = now + Duration::seconds(1);
    new.proxy = Some(ChannelSecret::new(vec![2]));
    assert!(
        jobs.set_project_network_defaults(scope, Some(&old), &new, &[], channels, now)
            .await
            .is_err()
    );
    assert_eq!(
        channels
            .get_settings(scope)
            .await
            .unwrap()
            .unwrap()
            .proxy
            .unwrap()
            .encrypted_bytes(),
        &[1]
    );
    let source = AccountNetworkSource::ProjectDefault {
        settings_updated_at: Some(new.settings.updated_at),
    };
    let updates = vec![
        ProjectNetworkBindingUpdate {
            expected_version: Some(1),
            binding: binding(accounts[0], source.clone(), 2),
            legacy: None,
        },
        ProjectNetworkBindingUpdate {
            expected_version: None,
            binding: binding(accounts[2], source.clone(), 2),
            legacy: Some(LegacyNetworkBootstrap {
                binding: binding(accounts[2], AccountNetworkSource::LegacySnapshot, 1),
                operation_id: Some(legacy_op.operation_id),
            }),
        },
        ProjectNetworkBindingUpdate {
            expected_version: Some(1),
            binding: binding(accounts[4], source, 2),
            legacy: None,
        },
    ];
    jobs.set_project_network_defaults(scope, Some(&old), &new, &updates, channels, now)
        .await
        .unwrap();
    assert_eq!(
        channels
            .get_settings(scope)
            .await
            .unwrap()
            .unwrap()
            .proxy
            .unwrap()
            .encrypted_bytes(),
        &[2]
    );
    let state = jobs
        .get_account_network(&inherited.scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.active.binding_id, inherited.binding_id);
    assert_eq!(
        state.pending.unwrap().effective_network.encrypted_bytes(),
        &[2]
    );
    assert_eq!(
        jobs.get_account_network(&explicit.scope)
            .await
            .unwrap()
            .unwrap()
            .active
            .binding_id,
        explicit.binding_id
    );
    let untouched = jobs
        .get_account_network(&explicit_pending.scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.version, 2);
    assert_eq!(untouched.active.binding_id, override_inherited.binding_id);
    assert_eq!(
        untouched.pending.unwrap().binding_id,
        explicit_pending.binding_id
    );
    assert_eq!(
        jobs.get_account_network(&idle_inherited.scope)
            .await
            .unwrap()
            .unwrap()
            .active
            .effective_network
            .encrypted_bytes(),
        &[2]
    );
    let mut rejected_default = new.clone();
    rejected_default.settings.updated_at = now + Duration::seconds(3);
    rejected_default.proxy = Some(ChannelSecret::new(vec![3]));
    let mut rejected_updates = vec![
        ProjectNetworkBindingUpdate {
            expected_version: Some(2),
            binding: binding(
                accounts[0],
                AccountNetworkSource::ProjectDefault {
                    settings_updated_at: Some(rejected_default.settings.updated_at),
                },
                3,
            ),
            legacy: None,
        },
        ProjectNetworkBindingUpdate {
            expected_version: Some(1),
            binding: binding(
                accounts[2],
                AccountNetworkSource::ProjectDefault {
                    settings_updated_at: Some(rejected_default.settings.updated_at),
                },
                3,
            ),
            legacy: None,
        },
        ProjectNetworkBindingUpdate {
            expected_version: Some(2),
            binding: binding(
                accounts[4],
                AccountNetworkSource::ProjectDefault {
                    settings_updated_at: Some(rejected_default.settings.updated_at),
                },
                3,
            ),
            legacy: None,
        },
    ];
    // PostgreSQL applies heads in account-ID order; fail the final update so
    // this assertion always exercises rollback after earlier successful writes.
    rejected_updates
        .iter_mut()
        .max_by_key(|u| u.binding.scope.account_id)
        .unwrap()
        .expected_version = Some(0);
    assert!(
        jobs.set_project_network_defaults(
            scope,
            Some(&new),
            &rejected_default,
            &rejected_updates,
            channels,
            now
        )
        .await
        .is_err()
    );
    assert_eq!(
        channels
            .get_settings(scope)
            .await
            .unwrap()
            .unwrap()
            .proxy
            .unwrap()
            .encrypted_bytes(),
        &[2]
    );
    assert_eq!(
        jobs.get_account_network(&inherited.scope)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    let mut group_only = new.clone();
    group_only.settings.updated_at = now + Duration::seconds(2);
    let mut stale_settings = new.clone();
    stale_settings.settings.updated_at = now - Duration::seconds(1);
    assert!(
        jobs.set_project_network_defaults(
            scope,
            Some(&stale_settings),
            &group_only,
            &[],
            channels,
            now
        )
        .await
        .is_err()
    );
    jobs.set_project_network_defaults(scope, Some(&new), &group_only, &[], channels, now)
        .await
        .unwrap();
    assert_eq!(
        jobs.get_account_network(&inherited.scope)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    let stale = binding(
        accounts[3],
        AccountNetworkSource::ProjectDefault {
            settings_updated_at: Some(now),
        },
        1,
    );
    assert!(
        jobs.set_account_network(&stale.scope, None, &stale, channels, now)
            .await
            .is_err()
    );
    jobs.release_browser_unstarted(&owner, running.operation_id, now)
        .await
        .unwrap();
    jobs.release_browser_unstarted(&owner, legacy_op.operation_id, now)
        .await
        .unwrap();
    jobs.release_browser_unstarted(&owner, override_op.operation_id, now)
        .await
        .unwrap();
    assert_eq!(
        jobs.get_account_network(&inherited.scope)
            .await
            .unwrap()
            .unwrap()
            .active
            .effective_network
            .encrypted_bytes(),
        &[2]
    );
    let expected = channels.get_account(scope, accounts[1]).await.unwrap();
    let mut updated = expected.clone();
    updated.account.display_name = Some("Atomic edit".into());
    let metadata = AccountNetworkMetadataUpdate::Customer {
        expected: expected.clone(),
        updated,
    };
    let target = binding(accounts[1], AccountNetworkSource::Standalone, 3);
    assert!(
        jobs.set_account_network_with_metadata(
            &explicit.scope,
            Some(99),
            &target,
            None,
            Some(&metadata),
            channels,
            now
        )
        .await
        .is_err()
    );
    assert!(
        channels
            .get_account(scope, accounts[1])
            .await
            .unwrap()
            .account
            .display_name
            .is_none()
    );
    jobs.set_account_network_with_metadata(
        &explicit.scope,
        Some(1),
        &target,
        None,
        Some(&metadata),
        channels,
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        channels
            .get_account(scope, accounts[1])
            .await
            .unwrap()
            .account
            .display_name
            .as_deref(),
        Some("Atomic edit")
    );
    let rejected = binding(accounts[1], AccountNetworkSource::Direct, 4);
    assert!(
        jobs.set_account_network_with_metadata(
            &explicit.scope,
            Some(2),
            &rejected,
            None,
            Some(&metadata),
            channels,
            now
        )
        .await
        .is_err()
    );
    assert_eq!(
        jobs.get_account_network(&explicit.scope)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    assert!(
        jobs.get_network_binding(&explicit.scope, rejected.binding_id)
            .await
            .is_err()
    );
    // An actual binding replacement and an independent session renewal commute:
    // neither whole-record stale write can undo the other.
    let mut ready = channels.get_account(scope, accounts[1]).await.unwrap();
    ready.account.status = ChannelStatus::Ready;
    ready.account.platform_account_id = Some(format!("synthetic-{}", accounts[1]));
    ready.session = Some(ChannelSecret::new(vec![11]));
    channels.save_account(scope, ready.clone()).await.unwrap();
    let version = ChannelSessionVersion {
        account_id: accounts[1],
        owner_kind: ChannelOwnerKind::Customer,
        platform: ready.account.platform.clone(),
        platform_account_id: ready.account.platform_account_id.clone().unwrap(),
        session: ready.session.unwrap(),
    };
    let (network, renewed) = tokio::join!(
        jobs.set_account_network(&explicit.scope, Some(2), &rejected, channels, now),
        channels.renew_session(scope, &version, ChannelSecret::new(vec![12]))
    );
    assert_eq!(network.unwrap().active.binding_id, rejected.binding_id);
    assert!(renewed.unwrap());
    assert_eq!(
        channels
            .get_account(scope, accounts[1])
            .await
            .unwrap()
            .session
            .unwrap()
            .encrypted_bytes(),
        &[12]
    );
    assert_eq!(
        jobs.get_account_network(&explicit.scope)
            .await
            .unwrap()
            .unwrap()
            .active
            .effective_network
            .encrypted_bytes(),
        &[4]
    );
}

pub async fn verify(
    jobs: &impl ChannelJobRepository,
    channels: &impl ChannelRepository,
    operator: OperatorId,
) -> AccountNetworkScope {
    let now = Utc::now();
    let account = PoolAccount {
        account_id: Uuid::new_v4(),
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
    let scope = AccountNetworkScope {
        operator_id: operator,
        account_id: account.account_id,
        tenant_id: None,
        project_id: None,
    };
    channels
        .save_pool_account(
            operator,
            PoolAccountRecord {
                account,
                session: None,
                proxy: Some(ChannelSecret::new(vec![7])),
            },
        )
        .await
        .unwrap();
    let binding = |value| AccountNetworkBinding {
        binding_id: Uuid::new_v4(),
        scope: scope.clone(),
        source: AccountNetworkSource::Direct,
        effective_network: ChannelSecret::new(vec![value]),
        created_at: now,
    };
    assert!(jobs.get_account_network(&scope).await.unwrap().is_none());
    let first = binding(1);
    jobs.set_account_network(&scope, None, &first, channels, now)
        .await
        .unwrap();
    let mut wrong = scope.clone();
    wrong.tenant_id = Some(Uuid::new_v4().into());
    wrong.project_id = Some(Uuid::new_v4().into());
    assert!(
        jobs.get_network_binding(&wrong, first.binding_id)
            .await
            .is_err()
    );
    assert!(jobs.get_account_network(&wrong).await.is_err());
    assert!(
        jobs.set_account_network(&scope, None, &binding(8), channels, now)
            .await
            .is_err()
    );
    assert!(
        jobs.set_account_network(&scope, Some(1), &first, channels, now)
            .await
            .is_err()
    );
    let operation_scope = BrowserOperationScope {
        operator_id: operator,
        tenant_id: None,
        project_id: None,
        owner_id: Uuid::new_v4(),
    };
    let begin = |historical_binding_id| BeginBrowserOperation {
        network_selection: Some(BrowserNetworkSelection {
            scope: scope.clone(),
            historical_binding_id,
        }),
        operation_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        account_id: scope.account_id,
        purpose: BrowserOperationPurpose::Login,
        runner_generation: Uuid::new_v4(),
        effective_network: ChannelSecret::new(vec![99]),
        reservation_id: None,
        expires_at: now + Duration::minutes(5),
    };
    let input = begin(None);
    let op = jobs
        .begin_browser_operation(&operation_scope, &input, now)
        .await
        .unwrap();
    assert_eq!(op.network_binding_id, Some(first.binding_id));
    assert_eq!(op.effective_network.encrypted_bytes(), &[1]);
    let second = binding(2);
    let third = binding(3);
    let (a, b) = tokio::join!(
        jobs.set_account_network(&scope, Some(1), &second, channels, now),
        jobs.set_account_network(&scope, Some(1), &third, channels, now)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let pending = if a.is_ok() { second } else { third };
    let state = jobs.get_account_network(&scope).await.unwrap().unwrap();
    assert!(state.busy);
    assert_eq!(state.active.binding_id, first.binding_id);
    assert_eq!(state.pending.unwrap().binding_id, pending.binding_id);
    assert!(
        jobs.confirm_browser_disposed(
            &operation_scope,
            op.operation_id,
            op.session_id,
            Uuid::new_v4(),
            now
        )
        .await
        .is_err()
    );
    assert_eq!(
        jobs.get_account_network(&scope)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    jobs.release_browser_unstarted(&operation_scope, op.operation_id, now)
        .await
        .unwrap();
    let state = jobs.get_account_network(&scope).await.unwrap().unwrap();
    assert!(!state.busy);
    assert!(state.pending.is_none());
    assert_eq!(state.version, 3);
    assert_eq!(state.active.binding_id, pending.binding_id);
    assert_eq!(
        jobs.get_network_binding(&scope, first.binding_id)
            .await
            .unwrap()
            .effective_network
            .encrypted_bytes(),
        &[1]
    );
    assert_eq!(
        jobs.begin_browser_operation(&operation_scope, &input, now + Duration::hours(1))
            .await
            .unwrap()
            .network_binding_id,
        Some(first.binding_id)
    );
    // Historical scope/pin wins over current active, never over another account.
    let historical = jobs
        .begin_browser_operation(&operation_scope, &begin(Some(first.binding_id)), now)
        .await
        .unwrap();
    assert_eq!(historical.effective_network.encrypted_bytes(), &[1]);
    let fourth = binding(4);
    jobs.set_account_network(&scope, Some(3), &fourth, channels, now)
        .await
        .unwrap();
    // Replayed old disposal cannot promote a successor's pending network.
    jobs.confirm_browser_disposed(
        &operation_scope,
        op.operation_id,
        op.session_id,
        op.runner_generation,
        now,
    )
    .await
    .unwrap();
    let state = jobs.get_account_network(&scope).await.unwrap().unwrap();
    assert_eq!(state.active.binding_id, pending.binding_id);
    assert_eq!(state.pending.unwrap().binding_id, fourth.binding_id);
    jobs.release_browser_unstarted(&operation_scope, historical.operation_id, now)
        .await
        .unwrap();
    // Metadata edits cannot reinstall stale legacy proxy bytes or flags.
    let mut metadata = channels
        .get_pool_account(operator, scope.account_id)
        .await
        .unwrap();
    metadata.proxy = Some(ChannelSecret::new(vec![88]));
    metadata.account.proxy_configured = false;
    metadata.account.proxy_server = None;
    metadata.account.display_name = Some("Edited metadata".into());
    channels
        .save_pool_account_metadata(operator, metadata)
        .await
        .unwrap();
    let readback = channels
        .get_pool_account(operator, scope.account_id)
        .await
        .unwrap();
    assert_eq!(readback.proxy.unwrap().encrypted_bytes(), &[7]);
    assert!(readback.account.proxy_configured);
    assert_eq!(
        readback.account.display_name.as_deref(),
        Some("Edited metadata")
    );
    let pool = ProxyPool {
        pool_id: Uuid::new_v4(),
        name: "Synthetic pool".into(),
        enabled: true,
        version: 1,
        created_at: now,
        updated_at: now,
    };
    channels
        .save_proxy_pool(operator, None, pool.clone())
        .await
        .unwrap();
    let mut member = ProxyPoolMember {
        pool_id: pool.pool_id,
        member_id: Uuid::new_v4(),
        name: "Synthetic member".into(),
        enabled: true,
        version: 1,
        revision_id: Uuid::new_v4(),
        proxy_server: "http://example.invalid".into(),
        created_at: now,
        updated_at: now,
    };
    channels
        .save_proxy_member(
            operator,
            None,
            member.clone(),
            Some(ChannelSecret::new(vec![5])),
        )
        .await
        .unwrap();
    let mut pool_binding = binding(5);
    pool_binding.source = AccountNetworkSource::PoolRevision {
        pool_id: pool.pool_id,
        member_id: member.member_id,
        revision_id: member.revision_id,
    };
    let version = jobs
        .get_account_network(&scope)
        .await
        .unwrap()
        .unwrap()
        .version;
    jobs.set_account_network(&scope, Some(version), &pool_binding, channels, now)
        .await
        .unwrap();
    member.enabled = false;
    member.version = 2;
    channels
        .save_proxy_member(operator, Some(1), member.clone(), None)
        .await
        .unwrap();
    let mut disabled = pool_binding.clone();
    disabled.binding_id = Uuid::new_v4();
    assert!(
        jobs.set_account_network(&scope, Some(version + 1), &disabled, channels, now)
            .await
            .is_err()
    );
    // Rotation/disable cannot rewrite history. Restoring old execution is pinned.
    let original = jobs
        .begin_browser_operation(&operation_scope, &begin(Some(pool_binding.binding_id)), now)
        .await
        .unwrap();
    assert_eq!(original.effective_network.encrypted_bytes(), &[5]);
    jobs.release_browser_unstarted(&operation_scope, original.operation_id, now)
        .await
        .unwrap();
    scope
}

pub async fn verify_busy_legacy_bootstrap(
    jobs: &impl ChannelJobRepository,
    channels: &impl ChannelRepository,
    operator: OperatorId,
) {
    let now = Utc::now();
    let account_id = Uuid::new_v4();
    channels
        .save_pool_account(
            operator,
            PoolAccountRecord {
                account: PoolAccount {
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
                },
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
    let scope = AccountNetworkScope {
        operator_id: operator,
        account_id,
        tenant_id: None,
        project_id: None,
    };
    let owner = BrowserOperationScope {
        operator_id: operator,
        tenant_id: None,
        project_id: None,
        owner_id: Uuid::new_v4(),
    };
    let op = jobs
        .begin_browser_operation(
            &owner,
            &BeginBrowserOperation {
                network_selection: None,
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
    let target = AccountNetworkBinding {
        binding_id: Uuid::new_v4(),
        scope: scope.clone(),
        source: AccountNetworkSource::Direct,
        effective_network: ChannelSecret::new(vec![2]),
        created_at: now,
    };
    assert!(
        jobs.set_account_network(&scope, None, &target, channels, now)
            .await
            .is_err()
    );
    assert!(jobs.get_account_network(&scope).await.unwrap().is_none());
    let mut legacy = LegacyNetworkBootstrap {
        binding: AccountNetworkBinding {
            binding_id: Uuid::new_v4(),
            scope: scope.clone(),
            source: AccountNetworkSource::LegacySnapshot,
            effective_network: ChannelSecret::new(vec![1]),
            created_at: now,
        },
        operation_id: Some(Uuid::new_v4()),
    };
    assert!(
        jobs.set_account_network_with_legacy(&scope, None, &target, Some(&legacy), channels, now)
            .await
            .is_err()
    );
    legacy.operation_id = Some(op.operation_id);
    let state = jobs
        .set_account_network_with_legacy(&scope, None, &target, Some(&legacy), channels, now)
        .await
        .unwrap();
    assert_eq!(state.version, 1);
    assert_eq!(state.active.binding_id, legacy.binding.binding_id);
    assert_eq!(state.pending.unwrap().binding_id, target.binding_id);
    jobs.release_browser_unstarted(&owner, op.operation_id, now)
        .await
        .unwrap();
    assert_eq!(
        jobs.get_account_network(&scope)
            .await
            .unwrap()
            .unwrap()
            .active
            .binding_id,
        target.binding_id
    );
}
