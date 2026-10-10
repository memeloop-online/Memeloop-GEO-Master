use chrono::Utc;
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelRepository, ChannelSecret,
    ChannelSessionVersion, ChannelStatus, PoolAccount, PoolAccountRecord, TenantScope,
};
use uuid::Uuid;

/// The same safety contract runs against memory and disposable PostgreSQL.
pub async fn verify(repo: &dyn ChannelRepository, scope: &TenantScope) {
    for owner_kind in [ChannelOwnerKind::Customer, ChannelOwnerKind::OperatorPool] {
        let now = Utc::now();
        let account = ChannelAccount {
            account_id: Uuid::new_v4(),
            project_id: scope.project_id.unwrap(),
            owner_kind,
            platform: "kimi".into(),
            group_id: None,
            status: ChannelStatus::Ready,
            display_name: Some("Synthetic account".into()),
            platform_account_id: Some(Uuid::new_v4().to_string()),
            avatar_url: Some("https://example.org/avatar.png".into()),
            enabled: true,
            proxy_configured: true,
            proxy_server: Some("http://proxy.example.org:3128".into()),
            created_at: now,
            updated_at: now,
        };
        let mut record = ChannelAccountRecord {
            account: account.clone(),
            session: Some(ChannelSecret::new(vec![1, 2, 3])),
            proxy: Some(ChannelSecret::new(vec![8, 9])),
        };
        save(repo, scope, &record).await;
        if owner_kind == ChannelOwnerKind::OperatorPool {
            repo.assign_pool_account(scope, account.account_id, true)
                .await
                .unwrap();
        }
        let mut version = ChannelSessionVersion {
            account_id: account.account_id,
            owner_kind,
            platform: account.platform.clone(),
            platform_account_id: account.platform_account_id.clone().unwrap(),
            session: record.session.clone().unwrap(),
        };
        let before = read(repo, scope, &version).await;
        let pool_before = if owner_kind == ChannelOwnerKind::OperatorPool {
            Some(
                repo.get_pool_account(scope.operator_id, account.account_id)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        assert!(
            repo.renew_session(scope, &version, ChannelSecret::new(vec![4]))
                .await
                .unwrap()
        );
        // A metadata PATCH which read the old envelope cannot undo renewal.
        if let Some(pool_before) = pool_before {
            assert!(
                repo.save_pool_account_metadata(scope.operator_id, pool_before)
                    .await
                    .is_err()
            );
        } else {
            assert!(
                repo.save_account_metadata(scope, before.clone())
                    .await
                    .is_err()
            );
        }
        let after = read(repo, scope, &version).await;
        assert_eq!(
            serde_json::to_value(&before.account).unwrap(),
            serde_json::to_value(&after.account).unwrap()
        );
        assert_eq!(
            before.proxy.unwrap().encrypted_bytes(),
            after.proxy.unwrap().encrypted_bytes()
        );
        assert_eq!(after.session.unwrap().encrypted_bytes(), &[4]);
        // A concurrent older context cannot overwrite the winner.
        assert!(
            !repo
                .renew_session(scope, &version, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
        version.session = ChannelSecret::new(vec![4]);
        // Reconnecting the same identity still invalidates the old envelope.
        record.session = Some(ChannelSecret::new(vec![6]));
        save(repo, scope, &record).await;
        assert!(
            !repo
                .renew_session(scope, &version, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
        version.session = ChannelSecret::new(vec![6]);
        for state in [
            ChannelStatus::Disabled,
            ChannelStatus::NeedsLogin,
            ChannelStatus::Expired,
            ChannelStatus::Unverified,
        ] {
            record.account.status = state;
            save(repo, scope, &record).await;
            assert!(
                !repo
                    .renew_session(scope, &version, ChannelSecret::new(vec![5]))
                    .await
                    .unwrap()
            );
        }
        record.account.status = ChannelStatus::Ready;
        record.account.enabled = false;
        save(repo, scope, &record).await;
        assert!(
            !repo
                .renew_session(scope, &version, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
        record.account.enabled = true;
        save(repo, scope, &record).await;
        let mut mismatch = version.clone();
        mismatch.platform_account_id = "different-synthetic-identity".into();
        assert!(
            !repo
                .renew_session(scope, &mismatch, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
        mismatch.platform_account_id.clear();
        assert!(
            !repo
                .renew_session(scope, &mismatch, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
        mismatch = version.clone();
        mismatch.platform = "zhihu".into();
        assert!(
            !repo
                .renew_session(scope, &mismatch, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert!(
            !repo
                .renew_session(&other, &version, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
        // Current metadata and network edits survive a session-only write.
        record.account.display_name = Some("Edited synthetic account".into());
        record.proxy = Some(ChannelSecret::new(vec![10]));
        save(repo, scope, &record).await;
        assert!(
            repo.renew_session(scope, &version, ChannelSecret::new(vec![7]))
                .await
                .unwrap()
        );
        let current = read(repo, scope, &version).await;
        assert_eq!(current.account.display_name, record.account.display_name);
        assert_eq!(current.proxy.unwrap().encrypted_bytes(), &[10]);
        version.session = ChannelSecret::new(vec![7]);
        if owner_kind == ChannelOwnerKind::OperatorPool {
            assert!(repo.get_account(scope, account.account_id).await.is_err());
            repo.assign_pool_account(scope, account.account_id, false)
                .await
                .unwrap();
            assert!(
                !repo
                    .renew_session(scope, &version, ChannelSecret::new(vec![5]))
                    .await
                    .unwrap()
            );
            repo.delete_pool_account(scope.operator_id, account.account_id)
                .await
                .unwrap();
        } else {
            repo.delete_account(scope, account.account_id)
                .await
                .unwrap();
        }
        assert!(
            !repo
                .renew_session(scope, &version, ChannelSecret::new(vec![5]))
                .await
                .unwrap()
        );
    }
}

async fn save(repo: &dyn ChannelRepository, scope: &TenantScope, record: &ChannelAccountRecord) {
    if record.account.owner_kind == ChannelOwnerKind::Customer {
        repo.save_account(scope, record.clone()).await.unwrap();
    } else {
        let a = &record.account;
        repo.save_pool_account(
            scope.operator_id,
            PoolAccountRecord {
                account: PoolAccount {
                    account_id: a.account_id,
                    platform: a.platform.clone(),
                    group_id: a.group_id,
                    status: a.status,
                    display_name: a.display_name.clone(),
                    platform_account_id: a.platform_account_id.clone(),
                    avatar_url: a.avatar_url.clone(),
                    enabled: a.enabled,
                    proxy_configured: a.proxy_configured,
                    proxy_server: a.proxy_server.clone(),
                    created_at: a.created_at,
                    updated_at: a.updated_at,
                },
                session: record.session.clone(),
                proxy: record.proxy.clone(),
            },
        )
        .await
        .unwrap();
    }
}

async fn read(
    repo: &dyn ChannelRepository,
    scope: &TenantScope,
    version: &ChannelSessionVersion,
) -> ChannelAccountRecord {
    if version.owner_kind == ChannelOwnerKind::Customer {
        repo.get_account(scope, version.account_id).await.unwrap()
    } else {
        let record = repo
            .get_pool_account(scope.operator_id, version.account_id)
            .await
            .unwrap();
        ChannelAccountRecord {
            account: record.account.assigned_view(scope.project_id.unwrap()),
            session: record.session,
            proxy: record.proxy,
        }
    }
}
