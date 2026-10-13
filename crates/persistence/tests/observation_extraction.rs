use chrono::Utc;
use geo_domain::{
    ChannelJobRepository, ChannelTarget, ChannelTargetInput, ObservationAnalysisOutcome,
    ObservationAnalysisResult, ObservationCaptureInput, ObservationCaptureRepository,
    ObservationCaptureSnapshot, ObservationExtractionInterruption, ObservationExtractionRepository,
    ObservationExtractionRequest, ObservationExtractionResponse,
    ObservationExtractionResponseDisposition, ObservationExtractionState,
    StandaloneMeasurementPlan, TenantScope, sha256_hex,
};
use geo_persistence::{
    Database, DatabaseConfig, PgChannelJobRepository, PgObservationCaptureRepository,
    PgObservationExtractionRepository,
};
use uuid::Uuid;

async fn source_is_discovered(
    store: &PgObservationExtractionRepository,
    as_of: chrono::DateTime<Utc>,
    capture_id: Uuid,
) -> bool {
    store
        .scan_unqueued_sources(as_of, Some(Uuid::from_u128(capture_id.as_u128() - 1)), 1)
        .await
        .unwrap()
        .first()
        .is_some_and(|item| item.capture_id == capture_id)
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn extraction_single_claim_response_recovery_fencing_and_scope() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let (operator, tenant, project, other_project) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("extraction-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("extraction-{tenant}")).execute(database.pool()).await.unwrap();
    for id in [project, other_project] {
        sqlx::query("INSERT INTO projects(project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Synthetic')")
        .bind(id).bind(operator).bind(tenant).bind(format!("extraction-{id}")).execute(database.pool()).await.unwrap();
    }
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let other = TenantScope::new(operator.into(), tenant.into(), Some(other_project.into()));
    let jobs = PgChannelJobRepository::from_database(&database);
    let captures = PgObservationCaptureRepository::from_database(&database);
    let store = PgObservationExtractionRepository::from_database(&database);
    let (target_id, attempt_id, account_id, capture_id) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let now = Utc::now();
    jobs.create_measurement_plan(
        &scope,
        "synthetic-plan",
        "synthetic-digest",
        StandaloneMeasurementPlan {
            plan_id: Uuid::new_v4(),
            project_id: project.into(),
            title: "Synthetic".into(),
            input_hash: "synthetic-digest".into(),
            revision: 1,
            created_at: now,
            targets: vec![ChannelTarget {
                target_id,
                input: ChannelTargetInput::Measure {
                    account_id,
                    provider: "synthetic".into(),
                    model: "synthetic".into(),
                    surface: "consumer_web".into(),
                    search_mode: "web_search".into(),
                    protocol_version: "v1".into(),
                    question_set_version: "adhoc".into(),
                    question: "Synthetic question?".into(),
                    market: "global".into(),
                    language: "en".into(),
                    scheduled_at: now,
                    sample_ordinal: 0,
                    question_binding: None,
                },
            }],
        },
    )
    .await
    .unwrap();
    // Still-running measurement: durable API extraction must not require the
    // original attempt to have a receipt before accepting its saved source.
    sqlx::query("INSERT INTO channel_execution_attempts(attempt_id,operator_id,tenant_id,project_id,target_id,account_id,target_kind,claimed_at) VALUES ($1,$2,$3,$4,$5,$6,'measure',$7)")
        .bind(attempt_id).bind(operator).bind(tenant).bind(project).bind(target_id).bind(account_id).bind(now).execute(database.pool()).await.unwrap();
    let source_json = r#"{"messages":[{"content":"Synthetic response"}]}"#.to_owned();
    let source_sha256 = sha256_hex(source_json.as_bytes());
    let receipt = captures
        .save(
            &scope,
            ObservationCaptureInput {
                capture_id,
                target_id,
                attempt_id,
                account_id,
                runner_session_id: Uuid::new_v4(),
                original_identity: None,
                ordinal: 0,
                observed_at: now,
                snapshot: ObservationCaptureSnapshot::Source {
                    source_json,
                    source_sha256: source_sha256.clone(),
                },
                owned_conversation: None,
                completion: None,
            },
        )
        .await
        .unwrap();
    assert!(source_is_discovered(&store, receipt.stored_at, capture_id).await);
    assert!(
        !source_is_discovered(
            &store,
            receipt.stored_at - chrono::Duration::microseconds(1),
            capture_id,
        )
        .await
    );
    let before = Some(Uuid::from_u128(capture_id.as_u128() - 1));
    let source_page = captures
        .scan_sources(receipt.stored_at, before, 1)
        .await
        .unwrap();
    assert_eq!(source_page.len(), 1);
    assert_eq!(source_page[0].scope, scope);
    assert_eq!(source_page[0].capture_id, capture_id);
    assert!(
        captures
            .scan_sources(receipt.stored_at, Some(capture_id), 100)
            .await
            .unwrap()
            .iter()
            .all(|item| item.capture_id > capture_id)
    );
    for limit in [0, 101] {
        assert!(
            captures
                .scan_sources(receipt.stored_at, None, limit)
                .await
                .is_err()
        );
        assert!(
            store
                .scan_unqueued_sources(receipt.stored_at, None, limit)
                .await
                .is_err()
        );
    }
    let request = ObservationExtractionRequest {
        target_id,
        attempt_id,
        source_capture_id: capture_id,
        source_sha256,
        prompt_version: "extract.v1".into(),
        parser_version: "ground.v1".into(),
        policy_version: "automatic.v1".into(),
    };
    let intent = store.enqueue(&scope, request.clone(), now).await.unwrap();
    assert!(!source_is_discovered(&store, receipt.stored_at, capture_id).await);
    // Another capture of identical bytes is not the same extraction identity.
    let mut equal_source = captures
        .get(&scope, capture_id)
        .await
        .unwrap()
        .unwrap()
        .input;
    equal_source.capture_id = Uuid::new_v4();
    equal_source.runner_session_id = Uuid::new_v4();
    let equal_receipt = captures.save(&scope, equal_source).await.unwrap();
    assert!(source_is_discovered(&store, equal_receipt.stored_at, equal_receipt.capture_id).await);
    assert_eq!(
        store
            .status_for_attempt(&scope, target_id, attempt_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ObservationExtractionState::Queued
    );
    for wrong_scope in [
        other.clone(),
        TenantScope::new(Uuid::new_v4().into(), scope.tenant_id, scope.project_id),
        TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id),
    ] {
        assert!(
            store
                .status_for_attempt(&wrong_scope, target_id, attempt_id)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        store
            .status_for_attempt(&scope, Uuid::new_v4(), attempt_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .status_for_attempt(&scope, target_id, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .enqueue(&scope, request.clone(), now)
            .await
            .unwrap()
            .intent_id,
        intent.intent_id
    );
    assert!(store.enqueue(&other, request.clone(), now).await.is_err());
    let mut wrong = request.clone();
    wrong.attempt_id = Uuid::new_v4();
    assert!(store.enqueue(&scope, wrong, now).await.is_err());
    let (a, b) = tokio::join!(
        store.claim(&scope, intent.intent_id, now),
        store.claim(&scope, intent.intent_id, now)
    );
    let claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(claims.len(), 1);
    let token = claims[0].claim_token;
    assert!(!source_is_discovered(&store, receipt.stored_at, capture_id).await);
    assert_eq!(
        store
            .status_for_attempt(&scope, target_id, attempt_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ObservationExtractionState::Running
    );
    assert!(
        store
            .claim(&scope, intent.intent_id, now)
            .await
            .unwrap()
            .is_none()
    );
    let response = ObservationExtractionResponse {
        model_text: "  {invalid JSON}\n中文  ".into(),
        disposition: ObservationExtractionResponseDisposition::Complete,
        actual_model: "synthetic-model".into(),
        config_revision: Some(5),
        prompt_tokens: 13,
        completion_tokens: 8,
    };
    assert!(
        store
            .record_response(
                &scope,
                intent.intent_id,
                Uuid::new_v4(),
                response.clone(),
                now
            )
            .await
            .is_err()
    );
    store
        .record_response(&scope, intent.intent_id, token, response.clone(), now)
        .await
        .unwrap();
    let resumed = PgObservationExtractionRepository::from_database(&database);
    assert!(!source_is_discovered(&resumed, receipt.stored_at, capture_id).await);
    let status = resumed
        .status_for_attempt(&scope, target_id, attempt_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.state, ObservationExtractionState::ResponseReceived);
    assert_eq!(
        serde_json::to_value(status)
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        5
    );
    let claimed = resumed
        .claim_response(&scope, intent.intent_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.intent.response, Some(response.clone()));
    assert_eq!(claimed.claim_token, token);
    assert!(
        resumed
            .get(&other, intent.intent_id)
            .await
            .unwrap()
            .is_none()
    );
    let result = ObservationAnalysisResult {
        config_revision: response.config_revision,
        actual_model: Some(response.actual_model.clone()),
        candidate_json: None,
        outcome: ObservationAnalysisOutcome::Failed {
            code: "invalid_model_response".into(),
        },
        prompt_tokens: response.prompt_tokens,
        completion_tokens: response.completion_tokens,
    };
    let complete = resumed
        .complete(&scope, intent.intent_id, token, result.clone(), now)
        .await
        .unwrap();
    assert_eq!(complete.state, ObservationExtractionState::Completed);
    assert!(!source_is_discovered(&resumed, receipt.stored_at, capture_id).await);
    assert_eq!(
        resumed
            .status_for_attempt(&scope, target_id, attempt_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ObservationExtractionState::Completed
    );
    assert_eq!(
        resumed
            .complete(&scope, intent.intent_id, token, result, now)
            .await
            .unwrap(),
        complete
    );
    // Database guard also rejects attempts to requeue or rewrite saved response.
    assert!(
        sqlx::query("UPDATE observation_extraction_intents SET state='queued' WHERE intent_id=$1")
            .bind(intent.intent_id)
            .execute(database.pool())
            .await
            .is_err()
    );
    let mut second = request;
    second.policy_version = "automatic.v2".into();
    let second = store.enqueue(&scope, second, now).await.unwrap();
    assert!(!source_is_discovered(&store, receipt.stored_at, capture_id).await);
    assert_eq!(
        store
            .status_for_attempt(&scope, target_id, attempt_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ObservationExtractionState::Queued
    );
    let token = store
        .claim(&scope, second.intent_id, now)
        .await
        .unwrap()
        .unwrap()
        .claim_token;
    store
        .record_response(&scope, second.intent_id, token, response.clone(), now)
        .await
        .unwrap();
    let interrupted = store
        .interrupt(
            &scope,
            second.intent_id,
            token,
            ObservationExtractionInterruption::GroundingInterrupted,
            now,
        )
        .await
        .unwrap();
    assert_eq!(interrupted.response, Some(response.clone()));
    assert!(!source_is_discovered(&store, receipt.stored_at, capture_id).await);
    assert!(
        store
            .record_response(&scope, second.intent_id, token, response, now)
            .await
            .is_err()
    );
    assert!(
        store
            .claim(&scope, second.intent_id, now)
            .await
            .unwrap()
            .is_none()
    );
}
