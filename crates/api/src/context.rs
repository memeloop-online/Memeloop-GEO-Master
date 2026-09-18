use axum::{
    extract::{Extension, Request, State},
    http::{HeaderMap, HeaderValue, Method, header::HeaderName},
    middleware::Next,
    response::Response,
};
use geo_domain::{
    AppError, AuthRepository, Membership, Operator, OperatorId, ProjectId, Session, TenantId,
    TenantScope, User,
};
use std::sync::Arc;
use uuid::Uuid;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
pub const CORRELATION_ID_HEADER: &str = "x-correlation-id";
pub const OPERATOR_ID_HEADER: &str = "x-operator-id";
pub const TENANT_ID_HEADER: &str = "x-tenant-id";
pub const PROJECT_ID_HEADER: &str = "x-project-id";
pub const TENANT_SELECTOR_HEADER: &str = "x-tenant-selector";
pub const CSRF_HEADER: &str = "x-csrf-token";
pub const SESSION_COOKIE_NAME: &str = "__Host-geo_session";
pub const DEV_SESSION_COOKIE_NAME: &str = "geo_dev_session";

#[derive(Clone, Copy)]
pub struct SessionCookieConfig(pub &'static str);

#[derive(Clone)]
pub struct AuthMiddlewareState {
    pub repository: SharedAuthRepository,
    pub cookie_name: &'static str,
    pub origin_config: OriginConfig,
}

pub type SharedAuthRepository = Arc<dyn AuthRepository>;

#[derive(Debug, Clone)]
pub struct OriginConfig {
    pub scheme: Arc<str>,
    pub allowed_origins: Option<Arc<Vec<String>>>,
}

impl OriginConfig {
    pub fn new(scheme: impl Into<Arc<str>>) -> Self {
        Self {
            scheme: scheme.into(),
            allowed_origins: None,
        }
    }

    pub fn from_allowed_origins<I, S>(origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            scheme: Arc::from("http"),
            allowed_origins: Some(Arc::new(
                origins
                    .into_iter()
                    .map(Into::into)
                    .map(|origin: String| origin.trim().trim_end_matches('/').to_ascii_lowercase())
                    .filter(|origin| !origin.is_empty())
                    .collect(),
            )),
        }
    }

    pub fn local_http() -> Self {
        Self::from_allowed_origins([
            "http://localhost:5173",
            "http://127.0.0.1:5173",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
        ])
    }
}

/// Server-resolved identity.  The operator comes from the request Host and
/// the user comes from the opaque session cookie; no client-supplied identity
/// header is consulted.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub operator: Operator,
    pub user: User,
    pub session: Session,
    pub memberships: Vec<Membership>,
    pub scope: TenantScope,
}

/// IDs attached to every request and echoed in every response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestContext {
    pub request_id: Uuid,
    pub correlation_id: Uuid,
}

impl RequestContext {
    pub fn new(request_id: Uuid, correlation_id: Uuid) -> Self {
        Self {
            request_id,
            correlation_id,
        }
    }
}

pub async fn request_context_middleware(mut request: Request, next: Next) -> Response {
    let request_id = header_uuid(request.headers(), REQUEST_ID_HEADER).unwrap_or_else(Uuid::new_v4);
    let correlation_id =
        header_uuid(request.headers(), CORRELATION_ID_HEADER).unwrap_or(request_id);
    request
        .extensions_mut()
        .insert(RequestContext::new(request_id, correlation_id));

    let mut response = next.run(request).await;
    insert_uuid_header(&mut response, REQUEST_ID_HEADER, request_id);
    insert_uuid_header(&mut response, CORRELATION_ID_HEADER, correlation_id);
    response
}

pub async fn no_store_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

/// Resolve Host -> operator -> opaque session -> user -> membership -> tenant
/// scope.  The tenant selector is deliberately a separate header/query value:
/// it selects among server-loaded memberships but cannot invent a tenant.
pub async fn auth_scope_middleware(
    State(state): State<crate::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let cookie_name = if state.durable_storage() {
        SESSION_COOKIE_NAME
    } else {
        DEV_SESSION_COOKIE_NAME
    };
    auth_scope_with_repository_and_cookie(state.auth_repository(), Some(cookie_name), request, next)
        .await
}

pub async fn auth_scope_from_request(mut request: Request, next: Next) -> Response {
    let Some(state) = request.extensions().get::<crate::AppState>().cloned() else {
        return crate::error::error_response(
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "application state is unavailable",
            ),
            request_context(&request),
        );
    };
    let cookie_name = if state.durable_storage() {
        SESSION_COOKIE_NAME
    } else {
        DEV_SESSION_COOKIE_NAME
    };
    let request_context = request_context(&request);
    let input = match auth_request_input(&request, true, Some(cookie_name)) {
        Ok(input) => input,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    let repository = state.auth_repository();
    let auth = match resolve_auth_context_input(&*repository, input).await {
        Ok(auth) => auth,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    request.extensions_mut().insert(auth.scope.clone());
    request.extensions_mut().insert(auth);
    next.run(request).await
}

pub async fn auth_scope_with_repository(
    repository: SharedAuthRepository,
    request: Request,
    next: Next,
) -> Response {
    auth_scope_with_repository_and_cookie(repository, None, request, next).await
}

pub async fn auth_scope_with_repository_and_cookie(
    repository: SharedAuthRepository,
    cookie_name: Option<&'static str>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_context = request_context(&request);
    let input = match auth_request_input(&request, true, cookie_name) {
        Ok(input) => input,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    let auth = match resolve_auth_context_input(&*repository, input).await {
        Ok(auth) => auth,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    request.extensions_mut().insert(auth.scope.clone());
    request.extensions_mut().insert(auth);
    next.run(request).await
}

pub async fn auth_scope_from_extension(
    Extension(repository): Extension<SharedAuthRepository>,
    request: Request,
    next: Next,
) -> Response {
    auth_scope_with_repository(repository, request, next).await
}

pub async fn auth_scope_from_extensions(
    Extension(repository): Extension<SharedAuthRepository>,
    Extension(cookie): Extension<SessionCookieConfig>,
    request: Request,
    next: Next,
) -> Response {
    auth_scope_with_repository_and_cookie(repository, Some(cookie.0), request, next).await
}

pub async fn auth_scope_from_middleware_state(
    Extension(config): Extension<AuthMiddlewareState>,
    request: Request,
    next: Next,
) -> Response {
    auth_scope_with_repository_and_cookie(
        config.repository,
        Some(config.cookie_name),
        request,
        next,
    )
    .await
}

/// Resolve only the host and session identity for auth/session endpoints.  A
/// tenant selector is not needed to inspect or revoke the current session.
pub async fn session_auth_middleware(
    State(state): State<crate::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let cookie_name = if state.durable_storage() {
        SESSION_COOKIE_NAME
    } else {
        DEV_SESSION_COOKIE_NAME
    };
    session_auth_with_repository_and_cookie(
        state.auth_repository(),
        Some(cookie_name),
        request,
        next,
    )
    .await
}

pub async fn session_auth_from_request(mut request: Request, next: Next) -> Response {
    let Some(state) = request.extensions().get::<crate::AppState>().cloned() else {
        return crate::error::error_response(
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "application state is unavailable",
            ),
            request_context(&request),
        );
    };
    let cookie_name = if state.durable_storage() {
        SESSION_COOKIE_NAME
    } else {
        DEV_SESSION_COOKIE_NAME
    };
    let request_context = request_context(&request);
    let input = match auth_request_input(&request, false, Some(cookie_name)) {
        Ok(input) => input,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    let repository = state.auth_repository();
    let auth = match resolve_auth_context_input(&*repository, input).await {
        Ok(auth) => auth,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    request.extensions_mut().insert(auth);
    next.run(request).await
}

pub async fn session_auth_with_repository(
    repository: SharedAuthRepository,
    request: Request,
    next: Next,
) -> Response {
    session_auth_with_repository_and_cookie(repository, None, request, next).await
}

pub async fn session_auth_with_repository_and_cookie(
    repository: SharedAuthRepository,
    cookie_name: Option<&'static str>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_context = request_context(&request);
    let input = match auth_request_input(&request, false, cookie_name) {
        Ok(input) => input,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    let auth = match resolve_auth_context_input(&*repository, input).await {
        Ok(auth) => auth,
        Err(error) => return crate::error::error_response(error, request_context),
    };
    request.extensions_mut().insert(auth);
    next.run(request).await
}

pub async fn session_auth_from_extension(
    Extension(repository): Extension<SharedAuthRepository>,
    request: Request,
    next: Next,
) -> Response {
    session_auth_with_repository(repository, request, next).await
}

pub async fn session_auth_from_extensions(
    Extension(repository): Extension<SharedAuthRepository>,
    Extension(cookie): Extension<SessionCookieConfig>,
    request: Request,
    next: Next,
) -> Response {
    session_auth_with_repository_and_cookie(repository, Some(cookie.0), request, next).await
}

pub async fn session_auth_from_middleware_state(
    Extension(config): Extension<AuthMiddlewareState>,
    request: Request,
    next: Next,
) -> Response {
    session_auth_with_repository_and_cookie(
        config.repository,
        Some(config.cookie_name),
        request,
        next,
    )
    .await
}

/// Same-origin and session-bound CSRF validation for state-changing protected
/// requests.  This middleware is installed outside the idempotency middleware
/// so rejected cross-site requests never reserve a business idempotency key.
pub async fn csrf_origin_middleware(
    State(state): State<crate::AppState>,
    request: Request,
    next: Next,
) -> Response {
    csrf_origin_with_config(state.origin_config().clone(), request, next).await
}

pub async fn csrf_origin_from_request(request: Request, next: Next) -> Response {
    let origin_config = request
        .extensions()
        .get::<crate::AppState>()
        .map(|state| state.origin_config().clone());
    let Some(origin_config) = origin_config else {
        return crate::error::error_response(
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "application state is unavailable",
            ),
            request_context(&request),
        );
    };
    csrf_origin_with_config(origin_config, request, next).await
}

pub async fn csrf_origin_with_scheme(scheme: Arc<str>, request: Request, next: Next) -> Response {
    csrf_origin_with_config(OriginConfig::new(scheme), request, next).await
}

pub async fn csrf_origin_with_config(
    origin_config: OriginConfig,
    request: Request,
    next: Next,
) -> Response {
    if matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        let request_context = request_context(&request);
        if let Err(error) = validate_origin_with_config(&request, &origin_config) {
            return crate::error::error_response(error, request_context);
        }
        let Some(auth) = request.extensions().get::<AuthContext>() else {
            return crate::error::error_response(
                AppError::unauthorized("authenticated session is required"),
                request_context,
            );
        };
        let Some(token) = request
            .headers()
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
        else {
            return crate::error::error_response(
                AppError::forbidden("missing CSRF token"),
                request_context,
            );
        };
        if token != auth.session.csrf_token() {
            return crate::error::error_response(
                AppError::forbidden("invalid CSRF token"),
                request_context,
            );
        }
    }
    next.run(request).await
}

pub async fn csrf_origin_from_config_extension(
    Extension(origin_config): Extension<OriginConfig>,
    request: Request,
    next: Next,
) -> Response {
    csrf_origin_with_config(origin_config, request, next).await
}

pub async fn csrf_origin_from_middleware_state(
    Extension(config): Extension<AuthMiddlewareState>,
    request: Request,
    next: Next,
) -> Response {
    csrf_origin_with_config(config.origin_config, request, next).await
}

pub fn validate_origin(request: &Request) -> Result<(), AppError> {
    validate_origin_with_scheme(request, request.uri().scheme_str().unwrap_or("http"))
}

pub fn validate_origin_with_scheme(request: &Request, scheme: &str) -> Result<(), AppError> {
    validate_origin_headers(request.headers(), scheme)
}

pub fn validate_origin_with_config(
    request: &Request,
    origin_config: &OriginConfig,
) -> Result<(), AppError> {
    validate_origin_headers_with_config(request.headers(), origin_config)
}

pub fn validate_origin_headers(headers: &HeaderMap, scheme: &str) -> Result<(), AppError> {
    let origin = headers
        .get("origin")
        .ok_or_else(|| {
            AppError::forbidden("Origin header is required for state-changing requests")
        })?
        .to_str()
        .map_err(|_| AppError::forbidden("invalid Origin header"))?;
    if origin == "null" || origin.is_empty() {
        return Err(AppError::forbidden("cross-origin requests are not allowed"));
    }
    let host = host_from_headers(headers)?;
    let expected = format!("{scheme}://{host}");
    if !origin.eq_ignore_ascii_case(&expected) {
        return Err(AppError::forbidden(
            "Origin does not match the request host",
        ));
    }
    Ok(())
}

pub fn validate_origin_headers_with_config(
    headers: &HeaderMap,
    origin_config: &OriginConfig,
) -> Result<(), AppError> {
    if let Some(allowed_origins) = &origin_config.allowed_origins {
        let origin = headers
            .get("origin")
            .ok_or_else(|| {
                AppError::forbidden("Origin header is required for state-changing requests")
            })?
            .to_str()
            .map_err(|_| AppError::forbidden("invalid Origin header"))?
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase();
        if origin.is_empty() || origin == "null" {
            return Err(AppError::forbidden("cross-origin requests are not allowed"));
        }
        if !allowed_origins.iter().any(|allowed| allowed == &origin) {
            return Err(AppError::forbidden(
                "Origin is not in the configured allow-list",
            ));
        }
        return Ok(());
    }
    validate_origin_headers(headers, &origin_config.scheme)
}

pub async fn resolve_auth_context(
    repository: &dyn AuthRepository,
    request: &Request,
    require_tenant: bool,
) -> Result<AuthContext, AppError> {
    let input = auth_request_input(request, require_tenant, None)?;
    resolve_auth_context_input(repository, input).await
}

pub async fn resolve_auth_context_with_cookie(
    repository: &dyn AuthRepository,
    request: &Request,
    require_tenant: bool,
    expected_cookie_name: Option<&str>,
) -> Result<AuthContext, AppError> {
    let input = auth_request_input(request, require_tenant, expected_cookie_name)?;
    resolve_auth_context_input(repository, input).await
}

#[derive(Debug)]
struct AuthRequestInput {
    host: String,
    session_token: String,
    require_tenant: bool,
    tenant_id: Option<TenantId>,
}

fn auth_request_input(
    request: &Request,
    require_tenant: bool,
    expected_cookie_name: Option<&str>,
) -> Result<AuthRequestInput, AppError> {
    let host = request_host(request)?;
    let session_token = request
        .headers()
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| match expected_cookie_name {
            Some(cookie_name) => cookie_value(value, cookie_name),
            None => cookie_value(value, SESSION_COOKIE_NAME)
                .or_else(|| cookie_value(value, DEV_SESSION_COOKIE_NAME)),
        })
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AppError::unauthorized("authenticated session is required"))?
        .to_owned();
    let tenant_id = if require_tenant {
        Some(
            tenant_selector(request)?
                .ok_or_else(|| AppError::invalid_request("tenant selector is required"))?,
        )
    } else {
        None
    };
    Ok(AuthRequestInput {
        host,
        session_token,
        require_tenant,
        tenant_id,
    })
}

async fn resolve_auth_context_input(
    repository: &dyn AuthRepository,
    input: AuthRequestInput,
) -> Result<AuthContext, AppError> {
    let operator = repository
        .operator_for_host(&input.host)
        .await?
        .ok_or_else(|| AppError::unauthorized("request host is not configured"))?;
    let session = repository
        .find_session(operator.id, &input.session_token)
        .await?
        .ok_or_else(|| AppError::unauthorized("authenticated session is required"))?;
    if !session.is_active_at(chrono::Utc::now()) {
        return Err(AppError::unauthorized("session is expired or revoked"));
    }
    let user = repository
        .find_user(operator.id, session.user_id)
        .await?
        .filter(|user| user.active)
        .ok_or_else(|| AppError::unauthorized("user is inactive"))?;
    let memberships = repository.memberships(user.id, operator.id).await?;
    if memberships.is_empty() {
        return Err(AppError::forbidden(
            "user has no membership for this operator",
        ));
    }
    let scope = if input.require_tenant {
        let tenant_id = input
            .tenant_id
            .ok_or_else(|| AppError::invalid_request("tenant selector is required"))?;
        let membership = memberships
            .iter()
            .find(|membership| membership.tenant_id == tenant_id && membership.active)
            .ok_or_else(|| AppError::forbidden("user is not a member of the selected tenant"))?;
        TenantScope::new(membership.operator_id, membership.tenant_id, None)
    } else {
        TenantScope::new(operator.id, memberships[0].tenant_id, None)
    };
    Ok(AuthContext {
        operator,
        user,
        session,
        memberships,
        scope,
    })
}

pub fn request_host(request: &Request) -> Result<String, AppError> {
    host_from_headers(request.headers())
}

pub fn host_from_headers(headers: &HeaderMap) -> Result<String, AppError> {
    headers
        .get("host")
        .ok_or_else(|| AppError::unauthorized("Host header is required"))?
        .to_str()
        .map(|value| value.trim().trim_end_matches('.').to_ascii_lowercase())
        .map_err(|_| AppError::unauthorized("invalid Host header"))
        .and_then(|value| {
            if value.is_empty() {
                Err(AppError::unauthorized("Host header is required"))
            } else {
                Ok(value)
            }
        })
}

fn tenant_selector(request: &Request) -> Result<Option<TenantId>, AppError> {
    if let Some(value) = request.headers().get(TENANT_SELECTOR_HEADER) {
        let value = value
            .to_str()
            .map_err(|_| AppError::invalid_request("invalid tenant selector"))?;
        return value
            .parse()
            .map(Some)
            .map_err(|_| AppError::invalid_request("invalid tenant selector"));
    }
    let Some(query) = request.uri().query() else {
        return Ok(None);
    };
    for part in query.split('&') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if key == "tenant_id" {
            return value
                .parse()
                .map(Some)
                .map_err(|_| AppError::invalid_request("invalid tenant selector"));
        }
    }
    Ok(None)
}

fn cookie_value<'a>(cookies: &'a str, name: &str) -> Option<&'a str> {
    cookies.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Development-only scope extraction.
///
/// Production authentication must resolve the operator and tenant from the
/// server-side identity; these headers are intentionally an explicit local
/// development adapter and must not be treated as user-submitted body data.
pub async fn dev_scope_middleware(mut request: Request, next: Next) -> Response {
    let scope = match scope_from_headers(request.headers()) {
        Ok(scope) => scope,
        Err(error) => return crate::error::error_response(error, request_context(&request)),
    };
    request.extensions_mut().insert(scope);
    next.run(request).await
}

pub fn scope_from_headers(headers: &axum::http::HeaderMap) -> Result<TenantScope, AppError> {
    let operator_id = parse_header::<OperatorId>(headers, OPERATOR_ID_HEADER)?;
    let tenant_id = parse_header::<TenantId>(headers, TENANT_ID_HEADER)?;
    let project_id = optional_header::<ProjectId>(headers, PROJECT_ID_HEADER)?;
    Ok(TenantScope::new(operator_id, tenant_id, project_id))
}

fn parse_header<T>(headers: &axum::http::HeaderMap, name: &str) -> Result<T, AppError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = headers
        .get(name)
        .ok_or_else(|| AppError::invalid_request(format!("missing {name} header")))?
        .to_str()
        .map_err(|_| AppError::invalid_request(format!("invalid {name} header")))?;
    value
        .parse()
        .map_err(|error| AppError::invalid_request(format!("invalid {name} header: {error}")))
}

fn optional_header<T>(headers: &axum::http::HeaderMap, name: &str) -> Result<Option<T>, AppError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let Some(value) = headers.get(name) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::invalid_request(format!("invalid {name} header")))?;
    value
        .parse()
        .map(Some)
        .map_err(|error| AppError::invalid_request(format!("invalid {name} header: {error}")))
}

fn header_uuid(headers: &axum::http::HeaderMap, name: &str) -> Option<Uuid> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

fn insert_uuid_header(response: &mut Response, name: &str, value: Uuid) {
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(&value.to_string()),
    ) {
        response.headers_mut().insert(name, value);
    }
}

fn request_context(request: &Request) -> Option<RequestContext> {
    request.extensions().get::<RequestContext>().copied()
}
