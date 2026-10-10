use geo_domain::{
    ContentMediaBindingState, ContentMediaRepository, ErrorCode, MediaObjectKey,
    MemoryContentMediaRepository, TenantScope, VerifiedImage,
};
use std::time::Duration;
use tokio::time::timeout;
use uuid::Uuid;

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

fn image() -> VerifiedImage {
    VerifiedImage {
        key: MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 1,
            sha256: "a".repeat(64),
        },
        media_type: "image/png".into(),
        byte_len: 1024,
        width: 320,
        height: 240,
    }
}

#[test]
fn image_input_rejects_unknown_fields_and_invalid_metadata() {
    let original = image();
    assert!(original.validate().is_ok());
    for payload in [
        serde_json::json!({
            "key": original.key, "media_type": "image/png",
            "byte_len": 1024, "width": 320, "height": 240, "unknown": 7
        }),
        serde_json::json!({
            "key": {"object_id": Uuid::new_v4(), "object_version": 1,
                    "sha256": "a".repeat(64), "unknown": 7},
            "media_type": "image/png", "byte_len": 1024, "width": 320, "height": 240
        }),
    ] {
        assert!(serde_json::from_value::<VerifiedImage>(payload).is_err());
    }
    let mut invalid = image();
    invalid.key.object_version = 0;
    assert_eq!(
        invalid.validate().unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    invalid = image();
    invalid.key.sha256 = "F".repeat(64);
    assert!(invalid.validate().is_err());
    invalid = image();
    invalid.media_type = "image/svg+xml".into();
    assert!(invalid.validate().is_err());
    invalid = image();
    invalid.byte_len = 0;
    assert!(invalid.validate().is_err());
    invalid = image();
    invalid.byte_len = geo_domain::MAX_UPLOAD_BYTES + 1;
    assert!(invalid.validate().is_err());
    invalid = image();
    invalid.width = 0;
    assert!(invalid.validate().is_err());
    invalid = image();
    invalid.width = 16_385;
    assert!(invalid.validate().is_err());
    invalid = image();
    invalid.width = 16_384;
    invalid.height = 16_384;
    assert!(invalid.validate().is_err());
}

#[tokio::test]
async fn binding_replays_only_exact_metadata_and_withdrawal_is_terminal() {
    let repo = MemoryContentMediaRepository::default();
    let scope = scope();
    let original = image();
    let first = repo.create_binding(&scope, original.clone()).await.unwrap();
    let replay = repo.create_binding(&scope, original.clone()).await.unwrap();
    assert_eq!(first, replay);

    let mut changed = original.clone();
    changed.height += 1;
    assert_eq!(
        repo.create_binding(&scope, changed).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let withdrawn = repo
        .withdraw_binding(&scope, first.binding_id)
        .await
        .unwrap();
    assert_eq!(withdrawn.state, ContentMediaBindingState::Withdrawn);
    assert!(withdrawn.withdrawn_at.is_some());
    assert_eq!(
        repo.withdraw_binding(&scope, first.binding_id)
            .await
            .unwrap(),
        withdrawn
    );
    assert_eq!(
        repo.create_binding(&scope, original.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repo.hold_authorized_images(&scope, &[original.key])
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::Conflict
    );
    assert!(
        repo.list_bindings(&scope, None, 20)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.get_binding(&scope, first.binding_id).await.unwrap(),
        Some(withdrawn)
    );
}

#[tokio::test]
async fn bindings_hide_other_projects_and_page_by_uuid() {
    let repo = MemoryContentMediaRepository::default();
    let owner = scope();
    let other_project = TenantScope::new(
        owner.operator_id,
        owner.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    let other_tenant = TenantScope::new(owner.operator_id, Uuid::new_v4().into(), owner.project_id);
    let shared_image = image();
    let first = repo
        .create_binding(&owner, shared_image.clone())
        .await
        .unwrap();
    let second = repo
        .create_binding(&other_project, shared_image)
        .await
        .unwrap();
    assert_ne!(first.binding_id, second.binding_id);
    assert_eq!(
        repo.get_binding(&owner, second.binding_id).await.unwrap(),
        None
    );
    assert_eq!(
        repo.withdraw_binding(&other_tenant, first.binding_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repo.hold_authorized_images(&other_tenant, &[first.image.key])
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::Conflict
    );
    for _ in 0..8 {
        repo.create_binding(&owner, image()).await.unwrap();
    }
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let page = repo.list_bindings(&owner, after, 2).await.unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|binding| binding.binding_id);
        all.extend(page.into_iter().map(|binding| binding.binding_id));
    }
    assert_eq!(all.len(), 9);
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        repo.list_bindings(&other_project, None, 5)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        repo.list_bindings(&owner, None, 0)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn held_guard_blocks_withdrawal_until_content_commit_releases_it() {
    let repo = MemoryContentMediaRepository::default();
    let scope = scope();
    let original = image();
    let binding = repo.create_binding(&scope, original.clone()).await.unwrap();
    let hold = repo.read_guard().await;
    assert_eq!(
        hold.validate(&scope, &[original.key.clone(), original.key.clone()])
            .unwrap(),
        vec![binding.clone(), binding.clone()]
    );
    let other_scope = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        hold.validate(&other_scope, &[original.key])
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let clone = repo.clone();
    let target = scope.clone();
    let mut withdrawal = tokio::spawn(async move {
        let _ = started_tx.send(());
        clone.withdraw_binding(&target, binding.binding_id).await
    });
    started_rx.await.unwrap();
    assert!(
        timeout(Duration::from_millis(30), &mut withdrawal)
            .await
            .is_err()
    );
    drop(hold);
    assert_eq!(
        timeout(Duration::from_secs(1), withdrawal)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .state,
        ContentMediaBindingState::Withdrawn
    );
}
