use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use geo_domain::{AppError, ErrorCode};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub error: AppError,
    pub request_id: Uuid,
}

impl ApiError {
    pub fn new(error: AppError, request_id: Uuid) -> Self {
        Self { error, request_id }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    pub request_id: Uuid,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(
            self.error,
            Some(crate::context::RequestContext {
                request_id: self.request_id,
                correlation_id: self.request_id,
            }),
        )
    }
}

pub fn error_response(
    error: AppError,
    request_context: Option<crate::context::RequestContext>,
) -> Response {
    let request_id = request_context
        .map(|context| context.request_id)
        .unwrap_or_else(Uuid::new_v4);
    let status = StatusCode::from_u16(error.code.default_status())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        Json(ErrorResponse {
            field: error
                .details
                .as_ref()
                .and_then(|details| details.get("field"))
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
            retryable: matches!(
                error.code,
                ErrorCode::NotReady
                    | ErrorCode::CapabilityMissing
                    | ErrorCode::DependencyUnavailable
            ),
            code: error.code,
            message: error.message,
            details: error.details,
            request_id,
        }),
    )
        .into_response()
}

pub fn api_error(error: AppError, request_id: Uuid) -> ApiError {
    ApiError::new(error, request_id)
}
