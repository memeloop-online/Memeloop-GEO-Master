//! Synthetic, in-process API coverage; no external search acceptance is implied.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use chrono::{Duration, Utc};
use geo_api::{AppState, CSRF_HEADER, router};
use geo_domain::{
    ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelStatus, DEVELOPMENT_OPERATOR_ID,
    DEVELOPMENT_TENANT_ID, ProjectCreate, ProjectSettings, TenantScope,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn request(
    method: &str,
    path: &str,
    auth: Option<&(String, String)>,
    body: Value,
) -> Request<Body> {
    let path = format!(
        "{path}{}tenant_id={DEVELOPMENT_TENANT_ID}",
        if path.contains('?') { "&" } else { "?" }
    );
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if let Some((cookie, csrf)) = auth {
        builder = builder.header("cookie", cookie).header(CSRF_HEADER, csrf);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn login(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({"login_name":"demo@localhost","password":"measurement-test"}),
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
    (
        cookie,
        body(response).await["csrf_token"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

async fn fixture() -> (AppState, TenantScope, Uuid) {
    let state = AppState::development_with_password("measurement-test");
    let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &tenant,
            ProjectCreate {
                slug: None,
                display_name: "Measurement workspace".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    assert!(project.current_cycle_id.is_none());
    let scope = TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id));
    let account_id = Uuid::new_v4();
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
                    status: ChannelStatus::NeedsLogin,
                    display_name: None,
                    platform_account_id: None,
                    avatar_url: None,
                    enabled: true,
                    proxy_configured: false,
                    proxy_server: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                session: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
    (state, scope, account_id)
}

fn plan(account: Uuid) -> Value {
    json!({"idempotency_key":"standalone-v1","title":"Independent topic",
        "measurements":[{"account_id":account,"provider":"kimi","model":"fixed",
            "surface":"consumer_web","search_mode":"web_search","protocol_version":"v1",
            "question_set_version":"ad-hoc-v1","question":"How does a rain gauge work?",
            "market":"CN","language":"en","scheduled_at":Utc::now()-Duration::seconds(1),
            "sample_ordinal":0}]})
}

#[tokio::test]
async fn draft_without_cycle_freezes_replays_conflicts_and_scans_due_targets() {
    let (state, scope, account) = fixture().await;
    let app = router(state.clone());
    let auth = login(&app).await;
    let path = format!(
        "/api/v1/projects/{}/measurement-plans",
        scope.project_id.unwrap()
    );
    let input = plan(account);
    let mut duplicates = input.clone();
    duplicates["measurements"] = json!([input["measurements"][0], input["measurements"][0]]);
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &path, Some(&auth), duplicates))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut oversized = input.clone();
    oversized["measurements"] = json!(vec![input["measurements"][0].clone(); 101]);
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &path, Some(&auth), oversized))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let response = app
        .clone()
        .oneshot(request("POST", &path, Some(&auth), input.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let frozen = body(response).await;
    assert!(frozen.get("cycle_id").is_none());
    assert!(frozen.get("idempotency_key").is_none());
    assert_eq!(
        frozen["targets"][0]["input"]["question"],
        input["measurements"][0]["question"]
    );
    let replay = app
        .clone()
        .oneshot(request("POST", &path, Some(&auth), input.clone()))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(body(replay).await, frozen);
    let mut changed = input.clone();
    changed["title"] = json!("Changed");
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &path, Some(&auth), changed))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let due = state
        .channel_job_repository()
        .scan_pending(None, Utc::now(), 100)
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        due[0].target_id.to_string(),
        frozen["targets"][0]["target_id"]
    );
    let detail = format!("{path}/{}", frozen["plan_id"].as_str().unwrap());
    let fetched = app
        .clone()
        .oneshot(request("GET", &detail, Some(&auth), Value::Null))
        .await
        .unwrap();
    assert_eq!(body(fetched).await, frozen);
    let listed = app
        .clone()
        .oneshot(request("GET", &path, Some(&auth), Value::Null))
        .await
        .unwrap();
    let listed = body(listed).await;
    assert_eq!(listed["items"], json!([frozen]));
    assert!(listed["next_after"].is_null());
    let mut future = input.clone();
    future["idempotency_key"] = json!("future");
    future["measurements"][0]["scheduled_at"] = json!(Utc::now() + Duration::days(1));
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &path, Some(&auth), future))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        state
            .channel_job_repository()
            .scan_pending(None, Utc::now(), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let page = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("{path}?limit=1"),
            Some(&auth),
            Value::Null,
        ))
        .await
        .unwrap();
    let page = body(page).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let cursor = page["next_after"].as_str().unwrap();
    let next = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("{path}?limit=1&after={cursor}"),
            Some(&auth),
            Value::Null,
        ))
        .await
        .unwrap();
    let next = body(next).await;
    assert_eq!(next["items"].as_array().unwrap().len(), 1);
    assert!(next["next_after"].is_null());
    assert_ne!(next["items"][0]["plan_id"], page["items"][0]["plan_id"]);
    state
        .channel_service()
        .repository
        .delete_account(&scope, account)
        .await
        .unwrap();
    let replay = app
        .clone()
        .oneshot(request("POST", &path, Some(&auth), input))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(body(replay).await["plan_id"], frozen["plan_id"]);
}

#[tokio::test]
async fn strict_body_scope_and_empty_plan_are_rejected() {
    let (state, scope, account) = fixture().await;
    let app = router(state.clone());
    let auth = login(&app).await;
    let path = format!(
        "/api/v1/projects/{}/measurement-plans",
        scope.project_id.unwrap()
    );
    let mut invalid = plan(account);
    invalid["publications"] = json!([]);
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &path, Some(&auth), invalid))
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                &path,
                Some(&auth),
                json!({"idempotency_key":"empty","title":"Empty","measurements":[]})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let input = plan(account);
    let response = app
        .clone()
        .oneshot(request("POST", &path, Some(&auth), input.clone()))
        .await
        .unwrap();
    let frozen = body(response).await;
    let other = state
        .project_repository()
        .create(
            &TenantScope::new(scope.operator_id, scope.tenant_id, None),
            ProjectCreate {
                slug: None,
                display_name: "Other workspace".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let other_path = format!(
        "/api/v1/projects/{}/measurement-plans/{}",
        other.id,
        frozen["plan_id"].as_str().unwrap()
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", &other_path, Some(&auth), Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let target_path = format!(
        "/api/v1/projects/{}/channel-targets/{}",
        other.id,
        frozen["targets"][0]["target_id"].as_str().unwrap()
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", &target_path, Some(&auth), Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_ne!(
        app.oneshot(request("POST", &path, None, input))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}
