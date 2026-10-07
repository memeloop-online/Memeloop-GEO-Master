use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use geo_domain::{
    AttachmentId, AttachmentReference, KnowledgeSearchRequest, KnowledgeSearchResult,
    MediaObjectKey, RunId, StructuredDocument, TenantScope, ToolCallIdentity, ToolCallOutcome,
};
use geo_worker::{
    ContentDocumentReadRequest, ContentDocumentSnapshot, ContentMediaBindRequest,
    ContentMediaInsertReceipt, ContentMediaInsertRequest, ContentMediaListRequest,
    ContentMediaPage, ContentMediaRef, HOST_OPS_VERSION, HostBridge, HostOp, HostOpError,
    HostOpErrorCode, HostOps, HostRuntime, ManifestPage, ManifestReadRequest, MeasureRequest,
    MeasureSample, ModelCompletion, ModelCompletionRequest, PublishReceipt, PublishRequest,
    ToolCallRecorder,
};
use uuid::Uuid;

fn scope() -> TenantScope {
    TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    )
}

fn media() -> ContentMediaRef {
    ContentMediaRef {
        binding_id: Uuid::new_v4(),
        key: MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 1,
            sha256: "a".repeat(64),
        },
        media_type: "image/png".into(),
        byte_len: 2048,
        width: 32,
        height: 24,
    }
}

#[derive(Default)]
struct Recorder {
    outcomes: Mutex<Vec<ToolCallOutcome>>,
}

#[async_trait]
impl ToolCallRecorder for Recorder {
    async fn begin(&self, _: &ToolCallIdentity) -> Result<bool, HostOpError> {
        Ok(true)
    }

    async fn attempt(&self, _: &ToolCallIdentity) -> Result<bool, HostOpError> {
        Ok(true)
    }

    async fn finish(
        &self,
        _: &ToolCallIdentity,
        outcome: ToolCallOutcome,
    ) -> Result<(), HostOpError> {
        self.outcomes.lock().unwrap().push(outcome);
        Ok(())
    }
}

struct FakeOps;

#[async_trait]
impl HostOps for FakeOps {
    async fn model_complete(
        &self,
        _: &TenantScope,
        _: ModelCompletionRequest,
    ) -> Result<ModelCompletion, HostOpError> {
        Err(HostOpError::capability_missing(
            HostOp::ModelComplete,
            "not configured",
        ))
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
}

fn recorded_bridge(recorder: Arc<Recorder>) -> HostBridge {
    HostBridge::new(
        Arc::new(FakeOps),
        scope(),
        tokio::runtime::Handle::current(),
    )
    .with_recorder(RunId::from(Uuid::new_v4()), recorder)
}

#[test]
fn v15_surface_and_strict_media_dtos() {
    assert_eq!(HOST_OPS_VERSION, "geo.hostops.v15");
    assert_eq!(HostOp::COUNT, 43);
    let names = HostRuntime::op_surface();
    for (op, js_name) in [
        (HostOp::ContentMediaList, "op_host_content_media_list_v1"),
        (HostOp::ContentMediaBind, "op_host_content_media_bind_v1"),
        (
            HostOp::ContentDocumentRead,
            "op_host_content_document_read_v1",
        ),
        (
            HostOp::ContentMediaInsert,
            "op_host_content_media_insert_v1",
        ),
    ] {
        assert!(names.contains(&js_name.to_string()));
        assert_eq!(op.op_name(), js_name);
    }
    assert_eq!(names.len(), HostOp::COUNT + 2);
    assert!(
        serde_json::from_str::<ContentMediaBindRequest>(
            r#"{"attachment_id":"00000000-0000-0000-0000-000000000001","object_id":"hidden"}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<ContentMediaListRequest>(r#"{"limit":1,"project_id":"x"}"#).is_err()
    );
    assert!(serde_json::from_str::<ContentMediaInsertRequest>(
        r#"{"execution_id":"00000000-0000-0000-0000-000000000001","item_id":"00000000-0000-0000-0000-000000000001","base_revision_id":"00000000-0000-0000-0000-000000000001","binding_id":"00000000-0000-0000-0000-000000000001","alt":"a","caption":"","tenant_id":"x"}"#
    ).is_err());
    assert!(
        ContentMediaListRequest {
            after: None,
            limit: Some(26)
        }
        .validate()
        .is_err()
    );
}

#[test]
fn media_insert_text_and_page_validation() {
    let request = ContentMediaInsertRequest {
        execution_id: Uuid::new_v4(),
        item_id: Uuid::new_v4(),
        base_revision_id: Uuid::new_v4(),
        binding_id: Uuid::new_v4(),
        after_block_id: None,
        alt: "   ".into(),
        caption: String::new(),
    };
    assert!(request.validate().is_err());
    let request = ContentMediaInsertRequest {
        alt: "a".repeat(1_000_001),
        ..request
    };
    assert!(request.validate().is_err());
    let item = media();
    assert!(
        ContentMediaPage {
            items: vec![item.clone(), item],
            next_cursor: None,
        }
        .validate_for(&ContentMediaListRequest::default())
        .is_err()
    );
}

#[tokio::test]
async fn selected_attachment_must_be_from_this_turn_before_host_dispatch() {
    const SCRIPT: &str = r#"
        const result = await Deno.core.ops.op_host_content_media_bind_v1(
            JSON.stringify({attachment_id: "00000000-0000-0000-0000-000000000001"})
        ).then(() => "unexpected success", error => error.message);
        Deno.core.ops.op_host_emit("media-denial", result);
    "#;
    let attachment_id = Uuid::from_u128(1);
    let request = ContentMediaBindRequest { attachment_id };
    let recorder = Arc::new(Recorder::default());
    let bridge = recorded_bridge(Arc::clone(&recorder));
    let mut runtime =
        HostRuntime::new(&[("memeloop://bundle/media-test.js", SCRIPT)], bridge, None)
            .expect("runtime");
    runtime
        .evaluate_module("memeloop://bundle/media-test.js", Duration::from_secs(10))
        .await
        .expect("denied op reports a structured error");
    let state = runtime.host_state();
    let event = state
        .events
        .iter()
        .find(|event| event.topic == "media-denial")
        .expect("denial event");
    let denial: serde_json::Value = serde_json::from_str(&event.payload).expect("typed denial");
    assert_eq!(denial["code"], "denied");
    assert_eq!(denial["op"], "content_media_bind");
    // The same check made by the op before invoke_recorded, with immutable
    // Rust-bound references: arbitrary object key metadata is never accepted.
    assert_eq!(
        request.validate(&[]).unwrap_err().code,
        HostOpErrorCode::Denied
    );
    let bound = AttachmentReference {
        attachment_id: AttachmentId::from(attachment_id),
        object_id: "opaque".into(),
        filename: "image.png".into(),
        media_type: Some("image/png".into()),
        size_bytes: Some(2048),
        sha256: Some("a".repeat(64)),
        object_version: Some("1".into()),
    };
    assert!(request.validate(&[bound]).is_ok());
    assert!(recorder.outcomes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_insert_receipt_never_records_success() {
    let recorder = Arc::new(Recorder::default());
    let bridge = recorded_bridge(Arc::clone(&recorder));
    let request = ContentMediaInsertRequest {
        execution_id: Uuid::new_v4(),
        item_id: Uuid::new_v4(),
        base_revision_id: Uuid::new_v4(),
        binding_id: Uuid::new_v4(),
        after_block_id: None,
        alt: "descriptive text".into(),
        caption: String::new(),
    };
    let wrong_item = Uuid::new_v4();
    let response_request = request.clone();
    let failed = bridge
        .invoke_recorded(HostOp::ContentMediaInsert, &request, move |_| async move {
            Ok::<_, HostOpError>(ContentMediaInsertReceipt {
                execution_id: response_request.execution_id,
                item_id: wrong_item,
                asset_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                base_revision_id: response_request.base_revision_id,
                block_id: Uuid::new_v4(),
                revision: 2,
            })
        })
        .await;
    assert_eq!(failed.unwrap_err().code, HostOpErrorCode::Internal);
    assert_eq!(
        *recorder.outcomes.lock().unwrap(),
        vec![ToolCallOutcome::Failed]
    );
}

#[tokio::test]
async fn foreign_attachment_binding_never_records_success() {
    let recorder = Arc::new(Recorder::default());
    let bridge = recorded_bridge(Arc::clone(&recorder));
    let request = ContentMediaBindRequest {
        attachment_id: Uuid::new_v4(),
    };
    let foreign = bridge
        .invoke_recorded(HostOp::ContentMediaBind, &request, |_| async {
            Ok::<_, HostOpError>(media())
        })
        .await;
    assert_eq!(foreign.unwrap_err().code, HostOpErrorCode::Internal);
    assert_eq!(
        *recorder.outcomes.lock().unwrap(),
        vec![ToolCallOutcome::Failed]
    );
}

#[tokio::test]
async fn list_and_document_foreign_results_fail_before_ledger_success() {
    let recorder = Arc::new(Recorder::default());
    let bridge = recorded_bridge(Arc::clone(&recorder));
    let page_request = ContentMediaListRequest {
        after: None,
        limit: Some(1),
    };
    let invalid_page = bridge
        .invoke_recorded(HostOp::ContentMediaList, &page_request, |_| async {
            Ok::<_, HostOpError>(ContentMediaPage {
                items: vec![media(), media()],
                next_cursor: None,
            })
        })
        .await;
    assert_eq!(invalid_page.unwrap_err().code, HostOpErrorCode::Internal);

    let request = ContentDocumentReadRequest {
        execution_id: Uuid::new_v4(),
        item_id: Uuid::new_v4(),
        revision_id: Some(Uuid::new_v4()),
    };
    let response_request = request.clone();
    let invalid_snapshot = bridge
        .invoke_recorded(HostOp::ContentDocumentRead, &request, move |_| async move {
            Ok::<_, HostOpError>(ContentDocumentSnapshot {
                execution_id: response_request.execution_id,
                item_id: response_request.item_id,
                asset_id: Uuid::new_v4(),
                revision_id: Uuid::new_v4(),
                current_revision_id: Uuid::new_v4(),
                revision: 1,
                is_reused: false,
                document: StructuredDocument {
                    title: "Document".into(),
                    blocks: Vec::new(),
                    schema_version: Some(2),
                },
            })
        })
        .await;
    assert_eq!(
        invalid_snapshot.unwrap_err().code,
        HostOpErrorCode::Internal
    );
    assert_eq!(
        *recorder.outcomes.lock().unwrap(),
        vec![ToolCallOutcome::Failed, ToolCallOutcome::Failed]
    );
}
