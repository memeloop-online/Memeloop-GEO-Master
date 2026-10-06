//! The minimal, allow-listed JavaScript bundle used by the compatibility
//! probe.
//!
//! The upstream `memeloop/loop-api` entry cannot be loaded in a bare embedded
//! runtime: its chunk graph imports `node:fs`, `node:http`, `node:https`,
//! `node:net`, `node:crypto`, `node:child_process`, `node:os` and
//! `node:events`.  Until those are replaced by host ops (or an upstream
//! embedded-runtime export lands), the probe pins the *shape* of the loop in a
//! host-neutral bundle so the engine contract can be proven independently of
//! the bundle-porting work.

/// The loop core, mirroring the host-neutral surface of `memeloop/loop-api`:
/// a turn awaits a model host op, then runs tools, then returns a result.
pub const LOOP_CORE_JS: &str = r#"
export async function runToolLoop({ turnId, prompt, tools }) {
  const answer = await Deno.core.ops.op_host_model_complete(prompt);
  const toolCalls = [];
  for (const tool of tools) {
    toolCalls.push({ name: tool.name, observation: await tool.run() });
  }
  return { turnId, answer, toolCalls };
}
"#;

/// The loop script.  It uses top-level await, so module evaluation itself
/// depends on the engine driving a microtask queue.
pub const GEO_LOOP_JS: &str = r#"
import { runToolLoop } from "./loop-core.js";

const tools = [
  {
    name: "knowledge.search",
    run: async () => "evidence: quoted evidence from the knowledge release",
  },
];

export async function main() {
  const result = await runToolLoop({
    turnId: "turn-probe-1",
    prompt: "how long is the warranty?",
    tools,
  });
  await Deno.core.ops.op_host_emit("loop.completed", JSON.stringify(result));
  return result;
}

await main();

export const ready = true;
"#;

/// The bundle an embedding host would ship: absolute, host-neutral specifiers
/// with no filesystem or network reachability.
pub const PROBE_BUNDLE: &[(&str, &str)] = &[
    ("memeloop://bundle/loop-core.js", LOOP_CORE_JS),
    ("memeloop://bundle/geo-loop.js", GEO_LOOP_JS),
];

/// The module the probe evaluates as its entry point.
pub const MAIN_MODULE: &str = "memeloop://bundle/geo-loop.js";

/// The host façade over the production op surface.
///
/// A script reaches a capability only by naming an op here, and an op that the
/// Rust host did not register is refused rather than silently skipped.  Results
/// are parsed once, so call sites never see a raw JSON string, and every
/// failure is the structured `GeoHostOpError` the Rust side produced.
pub const HOST_OPS_JS: &str = r#"
export const HOST_OPS_VERSION = "geo.hostops.v10";

const OPS = {
  modelComplete: "op_host_model_complete_v1",
  knowledgeSearch: "op_host_knowledge_search_v1",
  knowledgeImportStatus: "op_host_knowledge_import_status_v1",
  manifestRead: "op_host_manifest_read_v2",
  publishSubmit: "op_host_publish_submit_v2",
  measureSample: "op_host_measure_sample_v2",
  reportGet: "op_host_report_get_v1",
  reportPreview: "op_host_report_preview_v1",
  reportReduce: "op_host_report_reduce_v1",
  channelDiscover: "op_host_channel_discover_v1",
  channelPlan: "op_host_channel_plan_v1",
  questionDiscover: "op_host_question_discover_v1",
  questionCreate: "op_host_question_create_v1",
  questionRevise: "op_host_question_revise_v1",
  channelManifestRead: "op_host_channel_manifest_read_v1",
  channelTargetExecute: "op_host_channel_target_execute_v1",
  contentItemsRead: "op_host_content_items_read_v1",
  contentPrepare: "op_host_content_prepare_v1",
  contentGenerate: "op_host_content_generate_v1",
  contentCheck: "op_host_content_check_v1",
  contentRepair: "op_host_content_repair_v1",
  contentClose: "op_host_content_close_v1",
  contentStart: "op_host_content_start_v1",
  contentExecutionRead: "op_host_content_execution_read_v1",
  distributionStart: "op_host_distribution_start_v1",
  distributionRead: "op_host_distribution_read_v1",
  distributionResume: "op_host_distribution_resume_v1",
  distributionTargetsRead: "op_host_distribution_targets_read_v1",
};

async function callOp(opName, payload) {
  const op = Deno.core.ops[opName];
  if (typeof op !== "function") {
    throw new Error(`host op ${opName} is not part of the approved surface`);
  }
  return JSON.parse(await op(JSON.stringify(payload)));
}

export const hostOps = {
  version: HOST_OPS_VERSION,
  modelComplete: (request) => callOp(OPS.modelComplete, request),
  knowledgeSearch: (request) => callOp(OPS.knowledgeSearch, request),
  knowledgeImportStatus: (request) => callOp(OPS.knowledgeImportStatus, request),
  manifestRead: (request) => callOp(OPS.manifestRead, request),
  publishSubmit: (request) => callOp(OPS.publishSubmit, request),
  measureSample: (request) => callOp(OPS.measureSample, request),
  reportGet: (request) => callOp(OPS.reportGet, request),
  reportPreview: (request) => callOp(OPS.reportPreview, request),
  reportReduce: (request) => callOp(OPS.reportReduce, request),
  channelDiscover: (request) => callOp(OPS.channelDiscover, request),
  channelPlan: (request) => callOp(OPS.channelPlan, request),
  questionDiscover: (request) => callOp(OPS.questionDiscover, request),
  questionCreate: (request) => callOp(OPS.questionCreate, request),
  questionRevise: (request) => callOp(OPS.questionRevise, request),
  channelManifestRead: (request) => callOp(OPS.channelManifestRead, request),
  channelTargetExecute: (request) => callOp(OPS.channelTargetExecute, request),
  contentItemsRead: (request) => callOp(OPS.contentItemsRead, request),
  contentPrepare: (request) => callOp(OPS.contentPrepare, request),
  contentGenerate: (request) => callOp(OPS.contentGenerate, request),
  contentCheck: (request) => callOp(OPS.contentCheck, request),
  contentRepair: (request) => callOp(OPS.contentRepair, request),
  contentClose: (request) => callOp(OPS.contentClose, request),
  contentStart: (request) => callOp(OPS.contentStart, request),
  contentExecutionRead: (request) => callOp(OPS.contentExecutionRead, request),
  distributionStart: (request) => callOp(OPS.distributionStart, request),
  distributionRead: (request) => callOp(OPS.distributionRead, request),
  distributionResume: (request) => callOp(OPS.distributionResume, request),
  distributionTargetsRead: (request) => callOp(OPS.distributionTargetsRead, request),
};

// Reports one op outcome through the Rust-owned event op, so the host can
// observe a typed result without the script having to succeed.
export async function attempt(topic, thunk) {
  let record;
  try {
    record = { ok: true, value: await thunk() };
  } catch (error) {
    record = { ok: false, name: error.name, error: parseHostOpError(error) };
  }
  await Deno.core.ops.op_host_emit(topic, JSON.stringify(record));
  return record;
}

function parseHostOpError(error) {
  try {
    return JSON.parse(error.message);
  } catch {
    return { code: "unparsed", message: String(error.message) };
  }
}
"#;

/// The reference loop: the host-neutral turn shape the engine contract is
/// proven against.
///
/// The approved MemeLoop server bundle replaces this entry once the bundle port
/// lands.  Evaluating this module does not run a turn: it declares the surface
/// it expects through a `loop.ready` event, and the embedding host calls `main`
/// once per turn with that turn's inputs.
///
/// `query` defaults to `prompt` so a host payload that omits it still runs the
/// turn.  Without the default the omission survives `JSON.stringify` as a
/// missing key, and the very first host op would reject with a typed
/// `invalid_request` — a turn failing after doing real work, blaming the
/// runtime for the host's own gap.
pub const HOST_LOOP_JS: &str = r#"
import { hostOps } from "./host-ops.js";

export { hostOps, attempt } from "./host-ops.js";

export async function main({ prompt, query = prompt }) {
  const retrieval = await hostOps.knowledgeSearch({ query });
  const completion = await hostOps.modelComplete({ prompt });
  const result = {
    answer: completion.text,
    model: completion.model,
    evidence: retrieval.evidence,
  };
  await Deno.core.ops.op_host_emit("loop.completed", JSON.stringify(result));
  return result;
}

await Deno.core.ops.op_host_emit(
  "loop.ready",
  JSON.stringify({ version: hostOps.version, capabilities: Object.keys(hostOps) }),
);

export const ready = true;
"#;

/// The reference bundle an embedding host may run before the approved MemeLoop
/// bundle is available.
pub const HOST_BUNDLE: &[(&str, &str)] = &[
    ("memeloop://bundle/host-ops.js", HOST_OPS_JS),
    ("memeloop://bundle/host-loop.js", HOST_LOOP_JS),
];

/// The module the reference bundle exports its entry from.
pub const HOST_MAIN_MODULE: &str = "memeloop://bundle/host-loop.js";

/// The event topic the reference loop reports a completed turn through.
///
/// Named beside the JavaScript that emits it (see `HOST_LOOP_JS`) so the host
/// and the bundle cannot drift apart about the one channel a turn result
/// travels on.  A bundle that reports through a different topic is not a
/// completed turn, and the host says so rather than reporting an empty answer.
pub const TURN_COMPLETION_TOPIC: &str = "loop.completed";
