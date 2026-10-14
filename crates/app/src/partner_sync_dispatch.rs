//! Resume durable inbound synchronization; acceptance and authentication remain
//! HTTP responsibilities, while the repository commits each item atomically.
use geo_domain::{PartnerAdvance, PartnerRepository};
use std::{sync::Arc, time::Duration};
use tracing::warn;

const PAGE_SIZE: u32 = 100;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub fn spawn(repository: Arc<dyn PartnerRepository>) {
    tokio::spawn(async move {
        loop {
            if advance_batch(repository.as_ref()).await {
                // Re-read durable due ordering after each batch. Committing an
                // item moves its operation behind other due work, so neither a
                // large operation nor a fixed per-poll quota limits progress.
                tokio::task::yield_now().await;
            } else {
                // Includes locked/not-due rows and scan failures: never spin
                // when another replica owns all candidates.
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    });
}

/// True means an item committed and the runner should immediately drain again.
/// False asks the runner to sleep; retry timing itself lives in the repository.
async fn advance_batch(repository: &dyn PartnerRepository) -> bool {
    let candidates = match repository.candidates(PAGE_SIZE).await {
        Ok(candidates) => candidates,
        Err(error) => {
            warn!(code = ?error.code, "partner synchronization scan failed");
            return false;
        }
    };
    let mut progressed = false;
    for work in candidates {
        match repository.advance_one(&work).await {
            Ok(PartnerAdvance::ItemCommitted { .. }) => progressed = true,
            Ok(PartnerAdvance::Completed | PartnerAdvance::Deferred) => {}
            Err(error) => {
                // Log only the typed code, never source content, identities,
                // credentials, database diagnostics or request payloads.
                warn!(code = ?error.code, "partner synchronization item deferred");
            }
        }
    }
    progressed
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use geo_domain::{
        AppError, OperatorId, PartnerDisposition, PartnerPrincipal, PartnerSyncPage,
        PartnerSyncReceipt, PartnerSyncRequest, PartnerSyncWork, ProjectId, TenantId, TenantScope,
    };
    use std::collections::VecDeque;
    use tokio::sync::Mutex;
    use uuid::Uuid;

    enum Step {
        Commit(PartnerDisposition),
        Deferred,
        Completed,
        Error,
    }

    struct Pending {
        work: PartnerSyncWork,
        steps: VecDeque<Step>,
    }

    #[derive(Default)]
    struct Stored {
        pending: VecDeque<Pending>,
        calls: Vec<Uuid>,
        results: Vec<PartnerDisposition>,
        scan_error: bool,
    }

    #[derive(Default)]
    struct Repository {
        stored: Mutex<Stored>,
    }

    #[async_trait]
    impl PartnerRepository for Repository {
        async fn authenticate(
            &self,
            _: Uuid,
            _: Uuid,
            _: &str,
        ) -> Result<PartnerPrincipal, AppError> {
            panic!("recovery must not authenticate an HTTP request")
        }

        async fn accept(
            &self,
            _: &PartnerPrincipal,
            _: &str,
            _: PartnerSyncRequest,
        ) -> Result<PartnerSyncReceipt, AppError> {
            panic!("recovery must not accept another request")
        }

        async fn results(
            &self,
            _: &PartnerPrincipal,
            _: Uuid,
            _: Option<u32>,
            _: u32,
        ) -> Result<PartnerSyncPage, AppError> {
            panic!("recovery uses persisted candidates, not HTTP results")
        }

        async fn candidates(&self, limit: u32) -> Result<Vec<PartnerSyncWork>, AppError> {
            assert!((1..=100).contains(&limit));
            let stored = self.stored.lock().await;
            if stored.scan_error {
                return Err(AppError::conflict("synthetic scan failure"));
            }
            Ok(stored
                .pending
                .iter()
                .take(limit as usize)
                .map(|pending| pending.work.clone())
                .collect())
        }

        async fn advance_one(&self, work: &PartnerSyncWork) -> Result<PartnerAdvance, AppError> {
            let mut stored = self.stored.lock().await;
            stored.calls.push(work.operation_id);
            let position = stored
                .pending
                .iter()
                .position(|pending| pending.work.operation_id == work.operation_id)
                .unwrap();
            let mut pending = stored.pending.remove(position).unwrap();
            assert_eq!(pending.work.scope, work.scope);
            assert_eq!(pending.work.connection_id, work.connection_id);
            let outcome = match pending.steps.pop_front().unwrap() {
                Step::Commit(disposition) => {
                    stored.results.push(disposition);
                    Ok(PartnerAdvance::ItemCommitted {
                        index: 0,
                        operation_completed: pending.steps.is_empty(),
                    })
                }
                Step::Deferred => {
                    pending.steps.push_front(Step::Deferred);
                    Ok(PartnerAdvance::Deferred)
                }
                Step::Completed => Ok(PartnerAdvance::Completed),
                Step::Error => Err(AppError::conflict("synthetic item failure")),
            };
            if !pending.steps.is_empty() {
                // Model the persistent scheduler moving a committed operation
                // behind the remaining due work.
                stored.pending.push_back(pending);
            }
            outcome
        }
    }

    fn repository(operations: Vec<Vec<Step>>) -> Repository {
        Repository {
            stored: Mutex::new(Stored {
                pending: operations
                    .into_iter()
                    .map(|steps| Pending {
                        work: PartnerSyncWork {
                            scope: TenantScope::new(
                                OperatorId::new(Uuid::new_v4()),
                                TenantId::new(Uuid::new_v4()),
                                Some(ProjectId::new(Uuid::new_v4())),
                            ),
                            connection_id: Uuid::new_v4(),
                            operation_id: Uuid::new_v4(),
                        },
                        steps: steps.into(),
                    })
                    .collect(),
                ..Stored::default()
            }),
        }
    }

    #[tokio::test]
    async fn restart_resumes_existing_pending_without_accepting_or_authenticating() {
        let repository = repository(vec![vec![
            Step::Commit(PartnerDisposition::Accepted),
            Step::Commit(PartnerDisposition::Unchanged),
        ]]);
        assert!(advance_batch(&repository).await);
        // No runner-local cursor or request state survives a restart.
        assert!(advance_batch(&repository).await);
        assert!(!advance_batch(&repository).await);
        let stored = repository.stored.lock().await;
        assert_eq!(stored.calls.len(), 2);
        assert_eq!(stored.calls[0], stored.calls[1]);
        assert_eq!(
            stored.results,
            vec![PartnerDisposition::Accepted, PartnerDisposition::Unchanged]
        );
    }

    #[tokio::test]
    async fn rejection_and_repository_error_do_not_stop_other_candidates() {
        let repository = repository(vec![
            vec![
                Step::Commit(PartnerDisposition::Rejected),
                Step::Commit(PartnerDisposition::Accepted),
            ],
            vec![Step::Error],
            vec![Step::Commit(PartnerDisposition::Accepted)],
        ]);
        assert!(advance_batch(&repository).await);
        assert_eq!(repository.stored.lock().await.calls.len(), 3);
        assert!(advance_batch(&repository).await);
        assert!(!advance_batch(&repository).await);
        assert_eq!(
            repository.stored.lock().await.results,
            vec![
                PartnerDisposition::Rejected,
                PartnerDisposition::Accepted,
                PartnerDisposition::Accepted,
            ]
        );
    }

    #[tokio::test]
    async fn idle_deferred_completed_and_failed_scans_request_sleep() {
        assert!(!advance_batch(&Repository::default()).await);
        let deferred = repository(vec![vec![Step::Deferred]]);
        assert!(!advance_batch(&deferred).await);
        assert!(!advance_batch(&deferred).await);
        assert!(!advance_batch(&repository(vec![vec![Step::Completed]])).await);
        assert!(!advance_batch(&repository(vec![vec![Step::Error]])).await);
        let failing = Repository::default();
        failing.stored.lock().await.scan_error = true;
        assert!(!advance_batch(&failing).await);
        assert!(failing.stored.lock().await.calls.is_empty());
    }

    #[tokio::test]
    async fn progress_drains_beyond_one_batch_without_a_poll_quota() {
        let repository = repository(
            (0..PAGE_SIZE + 1)
                .map(|_| vec![Step::Commit(PartnerDisposition::Accepted)])
                .collect(),
        );
        assert!(advance_batch(&repository).await);
        assert_eq!(
            repository.stored.lock().await.calls.len(),
            PAGE_SIZE as usize
        );
        assert!(advance_batch(&repository).await);
        assert!(!advance_batch(&repository).await);
        assert_eq!(
            repository.stored.lock().await.results.len(),
            PAGE_SIZE as usize + 1
        );
    }
}
