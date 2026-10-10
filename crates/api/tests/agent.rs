use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use futures_util::StreamExt;
use geo_api::{
    AppState, CSRF_HEADER, EventBus, MemoryIdempotencyStore, MemoryOperationStore, router,
};
use geo_domain::{
    AppError, AppendMessage, AuthRepository, CreateConversation, DEVELOPMENT_OPERATOR_ID,
    DEVELOPMENT_TENANT_ID, LoginIdentity, Membership, MemoryAuthRepository, Operator, OperatorId,
    RuntimeCapability, Session, SessionCredentials, SessionId, TenantScope, User, UserId,
};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;

struct RevocableMembershipRepository {
    inner: MemoryAuthRepository,
    membership_active: AtomicBool,
}

impl RevocableMembershipRepository {
    fn new() -> Self {
        Self {
            inner: MemoryAuthRepository::development_with_password("test-password"),
            membership_active: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl AuthRepository for RevocableMembershipRepository {
    async fn operator_for_host(&self, host: &str) -> Result<Option<Operator>, AppError> {
        self.inner.operator_for_host(host).await
    }

    async fn authenticate(
        &self,
        operator_id: OperatorId,
        email: &str,
        password: &str,
    ) -> Result<Option<LoginIdentity>, AppError> {
        self.inner.authenticate(operator_id, email, password).await
    }

    async fn find_session(
        &self,
        operator_id: OperatorId,
        token: &str,
    ) -> Result<Option<Session>, AppError> {
        self.inner.find_session(operator_id, token).await
    }

    async fn find_user(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
    ) -> Result<Option<User>, AppError> {
        self.inner.find_user(operator_id, user_id).await
    }

    async fn memberships(
        &self,
        user_id: UserId,
        operator_id: OperatorId,
    ) -> Result<Vec<Membership>, AppError> {
        if !self.membership_active.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        self.inner.memberships(user_id, operator_id).await
    }

    async fn create_session(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
        ttl: chrono::Duration,
    ) -> Result<SessionCredentials, AppError> {
        self.inner.create_session(operator_id, user_id, ttl).await
    }

    async fn revoke_session(
        &self,
        operator_id: OperatorId,
        session_id: SessionId,
    ) -> Result<(), AppError> {
        self.inner.revoke_session(operator_id, session_id).await
    }
}

async fn login(app: &Router) -> (String, String) {
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
                    r#"{"login_name":"demo@localhost","password":"test-password"}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("login response");
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("cookie header")
        .split(';')
        .next()
        .expect("cookie value")
        .to_owned();
    let body = to_bytes(response.into_body(), 16 * 1024)
        .await
        .expect("login body");
    let body: Value = serde_json::from_slice(&body).expect("login json");
    (
        cookie,
        body["csrf_token"].as_str().expect("csrf token").to_owned(),
    )
}

fn request(
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    key: Option<&str>,
    body: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie)
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

#[tokio::test]
async fn agent_conversation_submission_is_scoped_idempotent_and_honest_about_runtime() {
    let app = router(AppState::development_with_password("test-password"));
    let (cookie, csrf) = login(&app).await;
    let tenant_id = DEVELOPMENT_TENANT_ID;
    let project_id = uuid::Uuid::new_v4();
    let project_id_text = project_id.to_string();

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/agent/conversations?tenant_id={tenant_id}&project_id={project_id_text}"
            ),
            &cookie,
            Some(&csrf),
            Some("conversation-1"),
            r#"{"title":"P00"}"#,
        ))
        .await
        .expect("create response");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(
        &to_bytes(created.into_body(), 64 * 1024)
            .await
            .expect("create body"),
    )
    .expect("create json");
    let conversation_id = created["id"].as_str().expect("conversation id");

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations?tenant_id={tenant_id}&project_id={project_id_text}"
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("list response");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: Value = serde_json::from_slice(
        &to_bytes(listed.into_body(), 64 * 1024)
            .await
            .expect("list body"),
    )
    .expect("list json");
    assert_eq!(listed["items"].as_array().expect("items").len(), 1);

    let other_project = uuid::Uuid::new_v4();
    let isolated = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations?tenant_id={tenant_id}&project_id={other_project}"
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("isolated response");
    assert_eq!(isolated.status(), StatusCode::OK);
    let isolated: Value = serde_json::from_slice(
        &to_bytes(isolated.into_body(), 64 * 1024)
            .await
            .expect("isolated body"),
    )
    .expect("isolated json");
    assert!(isolated["items"].as_array().expect("items").is_empty());

    let message_uri = format!(
        "/api/v1/agent/conversations/{conversation_id}/messages?tenant_id={tenant_id}&project_id={project_id_text}"
    );
    let submitted = app
        .clone()
        .oneshot(request(
            "POST",
            &message_uri,
            &cookie,
            Some(&csrf),
            Some("message-1"),
            r#"{"content":"hello"}"#,
        ))
        .await
        .expect("submit response");
    assert_eq!(submitted.status(), StatusCode::ACCEPTED);
    let submitted_body = to_bytes(submitted.into_body(), 64 * 1024)
        .await
        .expect("submit body");
    let submitted_json: Value = serde_json::from_slice(&submitted_body).expect("submit json");
    assert_eq!(submitted_json["status"], "accepted");
    assert_eq!(submitted_json["run_status"], "failed");
    assert_eq!(submitted_json["error"]["code"], "capability_missing");

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &message_uri,
            &cookie,
            Some(&csrf),
            Some("message-1"),
            r#"{"content":"hello"}"#,
        ))
        .await
        .expect("replay response");
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(
        to_bytes(replay.into_body(), 64 * 1024)
            .await
            .expect("replay body"),
        submitted_body
    );

    let events = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations/{conversation_id}/events?tenant_id={tenant_id}&project_id={project_id_text}&after=0"
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("events response");
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(
        events
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut malformed_cursor = request(
        "GET",
        &format!(
            "/api/v1/agent/conversations/{conversation_id}/events?tenant_id={tenant_id}&project_id={project_id_text}"
        ),
        &cookie,
        None,
        None,
        "",
    );
    malformed_cursor
        .headers_mut()
        .insert("last-event-id", "not-a-sequence".parse().unwrap());
    let malformed = app
        .clone()
        .oneshot(malformed_cursor)
        .await
        .expect("malformed cursor response");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    // An explicit query cursor is authoritative, so a stale/bad browser
    // header cannot prevent a caller from deliberately restarting at zero.
    let mut query_precedence = request(
        "GET",
        &format!(
            "/api/v1/agent/conversations/{conversation_id}/events?tenant_id={tenant_id}&project_id={project_id_text}&after=0"
        ),
        &cookie,
        None,
        None,
        "",
    );
    query_precedence
        .headers_mut()
        .insert("last-event-id", "not-a-sequence".parse().unwrap());
    let query_precedence = app
        .oneshot(query_precedence)
        .await
        .expect("query precedence response");
    assert_eq!(query_precedence.status(), StatusCode::OK);
}

#[tokio::test]
async fn agent_event_cursor_replays_exactly_after_last_event_id() {
    let state = AppState::development_with_password("test-password");
    let app = router(state.clone());
    let (cookie, _) = login(&app).await;
    let project_id = uuid::Uuid::new_v4();
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id.into()),
    );
    let conversation = state
        .agent_repository()
        .create_conversation(&scope, None, CreateConversation::default())
        .await
        .expect("conversation");
    let acceptance = state
        .agent_repository()
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: "hello".to_owned(),
                attachments: Vec::new(),
                metadata: Value::Null,
            },
            "cursor-key".to_owned(),
            "cursor-body".to_owned(),
            RuntimeCapability::missing("test"),
        )
        .await
        .expect("append");
    assert_eq!(acceptance.run.conversation_id, conversation.id);

    let mut resume = request(
        "GET",
        &format!(
            "/api/v1/agent/conversations/{}/events?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}",
            conversation.id
        ),
        &cookie,
        None,
        None,
        "",
    );
    resume
        .headers_mut()
        .insert("last-event-id", "1".parse().unwrap());
    let response = app.clone().oneshot(resume).await.expect("resume response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut replay_chunks = response.into_body().into_data_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(2), replay_chunks.next())
        .await
        .expect("replayed event should arrive")
        .expect("stream is open")
        .expect("SSE chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("UTF-8 SSE");
    assert!(text.contains("id: 2"), "{text}");
    assert!(!text.contains("id: 1"), "{text}");

    let mut next_resume = request(
        "GET",
        &format!(
            "/api/v1/agent/conversations/{}/events?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}",
            conversation.id
        ),
        &cookie,
        None,
        None,
        "",
    );
    next_resume
        .headers_mut()
        .insert("last-event-id", "2".parse().unwrap());
    let response = app
        .oneshot(next_resume)
        .await
        .expect("next resume response");
    let mut next_chunks = response.into_body().into_data_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(2), next_chunks.next())
        .await
        .expect("next event should arrive")
        .expect("stream is open")
        .expect("SSE chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("UTF-8 SSE");
    assert!(text.contains("id: 3"), "{text}");
    assert!(!text.contains("id: 2"), "{text}");

    // The first connection still has event 3 queued in its historical replay.
    // Revocation must prevent that replay item from leaking as well.
    let token = cookie.split_once('=').expect("cookie token").1;
    let auth_repository = state.auth_repository();
    let session = auth_repository
        .find_session(DEVELOPMENT_OPERATOR_ID, token)
        .await
        .expect("session lookup")
        .expect("session");
    auth_repository
        .revoke_session(DEVELOPMENT_OPERATOR_ID, session.id)
        .await
        .expect("revoke");
    let chunk = tokio::time::timeout(Duration::from_secs(2), replay_chunks.next())
        .await
        .expect("auth event should arrive")
        .expect("stream is open")
        .expect("SSE chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("UTF-8 SSE");
    assert!(text.contains("event: auth.revoked"), "{text}");
    assert!(!text.contains("id: 3"), "{text}");
}

#[tokio::test]
async fn agent_event_stream_closes_after_session_revocation_before_new_event() {
    let state = AppState::development_with_password("test-password");
    let app = router(state.clone());
    let (cookie, _) = login(&app).await;
    let project_id = uuid::Uuid::new_v4();
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id.into()),
    );
    let conversation = state
        .agent_repository()
        .create_conversation(&scope, None, CreateConversation::default())
        .await
        .expect("conversation");
    let response = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations/{}/events?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}&after=1",
                conversation.id
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("stream response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut chunks = response.into_body().into_data_stream();
    let token = cookie.split_once('=').expect("cookie token").1;
    let auth_repository = state.auth_repository();
    let session = auth_repository
        .find_session(DEVELOPMENT_OPERATOR_ID, token)
        .await
        .expect("session lookup")
        .expect("session");
    auth_repository
        .revoke_session(DEVELOPMENT_OPERATOR_ID, session.id)
        .await
        .expect("revoke");
    state
        .agent_repository()
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: "not delivered".to_owned(),
                attachments: Vec::new(),
                metadata: Value::Null,
            },
            "revoked-key".to_owned(),
            "revoked-body".to_owned(),
            RuntimeCapability::missing("test"),
        )
        .await
        .expect("append");

    let chunk = tokio::time::timeout(Duration::from_secs(2), chunks.next())
        .await
        .expect("auth event should arrive")
        .expect("stream is open")
        .expect("SSE chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("UTF-8 SSE");
    assert!(text.contains("event: auth.revoked"), "{text}");
    assert!(!text.contains("not delivered"), "{text}");
    assert!(
        tokio::time::timeout(Duration::from_secs(2), chunks.next())
            .await
            .expect("stream should close")
            .is_none()
    );
}

#[tokio::test]
async fn agent_event_stream_closes_when_tenant_membership_is_removed() {
    let auth = Arc::new(RevocableMembershipRepository::new());
    let state = AppState::with_stores_and_auth(
        Arc::new(MemoryOperationStore::default()),
        Arc::new(MemoryIdempotencyStore::default()),
        auth.clone(),
        EventBus::default(),
        false,
    );
    let app = router(state.clone());
    let (cookie, _) = login(&app).await;
    let project_id = uuid::Uuid::new_v4();
    let scope = TenantScope::new(
        DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID,
        Some(project_id.into()),
    );
    let conversation = state
        .agent_repository()
        .create_conversation(&scope, None, CreateConversation::default())
        .await
        .expect("conversation");
    let response = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations/{}/events?tenant_id={DEVELOPMENT_TENANT_ID}&project_id={project_id}&after=1",
                conversation.id
            ),
            &cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("stream response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut chunks = response.into_body().into_data_stream();
    auth.membership_active.store(false, Ordering::Release);
    state
        .agent_repository()
        .append_message(
            &scope,
            conversation.id,
            AppendMessage {
                content: "not delivered".to_owned(),
                attachments: Vec::new(),
                metadata: Value::Null,
            },
            "membership-key".to_owned(),
            "membership-body".to_owned(),
            RuntimeCapability::missing("test"),
        )
        .await
        .expect("append");
    let chunk = tokio::time::timeout(Duration::from_secs(2), chunks.next())
        .await
        .expect("auth event should arrive")
        .expect("stream is open")
        .expect("SSE chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("UTF-8 SSE");
    assert!(text.contains("event: auth.revoked"), "{text}");
    assert!(!text.contains("not delivered"), "{text}");
    assert!(
        tokio::time::timeout(Duration::from_secs(2), chunks.next())
            .await
            .expect("stream should close")
            .is_none()
    );
}
