//! Run with GEO_TEST_DATABASE_URL against a disposable PostgreSQL database.
use chrono::{Duration, Utc};
use geo_domain::{
    ChannelJobRepository, ChannelOutcome, ChannelOutcomeStatus, ChannelPlan, ChannelTargetInput,
    ContentBlock, ContentBlockKind, ContentCoverage, ContentExecution, ContentExecutionStatus,
    ContentHandoff, ContentHandoffItem, ContentItemStatus, ContentRevision, DistributionRepository,
    DistributionTargetStatus, DocumentManifest, DocumentManifestItemState, ErrorCode,
    FreezeDistribution, InitialSource, InitialSourceKind, InitialSourceVisibility,
    PlatformPlacement, PreparedDistribution, ProjectCreate, ProjectRepository, ProjectSettings,
    ProjectStartCommand, StructuredDocument, TenantScope, hash_idempotency_key, settings_hash,
    start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgDistributionRepository, PgProjectRepository,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    scope: TenantScope,
    cycle: Uuid,
    manifest: DocumentManifest,
    execution: ContentExecution,
    handoff: ContentHandoff,
    revision: ContentRevision,
    skeleton: Uuid,
}

async fn fixture(pool: &PgPool) -> Fixture {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Fixture')")
        .bind(operator)
        .bind(format!("distribution-{operator}"))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Fixture')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("distribution-{tenant}"))
    .execute(pool)
    .await
    .unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let projects = PgProjectRepository::new(pool.clone());
    let mut settings = ProjectSettings {
        brand_name: "Fixture".into(),
        market: "global".into(),
        language: "en".into(),
        initial_sources: vec![InitialSource {
            kind: InitialSourceKind::Url,
            value: "https://example.invalid/reference".into(),
            visibility: InitialSourceVisibility::Public,
            version_ref: None,
            content_hash: None,
        }],
        ..ProjectSettings::default()
    };
    settings.document_scope.content_types = vec!["article".into(), "faq".into()];
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("distribution-{tenant}")),
                display_name: "Fixture".into(),
                settings,
            },
        )
        .await
        .unwrap();
    let frozen = project.settings.clone().validate_start().unwrap();
    let hash = settings_hash(&frozen).unwrap();
    let accepted = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("distribution-fixture"),
                request_hash: start_request_hash(project.id, project.revision, &hash),
                settings_hash: hash,
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
    let document_id = accepted.document_manifest.manifest_id;
    let release = Uuid::new_v4();
    let keys = (operator, tenant, project.id.as_uuid());
    sqlx::query(
        "INSERT INTO knowledge_releases(knowledge_release_id,operator_id,tenant_id,project_id, \
         sequence,index_build_id,pipeline_versions,content_hash,coverage) \
         VALUES($1,$2,$3,$4,1,'fixture','{}','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','{}')"
    ).bind(release).bind(keys.0).bind(keys.1).bind(keys.2).execute(pool).await.unwrap();
    sqlx::query(
        "UPDATE document_manifests SET state='ready',sealed=true,expected_count=2,scope_hash='fixture', \
         input_refs=$1 WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND manifest_id=$5"
    ).bind(serde_json::json!({"knowledge_release_id":release,"planner_version":"fixture"}))
        .bind(keys.0).bind(keys.1).bind(keys.2).bind(document_id).execute(pool).await.unwrap();
    let items = [
        (Uuid::new_v4(), "article", "a"),
        (Uuid::new_v4(), "faq", "b"),
    ];
    for (id, kind, name) in items {
        sqlx::query(
            "INSERT INTO document_manifest_items \
             (document_manifest_item_id,operator_id,tenant_id,project_id,manifest_id,knowledge_release_id, \
             document_key,content_type,market,language,state,dependency_hash) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,'global','en','planned',$9)"
        ).bind(id).bind(keys.0).bind(keys.1).bind(keys.2).bind(document_id).bind(release)
            .bind(name).bind(kind).bind(format!("dependency-{name}")).execute(pool).await.unwrap();
    }
    let manifest = geo_domain::KnowledgeRepository::get_document_manifest(
        &geo_persistence::PgKnowledgeRepository::new(pool.clone()),
        &scope,
        document_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(manifest.items.len(), 2);
    assert!(
        manifest
            .items
            .iter()
            .all(|item| item.state == DocumentManifestItemState::Planned)
    );
    let execution_id = Uuid::new_v4();
    let handoff_id = Uuid::new_v4();
    let coverage = ContentCoverage {
        total: 2,
        ready: 2,
        blocked: 0,
        deferred: 0,
        not_applicable: 0,
        cancelled: 0,
        incomplete: 0,
    };
    let execution = ContentExecution {
        execution_id,
        project_id: project.id,
        cycle_id: accepted.cycle_id,
        manifest_id: document_id,
        manifest_revision: manifest.revision,
        policy_version: "fixture".into(),
        input_hash: format!("fixture-{execution_id}"),
        status: ContentExecutionStatus::Closed,
        expected_count: 2,
        coverage: coverage.clone(),
        handoff_id: Some(handoff_id),
    };
    let document = StructuredDocument {
        title: "Fixture title".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: "Fixture source-free paragraph.".into(),
            citation_ids: vec![],
            items: vec![],
        }],
    };
    let revision = ContentRevision {
        revision_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        markdown: document.markdown(),
        document,
        evidence: vec![],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    };
    let handoff = ContentHandoff {
        handoff_id,
        execution_id,
        revision: 1,
        supersedes_handoff_id: None,
        coverage,
        items: manifest
            .items
            .iter()
            .map(|item| ContentHandoffItem {
                item_id: item.document_manifest_item_id,
                document_key: item.document_key.clone(),
                status: ContentItemStatus::Ready,
                reason: None,
                revision_id: Some(revision.revision_id),
            })
            .collect(),
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO content_executions \
         (execution_id,operator_id,tenant_id,project_id,cycle_id,manifest_id,manifest_revision, \
         policy_version,input_hash,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(execution_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(accepted.cycle_id)
    .bind(document_id)
    .bind(manifest.revision)
    .bind("fixture")
    .bind(&execution.input_hash)
    .bind(serde_json::json!({"execution":execution}))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO content_revisions \
         (revision_id,operator_id,tenant_id,project_id,execution_id,asset_id,revision,body,created_at) \
         VALUES($1,$2,$3,$4,$5,$6,1,$7,$8)"
    ).bind(revision.revision_id).bind(keys.0).bind(keys.1).bind(keys.2).bind(execution_id)
        .bind(revision.asset_id).bind(serde_json::to_value(&revision).unwrap())
        .bind(revision.created_at).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO content_handoffs \
         (handoff_id,operator_id,tenant_id,project_id,execution_id,revision,body,created_at) \
         VALUES($1,$2,$3,$4,$5,1,$6,$7)",
    )
    .bind(handoff_id)
    .bind(keys.0)
    .bind(keys.1)
    .bind(keys.2)
    .bind(execution_id)
    .bind(serde_json::to_value(&handoff).unwrap())
    .bind(handoff.created_at)
    .execute(pool)
    .await
    .unwrap();
    Fixture {
        scope,
        cycle: accepted.cycle_id,
        manifest,
        execution,
        handoff,
        revision,
        skeleton: accepted.distribution_manifest.manifest_id,
    }
}

fn freeze(fixture: &Fixture) -> FreezeDistribution {
    FreezeDistribution {
        cycle_id: fixture.cycle,
        revision: 1,
        document_manifest: fixture.manifest.clone(),
        content_execution: fixture.execution.clone(),
        content_handoff: fixture.handoff.clone(),
        placements: ["alpha", "beta", "gamma"]
            .into_iter()
            .map(|platform_id| PlatformPlacement {
                platform_id: platform_id.into(),
                placement_slot: "primary".into(),
                capability_version: "fixture".into(),
                supported_formats: vec!["article".into(), "faq".into()],
                unavailable_reason: None,
                fixture: true,
            })
            .collect(),
        sealed_at: Utc::now() - Duration::days(30),
    }
}

async fn next_cycle(pool: &PgPool, first: &Fixture) -> Fixture {
    let cycle = Uuid::new_v4();
    let document_id = Uuid::new_v4();
    let skeleton = Uuid::new_v4();
    let op = first.scope.operator_id.as_uuid();
    let tenant = first.scope.tenant_id.as_uuid();
    let project = first.scope.project_id.unwrap().as_uuid();
    sqlx::query(
        "INSERT INTO optimization_cycles \
         (cycle_id,operator_id,tenant_id,project_id,config_revision_id,state,report_timezone, \
         report_window_start_at,report_window_end_at,cutoff_at) \
         SELECT $1,operator_id,tenant_id,project_id,config_revision_id,state,report_timezone, \
         report_window_start_at+interval '7 days',report_window_end_at+interval '7 days', \
         cutoff_at+interval '7 days' FROM optimization_cycles WHERE cycle_id=$2",
    )
    .bind(cycle)
    .bind(first.cycle)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO document_manifests \
         (manifest_id,operator_id,tenant_id,project_id,cycle_id,revision,state,sealed, \
         expected_count,scope_hash,input_refs) \
         SELECT $1,operator_id,tenant_id,project_id,$2,1,state,sealed,expected_count,scope_hash,input_refs \
         FROM document_manifests WHERE manifest_id=$3"
    ).bind(document_id).bind(cycle).bind(first.manifest.manifest_id)
        .execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO distribution_manifests \
         (manifest_id,operator_id,tenant_id,project_id,cycle_id,document_manifest_id,revision, \
         state,sealed,expected_count,scope_hash,input_refs) \
         VALUES($1,$2,$3,$4,$5,$6,1,'awaiting_documents',false,NULL,'fixture','{}')",
    )
    .bind(skeleton)
    .bind(op)
    .bind(tenant)
    .bind(project)
    .bind(cycle)
    .bind(document_id)
    .execute(pool)
    .await
    .unwrap();
    for item in &first.manifest.items {
        sqlx::query(
            "INSERT INTO document_manifest_items \
             (document_manifest_item_id,operator_id,tenant_id,project_id,manifest_id,knowledge_release_id, \
             document_key,content_type,market,language,state,dependency_hash,source_version_refs) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'planned',$11,'[]')"
        ).bind(Uuid::new_v4()).bind(op).bind(tenant).bind(project).bind(document_id)
            .bind(item.knowledge_release_id).bind(&item.document_key).bind(&item.content_type)
            .bind(&item.market).bind(&item.language).bind(&item.dependency_hash)
            .execute(pool).await.unwrap();
    }
    let manifest = geo_domain::KnowledgeRepository::get_document_manifest(
        &geo_persistence::PgKnowledgeRepository::new(pool.clone()),
        &first.scope,
        document_id,
    )
    .await
    .unwrap()
    .unwrap();
    let execution_id = Uuid::new_v4();
    let handoff_id = Uuid::new_v4();
    let mut execution = first.execution.clone();
    execution.execution_id = execution_id;
    execution.cycle_id = cycle;
    execution.manifest_id = document_id;
    execution.input_hash = format!("fixture-{execution_id}");
    execution.handoff_id = Some(handoff_id);
    let mut handoff = first.handoff.clone();
    handoff.handoff_id = handoff_id;
    handoff.execution_id = execution_id;
    handoff.items = manifest
        .items
        .iter()
        .map(|item| ContentHandoffItem {
            item_id: item.document_manifest_item_id,
            document_key: item.document_key.clone(),
            status: ContentItemStatus::Ready,
            reason: None,
            revision_id: Some(first.revision.revision_id),
        })
        .collect();
    sqlx::query(
        "INSERT INTO content_executions \
         (execution_id,operator_id,tenant_id,project_id,cycle_id,manifest_id,manifest_revision, \
         policy_version,input_hash,state) VALUES($1,$2,$3,$4,$5,$6,1,'fixture',$7,$8)",
    )
    .bind(execution_id)
    .bind(op)
    .bind(tenant)
    .bind(project)
    .bind(cycle)
    .bind(document_id)
    .bind(&execution.input_hash)
    .bind(serde_json::json!({"execution":execution}))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO content_handoffs \
         (handoff_id,operator_id,tenant_id,project_id,execution_id,revision,body,created_at) \
         VALUES($1,$2,$3,$4,$5,1,$6,$7)",
    )
    .bind(handoff_id)
    .bind(op)
    .bind(tenant)
    .bind(project)
    .bind(execution_id)
    .bind(serde_json::to_value(&handoff).unwrap())
    .bind(handoff.created_at)
    .execute(pool)
    .await
    .unwrap();
    Fixture {
        scope: first.scope.clone(),
        cycle,
        manifest,
        execution,
        handoff,
        revision: first.revision.clone(),
        skeleton,
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn frozen_denominator_page_resume_and_scoped_cutoff() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").unwrap();
    let db = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let fixture = fixture(db.pool()).await;
    let repo = PgDistributionRepository::from_database(&db);
    let manifest = repo.freeze(&fixture.scope, freeze(&fixture)).await.unwrap();
    assert_ne!(manifest.manifest_id, fixture.skeleton);
    assert_eq!(manifest.expected_count, 6);
    assert!(manifest.sealed_at > Utc::now() - Duration::minutes(1));
    assert_eq!(
        repo.freeze(&fixture.scope, freeze(&fixture)).await.unwrap(),
        manifest
    );
    let mut changed = freeze(&fixture);
    changed.placements[0].capability_version = "changed-after-freeze".into();
    assert_eq!(
        repo.freeze(&fixture.scope, changed).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut changed_handoff = freeze(&fixture);
    changed_handoff.content_handoff.items[0].document_key = "mutated-handoff".into();
    assert!(repo.freeze(&fixture.scope, changed_handoff).await.is_err());
    let before_page = Utc::now();
    let page1 = repo
        .expansion_page(&fixture.scope, manifest.manifest_id, 0, 2)
        .await
        .unwrap();
    assert_eq!(page1.rows.len(), 2);
    let repo2 = PgDistributionRepository::from_database(&db);
    let (first, duplicate) = tokio::join!(
        repo.commit_expansion_page(&fixture.scope, manifest.manifest_id, 0, page1.rows.clone()),
        repo2.commit_expansion_page(&fixture.scope, manifest.manifest_id, 0, page1.rows.clone())
    );
    assert_eq!(first.unwrap().expansion_cursor, 2);
    assert_eq!(duplicate.unwrap().expansion_cursor, 2);
    let early = repo
        .cycle_inputs(&fixture.scope, fixture.cycle, before_page)
        .await
        .unwrap();
    assert_eq!(early.manifest.unwrap().expected_count, 6);
    assert!(!early.temporally_complete);
    assert!(early.targets.is_empty());
    let page2 = repo2
        .expansion_page(&fixture.scope, manifest.manifest_id, 2, 4)
        .await
        .unwrap();
    repo2
        .commit_expansion_page(&fixture.scope, manifest.manifest_id, 2, page2.rows)
        .await
        .unwrap();
    let as_of = repo
        .as_of(&fixture.scope, manifest.manifest_id, Utc::now())
        .await
        .unwrap();
    assert_eq!(as_of.targets.len(), 6);
    assert_eq!(
        repo.latest_for_cycle(&fixture.scope, fixture.cycle)
            .await
            .unwrap()
            .unwrap()
            .expansion_cursor,
        6
    );
    let other = TenantScope::new(
        fixture.scope.operator_id,
        Uuid::new_v4().into(),
        fixture.scope.project_id,
    );
    assert_eq!(
        repo.get(&other, manifest.manifest_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn immutable_handoff_revision_and_one_atomic_outbox() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").unwrap();
    let db = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let fixture = fixture(db.pool()).await;
    let repo = PgDistributionRepository::from_database(&db);
    let manifest = repo.freeze(&fixture.scope, freeze(&fixture)).await.unwrap();
    let page = repo
        .expansion_page(&fixture.scope, manifest.manifest_id, 0, 6)
        .await
        .unwrap();
    repo.commit_expansion_page(&fixture.scope, manifest.manifest_id, 0, page.rows)
        .await
        .unwrap();
    let target = repo
        .list_targets(&fixture.scope, manifest.manifest_id, None, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    let account_id = Uuid::new_v4();
    let prepared = PreparedDistribution {
        manifest_id: manifest.manifest_id,
        target_id: target.target_id,
        revision: Some(fixture.revision.clone()),
        account_id: Some(account_id),
        defer_reason: None,
    };
    let old_cutoff = Utc::now();
    let deferred = repo
        .materialize(
            &fixture.scope,
            PreparedDistribution {
                manifest_id: manifest.manifest_id,
                target_id: target.target_id,
                revision: None,
                account_id: None,
                defer_reason: Some(geo_domain::DistributionDeferralReason::SourceChanged),
            },
        )
        .await
        .unwrap();
    assert_eq!(deferred.target.status, DistributionTargetStatus::Deferred);
    assert_eq!(deferred.target.reason.as_deref(), Some("source_changed"));
    assert!(deferred.publication_commands.is_empty());
    let result = repo
        .materialize(&fixture.scope, prepared.clone())
        .await
        .unwrap();
    assert_eq!(result.target.status, DistributionTargetStatus::Ready);
    assert_eq!(result.publication_commands.len(), 1);
    let retry = repo
        .materialize(&fixture.scope, prepared.clone())
        .await
        .unwrap();
    assert_eq!(retry.target, result.target);
    assert!(retry.publication_commands.is_empty());
    let paused_after_binding = repo
        .materialize(
            &fixture.scope,
            PreparedDistribution {
                defer_reason: Some(geo_domain::DistributionDeferralReason::SourceUnavailable),
                ..prepared.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        paused_after_binding.target.status,
        DistributionTargetStatus::Deferred
    );
    assert_eq!(
        paused_after_binding.target.publication_intent_id,
        result.target.publication_intent_id
    );
    let resumed = repo
        .materialize(&fixture.scope, prepared.clone())
        .await
        .unwrap();
    assert_eq!(resumed.target.status, DistributionTargetStatus::Ready);
    assert_eq!(resumed.target.reason, None);
    assert!(resumed.publication_commands.is_empty());
    let historical = repo
        .as_of(&fixture.scope, manifest.manifest_id, old_cutoff)
        .await
        .unwrap();
    assert_eq!(
        historical.targets[0].status,
        DistributionTargetStatus::Pending
    );
    let forged = ContentRevision {
        markdown: "changed after freeze".into(),
        ..fixture.revision.clone()
    };
    assert_eq!(
        repo.materialize(
            &fixture.scope,
            PreparedDistribution {
                revision: Some(forged),
                ..prepared
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM distribution_publication_commands WHERE intent_id=$1",
    )
    .bind(result.intent.as_ref().unwrap().intent_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
    let intent_id = result.intent.unwrap().intent_id;
    assert_eq!(
        repo.record_intent_verification(
            &fixture.scope,
            intent_id,
            geo_domain::IntentVerification::Verified,
            Uuid::new_v4(),
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    let command_id: Uuid = sqlx::query_scalar(
        "SELECT command_id FROM distribution_publication_commands WHERE intent_id=$1",
    )
    .bind(intent_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE distribution_publication_commands SET status='claimed' WHERE command_id=$1",
    )
    .bind(command_id)
    .execute(db.pool())
    .await
    .unwrap();
    let attempt_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO distribution_publication_attempts \
         (attempt_id,operator_id,tenant_id,project_id,intent_id,command_id) \
         VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(attempt_id)
    .bind(fixture.scope.operator_id.as_uuid())
    .bind(fixture.scope.tenant_id.as_uuid())
    .bind(fixture.scope.project_id.unwrap().as_uuid())
    .bind(intent_id)
    .bind(command_id)
    .execute(db.pool())
    .await
    .unwrap();
    let evidence_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO distribution_intent_evidence \
         (evidence_id,operator_id,tenant_id,project_id,intent_id,result,attempt_id,fixture) \
         VALUES($1,$2,$3,$4,$5,'unknown',$6,true)",
    )
    .bind(evidence_id)
    .bind(fixture.scope.operator_id.as_uuid())
    .bind(fixture.scope.tenant_id.as_uuid())
    .bind(fixture.scope.project_id.unwrap().as_uuid())
    .bind(intent_id)
    .bind(attempt_id)
    .execute(db.pool())
    .await
    .unwrap();
    let unknown = repo
        .record_intent_verification(
            &fixture.scope,
            intent_id,
            geo_domain::IntentVerification::Unknown,
            evidence_id,
        )
        .await
        .unwrap();
    assert_eq!(unknown.verification_evidence_id, Some(evidence_id));
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn next_cycle_reuses_unknown_intent_without_second_command() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").unwrap();
    let db = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let first = fixture(db.pool()).await;
    let repo = PgDistributionRepository::from_database(&db);
    let initial = repo.freeze(&first.scope, freeze(&first)).await.unwrap();
    let initial_page = repo
        .expansion_page(&first.scope, initial.manifest_id, 0, 6)
        .await
        .unwrap();
    repo.commit_expansion_page(&first.scope, initial.manifest_id, 0, initial_page.rows)
        .await
        .unwrap();
    let initial_target = repo
        .list_targets(&first.scope, initial.manifest_id, None, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    let account_id = Uuid::new_v4();
    let first_result = repo
        .materialize(
            &first.scope,
            PreparedDistribution {
                manifest_id: initial.manifest_id,
                target_id: initial_target.target_id,
                revision: Some(first.revision.clone()),
                account_id: Some(account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(first_result.publication_commands.len(), 1);
    let successor = next_cycle(db.pool(), &first).await;
    let frozen = repo
        .freeze(&successor.scope, freeze(&successor))
        .await
        .unwrap();
    let page = repo
        .expansion_page(&successor.scope, frozen.manifest_id, 0, 6)
        .await
        .unwrap();
    repo.commit_expansion_page(&successor.scope, frozen.manifest_id, 0, page.rows)
        .await
        .unwrap();
    let target = repo
        .list_targets(&successor.scope, frozen.manifest_id, None, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    let replay = repo
        .materialize(
            &successor.scope,
            PreparedDistribution {
                manifest_id: frozen.manifest_id,
                target_id: target.target_id,
                revision: Some(successor.revision.clone()),
                account_id: Some(account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        replay.target.status,
        DistributionTargetStatus::ReusedUnknown
    );
    assert_eq!(
        replay.intent.unwrap().intent_id,
        first_result.intent.unwrap().intent_id
    );
    assert!(replay.publication_commands.is_empty());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM distribution_publication_commands \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
    )
    .bind(first.scope.operator_id.as_uuid())
    .bind(first.scope.tenant_id.as_uuid())
    .bind(first.scope.project_id.unwrap().as_uuid())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn outbox_bridge_is_atomic_replay_safe_and_coexists_with_legacy_plan() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").unwrap();
    // The global outbox scanner must not consume another test's pending work.
    let admin = PgPool::connect(&url).await.unwrap();
    let schema = format!("distribution_bridge_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    let db = Database::from_pool(pool);
    db.migrate().await.unwrap();
    let first = fixture(db.pool()).await;
    let distribution = PgDistributionRepository::from_database(&db);
    let jobs = PgChannelJobRepository::from_database(&db);
    let manifest = distribution
        .freeze(&first.scope, freeze(&first))
        .await
        .unwrap();
    let page = distribution
        .expansion_page(&first.scope, manifest.manifest_id, 0, 6)
        .await
        .unwrap();
    distribution
        .commit_expansion_page(&first.scope, manifest.manifest_id, 0, page.rows)
        .await
        .unwrap();
    let targets = distribution
        .list_targets(&first.scope, manifest.manifest_id, None, 2)
        .await
        .unwrap()
        .rows;
    let account_id = Uuid::new_v4();
    let mut commands = Vec::new();
    for target in targets {
        let output = distribution
            .materialize(
                &first.scope,
                PreparedDistribution {
                    manifest_id: manifest.manifest_id,
                    target_id: target.target_id,
                    revision: Some(first.revision.clone()),
                    account_id: Some(account_id),
                    defer_reason: None,
                },
            )
            .await
            .unwrap();
        commands.push(output.publication_commands[0].clone());
    }
    commands.sort_by_key(|command| command.command_id);
    let plan_id = Uuid::new_v4();
    let legacy = jobs
        .create_plan(
            &first.scope,
            ChannelPlan {
                plan_id,
                project_id: first.scope.project_id.unwrap(),
                cycle_id: first.cycle,
                input_hash: "independent-measurement".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        jobs.get_plan(&first.scope, first.cycle).await.unwrap(),
        Some(legacy)
    );
    assert_eq!(
        jobs.materialize_pending_commands(None, 0)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );

    // Make the later command invalid. A two-row bridge must roll back the
    // earlier insert and its marker along with the invalid dependency.
    sqlx::query(
        "UPDATE distribution_publication_commands SET payload_hash='invalid' WHERE command_id=$1",
    )
    .bind(commands[1].command_id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(jobs.materialize_pending_commands(None, 100).await.is_err());
    let (target_count, marker_count): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM channel_execution_targets WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND publication_intent_id IS NOT NULL), \
                (SELECT count(*) FROM distribution_publication_commands WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND materialized_target_id IS NOT NULL)"
    ).bind(first.scope.operator_id.as_uuid()).bind(first.scope.tenant_id.as_uuid())
        .bind(first.scope.project_id.unwrap().as_uuid()).fetch_one(db.pool()).await.unwrap();
    assert_eq!((target_count, marker_count), (0, 0));
    sqlx::query("UPDATE distribution_publication_commands SET payload_hash=$1 WHERE command_id=$2")
        .bind(&commands[1].payload_hash)
        .bind(commands[1].command_id)
        .execute(db.pool())
        .await
        .unwrap();

    let (a, b) = tokio::join!(
        jobs.materialize_pending_commands(None, 100),
        jobs.materialize_pending_commands(None, 100)
    );
    let mut created = [a.unwrap(), b.unwrap()].concat();
    created.sort_by_key(|row| row.target_id);
    assert_eq!(created.len(), 2);
    assert_eq!(
        jobs.materialize_pending_commands(None, 100)
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        jobs.materialize_pending_commands(Some(created[0].target_id), 100)
            .await
            .unwrap()
            .len(),
        0
    );
    let attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_execution_attempts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3"
    ).bind(first.scope.operator_id.as_uuid()).bind(first.scope.tenant_id.as_uuid())
        .bind(first.scope.project_id.unwrap().as_uuid()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(attempts, 0);
    for row in &created {
        let view = jobs.get_target(&row.scope, row.target_id).await.unwrap();
        assert!(view.attempts.is_empty());
        assert!(matches!(
            view.target.input,
            ChannelTargetInput::GeneratedPublish { .. }
        ));
        assert_eq!(row.scope, first.scope);
    }
    assert_eq!(
        jobs.get_plan(&first.scope, first.cycle)
            .await
            .unwrap()
            .unwrap()
            .plan_id,
        plan_id
    );
    let target_id = created[0].target_id;
    let now = Utc::now();
    let reservation = Uuid::new_v4();
    jobs.reserve_account(
        &first.scope,
        account_id,
        reservation,
        now,
        now + Duration::seconds(30),
    )
    .await
    .unwrap();
    let attempt = Uuid::new_v4();
    jobs.claim_reserved(&first.scope, target_id, attempt, reservation, now)
        .await
        .unwrap();
    let claimed: (String, String, i64) = sqlx::query_as(
        "SELECT c.status,i.verification, \
           (SELECT count(*) FROM distribution_publication_attempts a WHERE a.intent_id=i.intent_id) \
         FROM distribution_publication_commands c \
         JOIN distribution_publication_intents i ON i.intent_id=c.intent_id \
         WHERE c.command_id=$1"
    ).bind(target_id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(claimed, ("claimed".into(), "unknown".into(), 1));
    assert!(
        jobs.claim(&first.scope, created[1].target_id, Uuid::new_v4(), now)
            .await
            .is_err()
    );
    jobs.finish(
        &first.scope,
        target_id,
        attempt,
        ChannelOutcome {
            status: ChannelOutcomeStatus::Unknown,
            detail: Some("unresolved".into()),
            occurred_at: now,
            raw_answer: None,
            citations: vec![],
            public_url: None,
            screenshot_ref: None,
            connector_version: None,
            runner_evidence: vec![],
            fixture: true,
        },
        now,
    )
    .await
    .unwrap();
    let bundle = distribution
        .get_publication_bundle(
            &first.scope,
            commands
                .iter()
                .find(|command| command.command_id == target_id)
                .unwrap()
                .intent_id,
        )
        .await
        .unwrap();
    assert_eq!(
        bundle.intent.verification,
        geo_domain::IntentVerification::Unknown
    );
    assert_eq!(bundle.command.command_id, target_id);
    jobs.claim_reserved(
        &first.scope,
        created[1].target_id,
        Uuid::new_v4(),
        reservation,
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        jobs.materialize_pending_commands(None, 100)
            .await
            .unwrap()
            .len(),
        0
    );

    let successor = next_cycle(db.pool(), &first).await;
    let next_manifest = distribution
        .freeze(&successor.scope, freeze(&successor))
        .await
        .unwrap();
    let page = distribution
        .expansion_page(&successor.scope, next_manifest.manifest_id, 0, 6)
        .await
        .unwrap();
    distribution
        .commit_expansion_page(&successor.scope, next_manifest.manifest_id, 0, page.rows)
        .await
        .unwrap();
    let reused = distribution
        .list_targets(&successor.scope, next_manifest.manifest_id, None, 1)
        .await
        .unwrap()
        .rows
        .remove(0);
    let result = distribution
        .materialize(
            &successor.scope,
            PreparedDistribution {
                manifest_id: next_manifest.manifest_id,
                target_id: reused.target_id,
                revision: Some(successor.revision),
                account_id: Some(account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result.target.status,
        DistributionTargetStatus::ReusedUnknown
    );
    assert!(result.publication_commands.is_empty());
    assert!(
        jobs.materialize_pending_commands(None, 100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn report_projects_verified_readback_at_received_cutoff_for_each_reused_target() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").unwrap();
    // The global command scanner must not consume another test's outbox.
    let admin = PgPool::connect(&url).await.unwrap();
    let schema = format!("distribution_report_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    let db = Database::from_pool(pool);
    db.migrate().await.unwrap();
    let first = fixture(db.pool()).await;
    let repo = PgDistributionRepository::from_database(&db);
    let jobs = PgChannelJobRepository::from_database(&db);
    let mut frozen = freeze(&first);
    frozen.placements = vec![PlatformPlacement {
        platform_id: "zhihu".into(),
        placement_slot: "primary".into(),
        capability_version: "test-connector".into(),
        supported_formats: vec!["article".into(), "faq".into()],
        unavailable_reason: None,
        fixture: false,
    }];
    let manifest = repo.freeze(&first.scope, frozen.clone()).await.unwrap();
    let page = repo
        .expansion_page(&first.scope, manifest.manifest_id, 0, 2)
        .await
        .unwrap();
    repo.commit_expansion_page(&first.scope, manifest.manifest_id, 0, page.rows.clone())
        .await
        .unwrap();
    let account_id = Uuid::new_v4();
    let source = repo
        .materialize(
            &first.scope,
            PreparedDistribution {
                manifest_id: manifest.manifest_id,
                target_id: page.rows[0].target_id,
                revision: Some(first.revision.clone()),
                account_id: Some(account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    let source_target = source.target.clone();
    let intent_id = source.intent.unwrap().intent_id;
    let created = jobs.materialize_pending_commands(None, 10).await.unwrap();
    assert_eq!(created.len(), 1);
    let job_id = created[0].target_id;
    // Exercise the JSON nanosecond / SQL microsecond boundary deterministically.
    let now = chrono::Timelike::with_nanosecond(&Utc::now(), 123_456_789).unwrap();
    let reservation = Uuid::new_v4();
    jobs.reserve_account(
        &first.scope,
        account_id,
        reservation,
        now,
        now + Duration::seconds(30),
    )
    .await
    .unwrap();
    let attempt = Uuid::new_v4();
    jobs.claim_reserved(&first.scope, job_id, attempt, reservation, now)
        .await
        .unwrap();
    let before = repo
        .publication_results(&first.scope, std::slice::from_ref(&source_target), now)
        .await
        .unwrap();
    assert_eq!(
        before[0].status,
        geo_domain::ReportPublicationStatus::Unknown
    );
    assert!(before[0].evidence.is_empty());
    let job = jobs.get_target(&first.scope, job_id).await.unwrap().target;
    let ChannelTargetInput::GeneratedPublish { title, body, .. } = job.input else {
        panic!("generated publication expected");
    };
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let hash = hex::encode(Sha256::digest(
        format!("{}\n{}", normalize(&title), normalize(&body)).as_bytes(),
    ));
    let public_url = "https://www.zhihu.com/p/12345";
    let received = now + Duration::seconds(2);
    jobs.finish(
        &first.scope,
        job_id,
        attempt,
        ChannelOutcome {
            status: ChannelOutcomeStatus::Verified,
            detail: None,
            occurred_at: now + Duration::seconds(1),
            raw_answer: None,
            citations: vec![],
            public_url: Some(public_url.into()),
            screenshot_ref: None,
            connector_version: Some("test-connector".into()),
            runner_evidence: vec![serde_json::json!({
                "kind":"public_readback", "url":public_url,
                "content_matched":true, "owned_by_account":true,
                "expected_sha256":hash,"readback_sha256":hash
            })],
            fixture: false,
        },
        received,
    )
    .await
    .unwrap();
    let before_receipt = repo
        .publication_results(
            &first.scope,
            std::slice::from_ref(&source_target),
            now + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(
        before_receipt[0].status,
        geo_domain::ReportPublicationStatus::Unknown
    );
    let verified = repo
        .publication_results(&first.scope, std::slice::from_ref(&source_target), received)
        .await
        .unwrap();
    assert_eq!(
        verified[0].status,
        geo_domain::ReportPublicationStatus::Verified
    );
    let evidence = &verified[0].evidence[0];
    assert_eq!(evidence.kind, "public_verification");
    assert_eq!(evidence.resource_id, source_target.target_id);
    // Compare the originally persisted instants at PostgreSQL's precision;
    // JSON outcomes may retain sub-microsecond digits.
    assert_eq!(
        evidence.occurred_at,
        chrono::DateTime::from_timestamp_micros((now + Duration::seconds(1)).timestamp_micros())
    );
    assert_eq!(
        evidence.received_at,
        chrono::DateTime::from_timestamp_micros(received.timestamp_micros())
    );

    let next = next_cycle(db.pool(), &first).await;
    frozen.cycle_id = next.cycle;
    frozen.document_manifest = next.manifest.clone();
    frozen.content_execution = next.execution.clone();
    frozen.content_handoff = next.handoff.clone();
    let next_manifest = repo.freeze(&next.scope, frozen).await.unwrap();
    let page = repo
        .expansion_page(&next.scope, next_manifest.manifest_id, 0, 2)
        .await
        .unwrap();
    repo.commit_expansion_page(&next.scope, next_manifest.manifest_id, 0, page.rows.clone())
        .await
        .unwrap();
    let reused = repo
        .materialize(
            &next.scope,
            PreparedDistribution {
                manifest_id: next_manifest.manifest_id,
                target_id: page.rows[0].target_id,
                revision: Some(next.revision),
                account_id: Some(account_id),
                defer_reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reused.intent.unwrap().intent_id, intent_id);
    assert_eq!(
        reused.target.status,
        DistributionTargetStatus::ReusedVerified
    );
    assert!(reused.publication_commands.is_empty());
    let both = repo
        .publication_results(&next.scope, &[source_target, reused.target], received)
        .await
        .unwrap();
    assert_eq!(both.len(), 2);
    assert_eq!(
        both[0].status,
        geo_domain::ReportPublicationStatus::Verified
    );
    assert_eq!(
        both[1].status,
        geo_domain::ReportPublicationStatus::Verified
    );
    assert_ne!(
        both[0].evidence[0].evidence_id,
        both[1].evidence[0].evidence_id
    );
    assert_eq!(
        both[0].evidence[0].occurred_at,
        both[1].evidence[0].occurred_at
    );
}
