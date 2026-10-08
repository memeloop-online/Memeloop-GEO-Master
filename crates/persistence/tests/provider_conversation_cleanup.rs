use chrono::Utc;
use geo_domain::{
    CapturedConversation, CapturedConversationPurpose, ChannelTargetInput, ConversationCorrelation,
    ExtractionRoute, ObservationCaptureInput, ObservationCaptureRepository,
    ObservationCaptureSnapshot, ProviderCleanupAction, ProviderCleanupOutcome,
    ProviderConversationCleanupRepository, TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgObservationCaptureRepository,
    PgProviderConversationCleanupRepository,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn persisted_ownership_fenced_claim_unknown_recovery_and_capture_retention() {
    let config = DatabaseConfig::from_url(std::env::var("GEO_TEST_DATABASE_URL").unwrap()).unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, account, target, attempt, plan) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("cleanup-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')",
    )
    .bind(tenant)
    .bind(operator)
    .bind(format!("cleanup-{tenant}"))
    .execute(database.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,$4,'Synthetic')")
        .bind(project).bind(operator).bind(tenant).bind(format!("cleanup-{project}")).execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO measurement_execution_plans(plan_id,operator_id,tenant_id,project_id,idempotency_key,request_hash,input_hash,revision,plan,created_at) VALUES($1,$2,$3,$4,$5,'request','frozen',1,'{}',now())")
        .bind(plan).bind(operator).bind(tenant).bind(project).bind(format!("cleanup-{plan}")).execute(database.pool()).await.unwrap();
    let frozen = ChannelTargetInput::Measure {
        account_id: account,
        provider: "synthetic".into(),
        model: "fixed".into(),
        surface: "consumer_web".into(),
        search_mode: "web_search".into(),
        protocol_version: "v1".into(),
        question_set_version: "adhoc".into(),
        question: "Synthetic question?".into(),
        market: "global".into(),
        language: "en".into(),
        scheduled_at: Utc::now(),
        sample_ordinal: 0,
        question_binding: None,
    };
    sqlx::query("INSERT INTO channel_execution_targets(target_id,operator_id,tenant_id,project_id,kind,frozen_input,ordinal,measurement_plan_id) VALUES($1,$2,$3,$4,'measure',$5,0,$6)")
        .bind(target).bind(operator).bind(tenant).bind(project).bind(serde_json::to_value(frozen).unwrap()).bind(plan).execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO channel_execution_attempts(attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) VALUES($1,$2,$3,$4,$5,$6,'measure',now())")
        .bind(attempt).bind(operator).bind(tenant).bind(project).bind(target).bind(account).execute(database.pool()).await.unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let captures = PgObservationCaptureRepository::from_database(&database);
    let cleanup = PgProviderConversationCleanupRepository::from_database(&database);
    let source_json = r#"{"messages":[{"text":"synthetic answer"}]}"#.to_owned();
    let input = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        target_id: target,
        attempt_id: attempt,
        account_id: account,
        runner_session_id: Uuid::new_v4(),
        original_identity: Some(geo_domain::ObservationProviderIdentity {
            provider: "synthetic".into(),
            platform_account_id: "synthetic-account".into(),
        }),
        ordinal: 0,
        observed_at: Utc::now(),
        snapshot: ObservationCaptureSnapshot::Source {
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json,
        },
        owned_conversation: Some(CapturedConversation {
            provider: "synthetic".into(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        }),
    };
    assert!(cleanup.enqueue(&scope, input.capture_id).await.is_err());
    captures.save(&scope, input.clone()).await.unwrap();
    let id = cleanup.enqueue(&scope, input.capture_id).await.unwrap();
    assert_eq!(cleanup.enqueue(&scope, input.capture_id).await.unwrap(), id);
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());
    let legacy = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        original_identity: None,
        ..input.clone()
    };
    captures.save(&scope, legacy.clone()).await.unwrap();
    assert!(cleanup.enqueue(&scope, legacy.capture_id).await.is_err());
    let unowned = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        owned_conversation: None,
        ..input.clone()
    };
    captures.save(&scope, unowned.clone()).await.unwrap();
    assert!(cleanup.enqueue(&scope, unowned.capture_id).await.is_err());
    let duplicate = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        ..input.clone()
    };
    captures.save(&scope, duplicate.clone()).await.unwrap();
    assert_eq!(
        cleanup.enqueue(&scope, duplicate.capture_id).await.unwrap(),
        id
    );
    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(Uuid::new_v4().into()));
    assert!(
        cleanup
            .enqueue(&other_scope, input.capture_id)
            .await
            .is_err()
    );
    assert!(cleanup.claim_due(&other_scope).await.unwrap().is_none());
    let candidate_json = r#"{"decision":"unverified"}"#.to_owned();
    let candidate = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        ordinal: 1,
        snapshot: ObservationCaptureSnapshot::Candidate {
            source_capture_id: input.capture_id,
            route: ExtractionRoute::ConfiguredModelApi,
            candidate_sha256: sha256_hex(candidate_json.as_bytes()),
            candidate_json,
            grounding_reason: Some("model_unverified".into()),
        },
        owned_conversation: Some(CapturedConversation {
            provider: "synthetic".into(),
            external_conversation_id: "synthetic-extraction".into(),
            purpose: CapturedConversationPurpose::Extraction,
            correlation: ConversationCorrelation::CreateResponse,
        }),
        ..input.clone()
    };
    captures.save(&scope, candidate.clone()).await.unwrap();
    assert!(cleanup.enqueue(&scope, candidate.capture_id).await.is_err());
    // Unknown interpretation still preserves raw evidence and ends active use.
    sqlx::query("UPDATE channel_execution_attempts SET outcome='{\"status\":\"unknown\"}',received_at=now() WHERE attempt_id=$1")
        .bind(attempt).execute(database.pool()).await.unwrap();
    let claim = cleanup.claim_due(&scope).await.unwrap().unwrap();
    assert_eq!(claim.action, ProviderCleanupAction::Delete);
    assert_eq!(
        Some(&claim.original_identity),
        input.original_identity.as_ref()
    );
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());
    assert!(
        cleanup
            .finish(&scope, id, Uuid::new_v4(), ProviderCleanupOutcome::Deleted)
            .await
            .is_err()
    );
    sqlx::query("UPDATE provider_conversation_cleanup SET lease_until=now()-interval '1 second' WHERE cleanup_id=$1")
        .bind(id).execute(database.pool()).await.unwrap();
    let resumed = cleanup.claim_due(&scope).await.unwrap().unwrap();
    assert_eq!(resumed.action, ProviderCleanupAction::Reconcile);
    assert_ne!(claim.lease_id, resumed.lease_id);
    assert!(
        cleanup
            .finish(&scope, id, claim.lease_id, ProviderCleanupOutcome::Deleted)
            .await
            .is_err()
    );
    cleanup
        .finish(
            &scope,
            id,
            resumed.lease_id,
            ProviderCleanupOutcome::Unknown,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE provider_conversation_cleanup SET next_attempt_at=now(),attempt_count=25 WHERE cleanup_id=$1",
    )
    .bind(id)
    .execute(database.pool())
    .await
    .unwrap();
    let reconcile = cleanup.claim_due(&scope).await.unwrap().unwrap();
    assert_eq!(reconcile.action, ProviderCleanupAction::Reconcile);
    cleanup
        .finish(
            &scope,
            id,
            reconcile.lease_id,
            ProviderCleanupOutcome::Deleted,
        )
        .await
        .unwrap();
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());
    assert_eq!(
        captures
            .get(&scope, input.capture_id)
            .await
            .unwrap()
            .unwrap()
            .input,
        input
    );
    assert_eq!(
        cleanup.enqueue(&scope, duplicate.capture_id).await.unwrap(),
        id
    );
    // Raw extraction ownership survives malformed/missing interpretation and
    // is independently eligible for cleanup after the attempt has ended.
    let source_json = r#"{"messages":[{"text":"not JSON"}]}"#.to_owned();
    let raw_extraction = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        ordinal: 2,
        snapshot: ObservationCaptureSnapshot::Extraction {
            source_capture_id: input.capture_id,
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json,
        },
        owned_conversation: candidate.owned_conversation.clone(),
        ..input.clone()
    };
    captures.save(&scope, raw_extraction.clone()).await.unwrap();
    let raw_cleanup_id = cleanup
        .enqueue(&scope, raw_extraction.capture_id)
        .await
        .unwrap();
    assert_ne!(raw_cleanup_id, id);
    assert_eq!(
        cleanup
            .enqueue(&scope, raw_extraction.capture_id)
            .await
            .unwrap(),
        raw_cleanup_id
    );
    let raw_claim = cleanup.claim_due(&scope).await.unwrap().unwrap();
    assert_eq!(raw_claim.action, ProviderCleanupAction::Delete);
    cleanup
        .finish(
            &scope,
            raw_cleanup_id,
            raw_claim.lease_id,
            ProviderCleanupOutcome::Deleted,
        )
        .await
        .unwrap();
    assert!(cleanup.claim_due(&scope).await.unwrap().is_none());
}
