use std::sync::Arc;

use chrono::Utc;
use geo_domain::{
    ChunkLocator, ContentBlock, ContentBlockKind, ContentBrief, ContentItemStatus,
    ContentMediaRepository, ContentRepository, ContentStep, DocumentManifest,
    DocumentManifestCoverage, DocumentManifestItem, DocumentManifestItemState,
    DocumentManifestState, ErrorCode, EvidenceRef, MediaObjectKey, MediaReference,
    MemoryContentMediaRepository, MemoryContentRepository, RICH_CHECK_POLICY_VERSION,
    RICH_GENERATION_POLICY_VERSION, RichContent, RichNode, StructuredDocument, TenantScope,
    VerifiedImage,
};
use uuid::Uuid;

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

fn manifest(scope: &TenantScope) -> DocumentManifest {
    let manifest_id = Uuid::new_v4();
    let knowledge_release_id = Uuid::new_v4();
    let source_version_id = Uuid::new_v4();
    DocumentManifest {
        manifest_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope.project_id.unwrap(),
        revision: 1,
        knowledge_release_id,
        planner_version: "test".into(),
        state: DocumentManifestState::Ready,
        sealed: true,
        expected_count: Some(1),
        scope_hash: "test".into(),
        items: vec![DocumentManifestItem {
            document_manifest_item_id: Uuid::new_v4(),
            manifest_id,
            knowledge_release_id,
            document_key: "image-guide".into(),
            content_type: "guide".into(),
            product_id: None,
            market: "global".into(),
            language: "en".into(),
            state: DocumentManifestItemState::Planned,
            block_reason: None,
            dependency_hash: "test".into(),
            source_version_refs: vec![source_version_id],
        }],
        coverage: DocumentManifestCoverage {
            total: 1,
            planned: 1,
            blocked: 0,
            deferred: 0,
            not_applicable: 0,
        },
    }
}

fn image() -> VerifiedImage {
    VerifiedImage {
        key: MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 1,
            sha256: "a".repeat(64),
        },
        media_type: "image/png".into(),
        byte_len: 128,
        width: 8,
        height: 8,
    }
}

fn document(image: Option<&VerifiedImage>) -> StructuredDocument {
    let content = match image {
        Some(image) => RichNode::Media {
            attrs: MediaReference {
                object_id: image.key.object_id,
                object_version: image.key.object_version,
                sha256: image.key.sha256.clone(),
                alt: "Illustration".into(),
                caption: String::new(),
            },
        },
        None => RichNode::Paragraph {
            content: vec![RichNode::Text {
                text: "Image removed".into(),
                marks: vec![],
            }],
        },
    };
    StructuredDocument {
        title: "Media authority".into(),
        schema_version: Some(2),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Rich,
            text: String::new(),
            citation_ids: vec![],
            items: vec![],
            rich: Some(RichContent {
                version: 1,
                node: content,
            }),
        }],
    }
}

async fn prepared(repository: &MemoryContentRepository, scope: &TenantScope) -> (Uuid, Uuid) {
    let manifest = manifest(scope);
    let item_id = manifest.items[0].document_manifest_item_id;
    let source_version_id = manifest.items[0].source_version_refs[0];
    let execution = repository
        .start(
            scope,
            Uuid::new_v4(),
            manifest,
            RICH_GENERATION_POLICY_VERSION,
        )
        .await
        .unwrap();
    let prepare = repository
        .claim(
            scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    repository
        .complete_prepare(
            scope,
            &prepare,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Media".into(),
                objective: "Explain".into(),
                evidence: vec![EvidenceRef {
                    source_version_id,
                    chunk_id: Some(Uuid::new_v4()),
                    locator: ChunkLocator::Manual {},
                }],
                quotes: vec![],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    (execution.execution_id, item_id)
}

#[tokio::test]
async fn rejected_media_generation_rolls_back_lease_and_authorized_media_becomes_ready() {
    let scope = scope();
    let media = Arc::new(MemoryContentMediaRepository::default());
    let repository = MemoryContentRepository::with_media_repository(media.clone());
    let (execution_id, item_id) = prepared(&repository, &scope).await;
    let image = image();
    let generate = repository
        .claim(
            &scope,
            execution_id,
            item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .complete_generate(&scope, &generate, document(Some(&image)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .get_item(&scope, execution_id, item_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ContentItemStatus::Prepared
    );
    media.create_binding(&scope, image.clone()).await.unwrap();
    let revision = repository
        .complete_generate(&scope, &generate, document(Some(&image)))
        .await
        .unwrap();
    let check = repository
        .claim(
            &scope,
            execution_id,
            item_id,
            ContentStep::Check,
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let ready = repository
        .complete_check(&scope, &check, vec![])
        .await
        .unwrap();
    assert_eq!(ready.status, ContentItemStatus::Ready);
    assert_eq!(ready.ready_revision_id, Some(revision.revision_id));
}

#[tokio::test]
async fn revoked_image_does_not_block_removal_but_reuse_of_old_revision_is_rejected() {
    let scope = scope();
    let media = Arc::new(MemoryContentMediaRepository::default());
    let repository = MemoryContentRepository::with_media_repository(media.clone());
    let image = image();
    let binding = media.create_binding(&scope, image.clone()).await.unwrap();
    let (execution_id, item_id) = prepared(&repository, &scope).await;
    let generate = repository
        .claim(
            &scope,
            execution_id,
            item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let revision = repository
        .complete_generate(&scope, &generate, document(Some(&image)))
        .await
        .unwrap();
    media
        .withdraw_binding(&scope, binding.binding_id)
        .await
        .unwrap();
    let check = repository
        .claim(
            &scope,
            execution_id,
            item_id,
            ContentStep::Check,
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .complete_check(&scope, &check, vec![])
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .get_item(&scope, execution_id, item_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ContentItemStatus::Drafted
    );
    let removed = repository
        .edit(
            &scope,
            revision.asset_id,
            revision.revision_id,
            document(None),
        )
        .await
        .unwrap();
    assert!(removed.document.media_references().is_empty());
    assert_eq!(
        repository
            .list_revisions(&scope, revision.asset_id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        repository
            .edit(
                &scope,
                revision.asset_id,
                removed.revision_id,
                document(Some(&image))
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn cross_project_binding_cannot_authorize_a_document() {
    let scope = scope();
    let other = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    let image = image();
    let media = Arc::new(MemoryContentMediaRepository::default());
    media.create_binding(&other, image.clone()).await.unwrap();
    let repository = MemoryContentRepository::with_media_repository(media);
    let (execution_id, item_id) = prepared(&repository, &scope).await;
    let lease = repository
        .claim(
            &scope,
            execution_id,
            item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .complete_generate(&scope, &lease, document(Some(&image)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn save_and_withdrawal_have_a_single_authorized_order() {
    let scope = scope();
    let media = Arc::new(MemoryContentMediaRepository::default());
    let repository = Arc::new(MemoryContentRepository::with_media_repository(
        media.clone(),
    ));
    let image = image();
    let binding = media.create_binding(&scope, image.clone()).await.unwrap();
    let (execution_id, item_id) = prepared(&repository, &scope).await;
    let lease = repository
        .claim(
            &scope,
            execution_id,
            item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let (saved, withdrawn) = tokio::join!(
        repository.complete_generate(&scope, &lease, document(Some(&image))),
        media.withdraw_binding(&scope, binding.binding_id)
    );
    withdrawn.unwrap();
    match saved {
        Err(error) => {
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(
                repository
                    .get_item(&scope, execution_id, item_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                ContentItemStatus::Prepared
            );
        }
        Ok(revision) => {
            assert_eq!(
                repository
                    .list_revisions(&scope, revision.asset_id)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
    }
}
