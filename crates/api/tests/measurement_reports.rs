//! Synthetic persisted-evidence tests; no provider or model requests.
use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{DateTime, Duration, Utc};
use geo_api::{
    AppState, MeasurementPeriodRequest, ObservationEvidenceResolver, preview_project_measurements,
    save_project_measurement_report,
};
use geo_domain::{
    ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ChannelTargetInput,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ErrorCode, MeasurementPeriodWindow,
    MemoryObservationAnalysisRepository, MemoryObservationCaptureRepository,
    ObservationAnalysisOutcome, ObservationAnalysisRepository, ObservationAnalysisRequest,
    ObservationAnalysisResult, ObservationAnalysisSource, ProjectCreate, ProjectSettings,
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
    use geo_domain::*;
    let now = Utc::now();
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
        captured_at: Utc::now(),
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
                captured_at: Utc::now(),
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
    plan(&state, &scope, at).await;
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
    let fresh = preview_project_measurements(&state, &scope, Some(first.window()), report_now)
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

async fn plan(state: &AppState, scope: &TenantScope, at: DateTime<Utc>) -> (Uuid, ChannelTarget) {
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
            question_binding: None,
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
    let (_, target) = plan(&state, &scope, at).await;
    let jobs = state.channel_job_repository();
    let captures = Arc::new(MemoryObservationCaptureRepository::new(jobs.clone()));
    let analyses = Arc::new(MemoryObservationAnalysisRepository::new(
        jobs.clone(),
        captures.clone(),
    ));
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
    let preview = preview_project_measurements(&state, &scope, Some(window.clone()), now)
        .await
        .unwrap();
    assert_eq!(preview.coverage.planned, 1);
    assert_eq!(preview.coverage.counts["unknown"], 1);
    assert!(
        state
            .report_repository()
            .list_measurement_periods(&scope)
            .await
            .unwrap()
            .is_empty()
    );
    let request = MeasurementPeriodRequest {
        start_at: window.start_at,
        end_at: window.end_at,
        report_timezone: window.report_timezone.clone(),
        correction_of: None,
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
    analyses
        .create(
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
    let claim = analyses
        .claim(&scope, revision_id, analyzed)
        .await
        .unwrap()
        .unwrap();
    analyses.finish(&scope, revision_id, claim.claim_token, ObservationAnalysisResult {
        config_revision: Some(1), actual_model: Some("parser-model".into()),
        candidate_json: Some(json!({"completion":"complete","search_used":"yes"}).to_string()),
        outcome: ObservationAnalysisOutcome::Grounded {
            raw_answer: "A saved answer.".into(), citations: vec!["https://example.org/source".into()],
            audit: json!({
                "kind":"observation_extraction","method":"llm_grounded","prompt_version":"prompt.v1",
                "source_sha256":digest,"protocol":{
                    "provider":"provider-a","model":"model-a","surface":"consumer_web",
                    "search_mode":"web_search","protocol_version":"v1","market":"generic","language":"en"
                },"refs":[{"role":"answer_segment"},{"role":"completion"},{"role":"search_activity"}]
            }),
        }, prompt_tokens: 1, completion_tokens: 1,
    }, analyzed).await.unwrap();
    // A newly persisted historical plan may enter a fresh preview, never
    // inflate the first saved report's frozen sample denominator.
    plan(&state, &scope, at + Duration::minutes(1)).await;
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
        )
        .await;
    }
    plan(&state, &scope, now).await;
    plan(&state, &scope, now - Duration::days(8)).await;
    let preview = preview_project_measurements(&state, &scope, None, now)
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
        preview_project_measurements(&state, &scope, Some(invalid), now)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn http_cycle_free_preview_save_list_and_detail_share_the_same_snapshot() {
    let state = AppState::development_with_password("report-test");
    let scope = project(&state).await;
    let id = scope.project_id.unwrap();
    plan(&state, &scope, Utc::now() - Duration::hours(1)).await;
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
    assert_eq!(list["items"], json!([saved.clone()]));
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
