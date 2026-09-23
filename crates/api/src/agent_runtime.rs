//! The embedded JavaScript runtime as an application capability.
//!
//! The API process never executes tenant JavaScript inline: an isolate is not
//! `Send`, so it is created, driven and dropped on one worker thread.  What the
//! process holds instead is this seam — the approved bundle and the
//! [`HostOps`] implementation — and [`AgentRuntime::run_turn`] is what the run
//! executor calls to run one turn.
//!
//! That worker thread runs a current-thread runtime of its own, and the
//! capability work is handed back to the application runtime: the engine's op
//! driver only survives the former, and a connection pool only survives the
//! latter.  [`HostBridge::new`] carries the reasoning for both halves.
//!
//! Three rules are enforced here rather than documented and hoped for:
//!
//! - An unconfigured runtime reports `capability_missing`.  It never reports
//!   itself available, and the API therefore records a failed run instead of a
//!   fabricated answer.
//! - The capability an op reaches is the one the scope names.  The scope is
//!   taken from the run, and no request payload can widen it.
//! - A bundle that produces no answer produces a failed run.  The only result
//!   channel is the bundle's own completion event; a turn that returned without
//!   reporting one is an error, never an empty answer.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use geo_domain::{
    AgentRuntime, AppError, ErrorCode, KnowledgeRepository, RUNTIME_NOT_CONFIGURED,
    RuntimeCapability, TenantScope, TurnInput, TurnReport,
};
use geo_worker::{
    HOST_BUNDLE, HOST_MAIN_MODULE, HOST_OPS_VERSION, HostBridge, HostOp, HostOpBudgets,
    HostOpError, HostOpErrorCode, HostOps, HostRuntime, ManifestPage, ManifestReadRequest,
    MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest, PublishReceipt,
    PublishRequest, TURN_COMPLETION_TOPIC, WorkerError,
};
use serde_json::{Value, json};

/// The bundle and capabilities one production runtime is assembled from.
///
/// The entry specifier is configuration rather than a constant so that this
/// crate never names a particular bundle's module path: swapping in the
/// approved MemeLoop bundle is a change of value, not of layering.
#[derive(Clone)]
struct Configured {
    bundle: &'static [(&'static str, &'static str)],
    entry: &'static str,
    capabilities: Arc<dyn HostOps>,
}

/// The wall-clock ceiling for one turn.
///
/// Deliberately an outer net rather than the primary control: every host op
/// already carries its own budget, and that is what bounds legitimate work.
/// This only has to stop a script that spins in JavaScript and never reaches an
/// op, so it sits above the op budgets rather than guessing at a "reasonable"
/// turn length and tearing down real work.
const TURN_DEADLINE: Duration = Duration::from_secs(600);

/// A Rust-hosted JavaScript runtime, or the explicit absence of one.
///
/// A runtime whose bundle or provider bridge is not approved must not accept
/// runs it cannot execute, so the absence is a value rather than a flag: there
/// is no environment variable that can turn a configured runtime on.
pub struct EmbeddedAgentRuntime {
    configured: Option<Configured>,
}

impl EmbeddedAgentRuntime {
    /// The explicit absence of a runtime.
    ///
    /// This is what the application assembles until an approved bundle and a
    /// provider bridge exist: every accepted run is durably failed with
    /// `capability_missing`, and no answer is fabricated.
    pub fn unconfigured() -> Self {
        Self { configured: None }
    }

    /// A runtime over the crate's reference bundle and the given capabilities.
    ///
    /// The reference bundle declares the host-op surface and does not run a
    /// turn; the embedding host calls its `main` once per turn.  An approved
    /// MemeLoop bundle replaces it via [`Self::with_bundle`].
    ///
    /// The caller vouches that the capability set is complete.  Reporting
    /// `available` is a claim the UI acts on, so a bundle or bridge that is only
    /// partly wired must be assembled as [`Self::unconfigured`] instead: a run
    /// is then refused up front rather than accepted and failed op by op.
    pub fn configured(capabilities: Arc<dyn HostOps>) -> Self {
        Self::with_bundle(HOST_BUNDLE, HOST_MAIN_MODULE, capabilities)
    }

    /// A runtime over an explicit, approved bundle and its entry module.
    pub fn with_bundle(
        bundle: &'static [(&'static str, &'static str)],
        entry: &'static str,
        capabilities: Arc<dyn HostOps>,
    ) -> Self {
        Self {
            configured: Some(Configured {
                bundle,
                entry,
                capabilities,
            }),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.configured.is_some()
    }

    /// The versioned op surface a configured runtime registers, so an operator
    /// can tell which capability set a run was accepted against.
    pub fn op_surface() -> Vec<String> {
        HostRuntime::op_surface()
    }

    /// Starts one isolated run authorised for `scope`, driven by the caller.
    ///
    /// Fails only when no runtime is configured: an unconfigured run is
    /// rejected here rather than started and left to fail an op later.
    ///
    /// The returned isolate is `!Send` and is meant to be driven on the
    /// current-thread runtime the caller is already inside, so capability work
    /// is bound to that same runtime.  [`Self::run`] does not use this: it
    /// drives the isolate on a current-thread runtime of its own while the
    /// capabilities stay on the application's.
    pub fn start(
        &self,
        scope: &TenantScope,
        budgets: HostOpBudgets,
    ) -> Result<HostRuntime, WorkerError> {
        let Some(configured) = self.configured.as_ref() else {
            return Err(WorkerError::new("runtime", RUNTIME_NOT_CONFIGURED));
        };
        let bridge = HostBridge::new(
            Arc::clone(&configured.capabilities),
            scope.clone(),
            tokio::runtime::Handle::current(),
        )
        .with_budgets(budgets);
        HostRuntime::new(configured.bundle, bridge, None)
    }
}

impl Default for EmbeddedAgentRuntime {
    fn default() -> Self {
        Self::unconfigured()
    }
}

impl fmt::Debug for EmbeddedAgentRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddedAgentRuntime")
            .field("configured", &self.is_configured())
            .field("ops", &HostRuntime::op_surface())
            .finish_non_exhaustive()
    }
}

impl EmbeddedAgentRuntime {
    /// Runs one turn on its own thread and reports what the bundle produced.
    async fn run(&self, scope: &TenantScope, input: TurnInput) -> Result<TurnReport, AppError> {
        let Some(configured) = self.configured.as_ref() else {
            return Err(AppError::capability_missing(RUNTIME_NOT_CONFIGURED));
        };
        let argument = turn_argument(&input);
        let bundle = configured.bundle;
        let entry = configured.entry;
        let capabilities = Arc::clone(&configured.capabilities);
        let run_scope = scope.clone();
        // The isolate is `!Send`, so it is built, driven and dropped inside one
        // blocking task — and on a current-thread runtime of its own, because
        // that is the only flavor the engine's op driver can be driven on.  The
        // capability work is moved back onto the application runtime instead,
        // so the repository pools stay where they were built.  See
        // [`HostBridge::new`] for why both halves are load-bearing.
        let application = tokio::runtime::Handle::current();
        let finished = tokio::task::spawn_blocking(move || {
            let engine = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    WorkerError::new("runtime", format!("an isolate thread was refused: {error}"))
                })?;
            engine.block_on(async {
                let bridge = HostBridge::new(capabilities, run_scope, application)
                    .with_budgets(HostOpBudgets::default());
                let mut runtime = HostRuntime::new(bundle, bridge, None)?;
                runtime.call_main(entry, &argument, TURN_DEADLINE).await?;
                Ok::<_, WorkerError>(runtime.host_state())
            })
        })
        // A detached task would leave a panicked run `running` forever, which
        // is the same defect one state later.
        .await
        .map_err(|error| {
            AppError::new(
                ErrorCode::Internal,
                format!("the turn task did not finish: {error}"),
            )
        })?;
        let state = finished.map_err(turn_failure)?;
        let completed = state
            .events
            .iter()
            .rev()
            .find(|event| event.topic == TURN_COMPLETION_TOPIC)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Internal,
                    format!(
                        "the turn returned without reporting `{TURN_COMPLETION_TOPIC}`, so it produced no answer"
                    ),
                )
            })?;
        let payload: Value = serde_json::from_str(&completed.payload).map_err(|error| {
            AppError::new(
                ErrorCode::Internal,
                format!("the turn's reported result is not readable: {error}"),
            )
        })?;
        let content = payload
            .get("answer")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Internal,
                    "the turn's reported result carries no answer".to_owned(),
                )
            })?
            .to_owned();
        Ok(TurnReport {
            content,
            metadata: payload,
        })
    }
}

#[async_trait]
impl AgentRuntime for EmbeddedAgentRuntime {
    async fn capability(&self) -> RuntimeCapability {
        match self.configured {
            Some(_) => RuntimeCapability::available("deno_core", Some(HOST_OPS_VERSION.to_owned())),
            None => RuntimeCapability::missing(RUNTIME_NOT_CONFIGURED),
        }
    }

    async fn run_turn(
        &self,
        scope: &TenantScope,
        input: TurnInput,
    ) -> Result<TurnReport, AppError> {
        self.run(scope, input).await
    }
}

/// The single argument a turn is called with.
///
/// Deliberately explicit rather than an echo of the request: this is the
/// contract with the bundle, so naming it here is what makes it testable.  A
/// later turn may carry a retrieval query distinct from the prompt, which is
/// why the two are separate now rather than one field under two names.
fn turn_argument(input: &TurnInput) -> String {
    json!({
        "prompt": input.prompt,
        "query": input.prompt,
        "run_id": input.run_id,
        "turn_id": input.turn_id,
        "conversation_id": input.conversation_id,
    })
    .to_string()
}

/// Maps a worker failure onto a typed application error.
///
/// A capability gap must not arrive as an opaque 500-class failure.  A host op
/// reports failure by throwing the registered `GeoHostOpError` whose message is
/// the structured error the bridge produced, so the class survives to here and
/// does not have to be inferred from prose.  Only a failure of the *call* can
/// carry one: loading and evaluating a module never reaches an op.
fn turn_failure(error: WorkerError) -> AppError {
    if error.stage == "call"
        && let Some(code) = reported_host_op_code(&error.message)
    {
        return AppError::new(error_code_for_host_op(code), error.to_string());
    }
    AppError::new(ErrorCode::Internal, error.to_string())
}

/// Recovers the declared host-op failure class from an exception's message.
///
/// The payload is located inside the message rather than assumed to be the
/// whole of it, because the engine prefixes what it throws with the exception's
/// class name.
fn reported_host_op_code(message: &str) -> Option<HostOpErrorCode> {
    let start = message.find('{')?;
    let end = message.rfind('}')?;
    let payload: Value = serde_json::from_str(message.get(start..=end)?).ok()?;
    let code = payload.get("code")?.as_str()?;
    serde_json::from_value(Value::String(code.to_owned())).ok()
}

/// The application class for a declared host-op failure.
///
/// Exhaustive on purpose: a new op failure class has to be placed here
/// deliberately rather than defaulting into whatever the wildcard happened to
/// be.
fn error_code_for_host_op(code: HostOpErrorCode) -> ErrorCode {
    match code {
        HostOpErrorCode::CapabilityMissing => ErrorCode::CapabilityMissing,
        HostOpErrorCode::Denied => ErrorCode::Forbidden,
        HostOpErrorCode::NotFound => ErrorCode::NotFound,
        HostOpErrorCode::InvalidRequest => ErrorCode::InvalidRequest,
        // Operational rather than a programming error: the run's own budget or
        // deadline ran out, or the bridge could not produce a trustworthy
        // result for an effect that may have happened.
        HostOpErrorCode::BudgetExceeded
        | HostOpErrorCode::DeadlineExceeded
        | HostOpErrorCode::UnknownResult => ErrorCode::DependencyUnavailable,
        // The op was cancelled under a run that is no longer running, so this
        // completion no longer applies.
        HostOpErrorCode::Cancelled => ErrorCode::Conflict,
        HostOpErrorCode::Failed | HostOpErrorCode::Internal => ErrorCode::Internal,
    }
}

/// The product capabilities behind the host ops, as this process provides them.
///
/// Only the ops the process can actually honour are implemented.  The rest
/// return a typed `capability_missing`: an empty result a loop could mistake for
/// a complete one is never an acceptable stand-in for a capability that is
/// absent.
///
/// This set is deliberately incomplete while the provider bridge, manifest item
/// iteration, publishing and measurement are unbuilt, so an assembly over it
/// alone is not [`EmbeddedAgentRuntime::configured`].
pub struct RepositoryHostOps {
    knowledge: Arc<dyn KnowledgeRepository>,
}

impl RepositoryHostOps {
    /// The ops that read product state, over the repositories the API process
    /// already holds.
    pub fn new(knowledge: Arc<dyn KnowledgeRepository>) -> Self {
        Self { knowledge }
    }
}

impl fmt::Debug for RepositoryHostOps {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryHostOps")
            .finish_non_exhaustive()
    }
}

/// Maps a domain failure onto the stable host-op vocabulary, so a loop branches
/// on the same classes it would get from any other bridge.
fn worker_error(op: HostOp, error: AppError) -> HostOpError {
    match error.code {
        ErrorCode::CapabilityMissing => HostOpError::capability_missing(op, error.message),
        ErrorCode::NotFound => HostOpError::not_found(op, error.message),
        ErrorCode::InvalidRequest => HostOpError::invalid_request(op, error.message),
        ErrorCode::Forbidden | ErrorCode::Unauthorized => HostOpError::denied(op, error.message),
        _ => HostOpError::failed(op, error.message),
    }
}

#[async_trait]
impl HostOps for RepositoryHostOps {
    async fn model_complete(
        &self,
        _scope: &TenantScope,
        _request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ModelComplete,
            "no model provider bridge is configured",
        ))
    }

    async fn knowledge_search(
        &self,
        scope: &TenantScope,
        request: geo_worker::KnowledgeSearchRequest,
    ) -> Result<geo_worker::KnowledgeSearchResult, HostOpError> {
        self.knowledge
            .search(scope, request)
            .await
            .map_err(|error| worker_error(HostOp::KnowledgeSearch, error))
    }

    async fn manifest_read(
        &self,
        _scope: &TenantScope,
        _request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        // The manifest header is stored, but its items are not readable yet, and
        // an empty item list would read as "this manifest has no work".
        Err(HostOpError::capability_missing(
            HostOp::ManifestRead,
            "manifest item iteration is not implemented yet",
        ))
    }

    async fn publish_submit(
        &self,
        _scope: &TenantScope,
        _request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Publish,
            "platform publishing is not implemented yet",
        ))
    }

    async fn measure_sample(
        &self,
        _scope: &TenantScope,
        _request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Measure,
            "independent channel measurement is not implemented yet",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_worker::HostOpErrorCode;

    /// The published codes are a vocabulary, not a pass-through: a repository
    /// refusal must arrive at a script as one of the classes the surface
    /// declares.
    #[test]
    fn domain_failures_map_onto_the_declared_host_op_codes() {
        let cases = [
            (
                AppError::capability_missing("no adapter"),
                HostOpErrorCode::CapabilityMissing,
            ),
            (
                AppError::not_found("no such source"),
                HostOpErrorCode::NotFound,
            ),
            (
                AppError::invalid_request("query must not be empty"),
                HostOpErrorCode::InvalidRequest,
            ),
            (AppError::forbidden("not a writer"), HostOpErrorCode::Denied),
            (
                AppError::unauthorized("no session"),
                HostOpErrorCode::Denied,
            ),
            (
                AppError::conflict("already sealed"),
                HostOpErrorCode::Failed,
            ),
            (
                AppError::not_ready("still importing"),
                HostOpErrorCode::Failed,
            ),
        ];
        for (error, expected) in cases {
            let mapped = worker_error(HostOp::KnowledgeSearch, error);
            assert_eq!(mapped.op, HostOp::KnowledgeSearch);
            assert_eq!(mapped.code, expected, "{mapped:?}");
        }
    }

    /// An unconfigured runtime reports the same reason the domain's own missing
    /// runtime reports, so a client cannot tell the two apart and neither can be
    /// mistaken for a working deployment.
    #[tokio::test]
    async fn an_unconfigured_runtime_matches_the_domain_default() {
        let ours = EmbeddedAgentRuntime::unconfigured().capability().await;
        let theirs = geo_domain::MissingAgentRuntime.capability().await;
        assert_eq!(ours, theirs);
        assert!(!ours.is_available());
        assert_eq!(ours.reason.as_deref(), Some(RUNTIME_NOT_CONFIGURED));
    }

    /// An unconfigured runtime cannot run a turn either, so a configuration
    /// that cannot execute work cannot accept it.
    #[tokio::test]
    async fn an_unconfigured_runtime_refuses_to_run_a_turn() {
        let scope = TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        );
        let error = EmbeddedAgentRuntime::unconfigured()
            .run_turn(&scope, turn_input())
            .await
            .expect_err("an unconfigured runtime must refuse to run a turn");
        assert_eq!(error.code, ErrorCode::CapabilityMissing);
        assert_eq!(error.message, RUNTIME_NOT_CONFIGURED);
    }

    /// The turn argument is the contract with the bundle, so it is asserted
    /// rather than left to whatever the request happened to carry.
    #[test]
    fn the_turn_argument_names_everything_a_bundle_needs() {
        let input = turn_input();
        let argument: Value = serde_json::from_str(&turn_argument(&input)).expect("valid JSON");
        assert_eq!(argument["prompt"], "how long is the warranty?");
        assert_eq!(argument["query"], "how long is the warranty?");
        assert_eq!(argument["run_id"], input.run_id.to_string().as_str());
        assert_eq!(argument["turn_id"], input.turn_id.to_string().as_str());
        assert_eq!(
            argument["conversation_id"],
            input.conversation_id.to_string().as_str()
        );
    }

    /// A thrown host-op failure keeps its declared class instead of degrading
    /// into an opaque internal error, so an unfinished provider bridge is
    /// recorded as the capability gap it is.
    #[test]
    fn a_thrown_host_op_failure_keeps_its_declared_class() {
        let thrown = WorkerError::new(
            "call",
            r#"Uncaught GeoHostOpError: {"code":"capability_missing","op":"model_complete","message":"no model provider bridge is configured"}"#,
        );
        let mapped = turn_failure(thrown);
        assert_eq!(mapped.code, ErrorCode::CapabilityMissing);

        let denied = WorkerError::new(
            "call",
            r#"Uncaught GeoHostOpError: {"code":"denied","op":"publish_submit","message":"not permitted"}"#,
        );
        assert_eq!(turn_failure(denied).code, ErrorCode::Forbidden);

        // A failure that never reached an op cannot claim a host-op class.
        let unloadable = WorkerError::new("evaluate", "SyntaxError: unexpected token");
        assert_eq!(turn_failure(unloadable).code, ErrorCode::Internal);

        let opaque = WorkerError::new("call", "Uncaught Error: turn exploded");
        assert_eq!(turn_failure(opaque).code, ErrorCode::Internal);
    }

    fn turn_input() -> TurnInput {
        TurnInput {
            conversation_id: geo_domain::ConversationId::from(uuid::Uuid::new_v4()),
            turn_id: geo_domain::TurnId::from(uuid::Uuid::new_v4()),
            run_id: geo_domain::RunId::from(uuid::Uuid::new_v4()),
            prompt: "how long is the warranty?".to_owned(),
        }
    }
}
