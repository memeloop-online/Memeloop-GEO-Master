use chrono::{Duration, Utc};
use geo_domain::{
    ChunkLocator, ContentAttemptOutcome, ContentBlock, ContentBlockKind, ContentBrief,
    ContentEvidence, ContentFinding, ContentItemStatus, ContentRepository, ContentStep,
    DocumentManifest, DocumentManifestCoverage, DocumentManifestItem, DocumentManifestItemState,
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
            rich: None,
        }],
        schema_version: None,
    }
}

#[tokio::test]
async fn generation_policies_have_independent_asset_revision_chains() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let item_id = manifest.items[0].document_manifest_item_id;
    let reference = evidence(source);
    let mut generated = Vec::new();
    for policy in ["policy-a", "policy-b"] {
        let execution = repo
            .start(&scope, cycle, manifest.clone(), policy)
            .await
            .unwrap();
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
                objective: "Evidence-based answer".into(),
                evidence: vec![reference.clone()],
                quotes: vec![],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
        let lease = repo
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
        let revision = repo
            .complete_generate(&scope, &lease, document(reference.chunk_id.unwrap()))
            .await
            .unwrap();
        let replay = repo
            .start(&scope, cycle, manifest.clone(), policy)
            .await
            .unwrap();
        assert_eq!(replay.execution_id, execution.execution_id);
        assert_eq!(
            repo.get_item(&scope, replay.execution_id, item_id)
                .await
                .unwrap()
                .unwrap()
                .asset_id,
            Some(revision.asset_id)
        );
        generated.push(revision);
    }
    assert_ne!(generated[0].asset_id, generated[1].asset_id);
    for revision in generated {
        assert_eq!(
            repo.list_revisions(&scope, revision.asset_id)
                .await
                .unwrap(),
            vec![revision]
        );
    }
}
fn blocking_finding(block: Uuid, reference: &EvidenceRef) -> ContentFinding {
    ContentFinding {
        finding_id: Uuid::new_v4(),
        code: "unsubstantiated_claim".into(),
        block_id: Some(block),
        evidence: vec![reference.clone()],
        detail: "Revise claim against quoted source".into(),
        blocking: true,
    }
}
#[tokio::test]
async fn rich_checks_and_repairs_require_versioned_owners_and_preserve_structure() {
    use geo_domain::{
        RICH_CHECK_POLICY_VERSION, RICH_GENERATION_POLICY_VERSION, RICH_REPAIR_POLICY_VERSION,
    };
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let item_id = manifest.items[0].document_manifest_item_id;
    let reference = evidence(source);
    let execution = repo
        .start(&scope, cycle, manifest, RICH_GENERATION_POLICY_VERSION)
        .await
        .unwrap();
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
            title: "Rich brief".into(),
            objective: "Evidence-based".into(),
            evidence: vec![reference.clone()],
            quotes: vec![],
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let lease = repo
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
    let mut original = document(reference.chunk_id.unwrap());
    original.schema_version = Some(2);
    original.blocks[0].kind = ContentBlockKind::Rich;
    original.blocks[0].text.clear();
    original.blocks[0].rich = Some(geo_domain::RichContent {
        version: 1,
        node: geo_domain::RichNode::Paragraph {
            content: vec![geo_domain::RichNode::Text {
                text: "Original".into(),
                marks: vec![geo_domain::RichMark::Bold],
            }],
        },
    });
    let unchanged = geo_domain::ContentBlock {
        block_id: Uuid::new_v4(),
        kind: ContentBlockKind::Paragraph,
        text: "Unaffected claim".into(),
        citation_ids: vec![reference.chunk_id.unwrap()],
        items: vec![],
        rich: None,
    };
    original.blocks.push(unchanged.clone());
    let first = repo
        .complete_generate(&scope, &lease, original.clone())
        .await
        .unwrap();
    assert_eq!(
        repo.claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            "worker",
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
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let finding = blocking_finding(first.document.blocks[0].block_id, &reference);
    repo.complete_check(&scope, &check, vec![finding])
        .await
        .unwrap();
    assert_eq!(
        repo.claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Repair,
            "worker",
            Utc::now(),
            60
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let repair = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Repair,
            RICH_REPAIR_POLICY_VERSION,
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let mut downgraded = document(reference.chunk_id.unwrap());
    downgraded.blocks[0].block_id = first.document.blocks[0].block_id;
    assert_eq!(
        repo.complete_repair(&scope, &repair, downgraded)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let mut stripped = original.clone();
    if let Some(geo_domain::RichContent {
        node: geo_domain::RichNode::Paragraph { content },
        ..
    }) = &mut stripped.blocks[0].rich
    {
        *content = vec![geo_domain::RichNode::Text {
            text: "Changed".into(),
            marks: vec![],
        }];
    }
    assert_eq!(
        repo.complete_repair(&scope, &repair, stripped)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let mut corrected = original;
    if let Some(geo_domain::RichContent {
        node: geo_domain::RichNode::Paragraph { content },
        ..
    }) = &mut corrected.blocks[0].rich
    {
        *content = vec![geo_domain::RichNode::Text {
            text: "Corrected".into(),
            marks: vec![geo_domain::RichMark::Bold],
        }];
    }
    let next = repo
        .complete_repair(&scope, &repair, corrected)
        .await
        .unwrap();
    assert_eq!(next.base_revision_id, Some(first.revision_id));
    assert_eq!(next.document.schema_version, Some(2));
    assert_eq!(
        next.document.blocks[0].citation_ids,
        first.document.blocks[0].citation_ids
    );
    let check = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Check,
            RICH_CHECK_POLICY_VERSION,
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    repo.complete_check(
        &scope,
        &check,
        vec![blocking_finding(
            next.document.blocks[0].block_id,
            &reference,
        )],
    )
    .await
    .unwrap();
    let repair = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Repair,
            RICH_REPAIR_POLICY_VERSION,
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let mut deleted = next.document.clone();
    deleted.blocks.remove(0);
    let last = repo
        .complete_repair(&scope, &repair, deleted)
        .await
        .unwrap();
    assert_eq!(last.document.blocks, vec![unchanged]);
}
#[tokio::test]
async fn factual_repair_is_fenced_bounded_and_preserves_immutable_evidence() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let item_id = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, cycle, manifest, "repair-policy")
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
    let quote = ContentEvidence {
        reference: reference.clone(),
        exact_quote: "Exact public source excerpt".into(),
    };
    repo.complete_prepare(
        &scope,
        &prepare,
        ContentBrief {
            brief_id: Uuid::new_v4(),
            title: "Evidence".into(),
            objective: "Cited answer".into(),
            evidence: vec![reference.clone()],
            quotes: vec![quote.clone()],
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
    let first = repo
        .complete_generate(&scope, &generate, document(reference.chunk_id.unwrap()))
        .await
        .unwrap();
    assert_eq!(first.quotes, vec![quote.clone()]);
    let mut current = first.clone();
    for count in 0..=2 {
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
        assert_eq!(check.revision_id, Some(current.revision_id));
        let finding = blocking_finding(current.document.blocks[0].block_id, &reference);
        let checked = repo
            .complete_check(&scope, &check, vec![finding.clone()])
            .await
            .unwrap();
        assert_eq!(
            repo.list_checks(&scope, current.revision_id).await.unwrap()[0].findings,
            vec![finding]
        );
        assert_eq!(checked.automatic_repair_count, count);
        if count == 2 {
            assert_eq!(checked.status, ContentItemStatus::Blocked);
            assert_eq!(
                repo.claim(
                    &scope,
                    execution.execution_id,
                    item_id,
                    ContentStep::Repair,
                    "worker",
                    Utc::now(),
                    60
                )
                .await
                .unwrap_err()
                .code,
                ErrorCode::Conflict
            );
            break;
        }
        assert_eq!(checked.status, ContentItemStatus::NeedsRepair);
        assert_eq!(
            repo.close(&scope, execution.execution_id)
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
                ContentStep::Check,
                "worker",
                Utc::now(),
                60
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        let repair = repo
            .claim(
                &scope,
                execution.execution_id,
                item_id,
                ContentStep::Repair,
                "worker",
                Utc::now(),
                60,
            )
            .await
            .unwrap();
        let mut stale = repair.clone();
        stale.revision_id = Some(Uuid::new_v4());
        assert_eq!(
            repo.complete_repair(&scope, &stale, document(reference.chunk_id.unwrap()))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        let invalid = document(Uuid::new_v4());
        assert_eq!(
            repo.complete_repair(&scope, &repair, invalid)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        let next = repo
            .complete_repair(&scope, &repair, document(reference.chunk_id.unwrap()))
            .await
            .unwrap();
        assert_eq!(next.asset_id, first.asset_id);
        assert_eq!(next.revision, current.revision + 1);
        assert_eq!(next.base_revision_id, Some(current.revision_id));
        assert_eq!(next.evidence, first.evidence);
        assert_eq!(next.quotes, first.quotes);
        assert!(next.findings.is_empty());
        assert_eq!(
            repo.complete_repair(&scope, &repair, document(reference.chunk_id.unwrap()))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        let drafted = repo
            .get_item(&scope, execution.execution_id, item_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(drafted.status, ContentItemStatus::Drafted);
        assert_eq!(drafted.automatic_repair_count, count + 1);
        assert!(drafted.ready_revision_id.is_none());
        current = next;
    }
    assert_eq!(
        repo.list_revisions(&scope, first.asset_id)
            .await
            .unwrap()
            .len(),
        3
    );
    let blocked = repo
        .get_item(&scope, execution.execution_id, item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(blocked.automatic_repair_count, 2);
    let handoff = repo.close(&scope, execution.execution_id).await.unwrap();
    assert_eq!(handoff.coverage.blocked, 2);
}

#[tokio::test]
async fn manual_edits_do_not_consume_repair_budget_and_fence_pending_checks() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, source, cycle) = fixture();
    let item_id = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, cycle, manifest, "manual-policy")
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
    let generated = repo
        .complete_generate(&scope, &generate, document(reference.chunk_id.unwrap()))
        .await
        .unwrap();
    let stale_check = repo
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
    let edited = repo
        .edit(
            &scope,
            generated.asset_id,
            generated.revision_id,
            document(reference.chunk_id.unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(
        repo.complete_check(&scope, &stale_check, vec![])
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
            .automatic_repair_count,
        0
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
    assert_eq!(check.revision_id, Some(edited.revision_id));
    let needs_repair = repo
        .complete_check(
            &scope,
            &check,
            vec![blocking_finding(
                edited.document.blocks[0].block_id,
                &reference,
            )],
        )
        .await
        .unwrap();
    assert_eq!(needs_repair.status, ContentItemStatus::NeedsRepair);
    let stale_repair = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Repair,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let manual = repo
        .edit(
            &scope,
            generated.asset_id,
            edited.revision_id,
            document(reference.chunk_id.unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(
        repo.complete_repair(&scope, &stale_repair, document(reference.chunk_id.unwrap()))
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
            .automatic_repair_count,
        0
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
    let still_needs_repair = repo
        .complete_check(
            &scope,
            &check,
            vec![blocking_finding(
                manual.document.blocks[0].block_id,
                &reference,
            )],
        )
        .await
        .unwrap();
    assert_eq!(still_needs_repair.automatic_repair_count, 0);
    let repair = repo
        .claim(
            &scope,
            execution.execution_id,
            item_id,
            ContentStep::Repair,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let repaired = repo
        .complete_repair(&scope, &repair, document(reference.chunk_id.unwrap()))
        .await
        .unwrap();
    assert_eq!(repaired.base_revision_id, Some(manual.revision_id));
    let final_check = repo
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
    let ready = repo
        .complete_check(&scope, &final_check, vec![])
        .await
        .unwrap();
    assert_eq!(ready.status, ContentItemStatus::Ready);
    assert_eq!(ready.ready_revision_id, Some(repaired.revision_id));
    assert_eq!(ready.automatic_repair_count, 1);
}
#[tokio::test]
async fn generic_blocked_results_never_enter_repair_and_old_records_default_to_zero() {
    let repo = MemoryContentRepository::new();
    let (scope, manifest, _, cycle) = fixture();
    let first = manifest.items[0].document_manifest_item_id;
    let execution = repo
        .start(&scope, cycle, manifest, "generic-failure")
        .await
        .unwrap();
    let lease = repo
        .claim(
            &scope,
            execution.execution_id,
            first,
            ContentStep::Prepare,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let failed = repo
        .fail_step(&scope, &lease, "provider unavailable")
        .await
        .unwrap();
    assert_eq!(failed.status, ContentItemStatus::Blocked);
    assert_eq!(failed.automatic_repair_count, 0);
    assert_eq!(
        repo.claim(
            &scope,
            execution.execution_id,
            first,
            ContentStep::Repair,
            "worker",
            Utc::now(),
            60
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let mut legacy = serde_json::to_value(failed).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("automatic_repair_count");
    let restored: geo_domain::ContentItem = serde_json::from_value(legacy).unwrap();
    assert_eq!(restored.automatic_repair_count, 0);
    assert_eq!(restored.status, ContentItemStatus::Blocked);
    let (scope, manifest, _, cycle) = fixture();
    let classified_item = manifest.items[0].document_manifest_item_id;
    let classified_execution = repo
        .start(&scope, cycle, manifest, "generic-classification")
        .await
        .unwrap();
    let classified = repo
        .classify(
            &scope,
            classified_execution.execution_id,
            classified_item,
            ContentItemStatus::Blocked,
            "source unavailable",
        )
        .await
        .unwrap();
    assert_eq!(classified.status, ContentItemStatus::Blocked);
    assert_eq!(classified.automatic_repair_count, 0);
    assert_eq!(
        repo.claim(
            &scope,
            classified_execution.execution_id,
            classified_item,
            ContentStep::Repair,
            "worker",
            Utc::now(),
            60
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
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
