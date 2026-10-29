#[path = "support/customer_reconnect.rs"]
mod contract;

#[tokio::test]
async fn memory_customer_reconnect_preserves_ownership_and_cas() {
    let scope = geo_domain::TenantScope::new(
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
        Some(uuid::Uuid::new_v4().into()),
    );
    contract::verify(
        &geo_domain::MemoryChannelJobRepository::default(),
        &geo_domain::MemoryChannelRepository::default(),
        &scope,
    )
    .await;
}
