//! Compatibility probe for the generated, self-contained MemeLoop ESM bundle.
//!
//! The artifact is deliberately generated rather than checked in. Run this
//! test explicitly after `pnpm agent:bundle`; its ignored status makes a
//! missing local artifact visible instead of treating the probe as a passing
//! no-op in ordinary Rust test runs.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use geo_domain::{
    AppError, AttachmentId, AttachmentReference, ImportStatus, KnowledgeSearchRequest,
    KnowledgeSearchResult, TenantScope,
};
use geo_worker::{
    HostBridge, HostOp, HostOpError, HostOps, HostRuntime, KnowledgeImportAttachmentResultItem,
    KnowledgeImportAttachmentsRequest, KnowledgeImportAttachmentsResult, ManifestPage,
    ManifestReadRequest, MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest,
    PublishReceipt, PublishRequest, ReportGetRequest, ReportPreviewRequest, ReportReduceRequest,
};

const BUNDLE_SPECIFIER: &str = "memeloop://bundle/memeloop-agent-loop.bundle.mjs";
const TURN_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct RecordingHostOps {
    model_calls: Mutex<Vec<(String, ModelCompletionRequest)>>,
    search_calls: Mutex<Vec<(String, KnowledgeSearchRequest)>>,
    import_calls: Mutex<Vec<(String, KnowledgeImportAttachmentsRequest)>>,
    report_calls: Mutex<Vec<(String, String)>>,
    tool_turn: bool,
    import_turn: bool,
    report_turn: bool,
    preview_turn: bool,
}

impl RecordingHostOps {
    fn model_calls(&self) -> Vec<(String, ModelCompletionRequest)> {
        self.model_calls
            .lock()
            .expect("model call recorder must not be poisoned")
            .clone()
    }

    fn search_calls(&self) -> Vec<(String, KnowledgeSearchRequest)> {
        self.search_calls
            .lock()
            .expect("search call recorder must not be poisoned")
            .clone()
    }

    fn unavailable(op: HostOp) -> HostOpError {
        HostOpError::capability_missing(
            op,
            "this compatibility probe only exposes a model completion provider",
        )
    }
}

#[async_trait]
impl HostOps for RecordingHostOps {
    async fn model_complete(
        &self,
        scope: &TenantScope,
        request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let mut calls = self
            .model_calls
            .lock()
            .expect("model call recorder must not be poisoned");
        calls.push((scope.storage_key(), request));
        if self.preview_turn && calls.len() == 1 {
            return Ok(serde_json::from_value(serde_json::json!({
                "text": "",
                "tool_calls": [{
                    "id": "report-preview-1",
                    "type": "function",
                    "function": { "name": "report_preview", "arguments": "{}" }
                }],
                "model": "probe-model",
                "prompt_tokens": 7,
                "completion_tokens": 4,
                "finish_reason": "tool_calls"
            }))
            .expect("preview completion fixture must match the host DTO"));
        }
        if self.report_turn && calls.len() <= 2 {
            let first = calls.len() == 1;
            return Ok(serde_json::from_value(serde_json::json!({
                "text": "",
                "tool_calls": [{
                    "id": if first { "report-reduce-1" } else { "report-get-2" },
                    "type": "function",
                    "function": {
                        "name": if first { "report_reduce" } else { "report_get" },
                        "arguments": "{}"
                    }
                }],
                "model": "probe-model",
                "prompt_tokens": 7,
                "completion_tokens": 4,
                "finish_reason": "tool_calls"
            }))
            .expect("report completion fixture must match the host DTO"));
        }
        if self.import_turn && calls.len() <= 2 {
            let first = calls.len() == 1;
            return Ok(serde_json::from_value(serde_json::json!({
                "text": "",
                "tool_calls": [{
                    "id": if first { "import-call-1" } else { "search-call-2" },
                    "type": "function",
                    "function": if first {
                        serde_json::json!({
                            "name": "knowledge_import_attachments",
                            "arguments": format!(
                                "{{\"items\":[{{\"attachment_id\":\"{}\",\"purpose\":\"internal\"}},{{\"attachment_id\":\"{}\",\"purpose\":\"internal\"}}]}}",
                                uuid::Uuid::from_u128(40), uuid::Uuid::from_u128(41)
                            )
                        })
                    } else {
                        serde_json::json!({
                            "name": "knowledge_search",
                            "arguments": format!(
                                "{{\"query\":\"warranty\",\"knowledge_release_id\":\"{}\"}}",
                                uuid::Uuid::from_u128(50)
                            )
                        })
                    }
                }],
                "model": "probe-model",
                "prompt_tokens": 7,
                "completion_tokens": 4,
                "finish_reason": "tool_calls"
            }))
            .expect("import completion fixture must match the host DTO"));
        }
        if self.tool_turn && calls.len() == 1 {
            return Ok(serde_json::from_value(serde_json::json!({
                "text": "",
                "tool_calls": [{
                    "id": "search-call-1",
                    "type": "function",
                    "function": {
                        "name": "knowledge_search",
                        "arguments": "{\"query\":\"warranty\",\"limit\":2}"
                    }
                }],
                "model": "probe-model",
                "prompt_tokens": 7,
                "completion_tokens": 4,
                "finish_reason": "tool_calls"
            }))
            .expect("tool completion fixture must match the host DTO"));
        }
        Ok(ModelCompletion {
            text: if self.preview_turn {
                "Temporary preview with a coverage gap.".to_owned()
            } else if self.report_turn {
                "Report available with a coverage gap.".to_owned()
            } else if self.tool_turn || self.import_turn {
                "The warranty lasts two years (Manual).".to_owned()
            } else {
                "The warranty lasts two years.".to_owned()
            },
            tool_calls: Vec::new(),
            model: "probe-model".to_owned(),
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "stop".to_owned(),
        })
    }

    async fn knowledge_search(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError> {
        if !self.tool_turn && !self.import_turn {
            return Err(Self::unavailable(HostOp::KnowledgeSearch));
        }
        self.search_calls
            .lock()
            .expect("search call recorder must not be poisoned")
            .push((scope.storage_key(), request));
        Ok(serde_json::from_value(serde_json::json!({
            "knowledge_release_id": null,
            "evidence": [{
                "source_id": uuid::Uuid::from_u128(10),
                "source_version_id": uuid::Uuid::from_u128(11),
                "chunk_id": uuid::Uuid::from_u128(12),
                "source_name": "Manual",
                "purpose": "public",
                "locator": {
                    "kind": "text",
                    "start_line": 1,
                    "end_line": 1,
                    "start_char": 0,
                    "end_char": 9
                },
                "text": "Two years",
                "quote": "Two years"
            }],
            "capability_missing": null
        }))
        .expect("evidence fixture must match the domain DTO"))
    }

    async fn knowledge_import_attachments(
        &self,
        scope: &TenantScope,
        request: KnowledgeImportAttachmentsRequest,
        attachments: &[AttachmentReference],
    ) -> Result<KnowledgeImportAttachmentsResult, HostOpError> {
        assert_eq!(attachments.len(), 2);
        self.import_calls
            .lock()
            .expect("import call recorder must not be poisoned")
            .push((scope.storage_key(), request.clone()));
        Ok(KnowledgeImportAttachmentsResult {
            items: request
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| KnowledgeImportAttachmentResultItem {
                    attachment_id: item.attachment_id,
                    import_job_id: None,
                    status: if index == 0 {
                        ImportStatus::Succeeded
                    } else {
                        ImportStatus::Failed
                    },
                    source_id: (index == 0).then_some(uuid::Uuid::from_u128(48)),
                    source_version_id: (index == 0).then_some(uuid::Uuid::from_u128(49)),
                    knowledge_release_id: (index == 0).then_some(uuid::Uuid::from_u128(50)),
                    error: (index == 1).then(|| AppError::capability_missing("parser unavailable")),
                })
                .collect(),
        })
    }

    async fn manifest_read(
        &self,
        _scope: &TenantScope,
        _request: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        Err(Self::unavailable(HostOp::ManifestRead))
    }

    async fn publish_submit(
        &self,
        _scope: &TenantScope,
        _request: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        Err(Self::unavailable(HostOp::Publish))
    }

    async fn measure_sample(
        &self,
        _scope: &TenantScope,
        _request: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        Err(Self::unavailable(HostOp::Measure))
    }

    async fn report_reduce(
        &self,
        scope: &TenantScope,
        request: ReportReduceRequest,
    ) -> Result<geo_domain::ReportSnapshot, HostOpError> {
        self.report_calls.lock().unwrap().push((
            scope.storage_key(),
            format!("reduce:{:?}", request.cycle_id),
        ));
        assert_eq!(request.correction_of, None);
        Ok(report_fixture())
    }

    async fn report_get(
        &self,
        scope: &TenantScope,
        request: ReportGetRequest,
    ) -> Result<geo_domain::ReportSnapshot, HostOpError> {
        self.report_calls
            .lock()
            .unwrap()
            .push((scope.storage_key(), format!("get:{:?}", request.report_id)));
        Ok(report_fixture())
    }

    async fn report_preview(
        &self,
        scope: &TenantScope,
        request: ReportPreviewRequest,
    ) -> Result<geo_domain::ReportPreview, HostOpError> {
        self.report_calls.lock().unwrap().push((
            scope.storage_key(),
            format!("preview:{:?}", request.cycle_id),
        ));
        let mut value = serde_json::to_value(report_fixture()).unwrap();
        let fields = value.as_object_mut().unwrap();
        fields.remove("report_id");
        fields.remove("revision");
        fields.remove("correction_of");
        fields.insert("kind".to_owned(), serde_json::json!("preview"));
        fields.insert(
            "project_id".to_owned(),
            serde_json::to_value(scope.project_id.expect("project scope")).unwrap(),
        );
        serde_json::from_value(value).map_err(|_| {
            HostOpError::internal(
                HostOp::ReportPreview,
                "fixture preview could not be created",
            )
        })
    }
}

fn report_fixture() -> geo_domain::ReportSnapshot {
    let unavailable = serde_json::json!({
        "availability": "unavailable", "expected_count": null,
        "observed_count": 0, "counts": {}, "reason": "no frozen source"
    });
    serde_json::from_value(serde_json::json!({
        "report_id": uuid::Uuid::from_u128(32),
        "project_id": uuid::Uuid::from_u128(3),
        "cycle_id": uuid::Uuid::from_u128(31),
        "revision": 1,
        "correction_of": null,
        "report_window_start_at": "2026-09-01T00:00:00Z",
        "report_window_end_at": "2026-09-08T00:00:00Z",
        "report_timezone": "UTC",
        "cutoff_at": "2026-09-08T00:00:00Z",
        "evidence_as_of": "2026-09-08T00:00:00Z",
        "generated_at": "2026-09-08T00:00:00Z",
        "reducer_version": "test",
        "input_hash": "test-hash",
        "status": "partial",
        "input_manifest_versions": [],
        "documents": unavailable,
        "publications": unavailable,
        "measurements": unavailable,
        "publication_groups": [],
        "measurement_groups": [],
        "findings": [],
        "evidence": []
    }))
    .expect("report fixture must match the domain DTO")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test memeloop_bundle -- --ignored`"]
async fn durable_history_reaches_model_in_fresh_v8_isolates_without_duplication() {
    let source = generated_bundle();
    let bundle = [(BUNDLE_SPECIFIER, source.as_str())];
    let input = serde_json::json!({
        "conversation_id": "history-conversation",
        "message_id": "current-message",
        "turn_id": "current-turn",
        "run_id": "current-run",
        "prompt": "What did you say?",
        "timestamp": 1_700_000_000_000_u64,
        "history_omitted_turns": 2,
        "history": [
            {"message_id":"prior-user","root_message_id":"prior-user",
             "sequence":1,"role":"user","content":"Remember the warranty"},
            {"message_id":"prior-answer","root_message_id":"prior-user",
             "sequence":2,"role":"assistant","content":"Two years"}
        ]
    })
    .to_string();
    for _ in 0..2 {
        let provider = Arc::new(RecordingHostOps::default());
        let scope = test_scope();
        let expected_scope = scope.storage_key();
        let bridge = HostBridge::new(
            Arc::clone(&provider) as Arc<dyn HostOps>,
            scope,
            tokio::runtime::Handle::current(),
        );
        let mut runtime = HostRuntime::new(&bundle, bridge, Some(64 * 1024 * 1024)).unwrap();
        runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        runtime
            .call_main(BUNDLE_SPECIFIER, &input, TURN_DEADLINE)
            .await
            .unwrap();
        let calls = provider.model_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, expected_scope);
        let transcript: Vec<_> = calls[0]
            .1
            .messages
            .iter()
            .filter(|message| message.role == "user" || message.role == "assistant")
            .map(|message| {
                (
                    message.role.as_str(),
                    message.content.as_deref().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            transcript,
            vec![
                ("user", "Remember the warranty"),
                ("assistant", "Two years"),
                ("user", "What did you say?"),
            ]
        );
        assert!(
            !calls[0]
                .1
                .tools
                .iter()
                .any(|tool| tool.function.name == "knowledge_import_attachments")
        );
        let completed = runtime
            .host_state()
            .events
            .into_iter()
            .find(|event| event.topic == "loop.completed")
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&completed.payload).unwrap();
        assert_eq!(payload["history_omitted_turns"], 2);
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test memeloop_bundle -- --ignored`"]
async fn generated_memeloop_bundle_reduces_and_reads_a_report_through_rust() {
    let source = generated_bundle();
    let bundle = [(BUNDLE_SPECIFIER, source.as_str())];
    let provider = Arc::new(RecordingHostOps {
        report_turn: true,
        ..Default::default()
    });
    let scope = test_scope();
    let expected_scope = scope.storage_key();
    let bridge = HostBridge::new(
        Arc::clone(&provider) as Arc<dyn HostOps>,
        scope,
        tokio::runtime::Handle::current(),
    );
    let mut runtime = HostRuntime::new(&bundle, bridge, Some(64 * 1024 * 1024)).unwrap();
    runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    runtime
        .call_main(
            BUNDLE_SPECIFIER,
            &serde_json::json!({
                "conversation_id": "conversation-report-probe",
                "prompt": "Reduce the due cycle and read its report",
                "run_id": "run-report-probe",
                "turn_id": "turn-report-probe"
            })
            .to_string(),
            TURN_DEADLINE,
        )
        .await
        .expect("native MemeLoop must call both scoped report host ops");
    let calls = provider.report_calls.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        &[
            (expected_scope.clone(), "reduce:None".to_owned()),
            (expected_scope, "get:None".to_owned()),
        ]
    );
    assert_eq!(runtime.op_calls(HostOp::ReportReduce), 1);
    assert_eq!(runtime.op_calls(HostOp::ReportGet), 1);
    let models = provider.model_calls();
    assert_eq!(models.len(), 3);
    assert!(
        models[2]
            .1
            .messages
            .last()
            .unwrap()
            .content
            .as_deref()
            .unwrap()
            .contains("\"report_id\"")
    );
    let completion = runtime
        .host_state()
        .events
        .into_iter()
        .find(|event| event.topic == "loop.completed")
        .unwrap();
    assert!(
        completion
            .payload
            .contains("Report available with a coverage gap")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test memeloop_bundle -- --ignored`"]
async fn generated_memeloop_bundle_previews_without_formal_report_ops() {
    let source = generated_bundle();
    let provider = Arc::new(RecordingHostOps {
        preview_turn: true,
        ..Default::default()
    });
    let scoped = test_scope();
    let expected_scope = scoped.storage_key();
    let bridge = HostBridge::new(
        Arc::clone(&provider) as Arc<dyn HostOps>,
        scoped,
        tokio::runtime::Handle::current(),
    );
    let mut runtime = HostRuntime::new(
        &[(BUNDLE_SPECIFIER, source.as_str())],
        bridge,
        Some(64 * 1024 * 1024),
    )
    .unwrap();
    runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    runtime
        .call_main(
            BUNDLE_SPECIFIER,
            &serde_json::json!({
                "conversation_id": "conversation-preview-probe",
                "prompt": "Preview this cycle",
                "run_id": "run-preview-probe",
                "turn_id": "turn-preview-probe"
            })
            .to_string(),
            TURN_DEADLINE,
        )
        .await
        .expect("native MemeLoop must call the scoped preview op");
    assert_eq!(runtime.op_calls(HostOp::ReportPreview), 1);
    assert_eq!(runtime.op_calls(HostOp::ReportReduce), 0);
    assert_eq!(runtime.op_calls(HostOp::ReportGet), 0);
    assert_eq!(
        provider.report_calls.lock().unwrap().as_slice(),
        &[(expected_scope, "preview:None".to_owned())],
    );
    let models = provider.model_calls();
    let preview = models[1]
        .1
        .messages
        .last()
        .unwrap()
        .content
        .as_deref()
        .unwrap();
    assert!(preview.contains("\"kind\":\"preview\""));
    assert!(!preview.contains("\"report_id\""));
    assert!(!preview.contains("\"revision\""));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test memeloop_bundle -- --ignored`"]
async fn generated_memeloop_bundle_round_trips_a_native_tool_call_through_rust() {
    let source = generated_bundle();
    let bundle = [(BUNDLE_SPECIFIER, source.as_str())];
    let provider = Arc::new(RecordingHostOps {
        tool_turn: true,
        ..Default::default()
    });
    let scope = test_scope();
    let expected_scope = scope.storage_key();
    let bridge = HostBridge::new(
        Arc::clone(&provider) as Arc<dyn HostOps>,
        scope,
        tokio::runtime::Handle::current(),
    );
    let mut runtime = HostRuntime::new(&bundle, bridge, Some(64 * 1024 * 1024))
        .expect("the generated bundle must construct");
    runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    runtime
        .call_main(
            BUNDLE_SPECIFIER,
            &serde_json::json!({
                "conversation_id": "conversation-tool-probe-0001",
                "prompt": "What is the warranty?",
                "run_id": "run-tool-probe-0001",
                "timestamp": 1_700_000_000_000_u64,
                "turn_id": "turn-tool-probe-0001",
            })
            .to_string(),
            TURN_DEADLINE,
        )
        .await
        .expect("MemeLoop must perform model → search → model through the Rust host");

    let calls = provider.model_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, expected_scope);
    let tools = &calls[0].1.tools;
    assert!(
        tools
            .iter()
            .any(|tool| tool.function.name == "knowledge_search"
                && tool.function.parameters["required"][0] == "query")
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.function.name == "content_start"
                && tool.function.parameters["additionalProperties"] == false)
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.function.name == "content_execution_read"
                && tool.function.parameters["required"][0] == "execution_id")
    );
    for name in [
        "distribution_start",
        "distribution_read",
        "distribution_resume",
        "distribution_targets_read",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool.function.name == name)
            .expect("formal distribution tool must be available in native MemeLoop");
        assert_eq!(tool.function.parameters["additionalProperties"], false);
        assert!(
            tool.function.parameters["properties"]
                .get("account_id")
                .is_none()
        );
        assert!(tool.function.parameters["properties"].get("body").is_none());
    }
    assert_eq!(calls[1].1.messages.last().unwrap().role, "tool");
    assert_eq!(
        calls[1].1.messages.last().unwrap().tool_call_id.as_deref(),
        Some("search-call-1")
    );
    assert!(
        calls[1]
            .1
            .messages
            .last()
            .unwrap()
            .content
            .as_deref()
            .unwrap()
            .contains("Two years")
    );
    let searches = provider.search_calls();
    assert_eq!(searches.len(), 1);
    assert_eq!(searches[0].0, expected_scope);
    assert_eq!(searches[0].1.query, "warranty");
    assert_eq!(searches[0].1.limit, 2);
    let state = runtime.host_state();
    let completed = state
        .events
        .iter()
        .find(|event| event.topic == "loop.completed")
        .unwrap();
    let completion: serde_json::Value = serde_json::from_str(&completed.payload).unwrap();
    assert_eq!(
        completion["answer"],
        "The warranty lasts two years (Manual)."
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test memeloop_bundle -- --ignored`"]
async fn generated_memeloop_bundle_imports_bound_attachments_then_searches_and_answers() {
    let source = generated_bundle();
    let bundle = [(BUNDLE_SPECIFIER, source.as_str())];
    let provider = Arc::new(RecordingHostOps {
        import_turn: true,
        ..Default::default()
    });
    let scope = test_scope();
    let expected_scope = scope.storage_key();
    let attachments = vec![
        AttachmentReference {
            attachment_id: AttachmentId::from(uuid::Uuid::from_u128(40)),
            object_id: "object-40".to_owned(),
            filename: "Guide.txt".to_owned(),
            media_type: Some("text/plain".to_owned()),
            size_bytes: Some(9),
            sha256: Some("a".repeat(64)),
            object_version: Some("version-1".to_owned()),
        },
        AttachmentReference {
            attachment_id: AttachmentId::from(uuid::Uuid::from_u128(41)),
            object_id: "object-41".to_owned(),
            filename: "Data.pdf".to_owned(),
            media_type: Some("application/pdf".to_owned()),
            size_bytes: Some(11),
            sha256: Some("b".repeat(64)),
            object_version: Some("version-2".to_owned()),
        },
    ];
    let bridge = HostBridge::new(
        Arc::clone(&provider) as Arc<dyn HostOps>,
        scope,
        tokio::runtime::Handle::current(),
    )
    .with_attachments(attachments.clone());
    let mut runtime = HostRuntime::new(&bundle, bridge, Some(64 * 1024 * 1024))
        .expect("the generated bundle must construct");
    runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    runtime
        .call_main(
            BUNDLE_SPECIFIER,
            &serde_json::json!({
                "conversation_id": "conversation-import-probe-0001",
                "message_id": "message-import-probe-0001",
                "prompt": "",
                "run_id": "run-import-probe-0001",
                "turn_id": "turn-import-probe-0001",
                "timestamp": 1_700_000_000_000_u64,
                "attachments": attachments,
            })
            .to_string(),
            TURN_DEADLINE,
        )
        .await
        .expect("MemeLoop must perform import → search → answer through Rust host ops");

    let calls = provider.model_calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, expected_scope);
    let tools = &calls[0].1.tools;
    let import_tool = tools
        .iter()
        .find(|tool| tool.function.name == "knowledge_import_attachments")
        .expect("bound attachment import tool must be offered");
    assert!(
        tools
            .iter()
            .any(|tool| tool.function.name == "knowledge_search")
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.function.name == "content_start")
    );
    let schema = &import_tool.function.parameters;
    assert_eq!(
        schema["properties"]["items"]["items"]["properties"]["attachment_id"]["enum"][0],
        uuid::Uuid::from_u128(40).to_string()
    );
    assert!(
        schema["properties"]["items"]["items"]["properties"]["attachment_id"]["description"]
            .as_str()
            .unwrap()
            .contains("Guide.txt")
    );
    assert_eq!(
        calls[0].1.messages.last().unwrap().content.as_deref(),
        Some("")
    );
    let import_calls = provider
        .import_calls
        .lock()
        .expect("import call recorder must not be poisoned");
    assert_eq!(import_calls.len(), 1);
    assert_eq!(import_calls[0].0, expected_scope);
    assert_eq!(import_calls[0].1.items.len(), 2);
    assert!(
        calls[1]
            .1
            .messages
            .last()
            .unwrap()
            .content
            .as_deref()
            .unwrap()
            .contains("capability_missing")
    );
    let searches = provider.search_calls();
    assert_eq!(searches.len(), 1);
    assert_eq!(
        searches[0].1.knowledge_release_id.unwrap(),
        uuid::Uuid::from_u128(50)
    );
    assert_eq!(calls[2].1.messages.last().unwrap().role, "tool");
    assert!(
        calls[2]
            .1
            .messages
            .last()
            .unwrap()
            .content
            .as_deref()
            .unwrap()
            .contains("Two years")
    );
    let state = runtime.host_state();
    let completed = state
        .events
        .iter()
        .find(|event| event.topic == "loop.completed")
        .unwrap();
    let completion: serde_json::Value = serde_json::from_str(&completed.payload).unwrap();
    assert_eq!(completion["turn_id"], "turn-import-probe-0001");
    assert_eq!(
        completion["answer"],
        "The warranty lasts two years (Manual)."
    );
}

fn generated_bundle_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs")
}

fn generated_bundle() -> String {
    let path = generated_bundle_path();
    fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "the generated MemeLoop ESM bundle is required for this probe ({path:?}): {error}. \
             Run `pnpm agent:bundle` from the repository root, then rerun \
             `cargo test -p geo-worker --test memeloop_bundle -- --ignored`."
        )
    })
}

fn test_scope() -> TenantScope {
    TenantScope::new(
        uuid::Uuid::from_u128(1).into(),
        uuid::Uuid::from_u128(2).into(),
        Some(uuid::Uuid::from_u128(3).into()),
    )
}

/// Runs the actual generated MemeLoop loop through the production host-op
/// surface. The current-thread runtime is a hard requirement of deno_core's
/// async-op driver; changing this flavor can otherwise be unsound.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test memeloop_bundle -- --ignored`"]
async fn generated_memeloop_bundle_runs_a_turn_through_the_rust_host() {
    let source = generated_bundle();
    let bundle = [(BUNDLE_SPECIFIER, source.as_str())];
    let provider = Arc::new(RecordingHostOps::default());
    let scope = test_scope();
    let expected_scope = scope.storage_key();
    let bridge = HostBridge::new(
        Arc::clone(&provider) as Arc<dyn HostOps>,
        scope,
        tokio::runtime::Handle::current(),
    );
    let mut runtime = HostRuntime::new(&bundle, bridge, Some(64 * 1024 * 1024))
        .expect("the generated bundle must construct within the production heap budget");
    runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));

    assert_eq!(
        runtime.allowlisted_specifiers(),
        vec![BUNDLE_SPECIFIER.to_owned()],
        "the real bundle must be served only from the in-memory allow-list"
    );

    let unknown_import = runtime
        .evaluate_module("memeloop://bundle/not-approved.mjs", TURN_DEADLINE)
        .await
        .expect_err("an import outside the generated bundle must be denied");
    assert_eq!(unknown_import.stage, "load");
    assert!(
        unknown_import
            .message
            .contains("not part of the approved bundle"),
        "unexpected unknown-import error: {unknown_import:?}"
    );

    runtime
        .call_main(
            BUNDLE_SPECIFIER,
            &serde_json::json!({
                "conversation_id": "conversation-bundle-probe-0001",
                "prompt": "How long is the warranty?",
                "run_id": "run-bundle-probe-0001",
                "timestamp": 1_700_000_000_000_u64,
                "turn_id": "turn-bundle-probe-0001",
            })
            .to_string(),
            TURN_DEADLINE,
        )
        .await
        .expect("the generated MemeLoop bundle must complete its turn");

    let model_calls = provider.model_calls();
    assert_eq!(model_calls.len(), 1, "the loop must make one model call");
    let (call_scope, request) = model_calls
        .first()
        .expect("the checked model call must remain available");
    assert_eq!(call_scope, &expected_scope);
    assert!(
        request.prompt.contains("user: How long is the warranty?"),
        "the model bridge must receive the user message: {:?}",
        request
    );

    let state = runtime.host_state();
    let completed = state
        .events
        .iter()
        .find(|event| event.topic == "loop.completed")
        .expect("the real MemeLoop loop must emit its completion");
    let completion: serde_json::Value =
        serde_json::from_str(&completed.payload).expect("completion must be valid JSON");
    assert_eq!(completion["answer"], "The warranty lasts two years.");
    assert_eq!(
        completion["conversation_id"],
        "conversation-bundle-probe-0001"
    );
    assert_eq!(completion["model"], "probe-model");
    assert_eq!(completion["run_id"], "run-bundle-probe-0001");
    assert_eq!(completion["turn_id"], "turn-bundle-probe-0001");
    assert_eq!(
        state
            .events
            .iter()
            .filter(|event| event.topic == "loop.completed")
            .count(),
        1,
        "a real turn must report exactly one completion"
    );
}
