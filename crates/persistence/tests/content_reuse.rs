//! Explicit PostgreSQL suite: GEO_TEST_DATABASE_URL must be disposable.
use chrono::Utc;
use geo_domain::{
    ContentBlock, ContentBlockKind, ContentBrief, ContentEvidence, ContentRepository,
    ContentReuseDecision, ContentReuseRequest, ContentSemanticDescriptor, ContentStep,
    DocumentManifestPlanRequest, ImportItem, InitialSource, InitialSourceKind,
    InitialSourceVisibility, KnowledgePurpose, KnowledgeRepository, ProjectCreate,
    ProjectRepository, ProjectSettings, ProjectStartCommand, SourceKind, StructuredDocument,
    TenantScope, hash_idempotency_key, settings_hash, start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentRepository, PgKnowledgeRepository, PgProjectRepository,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn scoped_semantic_reservation_restarts_and_rechecks_live_source() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Test')")
        .bind(operator)
        .bind(format!("reuse-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Test')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("reuse-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let project_repo = PgProjectRepository::from_database(&database);
    let project = project_repo
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: None,
                display_name: "Scoped semantic reuse".into(),
                settings: ProjectSettings {
                    brand_name: "Example".into(),
                    market: "global".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Public example".into(),
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
    let accepted = project_repo
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("reuse-start"),
                request_hash: start_request_hash(project.id, project.revision, &frozen_hash),
                settings_hash: frozen_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let knowledge = PgKnowledgeRepository::from_database(&database);
    let imported = knowledge
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: format!("reuse-{tenant}"),
                kind: SourceKind::Text,
                name: "Public text".into(),
                purpose: KnowledgePurpose::Public,
                text: Some("Documented public capability.".into()),
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
    let planned = manifest
        .items
        .iter()
        .find(|i| i.source_version_refs.contains(&chunk.source_version_id))
        .unwrap();
    let repo = PgContentRepository::from_database(&database);
    let execution = repo
        .start(
            &scope,
            accepted.cycle_id,
            manifest.clone(),
            "reuse-policy-v1",
        )
        .await
        .unwrap();
    let item = repo
        .list_items(&scope, execution.execution_id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.document_key == planned.document_key)
        .unwrap();
    let descriptor = ContentSemanticDescriptor {
        version: 1,
        scope: scope.clone(),
        document_key: planned.document_key.clone(),
        content_type: planned.content_type.clone(),
        product_id: planned.product_id,
        market: planned.market.clone(),
        language: planned.language.clone(),
        planner_version: manifest.planner_version.clone(),
        source_version_ids: planned.source_version_refs.clone(),
        evidence: vec![ContentEvidence {
            reference: geo_domain::EvidenceRef {
                source_version_id: chunk.source_version_id,
                chunk_id: Some(chunk.chunk_id),
                locator: chunk.locator.clone(),
            },
            exact_quote: chunk.text.clone(),
        }],
        brand_name: "Example".into(),
        product_name: None,
        target_audience: None,
        objective: None,
        question_clusters: vec![],
        brief_title: "Public capability".into(),
        brief_objective: "Describe documented capability".into(),
        generation_policy_version: "reuse-policy-v1".into(),
        evidence_policy_version: "evidence-v1".into(),
        check_policy_version: "check-v1".into(),
        repair_policy_version: "repair-v1".into(),
        output_schema_version: "document-v1".into(),
        generation_policy_revision: "revision-1".into(),
    };
    let request = ContentReuseRequest {
        execution_id: execution.execution_id,
        item_id: item.item_id,
        descriptor: descriptor.clone(),
        owner: "worker-one".into(),
        now: Utc::now(),
        ttl_seconds: 600,
    };
    let first = repo
        .prepare_or_reuse(&scope, request.clone())
        .await
        .unwrap();
    let ContentReuseDecision::Reserved { lease, .. } = first else {
        panic!("first producer must reserve");
    };
    let restarted = PgContentRepository::from_database(&database);
    assert!(matches!(
        restarted
            .prepare_or_reuse(&scope, request.clone())
            .await
            .unwrap(),
        ContentReuseDecision::Busy(_)
    ));
    let started = project_repo
        .get_start(&tenant_scope, project.id)
        .await
        .unwrap()
        .unwrap();
    let successor = project_repo
        .schedule_next_cycle(&scope, project.id, accepted.cycle_id, started.cutoff_at)
        .await
        .unwrap();
    let mut next_scope = frozen.document_scope.clone();
    next_scope.markets = frozen.effective_markets();
    next_scope.languages = frozen.effective_languages();
    let successor_manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: successor.document_manifest.as_ref().unwrap().manifest_id,
                knowledge_release_id: release,
            },
            next_scope,
        )
        .await
        .unwrap();
    let next = restarted
        .start(
            &scope,
            successor.cycle_id,
            successor_manifest,
            "reuse-policy-v1",
        )
        .await
        .unwrap();
    let successor_item = restarted
        .list_items(&scope, next.execution_id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.document_key == planned.document_key)
        .unwrap();
    let successor_request = ContentReuseRequest {
        execution_id: next.execution_id,
        item_id: successor_item.item_id,
        descriptor: descriptor.clone(),
        owner: "worker-two".into(),
        now: Utc::now(),
        ttl_seconds: 600,
    };
    assert!(
        matches!(
            restarted
                .prepare_or_reuse(&scope, successor_request.clone())
                .await
                .unwrap(),
            ContentReuseDecision::Busy(_)
        ),
        "two executions cannot both become first producer for one semantic fingerprint"
    );
    repo.complete_prepare(
        &scope,
        &lease,
        ContentBrief {
            brief_id: Uuid::new_v4(),
            title: descriptor.brief_title.clone(),
            objective: descriptor.brief_objective.clone(),
            evidence: descriptor
                .evidence
                .iter()
                .map(|e| e.reference.clone())
                .collect(),
            quotes: descriptor.evidence.clone(),
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    let generate = repo
        .claim(
            &scope,
            execution.execution_id,
            item.item_id,
            ContentStep::Generate,
            "worker-one",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let original = repo
        .complete_generate(
            &scope,
            &generate,
            StructuredDocument {
                title: "Public capability".into(),
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Documented public capability.".into(),
                    citation_ids: vec![chunk.chunk_id],
                    items: vec![],
                    rich: None,
                }],
                schema_version: None,
            },
        )
        .await
        .unwrap();
    let check = repo
        .claim(
            &scope,
            execution.execution_id,
            item.item_id,
            ContentStep::Check,
            "worker-one",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    repo.complete_check(&scope, &check, vec![]).await.unwrap();
    let reused = restarted
        .prepare_or_reuse(&scope, successor_request)
        .await
        .unwrap();
    let ContentReuseDecision::Ready(reused) = reused else {
        panic!("must reuse checked origin")
    };
    assert_eq!(reused.ready_revision_id, Some(original.revision_id));
    assert_eq!(
        reused.reuse_binding.unwrap().origin_execution_id,
        execution.execution_id
    );
    assert!(
        restarted
            .list_assets(&scope, next.execution_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        restarted
            .resolve_checked_revision(
                &scope,
                next.execution_id,
                successor_item.item_id,
                original.revision_id
            )
            .await
            .unwrap()
            .unwrap()
            .revision_id,
        original.revision_id
    );
    let edited = restarted
        .edit(
            &scope,
            original.asset_id,
            original.revision_id,
            StructuredDocument {
                title: "Edited capability".into(),
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Documented public capability.".into(),
                    citation_ids: vec![chunk.chunk_id],
                    items: vec![],
                    rich: None,
                }],
                schema_version: None,
            },
        )
        .await
        .unwrap();
    assert_ne!(edited.revision_id, original.revision_id);
    // Editing the origin withdraws it from the candidate index but cannot
    // rewrite a successor's immutable historical handoff/revision binding.
    assert_eq!(
        restarted
            .resolve_checked_revision(
                &scope,
                next.execution_id,
                successor_item.item_id,
                original.revision_id
            )
            .await
            .unwrap()
            .unwrap()
            .revision_id,
        original.revision_id
    );
    let indexed: Option<Uuid> = sqlx::query_scalar(
        "SELECT revision_id FROM content_reuse_registry WHERE project_id=$1 AND fingerprint=$2",
    )
    .bind(project.id.as_uuid())
    .bind(descriptor.fingerprint().unwrap())
    .fetch_optional(database.pool())
    .await
    .unwrap()
    .flatten();
    assert!(indexed.is_none());
    let recheck = restarted
        .claim(
            &scope,
            execution.execution_id,
            item.item_id,
            ContentStep::Check,
            "editor",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    restarted
        .complete_check(&scope, &recheck, vec![])
        .await
        .unwrap();
    let newest: Option<Uuid> = sqlx::query_scalar(
        "SELECT revision_id FROM content_reuse_registry WHERE project_id=$1 AND fingerprint=$2",
    )
    .bind(project.id.as_uuid())
    .bind(descriptor.fingerprint().unwrap())
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(newest, Some(edited.revision_id));
    let fork = restarted
        .fork_reused_item(
            &scope,
            next.execution_id,
            successor_item.item_id,
            original.revision_id,
            StructuredDocument {
                title: "Reviewed successor".into(),
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Documented public capability.".into(),
                    citation_ids: vec![chunk.chunk_id],
                    items: vec![],
                    rich: None,
                }],
                schema_version: None,
            },
        )
        .await
        .unwrap();
    assert_ne!(fork.asset_id, original.asset_id);
    assert_eq!(fork.derived_from_revision_id, Some(original.revision_id));
    let relational_lineage: Option<Uuid> = sqlx::query_scalar(
        "SELECT derived_from_revision_id FROM content_revisions WHERE revision_id=$1",
    )
    .bind(fork.revision_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(relational_lineage, Some(original.revision_id));
    assert_eq!(
        restarted
            .resolve_checked_revision(
                &scope,
                next.execution_id,
                successor_item.item_id,
                original.revision_id
            )
            .await
            .unwrap()
            .unwrap()
            .revision_id,
        original.revision_id
    );
    // An old-style worker can still finish a descriptor-less branch after
    // migration. The successful check must update the legacy sidecar in the
    // same transaction, or the fourth cycle could mint a duplicate identity.
    let third = project_repo
        .schedule_next_cycle(&scope, project.id, successor.cycle_id, successor.cutoff_at)
        .await
        .unwrap();
    let mut third_scope = frozen.document_scope.clone();
    third_scope.markets = frozen.effective_markets();
    third_scope.languages = frozen.effective_languages();
    let third_manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: third.document_manifest.as_ref().unwrap().manifest_id,
                knowledge_release_id: release,
            },
            third_scope,
        )
        .await
        .unwrap();
    let old_execution = restarted
        .start(&scope, third.cycle_id, third_manifest, "legacy-policy-v1")
        .await
        .unwrap();
    let old_item = restarted
        .list_items(&scope, old_execution.execution_id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.document_key == planned.document_key)
        .unwrap();
    let old_prepare = restarted
        .claim(
            &scope,
            old_execution.execution_id,
            old_item.item_id,
            ContentStep::Prepare,
            "legacy",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    restarted
        .complete_prepare(
            &scope,
            &old_prepare,
            ContentBrief {
                brief_id: Uuid::new_v4(),
                title: "Legacy".into(),
                objective: "Existing path".into(),
                evidence: descriptor
                    .evidence
                    .iter()
                    .map(|e| e.reference.clone())
                    .collect(),
                quotes: descriptor.evidence.clone(),
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let old_generate = restarted
        .claim(
            &scope,
            old_execution.execution_id,
            old_item.item_id,
            ContentStep::Generate,
            "legacy",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    let old_revision = restarted
        .complete_generate(
            &scope,
            &old_generate,
            StructuredDocument {
                title: "Legacy".into(),
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Documented public capability.".into(),
                    citation_ids: vec![chunk.chunk_id],
                    items: vec![],
                    rich: None,
                }],
                schema_version: None,
            },
        )
        .await
        .unwrap();
    let old_check = restarted
        .claim(
            &scope,
            old_execution.execution_id,
            old_item.item_id,
            ContentStep::Check,
            "legacy",
            Utc::now(),
            600,
        )
        .await
        .unwrap();
    restarted
        .complete_check(&scope, &old_check, vec![])
        .await
        .unwrap();
    let indexed_legacy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM content_reuse_legacy_branches \
         WHERE execution_id=$1 AND item_id=$2)",
    )
    .bind(old_execution.execution_id)
    .bind(old_item.item_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert!(indexed_legacy);
    assert_eq!(
        restarted
            .resolve_checked_revision(
                &scope,
                old_execution.execution_id,
                old_item.item_id,
                old_revision.revision_id
            )
            .await
            .unwrap()
            .unwrap()
            .revision_id,
        old_revision.revision_id
    );
    let fourth = project_repo
        .schedule_next_cycle(&scope, project.id, third.cycle_id, third.cutoff_at)
        .await
        .unwrap();
    let mut fourth_scope = frozen.document_scope.clone();
    fourth_scope.markets = frozen.effective_markets();
    fourth_scope.languages = frozen.effective_languages();
    let fourth_manifest = knowledge
        .plan_document_manifest(
            &scope,
            DocumentManifestPlanRequest {
                manifest_id: fourth.document_manifest.as_ref().unwrap().manifest_id,
                knowledge_release_id: release,
            },
            fourth_scope,
        )
        .await
        .unwrap();
    let fourth_execution = restarted
        .start(&scope, fourth.cycle_id, fourth_manifest, "legacy-policy-v1")
        .await
        .unwrap();
    let fourth_item = restarted
        .list_items(&scope, fourth_execution.execution_id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.document_key == planned.document_key)
        .unwrap();
    let mut uncertain = descriptor.clone();
    uncertain.generation_policy_version = "legacy-policy-v1".into();
    let uncertain = restarted
        .prepare_or_reuse(
            &scope,
            ContentReuseRequest {
                execution_id: fourth_execution.execution_id,
                item_id: fourth_item.item_id,
                descriptor: uncertain,
                owner: "legacy-successor".into(),
                now: Utc::now(),
                ttl_seconds: 600,
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(uncertain,ContentReuseDecision::InsufficientEvidence(i)
        if i.status==geo_domain::ContentItemStatus::Blocked
           && i.reason.as_deref()==Some("reuse_provenance_insufficient"))
    );
    let foreign = TenantScope::new(operator.into(), Uuid::new_v4().into(), Some(project.id));
    assert!(
        restarted
            .prepare_or_reuse(&foreign, request.clone())
            .await
            .is_err()
    );
    let before_revoke = restarted
        .get_item(&scope, execution.execution_id, item.item_id)
        .await
        .unwrap()
        .unwrap();
    // The edit/recheck above replaced the original producer fence. Revocation
    // must preserve that current state, not restore the first Prepare token.
    assert_ne!(recheck.token, lease.token);
    assert_eq!(before_revoke.reuse_reservation_token, Some(recheck.token));
    sqlx::query("UPDATE knowledge_sources SET purpose='internal' WHERE source_id=$1")
        .bind(source)
        .execute(database.pool())
        .await
        .unwrap();
    assert!(restarted.prepare_or_reuse(&scope, request).await.is_err());
    assert_eq!(
        restarted
            .get_item(&scope, execution.execution_id, item.item_id)
            .await
            .unwrap()
            .unwrap(),
        before_revoke
    );
}
