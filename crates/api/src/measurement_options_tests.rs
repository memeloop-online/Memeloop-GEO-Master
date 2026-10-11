//! Synthetic browser bridge coverage, not external website acceptance.
use crate::{
    AppState, ChannelService,
    browser_bridge::BrowserBridge,
    measurement_options::discover,
    standalone_measurements::{create_agent_plan, read_agent_plan},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{delete, get, post},
};
use chrono::Utc;
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOutcome, ChannelOutcomeStatus, ChannelOwnerKind,
    ChannelRepository, ChannelSecret, ChannelStatus, ChannelTargetInput, DEVELOPMENT_OPERATOR_ID,
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
    measure_installed: Arc<AtomicBool>,
    discovery_installed: Arc<AtomicBool>,
    mismatch: Arc<AtomicBool>,
    unavailable: Arc<AtomicBool>,
    closed: Arc<AtomicUsize>,
    starts: Arc<tokio::sync::Mutex<Vec<Value>>>,
    started_ids: Arc<tokio::sync::Mutex<Vec<Uuid>>>,
    closed_ids: Arc<tokio::sync::Mutex<Vec<Uuid>>>,
    options_calls: Arc<AtomicUsize>,
    start_stalled: Arc<AtomicBool>,
    complete_paused: Arc<AtomicBool>,
    complete_entered: Arc<tokio::sync::Notify>,
    complete_continue: Arc<tokio::sync::Notify>,
    close_stalled: Arc<AtomicBool>,
    close_failed: Arc<AtomicBool>,
    close_absent: Arc<AtomicBool>,
    renewal_race: Arc<tokio::sync::Mutex<Option<RenewalRace>>>,
    capability_override: Arc<tokio::sync::Mutex<Option<Value>>>,
}

type RenewalRace = (Arc<dyn ChannelRepository>, TenantScope, Uuid, ChannelSecret);

async fn capabilities(State(stub): State<Stub>) -> Json<Value> {
    if let Some(value) = stub.capability_override.lock().await.clone() {
        return Json(value);
    }
    let operations = if stub.measure_installed.load(Ordering::SeqCst) {
        vec!["measure"]
    } else {
        vec![]
    };
    Json(
        json!({"connectors": (["kimi", "doubao", "deepseek", "glm"].map(|provider| json!({
            "platform":provider,"placement_slot":"primary","connector_version":"synthetic.v1",
            "operations":operations,"verified":false,"login_entry_available":true,"login_supported":true,
            "model_discovery_supported":stub.discovery_installed.load(Ordering::SeqCst)
        })))}),
    )
}

async fn start(State(stub): State<Stub>, Json(input): Json<Value>) -> Json<Value> {
    stub.starts
        .lock()
        .await
        .push(input["storage_state"].clone());
    stub.started_ids
        .lock()
        .await
        .push(serde_json::from_value(input["session_id"].clone()).unwrap());
    if stub.start_stalled.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    Json(json!({"session_id":input["session_id"]}))
}
async fn complete(State(stub): State<Stub>) -> Json<Value> {
    stub.complete_entered.notify_one();
    if stub.complete_paused.load(Ordering::SeqCst) {
        stub.complete_continue.notified().await;
    }
    if let Some((repository, scope, account_id, replacement)) =
        stub.renewal_race.lock().await.take()
    {
        let mut record = repository.get_account(&scope, account_id).await.unwrap();
        record.session = Some(replacement);
        repository.save_account(&scope, record).await.unwrap();
    }
    Json(json!({"identity":{
        "platform_account_id":if stub.mismatch.load(Ordering::SeqCst) { "changed" } else { "observed-account" },
        "display_name":"Fixture", "avatar_url":null
    },"storage_state":{"private":"not-returned"}}))
}
async fn options(State(stub): State<Stub>) -> (http::StatusCode, Json<Value>) {
    stub.options_calls.fetch_add(1, Ordering::SeqCst);
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
async fn close(
    State(stub): State<Stub>,
    Path(session_id): Path<Uuid>,
) -> (http::StatusCode, Json<Value>) {
    stub.closed.fetch_add(1, Ordering::SeqCst);
    stub.closed_ids.lock().await.push(session_id);
    if stub.close_stalled.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    (
        if stub.close_failed.load(Ordering::SeqCst) {
            http::StatusCode::SERVICE_UNAVAILABLE
        } else if stub.close_absent.load(Ordering::SeqCst) {
            http::StatusCode::NOT_FOUND
        } else {
            http::StatusCode::OK
        },
        Json(json!({"closed":true})),
    )
}

#[tokio::test]
async fn discovery_restores_identity_returns_only_models_and_always_closes() {
    discovery_for_provider("kimi").await;
}

#[tokio::test]
async fn registered_providers_require_discovery_adapter_and_keep_scoped_lifecycle() {
    for provider in ["doubao", "deepseek", "glm"] {
        discovery_for_provider(provider).await;
    }
}

async fn discovery_for_provider(provider: &str) {
    let stub = Stub::default();
    let app = Router::new()
        .route("/v1/capabilities", get(capabilities))
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
                    platform: provider.into(),
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
    stub.measure_installed.store(true, Ordering::SeqCst);
    assert_eq!(
        discover(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::CapabilityMissing
    );
    assert!(
        stub.starts.lock().await.is_empty(),
        "measurement alone must not authorize model discovery"
    );
    stub.measure_installed.store(false, Ordering::SeqCst);
    stub.discovery_installed.store(true, Ordering::SeqCst);
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
    stub.measure_installed.store(true, Ordering::SeqCst);
    let accepted = create_agent_plan(&state, &scope, command.clone())
        .await
        .unwrap();
    assert_eq!(accepted.state, "accepted");
    assert_eq!(accepted.model, "observed-model");
    let persisted = state
        .channel_job_repository()
        .get_target(&scope, accepted.target_id)
        .await
        .unwrap();
    assert!(
        matches!(&persisted.target.input, ChannelTargetInput::Measure { provider: saved, .. } if saved == provider)
    );
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

async fn boundary_fixture(
    stub: &Stub,
) -> (AppState, TenantScope, Uuid, tokio::task::JoinHandle<()>) {
    stub.discovery_installed.store(true, Ordering::SeqCst);
    let app = Router::new()
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/sessions", post(start))
        .route("/v1/sessions/{id}/complete", post(complete))
        .route("/v1/sessions/{id}/measurement-options", get(options))
        .route("/v1/sessions/{id}", delete(close))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let service = ChannelService::persistent(
        Arc::new(geo_domain::MemoryChannelRepository::default()),
        &"17".repeat(32),
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
                display_name: "Model inspection".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap()
        .id;
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project_id));
    let account_id = Uuid::new_v4();
    let session = synthetic_session(&scope, account_id, br#"{"cookies":[],"origins":[]}"#);
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
                    platform: "deepseek".into(),
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
                session: Some(session),
                proxy: None,
            },
        )
        .await
        .unwrap();
    (state, scope, account_id, server)
}

fn synthetic_session(scope: &TenantScope, account_id: Uuid, bytes: &[u8]) -> ChannelSecret {
    let aad = format!(
        "geo-channel-v1:{}:{}:{}:{}:session",
        scope.operator_id,
        scope.tenant_id,
        scope.project_id.unwrap().as_uuid(),
        account_id
    );
    ChannelSecret::new(
        SecretEnvelope::from_hex_key(&"17".repeat(32))
            .unwrap()
            .seal(aad.as_bytes(), bytes)
            .unwrap(),
    )
}

async fn reserve(
    state: &AppState,
    scope: &TenantScope,
    account_id: Uuid,
) -> Result<Uuid, geo_domain::AppError> {
    let now = Utc::now();
    let reservation = Uuid::new_v4();
    state
        .channel_job_repository()
        .reserve_account(
            scope,
            account_id,
            reservation,
            now,
            now + geo_domain::CHANNEL_EXECUTION_LEASE,
        )
        .await?;
    Ok(reservation)
}

#[tokio::test]
async fn discovery_reservation_contends_across_projects_and_releases_after_confirmed_close() {
    let stub = Stub::default();
    let (state, scope, account_id, server) = boundary_fixture(&stub).await;
    let other = TenantScope::new(
        scope.operator_id,
        scope.tenant_id,
        Some(ProjectId::new(Uuid::new_v4())),
    );
    let owner = reserve(&state, &other, account_id).await.unwrap();
    assert_eq!(
        discover(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert!(stub.starts.lock().await.is_empty());
    state
        .channel_job_repository()
        .release_account(&other, account_id, owner)
        .await
        .unwrap();
    discover(&state, &scope, account_id).await.unwrap();
    assert_eq!(stub.closed.load(Ordering::SeqCst), 1);
    reserve(&state, &other, account_id).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn concurrent_discovery_cannot_open_a_second_context_for_the_account() {
    let stub = Stub::default();
    stub.complete_paused.store(true, Ordering::SeqCst);
    let (state, scope, account_id, server) = boundary_fixture(&stub).await;
    let first_state = state.clone();
    let first_scope = scope.clone();
    let first = tokio::spawn(async move { discover(&first_state, &first_scope, account_id).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stub.complete_entered.notified(),
    )
    .await
    .unwrap();
    assert_eq!(
        discover(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(stub.starts.lock().await.len(), 1);
    stub.complete_continue.notify_one();
    first.await.unwrap().unwrap();
    reserve(&state, &scope, account_id).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn credential_cas_loss_rejects_before_reading_model_options() {
    let stub = Stub::default();
    let (state, scope, account_id, server) = boundary_fixture(&stub).await;
    let replacement = synthetic_session(
        &scope,
        account_id,
        br#"{"cookies":[],"origins":[],"renewed":true}"#,
    );
    *stub.renewal_race.lock().await = Some((
        Arc::clone(&state.channel_service().repository),
        scope.clone(),
        account_id,
        replacement.clone(),
    ));
    assert_eq!(
        discover(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(stub.options_calls.load(Ordering::SeqCst), 0);
    assert_eq!(stub.closed.load(Ordering::SeqCst), 1);
    let saved = state
        .channel_service()
        .repository
        .get_account(&scope, account_id)
        .await
        .unwrap();
    assert_eq!(
        saved.session.unwrap().encrypted_bytes(),
        replacement.encrypted_bytes()
    );
    reserve(&state, &scope, account_id).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn uncertain_start_closes_caller_id_but_retains_account_until_lease_expiry() {
    for absent in [false, true] {
        let stub = Stub::default();
        stub.start_stalled.store(true, Ordering::SeqCst);
        stub.close_absent.store(absent, Ordering::SeqCst);
        let (state, scope, account_id, server) = boundary_fixture(&stub).await;
        let result = super::discover_with_timeouts(
            &state,
            &scope,
            account_id,
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(50),
        )
        .await;
        assert_eq!(result.unwrap_err().code, ErrorCode::DependencyUnavailable);
        assert_eq!(
            stub.started_ids.lock().await.as_slice(),
            stub.closed_ids.lock().await.as_slice()
        );
        assert_eq!(stub.closed.load(Ordering::SeqCst), 1);
        assert_eq!(stub.options_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            reserve(&state, &scope, account_id).await.unwrap_err().code,
            ErrorCode::Conflict
        );
        let after_expiry = Utc::now() + geo_domain::CHANNEL_EXECUTION_LEASE;
        state
            .channel_job_repository()
            .reserve_account(
                &scope,
                account_id,
                Uuid::new_v4(),
                after_expiry,
                after_expiry + geo_domain::CHANNEL_EXECUTION_LEASE,
            )
            .await
            .unwrap();
        server.abort();
    }
}

#[tokio::test]
async fn failed_or_stalled_close_never_returns_options_or_releases_the_account() {
    for stalled in [false, true] {
        let stub = Stub::default();
        stub.close_failed.store(!stalled, Ordering::SeqCst);
        stub.close_stalled.store(stalled, Ordering::SeqCst);
        let (state, scope, account_id, server) = boundary_fixture(&stub).await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::discover_with_timeouts(
                &state,
                &scope,
                account_id,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_millis(50),
            ),
        )
        .await
        .expect("close must have its own bounded deadline");
        assert_eq!(result.unwrap_err().code, ErrorCode::DependencyUnavailable);
        assert_eq!(stub.options_calls.load(Ordering::SeqCst), 1);
        assert_eq!(stub.closed.load(Ordering::SeqCst), 1);
        assert_eq!(
            reserve(&state, &scope, account_id).await.unwrap_err().code,
            ErrorCode::Conflict
        );
        server.abort();
    }
}

#[tokio::test]
async fn inspection_error_is_not_replaced_by_a_close_failure() {
    let stub = Stub::default();
    stub.mismatch.store(true, Ordering::SeqCst);
    stub.close_failed.store(true, Ordering::SeqCst);
    let (state, scope, account_id, server) = boundary_fixture(&stub).await;
    assert_eq!(
        discover(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(stub.options_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        reserve(&state, &scope, account_id).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    server.abort();
}

#[tokio::test]
async fn total_inspection_deadline_includes_identity_and_confirmed_close_releases() {
    let stub = Stub::default();
    stub.complete_paused.store(true, Ordering::SeqCst);
    let (state, scope, account_id, server) = boundary_fixture(&stub).await;
    let result = super::discover_with_timeouts(
        &state,
        &scope,
        account_id,
        std::time::Duration::from_millis(100),
        std::time::Duration::from_millis(50),
    )
    .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::DependencyUnavailable);
    assert_eq!(stub.starts.lock().await.len(), 1);
    assert_eq!(stub.options_calls.load(Ordering::SeqCst), 0);
    assert_eq!(stub.closed.load(Ordering::SeqCst), 1);
    reserve(&state, &scope, account_id).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn an_inspector_does_not_grant_measurement_permission() {
    let stub = Stub::default();
    let (state, scope, account_id, server) = boundary_fixture(&stub).await;
    discover(&state, &scope, account_id).await.unwrap();
    assert_eq!(
        super::require_installed_measurement(&state, "deepseek")
            .await
            .unwrap_err()
            .code,
        ErrorCode::CapabilityMissing
    );
    assert_eq!(
        create_agent_plan(
            &state,
            &scope,
            MeasurementPlanCreateRequest {
                account_id,
                question: "How do rain gauges work?".into(),
                idempotency_key: "inspector-only".into(),
                model: None,
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::CapabilityMissing
    );
    assert!(
        state
            .channel_job_repository()
            .list_measurement_plans(&scope, None, 10)
            .await
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn revoked_pool_assignment_loses_renewal_cas_before_model_inspection() {
    let stub = Stub::default();
    stub.complete_paused.store(true, Ordering::SeqCst);
    let (state, scope, customer_id, server) = boundary_fixture(&stub).await;
    // This case uses only an operator-owned account, not a duplicate customer
    // identity in the same repository.
    state
        .channel_service()
        .repository
        .delete_account(&scope, customer_id)
        .await
        .unwrap();
    let pool_tenant = geo_domain::TenantId::new(Uuid::new_v4());
    let service = state
        .channel_service()
        .clone()
        .with_operator_pool_tenant_id(pool_tenant);
    let state = state.with_channel_service(service);
    let pool_id = Uuid::new_v4();
    let aad = format!(
        "geo-channel-v1:{}:{}:{}:{}:pool_session",
        scope.operator_id,
        pool_tenant,
        Uuid::nil(),
        pool_id
    );
    let encrypted = SecretEnvelope::from_hex_key(&"17".repeat(32))
        .unwrap()
        .seal(aad.as_bytes(), br#"{"cookies":[],"origins":[]}"#)
        .unwrap();
    let repository = Arc::clone(&state.channel_service().repository);
    repository
        .save_pool_account(
            scope.operator_id,
            geo_domain::PoolAccountRecord {
                account: geo_domain::PoolAccount {
                    account_id: pool_id,
                    platform: "deepseek".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: Some("Fixture pool".into()),
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
    repository
        .assign_pool_account(&scope, pool_id, true)
        .await
        .unwrap();
    let first_state = state.clone();
    let first_scope = scope.clone();
    let first = tokio::spawn(async move { discover(&first_state, &first_scope, pool_id).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stub.complete_entered.notified(),
    )
    .await
    .unwrap();
    repository
        .assign_pool_account(&scope, pool_id, false)
        .await
        .unwrap();
    stub.complete_continue.notify_one();
    assert_eq!(first.await.unwrap().unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(stub.options_calls.load(Ordering::SeqCst), 0);
    assert_eq!(stub.closed.load(Ordering::SeqCst), 1);
    assert_eq!(
        discover(&state, &scope, pool_id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(stub.starts.lock().await.len(), 1);
    reserve(&state, &scope, pool_id).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn old_ambiguous_or_unversioned_advertisements_do_not_grant_discovery() {
    let stub = Stub::default();
    let (state, scope, account_id, server) = boundary_fixture(&stub).await;
    let original = json!({
        "platform":"deepseek","placement_slot":"primary","connector_version":"synthetic.v1",
        "operations":["measure"],"verified":false,"model_discovery_supported":true
    });
    let mut missing = original.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("model_discovery_supported");
    let mut unversioned = original.clone();
    unversioned["connector_version"] = json!(" ");
    let mut secondary = original.clone();
    secondary["placement_slot"] = json!("secondary");
    for connectors in [
        json!([missing]),
        json!([unversioned]),
        json!([secondary]),
        json!([original.clone(), original]),
    ] {
        *stub.capability_override.lock().await = Some(json!({"connectors":connectors}));
        assert_eq!(
            discover(&state, &scope, account_id).await.unwrap_err().code,
            ErrorCode::CapabilityMissing
        );
    }
    assert!(stub.starts.lock().await.is_empty());
    reserve(&state, &scope, account_id).await.unwrap();
    server.abort();
}
