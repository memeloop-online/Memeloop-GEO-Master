//! The production host ops: the isolate-facing half of the boundary declared in
//! [`crate::host`].
//!
//! Each op parses a JSON payload, delegates to the [`HostBridge`] installed in
//! the runtime, and encodes the outcome as JSON.  The bodies stay thin on
//! purpose: budget, deadline and cancellation live in
//! [`HostBridge::invoke`], so a new op cannot accidentally ship without them,
//! and the capability itself lives in the [`HostOps`] implementation the API
//! process provides.
//!
//! There is no op here for SQL, network, files, processes, environment
//! variables or credentials, and no op that returns a synthesised answer when a
//! capability is missing: a missing capability is always a typed error.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::op2;
use deno_error::JsErrorBox;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::host::{
    ChannelDiscoverRequest, ChannelManifestReadRequest, ChannelPlanRequest,
    ChannelTargetExecuteRequest, ContentCloseRequest, ContentExecutionReadRequest,
    ContentItemsReadRequest, ContentStartRequest, ContentStepRequest, DistributionReadRequest,
    DistributionResumeRequest, DistributionStartRequest, DistributionTargetsReadRequest,
    HOST_OP_ERROR_NAME, HostBridge, HostOp, HostOpError, KnowledgeImportAttachmentsRequest,
    KnowledgeSearchRequest, KnowledgeSearchResult, ManifestReadRequest, MeasureRequest,
    ModelCompletionRequest, PublishRequest, ReportGetRequest, ReportReduceRequest,
};

#[op2]
#[string]
pub async fn op_host_content_items_read_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ContentItemsRead;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ContentItemsReadRequest>(op, &request)?;
    if request.execution_id.is_nil()
        || request.limit.is_some_and(|n| n == 0 || n > 100)
        || request
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.len() > 256)
    {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "invalid execution, cursor or page size",
        )));
    }
    let result = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .content_items_read(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &result)?)
}

macro_rules! content_step_op {
    ($name:ident, $variant:ident, $method:ident) => {
        #[op2]
        #[string]
        pub async fn $name(
            state: Rc<RefCell<OpState>>,
            #[string] request: String,
        ) -> Result<String, JsErrorBox> {
            let op = HostOp::$variant;
            let bridge = bridge(&state.borrow())?;
            let request = parse_request::<ContentStepRequest>(op, &request)?;
            if request.execution_id.is_nil() || request.item_id.is_nil() {
                return Err(js_error(HostOpError::invalid_request(
                    op,
                    "execution and item references must be non-zero",
                )));
            }
            let requested_item = request.item_id;
            let result = bridge
                .invoke(op, |bridge| async move {
                    bridge.capabilities().$method(bridge.scope(), request).await
                })
                .await?;
            if result.item_id != requested_item || result.branch_key.is_empty() {
                return Err(js_error(HostOpError::internal(
                    op,
                    "step returned an unrelated item",
                )));
            }
            Ok(encode(op, &result)?)
        }
    };
}
content_step_op!(op_host_content_prepare_v1, ContentPrepare, content_prepare);
content_step_op!(
    op_host_content_generate_v1,
    ContentGenerate,
    content_generate
);
content_step_op!(op_host_content_check_v1, ContentCheck, content_check);

#[op2]
#[string]
pub async fn op_host_content_close_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ContentClose;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ContentCloseRequest>(op, &request)?;
    if request.execution_id.is_nil() {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "execution reference must be non-zero",
        )));
    }
    let result = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .content_close(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &result)?)
}

#[op2]
#[string]
pub async fn op_host_content_start_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ContentStart;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ContentStartRequest>(op, &request)?;
    if request.cycle_id.is_some_and(|id| id.is_nil()) {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "cycle reference must be non-zero",
        )));
    }
    let result = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .content_start(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &result)?)
}

#[op2]
#[string]
pub async fn op_host_content_execution_read_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ContentExecutionRead;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ContentExecutionReadRequest>(op, &request)?;
    if request.execution_id.is_nil() {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "execution reference must be non-zero",
        )));
    }
    let result = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .content_execution_read(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &result)?)
}

macro_rules! distribution_op {
    ($name:ident, $variant:ident, $request:ty, $method:ident, $check:expr) => {
        #[op2]
        #[string]
        pub async fn $name(
            state: Rc<RefCell<OpState>>,
            #[string] payload: String,
        ) -> Result<String, JsErrorBox> {
            let op = HostOp::$variant;
            let bridge = bridge(&state.borrow())?;
            let request = parse_request::<$request>(op, &payload)?;
            ($check)(&request)
                .map_err(|reason| js_error(HostOpError::invalid_request(op, reason)))?;
            let result = bridge
                .invoke(op, |bridge| async move {
                    bridge.capabilities().$method(bridge.scope(), request).await
                })
                .await?;
            Ok(encode(op, &result)?)
        }
    };
}

distribution_op!(
    op_host_distribution_start_v1,
    DistributionStart,
    DistributionStartRequest,
    distribution_start,
    |request: &DistributionStartRequest| if request.cycle_id.is_some_and(|id| id.is_nil()) {
        Err("cycle reference must be non-zero")
    } else {
        Ok(())
    }
);
distribution_op!(
    op_host_distribution_read_v1,
    DistributionRead,
    DistributionReadRequest,
    distribution_read,
    |request: &DistributionReadRequest| if request.cycle_id.is_some_and(|id| id.is_nil())
        || request.manifest_id.is_some_and(|id| id.is_nil())
        || request.cycle_id.is_some() && request.manifest_id.is_some()
    {
        Err("invalid or conflicting distribution selectors")
    } else {
        Ok(())
    }
);
distribution_op!(
    op_host_distribution_resume_v1,
    DistributionResume,
    DistributionResumeRequest,
    distribution_resume,
    |request: &DistributionResumeRequest| if request.manifest_id.is_nil() {
        Err("manifest reference must be non-zero")
    } else {
        Ok(())
    }
);
distribution_op!(
    op_host_distribution_targets_read_v1,
    DistributionTargetsRead,
    DistributionTargetsReadRequest,
    distribution_targets_read,
    |request: &DistributionTargetsReadRequest| if request.manifest_id.is_nil()
        || request
            .limit
            .is_some_and(|limit| !(1..=100).contains(&limit))
    {
        Err("invalid manifest reference or page limit")
    } else {
        Ok(())
    }
);

#[op2]
#[string]
pub async fn op_host_channel_discover_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ChannelDiscover;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ChannelDiscoverRequest>(op, &request)?;
    request
        .validate()
        .map_err(|reason| js_error(HostOpError::invalid_request(op, reason)))?;
    let requested = request.clone();
    let page = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .channel_discover(bridge.scope(), request)
                .await
        })
        .await?;
    page.validate_for(&requested)
        .map_err(|reason| js_error(HostOpError::internal(op, reason)))?;
    Ok(encode(op, &page)?)
}

#[op2]
#[string]
pub async fn op_host_channel_plan_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ChannelPlan;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ChannelPlanRequest>(op, &request)?;
    request
        .validate()
        .map_err(|reason| js_error(HostOpError::invalid_request(op, reason)))?;
    let requested = request.clone();
    let receipt = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .channel_plan(bridge.scope(), request)
                .await
        })
        .await?;
    receipt
        .validate_for(&requested)
        .map_err(|reason| js_error(HostOpError::internal(op, reason)))?;
    Ok(encode(op, &receipt)?)
}

#[op2]
#[string]
pub async fn op_host_channel_manifest_read_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ChannelManifestRead;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ChannelManifestReadRequest>(op, &request)?;
    request
        .validate()
        .map_err(|reason| js_error(HostOpError::invalid_request(op, reason)))?;
    let requested = request.clone();
    let page = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .channel_manifest_read(bridge.scope(), request)
                .await
        })
        .await?;
    page.validate_for(&requested)
        .map_err(|reason| js_error(HostOpError::internal(op, reason)))?;
    Ok(encode(op, &page)?)
}

#[op2]
#[string]
pub async fn op_host_channel_target_execute_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ChannelTargetExecute;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ChannelTargetExecuteRequest>(op, &request)?;
    request
        .validate()
        .map_err(|reason| js_error(HostOpError::invalid_request(op, reason)))?;
    let target_id = request.target_id;
    let result = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .channel_target_execute(bridge.scope(), request)
                .await
        })
        .await?;
    result
        .validate_for(target_id)
        .map_err(|reason| js_error(HostOpError::unknown_result(op, reason)))?;
    Ok(encode(op, &result)?)
}

/// Converts a structured failure into the JS error the script sees.
///
/// The class is stable and the message is the serialised error, so a script
/// reads `error.name` and `JSON.parse(error.message).code` instead of matching
/// provider prose.  The boundary redacts here as well as in the constructors,
/// so a bridge that hands back a raw provider message still cannot leak a
/// credential into the isolate.
fn js_error(error: HostOpError) -> JsErrorBox {
    let error = error.redacted();
    JsErrorBox::new(
        HOST_OP_ERROR_NAME,
        serde_json::to_string(&error).unwrap_or_else(|_| error.to_string()),
    )
}

impl From<HostOpError> for JsErrorBox {
    fn from(error: HostOpError) -> Self {
        js_error(error)
    }
}

/// Reads the run bridge out of the isolate state.
///
/// A runtime assembled without a bridge refuses every op rather than reaching
/// some other fallback: no bridge means no declared capability.
fn bridge(state: &OpState) -> Result<HostBridge, JsErrorBox> {
    state
        .try_borrow::<HostBridge>()
        .cloned()
        .ok_or_else(|| JsErrorBox::generic("no host-op bridge is installed in this runtime"))
}

fn parse_request<T: DeserializeOwned>(op: HostOp, payload: &str) -> Result<T, HostOpError> {
    serde_json::from_str(payload).map_err(|error| {
        HostOpError::invalid_request(op, format!("request does not match {}: {error}", op.name()))
    })
}

fn encode<T: Serialize>(op: HostOp, value: &T) -> Result<String, HostOpError> {
    serde_json::to_string(value)
        .map_err(|error| HostOpError::internal(op, format!("response cannot be encoded: {error}")))
}

/// One model completion, through the Rust-side provider bridge.
///
/// The bridge owns the endpoint, the credential, retries, usage accounting and
/// redaction; the request carries only prompt, optional system text, an
/// optional configured routing identifier and an output-token cap.
#[op2]
#[string]
pub async fn op_host_model_complete_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ModelComplete;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ModelCompletionRequest>(op, &request)?;
    let completion = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .model_complete(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &completion)?)
}

/// Imports only attachment IDs already bound to this run. The capability
/// receives the original Rust references; JavaScript cannot supply object
/// identity, version, filename, or raw bytes.
#[op2]
#[string]
pub async fn op_host_knowledge_import_attachments_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::KnowledgeImportAttachments;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<KnowledgeImportAttachmentsRequest>(op, &request)?;
    request.validate(bridge.attachments())?;
    let result = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .knowledge_import_attachments(bridge.scope(), request, bridge.attachments())
                .await
        })
        .await?;
    Ok(encode(op, &result)?)
}

/// Evidence retrieval restricted to the run's frozen knowledge release.
///
/// A bridge that reports the retrieval capability as missing fails the op: an
/// empty evidence list must never be mistaken for "the knowledge base says
/// nothing", because the two have opposite meanings for a sourced answer.
#[op2]
#[string]
pub async fn op_host_knowledge_search_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::KnowledgeSearch;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<KnowledgeSearchRequest>(op, &request)?;
    if request.limit == 0 || request.limit > 50 {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "limit must be between 1 and 50",
        )));
    }
    let result = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .knowledge_search(bridge.scope(), request)
                .await
        })
        .await?;
    if let Some(reason) = result.capability_missing.clone() {
        return Err(js_error(HostOpError::capability_missing(op, reason)));
    }
    Ok(encode(
        op,
        &KnowledgeSearchResult {
            knowledge_release_id: result.knowledge_release_id,
            evidence: result.evidence,
            capability_missing: None,
        },
    )?)
}

/// Reads one page of a frozen document or distribution manifest.
#[op2]
#[string]
pub async fn op_host_manifest_read_v2(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ManifestRead;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ManifestReadRequest>(op, &request)?;
    if let Err(reason) = request.validate() {
        return Err(js_error(HostOpError::invalid_request(op, reason)));
    }
    if request.limit.is_some_and(|limit| limit == 0 || limit > 100) {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "limit must be between 1 and 100",
        )));
    }
    let requested = request.clone();
    let page = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .manifest_read(bridge.scope(), request)
                .await
        })
        .await?;
    if let Err(reason) = page.validate_for(&requested) {
        return Err(js_error(HostOpError::internal(op, reason)));
    }
    Ok(encode(op, &page)?)
}

/// Submits one document revision to one platform target.
///
/// The Rust capability must verify this target against the frozen manifest
/// and use the durable intent key; the script cannot authorize its own target.
#[op2]
#[string]
pub async fn op_host_publish_submit_v2(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::Publish;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<PublishRequest>(op, &request)?;
    if let Err(reason) = request.validate() {
        return Err(js_error(HostOpError::invalid_request(op, reason)));
    }
    let receipt = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .publish_submit(bridge.scope(), request)
                .await
        })
        .await?;
    if let Err(reason) = receipt.validate() {
        return Err(js_error(HostOpError::unknown_result(op, reason)));
    }
    Ok(encode(op, &receipt)?)
}

/// Takes one independent AI channel measurement sample.
#[op2]
#[string]
pub async fn op_host_measure_sample_v2(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::Measure;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<MeasureRequest>(op, &request)?;
    if let Err(reason) = request.validate() {
        return Err(js_error(HostOpError::invalid_request(op, reason)));
    }
    let requested = request.clone();
    let sample = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .measure_sample(bridge.scope(), request)
                .await
        })
        .await?;
    if let Err(reason) = sample.validate_for(&requested) {
        return Err(js_error(HostOpError::internal(op, reason)));
    }
    Ok(encode(op, &sample)?)
}

/// Returns only a report in the Rust-bound project scope.
#[op2]
#[string]
pub async fn op_host_report_get_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ReportGet;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ReportGetRequest>(op, &request)?;
    if request.report_id.is_some_and(|id| id.is_nil()) {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "report ID must be non-zero",
        )));
    }
    let report = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .report_get(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &report)?)
}

/// Builds a report from server-owned evidence after the frozen cutoff.
#[op2]
#[string]
pub async fn op_host_report_reduce_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ReportReduce;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ReportReduceRequest>(op, &request)?;
    if request.cycle_id.is_some_and(|id| id.is_nil())
        || request.correction_of.is_some_and(|id| id.is_nil())
    {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "report IDs must be non-zero",
        )));
    }
    let report = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .report_reduce(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &report)?)
}

/// The capabilities the production extension registers, as the isolate sees
/// them.  Exported so the crate (and its tests) can assert the surface without
/// enumerating registered ops from JavaScript.
pub const PRODUCTION_OP_NAMES: [&str; HostOp::COUNT + 2] = [
    HostOp::ModelComplete.op_name(),
    HostOp::KnowledgeSearch.op_name(),
    HostOp::KnowledgeImportAttachments.op_name(),
    HostOp::ManifestRead.op_name(),
    HostOp::Publish.op_name(),
    HostOp::Measure.op_name(),
    HostOp::ReportGet.op_name(),
    HostOp::ReportReduce.op_name(),
    HostOp::ChannelDiscover.op_name(),
    HostOp::ChannelPlan.op_name(),
    HostOp::ChannelManifestRead.op_name(),
    HostOp::ChannelTargetExecute.op_name(),
    HostOp::ContentItemsRead.op_name(),
    HostOp::ContentPrepare.op_name(),
    HostOp::ContentGenerate.op_name(),
    HostOp::ContentCheck.op_name(),
    HostOp::ContentClose.op_name(),
    HostOp::ContentStart.op_name(),
    HostOp::ContentExecutionRead.op_name(),
    HostOp::DistributionStart.op_name(),
    HostOp::DistributionRead.op_name(),
    HostOp::DistributionResume.op_name(),
    HostOp::DistributionTargetsRead.op_name(),
    // Rust-owned run state: the loop's emit contract and the checkpoint probe.
    "op_host_emit",
    "op_host_checkpoint",
];
