//! Read-only website model discovery; no prompts, sampling, or inferred model IDs.
#[cfg(test)]
#[path = "measurement_options_tests.rs"]
mod tests;

use axum::{
    Json,
    extract::{Extension, Path, State},
};
use chrono::Utc;
use geo_domain::{
    AppError, CHANNEL_EXECUTION_LEASE, ChannelStatus, ErrorCode, ProjectId, TenantScope,
};
use std::time::Duration;
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
    // Restore, identity, renewal and inspection share a deadline, followed by
    // bounded close. Both fit well inside the account-wide execution lease.
    discover_with_timeouts(
        state,
        scope,
        account_id,
        Duration::from_secs(85),
        Duration::from_secs(10),
    )
    .await
}

async fn discover_with_timeouts(
    state: &AppState,
    scope: &TenantScope,
    account_id: Uuid,
    operation_timeout: Duration,
    close_timeout: Duration,
) -> Result<MeasurementOptions, AppError> {
    let service = state.channel_service();
    let account = service.resolve_available_account(scope, account_id).await?;
    if !account.enabled || account.status != ChannelStatus::Ready {
        return Err(AppError::conflict(
            "channel account is not ready; reconnect the account",
        ));
    }
    if !geo_domain::consumer_web_provider(&account.platform) {
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
    let connectors = browser.capabilities().await?.connectors;
    let matching = connectors
        .iter()
        .filter(|connector| {
            connector.platform == account.platform && connector.placement_slot == "primary"
        })
        .collect::<Vec<_>>();
    if matching.len() != 1
        || matching[0].connector_version.trim().is_empty()
        || !matching[0].model_discovery_supported
    {
        return Err(AppError::capability_missing(
            "website model discovery is not supported by the installed adapter",
        ));
    }
    let reservation_id = Uuid::new_v4();
    let now = Utc::now();
    let operation_deadline = tokio::time::Instant::now() + operation_timeout;
    state
        .channel_job_repository()
        .reserve_account(
            scope,
            account_id,
            reservation_id,
            now,
            now + CHANNEL_EXECUTION_LEASE,
        )
        .await?;
    let session_id = Uuid::new_v4();
    let mut start_confirmed = false;
    let operation = async {
        if tokio::time::Instant::now() >= operation_deadline {
            return Err(discovery_timeout());
        }
        let (_, mut version) = service
            .resume_available_browser_with_renewal_id(scope, account_id, session_id)
            .await?;
        start_confirmed = true;
        if version.platform != account.platform || &version.platform_account_id != expected {
            return Err(AppError::conflict(
                "channel account changed while checking models; reconnect or retry",
            ));
        }
        let verified = browser.complete(session_id).await?;
        if &verified.identity.platform_account_id != expected {
            return Err(AppError::conflict(
                "channel account identity changed; reconnect the account",
            ));
        }
        if !service
            .persist_browser_renewal(scope, &mut version, &verified)
            .await?
        {
            return Err(AppError::conflict(
                "channel account changed while checking models; reconnect or retry",
            ));
        }
        browser.measurement_options(session_id).await
    };
    let result = tokio::time::timeout_at(operation_deadline, operation)
        .await
        .unwrap_or_else(|_| Err(discovery_timeout()));
    // Always discard the temporary inspection context, including on identity
    // failure or unavailable menus. Verified renewal stays encrypted server-side.
    let closed = tokio::time::timeout(close_timeout, browser.close(session_id))
        .await
        .unwrap_or_else(|_| {
            Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "browser inspection cleanup timed out; retry later",
            ))
        });
    // A lost start response can complete after close. Keep that reservation
    // until expiry even if close currently reports absence.
    if start_confirmed && closed.is_ok() {
        let released = state
            .channel_job_repository()
            .release_account(scope, account_id, reservation_id)
            .await;
        if result.is_ok() {
            released?;
        }
    }
    match result {
        Ok(options) => {
            closed?;
            Ok(options)
        }
        Err(error) => Err(error),
    }
}

fn discovery_timeout() -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "website model discovery timed out; retry later",
    )
}

/// Preserve the installed legacy Kimi protocol. Newly registered namespaces
/// remain unavailable until the running adapter explicitly offers measurement.
pub(crate) async fn require_installed_measurement(
    state: &AppState,
    provider: &str,
) -> Result<(), AppError> {
    if provider == "kimi" {
        return Ok(());
    }
    let connectors = crate::connector_capabilities::deployed_versions(state).await;
    if !geo_domain::consumer_web_provider(provider)
        || !crate::browser_bridge::measurement_connector_available(&connectors, provider)
    {
        return Err(AppError::capability_missing(
            "website measurement is not supported by the installed adapter",
        ));
    }
    Ok(())
}
