//! Service-only recovery of persisted browser ownership. Never starts a browser
//! or guesses an old operation's generation from the currently reachable runner.
use std::time::Duration;

use chrono::Utc;
use futures_util::future::join_all;
use geo_domain::{AppError, BrowserOperationState};

use crate::AppState;

#[derive(Default)]
pub struct BrowserReconciliationResult {
    pub examined: usize,
    pub disposed: usize,
    pub pending: usize,
}

pub async fn reconcile_browser_operations(
    state: AppState,
) -> Result<BrowserReconciliationResult, AppError> {
    if state.channel_service().browser.is_none() {
        return Ok(BrowserReconciliationResult::default());
    }
    let repository = state.channel_job_repository();
    let due = repository
        .list_due_browser_operations(Utc::now(), 100)
        .await?;
    // This is a bounded recovery page, not a product execution concurrency cap.
    // Each operation already has its own durable account gate. An unavailable
    // runner cannot serialize a whole page into hours of timeout waiting.
    let outcomes = join_all(due.into_iter().map(|operation| {
        let state = state.clone();
        let repository = repository.clone();
        async move {
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                state.channel_service().dispose_owned_operation(&operation),
            )
            .await;
            if matches!(result, Ok(Ok(()))) {
                return true;
            }
            // Another reconciler or the original caller may have completed
            // disposal while this request timed out. Read the same owner only.
            if let Ok(Some(current)) = repository
                .get_browser_operation(&operation.scope, operation.operation_id)
                .await
                && current.state == BrowserOperationState::Disposed
            {
                return true;
            }
            let now = Utc::now();
            if let Err(error) = repository
                .defer_browser_reconciliation(
                    &operation.scope,
                    operation.operation_id,
                    now + chrono::Duration::seconds(30),
                    now,
                )
                .await
            {
                tracing::warn!(code = ?error.code, "browser recovery scheduling failed");
            }
            false
        }
    }))
    .await;
    let disposed = outcomes.iter().filter(|completed| **completed).count();
    Ok(BrowserReconciliationResult {
        examined: outcomes.len(),
        disposed,
        pending: outcomes.len() - disposed,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Json, Router, extract::Request, http::StatusCode, routing::post};
    use chrono::Duration;
    use geo_domain::{
        BeginBrowserOperation, BrowserOperation, BrowserOperationPurpose,
        BrowserOperationRepository, BrowserOperationScope, ChannelJobRepository, ChannelSecret,
        MemoryChannelJobRepository, TenantScope,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::{browser_bridge::BrowserBridge, channels::ChannelService};

    async fn operation(
        repository: &MemoryChannelJobRepository,
        scope: &BrowserOperationScope,
        started: bool,
    ) -> BrowserOperation {
        let at = Utc::now() - Duration::minutes(2);
        let operation = repository
            .begin_browser_operation(
                scope,
                &BeginBrowserOperation {
                    network_selection: None,
                    operation_id: Uuid::new_v4(),
                    session_id: Uuid::new_v4(),
                    account_id: Uuid::new_v4(),
                    purpose: BrowserOperationPurpose::Login,
                    runner_generation: Uuid::new_v4(),
                    effective_network: ChannelSecret::new(vec![1, 2, 3]),
                    reservation_id: None,
                    expires_at: at + Duration::minutes(1),
                },
                at,
            )
            .await
            .unwrap();
        if started {
            repository
                .mark_browser_start_requested(scope, operation.operation_id, at)
                .await
                .unwrap()
        } else {
            operation
        }
    }

    #[tokio::test]
    async fn recovery_releases_unstarted_and_exactly_disposed_but_retains_unknown() {
        let repository = Arc::new(MemoryChannelJobRepository::default());
        let scope = BrowserOperationScope {
            operator_id: Uuid::new_v4().into(),
            tenant_id: None,
            project_id: None,
            owner_id: Uuid::new_v4(),
        };
        let prepared = operation(&repository, &scope, false).await;
        let started = operation(&repository, &scope, true).await;
        let unknown = operation(&repository, &scope, true).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let successful = calls.clone();
        let unsuccessful = calls.clone();
        let session = started.session_id;
        let generation = started.runner_generation;
        let app = Router::new()
            .route(
                &format!("/v1/sessions/{session}/dispose"),
                post(move |request: Request| {
                    let calls = successful.clone();
                    async move {
                        assert_eq!(request.headers()["authorization"], "Bearer fixture-token");
                        let body = axum::body::to_bytes(request.into_body(), 1024)
                            .await
                            .unwrap();
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                            json!({"expected_generation": generation})
                        );
                        calls.fetch_add(1, Ordering::SeqCst);
                        Json(json!({
                            "session_id": session,
                            "generation": generation,
                            "state": "disposed"
                        }))
                    }
                }),
            )
            .route(
                &format!("/v1/sessions/{}/dispose", unknown.session_id),
                post(move || {
                    let calls = unsuccessful.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NOT_FOUND
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let state = AppState::development()
            .with_channel_job_repository(repository.clone())
            .with_channel_service(ChannelService::development().with_browser(
                BrowserBridge::new(format!("http://{address}"), "fixture-token".into()).unwrap(),
            ));
        let result = reconcile_browser_operations(state.clone()).await.unwrap();
        assert_eq!(
            (result.examined, result.disposed, result.pending),
            (3, 2, 1)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        for operation in [&prepared, &started] {
            assert_eq!(
                repository
                    .get_browser_operation(&scope, operation.operation_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                BrowserOperationState::Disposed
            );
        }
        let remaining = repository
            .get_browser_operation(&scope, unknown.operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remaining.state, BrowserOperationState::Closing);
        assert!(remaining.next_check_at > Utc::now());
        let now = Utc::now();
        let project = TenantScope::new(
            scope.operator_id,
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        assert!(
            repository
                .reserve_account(
                    &project,
                    unknown.account_id,
                    Uuid::new_v4(),
                    now,
                    now + Duration::minutes(1),
                )
                .await
                .is_err()
        );
        let again = reconcile_browser_operations(state).await.unwrap();
        assert_eq!(again.examined, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
