//! Recover only persisted browser operations; never launch replacement sessions.
use geo_api::{AppState, reconcile_browser_operations};

pub fn spawn(state: AppState) {
    if state.channel_service().browser.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Err(error) = reconcile_browser_operations(state.clone()).await {
                tracing::warn!(code = ?error.code, "browser ownership recovery failed");
            }
        }
    });
}
