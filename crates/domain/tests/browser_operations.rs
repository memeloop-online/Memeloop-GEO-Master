#[path = "support/browser_operations.rs"]
mod contract;

#[tokio::test]
async fn memory_browser_ownership_contract() {
    let repo = geo_domain::MemoryChannelJobRepository::default();
    contract::verify(&repo, uuid::Uuid::new_v4().into()).await;
}

#[tokio::test]
async fn memory_login_commit_requires_current_operation_and_session_snapshot() {
    contract::verify_login_commit(
        &geo_domain::MemoryChannelJobRepository::default(),
        &geo_domain::MemoryChannelRepository::default(),
        uuid::Uuid::new_v4().into(),
    )
    .await;
}
