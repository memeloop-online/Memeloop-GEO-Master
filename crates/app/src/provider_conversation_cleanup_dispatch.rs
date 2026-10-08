//! Service-only bounded keyset discovery, including paused projects.
use std::sync::Arc;

use geo_api::{AppState, dispatch_provider_conversation_cleanup};
use geo_domain::ProviderConversationCleanupRepository;
use geo_persistence::PgProviderConversationCleanupRepository;
use tracing::warn;

pub fn spawn(state: AppState, repository: PgProviderConversationCleanupRepository) {
    if state.channel_service().browser.is_none() || state.provider_cleanup_callback().is_none() {
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
                match repository.scan_unqueued(as_of, after, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.capture_id);
                            if let Err(error) = repository
                                .enqueue(&candidate.scope, candidate.capture_id)
                                .await
                            {
                                warn!(code = ?error.code, "conversation cleanup enqueue failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "conversation cleanup discovery failed");
                        break;
                    }
                }
            }
            let mut after = None;
            loop {
                match repository.scan_due(as_of, after, 100).await {
                    Ok(candidates) => {
                        let count = candidates.len();
                        for candidate in candidates {
                            after = Some(candidate.cleanup_id);
                            if let Err(error) = dispatch_provider_conversation_cleanup(
                                state.clone(),
                                repository.clone(),
                                candidate.scope,
                                candidate.cleanup_id,
                            )
                            .await
                            {
                                warn!(code = ?error.code, "conversation cleanup dispatch failed");
                            }
                        }
                        if count < 100 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        warn!(code = ?error.code, "conversation cleanup due scan failed");
                        break;
                    }
                }
            }
        }
    });
}
