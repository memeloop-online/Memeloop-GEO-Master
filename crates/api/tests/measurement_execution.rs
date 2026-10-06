//! Synthetic contract tests, not real-account official-search acceptance.
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{Method, StatusCode, Uri},
    routing::any,
};
use chrono::{Duration, Utc};
use geo_api::{
    AppState, BrowserBridge, ChannelDispatchResult, ChannelService, EventBus,
    MemoryIdempotencyStore, MemoryOperationStore, execute_channel_target,
};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOutcomeStatus, ChannelOwnerKind, ChannelPlan,
    ChannelSecret, ChannelStatus, ChannelTarget, ChannelTargetInput, DEVELOPMENT_OPERATOR_ID,
    DEVELOPMENT_TENANT_ID, MemoryAuthRepository, MemoryChannelRepository, MemoryProjectRepository,
    ProjectCreate, ProjectSettings, TenantScope, sha256_hex,
};
use geo_provider::SecretEnvelope;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
enum Receipt {
    Valid,
    Refusal,
    Mismatch(&'static str),
    Fixture,
    NoSearchEvent,
    AnswerAlone,
    Unsupported,
    DuplicateProof,
    AuxiliaryEvidence,
    FutureTimestamp,
    AttestedVersion,
    RunnerFixtureProofLive,
    MissingProvenance,
    InvalidProvenance,
    ForgedMarker,
    MismatchedExecution,
    Unknown,
    Connect,
    ConnectMismatch(&'static str),
    ConnectWrongSchema,
    ConnectFuture,
    ConnectFixture,
}

#[derive(Clone)]
struct Runner(Receipt);

async fn runner(
    State(Runner(case)): State<Runner>,
    method: Method,
    uri: Uri,
    payload: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    let input = payload.map(|Json(value)| value).unwrap_or(Value::Null);
    let response = match (method.as_str(), uri.path()) {
        ("POST", "/v1/sessions") => json!({"session_id": input["session_id"]}),
        ("POST", "/v1/executions") => {
            let frozen = &input["payload"];
            let completed_at = Utc::now();
            let target_id = frozen["target_id"].clone();
            let mut proof = json!({
                "kind":"official_search_observation",
                "schema_version":"geo.measure.official_search.v1",
                "target_id":target_id,
                "account_id":frozen["account_id"],
                "provider":frozen["provider"],
                "model":frozen["model"],
                "surface":frozen["surface"],
                "search_mode":frozen["search_mode"],
                "protocol_version":frozen["protocol_version"],
                "question_set_version":frozen["question_set_version"],
                "question_sha256":sha256_hex(frozen["question"].as_str().unwrap().as_bytes()),
                "market":frozen["market"],
                "language":frozen["language"],
                "scheduled_at":frozen["scheduled_at"],
                "sample_ordinal":frozen["sample_ordinal"],
                "connector_version":"official_search_verified.v1",
                "provenance":"live",
                "disposition":"observed",
                "raw_answer":"Original answer",
                "citations":["http://example.org/source#section"],
                "search_event":{
                    "kind":"official_search_event","source":"provider_search_event",
                    "provenance":"live","event_id":"event-1",
                    "request_id":"request-1","occurred_at":completed_at
                }
            });
            // The runner must echo the server-frozen sample identity.
            proof["target_id"] = frozen["target_id"].clone();
            proof["account_id"] = frozen["account_id"].clone();
            if matches!(
                case,
                Receipt::Connect
                    | Receipt::ConnectMismatch(_)
                    | Receipt::ConnectWrongSchema
                    | Receipt::ConnectFuture
                    | Receipt::ConnectFixture
            ) {
                proof["schema_version"] = json!("geo.measure.official_search.v2");
                proof["search_event"] = json!({
                    "kind":"official_search_event",
                    "source":"provider_connect_stream",
                    "provenance":"live",
                    "chat_id":"chat-1",
                    "message_id":"message-1",
                    "block_id":"block-1",
                    "event_offset":"12",
                    "observed_at":completed_at,
                    "request_model":frozen["model"],
                    "request_question_sha256":proof["question_sha256"]
                });
                if let Receipt::ConnectMismatch(field) = case {
                    proof["search_event"][field] = json!("invalid value");
                }
                if matches!(case, Receipt::ConnectWrongSchema) {
                    proof["schema_version"] = json!("geo.measure.official_search.v1");
                }
                if matches!(case, Receipt::ConnectFuture) {
                    proof["search_event"]["observed_at"] = json!(completed_at + Duration::days(1));
                }
            }
            if let Receipt::Mismatch(field) = case {
                proof[field] = json!("changed");
            }
            if matches!(case, Receipt::Fixture) {
                proof["provenance"] = json!("fixture");
            }
            if matches!(case, Receipt::NoSearchEvent) {
                proof.as_object_mut().unwrap().remove("search_event");
            }
            if matches!(case, Receipt::Refusal) {
                proof["disposition"] = json!("refused");
                proof["raw_answer"] = json!("Cannot answer this request");
                proof["citations"] = json!([]);
            }
            if matches!(case, Receipt::FutureTimestamp) {
                proof["search_event"]["occurred_at"] = json!(completed_at + Duration::days(1));
            }
            if matches!(case, Receipt::AttestedVersion) {
                proof["connector_version"] = json!("attested-search.v2");
            }
            let evidence = if matches!(case, Receipt::AnswerAlone | Receipt::Unsupported) {
                vec![]
            } else if matches!(case, Receipt::DuplicateProof) {
                vec![proof.clone(), proof]
            } else if matches!(case, Receipt::ForgedMarker) {
                vec![proof, json!({"kind":"runner_receipt","provenance":"live"})]
            } else if matches!(case, Receipt::AuxiliaryEvidence) {
                vec![
                    json!({"kind":"screenshot_metadata","reference":"redacted"}),
                    proof,
                ]
            } else {
                vec![proof]
            };
            // Synthetic protocol fixture: "live" exercises consumer gating,
            // not a production search or real-world acceptance result.
            let mut receipt = json!({
                "execution_id":if matches!(case, Receipt::MismatchedExecution) {json!(Uuid::new_v4())} else {input["execution_id"].clone()},
                "status":if matches!(case, Receipt::Unsupported) {"unsupported"} else if matches!(case, Receipt::Unknown) {"unknown"} else {"completed"},
                "stage":"official_search_observation",
                "occurred_at":completed_at,
                "connector_version":if matches!(case, Receipt::Fixture) {"fixture.v1"} else if matches!(case, Receipt::AttestedVersion) {"attested-search.v2"} else {"official_search_verified.v1"},
                "provenance":if matches!(case, Receipt::Fixture | Receipt::RunnerFixtureProofLive | Receipt::ConnectFixture) {"fixture"} else if matches!(case, Receipt::InvalidProvenance) {"untrusted"} else {"live"},
                "evidence":evidence,
            });
            if matches!(case, Receipt::MissingProvenance) {
                receipt.as_object_mut().unwrap().remove("provenance");
            }
            receipt
        }
        ("POST", path) if path.ends_with("/complete") => json!({
            "identity":{"platform_account_id":"verified","display_name":"Verified"},
            "storage_state":{"cookies":[],"origins":[]}
        }),
        _ => json!({"closed":true}),
    };
    (StatusCode::OK, Json(response))
}

async fn run(case: Receipt) -> geo_domain::ChannelOutcome {
    run_with_plan(case, false).await
}

async fn run_with_plan(case: Receipt, standalone: bool) -> geo_domain::ChannelOutcome {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(runner)).with_state(Runner(case)),
        )
        .await
        .unwrap();
    });
    let channels = Arc::new(MemoryChannelRepository::default());
    let key = "b7".repeat(32);
    let bridge =
        BrowserBridge::new(format!("http://{address}"), "private-test-token".into()).unwrap();
    let service = ChannelService::persistent(channels, &key, Some(bridge)).unwrap();
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        Arc::new(MemoryAuthRepository::development_with_password("test")),
        Arc::new(MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    )
    .with_channel_service(service);
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Sample".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let account_id = Uuid::new_v4();
    let aad = format!(
        "geo-channel-v1:{}:{}:{}:{}:session",
        scope.operator_id, scope.tenant_id, project.id, account_id
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
                    project_id: project.id,
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "kimi".into(),
                    group_id: None,
                    status: ChannelStatus::Ready,
                    display_name: None,
                    platform_account_id: Some("verified".into()),
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
    let target_id = Uuid::new_v4();
    let scheduled_at = Utc::now() - Duration::minutes(1);
    let repo = state.channel_job_repository();
    let plan = ChannelPlan {
        plan_id: Uuid::new_v4(),
        project_id: project.id,
        cycle_id: Uuid::new_v4(),
        input_hash: target_id.to_string(),
        revision: 1,
        created_at: Utc::now(),
        targets: vec![ChannelTarget {
            target_id,
            input: ChannelTargetInput::Measure {
                account_id,
                provider: "kimi".into(),
                model: "frozen-model".into(),
                surface: "consumer_web".into(),
                search_mode: "web_search".into(),
                protocol_version: "protocol-v1".into(),
                question_set_version: "questions-v1".into(),
                question: "Frozen question?".into(),
                market: "CN".into(),
                language: "en".into(),
                scheduled_at,
                sample_ordinal: 3,
                question_binding: None,
            },
        }],
    };
    if standalone {
        repo.create_measurement_plan(
            &scope,
            "standalone-fixture",
            &plan.input_hash,
            geo_domain::StandaloneMeasurementPlan {
                plan_id: plan.plan_id,
                project_id: plan.project_id,
                title: "Independent topic".into(),
                input_hash: plan.input_hash.clone(),
                revision: 1,
                created_at: plan.created_at,
                targets: plan.targets,
            },
        )
        .await
        .unwrap();
        assert!(
            state
                .project_repository()
                .get(&tenant, project.id)
                .await
                .unwrap()
                .unwrap()
                .current_cycle_id
                .is_none()
        );
        assert_eq!(
            repo.scan_pending(None, Utc::now(), 100)
                .await
                .unwrap()
                .len(),
            1
        );
    } else {
        repo.create_plan(&scope, plan).await.unwrap();
    }
    let ChannelDispatchResult::Executed(view) = execute_channel_target(&state, &scope, target_id)
        .await
        .unwrap()
    else {
        panic!("unexpected deferral")
    };
    let outcome = view.attempts[0].outcome.clone().unwrap();
    assert_eq!(
        repo.get_target(&scope, target_id)
            .await
            .unwrap()
            .attempts
            .len(),
        1
    );
    server.abort();
    outcome
}

#[tokio::test]
async fn standalone_due_target_reuses_execution_and_does_not_promote_fixture_to_observed() {
    let outcome = run_with_plan(Receipt::Fixture, true).await;
    assert_eq!(outcome.status, ChannelOutcomeStatus::Missing);
    assert!(outcome.fixture);
}

#[tokio::test]
async fn verified_search_retains_original_answer_and_citations() {
    let outcome = run(Receipt::Valid).await;
    assert_eq!(outcome.status, ChannelOutcomeStatus::Observed);
    assert_eq!(outcome.raw_answer.as_deref(), Some("Original answer"));
    assert_eq!(outcome.citations, ["http://example.org/source#section"]);
    assert_eq!(outcome.runner_evidence.len(), 2);
    assert!(!outcome.fixture);
    assert_eq!(outcome.runner_evidence[1]["kind"], "runner_receipt");
    assert_eq!(outcome.runner_evidence[1]["provenance"], "live");
    assert_eq!(
        outcome.runner_evidence[1]["connector_version"],
        outcome.connector_version.as_deref().unwrap()
    );
    assert!(outcome.runner_evidence[1]["execution_id"].is_string());
    assert!(outcome.runner_evidence[1]["occurred_at"].is_string());
    let with_metadata = run(Receipt::AuxiliaryEvidence).await;
    assert_eq!(with_metadata.status, ChannelOutcomeStatus::Observed);
    assert_eq!(with_metadata.runner_evidence.len(), 3);
    assert_eq!(
        run(Receipt::AttestedVersion).await.status,
        ChannelOutcomeStatus::Observed
    );
    assert_eq!(
        run(Receipt::Refusal).await.status,
        ChannelOutcomeStatus::Refused
    );
}

#[tokio::test]
async fn connect_search_uses_actual_stream_correlation_without_synthetic_request_ids() {
    let outcome = run(Receipt::Connect).await;
    assert_eq!(outcome.status, ChannelOutcomeStatus::Observed);
    assert_eq!(outcome.raw_answer.as_deref(), Some("Original answer"));
    let event = &outcome.runner_evidence[0]["search_event"];
    assert_eq!(event["chat_id"], "chat-1");
    assert_eq!(event["message_id"], "message-1");
    assert_eq!(event["block_id"], "block-1");
    assert_eq!(event["event_offset"], "12");
    assert!(event.get("request_id").is_none());
    assert!(event.get("event_id").is_none());
    for field in [
        "kind",
        "source",
        "provenance",
        "chat_id",
        "message_id",
        "block_id",
        "event_offset",
        "observed_at",
        "request_model",
        "request_question_sha256",
    ] {
        assert_eq!(
            run(Receipt::ConnectMismatch(field)).await.status,
            ChannelOutcomeStatus::Missing,
            "field {field}"
        );
    }
    for case in [
        Receipt::ConnectWrongSchema,
        Receipt::ConnectFuture,
        Receipt::ConnectFixture,
    ] {
        let outcome = run(case).await;
        assert_eq!(outcome.status, ChannelOutcomeStatus::Missing, "{case:?}");
        assert!(outcome.raw_answer.is_none());
    }
}

#[tokio::test]
async fn mismatched_frozen_sample_never_becomes_an_observation() {
    for field in [
        "question_sha256",
        "model",
        "protocol_version",
        "target_id",
        "sample_ordinal",
        "question_set_version",
        "search_mode",
        "surface",
        "provider",
    ] {
        let outcome = run(Receipt::Mismatch(field)).await;
        assert_eq!(
            outcome.status,
            ChannelOutcomeStatus::Missing,
            "field {field}"
        );
        assert!(outcome.raw_answer.is_none(), "field {field}");
    }
}

#[tokio::test]
async fn fixtures_and_plausible_answers_without_official_search_are_missing() {
    for case in [
        Receipt::Fixture,
        Receipt::NoSearchEvent,
        Receipt::AnswerAlone,
        Receipt::DuplicateProof,
        Receipt::FutureTimestamp,
        Receipt::RunnerFixtureProofLive,
        Receipt::MissingProvenance,
        Receipt::InvalidProvenance,
        Receipt::ForgedMarker,
        Receipt::MismatchedExecution,
    ] {
        let outcome = run(case).await;
        assert_eq!(outcome.status, ChannelOutcomeStatus::Missing, "{case:?}");
        assert!(outcome.raw_answer.is_none());
        if matches!(
            case,
            Receipt::Fixture
                | Receipt::RunnerFixtureProofLive
                | Receipt::MissingProvenance
                | Receipt::InvalidProvenance
                | Receipt::ForgedMarker
                | Receipt::MismatchedExecution
        ) {
            assert!(outcome.fixture);
        }
        assert!(
            outcome
                .runner_evidence
                .iter()
                .filter(|proof| proof["kind"] == "runner_receipt")
                .count()
                <= 1
        );
    }
    assert_eq!(
        run(Receipt::Unsupported).await.status,
        ChannelOutcomeStatus::Unsupported
    );
    let unknown = run(Receipt::Unknown).await;
    assert_eq!(unknown.status, ChannelOutcomeStatus::Unknown);
    assert!(unknown.raw_answer.is_none());
}
