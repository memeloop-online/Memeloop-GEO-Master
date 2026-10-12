use chrono::Utc;
use geo_domain::{
    CapturedConversation, CapturedConversationPurpose, ChannelJobRepository, ChannelTarget,
    ChannelTargetInput, ConversationCorrelation, ErrorCode, ObservationCaptureInput,
    ObservationCaptureRepository, ObservationCaptureSnapshot, ObservationCompletionRecoveryInput,
    ObservationCompletionRecoveryRepository, ObservationProviderIdentity,
    StandaloneMeasurementPlan, TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgObservationCaptureRepository,
    PgObservationCompletionRecoveryRepository,
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn immutable_scoped_recovery_replay_restart_and_revalidation() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other_project, account, target, attempt) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("recovery-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("recovery-{tenant}"))
        .execute(database.pool()).await.unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("recovery-{id}"))
            .execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let plan = Uuid::new_v4();
    PgChannelJobRepository::from_database(&database)
        .create_measurement_plan(
            &scope,
            &format!("recovery-{plan}"),
            "synthetic",
            StandaloneMeasurementPlan {
                plan_id: plan,
                project_id: scope.project_id.unwrap(),
                title: "Synthetic recovery".into(),
                input_hash: "synthetic".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![ChannelTarget {
                    target_id: target,
                    input: ChannelTargetInput::Measure {
                        account_id: account,
                        provider: "deepseek".into(),
                        model: "synthetic-model".into(),
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
                    },
                }],
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO channel_execution_attempts \
         (attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) \
         VALUES ($1,$2,$3,$4,$5,$6,'measure',$7)",
    )
    .bind(attempt)
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(target)
    .bind(account)
    .bind(Utc::now())
    .execute(database.pool())
    .await
    .unwrap();
    let observed_at = "2026-01-01T00:00:00Z".parse().unwrap();
    let source_json = json!({
        "messages": [
            {"event": "ready", "data": {"request_message_id": 1, "response_message_id": 2, "model_type": "synthetic-model"}},
            {"event": "delta", "data": {"v": "synthetic answer"}},
            {"event": "finish", "raw_data": ""}
        ],
        "ownership": {
            "schema_version": "geo.deepseek.ownership.v1", "provider": "deepseek",
            "creation": {"source": "browser_observed_create", "chat_session_id": "synthetic-chat"},
            "submission": {
                "chat_session_id": "synthetic-chat", "parent_message_id": null,
                "model_type": "synthetic-model", "prompt_sha256": "0".repeat(64),
                "search_enabled": true, "thinking_enabled": false
            }
        }
    }).to_string();
    let history_json = json!({
        "source": "browser_observed_history", "observed_at": "2026-01-01T00:00:00Z",
        "cache_request": {"cache_version": null, "cache_reset_at": null},
        "cache_control": "REPLACE", "chat_session_id": "synthetic-chat",
        "messages": [
            {"message_id": 1, "parent_id": null, "role": "USER", "status": "FINISHED", "has_pending_fragment": false},
            {"message_id": 2, "parent_id": 1, "role": "ASSISTANT", "status": "FINISHED", "has_pending_fragment": false}
        ], "current_message_id": 2
    }).to_string();
    let identity = ObservationProviderIdentity {
        provider: "deepseek".into(),
        platform_account_id: "synthetic-account".into(),
    };
    let original = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        target_id: target,
        attempt_id: attempt,
        account_id: account,
        runner_session_id: Uuid::new_v4(),
        original_identity: Some(identity.clone()),
        ordinal: 0,
        observed_at,
        snapshot: ObservationCaptureSnapshot::Source {
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json: source_json.clone(),
        },
        owned_conversation: Some(CapturedConversation {
            provider: "deepseek".into(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        }),
        completion: None,
    };
    let captures = PgObservationCaptureRepository::from_database(&database);
    let original_receipt = captures.save(&scope, original.clone()).await.unwrap();
    let input = ObservationCompletionRecoveryInput {
        recovery_id: Uuid::new_v4(),
        scope: scope.clone(),
        source_capture_id: original.capture_id,
        source_sha256: sha256_hex(source_json.as_bytes()),
        source_receipt_sha256: original_receipt.digest_sha256.clone(),
        recovery_execution_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        observed_at,
        recovery_identity: identity,
        history_sha256: sha256_hex(history_json.as_bytes()),
        history_json,
    };
    let store = PgObservationCompletionRecoveryRepository::from_database(&database);
    let receipt = store.save(&scope, input.clone()).await.unwrap();
    assert_eq!(store.save(&scope, input.clone()).await.unwrap(), receipt);
    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let no_project = TenantScope::new(operator.into(), tenant.into(), None);
    for wrong_scope in [
        other_scope.clone(),
        TenantScope::new(Uuid::new_v4().into(), tenant.into(), Some(project.into())),
        TenantScope::new(operator.into(), Uuid::new_v4().into(), Some(project.into())),
    ] {
        assert!(
            store
                .get(&wrong_scope, input.recovery_id)
                .await
                .unwrap()
                .is_none()
        );
        let mut rebound = input.clone();
        rebound.scope = wrong_scope.clone();
        assert_eq!(
            store.save(&wrong_scope, rebound).await.unwrap_err().code,
            ErrorCode::Forbidden
        );
    }
    assert_eq!(
        store
            .get(&no_project, input.recovery_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    for field in 0..3 {
        let mut mismatch = input.clone();
        mismatch.recovery_id = Uuid::new_v4();
        mismatch.recovery_execution_id = Uuid::new_v4();
        match field {
            0 => mismatch.source_sha256 = "0".repeat(64),
            1 => mismatch.source_receipt_sha256 = "0".repeat(64),
            _ => mismatch.source_capture_id = Uuid::new_v4(),
        }
        assert!(store.save(&scope, mismatch).await.is_err());
    }
    let mut concurrent = input.clone();
    concurrent.recovery_id = Uuid::new_v4();
    concurrent.recovery_execution_id = Uuid::new_v4();
    let (first, second) = tokio::join!(
        store.save(&scope, concurrent.clone()),
        store.save(&scope, concurrent)
    );
    assert_eq!(first.unwrap(), second.unwrap());
    let mut conflicting = input.clone();
    conflicting.runner_session_id = Uuid::new_v4();
    assert_eq!(
        store.save(&scope, conflicting).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut duplicate_execution = input.clone();
    duplicate_execution.recovery_id = Uuid::new_v4();
    assert_eq!(
        store
            .save(&scope, duplicate_execution)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    for statement in [
        "UPDATE observation_completion_recoveries SET input_hash=input_hash WHERE recovery_id=$1",
        "DELETE FROM observation_completion_recoveries WHERE recovery_id=$1",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(input.recovery_id)
                .execute(database.pool())
                .await
                .is_err()
        );
    }
    // Direct SQL cannot bypass the scoped foreign key, size, or hash constraints.
    for mutation in 0..3 {
        let mut invalid = input.clone();
        invalid.recovery_id = Uuid::new_v4();
        invalid.recovery_execution_id = Uuid::new_v4();
        if mutation == 1 {
            invalid.history_json = "x".repeat(750_001);
        }
        let inserted = sqlx::query(
            "INSERT INTO observation_completion_recoveries \
             (recovery_id,operator_id,tenant_id,project_id,source_capture_id,recovery_execution_id,input_hash,input) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        ).bind(invalid.recovery_id).bind(operator).bind(tenant)
            .bind(if mutation == 0 { other_project } else { project })
            .bind(original.capture_id).bind(invalid.recovery_execution_id)
            .bind(if mutation == 2 { "invalid".to_owned() } else { "0".repeat(64) })
            .bind(serde_json::to_value(invalid).unwrap()).execute(database.pool()).await;
        assert!(inserted.is_err());
    }
    // An untrusted database projection with a syntactically valid but wrong hash
    // must not become validated evidence on read.
    let mut corrupt = input.clone();
    corrupt.recovery_id = Uuid::new_v4();
    corrupt.recovery_execution_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO observation_completion_recoveries \
         (recovery_id,operator_id,tenant_id,project_id,source_capture_id,recovery_execution_id,input_hash,input) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    ).bind(corrupt.recovery_id).bind(operator).bind(tenant).bind(project)
        .bind(original.capture_id).bind(corrupt.recovery_execution_id).bind("0".repeat(64))
        .bind(serde_json::to_value(&corrupt).unwrap()).execute(database.pool()).await.unwrap();
    assert_eq!(
        store
            .get(&scope, corrupt.recovery_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Internal
    );
    drop(store);
    let restarted_database = Database::connect_and_migrate(&config).await.unwrap();
    let restarted = PgObservationCompletionRecoveryRepository::from_database(&restarted_database);
    let recovered = restarted
        .get(&scope, input.recovery_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.input, input);
    assert_eq!(recovered.receipt, receipt);
    let unchanged = captures
        .get(&scope, original.capture_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.input, original);
    assert_eq!(unchanged.receipt, original_receipt);
    assert!(!unchanged.has_complete_conversation_evidence(&scope));
}
