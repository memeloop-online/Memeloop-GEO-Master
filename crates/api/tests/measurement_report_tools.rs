//! P00 and HTTP share persisted cycle-free reports; no model or provider calls.
use chrono::{Duration, Utc};
use geo_api::{AppState, RepositoryHostOps};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, EffectiveObservation, FrozenQuestionBinding,
    MeasurementPeriodSample, MeasurementPeriodWindow, ProjectCreate, ProjectSettings,
    QuestionPurpose, QuestionReference, TenantScope, freeze_measurement_period,
    preview_measurement_period,
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
    let preview = preview_measurement_period(&scope, &window, samples, now).unwrap();
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
        assert!(text.contains("\"omitted_sample_details\":2"));
        assert!(text.contains("\"planned\":3"));
    }
    let projection = geo_worker::MeasurementPeriodPreviewProjection::from(preview);
    let text = serde_json::to_string(&projection).unwrap();
    assert!(!text.contains("heldout_canary"));
    assert!(!text.contains("unclassified_canary"));
    assert_eq!(projection.omitted_sample_details, 2);
    assert_eq!(
        state
            .report_repository()
            .get_measurement_period(&scope, stored.report_id)
            .await
            .unwrap(),
        stored
    );
}
