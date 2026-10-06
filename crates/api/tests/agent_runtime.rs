//! The application assembly of the embedded JavaScript runtime.
//!
//! Two guarantees are pinned here.  A configured runtime is the one the API
//! actually consults, and a run's JavaScript reaches the injected capability
//! under the run's own scope.  An unconfigured runtime still yields the explicit
//! `capability_missing` result the API has always produced, with no field
//! through which a fabricated reply could reach the UI.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, CSRF_HEADER, EmbeddedAgentRuntime, RepositoryHostOps, router};
use geo_domain::{
    AgentRepository, AgentRuntime, AppendMessage, CreateConversation, DEVELOPMENT_TENANT_ID,
    ImportItem, KnowledgePurpose, KnowledgeRepository, KnowledgeSearchRequest,
    MemoryAgentRepository, MemoryKnowledgeRepository, RunStatus, RuntimeCapability, SourceKind,
    TenantScope, ToolCallOutcome,
};
use geo_worker::{
    HOST_LOOP_JS, HOST_MAIN_MODULE, HOST_OPS_JS, HOST_OPS_VERSION, HostOp, HostOpBudgets,
    HostOpError, HostOpErrorCode, HostOps, KnowledgeSearchResult, KnowledgeTextReadRequest,
    KnowledgeTextReviseRequest, ManifestKind, ManifestPage, ManifestReadRequest, MeasureRequest,
    MeasureSample, MeasurementSurface, ModelCompletion, ModelCompletionRequest, PublishReceipt,
    PublishRequest,
};
use serde_json::Value;
use tower::ServiceExt;

const GENEROUS_DEADLINE: Duration = Duration::from_secs(30);
const SCENARIO_MODULE: &str = "memeloop://bundle/scenario.js";
const HEAP_RUNAWAY_MODULE: &str = "memeloop://bundle/heap-runaway.js";
const HEAP_RUNAWAY_CHILD_ENV: &str = "GEO_TEST_HEAP_RUNAWAY_CHILD";
const TEST_V8_HEAP_LIMIT_BYTES: usize = EmbeddedAgentRuntime::MIN_V8_HEAP_LIMIT_BYTES;

/// How long a recorder holds a turn in flight when a test needs to observe the
/// run while it is still executing.
///
/// Deliberately short and finite: a turn is suspended in a timer on the
/// runtime being torn down, and a live timer outliving its runtime panics
/// inside that runtime rather than in the test.  A stall a test can wait out is
/// what keeps the teardown ordered.
const STALL: Duration = Duration::from_millis(500);

/// A bundle whose entry module runs one reference turn, so a test can show the
/// whole path from the assembly seam to the capability and back.
const SCENARIO_JS: &str = r#"
import { main } from "./host-loop.js";

export const result = await main({
  prompt: "how long is the warranty?",
  query: "warranty",
});
"#;

static SCENARIO_BUNDLE: &[(&str, &str)] = &[
    ("memeloop://bundle/host-ops.js", HOST_OPS_JS),
    ("memeloop://bundle/host-loop.js", HOST_LOOP_JS),
    (SCENARIO_MODULE, SCENARIO_JS),
];

/// Deliberately retains every allocation, so V8 cannot reclaim its way out of
/// the pressure.  This is only executed in a child process below: if a future
/// engine regression turns a near-heap callback into a process abort, the test
/// runner that launched it remains alive to report the failure.
const HEAP_RUNAWAY_JS: &str = r#"
export async function main() {
  const held = [];
  while (true) {
    held.push("x".repeat(1024));
  }
}
"#;

static HEAP_RUNAWAY_BUNDLE: &[(&str, &str)] = &[(HEAP_RUNAWAY_MODULE, HEAP_RUNAWAY_JS)];

const SPIN_MODULE: &str = "memeloop://bundle/cancel-spin.js";
const SPIN_JS: &str = r#"
export async function main() {
  while (true) {}
}
"#;
static SPIN_BUNDLE: &[(&str, &str)] = &[(SPIN_MODULE, SPIN_JS)];

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Records which op each call reached and under which scope, so a test can show
/// the isolate went through the bridge rather than around it.
#[derive(Debug, Default)]
struct Recorder {
    seen: Mutex<Vec<(HostOp, String)>>,
    /// The runtime flavor each capability call was polled on, as it reports
    /// itself.
    ///
    /// Recorded because the two runtimes in play are not interchangeable: the
    /// isolate has to be driven on a current-thread runtime, while capability
    /// work has to stay on the application's, which is where the repositories
    /// and their connection pools were built.
    flavors: Mutex<Vec<&'static str>>,
    /// How long to stall before answering, so a run can be observed while it is
    /// still executing.
    stall: Option<Duration>,
}

impl Recorder {
    fn new() -> Self {
        Self::default()
    }

    /// A recorder that stalls before answering.
    fn stalling(delay: Duration) -> Self {
        Self {
            stall: Some(delay),
            ..Self::default()
        }
    }

    fn seen(&self) -> Vec<(HostOp, String)> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn flavors(&self) -> Vec<&'static str> {
        self.flavors.lock().expect("flavors lock").clone()
    }

    fn record(&self, op: HostOp, scope: &TenantScope) {
        self.flavors.lock().expect("flavors lock").push(
            match tokio::runtime::Handle::current().runtime_flavor() {
                tokio::runtime::RuntimeFlavor::CurrentThread => "current_thread",
                _ => "multi_thread",
            },
        );
        self.seen
            .lock()
            .expect("seen lock")
            .push((op, scope.storage_key()));
    }

    async fn stall(&self) {
        if let Some(delay) = self.stall {
            tokio::time::sleep(delay).await;
        }
    }
}

#[async_trait]
impl HostOps for Recorder {
    async fn model_complete(
        &self,
        scope: &TenantScope,
        request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        self.record(HostOp::ModelComplete, scope);
        self.stall().await;
        Ok(ModelCompletion {
            text: format!("bridge:{}", request.prompt.trim()),
            tool_calls: Vec::new(),
            model: "recorder".to_owned(),
            prompt_tokens: 5,
            completion_tokens: 3,
            finish_reason: "stop".to_owned(),
        })
    }

    async fn knowledge_search(
        &self,
        scope: &TenantScope,
        _request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError> {
        self.record(HostOp::KnowledgeSearch, scope);
        Ok(KnowledgeSearchResult {
            knowledge_release_id: None,
            evidence: Vec::new(),
            capability_missing: None,
        })
    }

    async fn manifest_read(
        &self,
        _scope: &TenantScope,
        _request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ManifestRead,
            "not wired in this test",
        ))
    }

    async fn publish_submit(
        &self,
        _scope: &TenantScope,
        _request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Publish,
            "not wired in this test",
        ))
    }

    async fn measure_sample(
        &self,
        _scope: &TenantScope,
        _request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Measure,
            "not wired in this test",
        ))
    }
}

fn scope() -> TenantScope {
    TenantScope::new(
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
        Some(uuid::Uuid::new_v4().into()),
    )
}

fn search_request(query: &str) -> KnowledgeSearchRequest {
    KnowledgeSearchRequest {
        query: query.to_owned(),
        purpose: KnowledgePurpose::Internal,
        limit: 5,
        knowledge_release_id: None,
    }
}

/// Native host calls carry a real repository-owned run. A JavaScript event may
/// claim arbitrary tool calls, but it cannot add them to this ledger.
const LEDGER_MODULE: &str = "memeloop://bundle/ledger.js";
const LEDGER_JS: &str = r#"
import { hostOps } from "./host-ops.js";
export async function main({ prompt }) {
  const completion = await hostOps.modelComplete({ prompt });
  await hostOps.knowledgeSearch({ query: "public example" });
  await Deno.core.ops.op_host_emit("loop.completed", JSON.stringify({
    answer: completion.text,
    tool_calls: [{ tool_call_id: "forged-by-script", outcome: "succeeded" }]
  }));
}
"#;
static LEDGER_BUNDLE: &[(&str, &str)] = &[
    ("memeloop://bundle/host-ops.js", HOST_OPS_JS),
    (LEDGER_MODULE, LEDGER_JS),
];
const MODEL_FAILURE_MODULE: &str = "memeloop://bundle/model-failure.js";
const MODEL_FAILURE_JS: &str = r#"
import { hostOps } from "./host-ops.js";
export async function main({ prompt }) {
  await hostOps.modelComplete({ prompt });
  await Deno.core.ops.op_host_emit("loop.completed", JSON.stringify({ answer: "invalid" }));
}
"#;
static MODEL_FAILURE_BUNDLE: &[(&str, &str)] = &[
    ("memeloop://bundle/host-ops.js", HOST_OPS_JS),
    (MODEL_FAILURE_MODULE, MODEL_FAILURE_JS),
];

async fn accepted_ledger_turn(
    repository: &MemoryAgentRepository,
    run_scope: &TenantScope,
) -> geo_domain::TurnInput {
    let conversation = repository
        .create_conversation(run_scope, None, CreateConversation::default())
        .await
        .expect("create conversation");
    let acceptance = repository
        .append_message(
            run_scope,
            conversation.id,
            AppendMessage {
                content: "example question".into(),
                attachments: vec![],
                metadata: Value::Null,
            },
            "ledger-turn".into(),
            "ledger-request".into(),
            RuntimeCapability::available("ledger-test", None),
        )
        .await
        .expect("accepted turn");
    assert_eq!(acceptance.run.status, RunStatus::Queued);
    repository
        .begin_run(run_scope, acceptance.run.id)
        .await
        .expect("begin run")
        .expect("queued run can be claimed");
    repository
        .load_turn_input(run_scope, conversation.id, acceptance.run.id)
        .await
        .expect("trusted persisted turn input")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_model_and_knowledge_invocations_are_scoped_and_recorded_once() {
    let repository = Arc::new(MemoryAgentRepository::default());
    let recorder = Arc::new(Recorder::new());
    let runtime = EmbeddedAgentRuntime::with_bundle(LEDGER_BUNDLE, LEDGER_MODULE, recorder.clone())
        .with_tool_call_repository(repository.clone());
    let run_scope = scope();
    let input = accepted_ledger_turn(&repository, &run_scope).await;
    let answer = runtime
        .run_turn(&run_scope, input.clone())
        .await
        .expect("real native host calls succeed");
    assert!(answer.content.starts_with("bridge:"));
    assert_eq!(recorder.seen().len(), 2);
    let entries = repository
        .list_tool_calls(&run_scope, input.run_id)
        .await
        .expect("scoped ledger");
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(entry.run_id, input.run_id);
        assert_eq!(entry.scope(), run_scope);
        assert_eq!(entry.outcome, ToolCallOutcome::Succeeded);
        assert_eq!(entry.attempt_count, 1);
        assert_eq!(entry.permission, geo_domain::ToolCallDecision::Allowed);
        assert_eq!(entry.budget, geo_domain::ToolCallDecision::Allowed);
        assert_eq!(entry.result_ref, None);
        assert_eq!(entry.cost_minor, None);
        assert_eq!(entry.currency, None);
        assert_eq!(entry.intent, serde_json::json!({"kind": "host_op"}));
        assert_ne!(entry.tool_call_id, "forged-by-script");
        assert_eq!(entry.arguments_hash.len(), 64);
        assert_eq!(entry.idempotency_key_hash.len(), 64);
    }
    assert!(
        entries
            .iter()
            .any(|entry| entry.tool_name == "model.complete.v1")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.tool_name == "knowledge.search.v1")
    );
    assert!(
        repository
            .list_tool_calls(
                &TenantScope::new(
                    run_scope.operator_id,
                    run_scope.tenant_id,
                    Some(uuid::Uuid::new_v4().into())
                ),
                input.run_id
            )
            .await
            .is_err(),
        "ledger never leaks through another project"
    );

    // Re-entering with the same claimed run is not a recovery protocol:
    // repeating the first host invocation must be refused before dispatch.
    let duplicate = runtime.run_turn(&run_scope, input.clone()).await;
    assert!(duplicate.is_err(), "a claimed host call cannot be replayed");
    assert_eq!(recorder.seen().len(), 2);
    let entries_after = repository
        .list_tool_calls(&run_scope, input.run_id)
        .await
        .unwrap();
    assert_eq!(entries_after, entries);

    let other_scope = TenantScope::new(
        run_scope.operator_id,
        run_scope.tenant_id,
        Some(uuid::Uuid::new_v4().into()),
    );
    let other_input = accepted_ledger_turn(&repository, &other_scope).await;
    assert_ne!(other_input.run_id, input.run_id);
    assert!(
        repository
            .list_tool_calls(&other_scope, other_input.run_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        runtime.run_turn(&other_scope, input).await.is_err(),
        "trusted run ID cannot be used in a different scope"
    );
    assert_eq!(recorder.seen().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_model_capability_records_failure_without_a_synthetic_result() {
    let repository = Arc::new(MemoryAgentRepository::default());
    let runtime = EmbeddedAgentRuntime::with_bundle(
        MODEL_FAILURE_BUNDLE,
        MODEL_FAILURE_MODULE,
        Arc::new(RepositoryHostOps::new(Arc::new(
            MemoryKnowledgeRepository::default(),
        ))),
    )
    .with_tool_call_repository(repository.clone());
    let run_scope = scope();
    let input = accepted_ledger_turn(&repository, &run_scope).await;
    let error = runtime
        .run_turn(&run_scope, input.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, geo_domain::ErrorCode::CapabilityMissing);
    let entries = repository
        .list_tool_calls(&run_scope, input.run_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].tool_name, "model.complete.v1");
    assert_eq!(entries[0].outcome, ToolCallOutcome::Failed);
    assert_eq!(entries[0].attempt_count, 1);
    assert!(entries[0].result_ref.is_none());
    assert!(entries[0].cost_minor.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_run_cannot_create_a_tool_intent_or_invoke_a_capability() {
    let repository = Arc::new(MemoryAgentRepository::default());
    let capabilities = Arc::new(Recorder::new());
    let runtime =
        EmbeddedAgentRuntime::with_bundle(LEDGER_BUNDLE, LEDGER_MODULE, capabilities.clone())
            .with_tool_call_repository(repository.clone());
    let run_scope = scope();
    let conversation = repository
        .create_conversation(&run_scope, None, CreateConversation::default())
        .await
        .unwrap();
    let queued = repository
        .append_message(
            &run_scope,
            conversation.id,
            AppendMessage {
                content: "question".into(),
                attachments: vec![],
                metadata: Value::Null,
            },
            "unclaimed".into(),
            "unclaimed-request".into(),
            RuntimeCapability::available("ledger-test", None),
        )
        .await
        .unwrap();
    let input = repository
        .load_turn_input(&run_scope, conversation.id, queued.run.id)
        .await
        .unwrap();
    assert!(runtime.run_turn(&run_scope, input).await.is_err());
    assert!(capabilities.seen().is_empty());
    assert!(
        repository
            .list_tool_calls(&run_scope, queued.run.id)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn login(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("host", "localhost:8080")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"login_name":"demo@localhost","password":"test-password"}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("login response");
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("cookie header")
        .split(';')
        .next()
        .expect("cookie value")
        .to_owned();
    let body = to_bytes(response.into_body(), 16 * 1024)
        .await
        .expect("login body");
    let body: Value = serde_json::from_slice(&body).expect("login json");
    (
        cookie,
        body["csrf_token"].as_str().expect("csrf token").to_owned(),
    )
}

fn request(
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    key: Option<&str>,
    body: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("origin", "http://localhost:5173")
        .header("cookie", cookie)
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        builder = builder.header(CSRF_HEADER, csrf);
    }
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

/// One submitted conversation, with what later requests need to observe it.
struct Submission {
    status: StatusCode,
    body: Value,
    conversation_id: String,
    project_id: uuid::Uuid,
}

/// Submits one message and returns the parsed acceptance.
async fn submit(app: &Router, cookie: &str, csrf: &str) -> Submission {
    let tenant_id = DEVELOPMENT_TENANT_ID;
    let project_id = uuid::Uuid::new_v4();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/agent/conversations?tenant_id={tenant_id}&project_id={project_id}"),
            cookie,
            Some(csrf),
            Some("conversation-1"),
            r#"{"title":"runtime assembly"}"#,
        ))
        .await
        .expect("create response");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(
        &to_bytes(created.into_body(), 64 * 1024)
            .await
            .expect("create body"),
    )
    .expect("create json");
    let conversation_id = created["id"].as_str().expect("conversation id").to_owned();

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/agent/conversations/{conversation_id}/messages?tenant_id={tenant_id}&project_id={project_id}"
            ),
            cookie,
            Some(csrf),
            Some("message-1"),
            r#"{"content":"how long is the warranty?"}"#,
        ))
        .await
        .expect("submit response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("submit body");
    Submission {
        status,
        body: serde_json::from_slice(&body).expect("submit json"),
        conversation_id,
        project_id,
    }
}

async fn conversation_detail(app: &Router, cookie: &str, submission: &Submission) -> Value {
    let tenant_id = DEVELOPMENT_TENANT_ID;
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/api/v1/agent/conversations/{}?tenant_id={tenant_id}&project_id={}",
                submission.conversation_id, submission.project_id
            ),
            cookie,
            None,
            None,
            "",
        ))
        .await
        .expect("detail response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("detail body");
    let detail: Value = serde_json::from_slice(&body).expect("detail json");
    assert_eq!(status, StatusCode::OK, "detail must be readable: {detail}");
    detail
}

fn newest_run_status(detail: &Value) -> Option<&str> {
    detail["runs"].as_array()?.last()?.get("status")?.as_str()
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "cancelled")
}

fn assistant_messages(detail: &Value) -> Vec<Value> {
    detail["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "assistant")
        .cloned()
        .collect()
}

/// Polls until the newest run's status satisfies `accept`.
///
/// The turn runs on its own task, so its progress is observed the way a client
/// observes it — through the durable record — rather than by whatever the
/// submission returned.
async fn await_run(
    app: &Router,
    cookie: &str,
    submission: &Submission,
    accept: impl Fn(&str) -> bool,
    what: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let detail = conversation_detail(app, cookie, submission).await;
        if newest_run_status(&detail).is_some_and(&accept) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "the run never became {what}: {detail}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

/// An unconfigured runtime is never available, and a run is refused rather than
/// started against a runtime that is not there.
#[tokio::test]
async fn an_unconfigured_runtime_is_never_available() {
    let runtime = EmbeddedAgentRuntime::unconfigured();
    assert!(!runtime.is_configured());
    assert_eq!(
        runtime.capability().await.status,
        geo_domain::RuntimeCapabilityStatus::Missing
    );
    let error = runtime
        .start(&scope(), HostOpBudgets::default())
        .expect_err("an unconfigured runtime must not start a run");
    assert!(
        error.to_string().contains("not configured"),
        "the rejection must name the missing configuration: {error}"
    );
}

/// A configured runtime reports itself available against the surface it
/// registers, so an operator can tell which capability set a run was accepted
/// against.
#[tokio::test]
async fn a_configured_runtime_reports_the_surface_it_registers() {
    let runtime = EmbeddedAgentRuntime::configured(Arc::new(Recorder::new()));
    assert!(runtime.is_configured());
    let capability = runtime.capability().await;
    assert!(
        capability.is_available(),
        "expected available: {capability:?}"
    );
    assert_eq!(capability.runtime, "deno_core");
    assert_eq!(capability.version.as_deref(), Some(HOST_OPS_VERSION));

    let mut presented = HostOp::ALL
        .iter()
        .map(|op| op.op_name().to_owned())
        .collect::<Vec<_>>();
    // The loop's own transport ops are not capabilities: they report progress
    // and checkpoint Rust-owned state, and open no access of their own.
    presented.push("op_host_emit".to_owned());
    presented.push("op_host_checkpoint".to_owned());
    presented.sort();
    assert_eq!(
        EmbeddedAgentRuntime::op_surface(),
        presented,
        "the assembled runtime must register exactly the declared surface"
    );
}

/// A deployment can choose a stricter isolate cap, but it cannot accidentally
/// configure a value that is too small for the approved runtime bootstrap.
#[test]
fn configured_runtime_rejects_an_unusable_v8_heap_limit() {
    let error = EmbeddedAgentRuntime::configured_with_heap_limit(
        Arc::new(Recorder::new()),
        EmbeddedAgentRuntime::MIN_V8_HEAP_LIMIT_BYTES - 1,
    )
    .expect_err("a V8 cap below the supported floor must be rejected");
    assert_eq!(error.stage, "configuration");
    assert!(
        error
            .message
            .contains(&EmbeddedAgentRuntime::MIN_V8_HEAP_LIMIT_BYTES.to_string()),
        "the validation failure must name the supported floor: {error}"
    );
}

/// A run started from the seam carries the run's scope and reaches the injected
/// capability: the model answer the UI is shown comes from the bridge, not from
/// anything the script could have synthesised.
#[tokio::test]
async fn a_started_run_reaches_the_injected_capability_under_the_run_scope() {
    let recorder = Arc::new(Recorder::new());
    let runtime =
        EmbeddedAgentRuntime::with_bundle(SCENARIO_BUNDLE, HOST_MAIN_MODULE, recorder.clone());
    let run_scope = scope();
    let mut run = runtime
        .start(&run_scope, HostOpBudgets::default())
        .expect("a configured runtime must start a run");

    run.evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("the reference turn must complete");

    assert_eq!(
        recorder.seen(),
        vec![
            (HostOp::KnowledgeSearch, run_scope.storage_key()),
            (HostOp::ModelComplete, run_scope.storage_key()),
        ],
        "every op must reach the bridge under the run's own scope"
    );
    assert_eq!(run.op_calls(HostOp::KnowledgeSearch), 1);
    assert_eq!(run.op_calls(HostOp::ModelComplete), 1);

    let completed = run
        .host_state()
        .events
        .iter()
        .find(|event| event.topic == "loop.completed")
        .map(|event| event.payload.clone())
        .expect("the reference loop must report completion");
    let completed: Value = serde_json::from_str(&completed).expect("a completion payload");
    assert_eq!(
        completed["answer"], "bridge:how long is the warranty?",
        "the answer shown must be the one the bridge produced: {completed}"
    );
    assert_eq!(completed["model"], "recorder");
}

/// A bounded isolate remains suitable for an ordinary reference turn.  This
/// covers the direct `start` seam, where callers drive the isolate themselves.
#[tokio::test]
async fn a_heap_capped_started_run_still_completes_normally() {
    let recorder = Arc::new(Recorder::new());
    let runtime = EmbeddedAgentRuntime::with_bundle_and_heap_limit(
        SCENARIO_BUNDLE,
        HOST_MAIN_MODULE,
        recorder,
        TEST_V8_HEAP_LIMIT_BYTES,
    )
    .expect("the supported minimum must build an isolate");
    let mut run = runtime
        .start(&scope(), HostOpBudgets::default())
        .expect("a heap-capped runtime must start");

    run.evaluate_module(SCENARIO_MODULE, GENEROUS_DEADLINE)
        .await
        .expect("an ordinary turn must complete below the heap cap");
    assert!(
        run.host_state()
            .events
            .iter()
            .any(|event| event.topic == "loop.completed"),
        "the capped turn must report normal completion"
    );
}

/// The child half of the crash-regression test.  It is a no-op in the ordinary
/// test run and is invoked by
/// [`heap_runaway_returns_a_failure_without_terminating_the_test_runner`].
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heap_runaway_child_returns_a_recoverable_failure() {
    if std::env::var_os(HEAP_RUNAWAY_CHILD_ENV).is_none() {
        return;
    }

    let runtime = EmbeddedAgentRuntime::with_bundle_and_heap_limit(
        HEAP_RUNAWAY_BUNDLE,
        HEAP_RUNAWAY_MODULE,
        Arc::new(Recorder::new()),
        TEST_V8_HEAP_LIMIT_BYTES,
    )
    .expect("the supported minimum must build an isolate");
    let error = runtime
        .run_turn(
            &scope(),
            geo_domain::TurnInput {
                conversation_id: uuid::Uuid::new_v4().into(),
                message_id: uuid::Uuid::new_v4().into(),
                turn_id: uuid::Uuid::new_v4().into(),
                run_id: uuid::Uuid::new_v4().into(),
                prompt: "allocate until V8 stops this turn".to_owned(),
                attachments: Vec::new(),
                history: Vec::new(),
                history_omitted_turns: 0,
            },
        )
        .await
        .expect_err("heap exhaustion must fail the turn");
    assert!(
        error.message.contains("terminated"),
        "V8 must unwind the heap limit as a recoverable termination: {error}"
    );
}

/// A heap runaway executes in a separately launched test process.  The parent
/// holds a wall-clock deadline and kills only that exact child if it hangs; a
/// V8 abort therefore becomes a normal test failure instead of killing the
/// broader test runner.
#[test]
fn heap_runaway_returns_a_failure_without_terminating_the_test_runner() {
    let executable = std::env::current_exe().expect("the integration test executable");
    let mut child = std::process::Command::new(executable)
        .args([
            "--exact",
            "heap_runaway_child_returns_a_recoverable_failure",
            "--nocapture",
        ])
        .env(HEAP_RUNAWAY_CHILD_ENV, "1")
        .spawn()
        .expect("the heap regression child must start");
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if let Some(status) = child.try_wait().expect("the heap child status") {
            assert!(
                status.success(),
                "the heap child must report a recoverable turn failure, not abort: {status}"
            );
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the heap child did not return a recoverable failure before {deadline:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The four ops this process cannot yet honour report a typed
/// `capability_missing`.  A synthetic result here would be indistinguishable
/// from a real one once it reached a user.
#[tokio::test]
async fn unimplemented_host_ops_report_capability_missing_rather_than_a_result() {
    let ops = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()));
    let run_scope = scope();

    let completion = ModelCompletionRequest {
        prompt: "how long is the warranty?".to_owned(),
        system: None,
        model: None,
        max_output_tokens: None,
        messages: Vec::new(),
        tools: Vec::new(),
    };
    let manifest = ManifestReadRequest {
        manifest_id: None,
        kind: ManifestKind::Distribution,
        revision: None,
        cursor: None,
        limit: None,
    };
    let publish = PublishRequest {
        publication_intent_id: uuid::Uuid::new_v4(),
        document_revision_id: uuid::Uuid::new_v4(),
        platform_target_id: uuid::Uuid::new_v4(),
        payload_sha256: geo_domain::sha256_hex(b"the warranty runs for twenty-four months"),
        body: "the warranty runs for twenty-four months".to_owned(),
    };
    let measure = MeasureRequest {
        measurement_protocol_id: uuid::Uuid::new_v4(),
        scheduled_sample_id: uuid::Uuid::new_v4(),
        question: "how long is the warranty?".to_owned(),
        channel: "chatgpt".to_owned(),
        surface: MeasurementSurface::ConsumerWeb,
    };

    let failures = [
        (
            HostOp::ModelComplete,
            ops.model_complete(&run_scope, completion).await.err(),
        ),
        (
            HostOp::ManifestRead,
            ops.manifest_read(&run_scope, manifest).await.err(),
        ),
        (
            HostOp::Publish,
            ops.publish_submit(&run_scope, publish).await.err(),
        ),
        (
            HostOp::Measure,
            ops.measure_sample(&run_scope, measure).await.err(),
        ),
    ];

    for (op, error) in failures {
        let error = error.unwrap_or_else(|| panic!("{} must not return a result", op.name()));
        assert_eq!(error.op, op);
        assert_eq!(error.code, HostOpErrorCode::CapabilityMissing, "{error:?}");
        assert!(
            !error.message.is_empty(),
            "a gap must be explained: {error:?}"
        );
    }
}

/// The one op the process does honour is delegated to the repository, and a
/// repository refusal arrives as a typed failure rather than a plausible empty
/// result.
#[tokio::test]
async fn knowledge_search_delegates_to_the_repository_and_types_its_failures() {
    let ops = RepositoryHostOps::new(Arc::new(MemoryKnowledgeRepository::default()));
    let run_scope = scope();

    let empty = ops
        .knowledge_search(&run_scope, search_request("warranty"))
        .await
        .expect("a scoped search with no release is an empty result, not a failure");
    assert!(empty.evidence.is_empty());
    assert!(empty.capability_missing.is_none());

    let refused = ops
        .knowledge_search(&run_scope, search_request("  "))
        .await
        .expect_err("an empty query must be refused");
    assert_eq!(refused.op, HostOp::KnowledgeSearch);
    assert_eq!(refused.code, HostOpErrorCode::InvalidRequest, "{refused:?}");

    let unscoped = TenantScope::new(run_scope.operator_id, run_scope.tenant_id, None);
    let refused = ops
        .knowledge_search(&unscoped, search_request("warranty"))
        .await
        .expect_err("a search without a project must be refused");
    assert_eq!(refused.code, HostOpErrorCode::InvalidRequest, "{refused:?}");
}

#[tokio::test]
async fn knowledge_text_tools_share_the_existing_revision_repository_and_scope() {
    let knowledge = Arc::new(MemoryKnowledgeRepository::default());
    let run_scope = scope();
    let imported = knowledge
        .import_batch(
            &run_scope,
            vec![ImportItem {
                client_item_id: "knowledge-tool-test".into(),
                kind: SourceKind::Text,
                name: "Reference".into(),
                purpose: KnowledgePurpose::Internal,
                text: Some("Original text".into()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .expect("existing knowledge import");
    let item = &imported.items[0];
    let source = item.source.as_ref().expect("source");
    let version = item.source_version.as_ref().expect("version");
    let ops = RepositoryHostOps::new(knowledge.clone());
    let read = ops
        .knowledge_text_read(
            &run_scope,
            KnowledgeTextReadRequest {
                source_id: source.source_id,
                source_version_id: version.source_version_id,
            },
        )
        .await
        .expect("scoped original read");
    assert_eq!(read.content.text, "Original text");
    assert_eq!(
        read.source.current_version_id,
        Some(version.source_version_id)
    );
    let command = KnowledgeTextReviseRequest {
        source_id: source.source_id,
        expected_revision: source.revision,
        idempotency_key: "knowledge-tool-retry".into(),
        base_version_id: version.source_version_id,
        media_type: "text/markdown".into(),
        text: "# Revised\n\nExact 中文".into(),
    };
    let receipt = ops
        .knowledge_text_revise(&run_scope, command.clone())
        .await
        .expect("persisted revision");
    let replay = ops
        .knowledge_text_revise(&run_scope, command.clone())
        .await
        .expect("same request replays the original receipt");
    assert_eq!(receipt, replay);
    assert_eq!(
        receipt.source_version.parent_version_id,
        Some(version.source_version_id)
    );
    let current = ops
        .knowledge_text_read(
            &run_scope,
            KnowledgeTextReadRequest {
                source_id: source.source_id,
                source_version_id: receipt.source_version.source_version_id,
            },
        )
        .await
        .expect("read persisted text");
    assert_eq!(current.content.text, command.text);
    assert_eq!(
        current.content.text_basis,
        geo_domain::SourceTextBasis::Exact
    );
    let stale = ops
        .knowledge_text_revise(
            &run_scope,
            KnowledgeTextReviseRequest {
                idempotency_key: "new-operation".into(),
                ..command
            },
        )
        .await
        .expect_err("new operation on stale revision conflicts");
    assert_eq!(stale.code, HostOpErrorCode::Conflict);
    assert_eq!(stale.message, "source_revision_conflict");
    let foreign = TenantScope::new(
        run_scope.operator_id,
        run_scope.tenant_id,
        Some(uuid::Uuid::new_v4().into()),
    );
    let denied = ops
        .knowledge_text_read(
            &foreign,
            KnowledgeTextReadRequest {
                source_id: source.source_id,
                source_version_id: receipt.source_version.source_version_id,
            },
        )
        .await
        .expect_err("foreign project cannot read source");
    assert_eq!(denied.code, HostOpErrorCode::NotFound);
}

// ---------------------------------------------------------------------------
// The assembly
// ---------------------------------------------------------------------------

/// An unconfigured assembly is honest end to end: the run is accepted, durably
/// failed with `capability_missing`, and the response carries no field through
/// which a reply could be shown.
#[tokio::test]
async fn an_unconfigured_assembly_fails_the_run_with_capability_missing() {
    let app = router(
        AppState::development_with_password("test-password")
            .with_agent_runtime(Arc::new(EmbeddedAgentRuntime::unconfigured())),
    );
    let (cookie, csrf) = login(&app).await;
    let submission = submit(&app, &cookie, &csrf).await;
    let submitted = &submission.body;

    assert_eq!(submission.status, StatusCode::ACCEPTED);
    assert_eq!(submitted["status"], "accepted");
    assert_eq!(submitted["run_status"], "failed");
    assert_eq!(submitted["error"]["code"], "capability_missing");
    assert_eq!(
        submitted["error"]["message"], "embedded JavaScript runtime is not configured",
        "the reason must be the configured-absence one: {submitted}"
    );
    for absent in ["content", "answer", "completion"] {
        assert!(
            submitted.get(absent).is_none(),
            "the acceptance must carry no {absent} field: {submitted}"
        );
    }
}

/// A configured assembly does not merely accept the run: the turn is driven to
/// a terminal state and its answer is durably recorded, so the run's later state
/// is the same one a client polls for.
///
/// The acceptance itself is asserted separately: the HTTP response describes
/// acceptance, so it must not optimistically claim the run has started.
///
/// The multi-threaded flavor is part of the fixture, not incidental.  The
/// application's own runtime is multi-threaded, and the isolate has to be moved
/// to a current-thread one to be driven at all — so this is the arrangement a
/// real deployment runs, and the flavor assertion below is what keeps the two
/// halves on their own sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_assembly_runs_the_turn_to_a_recorded_answer() {
    let recorder = Arc::new(Recorder::new());
    let app = router(
        AppState::development_with_password("test-password")
            .with_agent_runtime(Arc::new(EmbeddedAgentRuntime::configured(recorder.clone()))),
    );
    let (cookie, csrf) = login(&app).await;
    let submission = submit(&app, &cookie, &csrf).await;

    assert_eq!(submission.status, StatusCode::ACCEPTED);
    assert_eq!(submission.body["status"], "accepted");
    assert_eq!(
        submission.body["run_status"], "queued",
        "a runtime that is configured must not fail the run for want of one: {}",
        submission.body
    );
    assert!(
        submission.body.get("error").is_none(),
        "an accepted run must not carry a capability error: {}",
        submission.body
    );

    let detail = await_run(&app, &cookie, &submission, is_terminal, "terminal").await;
    assert_eq!(
        newest_run_status(&detail),
        Some("succeeded"),
        "the reference bundle must complete a turn: {detail}"
    );
    let answers = assistant_messages(&detail);
    assert_eq!(
        answers.len(),
        1,
        "exactly one answer must be recorded: {detail}"
    );
    assert_eq!(answers[0]["content"], "bridge:how long is the warranty?");
    let flavors = recorder.flavors();
    assert!(
        !flavors.is_empty(),
        "the turn must have reached the bridge at all"
    );
    assert!(
        flavors.iter().all(|flavor| *flavor == "multi_thread"),
        "capability work must be polled on the application's runtime, not on the isolate's \
         own: a pooled connection stays bound to the runtime that created it; saw {flavors:?}"
    );
}

/// The anti-fabrication guarantee, end to end.  A runtime that is assembled and
/// available, but whose bridge cannot answer, must fail the run with the typed
/// capability error and record **no** assistant message: an empty answer would
/// be indistinguishable from a fabricated one in the UI.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_available_runtime_that_cannot_answer_records_no_message() {
    let app = router(
        AppState::development_with_password("test-password").with_agent_runtime(Arc::new(
            EmbeddedAgentRuntime::configured(Arc::new(RepositoryHostOps::new(Arc::new(
                MemoryKnowledgeRepository::default(),
            )))),
        )),
    );
    let (cookie, csrf) = login(&app).await;
    let submission = submit(&app, &cookie, &csrf).await;
    assert_eq!(submission.status, StatusCode::ACCEPTED);
    assert_eq!(
        submission.body["run_status"], "queued",
        "the capability is available, so the run is accepted: {}",
        submission.body
    );

    let detail = await_run(&app, &cookie, &submission, is_terminal, "terminal").await;
    assert_eq!(
        newest_run_status(&detail),
        Some("failed"),
        "a turn that reached no provider must fail: {detail}"
    );
    let run = &detail["runs"].as_array().expect("runs")[0];
    assert_eq!(
        run["error"]["code"], "capability_missing",
        "the gap must be named, not an opaque failure: {run}"
    );
    assert!(
        assistant_messages(&detail).is_empty(),
        "a failed turn must record no answer at all: {detail}"
    );
}

/// Cancelling a turn that is already executing leaves the run cancelled and
/// writes no answer, so a late completion cannot overwrite the user's decision.
///
/// The turn is held mid-flight by the recorder, cancelled there, and only then
/// allowed to finish — so the assertions below read the state *after* the late
/// completion has landed and been discarded, not merely before it existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_an_executing_turn_outranks_its_completion() {
    let app = router(
        AppState::development_with_password("test-password").with_agent_runtime(Arc::new(
            EmbeddedAgentRuntime::configured(Arc::new(Recorder::stalling(STALL))),
        )),
    );
    let (cookie, csrf) = login(&app).await;
    let submission = submit(&app, &cookie, &csrf).await;
    assert_eq!(submission.status, StatusCode::ACCEPTED);
    let running = await_run(
        &app,
        &cookie,
        &submission,
        |status| status == "running",
        "running",
    )
    .await;

    let tenant_id = DEVELOPMENT_TENANT_ID;
    let turn_id = running["turns"]
        .as_array()
        .expect("turns")
        .last()
        .expect("a turn")["id"]
        .as_str()
        .expect("turn id")
        .to_owned();
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/agent/turns/{turn_id}/cancel?tenant_id={tenant_id}&project_id={}",
                submission.project_id
            ),
            &cookie,
            Some(&csrf),
            Some("cancel-1"),
            "",
        ))
        .await
        .expect("cancel response");
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "the cancellation must be accepted while the turn is still executing"
    );

    // Let the stalled turn run to its end.  The isolate outlives the run it
    // produced, so this also keeps the test's runtime alive until the engine has
    // unwound rather than tearing it down underneath a live deadline timer.
    tokio::time::sleep(STALL + Duration::from_millis(200)).await;

    let detail = conversation_detail(&app, &cookie, &submission).await;
    assert_eq!(
        newest_run_status(&detail),
        Some("cancelled"),
        "the cancellation must outrank the completion that arrived after it: {detail}"
    );
    assert!(
        assistant_messages(&detail).is_empty(),
        "a cancelled turn must not record an answer: {detail}"
    );
}

fn cancellation_input() -> geo_domain::TurnInput {
    geo_domain::TurnInput {
        conversation_id: uuid::Uuid::new_v4().into(),
        message_id: uuid::Uuid::new_v4().into(),
        turn_id: uuid::Uuid::new_v4().into(),
        run_id: uuid::Uuid::new_v4().into(),
        prompt: "isolated cancellation".to_owned(),
        attachments: Vec::new(),
        history: Vec::new(),
        history_omitted_turns: 0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_interrupts_a_spinning_isolate_without_affecting_a_new_turn() {
    let runtime =
        EmbeddedAgentRuntime::with_bundle(SPIN_BUNDLE, SPIN_MODULE, Arc::new(Recorder::new()));
    let spin_scope = scope();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let started = Instant::now();
    let task = tokio::spawn(async move {
        runtime
            .run_turn_with_cancellation(&spin_scope, cancellation_input(), flag)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancelled.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("a spinning isolate must stop promptly")
        .expect("turn task must not panic");
    assert!(
        result.is_err(),
        "cancelled JavaScript cannot report success"
    );
    assert!(started.elapsed() < Duration::from_secs(2));

    let clean = EmbeddedAgentRuntime::configured(Arc::new(Recorder::new()));
    let answer = clean
        .run_turn(&scope(), cancellation_input())
        .await
        .expect("a new run must use its own cancellation state");
    assert!(answer.content.starts_with("bridge:"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_interrupts_a_suspended_model_op_without_waiting_for_its_budget() {
    let recorder = Arc::new(Recorder::stalling(Duration::from_secs(5)));
    let runtime = EmbeddedAgentRuntime::configured(recorder.clone());
    let scope = scope();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let task = tokio::spawn(async move {
        runtime
            .run_turn_with_cancellation(&scope, cancellation_input(), flag)
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while !recorder
        .seen()
        .iter()
        .any(|(op, _)| *op == HostOp::ModelComplete)
    {
        assert!(Instant::now() < deadline, "model op never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancelled.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("a suspended model op must stop before its five-second budget")
        .expect("turn task must not panic");
    assert!(result.is_err(), "cancelled model op cannot report success");
    assert_eq!(
        recorder
            .seen()
            .iter()
            .filter(|(op, _)| *op == HostOp::ModelComplete)
            .count(),
        1,
        "no additional model calls after cancellation"
    );
}
