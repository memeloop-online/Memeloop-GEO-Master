use std::sync::Arc;

use geo_api::RepositoryHostOps;
use geo_domain::{
    DocumentManifestPlanRequest, DocumentScope, ImportItem, KnowledgePurpose, KnowledgeRepository,
    MemoryKnowledgeRepository, SourceKind, TenantScope,
};
use geo_worker::{
    HostOpErrorCode, HostOps, ManifestKind, ManifestPlanningState, ManifestReadRequest,
};
use uuid::Uuid;

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

fn read(manifest_id: Uuid, cursor: Option<String>) -> ManifestReadRequest {
    ManifestReadRequest {
        manifest_id: Some(manifest_id),
        kind: ManifestKind::Document,
        revision: Some(1),
        cursor,
        limit: Some(1),
    }
}

async fn planned_fixture(purpose: KnowledgePurpose) -> (TenantScope, RepositoryHostOps, Uuid) {
    let scope = scope();
    let repository = Arc::new(MemoryKnowledgeRepository::default());
    let imported = repository
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: "source".into(),
                kind: SourceKind::Text,
                name: "source".into(),
                purpose,
                text: Some("Description with evidence".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let manifest_id = Uuid::new_v4();
    repository
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id,
                knowledge_release_id: imported.items[0]
                    .release
                    .as_ref()
                    .unwrap()
                    .knowledge_release_id,
            },
            DocumentScope {
                markets: vec!["CN".into(), "US".into()],
                languages: vec!["zh".into()],
                content_types: vec!["faq".into()],
                ..DocumentScope::default()
            },
        )
        .await
        .unwrap();
    (scope, RepositoryHostOps::new(repository), manifest_id)
}

#[tokio::test]
async fn pages_a_sealed_planning_snapshot_without_inventing_generated_revisions() {
    let (scope, ops, id) = planned_fixture(KnowledgePurpose::Public).await;
    let first = ops.manifest_read(&scope, read(id, None)).await.unwrap();
    first.validate_for(&read(id, None)).unwrap();
    assert!(first.sealed);
    assert_eq!(first.state, "ready");
    assert_eq!(first.expected_count, Some(2));
    let coverage = first.coverage.unwrap();
    assert_eq!(
        (coverage.total, coverage.planned, coverage.blocked),
        (2, 2, 0)
    );
    assert_eq!(first.items.len(), 1);
    assert_eq!(
        first.items[0].planning_state,
        Some(ManifestPlanningState::Planned)
    );
    assert!(first.items[0].document_manifest_item_id.is_some());
    assert!(first.items[0].document_revision_id.is_none());
    assert!(first.items[0].platform_target_id.is_none());
    let cursor = first.next_cursor.unwrap();
    let second = ops
        .manifest_read(&scope, read(id, Some(cursor.clone())))
        .await
        .unwrap();
    assert_ne!(first.items[0].branch_id, second.items[0].branch_id);
    assert!(second.next_cursor.is_none());
    let replay = ops
        .manifest_read(&scope, read(id, Some(cursor)))
        .await
        .unwrap();
    assert_eq!(second, replay);
}

#[tokio::test]
async fn blocked_items_remain_in_the_denominator_with_their_reason() {
    let (scope, ops, id) = planned_fixture(KnowledgePurpose::Internal).await;
    let page = ops.manifest_read(&scope, read(id, None)).await.unwrap();
    let coverage = page.coverage.unwrap();
    assert_eq!(
        (coverage.total, coverage.planned, coverage.blocked),
        (2, 0, 2)
    );
    assert_eq!(
        page.items[0].planning_state,
        Some(ManifestPlanningState::Blocked)
    );
    assert_eq!(
        page.items[0].block_reason.as_deref(),
        Some("knowledge_release_has_no_public_sources")
    );
    assert!(page.items[0].document_revision_id.is_none());
}

#[tokio::test]
async fn scope_revision_and_cursor_cannot_cross_snapshots() {
    let (scope, ops, id) = planned_fixture(KnowledgePurpose::Public).await;
    let cursor = ops
        .manifest_read(&scope, read(id, None))
        .await
        .unwrap()
        .next_cursor
        .unwrap();
    let wrong_scope = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    let denied = ops
        .manifest_read(&wrong_scope, read(id, None))
        .await
        .unwrap_err();
    assert_eq!(denied.code, HostOpErrorCode::NotFound);

    let mut stale = read(id, None);
    stale.revision = Some(2);
    assert_eq!(
        ops.manifest_read(&scope, stale).await.unwrap_err().code,
        HostOpErrorCode::InvalidRequest
    );
    let mut tampered = cursor.clone();
    tampered.push('0');
    for bad in [tampered, "v1.999.fake".into(), "v1.1".into()] {
        assert_eq!(
            ops.manifest_read(&scope, read(id, Some(bad)))
                .await
                .unwrap_err()
                .code,
            HostOpErrorCode::InvalidRequest
        );
    }
    let (other_scope, other_ops, other_id) = planned_fixture(KnowledgePurpose::Public).await;
    assert_eq!(
        other_ops
            .manifest_read(&other_scope, read(other_id, Some(cursor)))
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::InvalidRequest
    );
}

#[tokio::test]
async fn missing_binding_and_unimplemented_distribution_are_explicit_failures() {
    let (scope, ops, id) = planned_fixture(KnowledgePurpose::Public).await;
    let mut unbound = read(id, None);
    unbound.manifest_id = None;
    assert_eq!(
        ops.manifest_read(&scope, unbound).await.unwrap_err().code,
        HostOpErrorCode::InvalidRequest
    );
    let missing = read(Uuid::new_v4(), None);
    assert_eq!(
        ops.manifest_read(&scope, missing).await.unwrap_err().code,
        HostOpErrorCode::NotFound
    );
    let mut distribution = read(id, None);
    distribution.kind = ManifestKind::Distribution;
    assert_eq!(
        ops.manifest_read(&scope, distribution)
            .await
            .unwrap_err()
            .code,
        HostOpErrorCode::CapabilityMissing
    );
}
