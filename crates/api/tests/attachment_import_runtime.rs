//! Real generated MemeLoop/V8 attachment-import path over committed repository
//! bytes. The model is a deterministic fixture; the tool loop is not simulated.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use geo_api::{EmbeddedAgentRuntime, ModelProviderBridge, RepositoryHostOps};
use geo_domain::{
    AgentRuntime, AttachmentReference, ErrorCode, ImportStatus, KnowledgePurpose,
    KnowledgeRepository, KnowledgeSearchRequest, MemoryKnowledgeRepository, StoredObject,
    TenantScope, TurnInput, UploadSessionCommand, sha256_hex,
};
use geo_worker::{
    HostOpError, HostOps, KnowledgeImportAttachmentItem, KnowledgeImportAttachmentsRequest,
    KnowledgeImportStatusRequest, ModelCompletion, ModelCompletionRequest, ModelToolCall,
    ModelToolFunctionCall,
};
use serde_json::{Value, json};
use uuid::Uuid;

const BUNDLE_SPECIFIER: &str = "memeloop://bundle/memeloop-agent-loop.bundle.mjs";
const FACT: &str = "The standard warranty lasts twenty-four months.";

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

async fn upload(
    repository: &MemoryKnowledgeRepository,
    scope: &TenantScope,
    filename: &str,
    media_type: &str,
    bytes: &[u8],
) -> (AttachmentReference, StoredObject) {
    let session = repository
        .create_upload_session(
            scope,
            UploadSessionCommand {
                filename: filename.into(),
                declared_media_type: media_type.into(),
                expected_size: bytes.len() as u64,
                expected_sha256: sha256_hex(bytes),
                purpose: KnowledgePurpose::Internal,
            },
        )
        .await
        .unwrap();
    repository
        .put_upload_content(scope, session.upload_session_id, bytes.to_vec())
        .await
        .unwrap();
    let (object, name) = repository
        .complete_attachment_upload(scope, session.upload_session_id, filename)
        .await
        .unwrap();
    let reference = AttachmentReference {
        attachment_id: object.object_id.into(),
        object_id: object.object_id.to_string(),
        filename: name,
        media_type: Some(object.detected_media_type.clone()),
        size_bytes: Some(object.actual_size),
        sha256: Some(object.sha256.clone()),
        object_version: Some(object.object_version.to_string()),
    };
    (reference, object)
}

fn import_request(attachments: &[AttachmentReference]) -> KnowledgeImportAttachmentsRequest {
    KnowledgeImportAttachmentsRequest {
        items: attachments
            .iter()
            .map(|attachment| KnowledgeImportAttachmentItem {
                attachment_id: attachment.attachment_id.as_uuid(),
                purpose: KnowledgePurpose::Internal,
            })
            .collect(),
    }
}

fn tool_call(id: &str, name: &str, arguments: Value) -> ModelToolCall {
    ModelToolCall {
        id: id.into(),
        kind: "function".into(),
        function: ModelToolFunctionCall {
            name: name.into(),
            arguments: arguments.to_string(),
        },
    }
}

#[derive(Debug)]
struct ImportThenSearchModel {
    attachments: Vec<AttachmentReference>,
    calls: Mutex<usize>,
    receipts: Mutex<Vec<Value>>,
}

impl ImportThenSearchModel {
    fn new(attachments: Vec<AttachmentReference>) -> Self {
        Self {
            attachments,
            calls: Mutex::new(0),
            receipts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ModelProviderBridge for ImportThenSearchModel {
    async fn complete(
        &self,
        _scope: &TenantScope,
        request: &ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let mut calls = self.calls.lock().unwrap();
        let phase = *calls % 3;
        *calls += 1;
        let tool_names: Vec<_> = request
            .tools
            .iter()
            .map(|t| t.function.name.as_str())
            .collect();
        assert!(tool_names.contains(&"knowledge_import_attachments"));
        assert!(tool_names.contains(&"knowledge_search"));
        let mut completion = ModelCompletion {
            text: String::new(),
            tool_calls: Vec::new(),
            model: "fixture-model".into(),
            prompt_tokens: 10,
            completion_tokens: 5,
            finish_reason: "tool_calls".into(),
        };
        match phase {
            0 => {
                assert!(
                    request.messages.iter().any(|m| m.role == "user"),
                    "MemeLoop must supply the persisted user turn"
                );
                completion.tool_calls.push(tool_call(
                    "import-call",
                    "knowledge_import_attachments",
                    json!(import_request(&self.attachments)),
                ));
            }
            1 => {
                let tool = request
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == "tool")
                    .expect("the model must see the actual import tool result");
                let receipt: Value =
                    serde_json::from_str(tool.content.as_deref().unwrap()).unwrap();
                let items = receipt["items"]
                    .as_array()
                    .unwrap_or_else(|| panic!("import items missing from tool payload: {receipt}"));
                assert_eq!(items.len(), self.attachments.len());
                assert_eq!(items[0]["status"], "succeeded", "{receipt}");
                assert!(items[0]["source_id"].as_str().is_some());
                assert!(items[0]["source_version_id"].as_str().is_some());
                assert!(items[0]["knowledge_release_id"].as_str().is_some());
                if items.len() > 1 {
                    assert_eq!(items[1]["status"], "failed", "{receipt}");
                    assert_eq!(items[1]["error"]["code"], "capability_missing");
                }
                let release_id = items[0]["knowledge_release_id"].clone();
                self.receipts.lock().unwrap().push(receipt);
                completion.tool_calls.push(tool_call(
                    "search-call",
                    "knowledge_search",
                    json!({"query":"warranty", "purpose":"internal", "knowledge_release_id":release_id}),
                ));
            }
            _ => {
                let tool = request
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == "tool")
                    .expect("the model must see actual search evidence");
                let search: Value = serde_json::from_str(tool.content.as_deref().unwrap()).unwrap();
                let evidence = search["evidence"].as_array().expect("evidence");
                assert!(!evidence.is_empty(), "{search}");
                assert!(evidence[0]["text"].as_str().unwrap().contains(FACT));
                assert!(evidence[0]["locator"].is_object(), "{search}");
                completion.text = format!(
                    "The warranty lasts twenty-four months. [source:{}]",
                    evidence[0]["source_version_id"].as_str().unwrap()
                );
                completion.finish_reason = "stop".into();
            }
        }
        Ok(completion)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires `pnpm agent:bundle`; generated ESM is intentionally not tracked"]
async fn generated_memeloop_imports_committed_attachment_then_searches_and_cites_it() {
    let repository = Arc::new(MemoryKnowledgeRepository::default());
    let run_scope = scope();
    let (text_ref, object) = upload(
        &repository,
        &run_scope,
        "warranty.txt",
        "text/plain",
        FACT.as_bytes(),
    )
    .await;
    let (unsupported_ref, _) = upload(
        &repository,
        &run_scope,
        "manual.pdf",
        "application/pdf",
        b"opaque document bytes",
    )
    .await;
    assert!(
        repository
            .list_sources(&run_scope)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .search(
                &run_scope,
                KnowledgeSearchRequest {
                    query: "warranty".into(),
                    purpose: KnowledgePurpose::Internal,
                    limit: 5,
                    knowledge_release_id: None,
                },
            )
            .await
            .unwrap()
            .evidence
            .is_empty()
    );

    let attachments = vec![text_ref, unsupported_ref];
    let model = Arc::new(ImportThenSearchModel::new(attachments.clone()));
    let ops = RepositoryHostOps::new(repository.clone()).with_model_provider(model.clone());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs");
    let bundle: &'static str = Box::leak(
        fs::read_to_string(path)
            .expect("run pnpm agent:bundle first")
            .into_boxed_str(),
    );
    let modules: &'static [(&str, &str)] = Box::leak(Box::new([(BUNDLE_SPECIFIER, bundle)]));
    let runtime = EmbeddedAgentRuntime::with_bundle(modules, BUNDLE_SPECIFIER, Arc::new(ops));

    for _ in 0..2 {
        let report = runtime
            .run_turn(
                &run_scope,
                TurnInput {
                    conversation_id: Uuid::new_v4().into(),
                    message_id: Uuid::new_v4().into(),
                    turn_id: Uuid::new_v4().into(),
                    run_id: Uuid::new_v4().into(),
                    prompt: "Import the attached documents and answer the warranty question."
                        .into(),
                    attachments: attachments.clone(),
                    history: Vec::new(),
                    history_omitted_turns: 0,
                },
            )
            .await
            .expect("the native MemeLoop import -> search -> answer loop must complete");
        assert!(report.content.contains("twenty-four months"));
        assert_eq!(report.metadata["model"], "fixture-model");
    }
    assert_eq!(*model.calls.lock().unwrap(), 6);
    let receipts = model.receipts.lock().unwrap().clone();
    assert_eq!(receipts.len(), 2);
    assert_eq!(
        receipts[0]["items"][0], receipts[1]["items"][0],
        "repeated turns must receive the same durable import receipt"
    );
    let source_id =
        Uuid::parse_str(receipts[0]["items"][0]["source_id"].as_str().unwrap()).unwrap();
    let detail = repository
        .get_source_detail(&run_scope, source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.source.locator["object_id"],
        object.object_id.to_string()
    );
    assert_eq!(detail.versions.len(), 1);
    assert_eq!(detail.versions[0].object_id, Some(object.object_id));
    assert_eq!(
        detail.versions[0].object_version,
        Some(object.object_version)
    );
    assert_eq!(detail.versions[0].content_sha256, object.sha256);
    assert_eq!(repository.list_sources(&run_scope).await.unwrap().len(), 1);
}

#[tokio::test]
async fn direct_host_import_rejects_unbound_cross_scope_and_tampered_metadata_per_item() {
    let repository = Arc::new(MemoryKnowledgeRepository::default());
    let run_scope = scope();
    let (valid, _) = upload(
        &repository,
        &run_scope,
        "valid.txt",
        "text/plain",
        FACT.as_bytes(),
    )
    .await;
    let (unbound, _) = upload(
        &repository,
        &run_scope,
        "unbound.txt",
        "text/plain",
        b"unbound",
    )
    .await;
    let foreign_scope = TenantScope::new(
        run_scope.operator_id,
        run_scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    let (foreign, _) = upload(
        &repository,
        &foreign_scope,
        "foreign.txt",
        "text/plain",
        b"foreign",
    )
    .await;
    let mut tampered = valid.clone();
    tampered.sha256 = Some("0".repeat(64));
    let ops = RepositoryHostOps::new(repository.clone());
    let results = ops
        .knowledge_import_attachments(
            &run_scope,
            import_request(&[valid.clone(), unbound.clone(), foreign.clone()]),
            &[valid.clone(), foreign, tampered.clone()],
        )
        .await
        .unwrap();
    assert_eq!(results.items.len(), 3);
    assert_eq!(results.items[0].status, ImportStatus::Succeeded);
    assert_eq!(
        results.items[1].error.as_ref().unwrap().code,
        ErrorCode::Forbidden
    );
    assert_eq!(
        results.items[2].error.as_ref().unwrap().code,
        ErrorCode::NotFound
    );
    let tamper = ops
        .knowledge_import_attachments(&run_scope, import_request(&[valid]), &[tampered])
        .await
        .unwrap();
    assert_eq!(tamper.items[0].status, ImportStatus::Failed);
    assert_eq!(
        tamper.items[0].error.as_ref().unwrap().code,
        ErrorCode::Conflict
    );
    assert_eq!(repository.list_sources(&run_scope).await.unwrap().len(), 1);
    assert!(
        repository
            .list_sources(&foreign_scope)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn queued_attachment_receipt_exposes_actual_job_without_premature_evidence() {
    let repository = Arc::new(MemoryKnowledgeRepository::with_pdf_parser_profile(
        "test-parser-v1".to_owned(),
    ));
    let run_scope = scope();
    let (reference, _) = upload(
        &repository,
        &run_scope,
        "pending.pdf",
        "application/pdf",
        b"%PDF-1.7\nunparsed original",
    )
    .await;
    let ops = RepositoryHostOps::new(repository);
    let accepted = ops
        .knowledge_import_attachments(
            &run_scope,
            import_request(std::slice::from_ref(&reference)),
            std::slice::from_ref(&reference),
        )
        .await
        .unwrap();
    let receipt = &accepted.items[0];
    assert_eq!(receipt.status, ImportStatus::Queued);
    let job_id = receipt.import_job_id.expect("actual queued job ID");
    assert!(receipt.source_id.is_some());
    assert!(receipt.source_version_id.is_none());
    assert!(receipt.knowledge_release_id.is_none());
    let request = KnowledgeImportStatusRequest {
        import_job_id: job_id,
        purpose: KnowledgePurpose::Internal,
    };
    let progress = ops
        .knowledge_import_status(&run_scope, request.clone())
        .await
        .unwrap();
    assert_eq!(progress.import_job_id, Some(job_id));
    assert_eq!(progress.status, ImportStatus::Queued);
    assert!(progress.knowledge_release_id.is_none());
    let foreign = TenantScope::new(
        run_scope.operator_id,
        run_scope.tenant_id,
        Some(Uuid::new_v4().into()),
    );
    let error = ops
        .knowledge_import_status(&foreign, request)
        .await
        .unwrap_err();
    assert_eq!(error.code, geo_worker::HostOpErrorCode::NotFound);
}
