//! Synthetic API tests: no external search or publishing acceptance is implied.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{Duration, Utc};
use geo_api::{AppState, router};
use geo_domain::{
    ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ChannelTargetInput,
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ProjectCreate, ProjectSettings,
    StandaloneMeasurementPlan, TenantScope, sha256_hex,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn request(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(format!(
            "{path}{}tenant_id={DEVELOPMENT_TENANT_ID}",
            if path.contains('?') { "&" } else { "?" }
        ))
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::empty()).unwrap()
}

async fn response_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn login(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/auth/login?tenant_id={DEVELOPMENT_TENANT_ID}"
                ))
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "login_name":"demo@localhost",
                        "password":"citation-test"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn new_project(state: &AppState) -> TenantScope {
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Generic project".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id))
}

async fn add_plan(state: &AppState, scope: &TenantScope, plan_id: Uuid) -> ChannelTarget {
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
            scheduled_at: Utc::now(),
            sample_ordinal: 0,
            question_binding: None,
        },
    };
    let plan = StandaloneMeasurementPlan {
        plan_id,
        project_id: scope.project_id.unwrap(),
        title: "Topic".into(),
        input_hash: plan_id.to_string(),
        revision: 1,
        created_at: Utc::now(),
        targets: vec![target.clone()],
    };
    state
        .channel_job_repository()
        .create_measurement_plan(scope, &plan_id.to_string(), &plan_id.to_string(), plan)
        .await
        .unwrap();
    target
}

async fn complete_live_search(
    state: &AppState,
    scope: &TenantScope,
    target: &ChannelTarget,
) -> Uuid {
    let claimed = Utc::now();
    let attempt_id = Uuid::new_v4();
    state
        .channel_job_repository()
        .claim(scope, target.target_id, attempt_id, claimed)
        .await
        .unwrap();
    let observed_at = claimed + Duration::milliseconds(100);
    let completed_at = claimed + Duration::milliseconds(200);
    let received_at = claimed + Duration::milliseconds(300);
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
    let citations = vec!["https://example.org/article".to_owned()];
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
        "raw_answer":"Example answer",
        "citations":citations,
        "search_event":{
            "kind":"official_search_event",
            "event_id":"event-1",
            "request_id":"request-1",
            "occurred_at":observed_at,
            "source":"provider_search_event",
            "provenance":"live"
        }
    });
    let marker = json!({
        "kind":"runner_receipt",
        "schema_version":"geo.runner.receipt.v1",
        "execution_id":attempt_id,
        "provenance":"live",
        "connector_version":"connector-v1",
        "occurred_at":completed_at
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
                occurred_at: completed_at,
                raw_answer: Some("Example answer".into()),
                citations,
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("connector-v1".into()),
                runner_evidence: vec![evidence, marker],
                fixture: false,
            },
            received_at,
        )
        .await
        .unwrap();
    attempt_id
}

#[tokio::test]
async fn paged_denominator_and_cross_project_isolation_are_explicit() {
    let state = AppState::development_with_password("citation-test");
    let scope = new_project(&state).await;
    let other_scope = new_project(&state).await;
    let plan_ids = [
        Uuid::parse_str("00000000-0000-0000-0000-000000000011").unwrap(),
        Uuid::parse_str("00000000-0000-0000-0000-000000000012").unwrap(),
        Uuid::parse_str("00000000-0000-0000-0000-000000000013").unwrap(),
    ];
    for (index, plan_id) in plan_ids.into_iter().enumerate() {
        let target = add_plan(&state, &scope, plan_id).await;
        if index == 0 {
            complete_live_search(&state, &scope, &target).await;
        }
    }
    add_plan(&state, &other_scope, Uuid::new_v4()).await;
    let app = router(state);
    let cookie = login(&app).await;
    let base = format!(
        "/api/v1/projects/{}/citation-insights",
        scope.project_id.unwrap()
    );
    let unauthenticated = app.clone().oneshot(request(&base, None)).await.unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let first = app
        .clone()
        .oneshot(request(&format!("{base}?limit=2"), Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = response_body(first).await;
    assert_eq!(first["scope"], "returned_plans_only");
    assert_eq!(first["coverage"]["planned"], 2);
    assert_eq!(first["coverage"]["pending"], 1);
    assert_eq!(first["coverage"]["observed_live"], 1);
    assert_eq!(first["observed_sources"][0]["host"], "example.org");
    assert_eq!(
        first["observed_sources"][0]["urls"][0]["url"],
        "https://example.org/article"
    );
    assert_eq!(first["observed_sources"][0]["citing_answers"], 1);
    assert_eq!(first["plan_ids"], json!(&plan_ids[..2]));
    assert_eq!(first["next_after"], json!(plan_ids[1]));
    let recommendations_path = format!(
        "/api/v1/projects/{}/source-channel-recommendations?limit=2",
        scope.project_id.unwrap()
    );
    let recommendations = app
        .clone()
        .oneshot(request(&recommendations_path, Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(recommendations.status(), StatusCode::OK);
    let recommendations = response_body(recommendations).await;
    assert_eq!(recommendations["scope"], "returned_plans_only");
    assert_eq!(recommendations["coverage"]["planned"], 2);
    assert_eq!(
        recommendations["items"][0]["source_hosts"],
        json!(["example.org"])
    );
    assert_eq!(recommendations["items"][0]["platform_id"], Value::Null);
    assert_eq!(recommendations["items"][0]["citing_answers"], 1);
    assert_eq!(
        recommendations["items"][0]["publication"]["connector_availability"],
        "unmapped"
    );
    let second = app
        .clone()
        .oneshot(request(
            &format!("{base}?limit=2&after={}", plan_ids[1]),
            Some(&cookie),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second = response_body(second).await;
    assert_eq!(second["coverage"]["planned"], 1);
    assert_eq!(second["coverage"]["observed_live"], 0);
    assert_eq!(second["observed_sources"], json!([]));
    assert_eq!(second["next_after"], Value::Null);
    assert_eq!(second["plan_ids"], json!([plan_ids[2]]));
    let invalid_limit = app
        .clone()
        .oneshot(request(&format!("{base}?limit=11"), Some(&cookie)))
        .await
        .unwrap();
    assert_eq!(invalid_limit.status(), StatusCode::BAD_REQUEST);
}
