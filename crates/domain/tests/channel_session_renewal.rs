#[path = "support/channel_session_renewal.rs"]
mod contract;

#[tokio::test]
async fn renewal_is_session_only_scoped_and_compare_and_swap() {
    let scope = geo_domain::TenantScope::new(
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
        Some(uuid::Uuid::new_v4().into()),
    );
    contract::verify(&geo_domain::MemoryChannelRepository::default(), &scope).await;
}
