//! Generated MemeLoop bundle proof for the scoped media host tools. This
//! exercises the real V8 loop; only the business capabilities and model are
//! injected, not a replacement JS tool runner.
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use geo_domain::{
    AttachmentId, AttachmentReference, ContentBlock, ContentBlockKind, KnowledgeSearchRequest,
    KnowledgeSearchResult, MediaObjectKey, StructuredDocument, TenantScope,
};
use geo_worker::{
    ContentDocumentReadRequest, ContentDocumentSnapshot, ContentMediaBindRequest,
    ContentMediaInsertReceipt, ContentMediaInsertRequest, ContentMediaListRequest,
    ContentMediaPage, ContentMediaRef, HostBridge, HostOp, HostOpError, HostOps, HostRuntime,
    ManifestPage, ManifestReadRequest, MeasureRequest, MeasureSample, ModelCompletion,
    ModelCompletionRequest, PublishReceipt, PublishRequest,
};
use uuid::Uuid;

const ENTRY: &str = "memeloop://bundle/memeloop-agent-loop.bundle.mjs";
const EXECUTION: Uuid = Uuid::from_u128(70);
const ITEM: Uuid = Uuid::from_u128(71);
const ASSET: Uuid = Uuid::from_u128(72);
const BASE: Uuid = Uuid::from_u128(73);
const BINDING: Uuid = Uuid::from_u128(74);
const ATTACHMENT: Uuid = Uuid::from_u128(75);
const BLOCK: Uuid = Uuid::from_u128(76);
const INSERTED_REVISION: Uuid = Uuid::from_u128(77);
const INSERTED_BLOCK: Uuid = Uuid::from_u128(78);

#[derive(Default)]
struct MediaProvider {
    model_requests: Mutex<Vec<ModelCompletionRequest>>,
    calls: Mutex<Vec<HostOp>>,
}

fn image() -> ContentMediaRef {
    ContentMediaRef {
        binding_id: BINDING,
        key: MediaObjectKey {
            object_id: ATTACHMENT,
            object_version: 1,
            sha256: "a".repeat(64),
        },
        media_type: "image/png".into(),
        byte_len: 2048,
        width: 32,
        height: 24,
    }
}

#[async_trait]
impl HostOps for MediaProvider {
    async fn model_complete(
        &self,
        _: &TenantScope,
        request: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        let mut calls = self.model_requests.lock().unwrap();
        calls.push(request);
        let index = calls.len();
        let next = match index {
            1 => Some((
                "content_media_bind",
                serde_json::json!({"attachment_id": ATTACHMENT}),
            )),
            2 => Some(("content_media_list", serde_json::json!({"limit": 10}))),
            3 => Some((
                "content_document_read",
                serde_json::json!({"execution_id":EXECUTION,"item_id":ITEM}),
            )),
            4 => Some((
                "content_media_insert",
                serde_json::json!({
                    "execution_id": EXECUTION,
                    "item_id": ITEM,
                    "base_revision_id": BASE,
                    "binding_id": BINDING,
                    "alt": "A small diagram",
                    "caption": "A schematic."
                }),
            )),
            _ => None,
        };
        Ok(if let Some((tool, args)) = next {
            serde_json::from_value(serde_json::json!({
                "text": "",
                "tool_calls": [{
                    "id": format!("media-{index}"),
                    "type": "function",
                    "function": {"name": tool, "arguments": args.to_string()}
                }],
                "model": "injected-model",
                "prompt_tokens": 5,
                "completion_tokens": 4,
                "finish_reason": "tool_calls"
            }))
            .unwrap()
        } else {
            ModelCompletion {
                text: "The image was inserted in a new draft revision.".into(),
                tool_calls: vec![],
                model: "injected-model".into(),
                prompt_tokens: 5,
                completion_tokens: 4,
                finish_reason: "stop".into(),
            }
        })
    }

    async fn knowledge_search(
        &self,
        _: &TenantScope,
        _: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::KnowledgeSearch,
            "not configured",
        ))
    }
    async fn manifest_read(
        &self,
        _: &TenantScope,
        _: ManifestReadRequest,
    ) -> Result<ManifestPage, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ManifestRead,
            "not configured",
        ))
    }
    async fn publish_submit(
        &self,
        _: &TenantScope,
        _: PublishRequest,
    ) -> Result<PublishReceipt, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Publish,
            "not configured",
        ))
    }
    async fn measure_sample(
        &self,
        _: &TenantScope,
        _: MeasureRequest,
    ) -> Result<MeasureSample, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::Measure,
            "not configured",
        ))
    }

    async fn content_media_bind(
        &self,
        _: &TenantScope,
        request: ContentMediaBindRequest,
        attachments: &[AttachmentReference],
    ) -> Result<ContentMediaRef, HostOpError> {
        assert_eq!(request.attachment_id, ATTACHMENT);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].attachment_id.as_uuid(), ATTACHMENT);
        self.calls.lock().unwrap().push(HostOp::ContentMediaBind);
        Ok(image())
    }
    async fn content_media_list(
        &self,
        _: &TenantScope,
        request: ContentMediaListRequest,
    ) -> Result<ContentMediaPage, HostOpError> {
        assert_eq!(request.limit, Some(10));
        self.calls.lock().unwrap().push(HostOp::ContentMediaList);
        Ok(ContentMediaPage {
            items: vec![image()],
            next_cursor: None,
        })
    }
    async fn content_document_read(
        &self,
        _: &TenantScope,
        request: ContentDocumentReadRequest,
    ) -> Result<ContentDocumentSnapshot, HostOpError> {
        assert_eq!(request.execution_id, EXECUTION);
        assert_eq!(request.item_id, ITEM);
        self.calls.lock().unwrap().push(HostOp::ContentDocumentRead);
        Ok(ContentDocumentSnapshot {
            execution_id: EXECUTION,
            item_id: ITEM,
            asset_id: ASSET,
            revision_id: BASE,
            current_revision_id: BASE,
            revision: 1,
            is_reused: false,
            document: StructuredDocument {
                title: "Example draft".into(),
                blocks: vec![ContentBlock {
                    block_id: BLOCK,
                    kind: ContentBlockKind::Paragraph,
                    text: "A paragraph.".into(),
                    citation_ids: vec![],
                    items: vec![],
                    rich: None,
                }],
                schema_version: None,
            },
        })
    }
    async fn content_media_insert(
        &self,
        _: &TenantScope,
        request: ContentMediaInsertRequest,
    ) -> Result<ContentMediaInsertReceipt, HostOpError> {
        assert_eq!(request.base_revision_id, BASE);
        assert_eq!(request.binding_id, BINDING);
        assert!(request.after_block_id.is_none());
        self.calls.lock().unwrap().push(HostOp::ContentMediaInsert);
        Ok(ContentMediaInsertReceipt {
            execution_id: EXECUTION,
            item_id: ITEM,
            asset_id: ASSET,
            revision_id: INSERTED_REVISION,
            base_revision_id: BASE,
            block_id: INSERTED_BLOCK,
            revision: 2,
        })
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires `pnpm agent:bundle`; run with `cargo test -p geo-worker --test content_media_runtime -- --ignored`"]
async fn generated_memeloop_bundle_dispatches_real_media_host_ops() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs");
    let source = fs::read_to_string(path).expect("generate bundle with pnpm agent:bundle");
    let provider = Arc::new(MediaProvider::default());
    let attachment = AttachmentReference {
        attachment_id: AttachmentId::from(ATTACHMENT),
        object_id: ATTACHMENT.to_string(),
        filename: "diagram.png".into(),
        media_type: Some("image/png".into()),
        size_bytes: Some(2048),
        sha256: Some("a".repeat(64)),
        object_version: Some("1".into()),
    };
    let scope = TenantScope::new(
        Uuid::from_u128(1).into(),
        Uuid::from_u128(2).into(),
        Some(Uuid::from_u128(3).into()),
    );
    let bridge = HostBridge::new(
        Arc::clone(&provider) as Arc<dyn HostOps>,
        scope,
        tokio::runtime::Handle::current(),
    )
    .with_attachments(vec![attachment.clone()]);
    let mut runtime = HostRuntime::new(&[(ENTRY, &source)], bridge, Some(64 * 1024 * 1024))
        .expect("generated bundle must load");
    runtime.install_heap_limit_guard(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    runtime
        .call_main(
            ENTRY,
            &serde_json::json!({
                "conversation_id":"media-conversation",
                "message_id":"media-message",
                "prompt":"Bind and insert this image into the draft",
                "run_id":"media-run",
                "turn_id":"media-turn",
                "timestamp":1_700_000_000_000_u64,
                "attachments":[attachment]
            })
            .to_string(),
            Duration::from_secs(30),
        )
        .await
        .expect("real MemeLoop dispatch must finish");
    assert_eq!(
        *provider.calls.lock().unwrap(),
        vec![
            HostOp::ContentMediaBind,
            HostOp::ContentMediaList,
            HostOp::ContentDocumentRead,
            HostOp::ContentMediaInsert,
        ]
    );
    let calls = provider.model_requests.lock().unwrap();
    assert_eq!(calls.len(), 5);
    for tool in [
        "content_media_list",
        "content_media_bind",
        "content_document_read",
        "content_media_insert",
    ] {
        assert!(
            calls[0].tools.iter().any(|item| item.function.name == tool),
            "model must discover {tool}"
        );
    }
    let completion = runtime
        .host_state()
        .events
        .iter()
        .find(|event| event.topic == "loop.completed")
        .cloned()
        .expect("turn completion event");
    let parsed: serde_json::Value = serde_json::from_str(&completion.payload).unwrap();
    assert_eq!(
        parsed["answer"],
        "The image was inserted in a new draft revision."
    );
}
