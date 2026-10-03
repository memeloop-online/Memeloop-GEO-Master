use chrono::{Duration, Utc};
use geo_domain::{
    ChunkLocator, ContentAttemptOutcome, ContentBlock, ContentBlockKind, ContentBrief,
    ContentEvidence, ContentItemStatus, ContentRepository, ContentStep, DocumentManifest,
    DocumentManifestCoverage, DocumentManifestItem, DocumentManifestItemState,
    DocumentManifestState, ErrorCode, EvidenceRef, MemoryContentRepository, StructuredDocument,
    TenantScope,
};
use uuid::Uuid;

fn fixture() -> (TenantScope, DocumentManifest, Uuid, Uuid) {
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let manifest_id = Uuid::new_v4();
    let source = Uuid::new_v4();
    let release = Uuid::new_v4();
    let item = |key: &str, state, reason: Option<&str>| DocumentManifestItem {
        document_manifest_item_id: Uuid::new_v4(),
        manifest_id,
        knowledge_release_id: release,
        document_key: key.into(),
        content_type: "faq".into(),
        product_id: None,
        market: "global".into(),
        language: "en".into(),
        state,
        block_reason: reason.map(str::to_owned),
        dependency_hash: format!("dependency:{key}"),
        source_version_refs: vec![source],
    };
    let items = vec![
        item("a", DocumentManifestItemState::Planned, None),
        item(
            "b",
            DocumentManifestItemState::Blocked,
            Some("fact_conflict"),
        ),
        item("c", DocumentManifestItemState::Deferred, Some("budget")),
        item("d", DocumentManifestItemState::NotApplicable, Some("scope")),
    ];
    let manifest = DocumentManifest {
        manifest_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope.project_id.unwrap(),
        revision: 2,
        knowledge_release_id: release,
        planner_version: "test".into(),
        state: DocumentManifestState::Ready,
        sealed: true,
        expected_count: Some(items.len() as i64),
        scope_hash: "frozen-scope".into(),
        items,
        coverage: DocumentManifestCoverage {
            total: 4,
            planned: 1,
            blocked: 1,
            deferred: 1,
            not_applicable: 1,
        },
    };
    (scope, manifest, source, Uuid::new_v4())
}

#[tokio::test]
async fn transient_failure_releases_only_current_fence_and_preserves_completed_steps() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let item_id = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, cycle, manifest, "retry-policy")
        .await
        .unwrap();
    let prepare = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "first",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let released = repo.release_step(&scope, &prepare).await.unwrap();
    assert_eq!(released.status, ContentItemStatus::Pending);
    assert_eq!(
        released.attempts[0].outcome,
        ContentAttemptOutcome::Released
    );
    let retry = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "second",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        repo.release_step(&scope, &prepare).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let reference = evidence(source);
    repo.complete_prepare(
        &scope,
        &retry,
        ContentBrief {
            brief_id: Uuid::new_v4(),
            title: "Retry brief".into(),
            objective: "Use evidence".into(),
            evidence: vec![reference.clone()],
            quotes: vec![],
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let generate = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "first",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let prepared = repo.release_step(&scope, &generate).await.unwrap();
    assert_eq!(prepared.status, ContentItemStatus::Prepared);
    let retry_generate = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "second",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let draft = repo
        .complete_generate(
            &scope,
            &retry_generate,
            document(reference.chunk_id.unwrap()),
        )
        .await
        .unwrap();
    let check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "first",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let drafted = repo.release_step(&scope, &check).await.unwrap();
    assert_eq!(drafted.status, ContentItemStatus::Drafted);
    assert_eq!(drafted.current_revision_id, Some(draft.revision_id));
    let retry_check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "second",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let ready = repo
        .complete_check(&scope, &retry_check, vec![])
        .await
        .unwrap();
    assert_eq!(ready.status, ContentItemStatus::Ready);
    assert_eq!(
        repo.list_revisions(&scope, draft.asset_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        ready
            .attempts
            .iter()
            .filter(|a| a.outcome == ContentAttemptOutcome::Released)
            .count(),
        3
    );
    assert_eq!(
        repo.release_step(&scope, &retry_check)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn closed_content_can_be_edited_without_rewriting_earlier_handoff() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let item_id = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, cycle, manifest, "edit-policy")
        .await
        .unwrap();
    let reference = evidence(source);
    let prepare = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    repo.complete_prepare(
        &scope,
        &prepare,
        ContentBrief {
            brief_id: Uuid::new_v4(),
            title: "Brief".into(),
            objective: "Cited answer".into(),
            evidence: vec![reference.clone()],
            quotes: vec![],
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let generate = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let initial = repo
        .complete_generate(&scope, &generate, document(reference.chunk_id.unwrap()))
        .await
        .unwrap();
    let check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    repo.complete_check(&scope, &check, vec![]).await.unwrap();
    let first = repo.close(&scope, execution.execution_id).await.unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(first.items[0].revision_id, Some(initial.revision_id));
    assert_eq!(
        repo.edit(
            &scope,
            initial.asset_id,
            Uuid::new_v4(),
            document(reference.chunk_id.unwrap())
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repo.get_handoff(&scope, execution.execution_id)
            .await
            .unwrap(),
        Some(first.clone())
    );
    let edited = repo
        .edit(
            &scope,
            initial.asset_id,
            initial.revision_id,
            document(reference.chunk_id.unwrap()),
        )
        .await
        .unwrap();
    assert!(
        repo.get_handoff(&scope, execution.execution_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.list_handoffs(&scope, execution.execution_id)
            .await
            .unwrap(),
        vec![first.clone()]
    );
    assert_eq!(
        repo.close(&scope, execution.execution_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    repo.complete_check(&scope, &check, vec![]).await.unwrap();
    let second = repo.close(&scope, execution.execution_id).await.unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(second.supersedes_handoff_id, Some(first.handoff_id));
    assert_eq!(second.items[0].revision_id, Some(edited.revision_id));
    assert_eq!(
        repo.list_handoffs(&scope, execution.execution_id)
            .await
            .unwrap(),
        vec![first.clone(), second.clone()]
    );
    assert_eq!(
        repo.get_handoff(&scope, execution.execution_id)
            .await
            .unwrap(),
        Some(second)
    );
    assert_eq!(first.items[0].revision_id, Some(initial.revision_id));
    assert_eq!(
        repo.list_revisions(&scope, initial.asset_id)
            .await
            .unwrap()
            .len(),
        2
    );
}
fn evidence(source: Uuid) -> EvidenceRef {
    EvidenceRef {
        source_version_id: source,
        chunk_id: Some(Uuid::new_v4()),
        locator: ChunkLocator::Manual {},
    }
}
fn document(citation: Uuid) -> StructuredDocument {
    StructuredDocument {
        title: "Cited answer".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "The product has a documented feature.".into(),
            citation_ids: vec![citation],
            items: vec![],
        }],
    }
}
#[tokio::test]
async fn replay_scope_fencing_cancel_and_denominator() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let planning = manifest.clone();
    let execution = repo
        .start(&scope, cycle, manifest.clone(), "policy-v1")
        .await
        .unwrap();
    assert_eq!(
        repo.start(&scope, cycle, manifest, "policy-v1")
            .await
            .unwrap(),
        execution
    );
    assert_eq!(execution.expected_count, 4);
    let wrong = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert!(
        repo.get_execution(&wrong, execution.execution_id)
            .await
            .unwrap()
            .is_none()
    );
    let item_id = planning.items[0].document_manifest_item_id;
    let lease = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "worker-a",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        repo.claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "worker-b",
            Utc::now(),
            60
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repo.close(&scope, execution.execution_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let quote = evidence(source);
    let brief = ContentBrief {
        brief_id: Uuid::new_v4(),
        title: "FAQ".into(),
        objective: "Answer".into(),
        evidence: vec![quote.clone()],
        quotes: vec![ContentEvidence {
            reference: quote.clone(),
            exact_quote: "Exact source excerpt".into(),
        }],
        created_at: Utc::now(),
    };
    repo.complete_prepare(&scope, &lease, brief).await.unwrap();
    assert_eq!(
        repo.complete_prepare(
            &scope,
            &lease,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "x".into(),
                objective: "x".into(),
                evidence: vec![quote.clone()],
                quotes: vec![],
                created_at: Utc::now()
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let generate = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "worker-a",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let revision = repo
        .complete_generate(&scope, &generate, document(quote.chunk_id.unwrap()))
        .await
        .unwrap();
    assert_eq!(
        repo.complete_generate(&scope, &generate, document(quote.chunk_id.unwrap()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repo.claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "worker-a",
            Utc::now(),
            60
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "worker-a",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let ready = repo.complete_check(&scope, &check, vec![]).await.unwrap();
    assert_eq!(ready.status, ContentItemStatus::Ready);
    assert_eq!(ready.ready_revision_id, Some(revision.revision_id));
    let handoff = repo.close(&scope, execution.execution_id).await.unwrap();
    assert_eq!(handoff.coverage.total, 4);
    assert_eq!(
        (
            handoff.coverage.ready,
            handoff.coverage.blocked,
            handoff.coverage.deferred,
            handoff.coverage.not_applicable
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(
        repo.close(&scope, execution.execution_id).await.unwrap(),
        handoff
    );
    assert_eq!(
        repo.get_handoff(&scope, execution.execution_id)
            .await
            .unwrap(),
        Some(handoff)
    );
    assert_eq!(planning.items[0].state, DocumentManifestItemState::Planned);
    assert_eq!(
        repo.cancel(&scope, execution.execution_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}
#[tokio::test]
async fn expired_owner_edit_invalidation_and_cancel_prevent_late_commit() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let item_id = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, cycle, manifest, "policy-v1")
        .await
        .unwrap();
    let expired = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "old",
            Utc::now() - Duration::seconds(90),
            30,
        )
        .await
        .unwrap();
    let active = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Prepare,
            "new",
            Utc::now(),
            30,
        )
        .await
        .unwrap();
    let reference = evidence(source);
    let brief = ContentBrief {
        brief_id: Uuid::new_v4(),
        title: "Brief".into(),
        objective: "Answer".into(),
        evidence: vec![reference.clone()],
        quotes: vec![],
        created_at: Utc::now(),
    };
    assert_eq!(
        repo.complete_prepare(&scope, &expired, brief.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    repo.complete_prepare(&scope, &active, brief).await.unwrap();
    let generate = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Generate,
            "new",
            Utc::now(),
            30,
        )
        .await
        .unwrap();
    let first = repo
        .complete_generate(&scope, &generate, document(reference.chunk_id.unwrap()))
        .await
        .unwrap();
    let check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "new",
            Utc::now(),
            30,
        )
        .await
        .unwrap();
    repo.complete_check(&scope, &check, vec![]).await.unwrap();
    let second = repo
        .edit(
            &scope,
            first.asset_id,
            first.revision_id,
            document(reference.chunk_id.unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(second.base_revision_id, Some(first.revision_id));
    assert_eq!(
        repo.get_item(&scope, execution.execution_id, item_id)
            .await
            .unwrap()
            .unwrap()
            .ready_revision_id,
        None
    );
    assert_eq!(
        repo.edit(
            &scope,
            first.asset_id,
            first.revision_id,
            document(reference.chunk_id.unwrap())
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let pending_check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "new",
            Utc::now(),
            30,
        )
        .await
        .unwrap();
    repo.cancel(&scope, execution.execution_id).await.unwrap();
    assert_eq!(
        repo.complete_check(&scope, &pending_check, vec![])
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repo.get_item(&scope, execution.execution_id, item_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ContentItemStatus::Cancelled
    );
    let cancelled = repo
        .get_item(&scope, execution.execution_id, item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cancelled.attempts.last().unwrap().outcome,
        ContentAttemptOutcome::Cancelled
    );
    assert_eq!(
        repo.edit(
            &scope,
            first.asset_id,
            second.revision_id,
            document(reference.chunk_id.unwrap())
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
}
