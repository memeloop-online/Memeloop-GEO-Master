//! Project saved publication receipts into connector evidence without sending.
//! Independent of browser availability: all inputs are durable prior outcomes.
use geo_persistence::PgConnectorCapabilityRepository;
use tracing::warn;

pub fn spawn(repository: PgConnectorCapabilityRepository) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let mut after = None;
            loop {
                let candidates = match repository.scan_unprojected_publications(after, 100).await {
                    Ok(candidates) => candidates,
                    Err(error) => {
                        warn!(code = ?error.code, "publication verification scan unavailable");
                        break;
                    }
                };
                let count = candidates.len();
                for (scope, target_id, attempt_id) in candidates {
                    after = Some(attempt_id);
                    if let Err(error) = repository
                        .project_saved_publication_verification(&scope, target_id, attempt_id)
                        .await
                    {
                        // Projection cannot restart or retry an external send.
                        // Continue the page so one damaged record cannot starve it.
                        warn!(code = ?error.code, "publication verification projection deferred");
                    }
                }
                if count < 100 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
    });
}
