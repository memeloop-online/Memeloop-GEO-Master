use chrono::Utc;
use geo_domain::{ChannelSecret, OperatorId, ProxyPool, ProxyPoolMember, ProxyPoolRepository};
use uuid::Uuid;

pub async fn verify(
    repo: &dyn ProxyPoolRepository,
    operator: OperatorId,
    other: OperatorId,
) -> (Uuid, Uuid, Uuid) {
    let now = Utc::now();
    let pool = ProxyPool {
        pool_id: Uuid::new_v4(),
        name: "Synthetic pool".into(),
        enabled: true,
        version: 1,
        created_at: now,
        updated_at: now,
    };
    let (a, b) = tokio::join!(
        repo.save_proxy_pool(operator, None, pool.clone()),
        repo.save_proxy_pool(operator, None, pool.clone())
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let mut renamed = pool.clone();
    renamed.version = 2;
    renamed.name = "Renamed synthetic pool".into();
    repo.save_proxy_pool(operator, Some(1), renamed.clone())
        .await
        .unwrap();
    assert!(
        repo.save_proxy_pool(operator, Some(1), renamed)
            .await
            .is_err()
    );
    assert!(repo.get_proxy_pool(other, pool.pool_id).await.is_err());
    assert!(
        repo.list_proxy_pools(other, None, 25)
            .await
            .unwrap()
            .is_empty()
    );
    let member = ProxyPoolMember {
        member_id: Uuid::new_v4(),
        pool_id: pool.pool_id,
        name: "Synthetic member".into(),
        enabled: true,
        version: 1,
        revision_id: Uuid::new_v4(),
        proxy_server: "http://fixture.invalid:3128".into(),
        created_at: now,
        updated_at: now,
    };
    assert!(
        repo.save_proxy_member(
            other,
            None,
            member.clone(),
            Some(ChannelSecret::new(vec![1]))
        )
        .await
        .is_err()
    );
    let (a, b) = tokio::join!(
        repo.save_proxy_member(
            operator,
            None,
            member.clone(),
            Some(ChannelSecret::new(vec![1]))
        ),
        repo.save_proxy_member(
            operator,
            None,
            member.clone(),
            Some(ChannelSecret::new(vec![1]))
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let mut metadata = member.clone();
    metadata.version = 2;
    metadata.enabled = false;
    repo.save_proxy_member(operator, Some(1), metadata.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        repo.get_proxy_member(operator, pool.pool_id, member.member_id)
            .await
            .unwrap()
            .revision_id,
        member.revision_id
    );
    let mut changed = metadata.clone();
    changed.version = 3;
    changed.revision_id = Uuid::new_v4();
    changed.proxy_server = "socks5://fixture.invalid:1080".into();
    assert!(
        repo.save_proxy_member(operator, Some(2), changed.clone(), None)
            .await
            .is_err()
    );
    assert!(
        repo.save_proxy_member(
            operator,
            Some(1),
            changed.clone(),
            Some(ChannelSecret::new(vec![2]))
        )
        .await
        .is_err()
    );
    assert!(
        repo.get_proxy_revision(
            operator,
            pool.pool_id,
            member.member_id,
            changed.revision_id
        )
        .await
        .is_err()
    );
    repo.save_proxy_member(
        operator,
        Some(2),
        changed.clone(),
        Some(ChannelSecret::new(vec![2])),
    )
    .await
    .unwrap();
    assert_eq!(
        repo.get_proxy_revision(operator, pool.pool_id, member.member_id, member.revision_id)
            .await
            .unwrap()
            .encrypted_bytes(),
        &[1]
    );
    assert_eq!(
        repo.get_proxy_revision(
            operator,
            pool.pool_id,
            member.member_id,
            changed.revision_id
        )
        .await
        .unwrap()
        .encrypted_bytes(),
        &[2]
    );
    let mut overwrite = changed.clone();
    overwrite.version = 4;
    assert!(
        repo.save_proxy_member(
            operator,
            Some(3),
            overwrite,
            Some(ChannelSecret::new(vec![9]))
        )
        .await
        .is_err()
    );
    assert_eq!(
        repo.get_proxy_member(operator, pool.pool_id, member.member_id)
            .await
            .unwrap()
            .version,
        3
    );
    assert!(
        repo.get_proxy_revision(other, pool.pool_id, member.member_id, member.revision_id)
            .await
            .is_err()
    );
    assert!(
        repo.get_proxy_member(operator, Uuid::new_v4(), member.member_id)
            .await
            .is_err()
    );
    assert!(
        repo.list_proxy_members(other, pool.pool_id, None, 25)
            .await
            .is_err()
    );
    assert!(
        repo.list_proxy_members(operator, pool.pool_id, Some(member.member_id), 25)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.list_proxy_members(operator, pool.pool_id, None, 1)
            .await
            .unwrap()
            .len(),
        1
    );
    (pool.pool_id, member.member_id, member.revision_id)
}
