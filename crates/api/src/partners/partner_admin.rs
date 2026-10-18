//! First-party administration. One-time bearer responses bypass persisted
//! command idempotency; list and revoke responses contain metadata only.
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, PartnerAdminIssuedKey, PartnerAdminKey, PartnerAdminRepository, ProjectId,
    Role, TenantId, TenantScope,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyCommand {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueKey {
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentExportPermission {
    content_export: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    // Browser selectors must agree with authenticated scope, never create it.
    tenant_id: Option<TenantId>,
    project_id: Option<ProjectId>,
    after: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    50
}

impl PageQuery {
    fn validate(&self, scope: &TenantScope) -> Result<(), AppError> {
        if self.tenant_id.is_some_and(|id| id != scope.tenant_id)
            || self
                .project_id
                .is_some_and(|id| Some(id) != scope.project_id)
        {
            return Err(AppError::invalid_request("scope selector mismatch"));
        }
        if !(1..=100).contains(&self.limit) {
            return Err(AppError::invalid_request(
                "page limit must be between 1 and 100",
            ));
        }
        Ok(())
    }
}

// The sole opt-in serialization boundary for a freshly issued bearer.
#[derive(Serialize)]
struct IssuedKeyResponse {
    key: PartnerAdminKey,
    bearer: String,
}

fn issued_response(issued: PartnerAdminIssuedKey) -> Response {
    (
        StatusCode::CREATED,
        Json(IssuedKeyResponse {
            key: issued.key,
            bearer: issued.bearer,
        }),
    )
        .into_response()
}

fn safe_error(error: AppError, ctx: RequestContext) -> ApiError {
    let message = match error.code {
        ErrorCode::InvalidRequest => "invalid partner administration request",
        ErrorCode::Unauthorized => "authentication required",
        ErrorCode::Forbidden => "partner administration is not permitted",
        ErrorCode::NotFound => "partner resource not found",
        ErrorCode::Conflict => "partner resource state changed",
        ErrorCode::NotReady => "partner resource is not ready",
        ErrorCode::CapabilityMissing => "partner administration is unavailable",
        ErrorCode::DependencyUnavailable => "partner administration storage is unavailable",
        ErrorCode::Internal => "partner administration failed",
    };
    api_error(AppError::new(error.code, message), ctx.request_id)
}

fn body<T>(input: Result<Json<T>, JsonRejection>, ctx: RequestContext) -> Result<T, ApiError> {
    input.map(|Json(value)| value).map_err(|_| {
        safe_error(
            AppError::invalid_request("invalid partner administration body"),
            ctx,
        )
    })
}

async fn scope(
    state: &AppState,
    auth: &AuthContext,
    project: ProjectId,
) -> Result<TenantScope, AppError> {
    if !matches!(
        crate::selected_membership(auth).map(|membership| membership.role),
        Some(Role::CustomerAdmin)
    ) {
        return Err(AppError::forbidden("customer administrator required"));
    }
    state
        .project_repository()
        .get(&auth.scope, project)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(TenantScope::new(
        auth.scope.operator_id,
        auth.scope.tenant_id,
        Some(project),
    ))
}

fn repository(state: &AppState) -> Result<Arc<dyn PartnerAdminRepository>, AppError> {
    state
        .partner_admin_repository()
        .ok_or_else(|| AppError::capability_missing("partner administration unavailable"))
}

async fn list_connections(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path(project): Path<ProjectId>,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    let Query(query) = query.map_err(|_| map(AppError::invalid_request("invalid pagination")))?;
    query.validate(&scope).map_err(map)?;
    Ok(Json(
        repository(&state)
            .map_err(map)?
            .list_connections(&scope, query.after, query.limit)
            .await
            .map_err(map)?,
    )
    .into_response())
}

async fn create_connection(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path(project): Path<ProjectId>,
    input: Result<Json<EmptyCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    body(input, ctx)?;
    Ok((
        StatusCode::CREATED,
        Json(
            repository(&state)
                .map_err(map)?
                .create_connection(&scope)
                .await
                .map_err(map)?,
        ),
    )
        .into_response())
}

async fn set_permissions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((project, connection)): Path<(ProjectId, Uuid)>,
    input: Result<Json<ContentExportPermission>, JsonRejection>,
) -> Result<Response, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    let input = body(input, ctx)?;
    Ok(Json(
        repository(&state)
            .map_err(map)?
            .set_content_export(&scope, connection, input.content_export)
            .await
            .map_err(map)?,
    )
    .into_response())
}

async fn list_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((project, connection)): Path<(ProjectId, Uuid)>,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    let Query(query) = query.map_err(|_| map(AppError::invalid_request("invalid pagination")))?;
    query.validate(&scope).map_err(map)?;
    Ok(Json(
        repository(&state)
            .map_err(map)?
            .list_keys(&scope, connection, query.after, query.limit)
            .await
            .map_err(map)?,
    )
    .into_response())
}

async fn issue_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((project, connection)): Path<(ProjectId, Uuid)>,
    input: Result<Json<IssueKey>, JsonRejection>,
) -> Result<Response, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    let input = body(input, ctx)?;
    Ok(issued_response(
        repository(&state)
            .map_err(map)?
            .issue_key(&scope, connection, input.expires_at)
            .await
            .map_err(map)?,
    ))
}

async fn rotate_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((project, connection, key)): Path<(ProjectId, Uuid, Uuid)>,
    input: Result<Json<IssueKey>, JsonRejection>,
) -> Result<Response, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    let input = body(input, ctx)?;
    Ok(issued_response(
        repository(&state)
            .map_err(map)?
            .rotate_key(&scope, connection, key, input.expires_at)
            .await
            .map_err(map)?,
    ))
}

async fn revoke_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((project, connection, key)): Path<(ProjectId, Uuid, Uuid)>,
    input: Result<Json<EmptyCommand>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    body(input, ctx)?;
    repository(&state)
        .map_err(map)?
        .revoke_key(&scope, connection, key)
        .await
        .map_err(map)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_connection(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((project, connection)): Path<(ProjectId, Uuid)>,
    input: Result<Json<EmptyCommand>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let map = |error| safe_error(error, ctx);
    let scope = scope(&state, &auth, project).await.map_err(map)?;
    body(input, ctx)?;
    repository(&state)
        .map_err(map)?
        .revoke_connection(&scope, connection)
        .await
        .map_err(map)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/partner-connections",
            get(list_connections).post(create_connection),
        )
        .route(
            "/projects/{project_id}/partner-connections/{connection_id}/permissions",
            post(set_permissions),
        )
        .route(
            "/projects/{project_id}/partner-connections/{connection_id}/keys",
            get(list_keys).post(issue_key),
        )
        .route(
            "/projects/{project_id}/partner-connections/{connection_id}/keys/{key_id}/rotate",
            post(rotate_key),
        )
        .route(
            "/projects/{project_id}/partner-connections/{connection_id}/keys/{key_id}/revoke",
            post(revoke_key),
        )
        .route(
            "/projects/{project_id}/partner-connections/{connection_id}/revoke",
            post(revoke_connection),
        )
        .layer(middleware::from_fn(crate::csrf_origin_from_request))
        .layer(middleware::from_fn(crate::auth_scope_from_request))
        .layer(middleware::from_fn(crate::no_store_middleware))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use geo_domain::{
        AuthRepository, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, Membership,
        MemoryAuthRepository, PartnerAdminConnection, PartnerAdminConnectionPage,
        PartnerAdminKeyPage, ProjectCreate, ProjectSettings, User,
    };
    use serde_json::{Value, json};
    use std::sync::Mutex;
    use tower::ServiceExt;

    const SYNTHETIC_BEARER: &str = "synthetic-one-time-bearer";

    struct Fake {
        scope: TenantScope,
        connection: Uuid,
        key: PartnerAdminKey,
        calls: Mutex<Vec<String>>,
        failure: Mutex<Option<AppError>>,
    }

    impl Fake {
        fn record(&self, scope: &TenantScope, action: String) -> Result<(), AppError> {
            assert_eq!(scope, &self.scope);
            self.calls.lock().unwrap().push(action);
            if let Some(error) = self.failure.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(())
        }

        fn connection(&self) -> PartnerAdminConnection {
            PartnerAdminConnection {
                connection_id: self.connection,
                knowledge_write: true,
                content_export: false,
                created_at: self.key.created_at,
                revoked_at: None,
            }
        }

        fn issued(&self, expires_at: Option<DateTime<Utc>>) -> PartnerAdminIssuedKey {
            let mut key = self.key.clone();
            key.expires_at = expires_at;
            PartnerAdminIssuedKey {
                key,
                bearer: SYNTHETIC_BEARER.into(),
            }
        }
    }

    #[async_trait]
    impl PartnerAdminRepository for Fake {
        async fn list_connections(
            &self,
            scope: &TenantScope,
            after: Option<Uuid>,
            limit: u32,
        ) -> Result<PartnerAdminConnectionPage, AppError> {
            self.record(scope, format!("connections:{after:?}:{limit}"))?;
            Ok(PartnerAdminConnectionPage {
                items: vec![self.connection()],
                next_cursor: Some(self.connection),
            })
        }
        async fn create_connection(
            &self,
            scope: &TenantScope,
        ) -> Result<PartnerAdminConnection, AppError> {
            self.record(scope, "create".into())?;
            Ok(self.connection())
        }
        async fn set_content_export(
            &self,
            scope: &TenantScope,
            connection: Uuid,
            enabled: bool,
        ) -> Result<PartnerAdminConnection, AppError> {
            assert_eq!(connection, self.connection);
            self.record(scope, format!("content-export:{enabled}"))?;
            let mut connection = self.connection();
            connection.content_export = enabled;
            Ok(connection)
        }
        async fn list_keys(
            &self,
            scope: &TenantScope,
            connection: Uuid,
            after: Option<Uuid>,
            limit: u32,
        ) -> Result<PartnerAdminKeyPage, AppError> {
            assert_eq!(connection, self.connection);
            self.record(scope, format!("keys:{after:?}:{limit}"))?;
            Ok(PartnerAdminKeyPage {
                items: vec![self.key.clone()],
                next_cursor: None,
            })
        }
        async fn issue_key(
            &self,
            scope: &TenantScope,
            connection: Uuid,
            expires_at: Option<DateTime<Utc>>,
        ) -> Result<PartnerAdminIssuedKey, AppError> {
            assert_eq!(connection, self.connection);
            self.record(scope, "issue".into())?;
            Ok(self.issued(expires_at))
        }
        async fn rotate_key(
            &self,
            scope: &TenantScope,
            connection: Uuid,
            key: Uuid,
            expires_at: Option<DateTime<Utc>>,
        ) -> Result<PartnerAdminIssuedKey, AppError> {
            assert_eq!((connection, key), (self.connection, self.key.key_id));
            self.record(scope, "rotate".into())?;
            Ok(self.issued(expires_at))
        }
        async fn revoke_key(
            &self,
            scope: &TenantScope,
            connection: Uuid,
            key: Uuid,
        ) -> Result<(), AppError> {
            assert_eq!((connection, key), (self.connection, self.key.key_id));
            self.record(scope, "revoke-key".into())
        }
        async fn revoke_connection(
            &self,
            scope: &TenantScope,
            connection: Uuid,
        ) -> Result<(), AppError> {
            assert_eq!(connection, self.connection);
            self.record(scope, "revoke-connection".into())
        }
    }

    struct Fixture {
        app: Router,
        fake: Arc<Fake>,
        base: String,
        foreign_base: String,
        cookie: String,
        csrf: String,
    }

    impl Fixture {
        async fn new(role: Role) -> Self {
            let auth = Arc::new(MemoryAuthRepository::development_with_password(
                "synthetic-password",
            ));
            let user = User::new(
                Uuid::new_v4().into(),
                DEVELOPMENT_OPERATOR_ID,
                "synthetic-admin@localhost",
                "Synthetic",
                "synthetic-password",
            )
            .unwrap();
            auth.insert_user(user.clone()).await.unwrap();
            auth.insert_membership(Membership::new(
                user.id,
                DEVELOPMENT_OPERATOR_ID,
                DEVELOPMENT_TENANT_ID,
                role,
            ))
            .await
            .unwrap();
            let credentials = auth
                .create_persistent_session(DEVELOPMENT_OPERATOR_ID, user.id)
                .await
                .unwrap();
            let state = AppState::with_stores_and_auth_and_projects(
                Arc::new(crate::MemoryOperationStore::default()),
                Arc::new(crate::MemoryIdempotencyStore::default()),
                auth,
                Arc::new(geo_domain::MemoryProjectRepository::default()),
                crate::EventBus::default(),
                false,
            );
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
            let foreign = state
                .project_repository()
                .create(
                    &TenantScope::new(DEVELOPMENT_OPERATOR_ID, Uuid::new_v4().into(), None),
                    ProjectCreate {
                        slug: None,
                        display_name: "Synthetic other tenant".into(),
                        settings: ProjectSettings::default(),
                    },
                )
                .await
                .unwrap();
            let fake = Arc::new(Fake {
                scope: TenantScope::new(tenant.operator_id, tenant.tenant_id, Some(project.id)),
                connection: Uuid::new_v4(),
                key: PartnerAdminKey {
                    key_id: Uuid::new_v4(),
                    created_at: Utc::now(),
                    expires_at: None,
                    revoked_at: None,
                },
                calls: Mutex::new(Vec::new()),
                failure: Mutex::new(None),
            });
            Self {
                app: crate::router(state.with_partner_admin_repository(fake.clone())),
                fake,
                base: format!("/api/v1/projects/{}/partner-connections", project.id),
                foreign_base: format!("/api/v1/projects/{}/partner-connections", foreign.id),
                cookie: format!("geo_dev_session={}", credentials.token),
                csrf: credentials.session.csrf_token().to_owned(),
            }
        }

        fn request(&self, method: &str, path: &str, payload: &str) -> Request<Body> {
            let mut uri = path.to_owned();
            for (name, value) in [
                ("tenant_id", self.fake.scope.tenant_id.to_string()),
                (
                    "project_id",
                    self.fake.scope.project_id.unwrap().to_string(),
                ),
            ] {
                if !uri.contains(&format!("?{name}=")) && !uri.contains(&format!("&{name}=")) {
                    let separator = if uri.contains('?') { '&' } else { '?' };
                    uri.push_str(&format!("{separator}{name}={value}"));
                }
            }
            Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &self.cookie)
                .header(crate::CSRF_HEADER, &self.csrf)
                .body(Body::from(payload.to_owned()))
                .unwrap()
        }

        async fn send(&self, request: Request<Body>) -> (StatusCode, String) {
            let response = self.app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
            (status, String::from_utf8(bytes.to_vec()).unwrap())
        }
    }

    #[tokio::test]
    async fn export_permission_is_separate_and_requires_an_explicit_boolean() {
        let f = Fixture::new(Role::CustomerAdmin).await;
        let (status, value) = f.send(f.request("POST", &f.base, "{}")).await;
        assert_eq!(status, StatusCode::CREATED);
        let value: Value = serde_json::from_str(&value).unwrap();
        assert_eq!(value["content_export"], false);
        assert_eq!(value["knowledge_write"], true);
        let path = format!("{}/{}/permissions", f.base, f.fake.connection);
        for enabled in [true, false] {
            let payload = json!({"content_export": enabled}).to_string();
            let (status, value) = f.send(f.request("POST", &path, &payload)).await;
            assert_eq!(status, StatusCode::OK);
            let value: Value = serde_json::from_str(&value).unwrap();
            assert_eq!(value["content_export"], enabled);
            assert_eq!(value["knowledge_write"], true);
            assert!(value.get("bearer").is_none());
        }
        for payload in [
            "{}",
            "null",
            r#"{"content_export":"true"}"#,
            r#"{"content_export":true,"knowledge_write":true}"#,
            r#"{"content_export":true,"content_export":false}"#,
        ] {
            assert_eq!(
                f.send(f.request("POST", &path, payload)).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        let mut request = f.request("POST", &path, r#"{"content_export":true}"#);
        request.headers_mut().remove(crate::CSRF_HEADER);
        assert_eq!(f.send(request).await.0, StatusCode::FORBIDDEN);
        let f = Fixture::new(Role::CustomerMember).await;
        let path = format!("{}/{}/permissions", f.base, f.fake.connection);
        assert_eq!(
            f.send(f.request("POST", &path, r#"{"content_export":true}"#))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert!(f.fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn metadata_lists_and_revocations_never_return_bearer() {
        let f = Fixture::new(Role::CustomerAdmin).await;
        let keys = format!("{}/{}/keys", f.base, f.fake.connection);
        let key = format!("{}/{}", keys, f.fake.key.key_id);
        for (method, path, expected) in [
            ("GET", f.base.clone(), StatusCode::OK),
            ("POST", f.base.clone(), StatusCode::CREATED),
            ("GET", keys.clone(), StatusCode::OK),
            ("POST", format!("{key}/revoke"), StatusCode::NO_CONTENT),
            (
                "POST",
                format!("{}/{}/revoke", f.base, f.fake.connection),
                StatusCode::NO_CONTENT,
            ),
        ] {
            let (status, body) = f.send(f.request(method, &path, "{}")).await;
            assert_eq!(status, expected);
            assert!(!body.contains("bearer"));
            assert!(!body.contains(SYNTHETIC_BEARER));
            if status == StatusCode::NO_CONTENT {
                assert!(body.is_empty());
            }
        }
        for path in [keys, format!("{key}/rotate")] {
            let (status, body) = f.send(f.request("POST", &path, "{}")).await;
            assert_eq!(status, StatusCode::CREATED);
            let value: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["bearer"], SYNTHETIC_BEARER);
            assert!(value["key"]["expires_at"].is_null());
            assert_eq!(value.as_object().unwrap().len(), 2);
        }
        let (status, _) = f
            .send(f.request(
                "GET",
                &format!("{}?after={}&limit=7", f.base, f.fake.connection),
                "",
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            f.fake
                .calls
                .lock()
                .unwrap()
                .contains(&format!("connections:Some({}):7", f.fake.connection))
        );
    }

    #[tokio::test]
    async fn first_party_session_csrf_and_project_scope_are_required() {
        let f = Fixture::new(Role::CustomerAdmin).await;
        let mut request = f.request("POST", &f.base, "{}");
        request.headers_mut().remove(header::COOKIE);
        request.headers_mut().insert(
            header::AUTHORIZATION,
            "Bearer synthetic-service-key".parse().unwrap(),
        );
        assert_eq!(f.send(request).await.0, StatusCode::UNAUTHORIZED);
        let mut request = f.request("POST", &f.base, "{}");
        request.headers_mut().remove(crate::CSRF_HEADER);
        assert_eq!(f.send(request).await.0, StatusCode::FORBIDDEN);
        let unknown = format!("/api/v1/projects/{}/partner-connections", Uuid::new_v4());
        assert_eq!(
            f.send(f.request("GET", &unknown, "")).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            f.send(f.request("GET", &f.foreign_base, "")).await.0,
            StatusCode::NOT_FOUND
        );
        let mismatch = format!("{}?project_id={}", f.base, Uuid::new_v4());
        assert_eq!(
            f.send(f.request("GET", &mismatch, "")).await.0,
            StatusCode::BAD_REQUEST
        );
        let other_tenant = format!("{}?tenant_id={}", f.base, Uuid::new_v4());
        assert_eq!(
            f.send(f.request("GET", &other_tenant, "")).await.0,
            StatusCode::FORBIDDEN
        );
        // A header-selected membership cannot be changed by a conflicting query.
        let mut request = f.request("GET", &other_tenant, "");
        request.headers_mut().insert(
            crate::TENANT_SELECTOR_HEADER,
            f.fake.scope.tenant_id.to_string().parse().unwrap(),
        );
        assert_eq!(f.send(request).await.0, StatusCode::BAD_REQUEST);
        assert!(f.fake.calls.lock().unwrap().is_empty());
        for role in [
            Role::CustomerMember,
            Role::CustomerReadOnly,
            Role::Operator,
            Role::ResourceAdmin,
            Role::OemAdmin,
        ] {
            let f = Fixture::new(role).await;
            for method in ["GET", "POST"] {
                assert_eq!(
                    f.send(f.request(method, &f.base, "{}")).await.0,
                    StatusCode::FORBIDDEN
                );
            }
            assert!(f.fake.calls.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn strict_dtos_and_sanitized_repository_errors() {
        let f = Fixture::new(Role::CustomerAdmin).await;
        let keys = format!("{}/{}/keys", f.base, f.fake.connection);
        for (path, payload) in [
            (f.base.clone(), r#"{"tenant_id":"forged"}"#),
            (f.base.clone(), "null"),
            (keys.clone(), r#"{"expires_at":"not-a-date"}"#),
            (keys.clone(), r#"{"secret":"synthetic-secret"}"#),
            (keys.clone(), r#"{"expires_at":null,"expires_at":null}"#),
        ] {
            assert_eq!(
                f.send(f.request("POST", &path, payload)).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        for query in [
            "limit=0",
            "limit=101",
            "limit=-1",
            "limit=2&limit=3",
            "after=bad",
            "tenant_id=forged",
            "unexpected=forged",
        ] {
            for path in [&f.base, &keys] {
                assert_eq!(
                    f.send(f.request("GET", &format!("{path}?{query}"), ""))
                        .await
                        .0,
                    StatusCode::BAD_REQUEST
                );
            }
        }
        assert!(f.fake.calls.lock().unwrap().is_empty());
        *f.fake.failure.lock().unwrap() = Some(
            AppError::new(ErrorCode::DependencyUnavailable, SYNTHETIC_BEARER)
                .with_details(json!({"verification_hash":"synthetic-private-hash"})),
        );
        let (status, body) = f.send(f.request("POST", &keys, "{}")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!body.contains(SYNTHETIC_BEARER));
        assert!(!body.contains("verification_hash"));
        assert!(!body.contains("synthetic-private-hash"));
        assert_eq!(*f.fake.calls.lock().unwrap(), ["issue"]);
    }
}
