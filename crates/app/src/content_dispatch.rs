//! Cross-replica content recovery. Claims are taken in the same database
//! path used by HTTP dispatch; scan pages never hold execution locks.
use geo_api::AppState;
use geo_persistence::{PgContentRepository, PgProjectRepository};
use tracing::warn;

pub fn spawn(state: AppState, repository: PgContentRepository, projects: PgProjectRepository) {
    if !state.content_executor_available() {
        return;
    }
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let now = chrono::Utc::now();
            // A model is needed to create documents, but NOT to recover the
            // already closed handoff and prepare distribution coverage.
            if state.content_model_available() {
                let mut after = None;
                loop {
                    match projects
                        .scan_pending_content_cycles_after(after, now, 100)
                        .await
                    {
                        Ok(cycles) => {
                            let count = cycles.len();
                            for cycle in cycles {
                                after = Some(cycle.cycle_id);
                                let lease = match projects
                                    .try_claim_content_bootstrap(
                                        &cycle.scope,
                                        cycle.cycle_id,
                                        chrono::Utc::now(),
                                        chrono::Duration::minutes(15),
                                    )
                                    .await
                                {
                                    Ok(lease) => lease,
                                    Err(error) => {
                                        warn!(code = ?error.code, "content bootstrap claim failed");
                                        continue;
                                    }
                                };
                                let Some(lease) = lease else { continue };
                                let started = state
                                    .content_service()
                                    .start(&cycle.scope, cycle.cycle_id)
                                    .await;
                                let outcome = started.as_ref().map(|_| ()).map_err(|e| e.code);
                                if let Err(error) = projects
                                    .finish_content_bootstrap(&lease, chrono::Utc::now(), outcome)
                                    .await
                                {
                                    warn!(code = ?error.code, "content bootstrap release failed");
                                }
                                match started {
                                    Ok(execution) => {
                                        if let Err(error) = state.dispatch_content_execution(
                                            cycle.scope,
                                            execution.execution_id,
                                        ) {
                                            warn!(code = ?error.code, "content bootstrap dispatch failed");
                                        }
                                    }
                                    Err(error) => {
                                        warn!(code = ?error.code, "content bootstrap deferred");
                                    }
                                }
                            }
                            if count < 100 {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                        Err(error) => {
                            warn!(code = ?error.code, "content bootstrap scan failed");
                            break;
                        }
                    }
                }
            }
            for stage in [false, true] {
                if !stage && !state.content_model_available() {
                    continue;
                }
                let mut after = None;
                loop {
                    let page = if stage {
                        repository.scan_closed_after(after, now, 100).await
                    } else {
                        repository.scan_running_after(after, now, 100).await
                    };
                    match page {
                        Ok(candidates) => {
                            let count = candidates.len();
                            for candidate in candidates {
                                after = Some(candidate.execution_id);
                                // The same fence covers HTTP dispatch and both
                                // native workflow stages, including after close.
                                if let Err(error) = state.dispatch_content_execution(
                                    candidate.scope,
                                    candidate.execution_id,
                                ) {
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
        }
    });
}
