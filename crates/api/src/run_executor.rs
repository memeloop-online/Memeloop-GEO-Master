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
    AgentRepository, AgentRuntime, RunCompletion, RunStatus, SubmitAcceptance, TenantScope,
    TurnInput,
};

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
        turn_id: acceptance.turn.id,
        run_id: acceptance.run.id,
        prompt: acceptance.message.content.clone(),
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

    let completion = match runtime.run_turn(&scope, input).await {
        Ok(report) => RunCompletion::Succeeded {
            content: report.content,
            metadata: report.metadata,
        },
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
