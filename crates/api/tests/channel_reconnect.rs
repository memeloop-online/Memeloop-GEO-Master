use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{Request, StatusCode, header::SET_COOKIE},
    routing::{get, post},
};
use geo_api::{AppState, BrowserBridge, CSRF_HEADER, ChannelService, router};
use geo_domain::{
    AccountNetworkScope, ChannelAccount, ChannelAccountRecord, ChannelOwnerKind, ChannelSecret,
    ChannelStatus, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, MemoryChannelRepository,
    PoolAccount, PoolAccountRecord, ProjectCreate, ProjectSettings, TenantScope,
};
use geo_provider::SecretEnvelope;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const IDENTITY: &str = "synthetic-reconnect-identity";

#[derive(Clone, Default)]
struct Runner {
    completed: Arc<Mutex<Vec<Uuid>>>,
    disposed: Arc<Mutex<Vec<Uuid>>>,
    fail_next_disposal: Arc<AtomicBool>,
}

struct Fixture {
    app: Router,
    state: AppState,
    scope: TenantScope,
    cookie: String,
    csrf: String,
    runner: Runner,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let runner = Runner::default();
        let routes = Router::new()
            .route(
                "/v1/lifecycle",
                get(|| async { Json(json!({"generation":Uuid::from_u128(123)})) }),
            )
            .route(
                "/v1/sessions",
                post(|Json(input): Json<Value>| async move {
                    Json(json!({
                        "session_id":input["session_id"],
                        "generation":input["expected_generation"]
                    }))
                }),
            )
            .route(
                "/v1/sessions/{id}/complete",
                post(|State(runner): State<Runner>, Path(id): Path<Uuid>| async move {
                    runner.completed.lock().await.push(id);
                    Json(json!({
                        "identity":{
                            "platform_account_id":IDENTITY,
                            "display_name":"Synthetic account",
                            "avatar_url":null
                        },
                        "storage_state":{"cookies":[{"name":"synthetic","value":id.to_string()}]}
                    }))
                }),
            )
            .route(
                "/v1/sessions/{id}/dispose",
                post(
                    |State(runner): State<Runner>,
                     Path(id): Path<Uuid>,
                     Json(input): Json<Value>| async move {
                        if runner.fail_next_disposal.swap(false, Ordering::SeqCst) {
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                Json(json!({"error":"synthetic disposal unavailable"})),
                            );
                        }
                        runner.disposed.lock().await.push(id);
                        (
                            StatusCode::OK,
                            Json(json!({
                                "session_id":id,
                                "generation":input["expected_generation"],
                                "state":"disposed"
                            })),
                        )
                    },
                ),
            )
            .with_state(runner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let browser =
            BrowserBridge::new(format!("http://{address}"), "synthetic-token".into()).unwrap();
        let service = ChannelService::persistent(
            Arc::new(MemoryChannelRepository::default()),
            KEY,
            Some(browser),
        )
        .unwrap();
        let state =
            AppState::development_with_password("reconnect-test").with_channel_service(service);
        let mut scope = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
        let project = state
            .project_repository()
            .create(
                &scope,
                ProjectCreate {
                    slug: Some("reconnect-test".into()),
                    display_name: "Reconnect test".into(),
                    settings: ProjectSettings::default(),
                },
            )
            .await
            .unwrap();
        scope.project_id = Some(project.id);
        let app = router(state.clone());
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
                        json!({"login_name":"demo@localhost","password":"reconnect-test"})
                            .to_string(),
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
        let body = json_body(response).await;
        Self {
            app,
            state,
            scope,
            cookie,
            csrf: body["csrf_token"].as_str().unwrap().to_owned(),
            runner,
            server,
        }
    }

    async fn call(&self, method: &str, path: &str, input: Value) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "localhost:8080")
                    .header("origin", "http://localhost:5173")
                    .header("content-type", "application/json")
                    .header("x-tenant-selector", self.scope.tenant_id.to_string())
                    .header("cookie", &self.cookie)
                    .header(CSRF_HEADER, &self.csrf)
                    .body(Body::from(input.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn create_account(&self, proxy: Option<&str>) -> Uuid {
        let mut input = json!({"project_id":self.scope.project_id,"platform":"zhihu"});
        if let Some(server) = proxy {
            input["proxy"] = json!({"server":server});
        }
        let response = self.call("POST", "/api/v1/channel-accounts", input).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        uuid(&json_body(response).await["account_id"])
    }

    async fn start(&self, account: Uuid) -> Uuid {
        let response = self
            .call(
                "POST",
                "/api/v1/channel-login-sessions",
                json!({"project_id":self.scope.project_id,"account_id":account}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        uuid(&json_body(response).await["session_id"])
    }

    fn login_path(&self, session: Uuid, suffix: &str) -> String {
        format!(
            "/api/v1/channel-login-sessions/{session}{suffix}?project_id={}",
            self.scope.project_id.unwrap()
        )
    }

    async fn complete(&self, session: Uuid) -> axum::response::Response {
        self.call("POST", &self.login_path(session, "/complete"), Value::Null)
            .await
    }

    async fn connected(&self, proxy: Option<&str>) -> (Uuid, Uuid) {
        let account = self.create_account(proxy).await;
        let session = self.start(account).await;
        let response = self.complete(session).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            uuid(&json_body(response).await["account"]["account_id"]),
            account
        );
        (account, session)
    }

    async fn record(&self, account: Uuid) -> ChannelAccountRecord {
        self.state
            .channel_service()
            .repository
            .get_account(&self.scope, account)
            .await
            .unwrap()
    }

    async fn plaintext(&self, account: Uuid) -> Value {
        let record = self.record(account).await;
        let aad = format!(
            "geo-channel-v1:{}:{}:{}:{account}:session",
            self.scope.operator_id,
            self.scope.tenant_id,
            self.scope.project_id.unwrap()
        );
        let bytes = SecretEnvelope::from_hex_key(KEY)
            .unwrap()
            .open(aad.as_bytes(), record.session.unwrap().encrypted_bytes())
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn cancel(&self, session: Uuid) {
        let response = self
            .call("DELETE", &self.login_path(session, ""), Value::Null)
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

fn uuid(value: &Value) -> Uuid {
    serde_json::from_value(value.clone()).unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn repeated_verified_identity_updates_canonical_account_and_releases_placeholder() {
    for proxy in [None, Some("http://127.0.0.1:8091")] {
        let fixture = Fixture::new().await;
        let (canonical, first_login) = fixture.connected(proxy).await;
        let response = fixture
            .call(
                "POST",
                "/api/v1/channel-groups",
                json!({"project_id":fixture.scope.project_id,"name":"Synthetic retained group"}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let group = json_body(response).await["group_id"].clone();
        let response = fixture
            .call(
                "PATCH",
                &format!(
                    "/api/v1/channel-accounts/{canonical}?project_id={}",
                    fixture.scope.project_id.unwrap()
                ),
                json!({"group_id":group}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let before = fixture.record(canonical).await;
        let network_scope = AccountNetworkScope {
            operator_id: fixture.scope.operator_id,
            tenant_id: Some(fixture.scope.tenant_id),
            project_id: fixture.scope.project_id,
            account_id: canonical,
        };
        let network_before = fixture
            .state
            .channel_job_repository()
            .get_account_network(&network_scope)
            .await
            .unwrap();
        let placeholder = fixture.create_account(proxy).await;
        let login = fixture.start(placeholder).await;
        let original_operation = fixture
            .state
            .channel_job_repository()
            .get_account_browser_operation(fixture.scope.operator_id, placeholder)
            .await
            .unwrap()
            .unwrap();
        let response = fixture.complete(login).await;
        assert_eq!(response.status(), StatusCode::OK);
        let completed = json_body(response).await;
        assert_eq!(uuid(&completed["account"]["account_id"]), canonical);
        assert!(!completed.to_string().contains(&login.to_string()));
        let after = fixture.record(canonical).await;
        assert_eq!(after.account.created_at, before.account.created_at);
        assert_eq!(after.account.group_id, before.account.group_id);
        assert_eq!(after.account.project_id, before.account.project_id);
        assert_eq!(after.account.owner_kind, ChannelOwnerKind::Customer);
        assert_eq!(after.account.status, ChannelStatus::Ready);
        assert_ne!(
            after.session.as_ref().unwrap().encrypted_bytes(),
            before.session.as_ref().unwrap().encrypted_bytes()
        );
        assert_eq!(
            fixture.plaintext(canonical).await["cookies"][0]["value"],
            login.to_string()
        );
        assert_ne!(login, first_login);
        let network_after = fixture
            .state
            .channel_job_repository()
            .get_account_network(&network_scope)
            .await
            .unwrap();
        let before_network = network_before.unwrap();
        let after_network = network_after.unwrap();
        assert_eq!(after_network.version, before_network.version);
        assert_eq!(
            after_network.active.binding_id,
            before_network.active.binding_id
        );
        assert_eq!(after_network.active.scope, before_network.active.scope);
        assert_eq!(after_network.active.source, before_network.active.source);
        assert_eq!(
            after_network.active.effective_network.encrypted_bytes(),
            before_network.active.effective_network.encrypted_bytes()
        );
        assert!(after_network.pending.is_none());
        assert!(!after_network.busy);
        let retired = fixture.record(placeholder).await;
        assert_eq!(retired.account.status, ChannelStatus::Disabled);
        assert!(!retired.account.enabled);
        assert!(retired.session.is_none());
        assert!(retired.account.platform_account_id.is_none());
        assert!(fixture.runner.disposed.lock().await.contains(&login));
        let disposed_operation = fixture
            .state
            .channel_job_repository()
            .get_browser_operation(&original_operation.scope, original_operation.operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(disposed_operation.account_id, placeholder);
        assert_eq!(disposed_operation.session_id, login);
        assert_eq!(
            disposed_operation.login_verified_account_id,
            Some(canonical)
        );
        assert_eq!(
            disposed_operation.network_binding_id,
            original_operation.network_binding_id
        );
        assert_eq!(
            disposed_operation.state,
            geo_domain::BrowserOperationState::Disposed
        );
        assert!(
            fixture
                .state
                .channel_service()
                .repository
                .get_login(&fixture.scope, login)
                .await
                .is_err()
        );
        for account in [canonical, placeholder] {
            assert!(
                fixture
                    .state
                    .channel_job_repository()
                    .get_account_browser_operation(fixture.scope.operator_id, account)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let next = fixture.start(canonical).await;
        fixture.cancel(next).await;
    }
}

#[tokio::test]
async fn row_reconnect_updates_session_without_changing_account_id() {
    let fixture = Fixture::new().await;
    let (account, first) = fixture.connected(None).await;
    let next = fixture.start(account).await;
    let response = fixture.complete(next).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        uuid(&json_body(response).await["account"]["account_id"]),
        account
    );
    assert_ne!(first, next);
    assert_eq!(
        fixture.plaintext(account).await["cookies"][0]["value"],
        next.to_string()
    );
    assert_eq!(
        fixture.record(account).await.account.status,
        ChannelStatus::Ready
    );
}

#[tokio::test]
async fn new_verified_identity_keeps_the_new_account() {
    let fixture = Fixture::new().await;
    let (account, session) = fixture.connected(None).await;
    assert_eq!(
        fixture.plaintext(account).await["cookies"][0]["value"],
        session.to_string()
    );
    let accounts = fixture
        .state
        .channel_service()
        .repository
        .list_accounts(&fixture.scope)
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].account_id, account);
}

#[tokio::test]
async fn different_network_does_not_replace_canonical_session_or_release_unfinished_login() {
    let fixture = Fixture::new().await;
    let (canonical, _) = fixture.connected(Some("http://127.0.0.1:8091")).await;
    let before = fixture.record(canonical).await;
    let placeholder = fixture.create_account(None).await;
    let login = fixture.start(placeholder).await;
    let response = fixture.complete(login).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let after = fixture.record(canonical).await;
    assert_eq!(
        after.session.unwrap().encrypted_bytes(),
        before.session.unwrap().encrypted_bytes()
    );
    assert!(fixture.record(placeholder).await.session.is_none());
    assert!(!fixture.runner.disposed.lock().await.contains(&login));
    fixture.cancel(login).await;
    // The rejected handoff must not leave a reservation on the canonical account.
    let next = fixture.start(canonical).await;
    fixture.cancel(next).await;
}

#[tokio::test]
async fn busy_canonical_account_preserves_storage_and_allows_retry_after_release() {
    let fixture = Fixture::new().await;
    let (canonical, _) = fixture.connected(None).await;
    let before = fixture.record(canonical).await;
    let active = fixture.start(canonical).await;
    let placeholder = fixture.create_account(None).await;
    let login = fixture.start(placeholder).await;
    let response = fixture.complete(login).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        fixture
            .record(canonical)
            .await
            .session
            .unwrap()
            .encrypted_bytes(),
        before.session.unwrap().encrypted_bytes()
    );
    assert!(fixture.record(placeholder).await.session.is_none());
    fixture.cancel(active).await;
    let response = fixture.complete(login).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        uuid(&json_body(response).await["account"]["account_id"]),
        canonical
    );
}

#[tokio::test]
async fn saved_reconnection_retries_disposal_in_the_original_login_context() {
    let fixture = Fixture::new().await;
    let (canonical, _) = fixture.connected(None).await;
    let placeholder = fixture.create_account(None).await;
    let login = fixture.start(placeholder).await;
    fixture
        .runner
        .fail_next_disposal
        .store(true, Ordering::SeqCst);
    let response = fixture.complete(login).await;
    assert!(!response.status().is_success());
    assert_eq!(
        fixture.plaintext(canonical).await["cookies"][0]["value"],
        login.to_string()
    );
    assert_eq!(
        fixture.record(placeholder).await.account.status,
        ChannelStatus::Disabled
    );
    assert!(!fixture.runner.disposed.lock().await.contains(&login));
    assert!(
        fixture
            .state
            .channel_service()
            .repository
            .get_login(&fixture.scope, login)
            .await
            .is_ok()
    );
    let saved = fixture.record(canonical).await.session.unwrap();
    let blocked = fixture
        .call(
            "POST",
            "/api/v1/channel-login-sessions",
            json!({"project_id":fixture.scope.project_id,"account_id":canonical}),
        )
        .await;
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    // A live browser still owns the canonical identity after the timed
    // reservation expires. Advancing the reservation clock is not disposal.
    let later = chrono::Utc::now() + geo_domain::CHANNEL_EXECUTION_LEASE * 2;
    let competing = Uuid::new_v4();
    let blocked = fixture
        .state
        .channel_job_repository()
        .reserve_account(
            &fixture.scope,
            canonical,
            competing,
            later,
            later + geo_domain::CHANNEL_EXECUTION_LEASE,
        )
        .await;
    assert!(matches!(
        blocked,
        Err(error) if error.code == geo_domain::ErrorCode::Conflict
    ));
    let network_path = format!(
        "/api/v1/channel-accounts/{canonical}/network-binding?project_id={}",
        fixture.scope.project_id.unwrap()
    );
    let response = fixture.call("GET", &network_path, Value::Null).await;
    assert_eq!(response.status(), StatusCode::OK);
    let before_network = json_body(response).await;
    assert_eq!(before_network["busy"], true);
    let response = fixture
        .call(
            "PUT",
            &network_path,
            json!({
                "expected_version":before_network["version"],
                "source":{
                    "kind":"standalone",
                    "proxy":{"server":"http://127.0.0.1:8092"}
                }
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let pending_network = json_body(response).await;
    assert_eq!(pending_network["busy"], true);
    assert_eq!(pending_network["active"], before_network["active"]);
    assert_eq!(
        pending_network["pending"]["server"],
        "http://127.0.0.1:8092"
    );
    let response = fixture.complete(login).await;
    let status = response.status();
    let completed = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(uuid(&completed["account"]["account_id"]), canonical);
    assert_eq!(
        fixture
            .runner
            .completed
            .lock()
            .await
            .iter()
            .filter(|id| **id == login)
            .count(),
        1
    );
    assert_eq!(
        fixture
            .record(canonical)
            .await
            .session
            .unwrap()
            .encrypted_bytes(),
        saved.encrypted_bytes()
    );
    assert!(fixture.runner.disposed.lock().await.contains(&login));
    assert!(
        fixture
            .state
            .channel_service()
            .repository
            .get_login(&fixture.scope, login)
            .await
            .is_err()
    );
    let response = fixture.call("GET", &network_path, Value::Null).await;
    assert_eq!(response.status(), StatusCode::OK);
    let released_network = json_body(response).await;
    assert_eq!(released_network["busy"], false);
    assert_eq!(released_network["active"], pending_network["pending"]);
    assert!(released_network["pending"].is_null());
    let reservation = Uuid::new_v4();
    let now = chrono::Utc::now();
    fixture
        .state
        .channel_job_repository()
        .reserve_account(
            &fixture.scope,
            canonical,
            reservation,
            now,
            now + geo_domain::CHANNEL_EXECUTION_LEASE,
        )
        .await
        .unwrap();
    fixture
        .state
        .channel_job_repository()
        .release_account(&fixture.scope, canonical, reservation)
        .await
        .unwrap();
    let next = fixture.start(canonical).await;
    fixture.cancel(next).await;
}

#[tokio::test]
async fn identity_reassociation_never_crosses_project_tenant_operator_or_pool() {
    for boundary in ["project", "tenant", "operator", "pool"] {
        let fixture = Fixture::new().await;
        let mut outside = fixture.scope.clone();
        match boundary {
            "project" => outside.project_id = Some(Uuid::new_v4().into()),
            "tenant" => outside.tenant_id = Uuid::new_v4().into(),
            "operator" => outside.operator_id = Uuid::new_v4().into(),
            "pool" => {}
            _ => unreachable!(),
        }
        let canonical = Uuid::new_v4();
        let now = chrono::Utc::now();
        let record = ChannelAccountRecord {
            account: ChannelAccount {
                account_id: canonical,
                project_id: outside.project_id.unwrap(),
                owner_kind: ChannelOwnerKind::Customer,
                platform: "zhihu".into(),
                group_id: None,
                status: ChannelStatus::Ready,
                display_name: Some("Synthetic protected account".into()),
                platform_account_id: Some(IDENTITY.into()),
                avatar_url: None,
                enabled: true,
                proxy_configured: false,
                proxy_server: None,
                created_at: now,
                updated_at: now,
            },
            session: Some(ChannelSecret::new(vec![1, 2, 3])),
            proxy: None,
        };
        let repository = &fixture.state.channel_service().repository;
        if boundary == "pool" {
            repository
                .save_pool_account(
                    outside.operator_id,
                    PoolAccountRecord {
                        account: PoolAccount {
                            account_id: canonical,
                            platform: record.account.platform.clone(),
                            group_id: None,
                            status: ChannelStatus::Ready,
                            display_name: record.account.display_name.clone(),
                            platform_account_id: record.account.platform_account_id.clone(),
                            avatar_url: None,
                            enabled: true,
                            proxy_configured: false,
                            proxy_server: None,
                            created_at: now,
                            updated_at: now,
                        },
                        session: record.session.clone(),
                        proxy: None,
                    },
                )
                .await
                .unwrap();
        } else {
            repository.save_account(&outside, record).await.unwrap();
        }
        let placeholder = fixture.create_account(None).await;
        let login = fixture.start(placeholder).await;
        let response = fixture.complete(login).await;
        if matches!(boundary, "project" | "tenant" | "pool") {
            assert_eq!(response.status(), StatusCode::CONFLICT, "{boundary}");
            assert!(fixture.record(placeholder).await.session.is_none());
            fixture.cancel(login).await;
        } else {
            assert_eq!(response.status(), StatusCode::OK, "{boundary}");
            assert_eq!(
                uuid(&json_body(response).await["account"]["account_id"]),
                placeholder,
                "{boundary}"
            );
        }
        let protected = if boundary == "pool" {
            repository
                .get_pool_account(outside.operator_id, canonical)
                .await
                .unwrap()
                .session
                .unwrap()
        } else {
            repository
                .get_account(&outside, canonical)
                .await
                .unwrap()
                .session
                .unwrap()
        };
        assert_eq!(protected.encrypted_bytes(), &[1, 2, 3], "{boundary}");
    }
}
