use axum::{
    extract::Request,
    http::{HeaderValue, header::HeaderName},
    middleware::Next,
    response::Response,
};
use geo_domain::{AppError, OperatorId, ProjectId, TenantId, TenantScope};
use uuid::Uuid;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
pub const CORRELATION_ID_HEADER: &str = "x-correlation-id";
pub const OPERATOR_ID_HEADER: &str = "x-operator-id";
pub const TENANT_ID_HEADER: &str = "x-tenant-id";
pub const PROJECT_ID_HEADER: &str = "x-project-id";

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
