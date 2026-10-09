//! Synthetic browser bridge coverage, not external website acceptance.
use crate::{
    AppState, ChannelService,
    browser_bridge::BrowserBridge,
    measurement_options::discover,
    standalone_measurements::{create_agent_plan, read_agent_plan},
};
use axum::{
    Json, Router,
    extract::State,
    routing::{delete, get, post},
};
use chrono::Utc;
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOutcome, ChannelOutcomeStatus, ChannelOwnerKind,
    ChannelSecret, ChannelStatus, ChannelTargetInput, DEVELOPMENT_OPERATOR_ID,
    DEVELOPMENT_TENANT_ID, ErrorCode, FrozenQuestionBinding, ProjectCreate, ProjectId,
    ProjectSettings, QuestionPurpose, QuestionReference, TenantScope, sha256_hex,
};
use geo_provider::SecretEnvelope;
use geo_worker::{MeasurementPlanCreateRequest, MeasurementPlanReadRequest};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Stub {
    mismatch: Arc<AtomicBool>,
    unavailable: Arc<AtomicBool>,
    closed: Arc<AtomicUsize>,
    starts: Arc<tokio::sync::Mutex<Vec<Value>>>,
}

async fn start(State(stub): State<Stub>, Json(input): Json<Value>) -> Json<Value> {
    stub.starts
        .lock()
        .await
        .push(input["storage_state"].clone());
    Json(json!({"session_id":input["session_id"]}))
}
async fn complete(State(stub): State<Stub>) -> Json<Value> {
    Json(json!({"identity":{
        "platform_account_id":if stub.mismatch.load(Ordering::SeqCst) { "changed" } else { "observed-account" },
        "display_name":"Fixture", "avatar_url":null
    },"storage_state":{"private":"not-returned"}}))
}
async fn options(State(stub): State<Stub>) -> (http::StatusCode, Json<Value>) {
    if stub.unavailable.load(Ordering::SeqCst) {
        return (
            http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"measurement_options_unavailable"})),
        );
    }
    (
        http::StatusCode::OK,
        Json(
            json!({"models":[{"id":"observed-model","label":"Observed model"}],"selected_model":null}),
        ),
    )
}
async fn close(State(stub): State<Stub>) -> Json<Value> {
    stub.closed.fetch_add(1, Ordering::SeqCst);
    Json(json!({"closed":true}))
}

#[tokio::test]
async fn discovery_restores_identity_returns_only_models_and_always_closes() {
    let stub = Stub::default();
    let app = Router::new()
        .route("/v1/sessions", post(start))
        .route("/v1/sessions/{id}/complete", post(complete))
        .route("/v1/sessions/{id}/measurement-options", get(options))
        .route("/v1/sessions/{id}", delete(close))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let key = "17".repeat(32);
    let service = ChannelService::persistent(
        Arc::new(geo_domain::MemoryChannelRepository::default()),
        &key,
        Some(BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap()),
    )
    .unwrap();
    let state = AppState::development().with_channel_service(service);
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project_id = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Measurement test".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap()
        .id;
    assert!(
        state
            .project_repository()
            .get(
                &TenantScope::new(
                    DEVELOPMENT_OPERATOR_ID,
                    DEVELOPMENT_TENANT_ID,
                    Some(project_id)
                ),
                project_id,
            )
            .await
            .unwrap()
            .unwrap()
            .current_cycle_id
            .is_none()
    );
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id),
    );
    let account_id = Uuid::new_v4();
    let aad = format!(
        "geo-channel-v1:{}:{}:{}:{}:session",
        scope.operator_id,
        scope.tenant_id,
        project_id.as_uuid(),
        account_id
    );
    let encrypted = SecretEnvelope::from_hex_key(&key)
        .unwrap()
        .seal(aad.as_bytes(), br#"{"cookies":[],"origins":[]}"#)
        .unwrap();
    state
        .channel_service()
        .repository
        .save_account(
            &scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id,
                    project_id,
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "kimi".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: Some("Fixture".into()),
                    platform_account_id: Some("observed-account".into()),
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: false,
                    proxy_server: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                session: Some(ChannelSecret::new(encrypted)),
                proxy: None,
            },
        )
        .await
        .unwrap();
    let result = discover(&state, &scope, account_id).await.unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({
            "models":[{"id":"observed-model","label":"Observed model"}],"selected_model":null
        })
    );
    assert_eq!(stub.closed.load(Ordering::SeqCst), 1);
    assert_eq!(
        stub.starts.lock().await[0],
        json!({"cookies":[],"origins":[]})
    );
    let renewed = state
        .channel_service()
        .repository
        .get_account(&scope, account_id)
        .await
        .unwrap();
    let plaintext = SecretEnvelope::from_hex_key(&key)
        .unwrap()
        .open(
            aad.as_bytes(),
            renewed.session.as_ref().unwrap().encrypted_bytes(),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&plaintext).unwrap(),
        json!({"private":"not-returned"})
    );
    let command = MeasurementPlanCreateRequest {
        account_id,
        question: "How do rain gauges work?".into(),
        idempotency_key: "single-question-1".into(),
        model: None,
    };
    let accepted = create_agent_plan(&state, &scope, command.clone())
        .await
        .unwrap();
    assert_eq!(accepted.state, "accepted");
    assert_eq!(accepted.model, "observed-model");
    assert_eq!(
        stub.starts.lock().await[1],
        json!({"private":"not-returned"})
    );
    let replay = create_agent_plan(&state, &scope, command.clone())
        .await
        .unwrap();
    assert_eq!(replay, accepted);
    assert_eq!(
        stub.closed.load(Ordering::SeqCst),
        2,
        "replay does not inspect browser again"
    );
    let read = read_agent_plan(
        &state,
        &scope,
        MeasurementPlanReadRequest {
            plan_id: accepted.plan_id,
        },
    )
    .await
    .unwrap();
    assert_eq!(read.targets.len(), 1);
    assert_eq!(read.targets[0].state, "queued");
    assert_eq!(read.targets[0].target_id, accepted.target_id);
    let status_json = serde_json::to_string(&read).unwrap();
    assert!(!status_json.contains("rain gauges"));
    assert!(!status_json.contains("raw_answer"));
    assert!(!status_json.contains("cycle_id"));
    let repository = state.channel_job_repository();
    let observed_at = Utc::now();
    let attempt_id = Uuid::new_v4();
    repository
        .claim(&scope, accepted.target_id, attempt_id, observed_at)
        .await
        .unwrap();
    repository
        .finish(
            &scope,
            accepted.target_id,
            attempt_id,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Observed,
                detail: None,
                occurred_at: observed_at,
                raw_answer: Some("Actual answer from live evidence".into()),
                citations: vec!["https://example.org/evidence".into()],
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("web-test-v1".into()),
                runner_evidence: vec![],
                fixture: false,
            },
            observed_at,
        )
        .await
        .unwrap();
    let observed = read_agent_plan(
        &state,
        &scope,
        MeasurementPlanReadRequest {
            plan_id: accepted.plan_id,
        },
    )
    .await
    .unwrap();
    assert_eq!(observed.targets[0].state, "completed");
    assert_eq!(
        observed.targets[0].answer.as_deref(),
        Some("Actual answer from live evidence")
    );
    assert_eq!(
        observed.targets[0].citations,
        ["https://example.org/evidence"]
    );
    assert_eq!(observed.targets[0].fixture, Some(false));

    // A plan with a frozen-evaluation binding may be read for operational
    // progress, but its text and answer must not become optimization input.
    let mut heldout = repository
        .get_measurement_plan(&scope, accepted.plan_id)
        .await
        .unwrap()
        .unwrap();
    heldout.plan_id = Uuid::new_v4();
    heldout.targets[0].target_id = Uuid::new_v4();
    if let ChannelTargetInput::Measure {
        question,
        question_binding,
        ..
    } = &mut heldout.targets[0].input
    {
        *question = "HELDOUT_CANARY_DO_NOT_EXPOSE".into();
        *question_binding = Some(FrozenQuestionBinding {
            reference: QuestionReference {
                question_set_id: Uuid::new_v4(),
                question_set_version_id: Uuid::new_v4(),
                question_id: Uuid::new_v4(),
                question_revision_id: Uuid::new_v4(),
            },
            purpose: QuestionPurpose::FrozenEvaluation,
            split_policy_version: "v1".into(),
        });
    }
    if let ChannelTargetInput::Measure {
        account_id,
        question,
        ..
    } = &heldout.targets[0].input
    {
        heldout.input_hash =
            sha256_hex(&serde_json::to_vec(&(account_id, question, None::<String>)).unwrap());
    }
    let bound_target_id = heldout.targets[0].target_id;
    let bound_id = heldout.plan_id;
    let bound_hash = heldout.input_hash.clone();
    repository
        .create_measurement_plan(&scope, "heldout-plan", &bound_hash, heldout)
        .await
        .unwrap();
    let bound_attempt = Uuid::new_v4();
    repository
        .claim(&scope, bound_target_id, bound_attempt, observed_at)
        .await
        .unwrap();
    repository
        .finish(
            &scope,
            bound_target_id,
            bound_attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Observed,
                detail: None,
                occurred_at: observed_at,
                raw_answer: Some("HELDOUT_ANSWER_CANARY".into()),
                citations: vec!["https://example.org/heldout".into()],
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("web-test-v1".into()),
                runner_evidence: vec![],
                fixture: false,
            },
            observed_at,
        )
        .await
        .unwrap();
    let hidden = read_agent_plan(
        &state,
        &scope,
        MeasurementPlanReadRequest { plan_id: bound_id },
    )
    .await
    .unwrap();
    let hidden_json = serde_json::to_string(&hidden).unwrap();
    assert!(!hidden_json.contains("HELDOUT"));
    assert!(!hidden_json.contains("example.org/heldout"));
    assert_eq!(
        hidden.targets[0].outcome_status,
        Some(ChannelOutcomeStatus::Observed)
    );
    assert!(!hidden.targets[0].answer_available);
    let changed = MeasurementPlanCreateRequest {
        question: "Different question?".into(),
        ..command.clone()
    };
    assert_eq!(
        create_agent_plan(&state, &scope, changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(stub.closed.load(Ordering::SeqCst), 2);
    stub.mismatch.store(true, Ordering::SeqCst);
    assert_eq!(
        discover(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(stub.closed.load(Ordering::SeqCst), 3);
    stub.mismatch.store(false, Ordering::SeqCst);
    stub.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        discover(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::CapabilityMissing
    );
    assert_eq!(stub.closed.load(Ordering::SeqCst), 4);
    let failed = MeasurementPlanCreateRequest {
        idempotency_key: "unavailable-menu".into(),
        ..command.clone()
    };
    assert_eq!(
        create_agent_plan(&state, &scope, failed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::CapabilityMissing
    );
    assert_eq!(
        state
            .channel_job_repository()
            .list_measurement_plans(&scope, None, 100)
            .await
            .unwrap()
            .len(),
        2,
        "unavailable menu must not create an accepted plan"
    );
    assert_eq!(stub.closed.load(Ordering::SeqCst), 5);
    state
        .channel_service()
        .repository
        .delete_account(&scope, account_id)
        .await
        .unwrap();
    assert_eq!(
        create_agent_plan(&state, &scope, command).await.unwrap(),
        accepted,
        "a durable replay remains available after account loss"
    );
    assert_eq!(stub.closed.load(Ordering::SeqCst), 5);
    let other = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(ProjectId::new(Uuid::new_v4())),
    );
    assert_eq!(
        discover(&state, &other, account_id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        read_agent_plan(
            &state,
            &other,
            MeasurementPlanReadRequest {
                plan_id: accepted.plan_id,
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
    assert_eq!(stub.closed.load(Ordering::SeqCst), 5);
    server.abort();
}
