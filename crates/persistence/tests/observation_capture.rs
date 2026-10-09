use chrono::Utc;
use geo_domain::{
    CapturedConversation, CapturedConversationPurpose, ChannelJobRepository, ChannelTarget,
    ChannelTargetInput, ConversationCorrelation, ErrorCode, ObservationCaptureCompletion,
    ObservationCaptureInput, ObservationCaptureRepository, ObservationCaptureSnapshot,
    ObservationCompletionProtocol, ObservationProviderIdentity, StandaloneMeasurementPlan,
    TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgObservationCaptureRepository,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn capture_commit_replay_scope_and_attempt_binding() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required");
    let config = DatabaseConfig::from_url(url).expect("valid database URL");
    let database = Database::connect_and_migrate(&config)
        .await
        .expect("migrations");
    let (operator, tenant, project, other_project, account, target, attempt, session) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let plan = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("capture-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("capture-{tenant}")).execute(database.pool()).await.unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
            .bind(id).bind(operator).bind(tenant).bind(format!("capture-{id}")).execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let frozen = ChannelTarget {
        target_id: target,
        input: ChannelTargetInput::Measure {
            account_id: account,
            provider: "kimi".into(),
            model: "synthetic".into(),
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
    };
    PgChannelJobRepository::from_database(&database)
        .create_measurement_plan(
            &scope,
            &format!("capture-{plan}"),
            "request",
            StandaloneMeasurementPlan {
                plan_id: plan,
                project_id: scope.project_id.unwrap(),
                title: "Synthetic capture regression".into(),
                input_hash: "frozen".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![frozen.clone()],
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

    let other_scope = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let store = PgObservationCaptureRepository::from_database(&database);
    let source_json = r#"{"messages":[{"text":"synthetic answer"}]}"#.to_owned();
    let input = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        target_id: target,
        attempt_id: attempt,
        account_id: account,
        runner_session_id: session,
        original_identity: None,
        ordinal: 0,
        observed_at: Utc::now(),
        snapshot: ObservationCaptureSnapshot::Source {
            source_sha256: sha256_hex(source_json.as_bytes()),
            source_json,
        },
        owned_conversation: None,
        completion: None,
    };
    let receipt = store.save(&scope, input.clone()).await.unwrap();
    assert_eq!(store.save(&scope, input.clone()).await.unwrap(), receipt);
    let raw_json = r#"{"messages":[{"text":"malformed extraction JSON"}]}"#.to_owned();
    let raw = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        ordinal: 1,
        snapshot: ObservationCaptureSnapshot::Extraction {
            source_capture_id: input.capture_id,
            source_sha256: sha256_hex(raw_json.as_bytes()),
            source_json: raw_json,
        },
        ..input.clone()
    };
    let raw_receipt = store.save(&scope, raw.clone()).await.unwrap();
    assert_eq!(store.save(&scope, raw.clone()).await.unwrap(), raw_receipt);
    let complete_json = serde_json::json!({"messages": [
        {"chat": {"id": "synthetic-chat"}},
        {"message": {"id": "synthetic-message", "chat_id": "synthetic-chat", "role": "assistant", "status": "COMPLETED"}}
    ]}).to_string();
    let mut completed = ObservationCaptureInput {
        capture_id: Uuid::new_v4(),
        runner_session_id: Uuid::new_v4(),
        original_identity: Some(ObservationProviderIdentity {
            provider: "kimi".into(),
            platform_account_id: "synthetic-account".into(),
        }),
        owned_conversation: Some(CapturedConversation {
            provider: "kimi".into(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        }),
        completion: Some(ObservationCaptureCompletion {
            protocol: ObservationCompletionProtocol::ConnectJson,
            terminal: true,
            assistant_message_ids: vec!["synthetic-message".into()],
        }),
        snapshot: ObservationCaptureSnapshot::Source {
            source_sha256: sha256_hex(complete_json.as_bytes()),
            source_json: complete_json.clone(),
        },
        ..input.clone()
    };
    let completed_source_id = completed.capture_id;
    let mut wrong_provider = completed.clone();
    wrong_provider.completion = None;
    wrong_provider.owned_conversation.as_mut().unwrap().provider = "other".into();
    wrong_provider.original_identity.as_mut().unwrap().provider = "other".into();
    assert_eq!(
        store.save(&scope, wrong_provider).await.unwrap_err().code,
        ErrorCode::Forbidden
    );
    let mut corrupted_target = frozen.clone();
    corrupted_target.target_id = Uuid::new_v4();
    sqlx::query("UPDATE channel_execution_targets SET frozen_input=$1 WHERE target_id=$2")
        .bind(serde_json::to_value(corrupted_target).unwrap())
        .bind(target)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        store
            .save(&scope, completed.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    sqlx::query("UPDATE channel_execution_targets SET frozen_input=$1 WHERE target_id=$2")
        .bind(serde_json::to_value(&frozen).unwrap())
        .bind(target)
        .execute(database.pool())
        .await
        .unwrap();
    for ordinal in 0..=1 {
        if ordinal == 1 {
            completed.capture_id = Uuid::new_v4();
            completed.ordinal = ordinal;
            completed.owned_conversation.as_mut().unwrap().purpose =
                CapturedConversationPurpose::Extraction;
            completed.snapshot = ObservationCaptureSnapshot::Extraction {
                source_capture_id: completed_source_id,
                source_sha256: sha256_hex(complete_json.as_bytes()),
                source_json: complete_json.clone(),
            };
        }
        let receipt = store.save(&scope, completed.clone()).await.unwrap();
        assert_eq!(
            store.save(&scope, completed.clone()).await.unwrap(),
            receipt
        );
        let reread = store
            .get(&scope, completed.capture_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reread.input, completed);
        assert!(reread.has_complete_conversation_evidence(&scope));
    }
    assert_eq!(
        store
            .get(&scope, raw.capture_id)
            .await
            .unwrap()
            .unwrap()
            .input,
        raw
    );
    let mut mismatched = raw.clone();
    mismatched.capture_id = Uuid::new_v4();
    mismatched.runner_session_id = Uuid::new_v4();
    assert_eq!(
        store.save(&scope, mismatched).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    // Valid source JSON can nearly double in size when nested in the stored
    // input. Exercise the database bound, not only domain validation.
    let escaped_source = serde_json::json!({
        "messages": [],
        "rendered_text": "\"".repeat(300_000)
    })
    .to_string();
    let mut escaped_input = input.clone();
    escaped_input.capture_id = Uuid::new_v4();
    escaped_input.runner_session_id = Uuid::new_v4();
    escaped_input.snapshot = ObservationCaptureSnapshot::Source {
        source_sha256: sha256_hex(escaped_source.as_bytes()),
        source_json: escaped_source,
    };
    assert!(serde_json::to_vec(&escaped_input).unwrap().len() > 1_100_000);
    store.save(&scope, escaped_input.clone()).await.unwrap();
    assert_eq!(
        store
            .get(&scope, escaped_input.capture_id)
            .await
            .unwrap()
            .unwrap()
            .input,
        escaped_input
    );
    assert_eq!(
        store
            .get(&scope, input.capture_id)
            .await
            .unwrap()
            .unwrap()
            .input,
        input
    );
    assert!(
        store
            .get(&other_scope, input.capture_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .save(&other_scope, input.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    let mut conflicting = input.clone();
    conflicting.observed_at = Utc::now();
    assert_eq!(
        store.save(&scope, conflicting).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut wrong_account = input.clone();
    wrong_account.capture_id = Uuid::new_v4();
    wrong_account.account_id = Uuid::new_v4();
    assert_eq!(
        store.save(&scope, wrong_account).await.unwrap_err().code,
        ErrorCode::Forbidden
    );
    let mut same_ordinal = input;
    same_ordinal.capture_id = Uuid::new_v4();
    assert_eq!(
        store.save(&scope, same_ordinal).await.unwrap_err().code,
        ErrorCode::Conflict
    );
}
