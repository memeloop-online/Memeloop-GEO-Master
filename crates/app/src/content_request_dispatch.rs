//! Recover accepted single-article requests through the same atomic
//! materialization used by HTTP. Sending remains owned by the channel outbox.
use geo_api::AppState;
use geo_domain::{AppError, ContentDistributionRequestRepository};
use std::time::Duration;
use tracing::warn;
use uuid::Uuid;

const PAGE_SIZE: usize = 100;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub fn spawn(state: AppState) {
    let repository = state.content_distribution_request_repository();
    tokio::spawn(async move {
        let mut after = None;
        loop {
            match materialize_page(repository.as_ref(), after).await {
                Ok(Some(next)) => {
                    after = Some(next);
                    // Drain subsequent pages without imposing a fixed
                    // requests-per-poll throughput limit.
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(None) => after = None,
                Err(error) => {
                    // No account, request body, provider error, or private
                    // endpoint is included in operational diagnostics.
                    warn!(code = ?error.code, "content request scan failed");
                }
            }
            // Retry incomplete rows after a full pass, or a database error.
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

async fn materialize_page(
    repository: &dyn ContentDistributionRequestRepository,
    after: Option<Uuid>,
) -> Result<Option<Uuid>, AppError> {
    let requests = repository.list_unlinked(after, PAGE_SIZE).await?;
    let next = if requests.len() == PAGE_SIZE {
        requests.last().map(|request| request.request_id)
    } else {
        None
    };
    for request in requests {
        // Recheck scoped, frozen dependencies inside the repository
        // transaction. Never construct a new request or change its key.
        if let Err(error) = repository
            .materialize(&request.scope, request.request_id)
            .await
        {
            warn!(code = ?error.code, "content request materialization deferred");
        }
    }
    // Advance even when a row is temporarily ineligible. Otherwise one bad
    // page could prevent every later accepted request from being recovered.
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use geo_domain::{
        AcceptContentDistributionRequest, ContentDistributionRequest, OperatorId, ProjectId,
        TenantId, TenantScope,
    };
    use std::collections::HashSet;
    use tokio::sync::Mutex;

    struct Repository {
        requests: Vec<ContentDistributionRequest>,
        calls: Mutex<Vec<(TenantScope, Uuid)>>,
        linked: Mutex<HashSet<Uuid>>,
        failed: Uuid,
    }

    #[async_trait]
    impl ContentDistributionRequestRepository for Repository {
        async fn get_by_idempotency_key(
            &self,
            _: &TenantScope,
            _: &str,
        ) -> Result<Option<ContentDistributionRequest>, AppError> {
            panic!("recovery must not look up an HTTP idempotency key")
        }

        async fn accept(
            &self,
            _: &TenantScope,
            _: AcceptContentDistributionRequest,
        ) -> Result<ContentDistributionRequest, AppError> {
            panic!("recovery must not accept another request")
        }

        async fn get(
            &self,
            _: &TenantScope,
            _: Uuid,
        ) -> Result<ContentDistributionRequest, AppError> {
            panic!("the scan already carries the persisted request")
        }

        async fn link_intent(
            &self,
            _: &TenantScope,
            _: Uuid,
            _: Uuid,
        ) -> Result<ContentDistributionRequest, AppError> {
            panic!("only atomic materialization may link an intent")
        }

        async fn list_unlinked(
            &self,
            after: Option<Uuid>,
            limit: usize,
        ) -> Result<Vec<ContentDistributionRequest>, AppError> {
            let linked = self.linked.lock().await;
            Ok(self
                .requests
                .iter()
                .filter(|request| {
                    after.is_none_or(|id| request.request_id > id)
                        && !linked.contains(&request.request_id)
                })
                .take(limit)
                .cloned()
                .collect())
        }

        async fn materialize(
            &self,
            scope: &TenantScope,
            id: Uuid,
        ) -> Result<ContentDistributionRequest, AppError> {
            self.calls.lock().await.push((scope.clone(), id));
            if id == self.failed {
                return Err(AppError::conflict("fixture source unavailable"));
            }
            let mut request = self
                .requests
                .iter()
                .find(|request| request.request_id == id && &request.scope == scope)
                .expect("the persisted record determines the scope")
                .clone();
            request.publication_intent_id = Some(Uuid::new_v4());
            self.linked.lock().await.insert(id);
            Ok(request)
        }
    }

    fn repository(count: usize) -> Repository {
        Repository {
            requests: (1..=count)
                .map(|number| ContentDistributionRequest {
                    request_id: Uuid::from_u128(number as u128),
                    scope: TenantScope::new(
                        OperatorId::new(Uuid::new_v4()),
                        TenantId::new(Uuid::new_v4()),
                        Some(ProjectId::new(Uuid::new_v4())),
                    ),
                    schema_version: 1,
                    content_revision_id: Uuid::new_v4(),
                    content_asset_id: Uuid::new_v4(),
                    platform_id: "fixture".into(),
                    placement_slot: "primary".into(),
                    account_id: Uuid::new_v4(),
                    account_owner_kind: "customer".into(),
                    format: "markdown.v1".into(),
                    idempotency_key_hash: "fixture".into(),
                    request_hash: "fixture".into(),
                    publication_intent_id: None,
                    materialization_deferral: None,
                    created_at: chrono::Utc::now(),
                })
                .collect(),
            calls: Mutex::default(),
            linked: Mutex::default(),
            failed: Uuid::from_u128(1),
        }
    }

    #[tokio::test]
    async fn a_failed_request_does_not_starve_later_pages_or_change_scope() {
        let repository = repository(PAGE_SIZE + 1);
        let cursor = materialize_page(&repository, None).await.unwrap();
        assert_eq!(cursor, Some(Uuid::from_u128(PAGE_SIZE as u128)));
        assert_eq!(repository.calls.lock().await.len(), PAGE_SIZE);
        assert_eq!(materialize_page(&repository, cursor).await.unwrap(), None);
        let calls = repository.calls.lock().await;
        assert_eq!(calls.len(), PAGE_SIZE + 1);
        for ((scope, id), request) in calls.iter().zip(&repository.requests) {
            assert_eq!(scope, &request.scope);
            assert_eq!(*id, request.request_id);
        }
    }

    #[tokio::test]
    async fn restarting_scan_recovers_only_still_unlinked_requests() {
        let repository = repository(2);
        assert_eq!(materialize_page(&repository, None).await.unwrap(), None);
        repository.calls.lock().await.clear();
        // Restart drops only the scan cursor, never persisted requests/links.
        assert_eq!(materialize_page(&repository, None).await.unwrap(), None);
        assert_eq!(
            repository.calls.lock().await.as_slice(),
            &[(repository.requests[0].scope.clone(), repository.failed)]
        );
    }

    #[tokio::test]
    async fn empty_scan_creates_no_requests_or_publication_work() {
        let repository = repository(0);
        assert_eq!(materialize_page(&repository, None).await.unwrap(), None);
        assert!(repository.calls.lock().await.is_empty());
    }
}
