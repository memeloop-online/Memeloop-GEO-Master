use geo_domain::{
    ContentMediaBindingState, ContentMediaRepository, ErrorCode, KnowledgePurpose,
    KnowledgeRepository, MAX_MEDIA_SNAPSHOT_BYTES, MAX_MEDIA_SNAPSHOT_IMAGES, MediaObjectKey,
    MemoryContentMediaRepository, MemoryKnowledgeRepository, TenantScope, UploadSessionCommand,
    VerifiedImage, add_media_snapshot_bytes, ordered_media_snapshot_keys, sha256_hex,
};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

async fn image(
    knowledge: &impl KnowledgeRepository,
    scope: &TenantScope,
    bytes: &[u8],
) -> VerifiedImage {
    let session = knowledge
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: "synthetic.png".into(),
                declared_media_type: "image/png".into(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    knowledge
        .put_upload_content(scope, session.upload_session_id, bytes.to_vec())
        .await
        .unwrap();
    let object = knowledge
        .complete_attachment_upload(scope, session.upload_session_id, "synthetic-attachment")
        .await
        .unwrap()
        .0;
    VerifiedImage {
        key: MediaObjectKey {
            object_id: object.object_id,
            object_version: object.object_version,
            sha256: object.sha256,
        },
        media_type: "image/png".into(),
        byte_len: bytes.len() as u64,
        width: 1,
        height: 1,
    }
}

#[tokio::test]
async fn snapshot_owns_bytes_after_withdrawal_and_requires_live_authorization() {
    let scope = scope();
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let repo = MemoryContentMediaRepository::with_knowledge_repository(knowledge.clone());
    let first = image(&*knowledge, &scope, b"synthetic first original").await;
    let second = image(&*knowledge, &scope, b"synthetic second original").await;
    let grant = repo.create_binding(&scope, first.clone()).await.unwrap();
    repo.create_binding(&scope, second.clone()).await.unwrap();
    let snapshots = repo
        .snapshot_authorized_images(
            &scope,
            &[second.key.clone(), first.key.clone(), first.key.clone()],
        )
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(
        snapshots[0].image.key,
        std::cmp::min(first.key.clone(), second.key.clone())
    );
    assert!(
        snapshots
            .iter()
            .any(|item| item.bytes.as_slice() == b"synthetic first original")
    );
    repo.withdraw_binding(&scope, grant.binding_id)
        .await
        .unwrap();
    assert_eq!(
        repo.snapshot_authorized_images(&scope, &[first.key])
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
    );
    assert_eq!(grant.state, ContentMediaBindingState::Active);
    assert!(
        snapshots
            .iter()
            .any(|item| item.bytes.as_slice() == b"synthetic first original")
    );
}

#[tokio::test]
async fn snapshots_reject_missing_source_foreign_scope_and_conflicting_digests() {
    let scope = scope();
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let original = image(&*knowledge, &scope, b"synthetic private image").await;
    let without_source = MemoryContentMediaRepository::default();
    without_source
        .create_binding(&scope, original.clone())
        .await
        .unwrap();
    assert_eq!(
        without_source
            .snapshot_authorized_images(&scope, std::slice::from_ref(&original.key))
            .await
            .unwrap_err()
            .code,
        ErrorCode::CapabilityMissing,
    );
    let repo = MemoryContentMediaRepository::with_knowledge_repository(knowledge);
    repo.create_binding(&scope, original.clone()).await.unwrap();
    let foreign = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        repo.snapshot_authorized_images(&foreign, std::slice::from_ref(&original.key))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
    );
    let mut conflict = original.key.clone();
    conflict.sha256 = "a".repeat(64);
    assert_eq!(
        repo.snapshot_authorized_images(&scope, &[original.key, conflict])
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict,
    );
}

#[tokio::test]
async fn queued_withdrawal_precedes_snapshot_without_recursive_media_lock() {
    let scope = scope();
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let repo = MemoryContentMediaRepository::with_knowledge_repository(knowledge.clone());
    let original = image(&*knowledge, &scope, b"synthetic queued image").await;
    let grant = repo.create_binding(&scope, original.clone()).await.unwrap();
    let hold = repo.read_guard().await;

    let withdrawing = repo.clone();
    let withdrawal_scope = scope.clone();
    let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
    let mut withdrawal = tokio::spawn(async move {
        let _ = queued_tx.send(());
        withdrawing
            .withdraw_binding(&withdrawal_scope, grant.binding_id)
            .await
    });
    queued_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut withdrawal)
            .await
            .is_err(),
        "the withdrawal must be waiting on the initial media guard"
    );

    let snapshotting = repo.clone();
    let snapshot_scope = scope.clone();
    let snapshot = tokio::spawn(async move {
        snapshotting
            .snapshot_authorized_images(&snapshot_scope, &[original.key])
            .await
    });
    // The immutable attachment read can finish, but the media read request
    // must queue behind the already-waiting withdrawal writer.
    tokio::task::yield_now().await;
    drop(hold);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), withdrawal)
            .await
            .expect("withdrawal cannot deadlock")
            .expect("withdrawal task")
            .expect("withdrawal succeeds")
            .state,
        ContentMediaBindingState::Withdrawn
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), snapshot)
            .await
            .expect("snapshot cannot deadlock")
            .expect("snapshot task")
            .expect_err("withdrawal wins authorization")
            .code,
        ErrorCode::Conflict
    );
}

#[test]
fn snapshot_bounds_are_checked_without_allocating_blob_bytes() {
    let keys = (0..=MAX_MEDIA_SNAPSHOT_IMAGES)
        .map(|_| MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 1,
            sha256: "a".repeat(64),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ordered_media_snapshot_keys(&keys).unwrap_err().code,
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        ordered_media_snapshot_keys(&[keys[0].clone(), keys[0].clone()])
            .unwrap()
            .len(),
        1,
    );
    assert_eq!(
        add_media_snapshot_bytes(MAX_MEDIA_SNAPSHOT_BYTES, 1)
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest,
    );
}
