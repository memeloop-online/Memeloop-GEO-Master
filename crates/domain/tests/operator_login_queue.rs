#[path = "support/operator_login_queue.rs"]
mod contract;

#[tokio::test]
async fn memory_operator_login_queue_contract() {
    contract::verify(
        &geo_domain::MemoryChannelJobRepository::default(),
        &geo_domain::MemoryChannelRepository::default(),
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
    )
    .await;
}
