//! Cross-table identity regression, separate from channel lifecycle coverage.
use chrono::Utc;
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelRepository, ChannelStatus,
    PoolAccount, PoolAccountRecord, ProjectId, TenantScope,
};
use geo_persistence::{Database, DatabaseConfig, PgChannelRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn pool_identity_and_assignments_are_scoped_and_deduplicated() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let db = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,$3)")
        .bind(operator)
        .bind(format!("channel-{operator}"))
        .bind("test operator")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,$4)")
        .bind(tenant)
        .bind(operator)
        .bind(format!("channel-{tenant}"))
        .bind("test tenant")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,$5)")
        .bind(project).bind(operator).bind(tenant).bind(format!("channel-{project}")).bind("test project")
        .execute(db.pool()).await.unwrap();
    let scope = TenantScope::new(
        operator.into(),
        tenant.into(),
        Some(ProjectId::new(project)),
    );
    let repo = PgChannelRepository::from_database(&db);
    let now = Utc::now();
    let pool = PoolAccount {
        account_id: Uuid::new_v4(),
        platform: "zhihu".into(),
        group_id: None,
        status: ChannelStatus::Ready,
        display_name: Some("verified".into()),
        platform_account_id: Some("verified-platform-identity".into()),
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: now,
        updated_at: now,
    };
    repo.save_pool_account(
        operator.into(),
        PoolAccountRecord {
            account: pool.clone(),
            session: None,
            proxy: None,
        },
    )
    .await
    .unwrap();
    assert!(
        repo.list_assigned_pool_accounts(&scope)
            .await
            .unwrap()
            .is_empty()
    );
    repo.assign_pool_account(&scope, pool.account_id, true)
        .await
        .unwrap();
    assert_eq!(
        repo.list_assigned_pool_accounts(&scope)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repo.list_pool_assignments(operator.into(), pool.account_id)
            .await
            .unwrap()
            .len(),
        1
    );
    let own = ChannelAccount {
        account_id: Uuid::new_v4(),
        project_id: ProjectId::new(project),
        owner_kind: ChannelOwnerKind::Customer,
        platform: pool.platform.clone(),
        group_id: None,
        status: ChannelStatus::Ready,
        display_name: Some("duplicate".into()),
        platform_account_id: pool.platform_account_id.clone(),
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: now,
        updated_at: now,
    };
    assert!(
        repo.save_account(
            &scope,
            ChannelAccountRecord {
                account: own,
                session: None,
                proxy: None
            }
        )
        .await
        .is_err()
    );
    repo.assign_pool_account(&scope, pool.account_id, false)
        .await
        .unwrap();
    assert!(
        repo.list_assigned_pool_accounts(&scope)
            .await
            .unwrap()
            .is_empty()
    );
    // The same pool record may reconnect without being counted twice.
    repo.save_pool_account(
        operator.into(),
        PoolAccountRecord {
            account: pool,
            session: None,
            proxy: None,
        },
    )
    .await
    .unwrap();
}
