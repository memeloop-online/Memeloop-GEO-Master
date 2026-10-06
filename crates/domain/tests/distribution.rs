use chrono::{Duration, Utc};
use geo_domain::{
    ContentBlock, ContentBlockKind, ContentCoverage, ContentExecution, ContentExecutionStatus,
    ContentHandoff, ContentHandoffItem, ContentItemStatus, ContentRevision,
    DistributionDeferralReason, DistributionRepository, DistributionTargetStatus, DocumentManifest,
    DocumentManifestCoverage, DocumentManifestItem, DocumentManifestItemState,
    DocumentManifestState, ErrorCode, FreezeDistribution, IntentVerification,
    MemoryDistributionRepository, PlatformPlacement, PreparedDistribution, StructuredDocument,
    TenantScope,
};
use uuid::Uuid;

fn fixture(
    scope: &TenantScope,
    cycle_id: Uuid,
    revision: i32,
    ready: [bool; 2],
) -> FreezeDistribution {
    let manifest_id = Uuid::new_v4();
    let release = Uuid::new_v4();
    let items: Vec<_> = (0..2)
        .map(|i| DocumentManifestItem {
            document_manifest_item_id: Uuid::new_v4(),
            manifest_id,
            knowledge_release_id: release,
            document_key: format!("document-{i}"),
            content_type: "article".into(),
            product_id: None,
            market: "global".into(),
            language: "en".into(),
            state: DocumentManifestItemState::Planned,
            block_reason: None,
            dependency_hash: format!("dependency-{i}"),
            source_version_refs: vec![],
        })
        .collect();
    let document_manifest = DocumentManifest {
        manifest_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope.project_id.unwrap(),
        revision: 1,
        knowledge_release_id: release,
        planner_version: "test".into(),
        state: DocumentManifestState::Closed,
        sealed: true,
        expected_count: Some(2),
        scope_hash: "test".into(),
        items: items.clone(),
        coverage: DocumentManifestCoverage {
            total: 2,
            planned: 2,
            ..Default::default()
        },
    };
    let execution_id = Uuid::new_v4();
    let handoff_id = Uuid::new_v4();
    let content_execution = ContentExecution {
        execution_id,
        project_id: scope.project_id.unwrap(),
        cycle_id,
        manifest_id,
        manifest_revision: 1,
        policy_version: "test".into(),
        input_hash: format!("input-{revision}"),
        status: ContentExecutionStatus::Closed,
        expected_count: 2,
        coverage: ContentCoverage {
            total: 2,
            ready: ready.iter().filter(|&&value| value).count() as u64,
            blocked: ready.iter().filter(|&&value| !value).count() as u64,
            deferred: 0,
            not_applicable: 0,
            cancelled: 0,
            incomplete: 0,
        },
        handoff_id: Some(handoff_id),
    };
    let handoff = ContentHandoff {
        handoff_id,
        execution_id,
        revision: 1,
        supersedes_handoff_id: None,
        coverage: content_execution.coverage.clone(),
        items: items
            .iter()
            .enumerate()
            .map(|(i, item)| ContentHandoffItem {
                item_id: item.document_manifest_item_id,
                document_key: item.document_key.clone(),
                status: if ready[i] {
                    ContentItemStatus::Ready
                } else {
                    ContentItemStatus::Blocked
                },
                reason: if ready[i] {
                    None
                } else {
                    Some("fact_conflict".into())
                },
                revision_id: ready[i].then_some(Uuid::new_v4()),
            })
            .collect(),
        created_at: Utc::now(),
    };
    FreezeDistribution {
        cycle_id,
        revision,
        document_manifest,
        content_execution,
        content_handoff: handoff,
        placements: vec![
            placement("a", vec!["article"], None),
            placement("b", vec!["article"], Some("connector_unavailable")),
            placement("c", vec!["faq"], None),
        ],
        sealed_at: Utc::now() - Duration::seconds(5),
    }
}

fn placement(name: &str, formats: Vec<&str>, unavailable: Option<&str>) -> PlatformPlacement {
    PlatformPlacement {
        platform_id: name.into(),
        placement_slot: "primary".into(),
        capability_version: "v1".into(),
        supported_formats: formats.into_iter().map(str::to_owned).collect(),
        unavailable_reason: unavailable.map(str::to_owned),
        fixture: true,
    }
}

fn content_revision(id: Uuid) -> ContentRevision {
    let document = StructuredDocument {
        title: "An exact title".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "An exact paragraph.".into(),
            citation_ids: vec![],
            items: vec![],
        }],
    };
    ContentRevision {
        derived_from_revision_id: None,
        revision_id: id,
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        markdown: document.markdown(),
        document,
        evidence: vec![],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    }
}

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

async fn expand(repo: &MemoryDistributionRepository, scope: &TenantScope, id: Uuid) {
    for cursor in 0..6 {
        let page = repo.expansion_page(scope, id, cursor, 1).await.unwrap();
        repo.commit_expansion_page(scope, id, cursor, page.rows.clone())
            .await
            .unwrap();
        // Exact retry does not move the cursor twice.
        assert_eq!(
            repo.commit_expansion_page(scope, id, cursor, page.rows)
                .await
                .unwrap()
                .expansion_cursor,
            cursor + 1
        );
    }
}

#[tokio::test]
async fn all_six_cells_are_frozen_before_expansion_and_replay_is_strict() {
    let repo = MemoryDistributionRepository::new();
    let scope = scope();
    let cycle = Uuid::new_v4();
    let frozen = fixture(&scope, cycle, 1, [true, false]);
    let manifest = repo.freeze(&scope, frozen.clone()).await.unwrap();
    assert_eq!(manifest.expected_count, 6);
    assert_eq!(manifest.expansion_cursor, 0);
    assert!(!manifest.complete);
    assert_eq!(repo.freeze(&scope, frozen.clone()).await.unwrap(), manifest);
    let mut changed = frozen;
    changed.placements[0].supported_formats.clear();
    assert_eq!(
        repo.freeze(&scope, changed).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let page = repo
        .expansion_page(&scope, manifest.manifest_id, 0, 1)
        .await
        .unwrap();
    let mut wrong = page.rows.clone();
    wrong[0].reason = Some("tampering".into());
    assert_eq!(
        repo.commit_expansion_page(&scope, manifest.manifest_id, 0, wrong)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    expand(&repo, &scope, manifest.manifest_id).await;
    let snapshot = repo
        .list_targets(&scope, manifest.manifest_id, None, 6)
        .await
        .unwrap();
    let statuses: Vec<_> = snapshot.rows.iter().map(|target| target.status).collect();
    assert_eq!(
        statuses,
        vec![
            DistributionTargetStatus::Pending,
            DistributionTargetStatus::Deferred,
            DistributionTargetStatus::NotApplicable,
            DistributionTargetStatus::Blocked,
            DistributionTargetStatus::Blocked,
            DistributionTargetStatus::Blocked,
        ]
    );
    assert_eq!(snapshot.rows[3].reason.as_deref(), Some("fact_conflict"));
    assert!(
        snapshot
            .rows
            .iter()
            .all(|target| target.account_id.is_none())
    );
    let first = repo
        .list_targets(&scope, manifest.manifest_id, None, 1)
        .await
        .unwrap();
    let second = repo
        .list_targets(&scope, manifest.manifest_id, first.next_ordinal, 1)
        .await
        .unwrap();
    assert_eq!(second.rows[0].ordinal, 1);
    assert_eq!(
        repo.commit_expansion_page(&scope, manifest.manifest_id, 4, page.rows)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn materialization_binds_exact_revision_and_one_intent_and_outbox_command() {
    let repo = MemoryDistributionRepository::new();
    let scope = scope();
    let manifest = repo
        .freeze(&scope, fixture(&scope, Uuid::new_v4(), 1, [true, true]))
        .await
        .unwrap();
    expand(&repo, &scope, manifest.manifest_id).await;
    let target = repo
        .list_targets(&scope, manifest.manifest_id, None, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    let unassigned = repo
        .materialize(
            &scope,
            PreparedDistribution {
                manifest_id: manifest.manifest_id,
                target_id: target.target_id,
                revision: None,
                account_id: None,
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(unassigned.target.status, DistributionTargetStatus::Deferred);
    assert_eq!(
        unassigned.target.reason.as_deref(),
        Some("account_unassigned")
    );
    let revision = content_revision(target.content_revision_id.unwrap());
    let prepared = PreparedDistribution {
        manifest_id: manifest.manifest_id,
        target_id: target.target_id,
        revision: Some(revision.clone()),
        account_id: Some(Uuid::new_v4()),
        defer_reason: None,
    };
    let first = repo.materialize(&scope, prepared.clone()).await.unwrap();
    assert_eq!(first.publication_commands.len(), 1);
    assert_eq!(first.variant.as_ref().unwrap().markdown, revision.markdown);
    assert_eq!(first.target.status, DistributionTargetStatus::Ready);
    let repeated = repo.materialize(&scope, prepared).await.unwrap();
    assert!(repeated.publication_commands.is_empty());
    assert_eq!(repeated.intent, first.intent);
    assert_eq!(repo.publication_commands(&scope).await.len(), 1);
    assert_eq!(repeated.target.version, first.target.version);
    let as_of = repo
        .as_of(
            &scope,
            manifest.manifest_id,
            Utc::now() + Duration::seconds(2),
        )
        .await
        .unwrap();
    assert_eq!(as_of.targets.len(), 6);
    assert_eq!(
        as_of.targets[0].publication_intent_id,
        first.target.publication_intent_id
    );
    let other = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert_eq!(
        repo.get(&other, manifest.manifest_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn as_of_keeps_frozen_denominator_and_avoids_future_target_state() {
    let repo = MemoryDistributionRepository::new();
    let scope = scope();
    let cycle = Uuid::new_v4();
    let manifest = repo
        .freeze(&scope, fixture(&scope, cycle, 1, [true, true]))
        .await
        .unwrap();
    let before = Utc::now();
    let input = repo.cycle_inputs(&scope, cycle, before).await.unwrap();
    assert_eq!(input.manifest.unwrap().expected_count, 6);
    assert!(!input.temporally_complete);
    assert!(input.targets.is_empty());
    expand(&repo, &scope, manifest.manifest_id).await;
    let old = repo.cycle_inputs(&scope, cycle, before).await.unwrap();
    assert!(old.targets.is_empty());
    assert!(!old.temporally_complete);
    let recent = repo
        .cycle_inputs(&scope, cycle, Utc::now() + Duration::seconds(2))
        .await
        .unwrap();
    assert_eq!(recent.targets.len(), 6);
    assert!(recent.temporally_complete);
    let newer = repo
        .freeze(&scope, fixture(&scope, cycle, 2, [true, false]))
        .await
        .unwrap();
    assert_eq!(
        repo.latest_for_cycle(&scope, cycle)
            .await
            .unwrap()
            .unwrap()
            .manifest_id,
        newer.manifest_id
    );
    assert_eq!(
        repo.cycle_inputs(&scope, cycle, before)
            .await
            .unwrap()
            .manifest
            .unwrap()
            .manifest_id,
        manifest.manifest_id
    );
}

#[tokio::test]
async fn cross_cycle_unchanged_verified_and_unknown_intents_are_not_requeued() {
    let repo = MemoryDistributionRepository::new();
    let scope = scope();
    let account = Uuid::new_v4();
    let prior = fixture(&scope, Uuid::new_v4(), 1, [true, true]);
    let first_revision_id = prior.content_handoff.items[0].revision_id.unwrap();
    let second_revision_id = prior.content_handoff.items[1].revision_id.unwrap();
    let first_revision = content_revision(first_revision_id);
    let second_revision = content_revision(second_revision_id);
    let first = repo.freeze(&scope, prior).await.unwrap();
    expand(&repo, &scope, first.manifest_id).await;
    let first_rows = repo
        .list_targets(&scope, first.manifest_id, None, 6)
        .await
        .unwrap()
        .rows;
    let publish = |target_id, revision: ContentRevision| PreparedDistribution {
        manifest_id: first.manifest_id,
        target_id,
        revision: Some(revision),
        account_id: Some(account),
        defer_reason: None,
    };
    let verified = repo
        .materialize(
            &scope,
            publish(first_rows[0].target_id, first_revision.clone()),
        )
        .await
        .unwrap();
    let unknown = repo
        .materialize(
            &scope,
            publish(first_rows[3].target_id, second_revision.clone()),
        )
        .await
        .unwrap();
    let verified_intent = verified.intent.unwrap();
    let unknown_intent = unknown.intent.unwrap();
    repo.record_intent_verification(
        &scope,
        verified_intent.intent_id,
        IntentVerification::Verified,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    repo.record_intent_verification(
        &scope,
        unknown_intent.intent_id,
        IntentVerification::Unknown,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    let mut later = fixture(&scope, Uuid::new_v4(), 1, [true, true]);
    later.content_handoff.items[0].revision_id = Some(first_revision_id);
    later.content_handoff.items[1].revision_id = Some(second_revision_id);
    let second = repo.freeze(&scope, later).await.unwrap();
    expand(&repo, &scope, second.manifest_id).await;
    let rows = repo
        .list_targets(&scope, second.manifest_id, None, 6)
        .await
        .unwrap()
        .rows;
    let reuse = |target_id, revision: ContentRevision| PreparedDistribution {
        manifest_id: second.manifest_id,
        target_id,
        revision: Some(revision),
        account_id: Some(account),
        defer_reason: None,
    };
    let reused_verified = repo
        .materialize(&scope, reuse(rows[0].target_id, first_revision))
        .await
        .unwrap();
    let reused_unknown = repo
        .materialize(&scope, reuse(rows[3].target_id, second_revision))
        .await
        .unwrap();
    assert_eq!(
        reused_verified.target.status,
        DistributionTargetStatus::ReusedVerified
    );
    assert_eq!(
        reused_unknown.target.status,
        DistributionTargetStatus::ReusedUnknown
    );
    assert_eq!(
        reused_verified.intent.unwrap().intent_id,
        verified_intent.intent_id
    );
    assert_eq!(
        reused_unknown.intent.unwrap().intent_id,
        unknown_intent.intent_id
    );
    assert!(reused_verified.publication_commands.is_empty());
    assert!(reused_unknown.publication_commands.is_empty());
    assert_eq!(repo.publication_commands(&scope).await.len(), 2);
}

#[tokio::test]
async fn cancelled_and_not_applicable_documents_preserve_every_platform_cell() {
    let repo = MemoryDistributionRepository::new();
    let scope = scope();
    let mut input = fixture(&scope, Uuid::new_v4(), 1, [false, false]);
    input.content_handoff.items[0].status = ContentItemStatus::Cancelled;
    input.content_handoff.items[0].reason = Some("cycle_cancelled".into());
    input.content_handoff.items[1].status = ContentItemStatus::NotApplicable;
    input.content_handoff.items[1].reason = Some("outside_scope".into());
    input.content_handoff.coverage.blocked = 0;
    input.content_handoff.coverage.cancelled = 1;
    input.content_handoff.coverage.not_applicable = 1;
    input.content_execution.coverage = input.content_handoff.coverage.clone();
    let manifest = repo.freeze(&scope, input).await.unwrap();
    expand(&repo, &scope, manifest.manifest_id).await;
    let rows = repo
        .list_targets(&scope, manifest.manifest_id, None, 6)
        .await
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 6);
    assert!(
        rows[..3]
            .iter()
            .all(|row| row.status == DistributionTargetStatus::Cancelled)
    );
    assert!(
        rows[3..]
            .iter()
            .all(|row| row.status == DistributionTargetStatus::NotApplicable)
    );
}

#[tokio::test]
async fn withdrawn_source_defers_only_its_cell_without_erasing_unknown_intent() {
    let repo = MemoryDistributionRepository::new();
    let scope = scope();
    let manifest = repo
        .freeze(&scope, fixture(&scope, Uuid::new_v4(), 1, [true, true]))
        .await
        .unwrap();
    let first_page = repo
        .expansion_page(&scope, manifest.manifest_id, 0, 1)
        .await
        .unwrap();
    repo.commit_expansion_page(&scope, manifest.manifest_id, 0, first_page.rows)
        .await
        .unwrap();
    // A ready cell can progress while the remaining denominator is still paging.
    let row = repo
        .list_targets(&scope, manifest.manifest_id, None, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    let original = PreparedDistribution {
        manifest_id: manifest.manifest_id,
        target_id: row.target_id,
        revision: Some(content_revision(row.content_revision_id.unwrap())),
        account_id: Some(Uuid::new_v4()),
        defer_reason: None,
    };
    let first = repo.materialize(&scope, original.clone()).await.unwrap();
    let intent = first.intent.unwrap();
    repo.materialize(
        &scope,
        PreparedDistribution {
            manifest_id: manifest.manifest_id,
            target_id: row.target_id,
            revision: None,
            account_id: None,
            defer_reason: Some(DistributionDeferralReason::SourceUnavailable),
        },
    )
    .await
    .unwrap();
    let resumed = repo.materialize(&scope, original.clone()).await.unwrap();
    assert_eq!(resumed.target.status, DistributionTargetStatus::Ready);
    assert_eq!(resumed.target.reason, None);
    assert_eq!(resumed.target.publication_intent_id, Some(intent.intent_id));
    assert!(resumed.publication_commands.is_empty());
    repo.record_intent_verification(
        &scope,
        intent.intent_id,
        IntentVerification::Unknown,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    let held = repo
        .materialize(
            &scope,
            PreparedDistribution {
                manifest_id: manifest.manifest_id,
                target_id: row.target_id,
                revision: None,
                account_id: None,
                defer_reason: Some(DistributionDeferralReason::SourceChanged),
            },
        )
        .await
        .unwrap();
    assert_eq!(held.target.status, DistributionTargetStatus::Deferred);
    assert_eq!(held.target.reason.as_deref(), Some("source_changed"));
    assert_eq!(held.target.publication_intent_id, Some(intent.intent_id));
    assert_eq!(held.target.account_id, Some(intent.account_id));
    assert_eq!(
        held.intent.unwrap().verification,
        IntentVerification::Unknown
    );
    assert!(held.publication_commands.is_empty());
    assert_eq!(repo.publication_commands(&scope).await.len(), 1);
    assert_eq!(
        repo.get_target(&scope, manifest.manifest_id, row.target_id)
            .await
            .unwrap(),
        held.target
    );
    let resumed_unknown = repo.materialize(&scope, original).await.unwrap();
    assert_eq!(
        resumed_unknown.target.status,
        DistributionTargetStatus::ReusedUnknown
    );
    assert_eq!(resumed_unknown.target.reason, None);
    assert_eq!(
        resumed_unknown.target.publication_intent_id,
        Some(intent.intent_id)
    );
    assert!(resumed_unknown.publication_commands.is_empty());
    assert_eq!(repo.publication_commands(&scope).await.len(), 1);
}
