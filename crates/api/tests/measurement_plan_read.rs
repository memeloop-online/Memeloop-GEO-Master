//! Exact-plan saved-evidence projection; all identities and observations are synthetic.
use std::sync::Arc;

use chrono::{Duration, Utc};
use geo_api::{AppState, ObservationEvidenceResolver, RepositoryHostOps, create_measurement_plan};
use geo_domain::*;
use geo_worker::{HostOps, MeasurementPlanReadRequest, MeasurementPlanStatus};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn durable_state_exposes_saved_evidence_before_runtime_snapshot_or_callbacks() {
    // Lazy construction opens no connection and needs no deployment credentials.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://synthetic:synthetic@127.0.0.1:1/synthetic")
        .unwrap();
    let database = geo_persistence::Database::from_pool(pool);
    let state = AppState::from_database_with_parser_profiles(&database, None, None);
    let runtime_snapshot = state.clone();
    assert!(state.observation_evidence_resolver().is_some());
    assert!(runtime_snapshot.observation_evidence_resolver().is_some());
}

async fn project(state: &AppState) -> TenantScope {
    let base = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &base,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic measurement".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    TenantScope::new(base.operator_id, base.tenant_id, Some(project.id))
}

async fn ui_plan(state: &AppState, scope: &TenantScope) -> StandaloneMeasurementPlan {
    let account_id = Uuid::new_v4();
    let at = Utc::now() - Duration::minutes(2);
    state
        .channel_service()
        .repository
        .save_account(
            scope,
            ChannelAccountRecord {
                account: ChannelAccount {
                    account_id,
                    project_id: scope.project_id.unwrap(),
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: "kimi".into(),
                    group_id: None,
                    status: ChannelStatus::NeedsLogin,
                    display_name: None,
                    platform_account_id: None,
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: false,
                    proxy_server: None,
                    created_at: at,
                    updated_at: at,
                },
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
    create_measurement_plan(
        state,
        scope,
        serde_json::from_value(json!({
            "idempotency_key":Uuid::new_v4(),"title":"Which rain gauge should I choose?",
            "measurements":[{
                "account_id":account_id,"provider":"kimi","model":"synthetic-model",
                "surface":"consumer_web","search_mode":"web_search","protocol_version":"v1",
                "question_set_version":"ad_hoc.v1","question":"Which rain gauge should I choose?",
                "market":"CN","language":"zh-CN","scheduled_at":at,"sample_ordinal":0
            }]
        }))
        .unwrap(),
    )
    .await
    .unwrap()
}

async fn read(
    state: &AppState,
    scope: &TenantScope,
    plan: &StandaloneMeasurementPlan,
) -> MeasurementPlanStatus {
    let request = MeasurementPlanReadRequest {
        plan_id: plan.plan_id,
    };
    let result = RepositoryHostOps::new(state.knowledge_repository())
        .with_content(state.clone())
        .measurement_plan_read(scope, request.clone())
        .await
        .unwrap();
    result.validate_for(&request).unwrap();
    result
}

async fn saved_analysis(
    state: AppState,
    scope: &TenantScope,
    plan: &StandaloneMeasurementPlan,
) -> (AppState, Uuid) {
    let jobs = state.channel_job_repository();
    let captures = Arc::new(MemoryObservationCaptureRepository::new(jobs.clone()));
    let gate = MemoryObservationAdmissionGate::default();
    let analyses = Arc::new(
        MemoryObservationAnalysisRepository::new(jobs.clone(), captures.clone())
            .with_admission_gate(gate.clone()),
    );
    let extraction = MemoryObservationExtractionRepository::new(captures.clone())
        .with_automatic_admission(gate, analyses.clone());
    let state = state
        .with_observation_evidence_resolver(ObservationEvidenceResolver::new(analyses, captures));
    let target = &plan.targets[0];
    let at = Utc::now() - Duration::minutes(1);
    let attempt_id = Uuid::new_v4();
    jobs.claim(scope, target.target_id, attempt_id, at)
        .await
        .unwrap();
    let source =
        json!({"messages":[{"role":"assistant","text":"A synthetic saved answer."}]}).to_string();
    let digest = sha256_hex(source.as_bytes());
    jobs.finish(
        scope,
        target.target_id,
        attempt_id,
        ChannelOutcome {
            status: ChannelOutcomeStatus::Unknown,
            detail: None,
            occurred_at: at,
            raw_answer: None,
            citations: vec![],
            public_url: None,
            screenshot_ref: None,
            connector_version: None,
            fixture: false,
            runner_evidence: vec![json!({
                "kind":"observation_capture","schema_version":"geo.observation.capture.v1",
                "phase":"source","source_json":source,"source_sha256":digest,"observed_at":at
            })],
        },
        at + Duration::seconds(1),
    )
    .await
    .unwrap();
    let revision_id = Uuid::new_v4();
    let analyzed = at + Duration::seconds(2);
    let admitted = extraction
        .admit_manual(
            scope,
            "synthetic-analysis",
            &sha256_hex(b"synthetic-analysis"),
            ObservationAnalysisRequest {
                revision_id,
                target_id: target.target_id,
                attempt_id,
                source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
                source_sha256: digest.clone(),
                observed_at: at,
                prompt_version: "prompt.v1".into(),
                parser_version: "parser.v1".into(),
            },
            analyzed,
        )
        .await
        .unwrap();
    let intent_id = admitted.intent.intent_id;
    let claim = extraction
        .claim(scope, intent_id, analyzed)
        .await
        .unwrap()
        .unwrap();
    extraction
        .record_response(
            scope,
            intent_id,
            claim.claim_token,
            ObservationExtractionResponse {
                model_text: json!({"completion":"complete","search_used":"yes"}).to_string(),
                disposition: ObservationExtractionResponseDisposition::Complete,
                actual_model: "synthetic-parser".into(),
                config_revision: Some(1),
                prompt_tokens: 1,
                completion_tokens: 1,
            },
            analyzed,
        )
        .await
        .unwrap();
    extraction.complete(scope, intent_id, claim.claim_token, ObservationAnalysisResult {
        config_revision: Some(1), actual_model: Some("synthetic-parser".into()),
        candidate_json: Some(json!({"completion":"complete","search_used":"yes"}).to_string()),
        outcome: ObservationAnalysisOutcome::Grounded {
            raw_answer: "A synthetic saved answer.".into(),
            citations: vec!["https://example.org/source".into()],
            audit: json!({
                "kind":"observation_extraction","method":"llm_grounded","prompt_version":"prompt.v1",
                "source_sha256":digest,
                "protocol":{"provider":"kimi","model":"synthetic-model","surface":"consumer_web",
                    "search_mode":"web_search","protocol_version":"v1","market":"CN","language":"zh-CN"},
                "refs":[{"role":"answer_segment"},{"role":"completion"},{"role":"search_activity"}],
                "recommendations":{"version":"geo.recommendations.v2","status":"verified",
                    "items":[{"name":"Synthetic gauge","kind":"product","mention_order":1,
                        "rank":null,"brand_attribution":null,"reason":"Saved evidence",
                        "evidence_quote":"A synthetic saved answer.","citation_urls":["https://example.org/source"]}]}
            }),
        }, prompt_tokens: 1, completion_tokens: 1,
    }, analyzed).await.unwrap();
    (state, revision_id)
}

async fn classified_plan(
    state: &AppState,
    scope: &TenantScope,
    purpose: Option<QuestionPurpose>,
) -> StandaloneMeasurementPlan {
    let mut plan = ui_plan(state, scope).await;
    plan.plan_id = Uuid::new_v4();
    plan.input_hash = sha256_hex(plan.plan_id.as_bytes());
    plan.targets[0].target_id = Uuid::new_v4();
    let ChannelTargetInput::Measure {
        question_binding,
        question_set_version,
        ..
    } = &mut plan.targets[0].input
    else {
        panic!()
    };
    *question_set_version = "unclassified.v1".into();
    *question_binding = purpose.map(|purpose| FrozenQuestionBinding {
        reference: QuestionReference {
            question_set_id: Uuid::new_v4(),
            question_set_version_id: Uuid::new_v4(),
            question_id: Uuid::new_v4(),
            question_revision_id: Uuid::new_v4(),
        },
        purpose,
        split_policy_version: "synthetic.v1".into(),
    });
    state
        .channel_job_repository()
        .create_measurement_plan(
            scope,
            &plan.plan_id.to_string(),
            &plan.input_hash,
            plan.clone(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn ui_created_plan_reads_saved_grounded_answer_without_upgrading_unknown_receipt() {
    let state = AppState::development_with_password("synthetic-test");
    let scope = project(&state).await;
    let plan = ui_plan(&state, &scope).await;
    let (state, revision_id) = saved_analysis(state, &scope, &plan).await;
    let before = state
        .channel_job_repository()
        .get_target(&scope, plan.targets[0].target_id)
        .await
        .unwrap();
    let result = read(&state, &scope, &plan).await;
    let target = &result.targets[0];
    assert_eq!(target.outcome_status, Some(ChannelOutcomeStatus::Unknown));
    assert_eq!(target.answer.as_deref(), Some("A synthetic saved answer."));
    assert_eq!(target.citations, ["https://example.org/source"]);
    assert_eq!(
        target.analysis_provenance.as_ref().unwrap().revision_id,
        revision_id
    );
    assert_eq!(
        target.recommendations.as_ref().unwrap()["items"][0]["name"],
        "Synthetic gauge"
    );
    let conditions = target.conditions.as_ref().unwrap();
    assert_eq!(conditions.question, "Which rain gauge should I choose?");
    assert_eq!(conditions.provider, "kimi");
    assert_eq!(conditions.model, "synthetic-model");
    let after = state
        .channel_job_repository()
        .get_target(&scope, plan.targets[0].target_id)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(read(&state, &scope, &plan).await, result);
}

#[tokio::test]
async fn bound_optimization_question_reads_current_saved_evidence() {
    let state = AppState::development_with_password("synthetic-test");
    let scope = project(&state).await;
    let plan = classified_plan(&state, &scope, Some(QuestionPurpose::Optimization)).await;
    let (state, _) = saved_analysis(state, &scope, &plan).await;
    let result = read(&state, &scope, &plan).await;
    assert!(result.targets[0].answer.is_some());
    assert!(result.targets[0].recommendations.is_some());
}

#[tokio::test]
async fn frozen_and_unclassified_plans_do_not_expose_question_or_saved_analysis() {
    for purpose in [Some(QuestionPurpose::FrozenEvaluation), None] {
        let state = AppState::development_with_password("synthetic-test");
        let scope = project(&state).await;
        let plan = classified_plan(&state, &scope, purpose).await;
        let (state, _) = saved_analysis(state, &scope, &plan).await;
        let result = read(&state, &scope, &plan).await;
        let target = &result.targets[0];
        assert_eq!(target.outcome_status, Some(ChannelOutcomeStatus::Unknown));
        assert!(target.conditions.is_none());
        assert!(target.answer.is_none());
        assert!(!target.answer_available);
        assert!(target.citations.is_empty());
        assert!(target.analysis_provenance.is_none());
        assert!(target.recommendations.is_none());
    }
}

#[tokio::test]
async fn exact_plan_read_does_not_cross_projects() {
    let state = AppState::development_with_password("synthetic-test");
    let scope = project(&state).await;
    let other = project(&state).await;
    let plan = ui_plan(&state, &scope).await;
    let (state, _) = saved_analysis(state, &scope, &plan).await;
    let tools = RepositoryHostOps::new(state.knowledge_repository()).with_content(state);
    assert!(
        tools
            .measurement_plan_read(
                &other,
                MeasurementPlanReadRequest {
                    plan_id: plan.plan_id
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn queued_ui_plan_has_conditions_but_no_invented_result() {
    let state = AppState::development_with_password("synthetic-test");
    let scope = project(&state).await;
    let plan = ui_plan(&state, &scope).await;
    let result = read(&state, &scope, &plan).await;
    let target = &result.targets[0];
    assert_eq!(target.state, "queued");
    assert!(target.conditions.is_some());
    assert!(target.outcome_status.is_none());
    assert!(target.answer.is_none());
    assert!(target.recommendations.is_none());
}
