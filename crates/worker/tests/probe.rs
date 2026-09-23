//! The W00 compatibility probe.
//!
//! One test per runtime concern the worker depends on.  Each asserts behaviour
//! that the production worker would rely on, so a failure here is a decision
//! point about the engine, not a flaky expectation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use geo_worker::{MAIN_MODULE, PROBE_BUNDLE, ProbeRuntime};

const GENEROUS_DEADLINE: Duration = Duration::from_secs(30);

/// ESM: a bundle split across two modules resolves and evaluates, and only
/// allow-listed specifiers are reachable.
#[tokio::test]
async fn loads_esm_bundle_across_modules() {
    let mut probe = ProbeRuntime::new();
    assert_eq!(
        probe.allowlisted_specifiers(),
        vec![
            "memeloop://bundle/geo-loop.js".to_owned(),
            "memeloop://bundle/loop-core.js".to_owned(),
        ]
    );

    probe
        .evaluate_module(MAIN_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the allow-listed bundle must evaluate");

    // Reaching an unlisted module is refused rather than silently fetched.
    let error = probe
        .evaluate_module("memeloop://bundle/not-approved.js", GENEROUS_DEADLINE)
        .await
        .expect_err("an unlisted module must not load");
    assert_eq!(error.stage, "load");
    assert!(
        error.message.contains("not part of the approved bundle"),
        "unexpected error: {error:?}"
    );
}

/// Promise + top-level await: the entry module blocks on an awaited loop and
/// still reports completion through a host op.
#[tokio::test]
async fn drives_promises_and_top_level_await() {
    let mut probe = ProbeRuntime::new();
    probe
        .evaluate_module(MAIN_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("module with top-level await must complete");

    let state = probe.host_state();
    let completion = state
        .events
        .iter()
        .find(|event| event.topic == "loop.completed")
        .expect("the loop must report completion through a host op");

    let payload: serde_json::Value =
        serde_json::from_str(&completion.payload).expect("payload must be JSON");
    assert_eq!(payload["turnId"], "turn-probe-1");
    assert_eq!(
        payload["answer"],
        "stub-completion:how long is the warranty?"
    );
    // The tool ran after the awaited model call, so both promises resolved in
    // order rather than racing.
    assert_eq!(payload["toolCalls"][0]["name"], "knowledge.search");
}

/// Host ops: JavaScript can mutate Rust-owned state only through the ops, and
/// the counter survives across separate JS calls.
#[tokio::test]
async fn calls_host_ops_and_keeps_state_in_rust() {
    let mut probe = ProbeRuntime::new();
    probe
        .evaluate_module(MAIN_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("bundle must evaluate");

    assert_eq!(probe.host_state().model_calls, 1);

    probe
        .execute_script(
            "probe.js",
            "Deno.core.ops.op_host_model_complete('second call');",
        )
        .expect("a second model call must succeed");
    assert_eq!(probe.host_state().model_calls, 2);
}

/// Timeout: a non-terminating script is stopped at the deadline instead of
/// hanging the worker.
#[tokio::test]
async fn enforces_wall_clock_deadline() {
    let mut probe = ProbeRuntime::new();
    let started = Instant::now();
    let error = probe
        .execute_script_with_deadline("spin.js", "for(;;) {}", Duration::from_millis(300))
        .expect_err("an unbounded script must be terminated");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline must actually stop execution, took {elapsed:?}"
    );
    assert!(
        error.message.contains("terminated"),
        "unexpected error: {error:?}"
    );
}

/// Cancellation: an external terminator stops a running isolate, and clearing
/// the terminating exception leaves the isolate reusable.
#[tokio::test(flavor = "multi_thread")]
async fn cancellation_leaves_isolate_reusable() {
    let mut probe = ProbeRuntime::new();
    let handle = probe.thread_safe_handle();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        handle.terminate_execution()
    });

    let error = probe
        .execute_script("spin.js", "for(;;) {}")
        .expect_err("the cancelled script must fail");
    assert!(
        error.message.contains("terminated"),
        "unexpected error: {error:?}"
    );
    assert!(canceller.join().expect("canceller thread must not panic"));

    assert!(probe.cancel_terminate_execution());
    probe
        .execute_script("after_cancel.js", "1 + 1")
        .expect("the isolate must be usable after cancellation is cleared");
}

/// Memory limit: with a hard heap cap the worker gets a recoverable
/// termination instead of an out-of-memory abort.
#[tokio::test]
async fn enforces_heap_limit() {
    let fired = Arc::new(AtomicBool::new(false));
    let mut probe = ProbeRuntime::with_bundle(PROBE_BUNDLE, Some(8 * 1024 * 1024));
    probe.install_heap_limit_guard(Arc::clone(&fired));

    let error = probe
        .execute_script(
            "allocate.js",
            r#"let held = []; while (true) { held.push("x".repeat(1024)); }"#,
        )
        .expect_err("allocation past the heap cap must fail");

    assert!(
        fired.load(Ordering::SeqCst),
        "the near-heap-limit callback must have fired"
    );
    assert!(
        error.message.contains("terminated"),
        "unexpected error: {error:?}"
    );
}

/// Checkpoint: Rust-owned loop state round-trips through serialisation, so a
/// resumed worker continues from stored state rather than from scratch.
#[tokio::test]
async fn checkpoints_and_restores_rust_owned_state() {
    let mut probe = ProbeRuntime::new();
    probe
        .evaluate_module(MAIN_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("bundle must evaluate");

    let before = probe.host_state();
    let checkpoint = probe.checkpoint().expect("checkpoint must serialise");
    assert_eq!(before.model_calls, 1);
    assert_eq!(before.events.len(), 1);

    let mut resumed = ProbeRuntime::new();
    assert!(resumed.host_state().events.is_empty());
    resumed
        .restore_checkpoint(&checkpoint)
        .expect("checkpoint must deserialise into a fresh runtime");
    assert_eq!(resumed.host_state(), before);
}
