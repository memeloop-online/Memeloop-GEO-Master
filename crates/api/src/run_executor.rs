//! The driver between the agent store and the embedded runtime.
//!
//! Both halves of this seam already existed and were tested separately: the
//! bridge that decides what a script may reach, and the repository that durably
//! records a turn.  What was missing was the thing in between, which is why a
//! run accepted against a configured runtime stayed `queued` forever.
//!
//! It is an in-process background task.  There is deliberately no second
//! runtime, no queue service and no polling worker: one process accepts the
//! turn and runs it, and the durable state machine in the repository — not a
//! scheduler — is what makes the run's progress observable and its completion
//! exactly once.

use std::sync::Arc;

use geo_domain::{
    AgentRepository, AgentRuntime, AppError, RunCompletion, RunStatus, StoreCheckpoint,
    SubmitAcceptance, TenantScope, TurnInput, TurnReport, sha256_hex,
};
use serde_json::json;

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
    let input = TurnInput {
        conversation_id: acceptance.conversation.id,
        message_id: acceptance.message.id,
        turn_id: acceptance.turn.id,
        run_id: acceptance.run.id,
        prompt: acceptance.message.content.clone(),
        attachments: acceptance.message.attachments.clone(),
    };
    tokio::spawn(async move {
        execute(runtime, repository, scope, input).await;
    });
}

/// Claims the run, runs exactly one turn, and records its terminal outcome.
///
/// Every step reports its failure through the run's own state rather than
/// through a return value nobody reads: a turn that failed produces a `failed`
/// run with a typed error and no answer, which is what the client sees.
async fn execute(
    runtime: Arc<dyn AgentRuntime>,
    repository: Arc<dyn AgentRepository>,
    scope: TenantScope,
    input: TurnInput,
) {
    let run_id = input.run_id;
    match repository.begin_run(&scope, run_id).await {
        // Claimed by someone else, already terminal, or cancelled between
        // acceptance and dispatch. Either way it is not ours to run, and
        // reporting anything would be reporting a turn that never happened.
        Ok(None) => return,
        Ok(Some(_)) => {}
        Err(error) => {
            // The run stays `queued` with no successor. Recovering it is the
            // restart-reconciliation gap recorded in TODO.md, not something to
            // paper over here by pretending the turn ran.
            tracing::warn!(%run_id, %error, "the run could not be claimed for execution");
            return;
        }
    }

    // The accepted message is the durable input authority, including its
    // attachment bindings. Reconstruct exactly the same input on reentry.
    let execution = async {
        let detail = repository
            .get_conversation(&scope, input.conversation_id)
            .await?
            .ok_or_else(|| AppError::not_found("run conversation not found"))?;
        let restored = detail.turn_input(run_id)?;
        let report = runtime.run_turn(&scope, restored.clone()).await?;
        Ok::<_, AppError>((restored, report))
    }
    .await;
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
        Err(error) => {
            tracing::warn!(%run_id, %error, "the run's outcome could not be recorded");
        }
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
        AgentRepository, AppendMessage, CreateConversation, MemoryAgentRepository,
        RuntimeCapability,
    };
    use serde_json::Value;
    use std::time::Duration;

    struct FixtureRuntime {
        metadata: Value,
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
}
