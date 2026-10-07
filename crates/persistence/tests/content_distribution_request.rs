//! Run against a disposable database with GEO_TEST_DATABASE_URL.
use chrono::Utc;
use geo_domain::{
    AcceptContentDistributionRequest, CHANNEL_VARIANT_POLICY, ChannelAccount, ChannelJobRepository,
    ChannelOwnerKind, ChannelStatus, ChannelVariant, ChunkLocator, ContentBlock, ContentBlockKind,
    ContentCheck, ContentDistributionRequestRepository, ContentEvidence,
    ContentRequestDeferralReason, ContentRevision, DistributionRepository, ErrorCode, EvidenceRef,
    InitialSource, InitialSourceKind, InitialSourceVisibility, IntentVerification, ProjectCreate,
    ProjectRepository, ProjectSettings, ProjectStartCommand, PublicationIntent, PublicationOrigin,
    StructuredDocument, TEXT_DISTRIBUTION_FORMAT, TenantScope, hash_idempotency_key, settings_hash,
    start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgContentDistributionRequestRepository,
    PgDistributionRepository, PgProjectRepository,
};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn accepted_requests_replay_conflict_scope_and_link_existing_intents() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,$3)")
        .bind(operator)
        .bind(format!("article-{operator}"))
        .bind("Article test")
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,$4)",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("article-{tenant}"))
    .bind("Article test")
    .execute(database.pool())
    .await
    .unwrap();
    let projects = PgProjectRepository::from_database(&database);
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let settings = ProjectSettings {
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
    };
    let project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("article-{tenant}")),
                display_name: "Article test".into(),
                settings: settings.clone(),
            },
        )
        .await
        .unwrap();
    let other_project = projects
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("article-other-{tenant}")),
                display_name: "Other project".into(),
                settings,
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(other_project.id));
    let frozen = project.settings.clone().validate_start().unwrap();
    let digest = settings_hash(&frozen).unwrap();
    let start = projects
        .start(
            &tenant_scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("article-test"),
                request_hash: start_request_hash(project.id, project.revision, &digest),
                settings_hash: digest,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let execution_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO content_executions \
         (execution_id,operator_id,tenant_id,project_id,cycle_id,manifest_id,manifest_revision,\
          policy_version,input_hash,state) VALUES ($1,$2,$3,$4,$5,$6,1,'test','test','{}'::jsonb)",
    )
    .bind(execution_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(start.cycle_id)
    .bind(start.document_manifest.manifest_id)
    .execute(database.pool())
    .await
    .unwrap();
    let revision = ContentRevision {
        revision_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        derived_from_revision_id: None,
        document: StructuredDocument {
            title: "Example".into(),
            blocks: vec![],
            schema_version: None,
        },
        markdown: "# Example".into(),
        evidence: vec![],
        quotes: vec![],
        findings: vec![],
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO content_revisions (revision_id,operator_id,tenant_id,project_id,\
         execution_id,asset_id,revision,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,1,$7,$8)",
    )
    .bind(revision.revision_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(execution_id)
    .bind(revision.asset_id)
    .bind(serde_json::to_value(&revision).unwrap())
    .bind(revision.created_at)
    .execute(database.pool())
    .await
    .unwrap();
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO channel_accounts \
         (operator_id,tenant_id,project_id,account_id,platform,metadata) \
         VALUES ($1,$2,$3,$4,'platform','{}'::jsonb)",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(account_id)
    .execute(database.pool())
    .await
    .unwrap();
    let account = ChannelAccount {
        account_id,
        project_id: project.id,
        owner_kind: ChannelOwnerKind::Customer,
        platform: "platform".into(),
        group_id: None,
        status: ChannelStatus::Ready,
        display_name: None,
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let input = AcceptContentDistributionRequest {
        revision: revision.clone(),
        account,
        placement_slot: "primary".into(),
        format: TEXT_DISTRIBUTION_FORMAT.into(),
        idempotency_key: "request-1".into(),
    };
    let repository = PgContentDistributionRequestRepository::from_database(&database);
    let accepted = repository.accept(&scope, input.clone()).await.unwrap();
    assert_eq!(accepted.publication_intent_id, None);
    assert_eq!(
        repository
            .get_by_idempotency_key(&scope, "request-1")
            .await
            .unwrap(),
        Some(accepted.clone())
    );
    assert_eq!(
        repository
            .get_by_idempotency_key(&other_scope, "request-1")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        repository
            .get_by_idempotency_key(&scope, "unused-key")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        repository.accept(&scope, input.clone()).await.unwrap(),
        accepted
    );
    assert_eq!(
        repository.get(&scope, accepted.request_id).await.unwrap(),
        accepted
    );
    assert_eq!(
        repository
            .get(&other_scope, accepted.request_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        repository
            .accept(&other_scope, input.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let mut changed = input.clone();
    changed.placement_slot = "secondary".into();
    assert_eq!(
        repository.accept(&scope, changed).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .link_intent(&scope, accepted.request_id, Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );

    // Construct an existing, normally materialized intent/outbox using the
    // same relational constraints. No request operation creates a send.
    let handoff_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO content_handoffs \
      (handoff_id,operator_id,tenant_id,project_id,execution_id,revision,body,created_at) \
      VALUES ($1,$2,$3,$4,$5,1,'{}'::jsonb,now())",
    )
    .bind(handoff_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(execution_id)
    .execute(database.pool())
    .await
    .unwrap();
    let manifest_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO distribution_execution_manifests \
      (manifest_id,operator_id,tenant_id,project_id,cycle_id,skeleton_manifest_id,\
       document_manifest_id,content_execution_id,content_handoff_id,revision,input_hash,\
       frozen,expected_count,expansion_cursor,complete) \
      VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,1,'test','{}'::jsonb,1,1,true)",
    )
    .bind(manifest_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(start.cycle_id)
    .bind(start.distribution_manifest.manifest_id)
    .bind(start.document_manifest.manifest_id)
    .bind(execution_id)
    .bind(handoff_id)
    .execute(database.pool())
    .await
    .unwrap();
    let target_id = Uuid::new_v4();
    sqlx::query("INSERT INTO distribution_execution_targets \
      (target_id,operator_id,tenant_id,project_id,manifest_id,ordinal,current_version,current_body) \
      VALUES ($1,$2,$3,$4,$5,0,1,'{}'::jsonb)")
      .bind(target_id).bind(operator).bind(tenant).bind(project.id.as_uuid()).bind(manifest_id)
      .execute(database.pool()).await.unwrap();
    let variant = ChannelVariant {
        variant_id: Uuid::new_v4(),
        content_revision_id: revision.revision_id,
        platform_id: "platform".into(),
        placement_slot: "primary".into(),
        policy_version: CHANNEL_VARIANT_POLICY.into(),
        title: "Example".into(),
        markdown: "# Example".into(),
        payload_hash: "payload".into(),
        evidence: vec![],
    };
    sqlx::query(
        "INSERT INTO distribution_channel_variants \
      (variant_id,operator_id,tenant_id,project_id,content_revision_id,body) \
      VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(variant.variant_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(revision.revision_id)
    .bind(serde_json::to_value(&variant).unwrap())
    .execute(database.pool())
    .await
    .unwrap();
    let intent = PublicationIntent {
        intent_id: Uuid::new_v4(),
        project_id: project.id,
        channel_target_id: target_id,
        variant_id: variant.variant_id,
        content_revision_id: revision.revision_id,
        platform_id: "platform".into(),
        placement_slot: "primary".into(),
        account_id,
        payload_hash: variant.payload_hash.clone(),
        logical_key: format!("article-{}", Uuid::new_v4()),
        verification: IntentVerification::Unverified,
        verification_evidence_id: None,
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO distribution_publication_intents \
      (intent_id,operator_id,tenant_id,project_id,origin_target_id,variant_id,logical_key,body) \
      VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(intent.intent_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(target_id)
    .bind(variant.variant_id)
    .bind(&intent.logical_key)
    .bind(serde_json::to_value(&intent).unwrap())
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO distribution_publication_commands \
      (command_id,operator_id,tenant_id,project_id,intent_id,origin_target_id,payload_hash,fixture) \
      VALUES ($1,$2,$3,$4,$5,$6,$7,true)")
      .bind(Uuid::new_v4()).bind(operator).bind(tenant).bind(project.id.as_uuid())
      .bind(intent.intent_id).bind(target_id).bind(&variant.payload_hash)
      .execute(database.pool()).await.unwrap();
    let linked = repository
        .link_intent(&scope, accepted.request_id, intent.intent_id)
        .await
        .unwrap();
    assert_eq!(linked.publication_intent_id, Some(intent.intent_id));
    assert_eq!(
        repository
            .link_intent(&scope, accepted.request_id, intent.intent_id)
            .await
            .unwrap(),
        linked
    );
    // A second valid ledger record must not replace the first association.
    let mut alternative = intent.clone();
    alternative.intent_id = Uuid::new_v4();
    alternative.logical_key = format!("article-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO distribution_publication_intents \
      (intent_id,operator_id,tenant_id,project_id,origin_target_id,variant_id,logical_key,body) \
      VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(alternative.intent_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(target_id)
    .bind(variant.variant_id)
    .bind(&alternative.logical_key)
    .bind(serde_json::to_value(&alternative).unwrap())
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO distribution_publication_commands \
      (command_id,operator_id,tenant_id,project_id,intent_id,origin_target_id,payload_hash,fixture) \
      VALUES ($1,$2,$3,$4,$5,$6,$7,true)")
      .bind(Uuid::new_v4()).bind(operator).bind(tenant).bind(project.id.as_uuid())
      .bind(alternative.intent_id).bind(target_id).bind(&variant.payload_hash)
      .execute(database.pool()).await.unwrap();
    assert_eq!(
        repository
            .link_intent(&scope, accepted.request_id, alternative.intent_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let mut conflicting = input;
    conflicting.idempotency_key = "request-2".into();
    conflicting.account.account_id = Uuid::new_v4();
    // A bogus account cannot be used even if the model supplies its UUID.
    assert_eq!(
        repository
            .accept(&scope, conflicting)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let second_account = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO channel_accounts \
      (operator_id,tenant_id,project_id,account_id,platform,metadata) \
      VALUES ($1,$2,$3,$4,'platform','{}'::jsonb)",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(second_account)
    .execute(database.pool())
    .await
    .unwrap();
    // Distinct accepted account cannot bind the original publication intent.
    let second_input = AcceptContentDistributionRequest {
        revision,
        account: repository_input_account(&scope, second_account),
        placement_slot: "primary".into(),
        format: TEXT_DISTRIBUTION_FORMAT.into(),
        idempotency_key: "request-second-account".into(),
    };
    let second_request = repository.accept(&scope, second_input).await.unwrap();
    assert_eq!(
        repository
            .link_intent(&scope, second_request.request_id, intent.intent_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .link_intent(&other_scope, accepted.request_id, intent.intent_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    // The legacy-link fixture above inserts intentionally minimal coverage
    // JSON for FK checks. It is not a runnable outbox command.
    sqlx::query(
        "UPDATE distribution_publication_commands SET status='claimed' \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
           AND intent_id IN ($4,$5)",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(intent.intent_id)
    .bind(alternative.intent_id)
    .execute(database.pool())
    .await
    .unwrap();

    // A genuinely independent checked article, with its own current public
    // evidence, must create one intent and one command without a coverage
    // target. The rest of this test performs no external send.
    let source_id = Uuid::new_v4();
    let source_version = Uuid::new_v4();
    let chunk_id = Uuid::new_v4();
    let text = "A cited public sentence.";
    let locator = ChunkLocator::Text {
        start_line: 1,
        end_line: 1,
        start_char: 0,
        end_char: text.len() as u32,
    };
    sqlx::query(
        "INSERT INTO knowledge_sources \
         (source_id,operator_id,tenant_id,project_id,revision,kind,name,purpose,state,locator) \
         VALUES ($1,$2,$3,$4,1,'text','test','public','active','{}'::jsonb)",
    )
    .bind(source_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO knowledge_source_versions \
         (source_version_id,operator_id,tenant_id,project_id,source_id,version,content_sha256,\
          captured_at,parser_version,extraction_version) VALUES ($1,$2,$3,$4,$5,1,$6,now(),'test','test')",
    )
    .bind(source_version)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(source_id)
    .bind(hex::encode(Sha256::digest(text.as_bytes())))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE knowledge_sources SET current_version_id=$1 \
         WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND source_id=$5",
    )
    .bind(source_version)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(source_id)
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO knowledge_chunks (chunk_id,operator_id,tenant_id,project_id,\
          source_version_id,ordinal,kind,text,text_hash,locator,extraction_method,confidence) \
         VALUES ($1,$2,$3,$4,$5,0,'paragraph',$6,$7,$8,'test',1)",
    )
    .bind(chunk_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(source_version)
    .bind(text)
    .bind(hex::encode(Sha256::digest(text.as_bytes())))
    .bind(serde_json::to_value(&locator).unwrap())
    .execute(database.pool())
    .await
    .unwrap();
    let cited = EvidenceRef {
        source_version_id: source_version,
        chunk_id: Some(chunk_id),
        locator,
    };
    let document = StructuredDocument {
        title: "Independent article".into(),
        blocks: vec![ContentBlock {
            block_id: Uuid::new_v4(),
            kind: ContentBlockKind::Paragraph,
            text: text.into(),
            citation_ids: vec![chunk_id],
            items: vec![],
            rich: None,
        }],
        schema_version: None,
    };
    let eligible = ContentRevision {
        revision_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        revision: 1,
        base_revision_id: None,
        derived_from_revision_id: None,
        markdown: document.markdown(),
        document,
        evidence: vec![cited.clone()],
        quotes: vec![ContentEvidence {
            reference: cited,
            exact_quote: text.into(),
        }],
        findings: vec![],
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO content_revisions (revision_id,operator_id,tenant_id,project_id,\
         execution_id,asset_id,revision,body,created_at) VALUES ($1,$2,$3,$4,$5,$6,1,$7,$8)",
    )
    .bind(eligible.revision_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(execution_id)
    .bind(eligible.asset_id)
    .bind(serde_json::to_value(&eligible).unwrap())
    .bind(eligible.created_at)
    .execute(database.pool())
    .await
    .unwrap();
    let check = ContentCheck {
        check_id: Uuid::new_v4(),
        revision_id: eligible.revision_id,
        findings: vec![],
        created_at: Utc::now(),
    };
    sqlx::query(
        "INSERT INTO content_checks \
         (check_id,operator_id,tenant_id,project_id,execution_id,revision_id,body,created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(check.check_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(execution_id)
    .bind(eligible.revision_id)
    .bind(serde_json::to_value(&check).unwrap())
    .bind(check.created_at)
    .execute(database.pool())
    .await
    .unwrap();
    let ready_account = repository_input_account(&scope, account_id);
    sqlx::query(
        "UPDATE channel_accounts SET metadata=$1 WHERE operator_id=$2 \
         AND tenant_id=$3 AND project_id=$4 AND account_id=$5",
    )
    .bind(serde_json::to_value(&ready_account).unwrap())
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(account_id)
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO connector_capability_settings \
         (operator_id,platform_id,placement_slot,revision,enabled,content_types) \
         VALUES ($1,'platform','primary',1,true,$2)",
    )
    .bind(operator)
    .bind(serde_json::json!(["plain_text_article.v1"]))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO connector_capability_verifications \
         (verification_id,operator_id,platform_id,placement_slot,connector_version,content_type,\
          publication_receipt,public_readback,verified_at) \
         VALUES ($1,$2,'platform','primary','fixture-v1','plain_text_article.v1',\
          '{}'::jsonb,'{}'::jsonb,now())",
    )
    .bind(Uuid::new_v4())
    .bind(operator)
    .execute(database.pool())
    .await
    .unwrap();
    let first_input = AcceptContentDistributionRequest {
        revision: eligible.clone(),
        account: ready_account.clone(),
        placement_slot: "primary".into(),
        format: TEXT_DISTRIBUTION_FORMAT.into(),
        idempotency_key: "independent-1".into(),
    };
    let first = repository
        .accept(&scope, first_input.clone())
        .await
        .unwrap();
    assert!(
        repository
            .list_unlinked(None, 100)
            .await
            .unwrap()
            .iter()
            .any(|request| request.request_id == first.request_id)
    );
    sqlx::query(
        "UPDATE connector_capability_settings SET enabled=false \
         WHERE operator_id=$1 AND platform_id='platform' AND placement_slot='primary'",
    )
    .bind(operator)
    .execute(database.pool())
    .await
    .unwrap();
    assert_eq!(
        repository
            .materialize(&scope, first.request_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // A new repository instance observes the same persisted, allowlisted
    // diagnosis, but the due scan skips this request until its short retry.
    let restarted = PgContentDistributionRequestRepository::from_database(&database);
    let deferred = restarted.get(&scope, first.request_id).await.unwrap();
    let deferral = deferred.materialization_deferral.unwrap();
    assert_eq!(
        deferral.reason,
        ContentRequestDeferralReason::ConnectorUnavailable
    );
    assert_eq!(deferral.attempts, 1);
    assert!(deferral.next_retry_at > first.created_at);
    // Pin the future due time for a timing-independent paging assertion.
    sqlx::query(
        "UPDATE content_distribution_requests \
         SET materialization_next_retry_at=clock_timestamp()+interval '1 hour' \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND request_id=$4",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(first.request_id)
    .execute(database.pool())
    .await
    .unwrap();
    let due = restarted.list_unlinked(None, 100).await.unwrap();
    assert!(
        due.iter()
            .all(|request| request.request_id != first.request_id)
    );
    assert!(
        due.iter()
            .any(|request| request.request_id == second_request.request_id)
    );
    sqlx::query(
        "UPDATE connector_capability_settings SET enabled=true \
         WHERE operator_id=$1 AND platform_id='platform' AND placement_slot='primary'",
    )
    .bind(operator)
    .execute(database.pool())
    .await
    .unwrap();
    // Direct recheck is allowed after restoring the prerequisite, without
    // accepting another request or waiting for the automatic retry clock.
    let materialized = repository
        .materialize(&scope, first.request_id)
        .await
        .unwrap();
    assert!(materialized.materialization_deferral.is_none());
    let first_intent = materialized.publication_intent_id.unwrap();
    // A stale failure writer's scoped, unlinked-only update cannot alter the
    // completed link. This uses the same predicate as record_deferral.
    let stale = sqlx::query(
        "UPDATE content_distribution_requests SET materialization_reason='internal_error', \
         materialization_attempts=1,materialization_next_retry_at=clock_timestamp()+interval '2 seconds' \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND request_id=$4 \
           AND publication_intent_id IS NULL",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(first.request_id)
    .execute(database.pool())
    .await
    .unwrap();
    assert_eq!(stale.rows_affected(), 0);
    assert!(
        repository
            .get(&scope, first.request_id)
            .await
            .unwrap()
            .materialization_deferral
            .is_none()
    );
    assert_eq!(
        repository
            .materialize(&scope, first.request_id)
            .await
            .unwrap(),
        materialized
    );
    let distribution = PgDistributionRepository::from_database(&database);
    let bundle = distribution
        .get_publication_bundle(&scope, first_intent)
        .await
        .unwrap();
    assert_eq!(bundle.intent.channel_target_id, Uuid::nil());
    assert_eq!(bundle.command.target_id, Uuid::nil());
    assert!(
        matches!(bundle.origin, PublicationOrigin::ContentRequest { request }
        if request.request_id == first.request_id)
    );
    let jobs = PgChannelJobRepository::from_database(&database);
    let bridge = jobs.materialize_pending_commands(None, 100).await.unwrap();
    assert!(
        bridge
            .iter()
            .any(|candidate| candidate.target_id == bundle.command.command_id)
    );
    let channel_row = sqlx::query(
        "SELECT cycle_id,frozen_input FROM channel_execution_targets \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND target_id=$4",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(bundle.command.command_id)
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(channel_row.get::<Option<Uuid>, _>("cycle_id"), None);
    let frozen: geo_domain::ChannelTarget =
        serde_json::from_value(channel_row.get("frozen_input")).unwrap();
    assert!(
        matches!(frozen.input, geo_domain::ChannelTargetInput::GeneratedPublish {
        origin_request_id: Some(id), distribution_target_id, ..
    } if id == first.request_id && distribution_target_id.is_nil())
    );
    assert!(
        jobs.materialize_pending_commands(None, 100)
            .await
            .unwrap()
            .is_empty()
    );

    // A second acceptance is a distinct user request, not a second logical
    // send—even after the original ledger has an unknown result.
    sqlx::query(
        "UPDATE distribution_publication_intents SET verification='unknown',\
         verification_evidence_id=$1,body=jsonb_set(jsonb_set(body,'{verification}',\
         '\"unknown\"'::jsonb),'{verification_evidence_id}',to_jsonb($1::text)) \
         WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND intent_id=$5",
    )
    .bind(Uuid::new_v4())
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(first_intent)
    .execute(database.pool())
    .await
    .unwrap();
    let mut next_input = first_input.clone();
    next_input.idempotency_key = "independent-2".into();
    let next = repository.accept(&scope, next_input).await.unwrap();
    let next = repository
        .materialize(&scope, next.request_id)
        .await
        .unwrap();
    assert_eq!(next.publication_intent_id, Some(first_intent));
    assert!(
        jobs.materialize_pending_commands(None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        distribution
            .get_publication_bundle(&scope, first_intent)
            .await
            .unwrap()
            .intent
            .verification,
        IntentVerification::Unknown
    );
    let bad_scope = repository
        .materialize(&other_scope, next.request_id)
        .await
        .unwrap_err();
    assert_eq!(bad_scope.code, ErrorCode::NotFound);

    let mut unsupported = first_input.clone();
    unsupported.idempotency_key = "rich-rejected".into();
    unsupported.format = geo_domain::RICH_DISTRIBUTION_FORMAT.into();
    let unsupported = repository.accept(&scope, unsupported).await.unwrap();
    assert_eq!(
        repository
            .materialize(&scope, unsupported.request_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .get(&scope, unsupported.request_id)
            .await
            .unwrap()
            .materialization_deferral
            .unwrap()
            .reason,
        ContentRequestDeferralReason::FormatUnsupported
    );
    sqlx::query(
        "UPDATE knowledge_sources SET purpose='internal' \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_id=$4",
    )
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(source_id)
    .execute(database.pool())
    .await
    .unwrap();
    let mut revoked = first_input;
    revoked.idempotency_key = "source-revoked".into();
    let revoked = repository.accept(&scope, revoked).await.unwrap();
    assert_eq!(
        repository
            .materialize(&scope, revoked.request_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        repository
            .get(&scope, revoked.request_id)
            .await
            .unwrap()
            .publication_intent_id,
        None
    );
    assert_eq!(
        repository
            .get(&scope, revoked.request_id)
            .await
            .unwrap()
            .materialization_deferral
            .unwrap()
            .reason,
        ContentRequestDeferralReason::SourceUnavailable
    );
}

fn repository_input_account(scope: &TenantScope, account_id: Uuid) -> ChannelAccount {
    ChannelAccount {
        account_id,
        project_id: scope.project_id.unwrap(),
        owner_kind: ChannelOwnerKind::Customer,
        platform: "platform".into(),
        group_id: None,
        status: ChannelStatus::Ready,
        display_name: None,
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: false,
        proxy_server: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
