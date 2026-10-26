//! Session-authenticated, transient report-font generation.

use axum::{
    Json,
    extract::{Extension, State},
    http::header,
};
use geo_domain::{AppError, ErrorCode};
use serde::Deserialize;

use crate::{ApiError, AppState, RequestContext, api_error};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FontSubsetRequest {
    codepoints: Vec<serde_json::Number>,
}

pub(crate) async fn subset(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<FontSubsetRequest>,
) -> Result<([(header::HeaderName, &'static str); 1], Vec<u8>), ApiError> {
    let browser = state.channel_service().browser.as_ref().ok_or_else(|| {
        api_error(
            AppError::new(ErrorCode::DependencyUnavailable, "PDF font unavailable"),
            context.request_id,
        )
    })?;
    let bytes = browser
        .font_subset(request.codepoints)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    Ok(([(header::CONTENT_TYPE, "font/otf")], bytes))
}
