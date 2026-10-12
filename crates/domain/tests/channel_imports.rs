#[path = "support/channel_imports.rs"]
mod contract;

#[tokio::test]
async fn memory_import_rows_are_atomic_scoped_and_replayable() {
    contract::verify(
        &geo_domain::MemoryChannelRepository::default(),
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
    )
    .await;
}
