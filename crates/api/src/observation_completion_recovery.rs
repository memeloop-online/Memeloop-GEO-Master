//! Read-only recovery of the exact originally owned conversation.
use std::{sync::Arc, time::Duration};

use chrono::Utc;
use geo_domain::{
    AppError, BrowserOperationPurpose, ObservationCaptureSnapshot,
    ObservationCompletionRecoveryClaim, ObservationCompletionRecoveryInput,
    ObservationCompletionRecoveryJobOutcome as Outcome, ObservationCompletionRecoveryJobRepository,
    ObservationCompletionRecoveryRepository, ObservationProviderIdentity, TenantScope,
};
use uuid::Uuid;

use crate::{
    AppState,
    browser_bridge::{
        CompletionRecoveryCode, CompletionRecoveryDiagnostic, CompletionRecoveryIdentity,
        CompletionRecoveryRequest, CompletionRecoveryResult, CompletionRecoveryStage,
    },
};

/// Claim before detaching so caller cancellation cannot dispatch a second read.
pub async fn dispatch_observation_completion_recovery(
    state: AppState,
    jobs: Arc<dyn ObservationCompletionRecoveryJobRepository>,
    evidence: Arc<dyn ObservationCompletionRecoveryRepository>,
    scope: TenantScope,
    job_id: Uuid,
) -> Result<bool, AppError> {
    if state.channel_service().browser.is_none() {
        return Ok(false);
    }
    let Some(claim) = jobs.claim(&scope, job_id).await? else {
        return Ok(false);
    };
    tokio::spawn(async move {
        let outcome = recover_once(&state, evidence.as_ref(), &scope, &claim).await;
        if let Err(error) = jobs
            .finish(&scope, claim.job_id, claim.lease_id, outcome)
            .await
        {
            tracing::warn!(code = ?error.code, "completion recovery finish failed");
        }
    });
    Ok(true)
}

async fn recover_once(
    state: &AppState,
    evidence: &dyn ObservationCompletionRecoveryRepository,
    scope: &TenantScope,
    claim: &ObservationCompletionRecoveryClaim,
) -> Outcome {
    let Some(bridge) = state.channel_service().browser.as_ref() else {
        return Outcome::Retry;
    };
    if Utc::now() + chrono::Duration::seconds(110) >= claim.lease_until {
        return Outcome::Retry;
    }
    let original = &claim.original;
    let binding = match original.recovery_request_binding(scope) {
        Ok(binding) if binding == claim.request_binding => binding,
        _ => return Outcome::Retained,
    };
    let account_id = original.input.account_id;
    let original_session = original.input.runner_session_id;
    match state
        .channel_job_repository()
        .get_browser_operation(
            &crate::browser_operations::execution_scope(scope, original_session),
            original_session,
        )
        .await
    {
        Ok(Some(operation))
            if operation.account_id == account_id
                && operation.session_id == original_session
                && operation.purpose == BrowserOperationPurpose::Measure => {}
        Ok(_) => return Outcome::Retained,
        Err(_) => return Outcome::Retry,
    }
    let reservation_id = Uuid::new_v4();
    if state
        .channel_job_repository()
        .reserve_account(
            scope,
            account_id,
            reservation_id,
            Utc::now(),
            claim.lease_until,
        )
        .await
        .is_err()
    {
        return Outcome::Retry;
    }
    if Utc::now() + chrono::Duration::seconds(110) >= claim.lease_until {
        let _ = state
            .channel_job_repository()
            .release_account(scope, account_id, reservation_id)
            .await;
        return Outcome::Retry;
    }
    let session_id = Uuid::new_v4();
    let operation = async {
        state
            .channel_service()
            .resume_completion_recovery_browser_reserved(
                scope,
                account_id,
                &ObservationProviderIdentity {
                    provider: binding.provider.clone(),
                    platform_account_id: binding.platform_account_id.clone(),
                },
                session_id,
                reservation_id,
                original_session,
            )
            .await?;
        let verified = bridge.complete(session_id).await?;
        if verified.identity.platform_account_id != binding.platform_account_id {
            return Ok::<_, AppError>(Outcome::NeedsLogin);
        }
        if Utc::now() + chrono::Duration::seconds(75) >= claim.lease_until {
            return Ok(Outcome::Retry);
        }
        let request = CompletionRecoveryRequest {
            execution_id: claim.lease_id,
            expected_identity: CompletionRecoveryIdentity {
                provider: binding.provider.clone(),
                platform_account_id: binding.platform_account_id.clone(),
            },
            external_conversation_id: binding.external_conversation_id.clone(),
            request_message_id: binding.request_message_id.clone(),
            response_message_id: binding.response_message_id.clone(),
        };
        match bridge
            .recover_conversation_completion(session_id, &request)
            .await?
        {
            CompletionRecoveryResult::Recovered {
                execution_id,
                external_conversation_id,
                recovery_identity,
                observed_at,
                history_json,
                history_sha256,
            } => {
                if execution_id != claim.lease_id
                    || external_conversation_id != binding.external_conversation_id
                    || Utc::now() + chrono::Duration::seconds(10) >= claim.lease_until
                {
                    return Ok(Outcome::Retry);
                }
                let ObservationCaptureSnapshot::Source { source_sha256, .. } =
                    &original.input.snapshot
                else {
                    return Ok(Outcome::Retained);
                };
                let input = ObservationCompletionRecoveryInput {
                    recovery_id: claim.lease_id,
                    scope: scope.clone(),
                    source_capture_id: original.input.capture_id,
                    source_sha256: source_sha256.clone(),
                    source_receipt_sha256: original.receipt.digest_sha256.clone(),
                    recovery_execution_id: execution_id,
                    runner_session_id: session_id,
                    observed_at,
                    recovery_identity: ObservationProviderIdentity {
                        provider: recovery_identity.provider,
                        platform_account_id: recovery_identity.platform_account_id,
                    },
                    history_json,
                    history_sha256,
                };
                if input.validate(scope, original).is_err() {
                    return Ok(Outcome::Retained);
                }
                evidence.save(scope, input).await?;
                // A durable read-back, not a transport success, settles recovery.
                let saved = evidence.get(scope, claim.lease_id).await?;
                match saved {
                    Some(saved) if saved.validate(scope, original).is_ok() => {
                        Ok(Outcome::Recovered)
                    }
                    _ => Ok(Outcome::Retry),
                }
            }
            CompletionRecoveryResult::NeedsLogin { .. } => Ok(Outcome::NeedsLogin),
            // A finished stream can precede readable, complete history. Only
            // this closed diagnostic retries the same read with durable backoff.
            CompletionRecoveryResult::Retained {
                diagnostic:
                    Some(CompletionRecoveryDiagnostic {
                        stage: CompletionRecoveryStage::History,
                        code: CompletionRecoveryCode::CompletionUnverified,
                    }),
                ..
            } => Ok(Outcome::Retry),
            CompletionRecoveryResult::Retained { .. } => Ok(Outcome::Retained),
            CompletionRecoveryResult::Unknown { .. } => Ok(Outcome::Retry),
        }
    };
    let outcome = match tokio::time::timeout(Duration::from_secs(85), operation).await {
        Ok(Ok(outcome)) => outcome,
        _ => Outcome::Retry,
    };
    // Even after an uncertain start/response, close only this owned browser.
    // Durable ownership survives a lost close; expiry never proves disposal.
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        state
            .channel_service()
            .close_owned_browser(scope, session_id),
    )
    .await;
    if let Err(error) = state
        .channel_job_repository()
        .release_account(scope, account_id, reservation_id)
        .await
    {
        tracing::warn!(code = ?error.code, "completion recovery reservation release failed");
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{Json, Router, extract::State, http::StatusCode, routing::any};
    use geo_domain::*;
    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct Runner {
        events: Arc<Mutex<Vec<String>>>,
        mode: &'static str,
        history: String,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    async fn runner(
        State(stub): State<Runner>,
        uri: axum::http::Uri,
        body: axum::body::Bytes,
    ) -> (StatusCode, Json<Value>) {
        let path = uri.path();
        stub.events.lock().await.push(path.to_owned());
        let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let result = if path == "/v1/lifecycle" {
            json!({"generation":Uuid::from_u128(123)})
        } else if path == "/v1/sessions" {
            assert_eq!(payload["platform"], "deepseek");
            json!({"session_id":payload["session_id"],"generation":payload["expected_generation"]})
        } else if path.ends_with("/dispose") {
            if stub.mode == "lost-close" {
                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
            }
            json!({"session_id":path.split('/').nth(3).unwrap(),
                "generation":payload["expected_generation"],"state":"disposed"})
        } else if path.ends_with("/complete") {
            json!({"identity":{"platform_account_id":if stub.mode == "wrong-identity" {
                "other-account"
            } else {"synthetic-account"},"display_name":"Account","avatar_url":null},
                "storage_state":{}})
        } else if path.ends_with("/recover-conversation-completion") {
            if stub.mode == "blocked-read" {
                stub.entered.notify_one();
                stub.release.notified().await;
            }
            assert_eq!(payload.as_object().unwrap().len(), 5);
            assert_eq!(payload["external_conversation_id"], "synthetic-chat");
            assert_eq!(payload["request_message_id"], "1");
            assert_eq!(payload["response_message_id"], "2");
            if stub.mode == "lost-response" {
                return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
            }
            if matches!(
                stub.mode,
                "retained" | "needs_login" | "incomplete-history" | "unsupported"
            ) {
                json!({"status":if stub.mode == "needs_login" {"needs_login"} else {"retained"},
                "execution_id":payload["execution_id"],
                "external_conversation_id":"synthetic-chat",
                "diagnostic":match stub.mode {
                    "incomplete-history" => json!({"stage":"history","code":"completion_unverified"}),
                    "unsupported" => json!({"stage":"scope","code":"unsupported_platform"}),
                    _ => Value::Null,
                }})
            } else {
                json!({"status":"recovered","execution_id":payload["execution_id"],
                    "external_conversation_id":"synthetic-chat",
                    "recovery_identity":{"provider":"deepseek","platform_account_id":"synthetic-account"},
                    "observed_at":"2026-01-01T00:00:00Z",
                    "history_json":stub.history,"history_sha256":sha256_hex(stub.history.as_bytes())})
            }
        } else {
            panic!("unexpected browser operation {path}");
        };
        (StatusCode::OK, Json(result))
    }

    struct Evidence {
        original: ObservationCapture,
        stored: Mutex<Option<ObservationCompletionRecovery>>,
        fail: bool,
    }
    #[async_trait]
    impl ObservationCompletionRecoveryRepository for Evidence {
        async fn save(
            &self,
            scope: &TenantScope,
            input: ObservationCompletionRecoveryInput,
        ) -> Result<ObservationCompletionRecoveryReceipt, AppError> {
            if self.fail {
                return Err(AppError::conflict("synthetic storage failure"));
            }
            let digest = input
                .validate(scope, &self.original)?
                .digest_sha256()
                .to_owned();
            let receipt = ObservationCompletionRecoveryReceipt {
                recovery_id: input.recovery_id,
                schema_version: 1,
                digest_sha256: digest,
                stored_at: Utc::now(),
            };
            *self.stored.lock().await = Some(ObservationCompletionRecovery {
                input,
                receipt: receipt.clone(),
            });
            Ok(receipt)
        }
        async fn get(
            &self,
            _: &TenantScope,
            _: Uuid,
        ) -> Result<Option<ObservationCompletionRecovery>, AppError> {
            Ok(self.stored.lock().await.clone())
        }
        async fn get_by_source(
            &self,
            scope: &TenantScope,
            id: Uuid,
        ) -> Result<Option<ObservationCompletionRecovery>, AppError> {
            self.get(scope, id).await
        }
    }

    fn capture(
        scope: &TenantScope,
        account_id: Uuid,
        session: Uuid,
    ) -> (ObservationCapture, String) {
        let history = json!({
            "source":"browser_observed_history","observed_at":"2026-01-01T00:00:00Z",
            "cache_request":{"cache_version":null,"cache_reset_at":null},
            "cache_control":"REPLACE","chat_session_id":"synthetic-chat",
            "messages":[
                {"message_id":1,"parent_id":null,"role":"USER","status":"FINISHED","has_pending_fragment":false},
                {"message_id":2,"parent_id":1,"role":"ASSISTANT","status":"FINISHED","has_pending_fragment":false}
            ],"current_message_id":2
        }).to_string();
        let source = json!({
            "messages":[
                {"event":"ready","data":{"request_message_id":1,"response_message_id":2,"model_type":"synthetic-model"}},
                {"event":"delta","data":{"v":"synthetic answer"}},
                {"event":"finish","raw_data":""}
            ],
            "ownership":{
                "schema_version":"geo.deepseek.ownership.v1","provider":"deepseek",
                "creation":{"source":"browser_observed_create","chat_session_id":"synthetic-chat"},
                "submission":{"chat_session_id":"synthetic-chat","parent_message_id":null,
                    "model_type":"synthetic-model","prompt_sha256":"0".repeat(64),
                    "search_enabled":true,"thinking_enabled":false}
            }
        }).to_string();
        let input = ObservationCaptureInput {
            capture_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            account_id,
            runner_session_id: session,
            ordinal: 0,
            observed_at: Utc::now(),
            original_identity: Some(ObservationProviderIdentity {
                provider: "deepseek".into(),
                platform_account_id: "synthetic-account".into(),
            }),
            snapshot: ObservationCaptureSnapshot::Source {
                source_sha256: sha256_hex(source.as_bytes()),
                source_json: source,
            },
            owned_conversation: Some(CapturedConversation {
                provider: "deepseek".into(),
                external_conversation_id: "synthetic-chat".into(),
                purpose: CapturedConversationPurpose::Measurement,
                correlation: ConversationCorrelation::CreateResponse,
            }),
            completion: None,
        };
        let receipt = ObservationCaptureReceipt {
            capture_id: input.capture_id,
            schema_version: 1,
            digest_sha256: input.validate(scope).unwrap(),
            stored_at: Utc::now(),
        };
        (ObservationCapture { input, receipt }, history)
    }

    async fn fixture(
        mode: &'static str,
    ) -> (
        AppState,
        TenantScope,
        ObservationCompletionRecoveryClaim,
        Evidence,
        Runner,
        tokio::task::JoinHandle<()>,
    ) {
        let scope = TenantScope::new(
            DEVELOPMENT_OPERATOR_ID,
            DEVELOPMENT_TENANT_ID,
            Some(Uuid::new_v4().into()),
        );
        let account_id = Uuid::new_v4();
        let original_session = Uuid::new_v4();
        let (original, history) = capture(&scope, account_id, original_session);
        let stub = Runner {
            events: Arc::new(Mutex::new(vec![])),
            mode,
            history,
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().fallback(any(runner)).with_state(stub.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let service = crate::channels::ChannelService::development().with_browser(
            crate::BrowserBridge::new(format!("http://{address}"), "synthetic-token".into())
                .unwrap(),
        );
        service
            .repository
            .save_account(
                &scope,
                ChannelAccountRecord {
                    account: ChannelAccount {
                        account_id,
                        project_id: scope.project_id.unwrap(),
                        owner_kind: ChannelOwnerKind::Customer,
                        platform: "deepseek".into(),
                        group_id: None,
                        status: ChannelStatus::Ready,
                        display_name: None,
                        platform_account_id: Some("synthetic-account".into()),
                        avatar_url: None,
                        enabled: true,
                        proxy_configured: false,
                        proxy_server: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    session: Some(
                        service
                            .encrypt(&scope, account_id, "session", b"{}")
                            .unwrap(),
                    ),
                    proxy: None,
                },
            )
            .await
            .unwrap();
        let state = AppState::development().with_channel_service(service);
        let jobs = state.channel_job_repository();
        let operation_scope = crate::browser_operations::execution_scope(&scope, original_session);
        jobs.begin_browser_operation(
            &operation_scope,
            &BeginBrowserOperation {
                operation_id: original_session,
                session_id: original_session,
                account_id,
                purpose: BrowserOperationPurpose::Measure,
                runner_generation: Uuid::from_u128(123),
                effective_network: state
                    .channel_service()
                    .encrypt(
                        &scope,
                        original_session,
                        "browser_operation_network",
                        b"null",
                    )
                    .unwrap(),
                network_selection: None,
                reservation_id: None,
                expires_at: Utc::now() + chrono::Duration::minutes(3),
            },
            Utc::now(),
        )
        .await
        .unwrap();
        jobs.release_browser_unstarted(&operation_scope, original_session, Utc::now())
            .await
            .unwrap();
        let claim = ObservationCompletionRecoveryClaim {
            job_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            lease_until: Utc::now() + chrono::Duration::minutes(3),
            request_binding: original.recovery_request_binding(&scope).unwrap(),
            original: original.clone(),
        };
        (
            state,
            scope,
            claim,
            Evidence {
                original,
                stored: Mutex::new(None),
                fail: mode == "lost-save",
            },
            stub,
            server,
        )
    }

    #[tokio::test]
    async fn exact_history_is_saved_before_success_without_prompt_or_delete() {
        let (state, scope, claim, evidence, stub, server) = fixture("success").await;
        assert_eq!(
            recover_once(&state, &evidence, &scope, &claim).await,
            Outcome::Recovered
        );
        let saved = evidence.stored.lock().await.clone().unwrap();
        saved.validate(&scope, &claim.original).unwrap();
        assert_ne!(
            saved.input.runner_session_id,
            claim.original.input.runner_session_id
        );
        let events = stub.events.lock().await;
        assert_eq!(
            events
                .iter()
                .filter(|p| p.ends_with("/recover-conversation-completion"))
                .count(),
            1
        );
        assert!(events.last().unwrap().ends_with("/dispose"));
        let operation = state
            .channel_job_repository()
            .get_browser_operation(
                &crate::browser_operations::execution_scope(&scope, saved.input.runner_session_id),
                saved.input.runner_session_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            operation.purpose,
            BrowserOperationPurpose::CompletionRecovery
        );
        assert_eq!(operation.state, BrowserOperationState::Disposed);
        server.abort();
    }

    #[tokio::test]
    async fn identity_transport_retention_and_storage_failures_stay_distinct() {
        for (mode, expected, read) in [
            ("wrong-identity", Outcome::NeedsLogin, false),
            ("lost-response", Outcome::Retry, true),
            ("retained", Outcome::Retained, true),
            ("incomplete-history", Outcome::Retry, true),
            ("unsupported", Outcome::Retained, true),
            ("needs_login", Outcome::NeedsLogin, true),
            ("lost-save", Outcome::Retry, true),
        ] {
            let (state, scope, claim, evidence, stub, server) = fixture(mode).await;
            assert_eq!(
                recover_once(&state, &evidence, &scope, &claim).await,
                expected,
                "{mode}"
            );
            assert!(evidence.stored.lock().await.is_none());
            let events = stub.events.lock().await;
            assert_eq!(
                events
                    .iter()
                    .any(|p| p.ends_with("/recover-conversation-completion")),
                read
            );
            assert!(events.last().unwrap().ends_with("/dispose"));
            server.abort();
        }
    }

    #[tokio::test]
    async fn expired_lease_and_changed_binding_do_not_start_browser() {
        for expired in [true, false] {
            let (state, scope, mut claim, evidence, stub, server) = fixture("success").await;
            if expired {
                claim.lease_until = Utc::now();
            } else {
                claim.request_binding.external_conversation_id = "other-chat".into();
            }
            assert_eq!(
                recover_once(&state, &evidence, &scope, &claim).await,
                if expired {
                    Outcome::Retry
                } else {
                    Outcome::Retained
                }
            );
            assert!(stub.events.lock().await.is_empty());
            server.abort();
        }
    }

    #[tokio::test]
    async fn lost_close_does_not_release_owned_account() {
        let (state, scope, claim, evidence, _stub, server) = fixture("lost-close").await;
        assert_eq!(
            recover_once(&state, &evidence, &scope, &claim).await,
            Outcome::Recovered
        );
        assert!(
            state
                .channel_job_repository()
                .reserve_account(
                    &scope,
                    claim.original.input.account_id,
                    Uuid::new_v4(),
                    Utc::now(),
                    Utc::now() + chrono::Duration::minutes(3)
                )
                .await
                .is_err()
        );
        server.abort();
    }

    struct Jobs {
        claim: ObservationCompletionRecoveryClaim,
        claimed: std::sync::atomic::AtomicBool,
        finished: tokio::sync::Notify,
        outcome: Mutex<Option<Outcome>>,
        reject_finish: bool,
    }

    #[async_trait]
    impl ObservationCompletionRecoveryJobRepository for Jobs {
        async fn scan_unqueued(
            &self,
            _: chrono::DateTime<Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<ObservationCompletionRecoveryBackfillItem>, AppError> {
            unreachable!()
        }
        async fn scan_due(
            &self,
            _: chrono::DateTime<Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<ObservationCompletionRecoveryDueItem>, AppError> {
            unreachable!()
        }
        async fn enqueue(&self, _: &TenantScope, _: Uuid) -> Result<Uuid, AppError> {
            unreachable!()
        }
        async fn claim(
            &self,
            _: &TenantScope,
            id: Uuid,
        ) -> Result<Option<ObservationCompletionRecoveryClaim>, AppError> {
            assert_eq!(id, self.claim.job_id);
            Ok(
                (!self.claimed.swap(true, std::sync::atomic::Ordering::SeqCst))
                    .then(|| self.claim.clone()),
            )
        }
        async fn finish(
            &self,
            _: &TenantScope,
            id: Uuid,
            lease: Uuid,
            outcome: Outcome,
        ) -> Result<(), AppError> {
            assert_eq!(id, self.claim.job_id);
            assert_eq!(lease, self.claim.lease_id);
            if self.reject_finish {
                self.finished.notify_one();
                return Err(AppError::conflict("synthetic stale lease"));
            }
            *self.outcome.lock().await = Some(outcome);
            self.finished.notify_one();
            Ok(())
        }
    }

    #[tokio::test]
    async fn caller_cancellation_keeps_close_and_fenced_finish_without_redispatch() {
        for reject_finish in [false, true] {
            let (state, scope, claim, evidence, stub, server) = fixture("blocked-read").await;
            let evidence = Arc::new(evidence);
            let jobs = Arc::new(Jobs {
                claim: claim.clone(),
                claimed: std::sync::atomic::AtomicBool::new(false),
                finished: tokio::sync::Notify::new(),
                outcome: Mutex::new(None),
                reject_finish,
            });
            let caller = tokio::spawn({
                let (state, scope, jobs, evidence) =
                    (state.clone(), scope.clone(), jobs.clone(), evidence.clone());
                async move {
                    assert!(
                        dispatch_observation_completion_recovery(
                            state,
                            jobs,
                            evidence,
                            scope,
                            claim.job_id
                        )
                        .await
                        .unwrap()
                    );
                    std::future::pending::<()>().await;
                }
            });
            tokio::time::timeout(Duration::from_secs(5), stub.entered.notified())
                .await
                .unwrap();
            caller.abort();
            assert!(
                !dispatch_observation_completion_recovery(
                    state,
                    jobs.clone(),
                    evidence,
                    scope,
                    claim.job_id
                )
                .await
                .unwrap()
            );
            stub.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), jobs.finished.notified())
                .await
                .unwrap();
            assert_eq!(
                *jobs.outcome.lock().await,
                if reject_finish {
                    None
                } else {
                    Some(Outcome::Recovered)
                }
            );
            let events = stub.events.lock().await;
            assert_eq!(
                events
                    .iter()
                    .filter(|p| p.ends_with("/recover-conversation-completion"))
                    .count(),
                1
            );
            assert!(events.last().unwrap().ends_with("/dispose"));
            server.abort();
        }
    }
}
