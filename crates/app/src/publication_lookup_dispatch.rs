//! Recover original ambiguous sends through independent, read-only lookup jobs.
//! Discovery, claim and observation persistence survive process restarts.

use std::sync::Arc;

use geo_api::{AppState, dispatch_publication_lookup};
use geo_domain::PublicationLookupRepository;
use geo_persistence::PgPublicationLookupRepository;
use tracing::warn;

pub fn spawn(state: AppState, repository: PgPublicationLookupRepository) {
    if state.channel_service().browser.is_none() {
        return;
    }
    let repository = Arc::new(repository);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let as_of = chrono::Utc::now();
            let mut after = None;
            loop {
                match repository.scan_unresolved(after, as_of, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.attempt_id);
                            if let Err(error) = repository
                                .enqueue(
                                    &candidate.scope,
                                    candidate.target_id,
                                    candidate.attempt_id,
                                    as_of,
                                )
                                .await
                            {
                                warn!(code = ?error.code, "publication lookup enqueue failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "ambiguous publication scan failed");
                        break;
                    }
                }
            }
            let mut after = None;
            loop {
                match repository.scan_due(after, as_of, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.attempt_id);
                            let lookup: Arc<dyn PublicationLookupRepository> = repository.clone();
                            // Await the durable claim, not the remote query. Only
                            // a successful claim creates an execution task.
                            if let Err(error) = dispatch_publication_lookup(
                                state.clone(),
                                lookup,
                                candidate.scope,
                                candidate.attempt_id,
                            )
                            .await
                            {
                                warn!(code = ?error.code, "publication lookup dispatch failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "publication lookup due scan failed");
                        break;
                    }
                }
            }
        }
    });
}
