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
///
/// Checkpoints intentionally contain only the durable model-call counter and
/// emitted events. Output limits are trusted Rust-side policy, so they are
/// deliberately omitted from the serialised form and restored as defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostState {
    pub model_calls: u64,
    pub events: Vec<HostEvent>,
    #[serde(skip)]
    output_limits: HostOutputLimits,
    /// A cache of bytes held by `events`. It is not checkpointed: the first
    /// post-restore emit rebuilds it from the durable event list, then later
    /// emits update it incrementally instead of repeatedly summing all events.
    #[serde(skip)]
    output_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEvent {
    pub topic: String,
    pub payload: String,
}

/// The maximum amount of output one isolate may retain by way of `op_host_emit`.
///
/// These limits are Rust-owned policy rather than checkpoint state: a
/// checkpoint must not be able to loosen them. In particular, `max_events`
/// bounds empty events, whose byte cost alone would otherwise be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostOutputLimits {
    max_event_bytes: usize,
    max_total_event_bytes: usize,
    max_events: usize,
}

impl Default for HostOutputLimits {
    fn default() -> Self {
        Self {
            max_event_bytes: HostState::DEFAULT_MAX_EVENT_BYTES,
            max_total_event_bytes: HostState::DEFAULT_MAX_TOTAL_EVENT_BYTES,
            max_events: HostState::DEFAULT_MAX_EVENTS,
        }
    }
}

impl HostState {
    /// A single event may retain at most 64 KiB of UTF-8 topic and payload
    /// bytes by default.
    pub const DEFAULT_MAX_EVENT_BYTES: usize = 64 * 1024;
    /// Events collectively retain at most 1 MiB of UTF-8 topic and payload
    /// bytes by default.
    pub const DEFAULT_MAX_TOTAL_EVENT_BYTES: usize = 1024 * 1024;
    /// The event count is independently capped so empty events cannot grow the
    /// host state without bound.
    pub const DEFAULT_MAX_EVENTS: usize = 1024;

    /// Builds state with a Rust-selected output policy.
    ///
    /// The limits are not serialised into checkpoints. A resumed checkpoint
    /// therefore uses [`Self::default`]'s policy, while retaining its already
    /// emitted events as spent output.
    pub fn with_output_limits(
        max_event_bytes: usize,
        max_total_event_bytes: usize,
        max_events: usize,
    ) -> Self {
        Self {
            model_calls: 0,
            events: Vec::new(),
            output_limits: HostOutputLimits {
                max_event_bytes,
                max_total_event_bytes,
                max_events,
            },
            output_bytes: Some(0),
        }
    }

    fn emit(&mut self, topic: String, payload: String) -> Result<(), JsErrorBox> {
        let event_bytes = topic
            .len()
            .checked_add(payload.len())
            .ok_or_else(|| output_budget_error("event byte count overflowed"))?;
        if event_bytes > self.output_limits.max_event_bytes {
            return Err(output_budget_error(format!(
                "event is {event_bytes} bytes, above the {} byte per-event limit",
                self.output_limits.max_event_bytes
            )));
        }
        if self.events.len() >= self.output_limits.max_events {
            return Err(output_budget_error(format!(
                "event count would exceed the {} event limit",
                self.output_limits.max_events
            )));
        }

        let output_bytes = self.output_bytes()?;
        let total_bytes = output_bytes
            .checked_add(event_bytes)
            .ok_or_else(|| output_budget_error("total event byte count overflowed"))?;
        if total_bytes > self.output_limits.max_total_event_bytes {
            return Err(output_budget_error(format!(
                "total event output would be {total_bytes} bytes, above the {} byte limit",
                self.output_limits.max_total_event_bytes
            )));
        }

        self.events.push(HostEvent { topic, payload });
        self.output_bytes = Some(total_bytes);
        Ok(())
    }

    /// Rehydrates the non-serialised output counter after checkpoint restore.
    ///
    /// Once it is rebuilt, each subsequent emit updates it in constant time.
    fn output_bytes(&mut self) -> Result<usize, JsErrorBox> {
        if let Some(output_bytes) = self.output_bytes {
            return Ok(output_bytes);
        }

        let output_bytes = self.events.iter().try_fold(0usize, |total, event| {
            let event_bytes = event
                .topic
                .len()
                .checked_add(event.payload.len())
                .ok_or_else(|| output_budget_error("restored event byte count overflowed"))?;
            total
                .checked_add(event_bytes)
                .ok_or_else(|| output_budget_error("restored total event byte count overflowed"))
        })?;
        self.output_bytes = Some(output_bytes);
        Ok(output_bytes)
    }
}

impl Default for HostState {
    fn default() -> Self {
        Self::with_output_limits(
            Self::DEFAULT_MAX_EVENT_BYTES,
            Self::DEFAULT_MAX_TOTAL_EVENT_BYTES,
            Self::DEFAULT_MAX_EVENTS,
        )
    }
}

impl PartialEq for HostState {
    fn eq(&self, other: &Self) -> bool {
        self.model_calls == other.model_calls
            && self.events == other.events
            && self.output_limits == other.output_limits
    }
}

impl Eq for HostState {}

fn output_budget_error(message: impl std::fmt::Display) -> JsErrorBox {
    // RangeError is a built-in JS error class, so deno_core preserves this
    // typed failure across the Rust/JavaScript boundary.
    JsErrorBox::range_error(format!("host output budget exceeded: {message}"))
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
    host.emit(topic.clone(), payload)?;
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

#[cfg(test)]
mod tests {
    use deno_core::{JsRuntime, RuntimeOptions};

    use super::{HostEvent, HostState, op_host_emit};

    deno_core::extension!(geo_host_output_budget_test, ops = [op_host_emit],);

    fn runtime(state: HostState) -> JsRuntime {
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![geo_host_output_budget_test::init()],
            ..Default::default()
        });
        runtime.op_state().borrow_mut().put(state);
        runtime
    }

    #[test]
    fn emit_records_a_normal_event_within_the_rust_output_budget() {
        let mut runtime = runtime(HostState::with_output_limits(32, 64, 2));
        runtime
            .execute_script(
                "normal-emit.js",
                r#"Deno.core.ops.op_host_emit("progress", "ok");"#,
            )
            .expect("an event below every limit must be accepted");

        let state = runtime
            .op_state()
            .borrow()
            .try_borrow::<HostState>()
            .cloned()
            .expect("host state must remain installed");
        assert_eq!(
            state.events,
            vec![HostEvent {
                topic: "progress".to_owned(),
                payload: "ok".to_owned(),
            }]
        );
    }

    #[test]
    fn emit_rejects_an_oversized_event_with_a_typed_error_without_mutating_state() {
        let mut runtime = runtime(HostState::with_output_limits(8, 64, 2));
        runtime
            .execute_script(
                "oversized-emit.js",
                r#"
                let name;
                try {
                  Deno.core.ops.op_host_emit("topic", "1234");
                } catch (error) {
                  name = error.name;
                }
                if (name !== "RangeError") {
                  throw new Error(`expected a RangeError, got ${name}`);
                }
                "#,
            )
            .expect("the script must be able to catch the typed rejection");

        let state = runtime
            .op_state()
            .borrow()
            .try_borrow::<HostState>()
            .cloned()
            .expect("host state must remain installed");
        assert!(
            state.events.is_empty(),
            "a rejected event must not be retained: {state:?}"
        );
    }

    #[test]
    fn emit_rejects_total_bytes_and_event_count_without_mutating_state() {
        let mut byte_limited = runtime(HostState::with_output_limits(8, 9, 2));
        byte_limited
            .execute_script(
                "total-budget.js",
                r#"
                Deno.core.ops.op_host_emit("a", "1234");
                let name;
                try {
                  Deno.core.ops.op_host_emit("b", "1234");
                } catch (error) {
                  name = error.name;
                }
                if (name !== "RangeError") {
                  throw new Error(`expected a RangeError, got ${name}`);
                }
                "#,
            )
            .expect("the script must be able to catch the total-budget rejection");
        assert_eq!(
            byte_limited
                .op_state()
                .borrow()
                .try_borrow::<HostState>()
                .expect("host state must remain installed")
                .events
                .len(),
            1,
            "a rejected event must not consume total output budget"
        );

        let mut count_limited = runtime(HostState::with_output_limits(8, 16, 1));
        count_limited
            .execute_script(
                "event-count-budget.js",
                r#"
                Deno.core.ops.op_host_emit("", "");
                let name;
                try {
                  Deno.core.ops.op_host_emit("", "");
                } catch (error) {
                  name = error.name;
                }
                if (name !== "RangeError") {
                  throw new Error(`expected a RangeError, got ${name}`);
                }
                "#,
            )
            .expect("the script must be able to catch the count-budget rejection");
        assert_eq!(
            count_limited
                .op_state()
                .borrow()
                .try_borrow::<HostState>()
                .expect("host state must remain installed")
                .events
                .len(),
            1,
            "empty events must still be bounded by count"
        );
    }

    #[test]
    fn legacy_checkpoints_restore_default_policy_and_preserve_spent_bytes() {
        let legacy_checkpoint = r#"{
            "model_calls": 3,
            "events": [{"topic": "a", "payload": "😀"}]
        }"#;
        let mut state: HostState =
            serde_json::from_str(legacy_checkpoint).expect("legacy checkpoint must deserialize");
        state.output_limits.max_event_bytes = 8;
        state.output_limits.max_total_event_bytes = 9;
        state.output_limits.max_events = 2;

        let error = state
            .emit("b".to_owned(), "1234".to_owned())
            .expect_err("the restored event's UTF-8 bytes must count against the total");
        assert!(
            error.to_string().contains("total event output"),
            "unexpected error: {error}"
        );
        assert_eq!(
            state.events,
            vec![HostEvent {
                topic: "a".to_owned(),
                payload: "😀".to_owned(),
            }],
            "a rejected post-restore event must not mutate the checkpoint state"
        );

        let configured = HostState::with_output_limits(1, 1, 1);
        let checkpoint = serde_json::to_value(configured).expect("state must serialize");
        assert!(
            checkpoint.get("output_limits").is_none() && checkpoint.get("output_bytes").is_none(),
            "checkpoint must not carry Rust-side policy or its cache: {checkpoint}"
        );
        let restored: HostState =
            serde_json::from_value(checkpoint).expect("checkpoint must deserialize");
        assert_eq!(
            restored.output_limits,
            super::HostOutputLimits::default(),
            "a checkpoint must restore the default trusted output policy"
        );
    }
}
