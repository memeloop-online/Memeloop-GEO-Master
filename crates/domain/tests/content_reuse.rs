use chrono::Utc;
use geo_domain::{
    CONTENT_SEMANTIC_DESCRIPTOR_VERSION, ChunkLocator, ContentBlock, ContentBlockKind,
    ContentBrief, ContentEvidence, ContentItemStatus, ContentRepository, ContentReuseDecision,
    ContentReuseRequest, ContentSemanticDescriptor, ContentStep, DocumentManifest,
    DocumentManifestCoverage, DocumentManifestItem, DocumentManifestItemState,
    DocumentManifestState, ErrorCode, EvidenceRef, MemoryContentRepository, StructuredDocument,
    TenantScope,
};
use uuid::Uuid;

fn inputs() -> (TenantScope, DocumentManifest, ContentSemanticDescriptor) {
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let manifest_id = Uuid::new_v4();
    let release = Uuid::new_v4();
    let source = Uuid::new_v4();
    let evidence = ContentEvidence {
        reference: EvidenceRef {
            source_version_id: source,
            chunk_id: Some(Uuid::new_v4()),
            locator: ChunkLocator::Text {
                start_line: 1,
                end_line: 1,
                start_char: 0,
                end_char: 6,
            },
        },
        exact_quote: "answer".into(),
    };
    let manifest = DocumentManifest {
        manifest_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope.project_id.unwrap(),
        revision: 1,
        knowledge_release_id: release,
        planner_version: "planner-1".into(),
        state: DocumentManifestState::Ready,
        sealed: true,
        expected_count: Some(1),
        scope_hash: "scope".into(),
        items: vec![DocumentManifestItem {
            document_manifest_item_id: Uuid::new_v4(),
            manifest_id,
            knowledge_release_id: release,
            document_key: "faq".into(),
            content_type: "faq".into(),
            product_id: None,
            market: "global".into(),
            language: "en".into(),
            state: DocumentManifestItemState::Planned,
            block_reason: None,
            dependency_hash: "dependency".into(),
            source_version_refs: vec![source],
        }],
        coverage: DocumentManifestCoverage {
            total: 1,
            planned: 1,
            blocked: 0,
            deferred: 0,
            not_applicable: 0,
        },
    };
    let descriptor = ContentSemanticDescriptor {
        version: CONTENT_SEMANTIC_DESCRIPTOR_VERSION,
        scope: scope.clone(),
        document_key: "faq".into(),
        content_type: "faq".into(),
        product_id: None,
        market: "global".into(),
        language: "en".into(),
        planner_version: "planner-1".into(),
        source_version_ids: vec![source],
        evidence: vec![evidence],
        brand_name: "Example".into(),
        product_name: None,
        target_audience: None,
        objective: None,
        question_clusters: vec!["general".into()],
        brief_title: "FAQ".into(),
        brief_objective: "Answer".into(),
        generation_policy_version: "generate-1".into(),
        evidence_policy_version: "evidence-1".into(),
        check_policy_version: "check-1".into(),
        repair_policy_version: "repair-1".into(),
        output_schema_version: "schema-1".into(),
        generation_policy_revision: "deliberate-1".into(),
    };
    (scope, manifest, descriptor)
}

fn next_manifest(previous: &DocumentManifest) -> DocumentManifest {
    let mut next = previous.clone();
    next.manifest_id = Uuid::new_v4();
    next.knowledge_release_id = Uuid::new_v4();
    next.items[0].manifest_id = next.manifest_id;
    next.items[0].knowledge_release_id = next.knowledge_release_id;
    next.items[0].document_manifest_item_id = Uuid::new_v4();
    next
}

fn document(citation: Uuid, text: &str) -> StructuredDocument {
    StructuredDocument {
        title: "FAQ".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: text.into(),
            citation_ids: vec![citation],
            items: vec![],
        }],
    }
}

fn request(
    execution_id: Uuid,
    item_id: Uuid,
    descriptor: &ContentSemanticDescriptor,
) -> ContentReuseRequest {
    ContentReuseRequest {
        execution_id,
        item_id,
        descriptor: descriptor.clone(),
        owner: "test-worker".into(),
        now: Utc::now(),
        ttl_seconds: 60,
    }
}

#[tokio::test]
async fn two_cycles_bind_the_same_checked_revision_and_edit_forks_without_changing_origin() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, descriptor) = inputs();
    let origin_item_id = manifest.items[0].document_manifest_item_id;
    let origin = repo
        .start(&scope, Uuid::new_v4(), manifest.clone(), "generate-1")
        .await
        .unwrap();
    let reserved = repo
        .prepare_or_reuse(
            &scope,
            request(origin.execution_id, origin_item_id, &descriptor),
        )
        .await
        .unwrap();
    let ContentReuseDecision::Reserved { lease, .. } = reserved else {
        panic!("first cycle must reserve production");
    };
    let evidence = descriptor.evidence[0].clone();
    repo.complete_prepare(
        &scope,
        &lease,
        ContentBrief {
            brief_id: Uuid::new_v4(),
            title: descriptor.brief_title.clone(),
            objective: descriptor.brief_objective.clone(),
            evidence: vec![evidence.reference.clone()],
            quotes: vec![evidence.clone()],
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let generate = repo
        .claim(
            &scope,
            origin.execution_id,
            origin_item_id,
            ContentStep::Generate,
            "test-worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let revision = repo
        .complete_generate(
            &scope,
            &generate,
            document(evidence.reference.chunk_id.unwrap(), "Original"),
        )
        .await
        .unwrap();
    let check = repo
        .claim(
            &scope,
            origin.execution_id,
            origin_item_id,
            ContentStep::Check,
            "test-worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    repo.complete_check(&scope, &check, vec![]).await.unwrap();
    repo.close(&scope, origin.execution_id).await.unwrap();
    let next = next_manifest(&manifest);
    let next_item_id = next.items[0].document_manifest_item_id;
    let successor = repo
        .start(&scope, Uuid::new_v4(), next, "generate-1")
        .await
        .unwrap();
    let reused = repo
        .prepare_or_reuse(
            &scope,
            request(successor.execution_id, next_item_id, &descriptor),
        )
        .await
        .unwrap();
    let ContentReuseDecision::Ready(item) = reused else {
        panic!("unchanged semantic input must reuse");
    };
    assert_eq!(item.ready_revision_id, Some(revision.revision_id));
    assert_eq!(item.asset_id, Some(revision.asset_id));
    assert!(
        repo.list_assets(&scope, successor.execution_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.resolve_checked_revision(
            &scope,
            successor.execution_id,
            next_item_id,
            revision.revision_id,
        )
        .await
        .unwrap()
        .unwrap()
        .revision_id,
        revision.revision_id
    );
    let edited = repo
        .edit(
            &scope,
            revision.asset_id,
            revision.revision_id,
            document(evidence.reference.chunk_id.unwrap(), "Origin edited later"),
        )
        .await
        .unwrap();
    // New reuse of the superseded candidate is forbidden, but an existing
    // frozen binding still resolves its immutable checked origin revision.
    assert_eq!(
        repo.resolve_checked_revision(
            &scope,
            successor.execution_id,
            next_item_id,
            revision.revision_id,
        )
        .await
        .unwrap()
        .unwrap()
        .revision_id,
        revision.revision_id
    );
    let updated_check = repo
        .claim(
            &scope,
            origin.execution_id,
            origin_item_id,
            ContentStep::Check,
            "edited-origin",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    repo.complete_check(&scope, &updated_check, vec![])
        .await
        .unwrap();
    let third = next_manifest(&manifest);
    let third_id = third.items[0].document_manifest_item_id;
    let third_execution = repo
        .start(&scope, Uuid::new_v4(), third, "generate-1")
        .await
        .unwrap();
    let ContentReuseDecision::Ready(third_item) = repo
        .prepare_or_reuse(
            &scope,
            request(third_execution.execution_id, third_id, &descriptor),
        )
        .await
        .unwrap()
    else {
        panic!("independently checked edit should become current candidate");
    };
    assert_eq!(third_item.ready_revision_id, Some(edited.revision_id));
    assert_eq!(
        repo.resolve_checked_revision(
            &scope,
            successor.execution_id,
            next_item_id,
            revision.revision_id,
        )
        .await
        .unwrap()
        .unwrap()
        .revision_id,
        revision.revision_id
    );
    let fork = repo
        .fork_reused_item(
            &scope,
            successor.execution_id,
            next_item_id,
            revision.revision_id,
            document(evidence.reference.chunk_id.unwrap(), "New wording"),
        )
        .await
        .unwrap();
    assert_ne!(fork.asset_id, revision.asset_id);
    assert_eq!(fork.derived_from_revision_id, Some(revision.revision_id));
    assert_eq!(fork.base_revision_id, None);
    assert_eq!(
        repo.get_asset(&scope, revision.asset_id)
            .await
            .unwrap()
            .unwrap()
            .current_revision_id,
        edited.revision_id
    );
    assert_eq!(
        repo.get_item(&scope, successor.execution_id, next_item_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ContentItemStatus::Drafted
    );
}

#[tokio::test]
async fn reservation_busy_and_stale_input_are_fenced() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, descriptor) = inputs();
    let item_id = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, Uuid::new_v4(), manifest, "generate-1")
        .await
        .unwrap();
    let first = repo
        .prepare_or_reuse(
            &scope,
            request(execution.execution_id, item_id, &descriptor),
        )
        .await
        .unwrap();
    assert!(matches!(first, ContentReuseDecision::Reserved { .. }));
    assert!(matches!(
        repo.prepare_or_reuse(
            &scope,
            request(execution.execution_id, item_id, &descriptor)
        )
        .await
        .unwrap(),
        ContentReuseDecision::Busy(_)
    ));
    let mut changed = descriptor.clone();
    changed.generation_policy_revision.push('2');
    assert_eq!(
        repo.prepare_or_reuse(&scope, request(execution.execution_id, item_id, &changed))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let foreign = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    assert_eq!(
        repo.prepare_or_reuse(
            &foreign,
            request(execution.execution_id, item_id, &descriptor)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Forbidden
    );
}

#[tokio::test]
async fn two_executions_have_one_fenced_producer_and_expired_claim_is_taken_over() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, descriptor) = inputs();
    let first_id = manifest.items[0].document_manifest_item_id;
    let first = repo
        .start(&scope, Uuid::new_v4(), manifest.clone(), "generate-1")
        .await
        .unwrap();
    let next = next_manifest(&manifest);
    let second_id = next.items[0].document_manifest_item_id;
    let second = repo
        .start(&scope, Uuid::new_v4(), next, "generate-1")
        .await
        .unwrap();
    let mut initial_request = request(first.execution_id, first_id, &descriptor);
    initial_request.ttl_seconds = 1;
    let ContentReuseDecision::Reserved { lease, .. } = repo
        .prepare_or_reuse(&scope, initial_request)
        .await
        .unwrap()
    else {
        panic!("first producer must own fingerprint");
    };
    assert!(matches!(
        repo.prepare_or_reuse(&scope, request(second.execution_id, second_id, &descriptor))
            .await
            .unwrap(),
        ContentReuseDecision::Busy(_)
    ));
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(matches!(
        repo.prepare_or_reuse(&scope, request(second.execution_id, second_id, &descriptor))
            .await
            .unwrap(),
        ContentReuseDecision::Reserved { .. }
    ));
    assert_eq!(
        repo.get_item(&scope, first.execution_id, first_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ContentItemStatus::Blocked
    );
    assert_eq!(
        repo.complete_prepare(
            &scope,
            &lease,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: descriptor.brief_title,
                objective: descriptor.brief_objective,
                evidence: vec![descriptor.evidence[0].reference.clone()],
                quotes: descriptor.evidence,
                created_at: Utc::now(),
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn expired_prepare_retries_same_execution_without_blocking_its_own_item() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, descriptor) = inputs();
    let item_id = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, Uuid::new_v4(), manifest, "generate-1")
        .await
        .unwrap();
    let mut first = request(execution.execution_id, item_id, &descriptor);
    first.ttl_seconds = 1;
    let ContentReuseDecision::Reserved { lease: expired, .. } =
        repo.prepare_or_reuse(&scope, first).await.unwrap()
    else {
        panic!("first prepare must reserve");
    };
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let ContentReuseDecision::Reserved {
        lease: renewed,
        item,
    } = repo
        .prepare_or_reuse(
            &scope,
            request(execution.execution_id, item_id, &descriptor),
        )
        .await
        .unwrap()
    else {
        panic!("same item must reclaim expired reservation");
    };
    assert_eq!(item.status, ContentItemStatus::Pending);
    assert_ne!(expired.token, renewed.token);
}

#[tokio::test]
async fn cancelled_producer_does_not_hold_a_successor_reservation() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, descriptor) = inputs();
    let item_id = manifest.items[0].document_manifest_item_id;
    let first = repo
        .start(&scope, Uuid::new_v4(), manifest.clone(), "generate-1")
        .await
        .unwrap();
    assert!(matches!(
        repo.prepare_or_reuse(&scope, request(first.execution_id, item_id, &descriptor))
            .await
            .unwrap(),
        ContentReuseDecision::Reserved { .. }
    ));
    repo.cancel(&scope, first.execution_id).await.unwrap();
    let next = next_manifest(&manifest);
    let next_id = next.items[0].document_manifest_item_id;
    let second = repo
        .start(&scope, Uuid::new_v4(), next, "generate-1")
        .await
        .unwrap();
    assert!(matches!(
        repo.prepare_or_reuse(&scope, request(second.execution_id, next_id, &descriptor))
            .await
            .unwrap(),
        ContentReuseDecision::Reserved { .. }
    ));
}
