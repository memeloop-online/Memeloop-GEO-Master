//! Cross-replica content recovery. Claims are taken in the same database
//! path used by HTTP dispatch; scan pages never hold execution locks.
use geo_api::AppState;
use geo_persistence::PgContentRepository;
use tracing::warn;

pub fn spawn(state: AppState, repository: PgContentRepository) {
    if !state.content_executor_available() || !state.content_model_available() {
        return;
    }
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let mut after = None;
            let now = chrono::Utc::now();
            loop {
                match repository.scan_running_after(after, now, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.execution_id);
                            // Shared dispatch claim fences parallel scanners
                            // and an already running HTTP-dispatched workflow.
                            if let Err(error) = state
                                .dispatch_content_execution(candidate.scope, candidate.execution_id)
                            {
                                warn!(code = ?error.code, "content recovery dispatch failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "content recovery scan failed");
                        break;
                    }
                }
            }
        }
    });
}
