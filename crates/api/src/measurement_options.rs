//! Read-only website model discovery; no prompts, sampling, or inferred model IDs.
#[cfg(test)]
#[path = "measurement_options_tests.rs"]
mod tests;

use axum::{
    Json,
    extract::{Extension, Path, State},
};
use geo_domain::{AppError, ChannelStatus, ProjectId, TenantScope};
use uuid::Uuid;

use crate::{ApiError, AppState, RequestContext, api_error, browser_bridge::MeasurementOptions};

pub async fn get_options(
    State(state): State<AppState>,
    Path((project_id, account_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<MeasurementOptions>, ApiError> {
    let result = async {
        let scope = crate::channel_jobs::scope(&state, &tenant, project_id).await?;
        discover(&state, &scope, account_id).await
    }
    .await;
    result
        .map(Json)
        .map_err(|error| api_error(error, context.request_id))
}

pub async fn discover(
    state: &AppState,
    scope: &TenantScope,
    account_id: Uuid,
) -> Result<MeasurementOptions, AppError> {
    let service = state.channel_service();
    let account = service.resolve_available_account(scope, account_id).await?;
    if !account.enabled || account.status != ChannelStatus::Ready {
        return Err(AppError::conflict(
            "channel account is not ready; reconnect the account",
        ));
    }
    if account.platform != "kimi" {
        return Err(AppError::capability_missing(
            "website model discovery is not supported for this channel",
        ));
    }
    let expected = account
        .platform_account_id
        .as_ref()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| AppError::conflict("channel account needs login"))?;
    let browser = service
        .browser
        .as_ref()
        .ok_or_else(|| AppError::capability_missing("browser runner is not configured"))?;
    let session_id = service.resume_available_browser(scope, account_id).await?;
    let result = async {
        let verified = browser.complete(session_id).await?;
        if &verified.identity.platform_account_id != expected {
            return Err(AppError::conflict(
                "channel account identity changed; reconnect the account",
            ));
        }
        browser.measurement_options(session_id).await
    }
    .await;
    // Always discard the temporary inspection context, including on identity
    // failure or unavailable menus. Never save/return its storage state.
    let closed = browser.close(session_id).await;
    match result {
        Ok(options) => {
            closed?;
            Ok(options)
        }
        Err(error) => Err(error),
    }
}
