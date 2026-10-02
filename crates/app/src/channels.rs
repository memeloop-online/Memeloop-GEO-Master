//! Deployment-only account execution settings. No secrets are sent to clients.

use std::{env, error::Error};

use geo_api::{AppState, BrowserBridge, ChannelService};

pub fn configure(state: AppState) -> Result<AppState, Box<dyn Error>> {
    let runner_url = env::var("GEO_BROWSER_RUNNER_URL").ok();
    let runner_token = env::var("GEO_BROWSER_RUNNER_TOKEN").ok();
    let browser = match (runner_url, runner_token) {
        (None, None) => None,
        (Some(url), Some(token)) => Some(BrowserBridge::new(url, token)?),
        _ => return Err("browser runner URL and token must be configured together".into()),
    };
    let current = state.channel_service();
    let mut service = if let Ok(key) = env::var("GEO_CHANNEL_SECRET_KEY") {
        ChannelService::persistent(current.repository.clone(), &key, browser.clone())?
    } else {
        // An unconfigured persistent vault does not block unrelated app work.
        // Its credential operations remain unavailable, with no plaintext fallback.
        current.clone()
    };
    if let Some(browser) = browser {
        service = service.with_browser(browser);
    }
    if let Ok(tenant_id) = env::var("GEO_OPERATOR_POOL_TENANT_ID") {
        service = service.with_operator_pool_tenant_id(tenant_id.parse()?);
    }
    Ok(state.with_channel_service(service))
}
