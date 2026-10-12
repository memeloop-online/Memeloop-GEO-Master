#[path = "support/proxy_pools.rs"]
mod contract;
#[tokio::test]
async fn memory_proxy_inventory_is_scoped_versioned_and_immutable() {
    contract::verify(
        &geo_domain::MemoryChannelRepository::default(),
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
    )
    .await;
}
