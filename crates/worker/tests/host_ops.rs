//! The production host-op surface.
//!
//! Each test pins one guarantee the boundary claims: the declared surface is
//! closed, JavaScript cannot reach an undeclared capability, every op has a
//! success *and* a typed failure path, budgets and deadlines are enforced, and a
//! missing capability is never reported as a plausible result.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use geo_domain::{
    ChunkLocator, KnowledgeEvidence, KnowledgeSearchRequest, KnowledgeSearchResult, TenantScope,
};
use geo_worker::{
    HOST_BUNDLE, HOST_MAIN_MODULE, HOST_OPS_VERSION, HostBridge, HostOp, HostOpBudgets,
    HostOpError, HostOpErrorCode, HostOpLimits, HostOps, HostRuntime, HostState, ManifestItem,
    ManifestPage, ManifestReadRequest, MeasureRequest, MeasureSample, ModelCompletion,
    ModelCompletionRequest, PublishReceipt, PublishRequest, PublishState,
};
use serde_json::Value;

const GENEROUS_DEADLINE: Duration = Duration::from_secs(30);
const SCENARIO_MODULE: &str = "memeloop://bundle/scenario.js";
const CHECKPOINT_MODULE: &str = "memeloop://bundle/checkpoint.js";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// How the fake bridge should answer, so one recorder covers the success and
/// failure paths of every op.
#[derive(Debug, Clone, Default)]
struct Behaviour {
    /// Ops that answer with a typed failure instead of a result.
    failing: BTreeMap<HostOp, HostOpErrorCode>,
    /// Ops that stall before answering, for the deadline and cancellation
    /// tests.
    stalling: BTreeMap<HostOp, Duration>,
    /// The message carried by every failure, so a test can show that the
    /// isolate boundary redacts what a bridge hands it.
    failure_message: Option<String>,
}

/// Records what the bridge was asked for and answers from `behaviour`.
#[derive(Debug, Clone, Default)]
struct FakeHostOps {
    behaviour: Behaviour,
    seen: Arc<Mutex<Vec<HostOp>>>,
    scopes: Arc<Mutex<Vec<String>>>,
}

impl FakeHostOps {
    fn new() -> Self {
        Self::default()
    }

    fn with(behaviour: Behaviour) -> Self {
        Self {
            behaviour,
            ..Self::default()
        }
    }

    fn seen(&self) -> Vec<HostOp> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn scopes(&self) -> Vec<String> {
        self.scopes.lock().expect("scope lock").clone()
    }

    async fn answer<T>(&self, op: HostOp, scope: &TenantScope, value: T) -> Result<T, HostOpError> {
        self.seen.lock().expect("seen lock").push(op);
        self.scopes
            .lock()
            .expect("scope lock")
            .push(scope.storage_key());
        if let Some(delay) = self.behaviour.stalling.get(&op) {
            tokio::time::sleep(*delay).await;
        }
        if let Some(code) = self.behaviour.failing.get(&op) {
            let message = self
                .behaviour
                .failure_message
                .clone()
                .unwrap_or_else(|| format!("{} is unavailable", op.name()));
            return Err(HostOpError::new(op, *code, message));
        }
        Ok(value)
    }
}

#[async_trait]
impl HostOps for FakeHostOps {
    async fn model_complete(
        &self,
        scope: &TenantScope,
        request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.answer(
            HostOp::ModelComplete,
            scope,
            ModelCompletion {
                text: format!("bridge:{}", request.prompt.trim()),
                model: request
                    .model
                    .unwrap_or_else(|| "test-routing-id".to_owned()),
                prompt_tokens: 11,
                completion_tokens: 7,
                finish_reason: "stop".to_owned(),
            },
        )
        .await
    }

    async fn knowledge_search(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError> {
        let evidence = KnowledgeEvidence {
            source_id: uuid::Uuid::new_v4(),
            source_version_id: uuid::Uuid::new_v4(),
            chunk_id: uuid::Uuid::new_v4(),
            source_name: "warranty-policy.md".to_owned(),
            purpose: request.purpose,
            locator: ChunkLocator::Text {
                start_line: 1,
                end_line: 3,
                start_char: 0,
                end_char: 64,
            },
            text: "the warranty runs for twenty-four months".to_owned(),
            quote: "twenty-four months".to_owned(),
        };
        self.answer(
            HostOp::KnowledgeSearch,
            scope,
            KnowledgeSearchResult {
                knowledge_release_id: Some(uuid::Uuid::new_v4()),
                evidence: vec![evidence],
                capability_missing: None,
            },
        )
        .await
    }

    async fn manifest_read(
        &self,
        scope: &TenantScope,
        request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        self.answer(
            HostOp::ManifestRead,
            scope,
            ManifestPage {
                kind: request.kind,
                manifest_id: uuid::Uuid::new_v4(),
                revision: 1,
                state: "frozen".to_owned(),
                sealed: false,
                expected_count: None,
                items: vec![ManifestItem {
                    branch_id: "document-key-1".to_owned(),
                    document_revision_id: uuid::Uuid::new_v4(),
                    platform_target_id: None,
                }],
                next_cursor: None,
            },
        )
        .await
    }

    async fn publish_submit(
        &self,
        scope: &TenantScope,
        request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        assert!(!request.body.is_empty(), "the body must reach the bridge");
        self.answer(
            HostOp::Publish,
            scope,
            PublishReceipt {
                publish_attempt_id: uuid::Uuid::new_v4(),
                state: PublishState::UnknownResult,
                external_url: None,
                evidence_ref: None,
            },
        )
        .await
    }

    async fn measure_sample(
        &self,
        scope: &TenantScope,
        request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        self.answer(
            HostOp::Measure,
            scope,
            MeasureSample {
                sample_id: uuid::Uuid::new_v4(),
                channel: request.channel,
                answer: "the warranty is twenty-four months".to_owned(),
                evidence_refs: vec![request.measurement_protocol_id],
                observed_at: Utc::now(),
            },
        )
        .await
    }
}

fn scope() -> TenantScope {
    TenantScope::new(
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
        Some(uuid::Uuid::new_v4().into()),
    )
}

fn bridge(ops: Arc<FakeHostOps>) -> HostBridge {
    // These tests drive the isolate on the test's own runtime, so the
    // capability work belongs there too.
    HostBridge::new(ops, scope(), tokio::runtime::Handle::current())
}

/// Builds a production runtime whose entry module is `script`.
///
/// The scenario imports the crate's own host façade, so the tests exercise the
/// same transport an embedding host would ship.
fn runtime(script: &'static str, bridge: HostBridge) -> HostRuntime {
    let bundle = [
        ("memeloop://bundle/host-ops.js", geo_worker::HOST_OPS_JS),
        (SCENARIO_MODULE, script),
    ];
    HostRuntime::new(&bundle, bridge, None).expect("the production runtime must be constructible")
}

/// Reads one reported op outcome out of the Rust-owned event log.
fn outcome(state: &HostState, topic: &str) -> Value {
    let event = state
        .events
        .iter()
        .find(|event| event.topic == topic)
        .unwrap_or_else(|| panic!("no `{topic}` outcome in {:?}", state.events));
    serde_json::from_str(&event.payload).expect("an outcome payload must be JSON")
}

fn assert_typed_error(record: &Value, code: &str, op: &str) {
    assert_eq!(record["ok"], false, "expected a failure: {record}");
    assert_eq!(
        record["name"], "GeoHostOpError",
        "failures must carry the stable class: {record}"
    );
    assert_eq!(record["error"]["code"], code, "unexpected code: {record}");
    assert_eq!(record["error"]["op"], op, "unexpected op: {record}");
}

fn assert_success(record: &Value) -> Value {
    assert_eq!(record["ok"], true, "expected a result: {record}");
    record["value"].clone()
}

// ---------------------------------------------------------------------------
// The closed surface
// ---------------------------------------------------------------------------

/// The registered surface is exactly the declared set: a capability added
/// without being declared, or declared without a registered body, fails here.
#[tokio::test]
async fn production_runtime_exposes_only_the_declared_host_ops() {
    let mut runtime = runtime(
        "export const ready = true;",
        bridge(Arc::new(FakeHostOps::new())),
    );
    runtime
        .execute_script_with_deadline(
            "op-names.js",
            r#"
            for (const name of Deno.core.opNames()) {
              Deno.core.ops.op_host_emit("everything", name);
              if (name.startsWith("op_host_")) {
                Deno.core.ops.op_host_emit("host-op", name);
              }
            }
            "#,
            Duration::from_secs(20),
        )
        .expect("enumerating op names must succeed");

    let mut presented = runtime
        .host_state()
        .events
        .iter()
        .filter(|event| event.topic == "host-op")
        .map(|event| event.payload.clone())
        .collect::<Vec<_>>();
    presented.sort();
    assert_eq!(
        presented,
        HostRuntime::op_surface(),
        "the registered surface must be the declared surface, no more and no less"
    );
    assert_eq!(presented.len(), HostOp::COUNT + 2);
    assert_eq!(runtime.host_ops_version(), HOST_OPS_VERSION);

    // Built-in engine ops remain (the loop needs the microtask queue and
    // timers), but none of them opens a file, a socket, a process or the
    // environment.  The needles are deliberately narrow: `op_read`/`op_write`
    // are the engine's own console paths, not file access.
    let dangerous = [
        "op_fs_",
        "op_read_file",
        "op_write_file",
        "op_net_",
        "op_http_",
        "op_tcp",
        "op_udp",
        "op_dns",
        "op_env_",
        "op_process_",
        "op_spawn",
        "op_child",
        "op_sql",
        "op_connect",
        "op_listen",
        "op_socket",
        "op_exec_",
    ];
    let everything = runtime
        .host_state()
        .events
        .iter()
        .filter(|event| event.topic == "everything")
        .map(|event| event.payload.clone())
        .collect::<Vec<_>>();
    assert!(!everything.is_empty(), "the op table must be visible");
    for name in everything {
        for needle in dangerous {
            assert!(
                !name.contains(needle),
                "`{name}` must not be registered: it would grant `{needle}`"
            );
        }
    }
}

/// JavaScript reaches the outside world only through the declared ops: no
/// filesystem, network, process or environment capability exists as a global,
/// and the probe's model stub is not registered here either.
#[tokio::test]
async fn javascript_cannot_reach_filesystem_network_process_or_environment() {
    let mut runtime = runtime(
        "export const ready = true;",
        bridge(Arc::new(FakeHostOps::new())),
    );
    runtime
        .execute_script_with_deadline(
            "globals.js",
            r#"
            const probes = [
              "fetch", "process", "require", "module", "Buffer", "XMLHttpRequest",
              "WebSocket", "EventSource", "Worker", "SharedWorker", "importScripts",
              "Deno.readTextFile", "Deno.writeTextFile", "Deno.remove", "Deno.mkdir",
              "Deno.readDir", "Deno.stat", "Deno.env", "Deno.exit", "Deno.run",
              "Deno.spawn", "Deno.connect", "Deno.listen", "Deno.serve", "Deno.chdir",
              "Deno.execPath", "Deno.makeTempDir", "Deno.open", "Deno.create",
              "Deno.core.ops.op_host_model_complete",
              "Deno.core.ops.op_host_model_call_count",
            ];
            for (const probe of probes) {
              let kind;
              try {
                kind = typeof eval(probe);
              } catch (error) {
                kind = "unresolvable:" + error.name;
              }
              Deno.core.ops.op_host_emit("probe", probe + "=" + kind);
            }
            "#,
            Duration::from_secs(20),
        )
        .expect("probing globals must succeed");

    let presented = runtime
        .host_state()
        .events
        .iter()
        .filter(|event| event.topic == "probe")
        .map(|event| event.payload.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        presented.len(),
        31,
        "every probe must report: {presented:?}"
    );
    for probe in presented {
        assert!(
            probe.ends_with("=undefined") || probe.ends_with("=unresolvable:ReferenceError"),
            "`{probe}` names a capability JavaScript can reach"
        );
    }
}

/// The reference bundle loads against the production op set and declares the
/// capabilities it expects, so an embedding host can tell "loaded" from
/// "loaded and able to reach its capabilities".
#[tokio::test]
async fn the_reference_bundle_declares_the_surface_it_expects() {
    let mut runtime = HostRuntime::new(HOST_BUNDLE, bridge(Arc::new(FakeHostOps::new())), None)
        .expect("the production runtime must be constructible");
    runtime
        .evaluate_module(HOST_MAIN_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the reference bundle must evaluate");

    let ready = outcome(&runtime.host_state(), "loop.ready");
    assert_eq!(ready["version"], HOST_OPS_VERSION);
    let declared = ready["capabilities"]
        .as_array()
        .expect("capabilities must be a list");
    for capability in [
        "modelComplete",
        "knowledgeSearch",
        "manifestRead",
        "publishSubmit",
        "measureSample",
    ] {
        assert!(
            declared.iter().any(|entry| entry == capability),
            "`{capability}` must be declared by the bundle: {declared:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Success and failure paths, one op at a time
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_completion_delegates_to_the_rust_bridge() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("model", () => hostOps.modelComplete({ prompt: "  how long is the warranty?  " }));
        "#,
        bridge(Arc::clone(&ops)),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    let value = assert_success(&outcome(&runtime.host_state(), "model"));
    assert_eq!(value["text"], "bridge:how long is the warranty?");
    assert_eq!(value["model"], "test-routing-id");
    assert_eq!(value["prompt_tokens"], 11);
    assert_eq!(value["finish_reason"], "stop");
    assert_eq!(ops.seen(), vec![HostOp::ModelComplete]);
    assert_eq!(runtime.op_calls(HostOp::ModelComplete), 1);
}

#[tokio::test]
async fn knowledge_search_returns_evidence() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("search", () => hostOps.knowledgeSearch({ query: "warranty", limit: 3 }));
        "#,
        bridge(Arc::clone(&ops)),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    let value = assert_success(&outcome(&runtime.host_state(), "search"));
    assert_eq!(value["evidence"][0]["source_name"], "warranty-policy.md");
    assert_eq!(value["evidence"][0]["quote"], "twenty-four months");
    assert!(
        value["capability_missing"].is_null(),
        "a successful retrieval must not carry a capability gap: {value}"
    );
    assert_eq!(ops.seen(), vec![HostOp::KnowledgeSearch]);
}

/// An empty evidence list and a missing retrieval capability mean opposite
/// things, so a missing capability must be an error, not an empty result.
#[tokio::test]
async fn knowledge_search_reports_a_missing_capability_as_a_typed_error() {
    let ops = Arc::new(FakeHostOps::with(Behaviour {
        failing: BTreeMap::from([(HostOp::KnowledgeSearch, HostOpErrorCode::CapabilityMissing)]),
        failure_message: Some("vector retrieval is not configured".to_owned()),
        ..Behaviour::default()
    }));
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("search", () => hostOps.knowledgeSearch({ query: "warranty" }));
        "#,
        bridge(ops),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    let record = outcome(&runtime.host_state(), "search");
    assert_typed_error(&record, "capability_missing", "knowledge_search");
    assert_eq!(record["error"]["retryable"], false);
    assert_eq!(
        record["error"]["message"], "vector retrieval is not configured",
        "the reason must survive: {record}"
    );
}

#[tokio::test]
async fn manifest_read_returns_the_frozen_manifest_state() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("manifest", () => hostOps.manifestRead({ kind: "document" }));
        "#,
        bridge(Arc::clone(&ops)),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    let value = assert_success(&outcome(&runtime.host_state(), "manifest"));
    assert_eq!(value["kind"], "document");
    assert_eq!(value["sealed"], false);
    assert_eq!(value["items"][0]["branch_id"], "document-key-1");
    assert_eq!(ops.seen(), vec![HostOp::ManifestRead]);
}

#[tokio::test]
async fn manifest_read_reports_a_missing_manifest_as_not_found() {
    let ops = Arc::new(FakeHostOps::with(Behaviour {
        failing: BTreeMap::from([(HostOp::ManifestRead, HostOpErrorCode::NotFound)]),
        failure_message: Some("the project has not been started".to_owned()),
        ..Behaviour::default()
    }));
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("manifest", () => hostOps.manifestRead({ kind: "distribution" }));
        "#,
        bridge(ops),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    assert_typed_error(
        &outcome(&runtime.host_state(), "manifest"),
        "not_found",
        "manifest_read",
    );
}

#[tokio::test]
async fn publish_submit_returns_a_typed_receipt() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("publish", () => hostOps.publishSubmit({
          document_revision_id: "00000000-0000-4000-8000-000000000001",
          platform_target_id: "00000000-0000-4000-8000-000000000002",
          body: "final copy",
        }));
        "#,
        bridge(Arc::clone(&ops)),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    // An unknown result is a first-class outcome: a loop must query rather than
    // blindly resend.
    let value = assert_success(&outcome(&runtime.host_state(), "publish"));
    assert_eq!(value["state"], "unknown_result");
    assert!(value["publish_attempt_id"].as_str().is_some());
    assert_eq!(ops.seen(), vec![HostOp::Publish]);
}

#[tokio::test]
async fn measure_sample_returns_an_observed_sample() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("measure", () => hostOps.measureSample({
          measurement_protocol_id: "00000000-0000-4000-8000-000000000003",
          question: "how long is the warranty?",
          channel: "independent-search",
        }));
        "#,
        bridge(Arc::clone(&ops)),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    let value = assert_success(&outcome(&runtime.host_state(), "measure"));
    assert_eq!(value["channel"], "independent-search");
    assert_eq!(value["answer"], "the warranty is twenty-four months");
    assert_eq!(
        value["evidence_refs"][0],
        "00000000-0000-4000-8000-000000000003"
    );
    assert_eq!(ops.seen(), vec![HostOp::Measure]);
}

// ---------------------------------------------------------------------------
// The boundary itself
// ---------------------------------------------------------------------------

/// A request cannot smuggle a destination, a credential or a foreign scope: the
/// declared shape is the whole shape.
#[tokio::test]
async fn undeclared_request_fields_are_refused() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("endpoint", () => hostOps.modelComplete({
          prompt: "hello",
          base_url: "https://attacker.example/v1",
          api_key: "sk-live-abcdefghijklmnopqrstuvwxyz0123456789",
        }));
        await attempt("foreign-scope", () => hostOps.knowledgeSearch({
          query: "warranty",
          tenant_id: "00000000-0000-4000-8000-00000000000f",
        }));
        "#,
        bridge(Arc::clone(&ops)),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    assert_typed_error(
        &outcome(&runtime.host_state(), "endpoint"),
        "invalid_request",
        "model_complete",
    );
    assert_typed_error(
        &outcome(&runtime.host_state(), "foreign-scope"),
        "invalid_request",
        "knowledge_search",
    );
    // Neither request reached the bridge, so no destination was ever dialled
    // and no foreign scope was ever addressed.
    assert!(
        ops.seen().is_empty(),
        "the bridge must not be called: {:?}",
        ops.seen()
    );
}

/// A request that is well-shaped but out of the declared range is refused the
/// same way, before any capability is consulted.
#[tokio::test]
async fn out_of_range_requests_are_refused() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("too-many", () => hostOps.knowledgeSearch({ query: "warranty", limit: 500 }));
        await attempt("zero", () => hostOps.knowledgeSearch({ query: "warranty", limit: 0 }));
        await attempt("page", () => hostOps.manifestRead({ kind: "document", limit: 1000 }));
        "#,
        bridge(Arc::clone(&ops)),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    for topic in ["too-many", "zero", "page"] {
        let record = outcome(&runtime.host_state(), topic);
        assert_eq!(
            record["ok"], false,
            "expected a failure for `{topic}`: {record}"
        );
        assert_eq!(record["error"]["code"], "invalid_request", "for `{topic}`");
    }
    assert!(
        ops.seen().is_empty(),
        "no capability may be consulted: {:?}",
        ops.seen()
    );
}

/// The scope a capability runs under is the one the worker was assembled with,
/// never one a script supplies.
#[tokio::test]
async fn the_tenant_scope_comes_from_the_bridge_not_from_the_script() {
    let ops = Arc::new(FakeHostOps::new());
    let scope = scope();
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("model", () => hostOps.modelComplete({ prompt: "hello" }));
        await attempt("search", () => hostOps.knowledgeSearch({ query: "warranty" }));
        "#,
        HostBridge::new(
            Arc::clone(&ops) as Arc<dyn HostOps>,
            scope.clone(),
            tokio::runtime::Handle::current(),
        ),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    assert_eq!(
        ops.scopes(),
        vec![scope.storage_key(), scope.storage_key()],
        "every capability must have run under the assembled scope"
    );
    assert_eq!(runtime.bridge().scope(), &scope);
}

/// A run that exhausts an op's budget is refused, the refused attempt is still
/// counted, and the spent budget does not spill into another op.
#[tokio::test]
async fn the_call_budget_is_enforced_per_op() {
    let ops = Arc::new(FakeHostOps::new());
    let budgets = HostOpBudgets::default()
        .with_limits(HostOp::ModelComplete, HostOpLimits::new(5_000, 1))
        .with_limits(HostOp::KnowledgeSearch, HostOpLimits::new(5_000, 3));
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("first", () => hostOps.modelComplete({ prompt: "hello" }));
        await attempt("second", () => hostOps.modelComplete({ prompt: "hello" }));
        await attempt("search", () => hostOps.knowledgeSearch({ query: "warranty" }));
        "#,
        bridge(Arc::clone(&ops)).with_budgets(budgets),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    assert_success(&outcome(&runtime.host_state(), "first"));
    let refused = outcome(&runtime.host_state(), "second");
    assert_typed_error(&refused, "budget_exceeded", "model_complete");
    assert_eq!(refused["error"]["retryable"], true);
    assert_eq!(runtime.op_calls(HostOp::ModelComplete), 2);
    assert_eq!(runtime.op_calls(HostOp::KnowledgeSearch), 1);
    assert_success(&outcome(&runtime.host_state(), "search"));
    assert_eq!(
        ops.seen(),
        vec![HostOp::ModelComplete, HostOp::KnowledgeSearch],
        "the refused attempt must not reach the capability"
    );
}

/// An op that outlives its wall-clock budget fails with a typed error instead
/// of stalling the run, and the budget bounds the op rather than the run.
#[tokio::test]
async fn the_op_deadline_is_enforced() {
    let ops = Arc::new(FakeHostOps::with(Behaviour {
        stalling: BTreeMap::from([(HostOp::ModelComplete, Duration::from_secs(30))]),
        ..Behaviour::default()
    }));
    let budgets =
        HostOpBudgets::default().with_limits(HostOp::ModelComplete, HostOpLimits::new(50, 4));
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("model", () => hostOps.modelComplete({ prompt: "hello" }));
        await attempt("search", () => hostOps.knowledgeSearch({ query: "warranty" }));
        "#,
        bridge(ops).with_budgets(budgets),
    );

    let started = Instant::now();
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must observe the deadline rather than hang");
    let elapsed = started.elapsed();

    assert_typed_error(
        &outcome(&runtime.host_state(), "model"),
        "deadline_exceeded",
        "model_complete",
    );
    assert_success(&outcome(&runtime.host_state(), "search"));
    assert!(
        elapsed < Duration::from_secs(5),
        "the op budget must stop the call, took {elapsed:?}"
    );
}

/// Cancelling a run fails its in-flight op and every later one with a typed
/// error, so a cancelled run cannot go on to publish or measure.
#[tokio::test]
async fn cancelling_the_run_fails_ops_with_a_typed_error() {
    let ops = Arc::new(FakeHostOps::with(Behaviour {
        stalling: BTreeMap::from([(HostOp::ModelComplete, Duration::from_secs(30))]),
        ..Behaviour::default()
    }));
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("model", () => hostOps.modelComplete({ prompt: "hello" }));
        await attempt("search", () => hostOps.knowledgeSearch({ query: "warranty" }));
        "#,
        bridge(Arc::clone(&ops)),
    );

    let cancellation = runtime.bridge().cancellation();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        cancellation.store(true, Ordering::SeqCst);
    });

    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must observe the cancellation rather than hang");
    canceller.join().expect("the canceller must not panic");

    assert_typed_error(
        &outcome(&runtime.host_state(), "model"),
        "cancelled",
        "model_complete",
    );
    assert_typed_error(
        &outcome(&runtime.host_state(), "search"),
        "cancelled",
        "knowledge_search",
    );
    // The stalling capability was entered once and never re-entered.
    assert_eq!(ops.seen(), vec![HostOp::ModelComplete]);
}

/// A provider message that quotes a credential reaches the script redacted, and
/// the part of the diagnosis that is not a credential survives.
#[tokio::test]
async fn provider_failures_reach_the_script_redacted() {
    const KEY: &str = "sk-live-abcdefghijklmnopqrstuvwxyz0123456789";
    let ops = Arc::new(FakeHostOps::with(Behaviour {
        failing: BTreeMap::from([(HostOp::ModelComplete, HostOpErrorCode::Failed)]),
        failure_message: Some(format!("provider rejected key {KEY} with status 401")),
        ..Behaviour::default()
    }));
    let mut runtime = runtime(
        r#"
        import { hostOps, attempt } from "./host-ops.js";
        await attempt("model", () => hostOps.modelComplete({ prompt: "hello" }));
        "#,
        bridge(ops),
    );
    runtime
        .evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the scenario must evaluate");

    let record = outcome(&runtime.host_state(), "model");
    assert_typed_error(&record, "failed", "model_complete");
    let message = record["error"]["message"]
        .as_str()
        .expect("the error must carry a message");
    assert!(
        !message.contains(KEY),
        "the key leaked into the isolate: {message}"
    );
    assert!(message.contains("***"), "unexpected redaction: {message}");
    assert!(
        message.contains("401"),
        "the diagnosis must survive: {message}"
    );
}

/// The run-level wall clock still terminates the production isolate, which is
/// what makes an unbounded script recoverable.
#[tokio::test]
async fn the_wall_clock_deadline_terminates_the_production_isolate() {
    let mut runtime = runtime(
        "export const ready = true;",
        bridge(Arc::new(FakeHostOps::new())),
    );
    let started = Instant::now();
    let error = runtime
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

/// The Rust-owned state a resumed run continues from round-trips through the
/// production runtime exactly as it does through the probe.
#[tokio::test]
async fn checkpoint_round_trips_through_the_production_runtime() {
    let bundle = [(CHECKPOINT_MODULE, CHECKPOINT_SCENARIO_JS)];
    let mut origin = HostRuntime::new(&bundle, bridge(Arc::new(FakeHostOps::new())), None)
        .expect("the production runtime must be constructible");
    origin
        .evaluate_module(CHECKPOINT_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the checkpoint scenario must evaluate");
    let checkpoint = origin.checkpoint().expect("checkpoint must serialise");
    assert_eq!(origin.host_state().events.len(), 1);

    let mut resumed = HostRuntime::new(&bundle, bridge(Arc::new(FakeHostOps::new())), None)
        .expect("the production runtime must be constructible");
    assert!(resumed.host_state().events.is_empty());
    resumed
        .restore_checkpoint(&checkpoint)
        .expect("checkpoint must deserialise");
    assert_eq!(resumed.host_state(), origin.host_state());
}

/// Reports a step through the emit contract, which is exactly what a checkpoint
/// carries: Rust-owned state, not an opaque engine snapshot.
const CHECKPOINT_SCENARIO_JS: &str = r#"
await Deno.core.ops.op_host_emit("loop.step", JSON.stringify({ step: 1 }));
export const ready = true;
"#;

// ---------------------------------------------------------------------------
// Turns: the host calls `main` once per run
// ---------------------------------------------------------------------------

/// Short enough that a turn relying on it cannot be confused with one that
/// finished, long enough that loading and evaluating the bundle is not racing
/// it.
const SHORT_DEADLINE: Duration = Duration::from_millis(400);

fn count_events(state: &HostState, topic: &str) -> usize {
    state
        .events
        .iter()
        .filter(|event| event.topic == topic)
        .count()
}

/// A turn is a *call*, not an evaluation: the host evaluates the entry module
/// and then calls `main` with that turn's inputs.  Every capability the turn
/// reaches carries the run's own scope, and the turn reports its result exactly
/// once.
#[tokio::test]
async fn calling_main_runs_one_turn_under_the_run_scope() {
    let ops = Arc::new(FakeHostOps::new());
    let bridge = bridge(Arc::clone(&ops));
    let run_scope = bridge.scope().storage_key();
    let mut runtime =
        HostRuntime::new(HOST_BUNDLE, bridge, None).expect("the runtime must be constructible");

    runtime
        .call_main(
            HOST_MAIN_MODULE,
            &serde_json::json!({ "prompt": "how long is the warranty?" }).to_string(),
            GENEROUS_DEADLINE,
        )
        .await
        .expect("the reference bundle must complete a turn");

    assert_eq!(
        ops.seen(),
        vec![HostOp::KnowledgeSearch, HostOp::ModelComplete],
        "a turn retrieves evidence before it asks the model"
    );
    assert!(
        ops.scopes().iter().all(|seen| *seen == run_scope),
        "every capability call must carry the run's scope: {:?}",
        ops.scopes()
    );

    let state = runtime.host_state();
    assert_eq!(
        count_events(&state, "loop.completed"),
        1,
        "a turn reports its result exactly once: {:?}",
        state.events
    );
    let completed = outcome(&state, "loop.completed");
    assert_eq!(completed["answer"], "bridge:how long is the warranty?");
}

/// Evaluating the entry module is not a turn.  It announces the surface it
/// expects and stops; nothing is retrieved and no model is asked.  Without this
/// a host that evaluated a bundle and read the event log would see a plausible
/// turn that never ran.
#[tokio::test]
async fn evaluating_the_entry_module_does_not_run_a_turn() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime =
        HostRuntime::new(HOST_BUNDLE, bridge(Arc::clone(&ops)), None).expect("constructible");

    runtime
        .evaluate_module(HOST_MAIN_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the reference bundle must evaluate");

    let state = runtime.host_state();
    assert_eq!(count_events(&state, "loop.ready"), 1);
    assert_eq!(
        count_events(&state, "loop.completed"),
        0,
        "evaluating a module must not be mistaken for running a turn: {:?}",
        state.events
    );
    assert!(
        ops.seen().is_empty(),
        "an evaluation must not exercise a capability: {:?}",
        ops.seen()
    );
}

/// A bundle that never exported `main` is refused by name, rather than appearing
/// to run a turn that produced nothing.
#[tokio::test]
async fn a_bundle_without_main_is_refused() {
    let ops = Arc::new(FakeHostOps::new());
    let mut runtime = runtime("export const ready = true;", bridge(Arc::clone(&ops)));

    let error = runtime
        .call_main(SCENARIO_MODULE, "{}", GENEROUS_DEADLINE)
        .await
        .expect_err("a bundle that exports no `main` cannot run a turn");

    assert_eq!(error.stage, "namespace", "unexpected error: {error:?}");
    assert!(
        error.message.contains("main"),
        "unexpected error: {error:?}"
    );
    assert!(ops.seen().is_empty(), "no capability may be exercised");
}

/// `main` that is not callable is refused just as explicitly as `main` that is
/// absent.
#[tokio::test]
async fn a_main_that_is_not_a_function_is_refused() {
    let mut runtime = runtime(
        "export const main = 42;",
        bridge(Arc::new(FakeHostOps::new())),
    );

    let error = runtime
        .call_main(SCENARIO_MODULE, "{}", GENEROUS_DEADLINE)
        .await
        .expect_err("a `main` that is not a function cannot run a turn");

    assert_eq!(error.stage, "namespace", "unexpected error: {error:?}");
}

/// A turn that throws surfaces the script's own message, so a failing turn can
/// be diagnosed from the run record alone.
#[tokio::test]
async fn a_main_that_throws_fails_the_call() {
    let mut runtime = runtime(
        "export async function main() { throw new Error('turn exploded'); }",
        bridge(Arc::new(FakeHostOps::new())),
    );

    let error = runtime
        .call_main(SCENARIO_MODULE, "{}", GENEROUS_DEADLINE)
        .await
        .expect_err("a throwing turn must fail");

    assert_eq!(error.stage, "call", "unexpected error: {error:?}");
    assert!(
        error.message.contains("turn exploded"),
        "unexpected error: {error:?}"
    );
}

/// A turn whose promise can never settle fails as soon as the event loop has
/// nothing left to do, rather than burning the whole deadline before saying so.
#[tokio::test]
async fn a_main_awaiting_an_unresolvable_promise_fails_promptly() {
    let mut runtime = runtime(
        "export async function main() { await new Promise(() => {}); }",
        bridge(Arc::new(FakeHostOps::new())),
    );

    let started = Instant::now();
    let error = runtime
        .call_main(SCENARIO_MODULE, "{}", GENEROUS_DEADLINE)
        .await
        .expect_err("a turn that can never settle must fail");
    let elapsed = started.elapsed();

    assert_eq!(error.stage, "call", "unexpected error: {error:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "it must not wait out the deadline, took {elapsed:?}"
    );
}

/// An unbounded turn is terminated at the deadline instead of pinning its
/// worker thread forever.  Multi-threaded because the deadline fires from a
/// separate task, and a spinning isolate owns its thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unbounded_turn_is_terminated_at_the_deadline() {
    let mut runtime = runtime(
        "export function main() { for(;;) {} }",
        bridge(Arc::new(FakeHostOps::new())),
    );

    let started = Instant::now();
    let error = runtime
        .call_main(SCENARIO_MODULE, "{}", SHORT_DEADLINE)
        .await
        .expect_err("an unbounded turn must be terminated");
    let elapsed = started.elapsed();

    assert_eq!(error.stage, "call", "unexpected error: {error:?}");
    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline must actually stop execution, took {elapsed:?}"
    );
    assert!(
        error.message.contains("terminated"),
        "unexpected error: {error:?}"
    );
}
