//! Bounded durable discovery includes paused projects; no new measurements.
use std::sync::Arc;

use geo_api::{AppState, dispatch_observation_completion_recovery};
use geo_domain::ObservationCompletionRecoveryJobRepository;
use geo_persistence::{
    PgObservationCompletionRecoveryJobRepository, PgObservationCompletionRecoveryRepository,
};

pub fn spawn(
    state: AppState,
    jobs: PgObservationCompletionRecoveryJobRepository,
    evidence: PgObservationCompletionRecoveryRepository,
) {
    if state.channel_service().browser.is_none() {
        return;
    }
    let jobs = Arc::new(jobs);
    let evidence = Arc::new(evidence);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let as_of = chrono::Utc::now();
            let mut after = None;
            loop {
                match jobs.scan_unqueued(as_of, after, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.capture_id);
                            if let Err(error) =
                                jobs.enqueue(&candidate.scope, candidate.capture_id).await
                            {
                                tracing::warn!(code = ?error.code, "completion recovery enqueue failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        tracing::warn!(code = ?error.code, "completion recovery discovery failed");
                        break;
                    }
                }
            }
            let mut after = None;
            loop {
                match jobs.scan_due(as_of, after, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.job_id);
                            if let Err(error) = dispatch_observation_completion_recovery(
                                state.clone(),
                                jobs.clone(),
                                evidence.clone(),
                                candidate.scope,
                                candidate.job_id,
                            )
                            .await
                            {
                                tracing::warn!(code = ?error.code, "completion recovery dispatch failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        tracing::warn!(code = ?error.code, "completion recovery due scan failed");
                        break;
                    }
                }
            }
        }
    });
}
