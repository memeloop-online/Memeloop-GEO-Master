//! The application assembly of the embedded JavaScript runtime.
//!
//! Two guarantees are pinned here.  A configured runtime is the one the API
//! actually consults, and a run's JavaScript reaches the injected capability
//! under the run's own scope.  An unconfigured runtime still yields the explicit
//! `capability_missing` result the API has always produced, with no field
//! through which a fabricated reply could reach the UI.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::SET_COOKIE},
};
use geo_api::{AppState, CSRF_HEADER, EmbeddedAgentRuntime, RepositoryHostOps, router};
use geo_domain::{
    AgentRuntime, DEVELOPMENT_TENANT_ID, KnowledgePurpose, KnowledgeSearchRequest,
    MemoryKnowledgeRepository, TenantScope,
};
use geo_worker::{
    HOST_LOOP_JS, HOST_MAIN_MODULE, HOST_OPS_JS, HOST_OPS_VERSION, HostOp, HostOpBudgets,
    HostOpError, HostOpErrorCode, HostOps, KnowledgeSearchResult, ManifestKind, ManifestPage,
    ManifestReadRequest, MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest,
    PublishReceipt, PublishRequest,
};
use serde_json::Value;
use tower::ServiceExt;

const GENEROUS_DEADLINE: Duration = Duration::from_secs(30);
const SCENARIO_MODULE: &str = "memeloop://bundle/scenario.js";

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
    };
    let manifest = ManifestReadRequest {
        kind: ManifestKind::Document,
        revision: None,
        cursor: None,
        limit: None,
    };
    let publish = PublishRequest {
        document_revision_id: uuid::Uuid::new_v4(),
        platform_target_id: uuid::Uuid::new_v4(),
        body: "the warranty runs for twenty-four months".to_owned(),
    };
    let measure = MeasureRequest {
        measurement_protocol_id: uuid::Uuid::new_v4(),
        question: "how long is the warranty?".to_owned(),
        channel: "chatgpt".to_owned(),
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
