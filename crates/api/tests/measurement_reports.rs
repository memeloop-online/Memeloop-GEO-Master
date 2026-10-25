//! Synthetic persisted-evidence tests; no provider or model requests.
use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{DateTime, Duration, Utc};
use geo_api::{
    AppState, MeasurementPeriodRequest, ObservationEvidenceResolver, preview_project_measurements,
    save_project_measurement_report, save_project_measurement_report_at_cutoff,
};
use geo_domain::{
    ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ChannelTargetInput,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ErrorCode, MeasurementPeriodWindow,
    MemoryObservationAdmissionGate, MemoryObservationAnalysisRepository,
    MemoryObservationCaptureRepository, MemoryObservationExtractionRepository,
    ObservationAnalysisOutcome, ObservationAnalysisRequest, ObservationAnalysisResult,
    ObservationAnalysisSource, ObservationExtractionRepository, ObservationExtractionResponse,
    ObservationExtractionResponseDisposition, ProjectCreate, ProjectSettings,
    StandaloneMeasurementPlan, TenantScope, sha256_hex,
};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

fn search_measurement(at: DateTime<Utc>, ordinal: usize) -> geo_domain::SerpMeasurement {
    use geo_domain::*;
    SerpMeasurement {
        measurement_id: Uuid::new_v4(),
        source_key: "synthetic-unconfigured".into(),
        protocol: SerpProtocol {
            query: format!("exact synthetic search {ordinal}"),
            engine: SerpEngine::Google,
            surface: SerpSurface::ThirdPartyApi,
            source: "synthetic".into(),
            source_location_code: "2840".into(),
            country: "US".into(),
            city: None,
            language: "en".into(),
            device: SerpDevice::Desktop,
            operating_system: "windows".into(),
            requested_depth: 10,
            max_pages: 1,
            priority: 1,
            login: "unspecified".into(),
            personalization: "unspecified".into(),
            protocol_version: SERP_PROTOCOL_VERSION.into(),
            connector_version: "synthetic.v1".into(),
        },
        target: None,
        target_rule_version: SERP_TARGET_RULE_VERSION.into(),
        question_binding: None,
        scheduled_at: at,
        created_at: at,
        state: SerpTaskState::Queued,
    }
}

async fn search_versions(
    repository: &dyn geo_domain::SerpRepository,
    scope: &TenantScope,
    id: Uuid,
) -> Uuid {
    search_versions_at(repository, scope, id, Utc::now()).await
}

async fn search_versions_at(
    repository: &dyn geo_domain::SerpRepository,
    scope: &TenantScope,
    id: Uuid,
    captured_at: DateTime<Utc>,
) -> Uuid {
    use geo_domain::*;
    let now = captured_at;
    let claim = repository
        .claim(scope, id, now, now + Duration::minutes(2))
        .await
        .unwrap()
        .unwrap();
    let intent = repository
        .begin_send(
            scope,
            &claim,
            &sha256_hex(b"synthetic-request"),
            "synthetic-report-tag",
            None,
            now,
        )
        .await
        .unwrap()
        .unwrap();
    let body = b"synthetic persisted search response".to_vec();
    let submission = SerpRawEvidence {
        evidence_id: Uuid::new_v4(),
        measurement_id: id,
        attempt_id: intent.attempt_id,
        operation: SerpEvidenceOperation::Submission,
        provider_task_id: None,
        request_sha256: intent.request_sha256.clone(),
        intent_request_sha256: intent.request_sha256.clone(),
        response_sha256: sha256_hex(&body),
        body,
        body_complete: true,
        http_status: Some(200),
        send_certainty: SerpSendCertainty::ResponseReceived,
        captured_at,
    };
    repository
        .append_raw(scope, &intent, submission.clone())
        .await
        .unwrap();
    repository
        .bind_provider_task(
            scope,
            &intent,
            SerpProviderTask {
                measurement_id: id,
                attempt_id: intent.attempt_id,
                binding_evidence_id: submission.evidence_id,
                provider_task_id: "synthetic-task".into(),
                correlation_tag: intent.correlation_tag.clone(),
            },
        )
        .await
        .unwrap();
    let raw = repository
        .append_raw(
            scope,
            &intent,
            SerpRawEvidence {
                evidence_id: Uuid::new_v4(),
                operation: SerpEvidenceOperation::ResultRead,
                provider_task_id: Some("synthetic-task".into()),
                captured_at,
                request_sha256: sha256_hex(b"synthetic-read"),
                ..submission
            },
        )
        .await
        .unwrap();
    let mut useful = Uuid::nil();
    // More than one observation page; later failed interpretations must not
    // erase the persisted useful partial response.
    for index in 0..101 {
        let observation_id = Uuid::new_v4();
        if index == 0 {
            useful = observation_id;
        }
        repository
            .append_observation(
                scope,
                SerpObservation {
                    observation_id,
                    measurement_id: id,
                    attempt_id: intent.attempt_id,
                    raw_evidence_id: raw.evidence.evidence_id,
                    raw_sha256: raw.evidence.response_sha256.clone(),
                    parser_version: format!("synthetic.v{index}"),
                    provider_observed_at: None,
                    received_at: raw.evidence.captured_at,
                    analyzed_at: Utc::now().max(raw.stored_at),
                    status: if index == 0 {
                        SerpObservationStatus::Partial
                    } else {
                        SerpObservationStatus::Failed
                    },
                    actual_conditions: SerpActualConditions::default(),
                    coverage: SerpCoverage {
                        requested_depth: 10,
                        observed_organic_depth: 0,
                        pages_received: 1,
                        completion: SerpCoverageCompletion::Partial,
                        truncated: true,
                        exhaustion_evidence_locator: None,
                    },
                    results: vec![],
                    source_limitations: vec![SerpLimitationCode::ResultUnavailable],
                },
            )
            .await
            .unwrap();
    }
    useful
}

#[tokio::test]
async fn delayed_generation_uses_cutoff_for_search_ingestion_and_freezes_cohort() {
    use geo_domain::{MeasurementPeriodSearchStatus, SerpRepository};
    let base = AppState::development_with_password("synthetic-cutoff-report");
    let scope = project(&base).await;
    let repository = Arc::new(geo_persistence::MemorySerpRepository::default());
    let state = base.clone().with_serp_service(geo_api::SerpService::new(
        repository.clone(),
        base.project_repository(),
        base.question_repository(),
    ));
    let at = Utc::now() - Duration::hours(1);
    let measurement = search_measurement(at, 0);
    let id = measurement.measurement_id;
    repository
        .accept(&scope, "before-cutoff", measurement)
        .await
        .unwrap();
    let cutoff = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    // The source timestamp is historical, but raw bytes and interpretations
    // enter storage only after cutoff. Generation must not make them eligible.
    let useful = search_versions_at(repository.as_ref(), &scope, id, at).await;
    repository
        .accept(&scope, "after-cutoff", search_measurement(at, 1))
        .await
        .unwrap();
    let generated = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap()
        + Duration::seconds(1);
    let request = MeasurementPeriodRequest {
        start_at: at - Duration::hours(1),
        end_at: cutoff,
        report_timezone: "UTC".into(),
        correction_of: None,
        question_set: None,
    };
    let first = save_project_measurement_report_at_cutoff(
        &state,
        &scope,
        request.clone(),
        cutoff,
        generated,
    )
    .await
    .unwrap();
    assert_eq!(first.evidence_as_of, cutoff);
    assert_eq!(first.generated_at, generated);
    let search = first.search.as_ref().unwrap();
    assert_eq!(search.coverage.planned, 1);
    assert_eq!(
        search.coverage.counts[&MeasurementPeriodSearchStatus::NoEligibleObservation],
        1
    );
    assert!(search.samples[0].evidence.is_none());
    assert_eq!(
        save_project_measurement_report_at_cutoff(
            &base,
            &scope,
            request.clone(),
            generated,
            generated + Duration::seconds(1),
        )
        .await
        .unwrap(),
        first
    );
    let correction_request = MeasurementPeriodRequest {
        correction_of: Some(first.report_id),
        ..request
    };
    let correction = save_project_measurement_report_at_cutoff(
        &state,
        &scope,
        correction_request.clone(),
        generated,
        generated + Duration::seconds(2),
    )
    .await
    .unwrap();
    let search = correction.search.as_ref().unwrap();
    assert_eq!(search.coverage.planned, 1);
    assert_eq!(
        search.samples[0]
            .evidence
            .as_ref()
            .unwrap()
            .observation
            .observation_id,
        useful
    );
    assert_eq!(correction.generated_at, generated + Duration::seconds(2));
    assert_eq!(
        save_project_measurement_report_at_cutoff(
            &base,
            &scope,
            correction_request,
            generated,
            generated + Duration::seconds(3),
        )
        .await
        .unwrap(),
        correction
    );
    let fresh = preview_project_measurements(&state, &scope, Some(first.window()), None, generated)
        .await
        .unwrap();
    assert_eq!(fresh.search.unwrap().coverage.planned, 2);
}

#[tokio::test]
async fn search_report_reads_unconfigured_persisted_pages_and_freezes_correction_cohort() {
    use geo_domain::{MeasurementPeriodSearchStatus, SearchEvidenceTimeBasis, SerpRepository};
    let without_search = AppState::development_with_password("synthetic-report-search");
    let scope = project(&without_search).await;
    let repository = Arc::new(geo_persistence::MemorySerpRepository::default());
    let state = without_search
        .clone()
        .with_serp_service(geo_api::SerpService::new(
            repository.clone(),
            without_search.project_repository(),
            without_search.question_repository(),
        ));
    let at = Utc::now() - Duration::hours(1);
    plan(&state, &scope, at, None).await;
    let mut ids = Vec::new();
    for ordinal in 0..101 {
        let measurement = search_measurement(at, ordinal);
        ids.push(measurement.measurement_id);
        repository
            .accept(&scope, &format!("search-{ordinal}"), measurement)
            .await
            .unwrap();
    }
    let now = Utc::now() + Duration::seconds(1);
    let request = MeasurementPeriodRequest {
        start_at: at - Duration::hours(1),
        end_at: now + Duration::minutes(1),
        report_timezone: "UTC".into(),
        correction_of: None,
        question_set: None,
    };
    // Explicit window ends after the evidence insertion below, but never after
    // the caller's current time used by report validation.
    let report_now = request.end_at;
    let first = save_project_measurement_report(&state, &scope, request.clone(), report_now)
        .await
        .unwrap();
    assert_eq!(first.coverage.planned, 1);
    let search = first.search.as_ref().unwrap();
    assert_eq!(search.coverage.planned, 101);
    assert_eq!(
        search.coverage.counts[&MeasurementPeriodSearchStatus::NoEligibleObservation],
        101
    );
    let useful = search_versions(repository.as_ref(), &scope, ids[0]).await;
    repository
        .accept(&scope, "new-after-freeze", search_measurement(at, 102))
        .await
        .unwrap();
    // Stored replay remains available even when the search service is absent.
    assert_eq!(
        save_project_measurement_report(&without_search, &scope, request.clone(), report_now)
            .await
            .unwrap(),
        first
    );
    let correction_request = MeasurementPeriodRequest {
        correction_of: Some(first.report_id),
        ..request.clone()
    };
    assert!(
        save_project_measurement_report(
            &without_search,
            &scope,
            correction_request.clone(),
            report_now
        )
        .await
        .is_err()
    );
    let correction =
        save_project_measurement_report(&state, &scope, correction_request.clone(), report_now)
            .await
            .unwrap();
    assert_eq!(correction.coverage.planned, 1);
    let search = correction.search.as_ref().unwrap();
    assert_eq!(search.coverage.planned, 101);
    assert_eq!(
        search.coverage.counts[&MeasurementPeriodSearchStatus::Partial],
        1
    );
    let evidence = search
        .samples
        .iter()
        .find(|sample| sample.cohort.measurement_id == ids[0])
        .unwrap()
        .evidence
        .as_ref()
        .unwrap();
    assert_eq!(evidence.observation.observation_id, useful);
    assert_eq!(
        evidence.evidence_time_basis,
        SearchEvidenceTimeBasis::ReceivedAt
    );
    assert_eq!(
        save_project_measurement_report(&without_search, &scope, correction_request, report_now)
            .await
            .unwrap(),
        correction
    );
    let fresh =
        preview_project_measurements(&state, &scope, Some(first.window()), None, report_now)
            .await
            .unwrap();
    assert_eq!(fresh.search.unwrap().coverage.planned, 102);
    assert_eq!(
        state
            .report_repository()
            .get_measurement_period(&scope, first.report_id)
            .await
            .unwrap(),
        first
    );
}

#[tokio::test]
async fn legacy_report_correction_keeps_search_unavailable_after_service_is_added() {
    use geo_domain::SerpRepository;
    let state = AppState::development_with_password("synthetic-legacy-report");
    let scope = project(&state).await;
    let now = Utc::now();
    let request = MeasurementPeriodRequest {
        start_at: now - Duration::days(1),
        end_at: now,
        report_timezone: "UTC".into(),
        correction_of: None,
        question_set: None,
    };
    let first = save_project_measurement_report(&state, &scope, request.clone(), now)
        .await
        .unwrap();
    assert!(first.search.is_none());
    let repository = Arc::new(geo_persistence::MemorySerpRepository::default());
    repository
        .accept(
            &scope,
            "new",
            search_measurement(now - Duration::hours(1), 0),
        )
        .await
        .unwrap();
    let service = geo_api::SerpService::new(
        repository,
        state.project_repository(),
        state.question_repository(),
    );
    let state = state.with_serp_service(service);
    let correction = save_project_measurement_report(
        &state,
        &scope,
        MeasurementPeriodRequest {
            correction_of: Some(first.report_id),
            ..request
        },
        now + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert!(correction.search.is_none());
}

async fn project(state: &AppState) -> TenantScope {
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Independent topic".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    assert!(
        state
            .project_repository()
            .get_start(&tenant, project.id)
            .await
            .unwrap()
            .is_none()
    );
    TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id))
}

async fn plan(
    state: &AppState,
    scope: &TenantScope,
    at: DateTime<Utc>,
    question_binding: Option<geo_domain::FrozenQuestionBinding>,
) -> (Uuid, ChannelTarget) {
    let target = ChannelTarget {
        target_id: Uuid::new_v4(),
        input: ChannelTargetInput::Measure {
            account_id: Uuid::new_v4(),
            provider: "provider-a".into(),
            model: "model-a".into(),
            surface: "consumer_web".into(),
            search_mode: "web_search".into(),
            protocol_version: "v1".into(),
            question_set_version: "ad_hoc.v1".into(),
            question: "What is a rain gauge?".into(),
            market: "generic".into(),
            language: "en".into(),
            scheduled_at: at,
            sample_ordinal: 0,
            question_binding,
        },
    };
    let id = Uuid::new_v4();
    state
        .channel_job_repository()
        .create_measurement_plan(
            scope,
            &id.to_string(),
            &id.to_string(),
            StandaloneMeasurementPlan {
                plan_id: id,
                project_id: scope.project_id.unwrap(),
                title: "Topic".into(),
                input_hash: id.to_string(),
                revision: 1,
                created_at: at,
                targets: vec![target.clone()],
            },
        )
        .await
        .unwrap();
    (id, target)
}

async fn question_version(
    state: &AppState,
    scope: &TenantScope,
    key: &str,
) -> geo_domain::QuestionSetVersion {
    state
        .question_repository()
        .create_set(
            scope,
            geo_domain::CreateQuestionSet {
                idempotency_key: key.into(),
                name: "Synthetic report questions".into(),
                questions: vec![geo_domain::QuestionDraft {
                    question_id: None,
                    text: "What is a rain gauge?".into(),
                    intent: "learn".into(),
                    product_refs: vec![],
                    market: "generic".into(),
                    language: "en".into(),
                    source: geo_domain::QuestionSource {
                        kind: geo_domain::QuestionSourceKind::UserProvided,
                        reference_id: None,
                    },
                    weight: 1,
                }],
            },
        )
        .await
        .unwrap()
}

fn report_question_set(
    version: &geo_domain::QuestionSetVersion,
) -> geo_domain::MeasurementReportQuestionSet {
    geo_domain::MeasurementReportQuestionSet {
        question_set_id: version.question_set_id,
        question_set_version_id: version.id,
    }
}

async fn binding(
    state: &AppState,
    scope: &TenantScope,
    version: &geo_domain::QuestionSetVersion,
) -> geo_domain::FrozenQuestionBinding {
    state
        .question_repository()
        .resolve_question(
            scope,
            geo_domain::QuestionReference {
                question_set_id: version.question_set_id,
                question_set_version_id: version.id,
                question_id: version.questions[0].question_id,
                question_revision_id: version.questions[0].id,
            },
        )
        .await
        .unwrap()
        .binding
}

#[tokio::test]
async fn scoped_reports_freeze_exact_version_all_history_and_keep_corrections_separate() {
    use geo_domain::{ReviseQuestionSet, SerpRepository};
    let base = AppState::development_with_password("synthetic-scoped-report");
    let scope = project(&base).await;
    let search_repository = Arc::new(geo_persistence::MemorySerpRepository::default());
    let state = base.clone().with_serp_service(geo_api::SerpService::new(
        search_repository.clone(),
        base.project_repository(),
        base.question_repository(),
    ));
    let version = question_version(&state, &scope, "scoped-first").await;
    let selected = report_question_set(&version);
    let bound = binding(&state, &scope, &version).await;
    let now = Utc::now();
    let first_at = now - Duration::days(20);
    let (_, first_target) = plan(&state, &scope, first_at, Some(bound.clone())).await;
    let (_, recent_target) = plan(
        &state,
        &scope,
        now - Duration::hours(1),
        Some(bound.clone()),
    )
    .await;
    let question = &version.questions[0];
    let next = state
        .question_repository()
        .revise_set(
            &scope,
            version.question_set_id,
            ReviseQuestionSet {
                idempotency_key: "scoped-next".into(),
                base_version_id: version.id,
                name: version.name.clone(),
                questions: vec![geo_domain::QuestionDraft {
                    question_id: Some(question.question_id),
                    text: question.text.clone(),
                    intent: question.intent.clone(),
                    product_refs: question.product_refs.clone(),
                    market: question.market.clone(),
                    language: question.language.clone(),
                    source: question.source.clone(),
                    weight: question.weight,
                }],
            },
        )
        .await
        .unwrap();
    plan(
        &state,
        &scope,
        now - Duration::hours(1),
        Some(binding(&state, &scope, &next).await),
    )
    .await;
    let other_set = question_version(&state, &scope, "scoped-other").await;
    plan(
        &state,
        &scope,
        now - Duration::hours(1),
        Some(binding(&state, &scope, &other_set).await),
    )
    .await;
    plan(&state, &scope, now - Duration::hours(1), None).await;
    let mut old_search = search_measurement(now - Duration::days(30), 0);
    old_search.question_binding = Some(bound.clone());
    search_repository
        .accept(&scope, "scoped-search-first", old_search.clone())
        .await
        .unwrap();
    let mut next_search = search_measurement(now - Duration::hours(1), 1);
    next_search.question_binding = Some(binding(&state, &scope, &next).await);
    search_repository
        .accept(&scope, "scoped-search-next", next_search)
        .await
        .unwrap();
    search_repository
        .accept(
            &scope,
            "scoped-search-adhoc",
            search_measurement(now - Duration::hours(1), 2),
        )
        .await
        .unwrap();
    let now = Utc::now() + Duration::seconds(1);
    let preview = preview_project_measurements(&state, &scope, None, Some(selected.clone()), now)
        .await
        .unwrap();
    assert_eq!(preview.coverage.planned, 2);
    assert!(
        preview
            .samples
            .iter()
            .all(|sample| selected.matches(sample.question_binding.as_ref()))
    );
    assert!(
        preview
            .samples
            .iter()
            .any(|sample| sample.target_id == first_target.target_id)
    );
    assert_eq!(
        preview.report_window_start_at,
        DateTime::from_timestamp_micros(old_search.scheduled_at.timestamp_micros()).unwrap()
    );
    let search = preview.search.as_ref().unwrap();
    assert_eq!(search.coverage.planned, 1);
    assert_eq!(
        search.samples[0].cohort.measurement_id,
        old_search.measurement_id
    );
    let request = MeasurementPeriodRequest {
        start_at: preview.report_window_start_at,
        end_at: preview.report_window_end_at,
        report_timezone: preview.report_timezone,
        correction_of: None,
        question_set: Some(selected.clone()),
    };
    let first = save_project_measurement_report(&state, &scope, request.clone(), now)
        .await
        .unwrap();
    let frozen = serde_json::to_vec(&first).unwrap();
    let project_report = save_project_measurement_report(
        &state,
        &scope,
        MeasurementPeriodRequest {
            question_set: None,
            ..request.clone()
        },
        now,
    )
    .await
    .unwrap();
    let next_report = save_project_measurement_report(
        &state,
        &scope,
        MeasurementPeriodRequest {
            question_set: Some(report_question_set(&next)),
            ..request.clone()
        },
        now,
    )
    .await
    .unwrap();
    assert_ne!(first.report_id, project_report.report_id);
    assert_ne!(first.report_id, next_report.report_id);
    assert_eq!(project_report.coverage.planned, 5);
    assert_eq!(next_report.coverage.planned, 1);
    plan(&state, &scope, now - Duration::hours(2), Some(bound)).await;
    assert_eq!(
        save_project_measurement_report(
            &state,
            &scope,
            request.clone(),
            now + Duration::seconds(1)
        )
        .await
        .unwrap(),
        first
    );
    let correction = save_project_measurement_report(
        &state,
        &scope,
        MeasurementPeriodRequest {
            correction_of: Some(first.report_id),
            ..request.clone()
        },
        now + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(correction.coverage.planned, 2);
    assert_eq!(
        serde_json::to_vec(
            &state
                .report_repository()
                .get_measurement_period(&scope, first.report_id)
                .await
                .unwrap()
        )
        .unwrap(),
        frozen
    );
    assert!(
        save_project_measurement_report(
            &state,
            &scope,
            MeasurementPeriodRequest {
                correction_of: Some(first.report_id),
                question_set: Some(report_question_set(&next)),
                ..request.clone()
            },
            now + Duration::seconds(1)
        )
        .await
        .is_err()
    );
    let foreign_scope = project(&state).await;
    assert_eq!(
        preview_project_measurements(&state, &foreign_scope, None, Some(selected.clone()), now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let window = MeasurementPeriodWindow {
        start_at: now - Duration::days(7),
        end_at: now,
        report_timezone: "UTC".into(),
    };
    let recent = preview_project_measurements(&state, &scope, Some(window), Some(selected), now)
        .await
        .unwrap();
    assert_eq!(recent.coverage.planned, 2);
    assert!(
        recent
            .samples
            .iter()
            .any(|sample| sample.target_id == recent_target.target_id)
    );
    assert_eq!(recent.search.unwrap().coverage.planned, 0);
}

#[tokio::test]
async fn no_cycle_preview_save_and_correction_preserve_unknown_and_frozen_sample_count() {
    let state = AppState::development_with_password("report-test");
    let scope = project(&state).await;
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let at = now - Duration::hours(1);
    let window = MeasurementPeriodWindow {
        start_at: now - Duration::days(7),
        end_at: now,
        report_timezone: "Asia/Shanghai".into(),
    };
    let (_, target) = plan(&state, &scope, at, None).await;
    let jobs = state.channel_job_repository();
    let captures = Arc::new(MemoryObservationCaptureRepository::new(jobs.clone()));
    let gate = MemoryObservationAdmissionGate::default();
    let analyses = Arc::new(
        MemoryObservationAnalysisRepository::new(jobs.clone(), captures.clone())
            .with_admission_gate(gate.clone()),
    );
    let extraction = MemoryObservationExtractionRepository::new(captures.clone())
        .with_automatic_admission(gate, analyses.clone());
    let state = state.with_observation_evidence_resolver(ObservationEvidenceResolver::new(
        analyses.clone(),
        captures,
    ));
    let attempt_id = Uuid::new_v4();
    jobs.claim(&scope, target.target_id, attempt_id, at)
        .await
        .unwrap();
    let source = json!({"messages":[{"role":"assistant","text":"A saved answer."}]}).to_string();
    let digest = sha256_hex(source.as_bytes());
    jobs.finish(
        &scope,
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
            runner_evidence: vec![json!({
                "kind":"observation_capture","schema_version":"geo.observation.capture.v1",
                "phase":"source","source_json":source,"source_sha256":digest,"observed_at":at
            })],
            fixture: false,
        },
        at + Duration::seconds(1),
    )
    .await
    .unwrap();
    let original = jobs.get_target(&scope, target.target_id).await.unwrap();
    let preview = preview_project_measurements(&state, &scope, Some(window.clone()), None, now)
        .await
        .unwrap();
    assert_eq!(preview.coverage.planned, 1);
    assert_eq!(preview.coverage.counts["unknown"], 1);
    assert!(
        state
            .report_repository()
            .list_measurement_periods(&scope, None)
            .await
            .unwrap()
            .is_empty()
    );
    let request = MeasurementPeriodRequest {
        start_at: window.start_at,
        end_at: window.end_at,
        report_timezone: window.report_timezone.clone(),
        correction_of: None,
        question_set: None,
    };
    let (first, replay) = tokio::join!(
        save_project_measurement_report(&state, &scope, request.clone(), now),
        save_project_measurement_report(
            &state,
            &scope,
            request.clone(),
            now + Duration::seconds(1)
        ),
    );
    let first = first.unwrap();
    assert_eq!(first, replay.unwrap());
    let analyzed = now + Duration::minutes(1);
    let revision_id = Uuid::new_v4();
    let admitted = extraction
        .admit_manual(
            &scope,
            "analysis",
            &sha256_hex(b"analysis"),
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
        .claim(&scope, intent_id, analyzed)
        .await
        .unwrap()
        .unwrap();
    extraction
        .record_response(
            &scope,
            intent_id,
            claim.claim_token,
            ObservationExtractionResponse {
                model_text: json!({"completion":"complete","search_used":"yes"}).to_string(),
                disposition: ObservationExtractionResponseDisposition::Complete,
                actual_model: "parser-model".into(),
                config_revision: Some(1),
                prompt_tokens: 1,
                completion_tokens: 1,
            },
            analyzed,
        )
        .await
        .unwrap();
    let recommendations = json!({
        "version":"geo.recommendations.v2","status":"verified",
        "items":[{"name":"Synthetic product","brand_attribution":null,"kind":"product","mention_order":1,"rank":1,
            "reason":"A saved recommendation","evidence_quote":"A saved answer.","citation_urls":["https://example.org/source"]}]
    });
    extraction.complete(&scope, intent_id, claim.claim_token, ObservationAnalysisResult {
        config_revision: Some(1), actual_model: Some("parser-model".into()),
        candidate_json: Some(json!({"completion":"complete","search_used":"yes"}).to_string()),
        outcome: ObservationAnalysisOutcome::Grounded {
            raw_answer: "A saved answer.".into(), citations: vec!["https://example.org/source".into()],
            audit: json!({
                "kind":"observation_extraction","method":"llm_grounded","prompt_version":"prompt.v1",
                "source_sha256":digest,"protocol":{
                    "provider":"provider-a","model":"model-a","surface":"consumer_web",
                    "search_mode":"web_search","protocol_version":"v1","market":"generic","language":"en"
                },"refs":[{"role":"answer_segment"},{"role":"completion"},{"role":"search_activity"}],
                "recommendations":recommendations
            }),
        }, prompt_tokens: 1, completion_tokens: 1,
    }, analyzed).await.unwrap();
    // Even when generated after this interpretation is already available,
    // the independent cutoff keeps it out of a newly frozen snapshot.
    let delayed_request = MeasurementPeriodRequest {
        report_timezone: "UTC".into(),
        ..request.clone()
    };
    let delayed = save_project_measurement_report_at_cutoff(
        &state,
        &scope,
        delayed_request.clone(),
        now,
        analyzed + Duration::seconds(1),
    )
    .await
    .unwrap();
    assert_eq!(delayed.evidence_as_of, now);
    assert_eq!(delayed.generated_at, analyzed + Duration::seconds(1));
    assert_eq!(delayed.coverage.grounded_saved_analysis, 0);
    assert_eq!(delayed.coverage.counts["unknown"], 1);
    assert!(delayed.samples[0].observation.is_none());
    assert!(delayed.samples[0].recommendations.is_none());
    // A newly persisted historical plan may enter a fresh preview, never
    // inflate the first saved report's frozen sample denominator.
    plan(&state, &scope, at + Duration::minutes(1), None).await;
    let delayed_correction = save_project_measurement_report_at_cutoff(
        &state,
        &scope,
        MeasurementPeriodRequest {
            correction_of: Some(delayed.report_id),
            ..delayed_request
        },
        analyzed,
        analyzed + Duration::seconds(2),
    )
    .await
    .unwrap();
    assert_eq!(delayed_correction.coverage.planned, 1);
    assert_eq!(delayed_correction.coverage.grounded_saved_analysis, 1);
    assert_eq!(
        save_project_measurement_report(&state, &scope, request.clone(), analyzed)
            .await
            .unwrap(),
        first
    );
    let corrected_request = MeasurementPeriodRequest {
        correction_of: Some(first.report_id),
        ..request
    };
    let correction =
        save_project_measurement_report(&state, &scope, corrected_request.clone(), analyzed)
            .await
            .unwrap();
    assert_eq!(correction.coverage.planned, 1);
    assert_eq!(correction.coverage.counts["unknown"], 1);
    assert_eq!(correction.coverage.observed_live, 0);
    assert_eq!(correction.coverage.grounded_saved_analysis, 1);
    assert_eq!(
        correction.samples[0].recommendations.as_ref(),
        Some(&recommendations)
    );
    assert!(first.samples[0].recommendations.is_none());
    assert_eq!(
        correction.samples[0].conditions.question,
        "What is a rain gauge?"
    );
    assert_eq!(correction.samples[0].conditions.model, "model-a");
    let provenance = correction.samples[0]
        .observation
        .as_ref()
        .unwrap()
        .provenance
        .as_ref()
        .unwrap();
    assert_eq!(provenance.revision_id, revision_id);
    assert_eq!(provenance.source_sha256, digest);
    assert_eq!(provenance.actual_model, "parser-model");
    assert_eq!(
        save_project_measurement_report(
            &state,
            &scope,
            corrected_request,
            analyzed + Duration::seconds(2)
        )
        .await
        .unwrap(),
        correction
    );
    // A newer effective interpretation without recommendations must not inherit
    // the preceding revision's recommendations, even for the same raw source.
    let newer_at = analyzed + Duration::seconds(3);
    let newer_revision_id = Uuid::new_v4();
    let admitted = extraction
        .admit_manual(
            &scope,
            "analysis-newer",
            &sha256_hex(b"analysis-newer"),
            ObservationAnalysisRequest {
                revision_id: newer_revision_id,
                target_id: target.target_id,
                attempt_id,
                source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
                source_sha256: digest.clone(),
                observed_at: at,
                prompt_version: "prompt.v1".into(),
                parser_version: "parser.v2".into(),
            },
            newer_at,
        )
        .await
        .unwrap();
    let newer_intent = admitted.intent.intent_id;
    let claim = extraction
        .claim(&scope, newer_intent, newer_at)
        .await
        .unwrap()
        .unwrap();
    extraction
        .record_response(
            &scope,
            newer_intent,
            claim.claim_token,
            ObservationExtractionResponse {
                model_text: json!({"completion":"complete","search_used":"yes"}).to_string(),
                disposition: ObservationExtractionResponseDisposition::Complete,
                actual_model: "parser-model".into(),
                config_revision: Some(1),
                prompt_tokens: 1,
                completion_tokens: 1,
            },
            newer_at,
        )
        .await
        .unwrap();
    extraction.complete(&scope, newer_intent, claim.claim_token, ObservationAnalysisResult {
        config_revision: Some(1), actual_model: Some("parser-model".into()), candidate_json: Some(json!({"completion":"complete","search_used":"yes"}).to_string()),
        outcome: ObservationAnalysisOutcome::Grounded {
            raw_answer: "A newer saved answer.".into(), citations: vec![],
            audit: json!({"kind":"observation_extraction","method":"llm_grounded","prompt_version":"prompt.v1", "source_sha256":digest,
                "protocol":{"provider":"provider-a","model":"model-a","surface":"consumer_web","search_mode":"web_search","protocol_version":"v1","market":"generic","language":"en"},
                "refs":[{"role":"answer_segment"},{"role":"completion"},{"role":"search_activity"}]}),
        }, prompt_tokens: 1, completion_tokens: 1,
    }, newer_at).await.unwrap();
    let updated = save_project_measurement_report(
        &state,
        &scope,
        MeasurementPeriodRequest {
            start_at: first.report_window_start_at,
            end_at: first.report_window_end_at,
            report_timezone: first.report_timezone.clone(),
            question_set: None,
            correction_of: Some(correction.report_id),
        },
        newer_at,
    )
    .await
    .unwrap();
    assert!(updated.samples[0].recommendations.is_none());
    assert_eq!(
        updated.samples[0].observation.as_ref().unwrap().raw_answer,
        "A newer saved answer."
    );
    assert_eq!(
        updated.samples[0]
            .observation
            .as_ref()
            .unwrap()
            .provenance
            .as_ref()
            .unwrap()
            .revision_id,
        newer_revision_id
    );
    assert_eq!(
        state
            .report_repository()
            .get_measurement_period(&scope, correction.report_id)
            .await
            .unwrap(),
        correction
    );
    assert_eq!(
        jobs.get_target(&scope, target.target_id).await.unwrap(),
        original
    );
    assert_eq!(
        state
            .report_repository()
            .get_measurement_period(&scope, first.report_id)
            .await
            .unwrap(),
        first
    );
    assert!(
        state
            .report_repository()
            .list(&scope, scope.project_id.unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .project_repository()
            .get_start(&scope, scope.project_id.unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let other = project(&state).await;
    assert_eq!(
        state
            .report_repository()
            .get_measurement_period(&other, first.report_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn default_window_paginates_all_plans_and_excludes_half_open_boundary() {
    let state = AppState::development_with_password("report-test");
    let scope = project(&state).await;
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    for index in 0..105 {
        plan(
            &state,
            &scope,
            now - Duration::hours(1) - Duration::seconds(index),
            None,
        )
        .await;
    }
    plan(&state, &scope, now, None).await;
    plan(&state, &scope, now - Duration::days(8), None).await;
    let preview = preview_project_measurements(&state, &scope, None, None, now)
        .await
        .unwrap();
    assert_eq!(preview.report_window_start_at, now - Duration::days(7));
    assert_eq!(preview.report_window_end_at, now);
    assert_eq!(preview.coverage.planned, 105);
    assert_eq!(preview.coverage.counts["pending"], 105);
    assert!(
        preview
            .samples
            .iter()
            .all(|sample| sample.observation.is_none())
    );
    let invalid = MeasurementPeriodWindow {
        start_at: now,
        end_at: now + Duration::days(1),
        report_timezone: "UTC".into(),
    };
    assert!(
        preview_project_measurements(&state, &scope, Some(invalid), None, now)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn invalid_report_clocks_are_rejected_before_project_or_evidence_reads() {
    let state = AppState::development_with_password("synthetic-invalid-clock");
    // No project exists: a project lookup would return NotFound rather than
    // InvalidRequest, proving the clock guard runs before evidence access.
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(Uuid::new_v4().into()),
    );
    let cutoff = Utc::now();
    let request = MeasurementPeriodRequest {
        start_at: cutoff - Duration::days(1),
        end_at: cutoff,
        report_timezone: "UTC".into(),
        correction_of: None,
        question_set: None,
    };
    for (request, evidence, generated) in [
        (request.clone(), cutoff, cutoff - Duration::seconds(1)),
        (request, cutoff - Duration::seconds(1), cutoff),
    ] {
        assert_eq!(
            save_project_measurement_report_at_cutoff(
                &state, &scope, request, evidence, generated,
            ).await.unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn saving_report_ignores_corrupt_other_windows_but_rejects_corrupt_current_window() {
    let config = geo_persistence::DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database required"),
    )
    .unwrap();
    let database = geo_persistence::Database::connect_and_migrate(&config)
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("window-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("window-{tenant}")).execute(database.pool()).await.unwrap();
    let state = AppState::from_database(&database);
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let created = state
        .project_repository()
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic report windows".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(
        tenant_scope.operator_id,
        tenant_scope.tenant_id,
        Some(created.id),
    );
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let version = question_version(&state, &scope, "window-report").await;
    let selected = report_question_set(&version);
    let question_binding = binding(&state, &scope, &version).await;
    plan(
        &state,
        &scope,
        now - Duration::days(2),
        Some(question_binding),
    )
    .await;
    for question_set in [None, Some(selected)] {
        let old = MeasurementPeriodRequest {
            start_at: now - Duration::days(7),
            end_at: now - Duration::days(1),
            report_timezone: "UTC".into(),
            correction_of: None,
            question_set,
        };
        let previous = save_project_measurement_report(&state, &scope, old.clone(), now)
            .await
            .unwrap();
        assert_eq!(previous.samples.len(), 1);
        sqlx::query("UPDATE measurement_period_reports SET snapshot=snapshot #- '{samples,0,conditions}' WHERE report_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4")
            .bind(previous.report_id).bind(operator).bind(tenant).bind(created.id.as_uuid())
            .execute(database.pool()).await.unwrap();
        assert_eq!(
            state
                .report_repository()
                .list_measurement_periods(&scope, old.question_set.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        let current = MeasurementPeriodRequest {
            end_at: now,
            ..old.clone()
        };
        let saved = save_project_measurement_report(&state, &scope, current.clone(), now)
            .await
            .unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.samples.len(), 1);
        assert_eq!(
            save_project_measurement_report(
                &state,
                &scope,
                current.clone(),
                now + Duration::minutes(1)
            )
            .await
            .unwrap(),
            saved
        );
        let correction = MeasurementPeriodRequest {
            correction_of: Some(saved.report_id),
            ..current
        };
        let corrected = save_project_measurement_report(
            &state,
            &scope,
            correction.clone(),
            now + Duration::minutes(1),
        )
        .await
        .unwrap();
        assert_eq!(corrected.revision, 2);
        assert_eq!(corrected.correction_of, Some(saved.report_id));
        assert_eq!(
            save_project_measurement_report(&state, &scope, correction, now + Duration::minutes(2))
                .await
                .unwrap(),
            corrected
        );
        assert_eq!(
            save_project_measurement_report(&state, &scope, old, now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
    }
}

#[tokio::test]
async fn http_cycle_free_preview_save_list_and_detail_share_the_same_snapshot() {
    let state = AppState::development_with_password("report-test");
    let scope = project(&state).await;
    let id = scope.project_id.unwrap();
    let version = question_version(&state, &scope, "http-scoped-report").await;
    let selected = report_question_set(&version);
    plan(&state, &scope, Utc::now() - Duration::hours(1), None).await;
    let app = geo_api::router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"login_name":"demo@localhost","password":"report-test"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let login: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let csrf = login["csrf_token"].as_str().unwrap();
    let request = |method: &str, path: &str, body: serde_json::Value, token: bool| {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost:8080")
            .header("origin", "http://localhost:5173")
            .header("cookie", &cookie)
            .header("content-type", "application/json");
        if token {
            builder = builder.header(geo_api::CSRF_HEADER, csrf);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    };
    let preview_path = format!(
        "/api/v1/projects/{id}/measurement-report-preview?tenant_id={DEVELOPMENT_TENANT_ID}"
    );
    let response = app
        .clone()
        .oneshot(request("GET", &preview_path, json!(null), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let preview: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(preview["kind"], "measurement_period_preview");
    assert!(preview.get("cycle_id").is_none());
    assert!(preview.get("report_id").is_none());
    assert_eq!(preview["coverage"]["planned"], 1);
    let save_path =
        format!("/api/v1/projects/{id}/measurement-reports?tenant_id={DEVELOPMENT_TENANT_ID}");
    let body = json!({
        "start_at":preview["report_window_start_at"],"end_at":preview["report_window_end_at"],
        "report_timezone":preview["report_timezone"]
    });
    let rejected = app
        .clone()
        .oneshot(request("POST", &save_path, body.clone(), false))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    for field in ["evidence_as_of", "generated_at"] {
        let mut injected = body.clone();
        injected[field] = preview["report_window_end_at"].clone();
        let response = app
            .clone()
            .oneshot(request("POST", &save_path, injected, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    let response = app
        .clone()
        .oneshot(request("POST", &save_path, body, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let saved: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(saved["kind"], "measurement_period");
    assert!(saved.get("cycle_id").is_none());
    let response = app
        .clone()
        .oneshot(request("GET", &save_path, json!(null), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let list: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let summary = |value: &serde_json::Value| {
        let report: geo_domain::MeasurementPeriodReport =
            serde_json::from_value(value.clone()).unwrap();
        serde_json::to_value(geo_domain::MeasurementPeriodReportSummary::from(&report)).unwrap()
    };
    assert_eq!(list["items"], json!([summary(&saved)]));
    for field in [
        "samples",
        "coverage",
        "search",
        "input_hash",
        "generated_at",
    ] {
        assert!(list["items"][0].get(field).is_none());
    }
    let scoped_query = format!(
        "question_set_id={}&question_set_version_id={}",
        selected.question_set_id, selected.question_set_version_id
    );
    let scoped_preview_path = format!("{preview_path}&{scoped_query}");
    for incomplete in [
        format!("question_set_id={}", selected.question_set_id),
        format!(
            "question_set_version_id={}",
            selected.question_set_version_id
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("{preview_path}&{incomplete}"),
                json!(null),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("{save_path}&{incomplete}"),
                json!(null),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = app
        .clone()
        .oneshot(request("GET", &scoped_preview_path, json!(null), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let scoped_preview: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(scoped_preview["question_set"], json!(selected));
    assert_eq!(scoped_preview["coverage"]["planned"], 0);
    let scoped_body = json!({"start_at":saved["report_window_start_at"],"end_at":saved["report_window_end_at"],"report_timezone":saved["report_timezone"],"question_set":selected});
    let response = app
        .clone()
        .oneshot(request("POST", &save_path, scoped_body.clone(), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let scoped_saved: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_ne!(scoped_saved["report_id"], saved["report_id"]);
    assert_eq!(scoped_saved["question_set"], json!(selected));
    assert_eq!(scoped_saved["coverage"]["planned"], 0);
    for (path, expected) in [
        (save_path.clone(), saved.clone()),
        (format!("{save_path}&{scoped_query}"), scoped_saved.clone()),
    ] {
        let response = app
            .clone()
            .oneshot(request("GET", &path, json!(null), false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let rows: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(rows["items"], json!([summary(&expected)]));
    }
    let mut unknown_scope = scoped_body;
    unknown_scope["question_set"]["question_set_version_id"] = json!(Uuid::new_v4());
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &save_path, unknown_scope, true))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let detail = format!(
        "/api/v1/measurement-reports/{}?project_id={id}&tenant_id={DEVELOPMENT_TENANT_ID}",
        saved["report_id"].as_str().unwrap()
    );
    let response = app
        .oneshot(request("GET", &detail, json!(null), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(result, saved);
}
