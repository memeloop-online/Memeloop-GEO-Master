//! Recoverable pending-target discovery. Attempts and account reservations,
//! not process-local task handles, remain the execution authority.

use geo_api::{AppState, execute_channel_target};
use tracing::warn;

pub fn spawn(state: AppState) {
    if state.channel_service().browser.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(5));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let due_at = chrono::Utc::now();
            let mut after = None;
            loop {
                match state
                    .channel_job_repository()
                    .scan_pending(after, due_at, 100)
                    .await
                {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.target_id);
                            let task_state = state.clone();
                            tokio::spawn(async move {
                                // Multiple processes may discover the same row.
                                // Durable account reservation + target claim
                                // decide which one may send. Deferred work
                                // remains pending and requires no approval.
                                if let Err(error) = execute_channel_target(
                                    &task_state,
                                    &candidate.scope,
                                    candidate.target_id,
                                )
                                .await
                                {
                                    warn!(code = ?error.code, "channel dispatch failed");
                                }
                            });
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "pending channel scan failed");
                        break;
                    }
                }
            }
        }
    });
}
