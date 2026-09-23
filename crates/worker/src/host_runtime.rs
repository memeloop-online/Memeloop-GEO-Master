//! The production worker runtime: the isolate configured with the declared
//! host-op surface and nothing else.
//!
//! This is the same engine and the same deadline/cancellation/cancellation
//! machinery as the compatibility probe; what differs is the op set.  The
//! probe-only model stub is **not** registered here, so there is no code path
//! through which a tenant script could obtain a synthesised completion.  A
//! capability that has no bridge fails with a typed error instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::host::{HOST_OP_ERROR_BOOTSTRAP, HOST_OPS_VERSION, HostBridge, HostOp, HostOpBudgets};
use crate::host_ops::{
    PRODUCTION_OP_NAMES, op_host_knowledge_search_v1, op_host_manifest_read_v1,
    op_host_measure_sample_v1, op_host_model_complete_v1, op_host_publish_submit_v1,
};
use crate::ops::{HostState, op_host_checkpoint, op_host_emit};
use crate::runtime::{EmbeddedIsolate, WorkerError};

deno_core::extension!(
    geo_host_ops,
    ops = [
        op_host_model_complete_v1,
        op_host_knowledge_search_v1,
        op_host_manifest_read_v1,
        op_host_publish_submit_v1,
        op_host_measure_sample_v1,
        op_host_emit,
        op_host_checkpoint
    ],
);

/// The name the failure-class bootstrap is executed under, so a stack trace
/// from it names the extension rather than a bundle module.
const HOST_OP_ERROR_SCRIPT: &str = "ext:geo_host_ops/error-class.js";

/// One isolated run: an approved bundle, a bridged host-op surface, and the
/// Rust-owned state the loop checkpoints.
///
/// The isolate is not `Send`: it is created, driven and dropped on one worker
/// thread, which is also why the API process never executes tenant JavaScript
/// inline.
pub struct HostRuntime {
    isolate: EmbeddedIsolate,
    bridge: HostBridge,
}

impl HostRuntime {
    /// Builds a production runtime over an explicit, approved bundle.
    ///
    /// Fails only if the isolate cannot be prepared for scripts at all — in
    /// particular if the failure class a host op reports cannot be registered,
    /// because a runtime whose op failures arrive as an unrelated `TypeError`
    /// would misreport every capability gap.
    pub fn new(
        bundle: &[(&str, &str)],
        bridge: HostBridge,
        heap_limit_bytes: Option<usize>,
    ) -> Result<Self, WorkerError> {
        let mut isolate =
            EmbeddedIsolate::new(vec![geo_host_ops::init()], bundle, heap_limit_bytes);
        isolate.install(HostState::default());
        isolate.install(bridge.clone());
        isolate.execute_script(HOST_OP_ERROR_SCRIPT, HOST_OP_ERROR_BOOTSTRAP)?;
        Ok(Self { isolate, bridge })
    }

    /// Builds a production runtime with the crate's reference bundle and the
    /// given capabilities, with the default budgets.
    pub fn with_default_bundle(
        bridge: HostBridge,
        heap_limit_bytes: Option<usize>,
    ) -> Result<Self, WorkerError> {
        Self::new(crate::bundle::HOST_BUNDLE, bridge, heap_limit_bytes)
    }

    /// The declared, JS-visible op names this runtime registers.
    pub fn op_surface() -> Vec<String> {
        let mut names = PRODUCTION_OP_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// The version of the host-op surface this runtime implements.
    pub fn host_ops_version(&self) -> &'static str {
        HOST_OPS_VERSION
    }

    /// The bridge backing every op, including this run's scope and budgets.
    pub fn bridge(&self) -> &HostBridge {
        &self.bridge
    }

    /// The run's cancellation flag.  Raising it fails in-flight ops with a
    /// typed `cancelled` error.
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.bridge.cancellation()
    }

    /// Cancels the run's in-flight and future host ops.
    pub fn cancel(&self) {
        self.bridge.cancellation().store(true, Ordering::SeqCst);
    }

    /// Invocations attempted for `op` in this run.
    pub fn op_calls(&self, op: HostOp) -> u32 {
        self.bridge.meter().calls(op)
    }

    pub fn budgets(&self) -> HostOpBudgets {
        self.bridge.budgets()
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

    /// Evaluates the entry module and calls its `main` export once with
    /// `argument_json`, terminating execution if the turn runs past `deadline`.
    ///
    /// This is the production turn: one call, one run.  The turn's result
    /// arrives through [`Self::host_state`], not through the return value, so a
    /// turn that returned without reporting anything is a failure the caller
    /// can see rather than a silently empty answer.
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

impl std::fmt::Debug for HostRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostRuntime")
            .field("version", &HOST_OPS_VERSION)
            .field("bridge", &self.bridge)
            .field("allowlisted", &self.isolate.allowlisted_specifiers())
            .finish_non_exhaustive()
    }
}
