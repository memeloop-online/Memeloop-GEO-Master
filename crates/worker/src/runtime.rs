//! The embedded isolate: a `JsRuntime` bound to an allow-listed in-memory
//! bundle, a wall-clock deadline and an optional heap cap.
//!
//! One engine, two configurations.  The compatibility probe ([`ProbeRuntime`])
//! and the production worker ([`crate::HostRuntime`]) differ only in which
//! extension they install and which host state they seed; the deadline,
//! cancellation, heap-guard and checkpoint behaviour is shared here so the two
//! cannot drift apart.
//!
//! Authoritative usage for every V8-level facility here is taken from
//! `deno_core` 0.412.0's own test-suite (`runtime/tests/misc.rs`:
//! `terminate_execution`, `test_heap_limits`) rather than from memory.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use deno_core::{
    Extension, JsRuntime, ModuleId, ModuleSpecifier, PollEventLoopOptions, RuntimeOptions,
};
use serde::{Deserialize, Serialize};

use crate::bundle::PROBE_BUNDLE;
use crate::loader::InMemoryModuleLoader;
use crate::ops::{
    HostState, op_host_checkpoint, op_host_emit, op_host_model_call_count, op_host_model_complete,
};

deno_core::extension!(
    geo_probe,
    ops = [
        op_host_model_complete,
        op_host_emit,
        op_host_model_call_count,
        op_host_checkpoint
    ],
);

/// Terminates the isolate if the stage it guards has not finished by its
/// deadline.
///
/// A Tokio task rather than a thread: module evaluation and host calls are
/// driven through the async event loop, so the timer can run concurrently with
/// the isolate instead of being blocked behind it.
struct DeadlineWatchdog {
    finished: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl DeadlineWatchdog {
    fn start(runtime: &mut JsRuntime, deadline: Duration) -> Self {
        let finished = Arc::new(AtomicBool::new(false));
        let handle = runtime.v8_isolate().thread_safe_handle();
        let watchdog_finished = Arc::clone(&finished);
        let task = tokio::spawn(async move {
            tokio::time::sleep(deadline).await;
            if !watchdog_finished.load(Ordering::SeqCst) {
                handle.terminate_execution();
            }
        });
        Self { finished, task }
    }

    /// Disarms the watchdog.  Called on every path out of the guarded stage so
    /// a slow caller cannot terminate an isolate that is already idle.
    fn disarm(self) {
        self.finished.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}

/// A runtime failure, kept structured so it can be written verbatim into the
/// work log as evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerError {
    pub stage: String,
    pub message: String,
}

impl WorkerError {
    pub fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.stage, self.message)
    }
}

impl std::error::Error for WorkerError {}

/// A Rust-hosted isolate.  It owns the allow-listed bundle and the host state
/// that scripts may reach only through host ops.
pub struct EmbeddedIsolate {
    runtime: JsRuntime,
    loader: InMemoryModuleLoader,
}

impl EmbeddedIsolate {
    /// Builds an isolate over an explicit extension set and bundle, optionally
    /// capped at `heap_limit_bytes` of V8 heap.
    pub(crate) fn new(
        extensions: Vec<Extension>,
        bundle: &[(&str, &str)],
        heap_limit_bytes: Option<usize>,
    ) -> Self {
        let loader = InMemoryModuleLoader::from_static(bundle);
        let create_params = heap_limit_bytes
            .map(|limit| deno_core::v8::Isolate::create_params().heap_limits(0, limit));
        let runtime = JsRuntime::new(RuntimeOptions {
            module_loader: Some(Rc::new(loader.clone())),
            extensions,
            create_params,
            ..Default::default()
        });
        Self { runtime, loader }
    }

    /// Installs a Rust-owned value — the probe's host state or the production
    /// host-op bridge — into the isolate's op state.
    pub(crate) fn install<T: Clone + 'static>(&mut self, value: T) {
        self.runtime.op_state().borrow_mut().put(value);
    }

    /// The specifiers this isolate is willing to serve.
    pub fn allowlisted_specifiers(&self) -> Vec<String> {
        self.loader.specifiers()
    }

    pub fn thread_safe_handle(&mut self) -> deno_core::v8::IsolateHandle {
        self.runtime.v8_isolate().thread_safe_handle()
    }

    /// Loads and evaluates an ES module, terminating execution if it runs past
    /// `deadline`.
    pub async fn evaluate_module(
        &mut self,
        specifier: &str,
        deadline: Duration,
    ) -> Result<(), WorkerError> {
        let watchdog = DeadlineWatchdog::start(&mut self.runtime, deadline);
        let evaluated = self.load_and_evaluate(specifier).await;
        watchdog.disarm();
        evaluated.map(|_| ())
    }

    /// Evaluates the entry module and then calls its `main` export once with
    /// the argument the host built for this turn, terminating execution if the
    /// whole thing runs past `deadline`.
    ///
    /// Evaluating a module is deliberately *not* a turn.  The bundle's contract
    /// is that the host announces itself at module scope and then calls `main`
    /// once per turn; because `deno_core` caches modules per specifier, a second
    /// evaluation of the same isolate would re-run nothing.  One isolate serves
    /// exactly one turn, so module-level state — including the `loop.ready`
    /// announcement — is per-turn rather than per-conversation.
    ///
    /// The result is whatever the script reported through `op_host_emit` into
    /// [`Self::host_state`]; that is the bundle's one channel, so the value
    /// `main` returns is discarded rather than becoming a second, competing
    /// source of truth.  A resolved `main` that emitted no completion is a
    /// typed failure, not a success.
    pub async fn call_main(
        &mut self,
        specifier: &str,
        argument_json: &str,
        deadline: Duration,
    ) -> Result<(), WorkerError> {
        let watchdog = DeadlineWatchdog::start(&mut self.runtime, deadline);
        let called = self.evaluate_and_call_main(specifier, argument_json).await;
        watchdog.disarm();
        called
    }

    async fn evaluate_and_call_main(
        &mut self,
        specifier: &str,
        argument_json: &str,
    ) -> Result<(), WorkerError> {
        let module_id = self.load_and_evaluate(specifier).await?;
        let main = self.main_function(module_id)?;
        let argument = self.json_argument(argument_json)?;

        // The call and the event loop have to be polled together: awaiting the
        // event loop first can report it ready while the promise is still
        // pending, and then nothing would ever wake the call future again.
        let call = self.runtime.call_with_args(&main, &[argument]);
        self.runtime
            .with_event_loop_promise(call, PollEventLoopOptions::default())
            .await
            .map_err(|error| WorkerError::new("call", error.to_string()))?;
        Ok(())
    }

    /// Loads, instantiates and evaluates one allow-listed entry module, driving
    /// the event loop so its top-level await settles.
    ///
    /// Polling the event loop before awaiting the evaluation future is safe
    /// here and only here: a pending module evaluation is itself tracked by the
    /// event loop, so the loop cannot report itself resolved while one is
    /// outstanding.
    async fn load_and_evaluate(&mut self, specifier: &str) -> Result<ModuleId, WorkerError> {
        let module_specifier = ModuleSpecifier::parse(specifier)
            .map_err(|error| WorkerError::new("resolve", error.to_string()))?;
        let module_id = self
            .runtime
            .load_main_es_module(&module_specifier)
            .await
            .map_err(|error| WorkerError::new("load", error.to_string()))?;

        let evaluation = self.runtime.mod_evaluate(module_id);
        self.runtime
            .run_event_loop(Default::default())
            .await
            .map_err(|error| WorkerError::new("event_loop", error.to_string()))?;
        evaluation
            .await
            .map_err(|error| WorkerError::new("evaluate", error.to_string()))?;
        Ok(module_id)
    }

    /// Reads the entry module's `main` export.
    ///
    /// A module namespace reports an export that does not exist as `undefined`
    /// rather than as absence, so a module that never declared `main` and one
    /// that set it to `undefined` both have to be rejected explicitly instead
    /// of being handed to `try_from`.
    fn main_function(
        &mut self,
        module_id: ModuleId,
    ) -> Result<deno_core::v8::Global<deno_core::v8::Function>, WorkerError> {
        let namespace = self
            .runtime
            .get_module_namespace(module_id)
            .map_err(|error| WorkerError::new("namespace", error.to_string()))?;

        deno_core::scope!(scope, self.runtime);
        let namespace = deno_core::v8::Local::<deno_core::v8::Object>::new(scope, namespace);
        let key = deno_core::v8::String::new(scope, "main")
            .ok_or_else(|| WorkerError::new("namespace", "the export name cannot be built"))?;
        let value = namespace
            .get(scope, key.into())
            .ok_or_else(|| WorkerError::new("namespace", "the export cannot be read"))?;
        if value.is_undefined() {
            return Err(WorkerError::new(
                "namespace",
                "the entry module does not export `main`",
            ));
        }
        let function =
            deno_core::v8::Local::<deno_core::v8::Function>::try_from(value).map_err(|_| {
                WorkerError::new("namespace", "the entry module's `main` is not a function")
            })?;
        Ok(deno_core::v8::Global::new(scope, function))
    }

    /// Builds the single argument a turn is called with.
    fn json_argument(
        &mut self,
        argument_json: &str,
    ) -> Result<deno_core::v8::Global<deno_core::v8::Value>, WorkerError> {
        let value: serde_json::Value = serde_json::from_str(argument_json)
            .map_err(|error| WorkerError::new("argument", error.to_string()))?;
        deno_core::scope!(scope, self.runtime);
        let local = deno_core::serde_v8::to_v8(scope, value)
            .map_err(|error| WorkerError::new("argument", error.to_string()))?;
        Ok(deno_core::v8::Global::new(scope, local))
    }

    /// Executes a classic script.
    ///
    /// `execute_script` blocks the calling thread, so a Tokio timer could not
    /// fire; deadline enforcement uses a polling thread instead.
    pub fn execute_script(&mut self, name: &str, source: &str) -> Result<(), WorkerError> {
        self.runtime
            .execute_script(name.to_owned(), source.to_owned())
            .map(|_| ())
            .map_err(|error| WorkerError::new("script", exception_message(&error)))
    }

    /// Executes a classic script, terminating it after `deadline` elapsed.
    pub fn execute_script_with_deadline(
        &mut self,
        name: &str,
        source: &str,
        deadline: Duration,
    ) -> Result<(), WorkerError> {
        let finished = Arc::new(AtomicBool::new(false));
        let handle = self.runtime.v8_isolate().thread_safe_handle();
        let watchdog_finished = Arc::clone(&finished);
        let poll = Duration::from_millis(10);
        let watchdog = std::thread::spawn(move || {
            let mut waited = Duration::ZERO;
            while waited < deadline {
                if watchdog_finished.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(poll);
                waited += poll;
            }
            if !watchdog_finished.load(Ordering::SeqCst) {
                handle.terminate_execution();
            }
        });

        let result = self
            .runtime
            .execute_script(name.to_owned(), source.to_owned());
        finished.store(true, Ordering::SeqCst);
        let _ = watchdog.join();

        result
            .map(|_| ())
            .map_err(|error| WorkerError::new("script", exception_message(&error)))
    }

    /// Clears a pending terminating exception so the isolate is usable again.
    pub fn cancel_terminate_execution(&mut self) -> bool {
        self.runtime.v8_isolate().cancel_terminate_execution()
    }

    /// Terminates execution immediately and calls `fired` when V8 reports that
    /// the isolate is approaching `heap_limit_bytes`.
    ///
    /// Following the upstream test, the callback both raises the limit (so V8
    /// can unwind instead of aborting the process) and terminates execution,
    /// turning an out-of-memory crash into a recoverable error.
    pub fn install_heap_limit_guard(&mut self, fired: Arc<AtomicBool>) {
        let handle = self.runtime.v8_isolate().thread_safe_handle();
        self.runtime
            .add_near_heap_limit_callback(move |current_limit, _initial_limit| {
                fired.store(true, Ordering::SeqCst);
                handle.terminate_execution();
                current_limit * 2
            });
    }

    /// A snapshot of the Rust-owned host state.
    pub fn host_state(&self) -> HostState {
        let op_state = self.runtime.op_state();
        let borrowed = op_state.borrow();
        borrowed
            .try_borrow::<HostState>()
            .cloned()
            .unwrap_or_default()
    }

    /// Serialises the Rust-owned state.
    ///
    /// Checkpoint/resume is deliberately implemented at the worker-state level
    /// rather than as a V8 heap snapshot: the state that must survive a restart
    /// is the turn/loop state Rust already owns, and it stays inspectable,
    /// migratable and tenant-scoped.
    pub fn checkpoint(&self) -> Result<String, WorkerError> {
        let op_state = self.runtime.op_state();
        let borrowed = op_state.borrow();
        let host = borrowed
            .try_borrow::<HostState>()
            .ok_or_else(|| WorkerError::new("checkpoint", "host state is not installed"))?;
        serde_json::to_string(host)
            .map_err(|error| WorkerError::new("checkpoint", error.to_string()))
    }

    /// Restores a previously serialised host state into this isolate.
    pub fn restore_checkpoint(&mut self, checkpoint: &str) -> Result<(), WorkerError> {
        let state: HostState = serde_json::from_str(checkpoint)
            .map_err(|error| WorkerError::new("restore", error.to_string()))?;
        self.runtime.op_state().borrow_mut().put(state);
        Ok(())
    }
}

impl std::fmt::Debug for EmbeddedIsolate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddedIsolate")
            .field("allowlisted", &self.loader.specifiers())
            .finish_non_exhaustive()
    }
}

/// The W00 compatibility probe: the isolate configured with the probe-only op
/// set, including the deterministic model stub the production runtime
/// deliberately does not register.
pub struct ProbeRuntime {
    isolate: EmbeddedIsolate,
}

impl ProbeRuntime {
    /// Builds a runtime on the built-in probe bundle with no heap cap.
    pub fn new() -> Self {
        Self::with_bundle(PROBE_BUNDLE, None)
    }

    /// Builds a runtime over an explicit bundle, optionally capped at
    /// `heap_limit_bytes` of V8 heap.
    pub fn with_bundle(bundle: &[(&str, &str)], heap_limit_bytes: Option<usize>) -> Self {
        let mut isolate = EmbeddedIsolate::new(vec![geo_probe::init()], bundle, heap_limit_bytes);
        isolate.install(HostState::default());
        Self { isolate }
    }

    /// The specifiers this runtime is willing to serve.
    pub fn allowlisted_specifiers(&self) -> Vec<String> {
        self.isolate.allowlisted_specifiers()
    }

    pub fn thread_safe_handle(&mut self) -> deno_core::v8::IsolateHandle {
        self.isolate.thread_safe_handle()
    }

    /// Loads and evaluates an ES module, terminating execution if it runs past
    /// `deadline`.
    pub async fn evaluate_module(
        &mut self,
        specifier: &str,
        deadline: Duration,
    ) -> Result<(), WorkerError> {
        self.isolate.evaluate_module(specifier, deadline).await
    }

    /// Executes a classic script.
    pub fn execute_script(&mut self, name: &str, source: &str) -> Result<(), WorkerError> {
        self.isolate.execute_script(name, source)
    }

    /// Evaluates the entry module and calls its `main` export once.
    pub async fn call_main(
        &mut self,
        specifier: &str,
        argument_json: &str,
        deadline: Duration,
    ) -> Result<(), WorkerError> {
        self.isolate
            .call_main(specifier, argument_json, deadline)
            .await
    }

    /// Executes a classic script, terminating it after `deadline` elapsed.
    pub fn execute_script_with_deadline(
        &mut self,
        name: &str,
        source: &str,
        deadline: Duration,
    ) -> Result<(), WorkerError> {
        self.isolate
            .execute_script_with_deadline(name, source, deadline)
    }

    /// Clears a pending terminating exception so the isolate is usable again.
    pub fn cancel_terminate_execution(&mut self) -> bool {
        self.isolate.cancel_terminate_execution()
    }

    /// Terminates execution when V8 reports that the heap cap is near.
    pub fn install_heap_limit_guard(&mut self, fired: Arc<AtomicBool>) {
        self.isolate.install_heap_limit_guard(fired);
    }

    /// A snapshot of the Rust-owned host state.
    pub fn host_state(&self) -> HostState {
        self.isolate.host_state()
    }

    /// Serialises the Rust-owned state.
    pub fn checkpoint(&self) -> Result<String, WorkerError> {
        self.isolate.checkpoint()
    }

    /// Restores a previously serialised host state into this runtime.
    pub fn restore_checkpoint(&mut self, checkpoint: &str) -> Result<(), WorkerError> {
        self.isolate.restore_checkpoint(checkpoint)
    }
}

impl Default for ProbeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ProbeRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProbeRuntime")
            .field("allowlisted", &self.isolate.allowlisted_specifiers())
            .finish_non_exhaustive()
    }
}

/// Prefers V8's exception message, which is the stable, user-visible text
/// asserted against in the upstream tests.
fn exception_message(error: &deno_core::error::JsError) -> String {
    if error.exception_message.is_empty() {
        error.to_string()
    } else {
        error.exception_message.clone()
    }
}
