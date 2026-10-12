use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{
    AppState, BrowserBridge, CSRF_HEADER, ChannelService, EventBus, MemoryIdempotencyStore,
    MemoryOperationStore, router,
};
use geo_domain::{
    DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, Membership, MemoryAuthRepository,
    MemoryProjectRepository, ProjectCreate, ProjectSettings, Role, TenantScope, UserId,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

async fn fixture() -> (Router, String, String, String) {
    let state = AppState::development_with_password("channel-test");
    let scope = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &scope,
            ProjectCreate {
                slug: Some("channel-test".into()),
                display_name: "Channel test".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":"demo@localhost","password":"channel-test"}).to_string(),
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
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16 * 1024).await.unwrap()).unwrap();
    (
        app,
        project.id.to_string(),
        cookie,
        body["csrf_token"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn fixed_pool_revision_runs_and_switch_waits_for_exact_disposal() {
    use axum::{
        Json,
        extract::{Path, State},
        routing::{get, post},
    };
    use tokio::sync::Mutex;
    #[derive(Clone, Default)]
    struct Capture {
        starts: Arc<Mutex<Vec<Value>>>,
        fail_close: Arc<Mutex<bool>>,
    }
    let capture = Capture::default();
    let runner=Router::new()
        .route("/v1/lifecycle",get(||async {Json(json!({"generation":Uuid::from_u128(321)}))}))
        .route("/v1/sessions",post(|State(s):State<Capture>,Json(input):Json<Value>|async move{
            s.starts.lock().await.push(input.clone());
            Json(json!({"session_id":input["session_id"],"generation":input["expected_generation"]}))
        }))
        .route("/v1/sessions/{id}/dispose",post(|State(s):State<Capture>,Path(id):Path<Uuid>,Json(input):Json<Value>|async move{
            if *s.fail_close.lock().await {(StatusCode::NOT_FOUND,Json(json!({})))}
            else {(StatusCode::OK,Json(json!({"session_id":id,"generation":input["expected_generation"],"state":"disposed"})))}
        })).with_state(capture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, runner).await.unwrap() });
    let browser = BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap();
    let (app, project, cookie, csrf, state) =
        pool_fixture_with_browser_state(Role::ResourceAdmin, true, Some(browser)).await;
    let call = |method: &str, path: &str, value: Value| {
        request(method, path, Some(&cookie), Some(&csrf), value.to_string())
    };
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/operator/channel-accounts",
            json!({"platform":"zhihu"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let account = json_body(response).await;
    let id = account["account_id"].as_str().unwrap();
    let network_path = format!("/api/v1/operator/channel-accounts/{id}/network-binding");
    let response = app
        .clone()
        .oneshot(call("GET", &network_path, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let initial = json_body(response).await;
    assert_eq!(initial["version"], 0);
    assert!(initial["active"]["binding_id"].is_null());
    let network_scope = geo_domain::AccountNetworkScope {
        operator_id: DEVELOPMENT_OPERATOR_ID,
        account_id: Uuid::parse_str(id).unwrap(),
        tenant_id: None,
        project_id: None,
    };
    assert!(
        state
            .channel_job_repository()
            .get_account_network(&network_scope)
            .await
            .unwrap()
            .is_none()
    );
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/operator/channel-proxy-pools",
            json!({"name":"Fixture pool"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let pool = json_body(response).await;
    let member_path = format!(
        "/api/v1/operator/channel-proxy-pools/{}/members",
        pool["pool_id"].as_str().unwrap()
    );
    let response=app.clone().oneshot(call("POST",&member_path,json!({"name":"Fixture member","proxy":{"server":"http://127.0.0.1:8089","username":"fixture-user","password":"fixture-old"}}))).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let member = json_body(response).await;
    let input = json!({"expected_version":0,"source":{"kind":"pool_revision","pool_id":pool["pool_id"],"member_id":member["member_id"],"revision_id":member["revision_id"]}});
    let response = app
        .clone()
        .oneshot(call("PUT", &network_path, input.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let head = json_body(response).await;
    assert_eq!(head["version"], 1);
    assert_eq!(head["active"]["pool_name"], "Fixture pool");
    assert_eq!(head["active"]["member_name"], "Fixture member");
    assert!(!head.to_string().contains("fixture-old"));
    let stale = app
        .clone()
        .oneshot(call("PUT", &network_path, input))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let response=app.clone().oneshot(call("PATCH",&format!("{member_path}/{}",member["member_id"].as_str().unwrap()),json!({"expected_version":1,"proxy":{"server":"http://127.0.0.1:8090","password":"fixture-new"}}))).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/operator/channel-login-sessions",
            json!({"account_id":id}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let login = json_body(response).await;
    assert_eq!(
        capture.starts.lock().await[0]["proxy"]["password"],
        "fixture-old"
    );
    assert_eq!(
        capture.starts.lock().await[0]["proxy"]["server"],
        "http://127.0.0.1:8089"
    );
    let response = app
        .clone()
        .oneshot(call(
            "PUT",
            &network_path,
            json!({"expected_version":1,"source":{"kind":"direct"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let waiting = json_body(response).await;
    assert_eq!(waiting["active"]["source"]["kind"], "pool_revision");
    assert_eq!(waiting["pending"]["source"]["kind"], "direct");
    assert_eq!(waiting["busy"], true);
    let response = app
        .clone()
        .oneshot(call(
            "PATCH",
            &format!("/api/v1/operator/channel-accounts/{id}"),
            json!({"group_id":null}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    *capture.fail_close.lock().await = true;
    let cancel_path = format!(
        "/api/v1/operator/channel-login-sessions/{}",
        login["session_id"].as_str().unwrap()
    );
    let response = app
        .clone()
        .oneshot(call("DELETE", &cancel_path, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = app
        .clone()
        .oneshot(call("GET", &network_path, Value::Null))
        .await
        .unwrap();
    assert_eq!(
        json_body(response).await["pending"]["source"]["kind"],
        "direct"
    );
    *capture.fail_close.lock().await = false;
    let response = app
        .clone()
        .oneshot(call("DELETE", &cancel_path, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app
        .clone()
        .oneshot(call("GET", &network_path, Value::Null))
        .await
        .unwrap();
    let promoted = json_body(response).await;
    assert_eq!(promoted["active"]["source"]["kind"], "direct");
    assert!(promoted["pending"].is_null());
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/operator/channel-login-sessions",
            json!({"account_id":id}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(capture.starts.lock().await[1]["proxy"].is_null());
    // Project defaults retain inheritance, while explicit direct overrides do
    // not change. The running inherited browser keeps its original route.
    let response = app
        .clone()
        .oneshot(call(
            "PATCH",
            "/api/v1/channel-settings",
            json!({"project_id":project,"proxy":{"server":"http://127.0.0.1:8091"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/channel-accounts",
            json!({"project_id":project,"platform":"zhihu"}),
        ))
        .await
        .unwrap();
    let inherited = json_body(response).await["account_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/channel-accounts",
            json!({"project_id":project,"platform":"zhihu"}),
        ))
        .await
        .unwrap();
    let explicit = json_body(response).await["account_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let inherited_path =
        format!("/api/v1/channel-accounts/{inherited}/network-binding?project_id={project}");
    let explicit_path =
        format!("/api/v1/channel-accounts/{explicit}/network-binding?project_id={project}");
    let response = app
        .clone()
        .oneshot(call(
            "PUT",
            &explicit_path,
            json!({"expected_version":0,"source":{"kind":"direct"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/channel-login-sessions",
            json!({"project_id":project,"account_id":inherited}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let inherited_login = json_body(response).await;
    assert_eq!(
        capture.starts.lock().await[2]["proxy"]["server"],
        "http://127.0.0.1:8091"
    );
    let response = app
        .clone()
        .oneshot(call(
            "PATCH",
            "/api/v1/channel-settings",
            json!({"project_id":project,"proxy":{"server":"http://127.0.0.1:8092"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(call("GET", &inherited_path, Value::Null))
        .await
        .unwrap();
    let default_pending = json_body(response).await;
    assert_eq!(
        default_pending["active"]["source"]["kind"],
        "project_default"
    );
    assert_eq!(default_pending["active"]["server"], "http://127.0.0.1:8091");
    assert_eq!(
        default_pending["pending"]["server"],
        "http://127.0.0.1:8092"
    );
    let response = app
        .clone()
        .oneshot(call("GET", &explicit_path, Value::Null))
        .await
        .unwrap();
    let direct_override = json_body(response).await;
    assert_eq!(direct_override["active"]["source"]["kind"], "direct");
    assert!(direct_override["pending"].is_null());
    let response = app
        .clone()
        .oneshot(call(
            "PATCH",
            "/api/v1/channel-settings",
            json!({"project_id":project,"default_group_id":null}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(call("GET", &inherited_path, Value::Null))
        .await
        .unwrap();
    assert_eq!(
        json_body(response).await["version"],
        default_pending["version"]
    );
    let response = app
        .clone()
        .oneshot(call(
            "DELETE",
            &format!(
                "/api/v1/channel-login-sessions/{}?project_id={project}",
                inherited_login["session_id"].as_str().unwrap()
            ),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/channel-login-sessions",
            json!({"project_id":project,"account_id":inherited}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        capture.starts.lock().await[3]["proxy"]["server"],
        "http://127.0.0.1:8092"
    );
    server.abort();
}

#[tokio::test]
async fn customer_network_binding_is_scoped_secret_write_only_and_rejects_operator_inventory() {
    let (app, project, cookie, csrf) = fixture().await;
    let call = |method: &str, path: &str, value: Value| {
        request(method, path, Some(&cookie), Some(&csrf), value.to_string())
    };
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/channel-accounts",
            json!({"project_id":project,"platform":"zhihu"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let account = json_body(response).await;
    let id = account["account_id"].as_str().unwrap();
    let path = format!("/api/v1/channel-accounts/{id}/network-binding?project_id={project}");
    let response = app
        .clone()
        .oneshot(call("GET", &path, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["version"], 0);
    let response=app.clone().oneshot(call("PUT",&path,json!({"expected_version":0,"source":{"kind":"pool_revision","pool_id":Uuid::new_v4(),"member_id":Uuid::new_v4(),"revision_id":Uuid::new_v4()}}))).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let input = json!({"expected_version":0,"source":{"kind":"standalone","proxy":{"server":"https://proxy.example.invalid:443","username":"fixture-private-user","password":"fixture-private-password"}}});
    let response = app
        .clone()
        .oneshot(call("PUT", &path, input.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let saved = json_body(response).await;
    assert_eq!(saved["active"]["source"]["kind"], "standalone");
    assert!(!saved.to_string().contains("fixture-private"));
    let response = app
        .clone()
        .oneshot(call("PUT", &path, input))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = app
        .clone()
        .oneshot(call(
            "PATCH",
            &format!("/api/v1/channel-accounts/{id}?project_id={project}"),
            json!({"group_id":null}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(call("GET", &path, Value::Null))
        .await
        .unwrap();
    assert_eq!(json_body(response).await, saved);
    let response = app
        .clone()
        .oneshot(call(
            "PUT",
            &path,
            json!({"expected_version":1,"source":{"kind":"direct"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let direct = json_body(response).await;
    assert_eq!(direct["active"]["source"]["kind"], "direct");
    assert!(direct["active"]["server"].is_null());
    let response = app
        .clone()
        .oneshot(call(
            "POST",
            "/api/v1/channel-groups",
            json!({"project_id":project,"name":"Fixture group"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let group = json_body(response).await;
    let account_path = format!("/api/v1/channel-accounts/{id}?project_id={project}");
    let response = app
        .clone()
        .oneshot(call(
            "PATCH",
            &account_path,
            json!({"group_id":group["group_id"],"proxy":{"server":"http://127.0.0.1:8087"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["group_id"], group["group_id"]);
    let response = app
        .clone()
        .oneshot(call("GET", &path, Value::Null))
        .await
        .unwrap();
    let combined = json_body(response).await;
    assert_eq!(combined["active"]["server"], "http://127.0.0.1:8087");
    let response = app
        .clone()
        .oneshot(call(
            "PATCH",
            &account_path,
            json!({"group_id":Uuid::new_v4(),"proxy":{"server":"http://127.0.0.1:8088"}}),
        ))
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let response = app
        .clone()
        .oneshot(call("GET", &path, Value::Null))
        .await
        .unwrap();
    assert_eq!(json_body(response).await, combined);
}

#[tokio::test]
async fn both_login_paths_keep_saved_storage_and_retry_handle_until_exact_disposal() {
    use axum::{
        Json,
        extract::{Path, State},
        routing::{get, post},
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[derive(Clone)]
    struct Lifecycle {
        fail_close: Arc<AtomicBool>,
        starts: Arc<AtomicUsize>,
    }
    for pool in [false, true] {
        let lifecycle = Lifecycle {
            fail_close: Arc::new(AtomicBool::new(true)),
            starts: Arc::new(AtomicUsize::new(0)),
        };
        let runner = Router::new()
            .route("/v1/lifecycle", get(|| async { Json(json!({"generation":Uuid::from_u128(123)})) }))
            .route("/v1/sessions", post(|State(s): State<Lifecycle>, Json(input): Json<Value>| async move {
                s.starts.fetch_add(1, Ordering::SeqCst);
                Json(json!({"session_id":input["session_id"],"generation":input["expected_generation"]}))
            }))
            .route("/v1/sessions/{id}/complete", post(|| async {
                Json(json!({"identity":{"platform_account_id":"fixture-identity","display_name":"Fixture","avatar_url":null},
                    "storage_state":{"cookies":[{"name":"fixture-cookie","value":"private-fixture"}]}}))
            }))
            .route("/v1/sessions/{id}/dispose", post(|State(s): State<Lifecycle>, Path(id):Path<Uuid>, Json(input):Json<Value>| async move {
                if s.fail_close.load(Ordering::SeqCst) {
                    // A plain absence response must not release the account.
                    (StatusCode::NOT_FOUND, Json(json!({"error":"missing"})))
                } else {
                    (StatusCode::OK, Json(json!({"session_id":id,"generation":input["expected_generation"],"state":"disposed"})))
                }
            }))
            .with_state(lifecycle.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, runner).await.unwrap() });
        let browser =
            BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap();
        let (app, project, cookie, csrf) =
            pool_fixture_with_browser(Role::ResourceAdmin, true, Some(browser)).await;
        let prefix = if pool { "/operator" } else { "" };
        let account_path = format!("/api/v1{prefix}/channel-accounts");
        let input = if pool {
            json!({"platform":"zhihu"})
        } else {
            json!({"platform":"zhihu","project_id":project})
        };
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &account_path,
                Some(&cookie),
                Some(&csrf),
                input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let account = json_body(response).await;
        let login_path = format!("/api/v1{prefix}/channel-login-sessions");
        let input = if pool {
            json!({"account_id":account["account_id"]})
        } else {
            json!({"account_id":account["account_id"],"project_id":project})
        };
        let start = || {
            request(
                "POST",
                &login_path,
                Some(&cookie),
                Some(&csrf),
                input.to_string(),
            )
        };
        let response = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let started = json_body(response).await;
        let response = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await["session_id"],
            started["session_id"]
        );
        assert_eq!(lifecycle.starts.load(Ordering::SeqCst), 1);
        let suffix = if pool {
            String::new()
        } else {
            format!("?project_id={project}")
        };
        let session = started["session_id"].as_str().unwrap();
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("{login_path}/{session}/complete{suffix}"),
                Some(&cookie),
                Some(&csrf),
                String::new(),
            ))
            .await
            .unwrap();
        assert!(!response.status().is_success());
        let list = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("{account_path}{suffix}"),
                Some(&cookie),
                None,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let body = json_body(list).await;
        assert!(body.to_string().contains("\"ready\""), "{body}");
        assert!(!body.to_string().contains("private-fixture"));
        let response = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await["session_id"],
            started["session_id"]
        );
        assert_eq!(lifecycle.starts.load(Ordering::SeqCst), 1);
        lifecycle.fail_close.store(false, Ordering::SeqCst);
        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("{login_path}/{session}{suffix}"),
                Some(&cookie),
                Some(&csrf),
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_ne!(
            json_body(response).await["session_id"],
            started["session_id"]
        );
        assert_eq!(lifecycle.starts.load(Ordering::SeqCst), 2);
        task.abort();
    }
}

#[tokio::test]
async fn canceled_completion_cannot_overwrite_newer_login_storage_on_either_route() {
    use axum::{
        Json,
        extract::{Path, State},
        routing::{get, post},
    };
    use tokio::sync::{Mutex, Notify};
    #[derive(Clone, Default)]
    struct Race {
        first: Arc<Mutex<Option<Uuid>>>,
        entered: Arc<Notify>,
        resume: Arc<Notify>,
    }
    for pool in [false, true] {
        let race = Race::default();
        let runner = Router::new()
            .route("/v1/lifecycle", get(|| async { Json(json!({"generation":Uuid::from_u128(123)})) }))
            .route("/v1/sessions", post(|State(s):State<Race>, Json(input):Json<Value>| async move {
                let id = serde_json::from_value::<Uuid>(input["session_id"].clone()).unwrap();
                s.first.lock().await.get_or_insert(id);
                Json(json!({"session_id":id,"generation":input["expected_generation"]}))
            }))
            .route("/v1/sessions/{id}/complete", post(|State(s):State<Race>, Path(id):Path<Uuid>| async move {
                let is_first = *s.first.lock().await == Some(id);
                if is_first {
                    s.entered.notify_one();
                    s.resume.notified().await;
                }
                Json(json!({"identity":{"platform_account_id":"fixture-identity","display_name":id.to_string(),"avatar_url":null},
                    "storage_state":{"cookies":[{"name":"fixture-cookie","value":id.to_string()}]}}))
            }))
            .route("/v1/sessions/{id}/dispose", post(|Path(id):Path<Uuid>, Json(input):Json<Value>| async move {
                Json(json!({"session_id":id,"generation":input["expected_generation"],"state":"disposed"}))
            }))
            .with_state(race.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, runner).await.unwrap() });
        let browser =
            BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap();
        let (app, project, cookie, csrf, state) =
            pool_fixture_with_browser_state(Role::ResourceAdmin, true, Some(browser)).await;
        let prefix = if pool { "/operator" } else { "" };
        let account_path = format!("/api/v1{prefix}/channel-accounts");
        let input = if pool {
            json!({"platform":"zhihu"})
        } else {
            json!({"platform":"zhihu","project_id":project})
        };
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &account_path,
                Some(&cookie),
                Some(&csrf),
                input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let account = json_body(response).await;
        let account_id = serde_json::from_value::<Uuid>(account["account_id"].clone()).unwrap();
        let login_path = format!("/api/v1{prefix}/channel-login-sessions");
        let input = if pool {
            json!({"account_id":account_id})
        } else {
            json!({"account_id":account_id,"project_id":project})
        };
        let start = || {
            request(
                "POST",
                &login_path,
                Some(&cookie),
                Some(&csrf),
                input.to_string(),
            )
        };
        let response = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let first = json_body(response).await["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let suffix = if pool {
            String::new()
        } else {
            format!("?project_id={project}")
        };
        let complete = request(
            "POST",
            &format!("{login_path}/{first}/complete{suffix}"),
            Some(&cookie),
            Some(&csrf),
            String::new(),
        );
        let completing_app = app.clone();
        let completing =
            tokio::spawn(async move { completing_app.oneshot(complete).await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(5), race.entered.notified())
            .await
            .unwrap();
        let response = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("{login_path}/{first}{suffix}"),
                Some(&cookie),
                Some(&csrf),
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let second = json_body(response).await["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(first, second);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("{login_path}/{second}/complete{suffix}"),
                Some(&cookie),
                Some(&csrf),
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let customer_scope = TenantScope::new(
            DEVELOPMENT_OPERATOR_ID,
            DEVELOPMENT_TENANT_ID,
            Some(Uuid::parse_str(&project).unwrap().into()),
        );
        let stored = if pool {
            state
                .channel_service()
                .repository
                .get_pool_account(DEVELOPMENT_OPERATOR_ID, account_id)
                .await
                .unwrap()
                .session
                .unwrap()
        } else {
            state
                .channel_service()
                .repository
                .get_account(&customer_scope, account_id)
                .await
                .unwrap()
                .session
                .unwrap()
        };
        race.resume.notify_one();
        let stale = completing.await.unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let (after, display) = if pool {
            let record = state
                .channel_service()
                .repository
                .get_pool_account(DEVELOPMENT_OPERATOR_ID, account_id)
                .await
                .unwrap();
            (record.session.unwrap(), record.account.display_name)
        } else {
            let record = state
                .channel_service()
                .repository
                .get_account(&customer_scope, account_id)
                .await
                .unwrap();
            (record.session.unwrap(), record.account.display_name)
        };
        assert_eq!(after.encrypted_bytes(), stored.encrypted_bytes());
        assert_eq!(display.as_deref(), Some(second.as_str()));
        server.abort();
    }
}

async fn pool_fixture(
    role: Role,
    membership_matches_configured_pool: bool,
) -> (Router, String, String, String) {
    pool_fixture_with_browser(role, membership_matches_configured_pool, None).await
}

async fn pool_fixture_with_browser(
    role: Role,
    membership_matches_configured_pool: bool,
    browser: Option<BrowserBridge>,
) -> (Router, String, String, String) {
    let (app, project, cookie, csrf, _) =
        pool_fixture_with_browser_state(role, membership_matches_configured_pool, browser).await;
    (app, project, cookie, csrf)
}

async fn pool_fixture_with_browser_state(
    role: Role,
    membership_matches_configured_pool: bool,
    browser: Option<BrowserBridge>,
) -> (Router, String, String, String, AppState) {
    let auth = Arc::new(MemoryAuthRepository::development_with_password(
        "channel-test",
    ));
    let pool_tenant = Uuid::new_v4();
    auth.insert_membership(Membership::new(
        UserId::new(Uuid::from_u128(0x00000000000040008000000000000004)),
        DEVELOPMENT_OPERATOR_ID,
        pool_tenant.into(),
        role,
    ))
    .await
    .unwrap();
    let configured_pool = if membership_matches_configured_pool {
        pool_tenant
    } else {
        Uuid::new_v4()
    };
    let service =
        ChannelService::development().with_operator_pool_tenant_id(configured_pool.into());
    let service = if let Some(browser) = browser {
        service.with_browser(browser)
    } else {
        service
    };
    let state = AppState::with_stores_and_auth_and_projects(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth,
        Arc::new(MemoryProjectRepository::default()),
        EventBus::default(),
        false,
    )
    .with_channel_service(service);
    let customer = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &customer,
            ProjectCreate {
                slug: Some("pool-test".into()),
                display_name: "Pool test".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let app = router(state.clone());
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            None,
            json!({"login_name":"demo@localhost","password":"channel-test"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body = json_body(login).await;
    (
        app,
        project.id.to_string(),
        cookie,
        body["csrf_token"].as_str().unwrap().to_owned(),
        state,
    )
}

fn request(
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: String,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("content-type", "application/json");
    if !path.starts_with("/api/v1/operator/") && path != "/api/v1/channel-platforms" {
        builder = builder.header("x-tenant-selector", DEVELOPMENT_TENANT_ID.to_string());
    }
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    builder.body(Body::from(body)).unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn proxy_inventory_http_versions_redaction_pagination_and_validation() {
    let (app, _, cookie, csrf) = pool_fixture(Role::ResourceAdmin, true).await;
    let base = "/api/v1/operator/channel-proxy-pools";
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            base,
            Some(&cookie),
            Some(&csrf),
            json!({"name":"Synthetic pool"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let pool = json_body(response).await;
    assert_eq!(pool["version"], 1);
    assert_eq!(pool["enabled"], true);
    let members = format!("{base}/{}/members", pool["pool_id"].as_str().unwrap());
    let mut req = request("POST",&members,Some(&cookie),Some(&csrf),json!({"name":"Synthetic member","proxy":{"server":"http://fixture.invalid:3128","username":"synthetic-user","password":"synthetic-secret"}}).to_string());
    req.headers_mut()
        .insert("idempotency-key", "synthetic-key".parse().unwrap());
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let member = json_body(response).await;
    assert!(!member.to_string().contains("synthetic-secret"));
    assert!(!member.to_string().contains("synthetic-user"));
    assert_eq!(member["proxy_server"], "http://fixture.invalid:3128");
    let path = format!("{members}/{}", member["member_id"].as_str().unwrap());
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &path,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_version":1,"enabled":false}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let changed = json_body(response).await;
    assert_eq!(changed["version"], 2);
    assert_eq!(changed["revision_id"], member["revision_id"]);
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &path,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_version":1,"enabled":true}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = app.clone().oneshot(request("PATCH",&path,Some(&cookie),Some(&csrf),json!({"expected_version":2,"proxy":{"server":"socks5://fixture.invalid:1080","password":"new-synthetic-secret"}}).to_string())).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rotated = json_body(response).await;
    assert_eq!(rotated["version"], 3);
    assert_ne!(rotated["revision_id"], member["revision_id"]);
    for server in [
        "http://user:secret@fixture.invalid:3128",
        "http://fixture.invalid:3128/private",
        "http://fixture.invalid:3128?secret=x",
        "http://fixture.invalid:3128#secret",
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &members,
                Some(&cookie),
                Some(&csrf),
                json!({"name":"Synthetic member","proxy":{"server":server}}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!json_body(response).await.to_string().contains(server));
    }
    for query in ["?limit=0", "?limit=101", "?cursor=invalid", "?unknown=1"] {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("{base}{query}"),
                Some(&cookie),
                None,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = app
        .clone()
        .oneshot(request("GET", &members, Some(&cookie), None, String::new()))
        .await
        .unwrap();
    let list = json_body(response).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["version"], 3);
    assert_eq!(list["next_cursor"], Value::Null);
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &path,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_version":3,"password":"synthetic-secret"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn proxy_inventory_requires_trusted_resource_scope_and_csrf() {
    let base = "/api/v1/operator/channel-proxy-pools";
    for (role, matching) in [(Role::CustomerAdmin, true), (Role::ResourceAdmin, false)] {
        let (app, _, cookie, csrf) = pool_fixture(role, matching).await;
        for method in ["GET", "POST"] {
            let response = app
                .clone()
                .oneshot(request(
                    method,
                    base,
                    Some(&cookie),
                    Some(&csrf),
                    json!({"name":"Synthetic pool"}).to_string(),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    let (app, _, cookie, _) = pool_fixture(Role::ResourceAdmin, true).await;
    let response = app
        .oneshot(request(
            "POST",
            base,
            Some(&cookie),
            None,
            json!({"name":"Synthetic pool"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn operator_import_http_is_scoped_durable_secretless_and_not_cached() {
    let (app, _, cookie, csrf) = pool_fixture(Role::ResourceAdmin, true).await;
    let path = "/api/v1/operator/channel-account-imports";
    let input = json!({"import_key":"synthetic-batch","rows":[
        {"row_key":"row-1","platform":"zhihu","username":"synthetic-user","password":"synthetic-password"},
        {"row_key":"row-2","platform":"unknown","username":"synthetic-user-2","password":"synthetic-password-2"}
    ]});
    // No Idempotency-Key header: the repository owns the boundary and never
    // stores an unkeyed password-bearing body hash in the HTTP cache.
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            path,
            Some(&cookie),
            Some(&csrf),
            input.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let outcome = json_body(response).await;
    assert_eq!(outcome["rows"][0]["status"], "needs_login");
    assert_eq!(outcome["rows"][1]["error_code"], "unsupported_platform");
    assert!(!outcome.to_string().contains("synthetic-password"));
    assert!(!outcome.to_string().contains("synthetic-user"));
    let reload = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("{path}/synthetic-batch"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(json_body(reload).await, outcome);
    let retry = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("{path}/synthetic-batch/rows/row-2/retry"),
            Some(&cookie),
            Some(&csrf),
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(json_body(retry).await, outcome);
    let mut changed = input.clone();
    changed["rows"][0]["password"] = json!("changed-synthetic-password");
    let conflict = app
        .clone()
        .oneshot(request(
            "POST",
            path,
            Some(&cookie),
            Some(&csrf),
            changed.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let malformed = app.clone().oneshot(request("POST", path, Some(&cookie), Some(&csrf),
        r#"{"import_key":"bad","rows":[{"row_key":"row","platform":"zhihu","username":"x","password":"synthetic-password","group_id":"synthetic-secret"}]}"#.into())).await.unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert!(
        !json_body(malformed)
            .await
            .to_string()
            .contains("synthetic-secret")
    );
    let missing_csrf = app
        .clone()
        .oneshot(request(
            "POST",
            path,
            Some(&cookie),
            None,
            input.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);
    for (role, matching) in [(Role::CustomerAdmin, true), (Role::ResourceAdmin, false)] {
        let (denied, _, cookie, csrf) = pool_fixture(role, matching).await;
        for (method, uri, body) in [
            ("POST", path.to_owned(), input.to_string()),
            ("GET", format!("{path}/synthetic-batch"), String::new()),
            (
                "POST",
                format!("{path}/synthetic-batch/rows/row-1/retry"),
                String::new(),
            ),
        ] {
            let response = denied
                .clone()
                .oneshot(request(method, &uri, Some(&cookie), Some(&csrf), body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
}

#[tokio::test]
async fn desktop_authorization_is_same_origin_and_cookie_session_bound() {
    use axum::{
        Json,
        extract::Path,
        routing::{get, post},
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    let ready = Arc::new(AtomicBool::new(true));
    let transport_ready = ready.clone();
    let runner = Router::new()
        .route("/v1/lifecycle", get(|| async {
            Json(json!({"generation":Uuid::from_u128(123)}))
        }))
        .route(
            "/v1/sessions",
            post(|Json(input): Json<Value>| async move {
                Json(json!({"session_id": input["session_id"],"generation":input["expected_generation"]}))
            }),
        )
        .route(
            "/v1/sessions/{id}/desktop-readiness",
            get(move |headers: axum::http::HeaderMap| {
                let ready = transport_ready.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer runner-fixture-token");
                    if ready.load(Ordering::SeqCst) {
                        (StatusCode::OK, Json(json!({"ready": true})))
                    } else {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error":"unavailable"})),
                        )
                    }
                }
            }),
        )
        .route(
            "/v1/sessions/{id}",
            axum::routing::delete(|Path(_id): Path<String>| async {
                Json(json!({"closed": true}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, runner).await.unwrap();
    });
    let state = AppState::development_with_password("desktop-fixture").with_channel_service(
        ChannelService::development().with_browser(
            BrowserBridge::new(format!("http://{address}"), "runner-fixture-token".into()).unwrap(),
        ),
    );
    let scope = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
    let project = state
        .project_repository()
        .create(
            &scope,
            ProjectCreate {
                slug: Some("desktop-fixture".into()),
                display_name: "Desktop fixture".into(),
                settings: ProjectSettings::default(),
            },
        )
        .await
        .unwrap();
    let app = router(state);
    let login = |app: Router| async move {
        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/auth/login",
                None,
                None,
                json!({"login_name":"demo@localhost","password":"desktop-fixture"}).to_string(),
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
        let body = json_body(response).await;
        (cookie, body["csrf_token"].as_str().unwrap().to_owned())
    };
    let (cookie, csrf) = login(app.clone()).await;
    let (other_cookie, other_csrf) = login(app.clone()).await;
    let account = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-accounts",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project.id,"platform":"zhihu"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::CREATED);
    let account = json_body(account).await;
    let started = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-login-sessions",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project.id,"account_id":account["account_id"]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::CREATED);
    let started = json_body(started).await;
    let url = format!(
        "/api/v1/channel-login-sessions/{}/desktop-authorization?project_id={}",
        started["session_id"].as_str().unwrap(),
        project.id
    );
    let authorize = |cookie: &str, csrf: &str, origin: &str, tenant: &str| {
        Request::builder()
            .method("POST")
            .uri(&url)
            .header("host", "localhost:8080")
            .header("origin", origin)
            .header("x-tenant-selector", tenant)
            .header("cookie", cookie)
            .header(CSRF_HEADER, csrf)
            .body(Body::empty())
            .unwrap()
    };
    let cross_origin = app
        .clone()
        .oneshot(authorize(
            &cookie,
            &csrf,
            "http://localhost:5173",
            &DEVELOPMENT_TENANT_ID.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);
    let other = app
        .clone()
        .oneshot(authorize(
            &other_cookie,
            &other_csrf,
            "http://localhost:8080",
            &DEVELOPMENT_TENANT_ID.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::FORBIDDEN);
    let foreign_tenant = app
        .clone()
        .oneshot(authorize(
            &cookie,
            &csrf,
            "http://localhost:8080",
            &Uuid::new_v4().to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(foreign_tenant.status(), StatusCode::FORBIDDEN);
    let authorized = app
        .clone()
        .oneshot(authorize(
            &cookie,
            &csrf,
            "http://localhost:8080",
            &DEVELOPMENT_TENANT_ID.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let body = json_body(authorized).await;
    assert!(
        body["protocol"]
            .as_str()
            .unwrap()
            .starts_with("geo-desktop.")
    );
    assert_eq!(
        body["websocket_path"],
        format!(
            "/api/v1/channel-login-sessions/{}/desktop?project_id={}&tenant_id={}",
            started["session_id"].as_str().unwrap(),
            project.id,
            DEVELOPMENT_TENANT_ID
        )
    );
    ready.store(false, Ordering::SeqCst);
    let unavailable = app
        .oneshot(authorize(
            &cookie,
            &csrf,
            "http://localhost:8080",
            &DEVELOPMENT_TENANT_ID.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    task.abort();
}

#[tokio::test]
async fn pool_desktop_authorization_checks_transport_not_identity() {
    use axum::{
        Json,
        routing::{get, post},
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    let ready = Arc::new(AtomicBool::new(true));
    let transport_ready = ready.clone();
    let runner = Router::new()
        .route("/v1/lifecycle", get(|| async {
            Json(json!({"generation":Uuid::from_u128(123)}))
        }))
        .route(
            "/v1/sessions",
            post(|Json(input): Json<Value>| async move {
                Json(json!({"session_id": input["session_id"],"generation":input["expected_generation"]}))
            }),
        )
        // No status route: authorization must never perform identity discovery.
        .route(
            "/v1/sessions/{id}/desktop-readiness",
            get(move |headers: axum::http::HeaderMap| {
                let ready = transport_ready.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer runner-fixture-token");
                    if ready.load(Ordering::SeqCst) {
                        (StatusCode::OK, Json(json!({"ready": true})))
                    } else {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": "unavailable"})),
                        )
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, runner).await.unwrap() });
    let (app, _, cookie, csrf) = pool_fixture_with_browser(
        Role::ResourceAdmin,
        true,
        Some(
            BrowserBridge::new(format!("http://{address}"), "runner-fixture-token".into()).unwrap(),
        ),
    )
    .await;
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/operator/channel-accounts",
            Some(&cookie),
            Some(&csrf),
            json!({"platform": "zhihu"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let account = json_body(response).await;
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/operator/channel-login-sessions",
            Some(&cookie),
            Some(&csrf),
            json!({"account_id": account["account_id"]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let started = json_body(response).await;
    let path = format!(
        "/api/v1/operator/channel-login-sessions/{}/desktop-authorization",
        started["session_id"].as_str().unwrap()
    );
    let authorize = |origin: &str| {
        let mut req = request("POST", &path, Some(&cookie), Some(&csrf), String::new());
        req.headers_mut().insert("origin", origin.parse().unwrap());
        req
    };
    let response = app
        .clone()
        .oneshot(authorize("http://localhost:5173"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = app
        .clone()
        .oneshot(authorize("http://localhost:8080"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let receipt = json_body(response).await;
    assert!(
        receipt["protocol"]
            .as_str()
            .unwrap()
            .starts_with("geo-desktop.")
    );
    assert_eq!(
        receipt["websocket_path"],
        format!(
            "/api/v1/operator/channel-login-sessions/{}/desktop",
            started["session_id"].as_str().unwrap()
        )
    );
    ready.store(false, Ordering::SeqCst);
    let response = app
        .oneshot(authorize("http://localhost:8080"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    task.abort();
}

#[tokio::test]
async fn legacy_snapshot_and_action_routes_are_removed() {
    let (app, _project, cookie, csrf) = fixture().await;
    let session_id = Uuid::new_v4();

    for prefix in ["", "/operator"] {
        for (method, suffix) in [
            ("GET", "snapshot"),
            ("POST", "snapshot"),
            ("GET", "actions"),
            ("POST", "actions"),
        ] {
            let path = format!("/api/v1{prefix}/channel-login-sessions/{session_id}/{suffix}");
            let response = app
                .clone()
                .oneshot(request(
                    method,
                    &path,
                    Some(&cookie),
                    Some(&csrf),
                    if method == "POST" {
                        "{}".to_owned()
                    } else {
                        String::new()
                    },
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "legacy {method} {path} route still registered"
            );
        }
    }
}

#[tokio::test]
async fn connector_settings_are_resource_admin_only_and_fail_closed() {
    let (app, project, cookie, csrf) = pool_fixture(Role::ResourceAdmin, true).await;
    let operator_url = "/api/v1/operator/connector-capabilities";
    let list = app
        .clone()
        .oneshot(request(
            "GET",
            operator_url,
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list = json_body(list).await;
    assert!(list["items"].as_array().unwrap().iter().any(|entry| {
        entry["platform_id"] == "zhihu" && entry["availability"] == "unavailable"
    }));

    let patch = format!("{operator_url}/zhihu/primary");
    let enabled = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":true,"content_types":["article"]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::BAD_REQUEST);
    let unknown = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":false,"content_types":[],"verification":true})
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let disabled = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":false,"content_types":[]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(json_body(disabled).await["revision"], 1);
    let stale = app
        .clone()
        .oneshot(request(
            "PATCH",
            &patch,
            Some(&cookie),
            Some(&csrf),
            json!({"expected_revision":0,"enabled":false,"content_types":[]}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let project_list = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/projects/{project}/connector-capabilities"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(project_list.status(), StatusCode::OK);
    let project_list = json_body(project_list).await;
    let row = project_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["platform_id"] == "zhihu")
        .unwrap();
    assert!(row.get("verified_content_types").is_none());
    assert!(row.get("deployed_version").is_none());
    assert_eq!(row["content_types"], json!([]));

    for (role, matches_pool) in [(Role::CustomerAdmin, true), (Role::ResourceAdmin, false)] {
        let (app, _, cookie, csrf) = pool_fixture(role, matches_pool).await;
        let list = app
            .clone()
            .oneshot(request(
                "GET",
                operator_url,
                Some(&cookie),
                None,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::FORBIDDEN);
        let denied = app
            .clone()
            .oneshot(request(
                "PATCH",
                &patch,
                Some(&cookie),
                Some(&csrf),
                json!({"expected_revision":0,"enabled":false,"content_types":[]}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn account_group_settings_scope_and_no_client_claimed_identity() {
    let (app, project, cookie, csrf) = fixture().await;
    let group = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-groups",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"name":"Editorial"}).to_string(),
        ))
        .await
        .unwrap();
    let group_status = group.status();
    let group = json_body(group).await;
    assert_eq!(group_status, StatusCode::CREATED, "group response: {group}");
    let group_id = group["group_id"].as_str().unwrap();
    let settings = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/api/v1/channel-settings",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"default_group_id":group_id}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(settings.status(), StatusCode::OK);
    let account = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-accounts",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"platform":"zhihu"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::CREATED);
    let account = json_body(account).await;
    assert_eq!(account["group_id"], group_id);
    assert_eq!(account["status"], "needs_login");
    assert!(account["platform_account_id"].is_null());
    let id = account["account_id"].as_str().unwrap();
    let forbidden_claim = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/channel-accounts/{id}?project_id={project}"),
            Some(&cookie),
            Some(&csrf),
            json!({"status":"ready","platform_account_id":"invented"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(forbidden_claim.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let no_browser = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/channel-login-sessions",
            Some(&cookie),
            Some(&csrf),
            json!({"project_id":project,"account_id":id}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(no_browser.status(), StatusCode::SERVICE_UNAVAILABLE);
    let foreign = uuid::Uuid::new_v4();
    let list = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={foreign}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pool_admin_assignment_and_customer_mutation_boundary() {
    let (app, project, cookie, csrf) = pool_fixture(Role::ResourceAdmin, true).await;
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/operator/channel-accounts",
            Some(&cookie),
            Some(&csrf),
            json!({"platform":"zhihu"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let id = created["account_id"].as_str().unwrap();
    let before = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={project}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(
        json_body(before).await["items"].as_array().unwrap().len(),
        0
    );
    let assigned = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/operator/channel-accounts/{id}/assignments"),
            Some(&cookie),
            Some(&csrf),
            json!({"tenant_id":DEVELOPMENT_TENANT_ID,"project_id":project}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(assigned.status(), StatusCode::NO_CONTENT);
    let visible = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={project}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    let visible = json_body(visible).await;
    assert_eq!(visible["items"][0]["owner_kind"], "operator_pool");
    assert!(visible["items"][0]["proxy_server"].is_null());
    let customer_patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/channel-accounts/{id}?project_id={project}"),
            Some(&cookie),
            Some(&csrf),
            json!({"enabled":false}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(customer_patch.status(), StatusCode::NOT_FOUND);
    let unassigned = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/operator/channel-accounts/{id}/assignments"),
            Some(&cookie),
            Some(&csrf),
            json!({"tenant_id":DEVELOPMENT_TENANT_ID,"project_id":project}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(unassigned.status(), StatusCode::NO_CONTENT);
    let after = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/channel-accounts?project_id={project}"),
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert!(
        json_body(after).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn only_pool_tenant_admin_roles_can_manage_pool() {
    for (role, membership_matches_configured_pool, expected) in [
        (Role::Operator, true, StatusCode::FORBIDDEN),
        (Role::OemAdmin, true, StatusCode::CREATED),
        (Role::ResourceAdmin, false, StatusCode::FORBIDDEN),
    ] {
        let (app, _, cookie, csrf) = pool_fixture(role, membership_matches_configured_pool).await;
        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/operator/channel-accounts",
                Some(&cookie),
                Some(&csrf),
                json!({"platform":"zhihu"}).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "role {role:?}");
    }
}

#[tokio::test]
async fn operator_project_directory_guards_scope_and_validates_queries() {
    for (role, matching_pool, allowed) in [
        (Role::Operator, true, false),
        (Role::ResourceAdmin, false, false),
        (Role::ResourceAdmin, true, true),
        (Role::OemAdmin, true, true),
    ] {
        let (app, project, cookie, _) = pool_fixture(role, matching_pool).await;
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/operator/channel-projects",
                Some(&cookie),
                None,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if allowed {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        if !allowed {
            continue;
        }
        let body = json_body(response).await;
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["project_id"], project);
        assert_eq!(body["items"][0]["display_name"], "Pool test");
        assert_eq!(body["items"][0].as_object().unwrap().len(), 4);
        assert!(body["next_cursor"].is_null());
        for query in [
            "limit=0",
            "limit=101",
            "cursor=invalid",
            "cursor=",
            "tenant_id=ignored",
            "operator_id=ignored",
            "project_id=ignored",
            &format!("search={}", "a".repeat(201)),
        ] {
            let response = app
                .clone()
                .oneshot(request(
                    "GET",
                    &format!("/api/v1/operator/channel-projects?{query}"),
                    Some(&cookie),
                    None,
                    String::new(),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        }
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/operator/channel-projects?search=missing",
                Some(&cookie),
                None,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(response).await["items"], json!([]));
        let response = app
            .oneshot(request(
                "GET",
                "/api/v1/operator/channel-projects",
                None,
                None,
                String::new(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn platform_catalogue_requires_session_but_no_tenant_selector() {
    let (app, _, cookie, _) = fixture().await;
    let anonymous = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/channel-platforms",
            None,
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let authenticated = app
        .oneshot(request(
            "GET",
            "/api/v1/channel-platforms",
            Some(&cookie),
            None,
            String::new(),
        ))
        .await
        .unwrap();
    assert_eq!(authenticated.status(), StatusCode::OK);
    let body = json_body(authenticated).await;
    let items = body["items"].as_array().unwrap();
    let ids: Vec<_> = items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "zhihu",
            "baidu_creator",
            "xiaohongshu",
            "kimi",
            "doubao",
            "deepseek",
            "glm",
            "qianwen",
            "yuanbao"
        ]
    );
    for item in items {
        assert!(item["login_entry_available"].is_boolean());
        assert!(item["login_supported"].is_boolean());
        assert!(item["measurement_supported"].is_boolean());
    }
}
