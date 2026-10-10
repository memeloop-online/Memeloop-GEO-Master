//! P00 and HTTP share persisted cycle-free reports; no model or provider calls.
use chrono::{Duration, Utc};
use geo_api::{AppState, RepositoryHostOps};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, EffectiveObservation, FrozenQuestionBinding,
    MeasurementPeriodSample, MeasurementPeriodWindow, ProjectCreate, ProjectSettings,
    QuestionPurpose, QuestionReference, TenantScope, freeze_measurement_period,
};
use geo_worker::{
    HostOps, ReportGetRequest, ReportKind, ReportPreviewRequest, ReportPreviewResult,
    ReportReduceRequest, ReportResult,
};
use uuid::Uuid;

async fn fixture() -> (AppState, TenantScope, RepositoryHostOps) {
    let state = AppState::development_with_password("synthetic-report-tools");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic topic".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let tools =
        RepositoryHostOps::new(state.knowledge_repository()).with_report_state(state.clone());
    (state, scope, tools)
}

fn search_sample(
    purpose: Option<QuestionPurpose>,
    canary: &str,
    at: chrono::DateTime<Utc>,
) -> geo_domain::MeasurementPeriodSearchSample {
    use geo_domain::*;
    let measurement = SerpMeasurement {
        measurement_id: Uuid::new_v4(),
        source_key: format!("{canary}-source"),
        protocol: SerpProtocol {
            query: format!("{canary}-query"),
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
        question_binding: purpose.map(|purpose| FrozenQuestionBinding {
            reference: QuestionReference {
                question_set_id: Uuid::new_v4(),
                question_set_version_id: Uuid::new_v4(),
                question_id: Uuid::new_v4(),
                question_revision_id: Uuid::new_v4(),
            },
            purpose,
            split_policy_version: "synthetic.v1".into(),
        }),
        scheduled_at: at,
        created_at: at,
        state: SerpTaskState::Queued,
    };
    let url = format!("https://example.org/{canary}");
    let observation = SerpObservation {
        observation_id: Uuid::new_v4(),
        measurement_id: measurement.measurement_id,
        attempt_id: Uuid::new_v4(),
        raw_evidence_id: Uuid::new_v4(),
        raw_sha256: sha256_hex(b"synthetic"),
        parser_version: "synthetic.v1".into(),
        provider_observed_at: None,
        received_at: at,
        analyzed_at: at,
        status: SerpObservationStatus::Partial,
        actual_conditions: SerpActualConditions::default(),
        coverage: SerpCoverage {
            requested_depth: 10,
            observed_organic_depth: 1,
            pages_received: 1,
            completion: SerpCoverageCompletion::Partial,
            truncated: true,
            exhaustion_evidence_locator: None,
        },
        results: vec![SerpResult {
            kind: SerpResultKind::Organic,
            raw_kind: "organic".into(),
            raw_url: Some(url.clone()),
            normalized_url: Some(url),
            host: Some("example.org".into()),
            normalization_version: SERP_URL_RULE_VERSION.into(),
            title: Some(format!("{canary}-title")),
            page: Some(1),
            position: 1,
            organic_rank: Some(1),
            absolute_position: Some(2),
            locator: "/results/0".into(),
        }],
        source_limitations: vec![],
    };
    let target_match = observation.target_match(&measurement).unwrap();
    MeasurementPeriodSearchSample {
        cohort: MeasurementPeriodSearchIdentity::from_measurement(&measurement, at),
        evidence: Some(MeasurementPeriodSearchEvidence {
            observation,
            raw_stored_at: at,
            observation_stored_at: at,
            evidence_time: at,
            evidence_time_basis: SearchEvidenceTimeBasis::ReceivedAt,
            target_match,
        }),
    }
}

#[tokio::test]
async fn p00_period_preview_save_replay_get_list_need_no_cycle_or_setup() {
    let (state, scope, tools) = fixture().await;
    let preview = tools
        .report_preview(
            &scope,
            ReportPreviewRequest {
                kind: ReportKind::MeasurementPeriod,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let ReportPreviewResult::MeasurementPeriod(preview) = preview else {
        panic!("period preview required")
    };
    let window = MeasurementPeriodWindow {
        start_at: preview.preview.report_window_start_at,
        end_at: preview.preview.report_window_end_at,
        report_timezone: preview.preview.report_timezone.clone(),
    };
    assert_eq!(window.end_at - window.start_at, Duration::days(7));
    assert!(
        state
            .report_repository()
            .list_measurement_periods(&scope)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        serde_json::to_value(&preview)
            .unwrap()
            .get("cycle_id")
            .is_none()
    );
    let save = ReportReduceRequest {
        kind: ReportKind::MeasurementPeriod,
        window: Some(window),
        ..Default::default()
    };
    let first = tools.report_reduce(&scope, save.clone()).await.unwrap();
    let replay = tools.report_reduce(&scope, save).await.unwrap();
    assert_eq!(first, replay);
    let id = first.report_id().unwrap();
    let exact = tools
        .report_get(
            &scope,
            ReportGetRequest {
                kind: ReportKind::MeasurementPeriod,
                report_id: Some(id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(first, exact);
    assert_eq!(
        tools
            .report_get(
                &scope,
                ReportGetRequest {
                    kind: ReportKind::MeasurementPeriod,
                    ..Default::default()
                }
            )
            .await
            .unwrap(),
        first
    );
    let ReportResult::MeasurementPeriodList { items, .. } = tools
        .report_get(
            &scope,
            ReportGetRequest {
                kind: ReportKind::MeasurementPeriod,
                list: true,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    else {
        panic!("period list required")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].report.report_id, id);
    assert!(
        state
            .project_repository()
            .get(&scope, scope.project_id.unwrap())
            .await
            .unwrap()
            .unwrap()
            .current_cycle_id
            .is_none()
    );
    let mut foreign = scope.clone();
    foreign.tenant_id = Uuid::new_v4().into();
    assert!(
        tools
            .report_get(
                &foreign,
                ReportGetRequest {
                    kind: ReportKind::MeasurementPeriod,
                    report_id: Some(id),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert!(
        tools
            .report_reduce(
                &scope,
                ReportReduceRequest {
                    kind: ReportKind::MeasurementPeriod,
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    // Old default retains cycle behavior, not a fabricated placeholder cycle.
    assert!(
        tools
            .report_preview(&scope, ReportPreviewRequest::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn model_report_details_exclude_heldout_and_unclassified_without_mutating_snapshot() {
    let (state, scope, tools) = fixture().await;
    let now = Utc::now();
    let window = MeasurementPeriodWindow {
        start_at: now - Duration::days(1),
        end_at: now,
        report_timezone: "UTC".into(),
    };
    let samples = [
        (Some(QuestionPurpose::Optimization), "allowed"),
        (Some(QuestionPurpose::FrozenEvaluation), "heldout_canary"),
        (None, "unclassified_canary"),
    ]
    .into_iter()
    .map(|(purpose, canary)| {
        let at = now - Duration::hours(1);
        MeasurementPeriodSample {
            plan_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Some(Uuid::new_v4()),
            comparison_key: format!("{canary}_question"),
            question_binding: purpose.map(|purpose| FrozenQuestionBinding {
                reference: QuestionReference {
                    question_set_id: Uuid::new_v4(),
                    question_set_version_id: Uuid::new_v4(),
                    question_id: Uuid::new_v4(),
                    question_revision_id: Uuid::new_v4(),
                },
                purpose,
                split_policy_version: "synthetic.v1".into(),
            }),
            scheduled_at: at,
            original_status: "observed".into(),
            observed_live: true,
            observation: Some(EffectiveObservation {
                raw_answer: format!("{canary}_answer"),
                citations: vec![format!("https://example.org/{canary}")],
                observed_at: at,
                received_at: at,
                provenance: None,
            }),
        }
    })
    .collect();
    let search = vec![
        search_sample(
            Some(QuestionPurpose::Optimization),
            "search_allowed",
            now - Duration::hours(1),
        ),
        search_sample(
            Some(QuestionPurpose::FrozenEvaluation),
            "search_heldout_canary",
            now - Duration::hours(1),
        ),
        search_sample(None, "search_adhoc_allowed", now - Duration::hours(1)),
    ];
    let preview = geo_domain::preview_measurement_period_with_search(
        &scope,
        &window,
        samples,
        Some(search),
        now,
    )
    .unwrap();
    let stored = freeze_measurement_period(&scope, preview.clone(), 1, None).unwrap();
    state
        .report_repository()
        .create_measurement_period(&scope, stored.clone())
        .await
        .unwrap();
    for request in [
        ReportGetRequest {
            kind: ReportKind::MeasurementPeriod,
            report_id: Some(stored.report_id),
            ..Default::default()
        },
        ReportGetRequest {
            kind: ReportKind::MeasurementPeriod,
            list: true,
            ..Default::default()
        },
    ] {
        let result = tools.report_get(&scope, request).await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("allowed_answer"));
        assert!(!text.contains("heldout_canary"));
        assert!(!text.contains("unclassified_canary"));
        assert!(!text.contains("search_heldout_canary"));
        assert!(text.contains("search_allowed-title"));
        assert!(text.contains("search_adhoc_allowed-title"));
        assert!(text.contains("\"omitted_search_sample_details\":1"));
        assert!(text.contains("\"omitted_sample_details\":2"));
        assert!(text.contains("\"planned\":3"));
    }
    let projection = geo_worker::MeasurementPeriodPreviewProjection::from(preview);
    let text = serde_json::to_string(&projection).unwrap();
    assert!(!text.contains("heldout_canary"));
    assert!(!text.contains("unclassified_canary"));
    assert!(!text.contains("search_heldout_canary"));
    assert_eq!(projection.omitted_sample_details, 2);
    assert_eq!(projection.omitted_search_sample_details, 1);
    assert_eq!(
        projection.preview.search.as_ref().unwrap().coverage.planned,
        3
    );
    assert_eq!(
        state
            .report_repository()
            .get_measurement_period(&scope, stored.report_id)
            .await
            .unwrap(),
        stored
    );
}
