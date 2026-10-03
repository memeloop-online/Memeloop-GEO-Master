//! Run with GEO_TEST_DATABASE_URL against a disposable database.
use chrono::Utc;
use geo_domain::{
    ContentBlock, ContentBlockKind, ContentBrief, ContentEvidence, ContentRepository, ContentStep,
    DocumentManifestPlanRequest, ErrorCode, EvidenceRef, ImportItem, InitialSource,
    InitialSourceKind, InitialSourceVisibility, KnowledgePurpose, KnowledgeRepository,
    ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand, SourceKind,
    StructuredDocument, TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentRepository, PgKnowledgeRepository, PgProjectRepository,
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
    let first = repository
        .start(&scope, accepted.cycle_id, manifest.clone(), "test-policy")
        .await
        .unwrap();
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
        }],
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
    assert_eq!(handoff.coverage.total, manifest.items.len() as u64);
    assert_eq!(handoff.coverage.ready, 1);
    assert_eq!(
        repository.close(&scope, first.execution_id).await.unwrap(),
        handoff
    );
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
                }],
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
    let cancelled = repository
        .start(&scope, accepted.cycle_id, manifest, "test-policy-cancel")
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
                evidence: vec![reference],
                quotes: vec![],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::Conflict);
}
