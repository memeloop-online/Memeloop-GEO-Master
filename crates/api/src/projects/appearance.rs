//! Host-resolved appearance is public before login; edits require an OEM admin.

use axum::{
    Json, Router,
    extract::{Extension, State},
    http::{
        HeaderMap, HeaderValue,
        header::{ETAG, IF_MATCH},
    },
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use geo_domain::{AppError, OperatorAppearance, Role, UpdateOperatorAppearance};

use crate::{
    ApiError, AppState, AuthContext, RequestContext, api_error, csrf_origin_from_request,
    host_from_headers, no_store_middleware, session_auth_from_request,
};

#[utoipa::path(
    get,
    path = "/api/v1/public/appearance",
    responses(
        (status = 200, description = "Appearance for the request Host before login", body = OperatorAppearance),
        (status = 404, description = "Unknown Host")
    )
)]
pub(crate) async fn public(
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    let host = host_from_headers(&headers).map_err(|error| api_error(error, context.request_id))?;
    let operator = state
        .auth_repository()
        .operator_for_host(&host)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .ok_or_else(|| {
            api_error(
                AppError::not_found("request host is not configured"),
                context.request_id,
            )
        })?;
    let appearance = state
        .auth_repository()
        .operator_appearance(operator.id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .ok_or_else(|| {
            api_error(
                AppError::not_found("operator is not configured"),
                context.request_id,
            )
        })?;
    response(appearance, context.request_id)
}

#[utoipa::path(
    get,
    path = "/api/v1/operator/appearance",
    security(("sessionCookie" = [])),
    responses(
        (status = 200, description = "Current operator appearance", body = OperatorAppearance),
        (status = 403, description = "OEM admin role required")
    )
)]
pub(crate) async fn current(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    require_oem_admin(&auth).map_err(|error| api_error(error, context.request_id))?;
    let appearance = state
        .auth_repository()
        .operator_appearance(auth.operator.id)
        .await
        .map_err(|error| api_error(error, context.request_id))?
        .ok_or_else(|| {
            api_error(
                AppError::not_found("operator is not configured"),
                context.request_id,
            )
        })?;
    response(appearance, context.request_id)
}

#[utoipa::path(
    put,
    path = "/api/v1/operator/appearance",
    security(("sessionCookie" = [])),
    request_body = UpdateOperatorAppearance,
    responses(
        (status = 200, description = "Appearance updated", body = OperatorAppearance),
        (status = 409, description = "Revision conflict")
    )
)]
pub(crate) async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<UpdateOperatorAppearance>,
) -> Result<Response, ApiError> {
    require_oem_admin(&auth).map_err(|error| api_error(error, context.request_id))?;
    let expected_revision =
        parse_if_match(&headers).map_err(|error| api_error(error, context.request_id))?;
    let appearance = state
        .auth_repository()
        .update_operator_appearance(auth.operator.id, expected_revision, input)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    response(appearance, context.request_id)
}

fn require_oem_admin(auth: &AuthContext) -> Result<(), AppError> {
    if auth
        .memberships
        .iter()
        .any(|membership| membership.active && membership.role == Role::OemAdmin)
    {
        Ok(())
    } else {
        Err(AppError::forbidden("OEM admin membership required"))
    }
}

fn parse_if_match(headers: &HeaderMap) -> Result<i64, AppError> {
    let value = headers
        .get(IF_MATCH)
        .and_then(|header| header.to_str().ok())
        .ok_or_else(|| AppError::invalid_request("If-Match appearance revision is required"))?;
    let revision = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0 && *value < i64::MAX)
        .ok_or_else(|| AppError::invalid_request("If-Match must be a quoted numeric revision"))?;
    Ok(revision)
}

fn response(appearance: OperatorAppearance, request_id: uuid::Uuid) -> Result<Response, ApiError> {
    let etag = HeaderValue::from_str(&format!("\"{}\"", appearance.revision)).map_err(|_| {
        api_error(
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "appearance revision is invalid",
            ),
            request_id,
        )
    })?;
    let mut response = Json(appearance).into_response();
    response.headers_mut().insert(ETAG, etag);
    Ok(response)
}

pub(crate) fn routes() -> Router<AppState> {
    let operator = Router::new()
        .route("/operator/appearance", get(current).put(update))
        .layer(middleware::from_fn(csrf_origin_from_request))
        .layer(middleware::from_fn(session_auth_from_request));
    Router::new()
        .route("/public/appearance", get(public))
        .merge(operator)
        .layer(middleware::from_fn(no_store_middleware))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_match_must_be_a_single_strong_numeric_etag() {
        for invalid in [
            "2",
            "W/\"2\"",
            "\"0\"",
            "\"2\", \"3\"",
            "\"9223372036854775807\"",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(IF_MATCH, invalid.parse().unwrap());
            assert!(parse_if_match(&headers).is_err(), "{invalid}");
        }
        let mut headers = HeaderMap::new();
        headers.insert(IF_MATCH, "\"3\"".parse().unwrap());
        assert_eq!(parse_if_match(&headers).unwrap(), 3);
    }
}
