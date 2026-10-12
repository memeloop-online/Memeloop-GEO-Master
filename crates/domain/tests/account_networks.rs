#[path = "support/account_networks.rs"]
mod contract;
#[tokio::test]
async fn memory_account_network_contract() {
    let jobs = geo_domain::MemoryChannelJobRepository::default();
    let channels = geo_domain::MemoryChannelRepository::default();
    let operator = uuid::Uuid::new_v4().into();
    contract::verify(&jobs, &channels, operator).await;
    contract::verify_busy_legacy_bootstrap(&jobs, &channels, operator).await;
    contract::verify_project_defaults(
        &jobs,
        &channels,
        &geo_domain::TenantScope::new(
            operator,
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        ),
    )
    .await;
}
