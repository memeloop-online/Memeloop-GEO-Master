//! Run against a disposable database with GEO_TEST_DATABASE_URL.
use chrono::Utc;
use geo_domain::{
    AcceptContentDistributionRequest, CHANNEL_VARIANT_POLICY, ChannelAccount, ChannelOwnerKind,
    ChannelStatus, ChannelVariant, ContentDistributionRequestRepository, ContentRevision,
    ErrorCode, InitialSource, InitialSourceKind, InitialSourceVisibility, IntentVerification,
    ProjectCreate, ProjectRepository, ProjectSettings, ProjectStartCommand, PublicationIntent,
    StructuredDocument, TEXT_DISTRIBUTION_FORMAT, TenantScope, hash_idempotency_key, settings_hash,
    start_request_hash,
};
use geo_persistence::{
    Database, DatabaseConfig, PgContentDistributionRequestRepository, PgProjectRepository,
};
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
