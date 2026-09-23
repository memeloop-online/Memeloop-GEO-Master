//! Host ops: the only way JavaScript reaches the outside world.
//!
//! Every op below is deliberately narrow.  There is no op for SQL, arbitrary
//! network access, files, processes or environment variables, so a tenant
//! script cannot obtain those capabilities by construction.  That is the
//! boundary the product spec requires of the Rust-hosted runtime.

use deno_core::OpState;
use deno_core::op2;
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};

/// Rust-owned state that JavaScript can reach only through the ops below.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostState {
    pub model_calls: u64,
    pub events: Vec<HostEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEvent {
    pub topic: String,
    pub payload: String,
}

fn host_state(state: &mut OpState) -> Result<&mut HostState, JsErrorBox> {
    state
        .try_borrow_mut::<HostState>()
        .ok_or_else(|| JsErrorBox::generic("host state is not installed in this runtime"))
}

/// A deterministic stand-in for a model provider call.
///
/// The probe performs no network I/O.  A production worker replaces the body
/// with the Rust-side provider bridge while the JS-visible contract, and
/// therefore the loop script, stays unchanged.
#[op2]
#[string]
pub fn op_host_model_complete(
    state: &mut OpState,
    #[string] prompt: String,
) -> Result<String, JsErrorBox> {
    let host = host_state(state)?;
    host.model_calls += 1;
    Ok(format!("stub-completion:{}", prompt.trim()))
}

/// Records a structured event emitted by the loop script.
///
/// This mirrors the upstream `runtime.emit` contract: the payload is carried
/// as a serialised JSON string so the Rust side can persist it verbatim.
#[op2]
#[string]
pub fn op_host_emit(
    state: &mut OpState,
    #[string] topic: String,
    #[string] payload: String,
) -> Result<String, JsErrorBox> {
    let host = host_state(state)?;
    host.events.push(HostEvent {
        topic: topic.clone(),
        payload,
    });
    Ok(topic)
}

/// Reads the model-call counter back, proving state survives across JS calls.
#[op2(fast)]
pub fn op_host_model_call_count(state: &mut OpState) -> Result<u32, JsErrorBox> {
    let host = host_state(state)?;
    Ok(host.model_calls as u32)
}

/// Serialises the Rust-owned state; this is what the checkpoint probe stores.
#[op2]
#[string]
pub fn op_host_checkpoint(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = host_state(state)?;
    serde_json::to_string(host).map_err(|error| JsErrorBox::generic(error.to_string()))
}
