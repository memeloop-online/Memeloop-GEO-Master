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
    HOST_OP_ERROR_NAME, HostBridge, HostOp, HostOpError, KnowledgeSearchRequest,
    KnowledgeSearchResult, ManifestReadRequest, MeasureRequest, ModelCompletionRequest,
    PublishRequest,
};

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
pub async fn op_host_manifest_read_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::ManifestRead;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<ManifestReadRequest>(op, &request)?;
    if request.limit.is_some_and(|limit| limit == 0 || limit > 100) {
        return Err(js_error(HostOpError::invalid_request(
            op,
            "limit must be between 1 and 100",
        )));
    }
    let page = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .manifest_read(bridge.scope(), request)
                .await
        })
        .await?;
    if page.items.len() > 100 {
        return Err(js_error(HostOpError::internal(
            op,
            "manifest page exceeded the declared page size",
        )));
    }
    Ok(encode(op, &page)?)
}

/// Submits one document revision to one platform target.
///
/// The branch identity and the idempotency key are derived by Rust from the
/// frozen manifest, so a script cannot re-key a duplicate submission.
#[op2]
#[string]
pub async fn op_host_publish_submit_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::Publish;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<PublishRequest>(op, &request)?;
    let receipt = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .publish_submit(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &receipt)?)
}

/// Takes one independent AI channel measurement sample.
#[op2]
#[string]
pub async fn op_host_measure_sample_v1(
    state: Rc<RefCell<OpState>>,
    #[string] request: String,
) -> Result<String, JsErrorBox> {
    let op = HostOp::Measure;
    let bridge = bridge(&state.borrow())?;
    let request = parse_request::<MeasureRequest>(op, &request)?;
    let sample = bridge
        .invoke(op, |bridge| async move {
            bridge
                .capabilities()
                .measure_sample(bridge.scope(), request)
                .await
        })
        .await?;
    Ok(encode(op, &sample)?)
}

/// The capabilities the production extension registers, as the isolate sees
/// them.  Exported so the crate (and its tests) can assert the surface without
/// enumerating registered ops from JavaScript.
pub const PRODUCTION_OP_NAMES: [&str; HostOp::COUNT + 2] = [
    HostOp::ModelComplete.op_name(),
    HostOp::KnowledgeSearch.op_name(),
    HostOp::ManifestRead.op_name(),
    HostOp::Publish.op_name(),
    HostOp::Measure.op_name(),
    // Rust-owned run state: the loop's emit contract and the checkpoint probe.
    "op_host_emit",
    "op_host_checkpoint",
];
