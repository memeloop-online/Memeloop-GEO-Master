//! Run with GEO_TEST_DATABASE_URL against a disposable database.
use chrono::Utc;
use geo_domain::{
    ContentBlock, ContentBlockKind, ContentBrief, ContentEvidence, ContentFinding,
    ContentItemStatus, ContentRepository, ContentStep, DistributionRepository,
    DocumentManifestPlanRequest, ErrorCode, EvidenceRef, FreezeDistribution, ImportItem,
    InitialSource, InitialSourceKind, InitialSourceVisibility, KnowledgePurpose,
    KnowledgeRepository, PlatformPlacement, ProjectCreate, ProjectRepository, ProjectSettings,
    ProjectStartCommand, SourceKind, StructuredDocument, TenantScope, hash_idempotency_key,
    settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentRepository, PgDistributionRepository, PgKnowledgeRepository,
    PgProjectRepository,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn content_fanout_replay_fence_and_immutable_outputs() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("test database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,$3)")
        .bind(operator)
        .bind(format!("content-{operator}"))
        .bind("Content test")
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,$4)",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("content-{tenant}"))
    .bind("Content test")
    .execute(database.pool())
    .await
    .unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::from_database(&database);
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("content-{tenant}")),
                display_name: "Content execution".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "global".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Url,
                        value: "https://example.org/source".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let frozen = project.settings.clone().validate_start().unwrap();
    let frozen_hash = settings_hash(&frozen).unwrap();
    let accepted = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("content-test"),
                request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
                settings_hash: frozen_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(
        tenant_scope.operator_id,
        tenant_scope.tenant_id,
        Some(project.id),
    );
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: format!("source-{tenant}"),
                kind: SourceKind::Text,
                name: "Public source".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Documented product capability.".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap();
    let release = imported.items[0]
        .release
        .as_ref()
        .unwrap()
        .knowledge_release_id;
    let source = imported.items[0].source.as_ref().unwrap().source_id;
    let detail = knowledge
        .get_source_detail(&scope, source)
        .await
        .unwrap()
        .unwrap();
    let chunk = &detail.chunks[0];
    let reference = EvidenceRef {
        source_version_id: chunk.source_version_id,
        chunk_id: Some(chunk.chunk_id),
        locator: chunk.locator.clone(),
    };
    let mut document_scope = frozen.document_scope.clone();
    document_scope.markets = frozen.effective_markets();
    document_scope.languages = frozen.effective_languages();
    let manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: accepted.document_manifest.manifest_id,
                knowledge_release_id: release,
            },
            document_scope,
        )
        .await
        .unwrap();
    let repository = PgContentRepository::from_database(&database);
    let bootstrap_candidates = projects
        .scan_pending_content_cycles_after(None, Utc::now(), 100)
        .await
        .unwrap();
    assert!(
        bootstrap_candidates
            .iter()
            .any(|candidate| candidate.cycle_id == accepted.cycle_id)
    );
    let bootstrap_time = Utc::now();
    let replica = PgProjectRepository::from_database(&database);
    let (claimed, duplicate) = tokio::join!(
        projects.try_claim_content_bootstrap(
            &scope,
            accepted.cycle_id,
            bootstrap_time,
            chrono::Duration::minutes(15)
        ),
        replica.try_claim_content_bootstrap(
            &scope,
            accepted.cycle_id,
            bootstrap_time,
            chrono::Duration::minutes(15)
        ),
    );
    let bootstrap = claimed
        .unwrap()
        .or(duplicate.unwrap())
        .expect("one bootstrap owner");
    assert!(
        projects
            .scan_pending_content_cycles_after(None, bootstrap_time, 100)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.cycle_id != accepted.cycle_id)
    );
    assert!(
        projects
            .finish_content_bootstrap(&bootstrap, bootstrap_time, Err(ErrorCode::NotReady))
            .await
            .unwrap()
    );
    assert!(
        projects
            .scan_pending_content_cycles_after(None, bootstrap_time, 100)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.cycle_id != accepted.cycle_id)
    );
    let retry_at = bootstrap_time + chrono::Duration::minutes(3);
    sqlx::query("UPDATE projects SET status='paused' WHERE project_id=$1")
        .bind(project.id.as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    assert!(
        projects
            .scan_pending_content_cycles_after(None, retry_at, 100)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.cycle_id != accepted.cycle_id)
    );
    assert!(
        projects
            .try_claim_content_bootstrap(
                &scope,
                accepted.cycle_id,
                retry_at,
                chrono::Duration::minutes(15)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repository
            .start(
                &scope,
                accepted.cycle_id,
                manifest.clone(),
                "test-policy-paused",
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    sqlx::query("UPDATE projects SET status='active' WHERE project_id=$1")
        .bind(project.id.as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    let bootstrap = projects
        .try_claim_content_bootstrap(
            &scope,
            accepted.cycle_id,
            retry_at,
            chrono::Duration::minutes(15),
        )
        .await
        .unwrap()
        .unwrap();
    let first = repository
        .start(&scope, accepted.cycle_id, manifest.clone(), "test-policy")
        .await
        .unwrap();
    assert!(
        projects
            .finish_content_bootstrap(&bootstrap, retry_at, Ok(()))
            .await
            .unwrap()
    );
    assert!(
        projects
            .scan_pending_content_cycles_after(None, retry_at, 100)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.cycle_id != accepted.cycle_id)
    );
    assert!(
        projects
            .try_claim_content_bootstrap(
                &scope,
                accepted.cycle_id,
                retry_at,
                chrono::Duration::minutes(15)
            )
            .await
            .unwrap()
            .is_none()
    );
    let restarted = PgContentRepository::from_database(&database);
    assert_eq!(
        first,
        restarted
            .start(&scope, accepted.cycle_id, manifest.clone(), "test-policy")
            .await
            .unwrap()
    );
    assert_eq!(first.expected_count, manifest.items.len() as u64);
    // Two independent repository instances contend on the durable execution
    // claim. A simulated crashed worker can be recovered after expiry, and
    // stale owners cannot renew or release the replacement owner's token.
    let tick = Utc::now();
    let page = restarted.scan_running_after(None, tick, 100).await.unwrap();
    assert!(
        page.iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    let lease = repository
        .try_claim_dispatch(
            &scope,
            first.execution_id,
            tick,
            chrono::Duration::seconds(90),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        restarted
            .try_claim_dispatch(
                &scope,
                first.execution_id,
                tick,
                chrono::Duration::seconds(90)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !restarted
            .scan_running_after(None, tick, 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    let raced = repository
        .start(
            &scope,
            accepted.cycle_id,
            manifest.clone(),
            "test-policy-race",
        )
        .await
        .unwrap();
    let (left, right) = tokio::join!(
        repository.try_claim_dispatch(
            &scope,
            raced.execution_id,
            tick,
            chrono::Duration::seconds(90)
        ),
        restarted.try_claim_dispatch(
            &scope,
            raced.execution_id,
            tick,
            chrono::Duration::seconds(90)
        ),
    );
    let wins = usize::from(left.unwrap().is_some()) + usize::from(right.unwrap().is_some());
    assert_eq!(wins, 1, "at most one replica can own a workflow");
    repository.cancel(&scope, raced.execution_id).await.unwrap();
    let takeover = tick + chrono::Duration::seconds(91);
    assert!(
        restarted
            .scan_running_after(None, takeover, 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    let replacement = restarted
        .try_claim_dispatch(
            &scope,
            first.execution_id,
            takeover,
            chrono::Duration::seconds(90),
        )
        .await
        .unwrap()
        .unwrap();
    assert_ne!(lease.token, replacement.token);
    assert!(
        !repository
            .renew_dispatch(&lease, takeover, chrono::Duration::seconds(90))
            .await
            .unwrap()
    );
    assert!(
        !repository
            .release_dispatch(&lease, takeover, chrono::Duration::zero())
            .await
            .unwrap()
    );
    sqlx::query("UPDATE projects SET status='paused' WHERE project_id=$1")
        .bind(project.id.as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    assert!(
        !repository
            .renew_dispatch(&replacement, takeover, chrono::Duration::seconds(90))
            .await
            .unwrap()
    );
    assert!(
        projects
            .try_claim_content_bootstrap(
                &scope,
                accepted.cycle_id,
                takeover,
                chrono::Duration::minutes(15)
            )
            .await
            .unwrap()
            .is_none(),
        "paused projects cannot bootstrap"
    );
    assert!(
        !restarted
            .scan_running_after(None, takeover, 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    assert!(
        repository
            .try_claim_dispatch(
                &scope,
                first.execution_id,
                takeover + chrono::Duration::seconds(91),
                chrono::Duration::seconds(90)
            )
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE projects SET status='active' WHERE project_id=$1")
        .bind(project.id.as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    assert!(
        repository
            .release_dispatch(&replacement, tick, chrono::Duration::zero())
            .await
            .unwrap()
    );
    let item = manifest.items[0].document_manifest_item_id;
    let wrong = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
    assert!(
        restarted
            .get_execution(&wrong, first.execution_id)
            .await
            .unwrap()
            .is_none()
    );
    let prepare = restarted
        .claim(
            &scope,
            first.execution_id,
            item,
            ContentStep::Prepare,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    // An abandoned provider step is not safe to re-enter until its own lease
    // expires, even if no workflow dispatch lease remains.
    assert!(
        repository
            .try_claim_dispatch(
                &scope,
                first.execution_id,
                Utc::now(),
                chrono::Duration::seconds(90)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !repository
            .scan_running_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    let step_retry: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT dispatch_retry_after FROM content_executions WHERE execution_id=$1",
    )
    .bind(first.execution_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert!(step_retry.is_some_and(|until| until > Utc::now()));
    assert_eq!(
        repository
            .claim(
                &scope,
                first.execution_id,
                item,
                ContentStep::Prepare,
                "other",
                Utc::now(),
                60
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let pending = repository.release_step(&scope, &prepare).await.unwrap();
    assert_eq!(pending.status, geo_domain::ContentItemStatus::Pending);
    let retry = restarted
        .claim(
            &scope,
            first.execution_id,
            item,
            ContentStep::Prepare,
            "retry",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .release_step(&scope, &prepare)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    restarted
        .complete_prepare(
            &scope,
            &retry,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "FAQ".into(),
                objective: "Answer with evidence".into(),
                evidence: vec![reference.clone()],
                quotes: vec![ContentEvidence {
                    reference: reference.clone(),
                    exact_quote: chunk.text.clone(),
                }],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let generated = restarted
        .claim(
            &scope,
            first.execution_id,
            item,
            ContentStep::Generate,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let prepared = repository.release_step(&scope, &generated).await.unwrap();
    assert_eq!(prepared.status, geo_domain::ContentItemStatus::Prepared);
    let generated = restarted
        .claim(
            &scope,
            first.execution_id,
            item,
            ContentStep::Generate,
            "retry",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let document = StructuredDocument {
        title: "Documented FAQ".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Documented product capability.".into(),
            citation_ids: vec![chunk.chunk_id],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    let revision = repository
        .complete_generate(&scope, &generated, document.clone())
        .await
        .unwrap();
    assert_eq!(
        restarted
            .complete_generate(&scope, &generated, document.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let check = repository
        .claim(
            &scope,
            first.execution_id,
            item,
            ContentStep::Check,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    restarted
        .complete_check(&scope, &check, vec![])
        .await
        .unwrap();
    assert_eq!(
        repository
            .list_revisions(&scope, revision.asset_id)
            .await
            .unwrap()
            .len(),
        1
    );
    let next = repository
        .edit(&scope, revision.asset_id, revision.revision_id, document)
        .await
        .unwrap();
    assert_eq!(next.revision, 2);
    assert_eq!(
        restarted
            .list_revisions(&scope, revision.asset_id)
            .await
            .unwrap()
            .len(),
        2
    );
    let check = repository
        .claim(
            &scope,
            first.execution_id,
            item,
            ContentStep::Check,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    restarted
        .complete_check(&scope, &check, vec![])
        .await
        .unwrap();
    let handoff = repository.close(&scope, first.execution_id).await.unwrap();
    let closed_retry: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT dispatch_retry_after FROM content_executions WHERE execution_id=$1",
    )
    .bind(first.execution_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert!(
        closed_retry.is_none(),
        "step-based deferral ends at content close"
    );
    let stage2_at = Utc::now();
    assert!(
        repository
            .scan_closed_after(None, stage2_at, 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    let stage2 = repository
        .try_claim_dispatch(
            &scope,
            first.execution_id,
            stage2_at,
            chrono::Duration::seconds(90),
        )
        .await
        .unwrap()
        .expect("closed stage can take same fence");
    assert!(
        repository
            .renew_dispatch(
                &stage2,
                stage2_at + chrono::Duration::seconds(15),
                chrono::Duration::seconds(90)
            )
            .await
            .unwrap(),
        "closing content must not cancel distribution preparation"
    );
    assert!(
        repository
            .release_dispatch(&stage2, stage2_at, chrono::Duration::minutes(5))
            .await
            .unwrap()
    );
    let deferred_stage2: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT dispatch_retry_after FROM content_executions WHERE execution_id=$1",
    )
    .bind(first.execution_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(
        deferred_stage2.map(|time| time.timestamp_micros()),
        Some((stage2_at + chrono::Duration::minutes(5)).timestamp_micros())
    );
    assert_eq!(handoff.coverage.total, manifest.items.len() as u64);
    assert_eq!(handoff.coverage.ready, 1);
    assert_eq!(
        repository.close(&scope, first.execution_id).await.unwrap(),
        handoff
    );
    let preserved_stage2: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT dispatch_retry_after FROM content_executions WHERE execution_id=$1",
    )
    .bind(first.execution_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(preserved_stage2, deferred_stage2);
    let post_close = restarted
        .edit(
            &scope,
            revision.asset_id,
            next.revision_id,
            StructuredDocument {
                title: "Revised FAQ".into(),
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Revised documented capability.".into(),
                    citation_ids: vec![chunk.chunk_id],
                    items: vec![],
                    rich: None,
                }],
                schema_version: None,
            },
        )
        .await
        .unwrap();
    assert!(
        repository
            .get_handoff(&scope, first.execution_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repository
            .list_handoffs(&scope, first.execution_id)
            .await
            .unwrap(),
        vec![handoff.clone()]
    );
    assert_eq!(
        repository
            .close(&scope, first.execution_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let check = repository
        .claim(
            &scope,
            first.execution_id,
            item,
            ContentStep::Check,
            "worker",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    restarted
        .complete_check(&scope, &check, vec![])
        .await
        .unwrap();
    let successor = repository.close(&scope, first.execution_id).await.unwrap();
    assert!(
        !repository
            .scan_running_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    assert!(
        repository
            .scan_closed_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id)
    );
    assert_eq!(successor.revision, 2);
    assert_eq!(successor.supersedes_handoff_id, Some(handoff.handoff_id));
    assert_eq!(successor.items[0].revision_id, Some(post_close.revision_id));
    assert_eq!(
        restarted
            .list_handoffs(&scope, first.execution_id)
            .await
            .unwrap(),
        vec![handoff.clone(), successor.clone()]
    );
    let persisted_handoffs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM content_handoffs WHERE execution_id=$1")
            .bind(first.execution_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(persisted_handoffs, 2);
    assert_eq!(
        knowledge
            .get_document_manifest(&scope, manifest.manifest_id)
            .await
            .unwrap(),
        Some(manifest.clone())
    );
    assert!(
        repository
            .get_asset(&wrong, revision.asset_id)
            .await
            .unwrap()
            .is_none()
    );
    let immutable = sqlx::query("UPDATE content_revisions SET revision=3 WHERE revision_id=$1")
        .bind(revision.revision_id)
        .execute(database.pool())
        .await;
    assert!(immutable.is_err());
    let distribution = PgDistributionRepository::from_database(&database);
    let formats: Vec<String> = manifest
        .items
        .iter()
        .map(|item| item.content_type.clone())
        .collect();
    let placements = (0..3)
        .map(|i| PlatformPlacement {
            platform_id: format!("fixture-{i}"),
            placement_slot: "primary".into(),
            capability_version: "fixture-v1".into(),
            supported_formats: formats.clone(),
            unavailable_reason: None,
            fixture: true,
        })
        .collect();
    let frozen = distribution
        .freeze(
            &scope,
            FreezeDistribution {
                cycle_id: accepted.cycle_id,
                revision: 1,
                document_manifest: manifest.clone(),
                content_execution: repository
                    .get_execution(&scope, first.execution_id)
                    .await
                    .unwrap()
                    .unwrap(),
                content_handoff: successor,
                placements,
                sealed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let partial = distribution
        .expansion_page(&scope, frozen.manifest_id, 0, 1)
        .await
        .unwrap();
    let partial = distribution
        .commit_expansion_page(&scope, frozen.manifest_id, 0, partial.rows)
        .await
        .unwrap();
    assert!(!partial.complete);
    assert!(
        repository
            .scan_closed_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id),
        "partial expansion remains recoverable"
    );
    let remaining = distribution
        .expansion_page(&scope, frozen.manifest_id, partial.expansion_cursor, 100)
        .await
        .unwrap();
    let complete = distribution
        .commit_expansion_page(&scope, frozen.manifest_id, remaining.cursor, remaining.rows)
        .await
        .unwrap();
    assert!(complete.complete);
    // A permanently unsupported first cell must not hide a pending cell on
    // a later page. Reused unknown cells never trigger another preparation.
    sqlx::query("UPDATE distribution_execution_targets SET \
        current_body=jsonb_set(jsonb_set(current_body,'{status}','\"deferred\"'::jsonb),'{reason}','\"connector_unconfigured\"'::jsonb) \
        WHERE manifest_id=$1 AND ordinal=0")
        .bind(frozen.manifest_id).execute(database.pool()).await.unwrap();
    let target = complete.expected_count - 1;
    assert!(target > 1);
    assert!(
        repository
            .scan_closed_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id),
        "later-page pending target remains discoverable"
    );
    sqlx::query(
        "UPDATE distribution_execution_targets SET \
        current_body=jsonb_set(current_body,'{status}','\"reused_unknown\"'::jsonb) \
        WHERE manifest_id=$1 AND current_body->>'status'='pending'",
    )
    .bind(frozen.manifest_id)
    .execute(database.pool())
    .await
    .unwrap();
    assert!(
        repository
            .scan_closed_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.execution_id != first.execution_id),
        "unsupported and unknown targets cannot drive blind retry"
    );
    sqlx::query("UPDATE distribution_execution_targets SET \
        current_body=jsonb_set(jsonb_set(current_body,'{status}','\"deferred\"'::jsonb),'{reason}','\"account_unassigned\"'::jsonb) \
        WHERE manifest_id=$1 AND ordinal=$2")
        .bind(frozen.manifest_id).bind(target as i64)
        .execute(database.pool()).await.unwrap();
    assert!(
        repository
            .scan_closed_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == first.execution_id),
        "later-page recheckable deferral must recover"
    );
    let cancelled = repository
        .start(
            &scope,
            accepted.cycle_id,
            manifest.clone(),
            "test-policy-cancel",
        )
        .await
        .unwrap();
    let late = repository
        .claim(
            &scope,
            cancelled.execution_id,
            item,
            ContentStep::Prepare,
            "stale",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    restarted
        .cancel(&scope, cancelled.execution_id)
        .await
        .unwrap();
    assert!(
        !repository
            .scan_running_after(None, Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.execution_id == cancelled.execution_id)
    );
    assert_eq!(
        repository
            .release_step(&scope, &late)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let rejected = repository
        .complete_prepare(
            &scope,
            &late,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Late".into(),
                objective: "Never commit".into(),
                evidence: vec![reference.clone()],
                quotes: vec![],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::Conflict);

    // Separate execution exercises a persisted factual repair across repository
    // instances without disturbing the distribution handoff above.
    let repairing = repository
        .start(&scope, accepted.cycle_id, manifest, "test-policy-repair")
        .await
        .unwrap();
    let prep = repository
        .claim(
            &scope,
            repairing.execution_id,
            item,
            ContentStep::Prepare,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let quote = ContentEvidence {
        reference: reference.clone(),
        exact_quote: chunk.text.clone(),
    };
    repository
        .complete_prepare(
            &scope,
            &prep,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Verified brief".into(),
                objective: "Cited answer".into(),
                evidence: vec![reference.clone()],
                quotes: vec![quote.clone()],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let generation = repository
        .claim(
            &scope,
            repairing.execution_id,
            item,
            ContentStep::Generate,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let repaired_document = StructuredDocument {
        title: "Verified claim".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Evidence-linked claim".into(),
            citation_ids: vec![reference.chunk_id.unwrap()],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    let original = repository
        .complete_generate(&scope, &generation, repaired_document.clone())
        .await
        .unwrap();
    assert_ne!(original.asset_id, revision.asset_id);
    assert_eq!(original.revision, 1);
    let check = repository
        .claim(
            &scope,
            repairing.execution_id,
            item,
            ContentStep::Check,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    let finding = ContentFinding {
        finding_id: Uuid::new_v4(),
        code: "unsupported_claim".into(),
        block_id: Some(repaired_document.blocks[0].block_id),
        evidence: vec![reference.clone()],
        detail: "Revise the unsupported claim".into(),
        blocking: true,
    };
    let checked = restarted
        .complete_check(&scope, &check, vec![finding.clone()])
        .await
        .unwrap();
    assert_eq!(checked.status, ContentItemStatus::NeedsRepair);
    assert_eq!(checked.automatic_repair_count, 0);
    assert_eq!(
        repository
            .list_checks(&scope, original.revision_id)
            .await
            .unwrap()[0]
            .findings,
        vec![finding]
    );
    assert!(
        repository
            .list_checks(&wrong, original.revision_id)
            .await
            .unwrap()
            .is_empty()
    );
    let repair = restarted
        .claim(
            &scope,
            repairing.execution_id,
            item,
            ContentStep::Repair,
            "owner",
            Utc::now(),
            60,
        )
        .await
        .unwrap();
    assert_eq!(repair.revision_id, Some(original.revision_id));
    let mut wrong_base = repair.clone();
    wrong_base.revision_id = Some(Uuid::new_v4());
    assert_eq!(
        repository
            .complete_repair(&scope, &wrong_base, repaired_document.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let second = repository
        .complete_repair(&scope, &repair, repaired_document)
        .await
        .unwrap();
    assert_eq!(second.base_revision_id, Some(original.revision_id));
    assert_eq!(second.asset_id, original.asset_id);
    assert_eq!(second.quotes, vec![quote]);
    assert_eq!(second.evidence, original.evidence);
    assert_eq!(
        restarted
            .complete_repair(&scope, &repair, second.document.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let recovered = restarted
        .get_item(&scope, repairing.execution_id, item)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.automatic_repair_count, 1);
    assert_eq!(recovered.status, ContentItemStatus::Drafted);
    assert_eq!(recovered.current_revision_id, Some(second.revision_id));
    let revisions = restarted
        .list_revisions(&scope, original.asset_id)
        .await
        .unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].revision_id, original.revision_id);
    assert_eq!(revisions[1].revision_id, second.revision_id);
}
