//! The driver between the agent store and the embedded runtime.
//!
//! Both halves of this seam already existed and were tested separately: the
//! bridge that decides what a script may reach, and the repository that durably
//! records a turn.  What was missing was the thing in between, which is why a
//! run accepted against a configured runtime stayed `queued` forever.
//!
//! HTTP dispatch and the PostgreSQL queued-run scanner both enter the same
//! atomic claim. The repository is authoritative for execution ownership and
//! for the turn input; scanning never synthesizes a message or a prompt.
//! Running runs are not reclaimed here: recovering a mid-turn side effect
//! requires a separate lease and durable tool-call protocol.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use geo_domain::{
    AgentRepository, AgentRuntime, AppError, Run, RunCompletion, RunId, RunStatus, StoreCheckpoint,
    SubmitAcceptance, TenantScope, TurnInput, TurnReport, sha256_hex,
};
use serde_json::json;

fn active_runs() -> &'static Mutex<HashMap<RunId, Arc<AtomicBool>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<RunId, Arc<AtomicBool>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ActiveRun(RunId);

impl ActiveRun {
    fn register(run_id: RunId, cancellation: Arc<AtomicBool>) -> Self {
        active_runs()
            .lock()
            .expect("active run lock")
            .insert(run_id, cancellation);
        Self(run_id)
    }
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        active_runs()
            .lock()
            .expect("active run lock")
            .remove(&self.0);
    }
}

/// Called only after the scoped repository has committed a cancelled run.
/// A terminal success/failure must not interrupt another run.
pub(crate) fn signal_cancelled(run_id: RunId) {
    if let Some(flag) = active_runs().lock().expect("active run lock").get(&run_id) {
        flag.store(true, Ordering::SeqCst);
    }
}

/// A cancellation can be accepted on another API replica. Check durable state
/// while this particular run is live; a failed scoped read must fail closed.
async fn watch_run(
    repository: Arc<dyn AgentRepository>,
    scope: TenantScope,
    run_id: RunId,
    cancellation: Arc<AtomicBool>,
) {
    // Local requests signal immediately. Remote replicas have a bounded
    // fallback without keeping a connection occupied or reading histories.
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    loop {
        interval.tick().await;
        let still_running = matches!(
            repository.run_status(&scope, run_id).await,
            Ok(Some(RunStatus::Running))
        );
        if !still_running {
            cancellation.store(true, Ordering::SeqCst);
            break;
        }
    }
}

/// Starts the run an accepted message created.
///
/// Called after the acceptance transaction has committed, never inside it: the
/// turn can take minutes, and holding a transaction open for its whole length
/// would pin a connection and a row lock for the duration.
///
/// Returns as soon as the work is scheduled.  The HTTP response describes
/// acceptance, so it stays whatever the repository recorded and never
/// optimistically reads `running`.
pub fn dispatch(
    runtime: Arc<dyn AgentRuntime>,
    repository: Arc<dyn AgentRepository>,
    scope: TenantScope,
    acceptance: &SubmitAcceptance,
) {
    // A filter, not a guard.  The atomic claim in `begin_run` is what decides
    // ownership; this only avoids scheduling work for the ordinary case of a
    // run that was already made terminal at acceptance time, because the
    // capability was missing.
    if acceptance.run.status != RunStatus::Queued {
        return;
    }
    let run_id = acceptance.run.id;
    tokio::spawn(async move {
        dispatch_queued(runtime, repository, scope, run_id).await;
    });
}

/// Claim a durable queued run before scheduling its execution. The scanner
/// awaits the claim so pending claims cannot pile up across scan passes.
/// This returns after the claim, without waiting for the model; duplicate
/// HTTP and recovery dispatches are resolved by the repository.
pub async fn dispatch_queued(
    runtime: Arc<dyn AgentRuntime>,
    repository: Arc<dyn AgentRepository>,
    scope: TenantScope,
    run_id: RunId,
) {
    let Some(claimed) = claim_run(&*repository, &scope, run_id).await else {
        return;
    };
    tokio::spawn(async move {
        execute_claimed(runtime, repository, scope, claimed).await;
    });
}

async fn claim_run(
    repository: &dyn AgentRepository,
    scope: &TenantScope,
    run_id: RunId,
) -> Option<Run> {
    match repository.begin_run(scope, run_id).await {
        // Claimed by someone else, already terminal, or cancelled between
        // acceptance and dispatch. Either way it is not ours to run, and
        // reporting anything would be reporting a turn that never happened.
        Ok(None) => None,
        Ok(Some(claimed)) => Some(claimed),
        Err(error) => {
            // The run remains queued and is eligible for the next scan.
            tracing::warn!(code = ?error.code, "the run could not be claimed for execution");
            None
        }
    }
}

/// Runs exactly one claimed turn and records its terminal outcome.
///
/// Every step reports its failure through the run's own state rather than
/// through a return value nobody reads: a turn that failed produces a `failed`
/// run with a typed error and no answer, which is what the client sees.
async fn execute_claimed(
    runtime: Arc<dyn AgentRuntime>,
    repository: Arc<dyn AgentRepository>,
    scope: TenantScope,
    claimed: Run,
) {
    let run_id = claimed.id;
    let cancellation = Arc::new(AtomicBool::new(false));
    let _active = ActiveRun::register(run_id, Arc::clone(&cancellation));
    // A cancel on another replica may have committed after begin_run but
    // before this executor registered its local flag. Resolve that gap before
    // the first model/tool call; later changes are caught by the watcher.
    if !matches!(
        repository.run_status(&scope, run_id).await,
        Ok(Some(RunStatus::Running))
    ) {
        cancellation.store(true, Ordering::SeqCst);
        let _ = repository
            .finish_run(
                &scope,
                run_id,
                RunCompletion::Failed {
                    error: AppError::new(
                        geo_domain::ErrorCode::DependencyUnavailable,
                        "run state could not be verified before execution",
                    ),
                },
            )
            .await;
        return;
    }
    let monitor = tokio::spawn(watch_run(
        Arc::clone(&repository),
        scope.clone(),
        run_id,
        Arc::clone(&cancellation),
    ));

    // The accepted message is the durable input authority, including its
    // attachment bindings. Reconstruct exactly the same input on reentry.
    let execution = async {
        let restored = repository
            .load_turn_input(&scope, claimed.conversation_id, run_id)
            .await?;
        let report = runtime
            .run_turn_with_cancellation(&scope, restored.clone(), Arc::clone(&cancellation))
            .await?;
        Ok::<_, AppError>((restored, report))
    }
    .await;
    monitor.abort();
    let _ = monitor.await;
    // Cancellation may have committed while the runtime was returning its
    // answer; only the store can decide the final result.
    if cancellation.load(Ordering::SeqCst) {
        // A replica's cancel remains authoritative; a failed watch read
        // instead closes this run as failed, never as a fabricated success.
        let _ = repository
            .finish_run(
                &scope,
                run_id,
                RunCompletion::Failed {
                    error: AppError::new(
                        geo_domain::ErrorCode::DependencyUnavailable,
                        "run state could not be verified during execution",
                    ),
                },
            )
            .await;
        return;
    }
    let completion = match execution {
        Ok((restored, report)) => {
            match persist_runtime_state(&*repository, &scope, &restored, &report).await {
                Ok(()) => RunCompletion::Succeeded {
                    content: report.content,
                    metadata: report.metadata,
                },
                Err(error) => RunCompletion::Failed { error },
            }
        }
        Err(error) => RunCompletion::Failed { error },
    };
    // `None` here means a cancellation won the race and already made the run
    // terminal. That verdict stands; this completion is discarded rather than
    // forced on top of it.
    match repository.finish_run(&scope, run_id, completion).await {
        Ok(Some(transition)) => tracing::debug!(
            %run_id,
            status = ?transition.run.status,
            "the turn reached a terminal state"
        ),
        Ok(None) => tracing::debug!(%run_id, "the run was already terminal; nothing written"),
        Err(error) => tracing::warn!(code = ?error.code, "the run's outcome could not be recorded"),
    }
}

#[cfg(test)]
async fn execute(
    runtime: Arc<dyn AgentRuntime>,
    repository: Arc<dyn AgentRepository>,
    scope: TenantScope,
    run_id: RunId,
) {
    if let Some(claimed) = claim_run(&*repository, &scope, run_id).await {
        execute_claimed(runtime, repository, scope, claimed).await;
    }
}

/// Persist the runtime's durable progress before publishing a terminal run
/// outcome.
///
/// The completed-result checkpoint is intentionally owned by the executor
/// rather than by the JavaScript isolate. It proves which input produced the
/// terminal result, but it is not an intermediate checkpoint and does not
/// permit resuming an interrupted turn. Tool-call intent and outcomes must be
/// recorded by Rust at the HostBridge before/after executing each capability,
/// never inferred from JavaScript-controlled completion metadata.
async fn persist_runtime_state(
    repository: &dyn AgentRepository,
    scope: &TenantScope,
    input: &TurnInput,
    report: &TurnReport,
) -> Result<(), AppError> {
    let input_hash = sha256_hex(
        serde_json::to_string(input)
            .map_err(|error| {
                AppError::new(
                    geo_domain::ErrorCode::Internal,
                    format!("turn input cannot be hashed: {error}"),
                )
            })?
            .as_bytes(),
    );
    repository
        .store_checkpoint(
            scope,
            input.run_id,
            StoreCheckpoint {
                checkpoint_scope: "agent.turn".to_owned(),
                step_key: "completed".to_owned(),
                input_hash,
                result_ref: None,
                state: json!({
                    "turn_id": input.turn_id,
                    "conversation_id": input.conversation_id,
                    "content": report.content,
                    "metadata": report.metadata,
                }),
            },
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use geo_domain::{
        AgentRepository, AppendMessage, AttachmentId, AttachmentReference, CreateConversation,
        MemoryAgentRepository, RuntimeCapability,
    };
    use serde_json::Value;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tokio::sync::Notify;

    struct FixtureRuntime {
        metadata: Value,
    }

    struct WaitingRuntime {
        started: Arc<Notify>,
    }

    #[derive(Default)]
    struct RecordingRuntime {
        inputs: Mutex<Vec<TurnInput>>,
        invocations: AtomicUsize,
    }

    #[async_trait]
    impl AgentRuntime for RecordingRuntime {
        async fn capability(&self) -> RuntimeCapability {
            RuntimeCapability::available("fixture", None)
        }

        async fn run_turn(
            &self,
            _scope: &TenantScope,
            input: TurnInput,
        ) -> Result<TurnReport, AppError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            self.inputs.lock().unwrap().push(input);
            Ok(TurnReport {
                content: "recorded answer".to_owned(),
                metadata: Value::Null,
            })
        }
    }

    #[async_trait]
    impl AgentRuntime for WaitingRuntime {
        async fn capability(&self) -> RuntimeCapability {
            RuntimeCapability::available("waiting", Some("1".to_owned()))
        }

        async fn run_turn(
            &self,
            _scope: &TenantScope,
            _input: TurnInput,
        ) -> Result<TurnReport, AppError> {
            unreachable!("executor must provide the run-scoped cancellation flag")
        }

        async fn run_turn_with_cancellation(
            &self,
            _scope: &TenantScope,
            _input: TurnInput,
            cancellation: Arc<AtomicBool>,
        ) -> Result<TurnReport, AppError> {
            self.started.notify_one();
            while !cancellation.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(AppError::conflict("run stopped"))
        }
    }

    #[async_trait]
    impl AgentRuntime for FixtureRuntime {
        async fn capability(&self) -> geo_domain::RuntimeCapability {
            RuntimeCapability::available("fixture", Some("1".to_owned()))
        }

        async fn run_turn(
            &self,
            _scope: &TenantScope,
            _input: TurnInput,
        ) -> Result<TurnReport, AppError> {
            Ok(TurnReport {
                content: "fixture answer".to_owned(),
                metadata: self.metadata.clone(),
            })
        }
    }

    fn fixture_scope() -> TenantScope {
        TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        )
    }

    #[tokio::test]
    async fn queued_recovery_uses_durable_root_history_and_attachments() {
        let repository = Arc::new(MemoryAgentRepository::new());
        let runtime = Arc::new(RecordingRuntime::default());
        let scope = fixture_scope();
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .unwrap();
        let first = repository
            .append_message(
                &scope,
                conversation.id,
                AppendMessage {
                    content: "earlier prompt".into(),
                    attachments: vec![],
                    metadata: Value::Null,
                },
                "history-key".into(),
                "history-body".into(),
                runtime.capability().await,
            )
            .await
            .unwrap();
        execute(
            runtime.clone(),
            repository.clone(),
            scope.clone(),
            first.run.id,
        )
        .await;
        let attachment = AttachmentReference {
            attachment_id: AttachmentId::from(uuid::Uuid::new_v4()),
            object_id: "stored-object".into(),
            filename: "notes.txt".into(),
            media_type: Some("text/plain".into()),
            size_bytes: Some(8),
            sha256: None,
            object_version: Some("v1".into()),
        };
        let second = repository
            .append_message(
                &scope,
                conversation.id,
                AppendMessage {
                    content: "follow-up".into(),
                    attachments: vec![attachment.clone()],
                    metadata: Value::Null,
                },
                "recovery-key".into(),
                "recovery-body".into(),
                runtime.capability().await,
            )
            .await
            .unwrap();
        // There was no HTTP dispatch for this turn. The scanner can supply
        // only its scoped run ID; all input must come from the store.
        execute(
            runtime.clone(),
            repository.clone(),
            scope.clone(),
            second.run.id,
        )
        .await;
        let inputs = runtime.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[1].run_id, second.run.id);
        assert_eq!(inputs[1].prompt, "follow-up");
        assert_eq!(inputs[1].attachments, vec![attachment]);
        assert_eq!(inputs[1].history.len(), 2);
        assert_eq!(inputs[1].history[0].content, "earlier prompt");
        assert_eq!(inputs[1].history[1].content, "recorded answer");
    }

    #[tokio::test]
    async fn duplicate_http_and_recovery_claim_executes_once() {
        let repository = Arc::new(MemoryAgentRepository::new());
        let runtime = Arc::new(RecordingRuntime::default());
        let scope = fixture_scope();
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .unwrap();
        let acceptance = repository
            .append_message(
                &scope,
                conversation.id,
                AppendMessage {
                    content: "one turn".into(),
                    attachments: vec![],
                    metadata: Value::Null,
                },
                "claim-key".into(),
                "claim-body".into(),
                runtime.capability().await,
            )
            .await
            .unwrap();
        let run_id = acceptance.run.id;
        tokio::join!(
            dispatch_queued(runtime.clone(), repository.clone(), scope.clone(), run_id),
            dispatch_queued(runtime.clone(), repository.clone(), scope.clone(), run_id)
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let detail = repository
                    .get_conversation(&scope, conversation.id)
                    .await
                    .unwrap()
                    .unwrap();
                if detail.runs[0].status == RunStatus::Succeeded {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("claimed run must finish");
        assert_eq!(runtime.invocations.load(Ordering::SeqCst), 1);
        let detail = repository
            .get_conversation(&scope, conversation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.runs[0].status, RunStatus::Succeeded);
        assert_eq!(
            detail
                .messages
                .iter()
                .filter(|message| message.content == "recorded answer")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn queued_dispatch_returns_after_claim_while_runtime_is_still_running() {
        let repository = Arc::new(MemoryAgentRepository::new());
        let scope = fixture_scope();
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .unwrap();
        let acceptance = repository
            .append_message(
                &scope,
                conversation.id,
                AppendMessage {
                    content: "wait for cancellation".into(),
                    attachments: vec![],
                    metadata: Value::Null,
                },
                "async-claim-key".into(),
                "async-claim-body".into(),
                RuntimeCapability::available("waiting", None),
            )
            .await
            .unwrap();
        let started = Arc::new(Notify::new());
        let ready = started.notified();
        tokio::time::timeout(
            Duration::from_secs(2),
            dispatch_queued(
                Arc::new(WaitingRuntime {
                    started: Arc::clone(&started),
                }),
                repository.clone(),
                scope.clone(),
                acceptance.run.id,
            ),
        )
        .await
        .expect("dispatch must return after the atomic claim");
        assert_eq!(
            repository
                .run_status(&scope, acceptance.run.id)
                .await
                .unwrap(),
            Some(RunStatus::Running),
            "claim must be durable before dispatch returns"
        );
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .expect("claimed runtime must start");
        repository
            .cancel_turn(&scope, acceptance.turn.id)
            .await
            .unwrap();
        signal_cancelled(acceptance.run.id);
        tokio::time::timeout(Duration::from_secs(2), async {
            while active_runs()
                .lock()
                .unwrap()
                .contains_key(&acceptance.run.id)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancellation must stop the claimed runtime");
        assert_eq!(
            repository
                .run_status(&scope, acceptance.run.id)
                .await
                .unwrap(),
            Some(RunStatus::Cancelled)
        );
    }

    #[tokio::test]
    async fn cancelled_and_running_runs_are_never_reclaimed() {
        let repository = Arc::new(MemoryAgentRepository::new());
        let runtime = Arc::new(RecordingRuntime::default());
        let scope = fixture_scope();
        for (index, cancel) in [true, false].into_iter().enumerate() {
            let conversation = repository
                .create_conversation(&scope, None, CreateConversation::default())
                .await
                .unwrap();
            let acceptance = repository
                .append_message(
                    &scope,
                    conversation.id,
                    AppendMessage {
                        content: "pending".into(),
                        attachments: vec![],
                        metadata: Value::Null,
                    },
                    format!("pending-key-{index}"),
                    format!("pending-body-{index}"),
                    runtime.capability().await,
                )
                .await
                .unwrap();
            if cancel {
                repository
                    .cancel_turn(&scope, acceptance.turn.id)
                    .await
                    .unwrap();
            } else {
                repository
                    .begin_run(&scope, acceptance.run.id)
                    .await
                    .unwrap();
            }
            execute(
                runtime.clone(),
                repository.clone(),
                scope.clone(),
                acceptance.run.id,
            )
            .await;
            let detail = repository
                .get_conversation(&scope, conversation.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                detail.runs[0].status,
                if cancel {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Running
                }
            );
        }
        assert_eq!(runtime.invocations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatch_persists_completed_result_without_forging_tool_calls() {
        let repository = Arc::new(MemoryAgentRepository::new());
        let scope = fixture_scope();
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .expect("conversation");
        let acceptance = repository
            .append_message(
                &scope,
                conversation.id,
                AppendMessage {
                    content: "fixture prompt".to_owned(),
                    attachments: Vec::new(),
                    metadata: Value::Null,
                },
                "dispatch-key".to_owned(),
                "dispatch-body".to_owned(),
                RuntimeCapability::available("fixture", Some("1".to_owned())),
            )
            .await
            .expect("acceptance");
        let run_id = acceptance.run.id;
        let metadata = json!({
            "tool_calls": [{
                "tool_call_id": "untrusted-script-claim",
                "outcome": "succeeded"
            }]
        });
        dispatch(
            Arc::new(FixtureRuntime { metadata }),
            repository.clone(),
            scope.clone(),
            &acceptance,
        );

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let detail = repository
                .get_conversation(&scope, conversation.id)
                .await
                .expect("detail")
                .expect("conversation exists");
            if detail
                .runs
                .iter()
                .any(|run| run.status == RunStatus::Succeeded)
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "dispatch did not finish"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let checkpoint = repository
            .load_checkpoint(&scope, run_id, "agent.turn", "completed")
            .await
            .expect("checkpoint lookup")
            .expect("completion checkpoint");
        assert_eq!(checkpoint.state["content"], "fixture answer");
        let calls = repository
            .list_tool_calls(&scope, run_id)
            .await
            .expect("ledger lookup");
        assert!(calls.is_empty(), "script metadata is not a trusted ledger");
    }

    #[tokio::test]
    async fn durable_cancellation_on_another_replica_stops_only_its_run() {
        let repository = Arc::new(MemoryAgentRepository::new());
        let scope = fixture_scope();
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .unwrap();
        let acceptance = repository
            .append_message(
                &scope,
                conversation.id,
                AppendMessage {
                    content: "wait".to_owned(),
                    attachments: Vec::new(),
                    metadata: Value::Null,
                },
                "waiting-key".to_owned(),
                "waiting-body".to_owned(),
                RuntimeCapability::available("waiting", None),
            )
            .await
            .unwrap();
        let started = Arc::new(Notify::new());
        let ready = started.notified();
        dispatch(
            Arc::new(WaitingRuntime {
                started: Arc::clone(&started),
            }),
            repository.clone(),
            scope.clone(),
            &acceptance,
        );
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .expect("run must start");
        // Simulate the cancellation being committed by a different API
        // replica: no local signal_cancelled call is made.
        repository
            .cancel_turn(&scope, acceptance.turn.id)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while active_runs()
                .lock()
                .unwrap()
                .contains_key(&acceptance.run.id)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("durable cancellation must stop the running executor");
        let detail = repository
            .get_conversation(&scope, conversation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            detail
                .runs
                .iter()
                .find(|run| run.id == acceptance.run.id)
                .unwrap()
                .status,
            RunStatus::Cancelled
        );
        assert!(
            repository
                .load_checkpoint(&scope, acceptance.run.id, "agent.turn", "completed")
                .await
                .unwrap()
                .is_none(),
            "cancelled run cannot publish a completed checkpoint"
        );
        let next = repository
            .append_message(
                &scope,
                conversation.id,
                AppendMessage {
                    content: "new turn".to_owned(),
                    attachments: Vec::new(),
                    metadata: Value::Null,
                },
                "fresh-key".to_owned(),
                "fresh-body".to_owned(),
                RuntimeCapability::available("fixture", None),
            )
            .await
            .unwrap();
        dispatch(
            Arc::new(FixtureRuntime {
                metadata: Value::Null,
            }),
            repository.clone(),
            scope.clone(),
            &next,
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let detail = repository
                    .get_conversation(&scope, conversation.id)
                    .await
                    .unwrap()
                    .unwrap();
                if detail
                    .runs
                    .iter()
                    .any(|run| run.id == next.run.id && run.status == RunStatus::Succeeded)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a new run is not poisoned by the cancelled run");
    }
}
