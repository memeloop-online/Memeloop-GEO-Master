//! PostgreSQL discovery of accepted runs that missed their HTTP dispatch.
//! Only queued runs are eligible; running runs need a separate lease and
//! intermediate checkpoint contract before they can be safely recovered.

use geo_api::{AppState, dispatch_queued};
use geo_persistence::PgAgentRepository;
use tracing::warn;

pub fn spawn(state: AppState, scanner: PgAgentRepository) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(5));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let runtime = state.agent_runtime();
            // A missing runtime cannot process accepted work. Leave queued
            // runs intact until a configured replica discovers them.
            if !runtime.capability().await.is_available() {
                continue;
            }
            let repository = state.agent_repository();
            let mut after = None;
            loop {
                match scanner.scan_queued_after(after, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some((candidate.created_at, candidate.run_id));
                            // HTTP and recovery share one atomic begin_run
                            // claim. No scanner-side semaphore or fake input.
                            dispatch_queued(
                                runtime.clone(),
                                repository.clone(),
                                candidate.scope,
                                candidate.run_id,
                            );
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "queued agent run scan failed");
                        break;
                    }
                }
            }
        }
    });
}
