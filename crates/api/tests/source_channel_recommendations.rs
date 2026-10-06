//! Synthetic in-process evidence proves configuration flow, not real platform acceptance.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{Duration, Utc};
use geo_api::{AppState, CSRF_HEADER, RepositoryHostOps, router};
use geo_domain::{
    ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ChannelTargetInput,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, FrozenQuestionBinding, InitialSource,
    InitialSourceKind, InitialSourceVisibility, ProjectCreate, ProjectSettings,
    ProjectStartCommand, QuestionPurpose, QuestionReference, StandaloneMeasurementPlan,
    TenantScope, hash_idempotency_key, settings_hash, sha256_hex, start_request_hash,
};
use geo_worker::{HostOps, SourceRecommendationsRequest};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn request(
    method: &str,
    path: &str,
    auth: Option<&(String, String)>,
    revision: Option<i64>,
    body: Value,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("{path}?tenant_id={DEVELOPMENT_TENANT_ID}"))
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if let Some((cookie, csrf)) = auth {
        builder = builder.header("cookie", cookie).header(CSRF_HEADER, csrf);
    }
    if let Some(revision) = revision {
        builder = builder
            .header("if-match", revision.to_string())
            .header("idempotency-key", "synthetic-target-edit");
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 128 * 1024).await.unwrap()).unwrap()
}

async fn login(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":"demo@localhost","password":"synthetic-recommendation-test"}),
        ))
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
    let csrf = body(response).await["csrf_token"]
        .as_str()
        .unwrap()
        .to_owned();
    (cookie, csrf)
}

async fn accepted_search(
    state: &AppState,
    scope: &TenantScope,
    plan_id: Uuid,
    purpose: Option<QuestionPurpose>,
    citation: &str,
    mixed_frozen: bool,
) {
    let answer = match purpose {
        Some(QuestionPurpose::Optimization) => "Synthetic optimization answer",
        Some(QuestionPurpose::FrozenEvaluation) => "SYNTHETIC_EXCLUDED_FROZEN_ANSWER",
        None => "SYNTHETIC_EXCLUDED_UNKNOWN_ANSWER",
    };
    let claimed = Utc::now();
    let question_set_version_id = Uuid::new_v4();
    let target = ChannelTarget {
        target_id: Uuid::new_v4(),
        input: ChannelTargetInput::Measure {
            account_id: Uuid::new_v4(),
            provider: "synthetic-provider".into(),
            model: "synthetic-model".into(),
            surface: "consumer_web".into(),
            search_mode: "web_search".into(),
            protocol_version: "v1".into(),
            question_set_version: question_set_version_id.to_string(),
            question: "How does a rain gauge work?".into(),
            market: "generic".into(),
            language: "en".into(),
            scheduled_at: claimed,
            sample_ordinal: 0,
            question_binding: purpose.map(|purpose| FrozenQuestionBinding {
                reference: QuestionReference {
                    question_set_id: Uuid::new_v4(),
                    question_set_version_id,
                    question_id: Uuid::new_v4(),
                    question_revision_id: Uuid::new_v4(),
                },
                purpose,
                split_policy_version: "synthetic_v1".into(),
            }),
        },
    };
    let frozen_target = mixed_frozen.then(|| {
        let mut frozen = target.clone();
        frozen.target_id = Uuid::new_v4();
        if let ChannelTargetInput::Measure {
            question,
            question_binding,
            ..
        } = &mut frozen.input
        {
            *question = "SYNTHETIC_EXCLUDED_FROZEN_QUESTION".into();
            question_binding
                .as_mut()
                .expect("bound optimization target")
                .purpose = QuestionPurpose::FrozenEvaluation;
        }
        frozen
    });
    let mut targets = vec![target.clone()];
    targets.extend(frozen_target.iter().cloned());
    state
        .channel_job_repository()
        .create_measurement_plan(
            scope,
            &plan_id.to_string(),
            &plan_id.to_string(),
            StandaloneMeasurementPlan {
                plan_id,
                project_id: scope.project_id.unwrap(),
                title: "Synthetic topic".into(),
                input_hash: plan_id.to_string(),
                revision: 1,
                created_at: claimed,
                targets,
            },
        )
        .await
        .unwrap();
    finish_accepted_search(state, scope, &target, citation, answer).await;
    if let Some(frozen_target) = frozen_target.as_ref() {
        finish_accepted_search(
            state,
            scope,
            frozen_target,
            "https://excluded-mixed.invalid/SYNTHETIC_EXCLUDED_MIXED_CITATION",
            "SYNTHETIC_EXCLUDED_MIXED_ANSWER",
        )
        .await;
    }
}

async fn finish_accepted_search(
    state: &AppState,
    scope: &TenantScope,
    target: &ChannelTarget,
    citation: &str,
    answer: &str,
) {
    let claimed = Utc::now();
    let observed = claimed + Duration::milliseconds(100);
    let completed = claimed + Duration::milliseconds(200);
    let received = claimed + Duration::milliseconds(300);
    let attempt_id = Uuid::new_v4();
    let ChannelTargetInput::Measure {
        account_id,
        provider,
        model,
        surface,
        search_mode,
        protocol_version,
        question_set_version,
        question,
        market,
        language,
        scheduled_at,
        sample_ordinal,
        ..
    } = &target.input
    else {
        unreachable!()
    };
    state
        .channel_job_repository()
        .claim(scope, target.target_id, attempt_id, claimed)
        .await
        .unwrap();
    // The exact same receipt acceptance checks are used by the recommendation
    // read; no external account, search, or publishing service is invoked.
    let citations = vec![citation.to_owned()];
    let evidence = json!({
        "kind":"official_search_observation",
        "schema_version":"geo.measure.official_search.v1",
        "target_id":target.target_id,
        "account_id":account_id,
        "provider":provider,
        "model":model,
        "surface":surface,
        "search_mode":search_mode,
        "protocol_version":protocol_version,
        "question_set_version":question_set_version,
        "question_sha256":sha256_hex(question.as_bytes()),
        "market":market,
        "language":language,
        "scheduled_at":scheduled_at,
        "sample_ordinal":sample_ordinal,
        "connector_version":"connector-v1",
        "provenance":"live",
        "disposition":"observed",
        "raw_answer":answer,
        "citations":citations,
        "search_event":{
            "kind":"official_search_event",
            "event_id":"synthetic-event",
            "request_id":"synthetic-request",
            "occurred_at":observed,
            "source":"provider_search_event",
            "provenance":"live"
        }
    });
    let receipt = json!({
        "kind":"runner_receipt",
        "schema_version":"geo.runner.receipt.v1",
        "execution_id":attempt_id,
        "provenance":"live",
        "connector_version":"connector-v1",
        "occurred_at":completed
    });
    state
        .channel_job_repository()
        .finish(
            scope,
            target.target_id,
            attempt_id,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Observed,
                detail: None,
                occurred_at: completed,
                raw_answer: Some(answer.into()),
                citations,
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("connector-v1".into()),
                runner_evidence: vec![evidence, receipt],
                fixture: false,
            },
            received,
        )
        .await
        .unwrap();
}

async fn pending_optimization_plan(state: &AppState, scope: &TenantScope, plan_id: Uuid) {
    let version = Uuid::new_v4();
    let plan = StandaloneMeasurementPlan {
        plan_id,
        project_id: scope.project_id.unwrap(),
        title: "Synthetic pending topic".into(),
        input_hash: plan_id.to_string(),
        revision: 1,
        created_at: Utc::now(),
        targets: vec![ChannelTarget {
            target_id: Uuid::new_v4(),
            input: ChannelTargetInput::Measure {
                account_id: Uuid::new_v4(),
                provider: "synthetic-provider".into(),
                model: "synthetic-model".into(),
                surface: "consumer_web".into(),
                search_mode: "web_search".into(),
                protocol_version: "v1".into(),
                question_set_version: version.to_string(),
                question: "Synthetic pending question".into(),
                market: "generic".into(),
                language: "en".into(),
                scheduled_at: Utc::now(),
                sample_ordinal: 0,
                question_binding: Some(FrozenQuestionBinding {
                    reference: QuestionReference {
                        question_set_id: Uuid::new_v4(),
                        question_set_version_id: version,
                        question_id: Uuid::new_v4(),
                        question_revision_id: Uuid::new_v4(),
                    },
                    purpose: QuestionPurpose::Optimization,
                    split_policy_version: "synthetic_v1".into(),
                }),
            },
        }],
    };
    state
        .channel_job_repository()
        .create_measurement_plan(scope, &plan_id.to_string(), &plan_id.to_string(), plan)
        .await
        .unwrap();
}

#[tokio::test]
async fn optimization_cursor_skips_excluded_only_pages_without_exposing_evaluation_evidence() {
    let state = AppState::development_with_password("synthetic-recommendation-test");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic project".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let first = Uuid::from_u128(0x100);
    let pending = Uuid::from_u128(0x180);
    let second = Uuid::from_u128(0x200);
    let mut excluded = Vec::new();
    for id in 1..=30u128 {
        let purpose = if id % 2 == 0 {
            Some(QuestionPurpose::FrozenEvaluation)
        } else {
            None
        };
        let plan_id = Uuid::from_u128(if id <= 15 { id } else { 0x100 + id - 15 });
        accepted_search(
            &state,
            &scope,
            plan_id,
            purpose,
            if purpose.is_some() {
                "https://excluded-frozen.invalid/SYNTHETIC_EXCLUDED_FROZEN_CITATION"
            } else {
                "https://excluded-unknown.invalid/SYNTHETIC_EXCLUDED_UNKNOWN_CITATION"
            },
            false,
        )
        .await;
        excluded.push(plan_id);
        if id == 15 {
            accepted_search(
                &state,
                &scope,
                first,
                Some(QuestionPurpose::Optimization),
                "https://medium.com/synthetic-eligible-first",
                true,
            )
            .await;
        }
    }
    pending_optimization_plan(&state, &scope, pending).await;
    accepted_search(
        &state,
        &scope,
        second,
        Some(QuestionPurpose::Optimization),
        "https://medium.com/synthetic-eligible-second",
        false,
    )
    .await;
    let ops = RepositoryHostOps::new(state.knowledge_repository()).with_content(state.clone());
    let page = ops
        .source_recommendations(
            &scope,
            SourceRecommendationsRequest {
                after: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.plan_ids, vec![first]);
    assert_eq!(page.next_after, Some(first));
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].citing_answers, 1);
    let serialized = serde_json::to_string(&page).unwrap();
    assert!(!serialized.contains("SYNTHETIC_EXCLUDED"));
    assert!(!serialized.contains("excluded-frozen.invalid"));
    assert!(!serialized.contains("excluded-unknown.invalid"));
    assert!(!serialized.contains("excluded-mixed.invalid"));
    assert!(!serialized.contains("\"coverage\""));
    for id in &excluded {
        assert!(!serialized.contains(&id.to_string()));
    }
    let page = ops
        .source_recommendations(
            &scope,
            SourceRecommendationsRequest {
                after: page.next_after,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.plan_ids, vec![pending]);
    assert_eq!(page.next_after, Some(pending));
    assert!(page.items.is_empty());
    let serialized = serde_json::to_string(&page).unwrap();
    assert!(!serialized.contains("SYNTHETIC_EXCLUDED"));
    for id in &excluded {
        assert!(!serialized.contains(&id.to_string()));
    }
    let page = ops
        .source_recommendations(
            &scope,
            SourceRecommendationsRequest {
                after: page.next_after,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.plan_ids, vec![second]);
    assert_eq!(page.next_after, None);
    assert_eq!(page.items[0].citing_answers, 1);
    let serialized = serde_json::to_string(&page).unwrap();
    assert!(!serialized.contains("SYNTHETIC_EXCLUDED"));
    for id in &excluded {
        assert!(!serialized.contains(&id.to_string()));
    }
}

#[tokio::test]
async fn accepted_recommendation_updates_only_future_cycle_distribution_configuration() {
    let state = AppState::development_with_password("synthetic-recommendation-test");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Synthetic project".into(),
                settings: ProjectSettings {
                    brand_name: "Generic".into(),
                    market: "generic".into(),
                    language: "en".into(),
                    initial_sources: vec![InitialSource {
                        kind: InitialSourceKind::Text,
                        value: "Generic product source".into(),
                        visibility: InitialSourceVisibility::Public,
                        version_ref: None,
                        content_hash: None,
                    }],
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let old_hash = settings_hash(&project.settings.clone().validate_start().unwrap()).unwrap();
    let started = state
        .project_repository()
        .start(
            &scope,
            project.id,
            ProjectStartCommand {
                expected_revision: project.revision,
                idempotency_key_hash: hash_idempotency_key("synthetic-start"),
                request_hash: start_request_hash(project.id, project.revision, &old_hash),
                settings_hash: old_hash,
                operation_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    let original = state
        .project_repository()
        .get_cycle_settings(&scope, project.id, started.cycle_id)
        .await
        .unwrap()
        .unwrap();
    accepted_search(
        &state,
        &scope,
        Uuid::new_v4(),
        Some(QuestionPurpose::Optimization),
        "https://medium.com/generic-test-article",
        false,
    )
    .await;
    let app = router(state.clone());
    let auth = login(&app).await;
    let path = format!("/api/v1/projects/{}", project.id);
    let recommended = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("{path}/source-channel-recommendations"),
            Some(&auth),
            None,
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(recommended.status(), StatusCode::OK);
    let recommended = body(recommended).await;
    assert_eq!(recommended["coverage"]["observed_live"], 1);
    assert_eq!(
        recommended["items"][0]["source_hosts"],
        json!(["medium.com"])
    );
    assert_eq!(
        recommended["items"][0]["samples"][0]["question_purpose"],
        "optimization"
    );
    assert_eq!(
        recommended["items"][0]["publication"]["connector_availability"],
        "unavailable"
    );
    let channel = recommended["items"][0]["platform_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = app
        .clone()
        .oneshot(request("GET", &path, Some(&auth), None, json!({})))
        .await
        .unwrap();
    assert_eq!(before.status(), StatusCode::OK);
    let before = body(before).await;
    let revision = before["revision"].as_i64().unwrap();
    let target_scope = json!({
        "mode":"explicit",
        "included_platform_ids":[channel],
        "excluded_platform_ids":[],
        "resource_pool_ids":[],
        "replication_policy":"one_account_per_platform"
    });
    let updated = app
        .clone()
        .oneshot(request(
            "PATCH",
            &path,
            Some(&auth),
            Some(revision),
            json!({"revision":revision,"distribution_scope":target_scope}),
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = body(updated).await;
    assert_eq!(updated["settings"]["distribution_scope"], target_scope);
    let persisted = app
        .clone()
        .oneshot(request("GET", &path, Some(&auth), None, json!({})))
        .await
        .unwrap();
    assert_eq!(persisted.status(), StatusCode::OK);
    assert_eq!(body(persisted).await, updated);
    let successor = state
        .project_repository()
        .schedule_next_cycle(
            &scope,
            project.id,
            started.cycle_id,
            Utc::now() + Duration::days(120),
        )
        .await
        .unwrap();
    let successor_settings = state
        .project_repository()
        .get_cycle_settings(&scope, project.id, successor.cycle_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&successor_settings.distribution_scope).unwrap(),
        target_scope
    );
    assert_eq!(successor_settings.report_timezone, original.report_timezone);
    assert_eq!(successor_settings.report_schedule, original.report_schedule);
    assert_eq!(
        state
            .project_repository()
            .get_cycle_settings(&scope, project.id, started.cycle_id)
            .await
            .unwrap(),
        Some(original)
    );
}
