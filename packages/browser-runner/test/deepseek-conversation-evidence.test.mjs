import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import {
  observationDocument,
  extractionCaptureRecord,
  extractionPrompt,
} from "../src/ai-observation-parser.mjs";
import {
  capturedConversationCompletion as registeredCompletion,
  capturedNewConversationId as registeredConversation,
} from "../src/provider-conversation-ownership.mjs";
import {
  capturedConversationCompletion,
  capturedNewConversationId,
  validDeepSeekLifecycle,
  validDeepSeekOwnership,
  deepSeekRecoveryBinding,
  recoveredDeepSeekCompletion,
} from "../src/deepseek-conversation-evidence.mjs";

function exchange() {
  return {
    messages: [
      {
        event: "ready",
        data: {
          request_message_id: 0,
          response_message_id: 1,
          model_type: "model-a",
        },
      },
      { event: "delta", data: { arbitrary: "uninterpreted source" } },
      { event: "finish", raw_data: "not necessarily JSON" },
    ],
    lifecycle: {
      schema_version: "geo.deepseek.lifecycle.v1",
      provider: "deepseek",
      creation: {
        source: "browser_observed_create",
        chat_session_id: "synthetic-chat",
      },
      submission: {
        chat_session_id: "synthetic-chat",
        parent_message_id: null,
        model_type: "model-a",
        prompt_sha256: "a".repeat(64),
        search_enabled: true,
        thinking_enabled: false,
      },
      ready: { request_message_id: 0, response_message_id: 1 },
      sse_terminal: true,
      history: {
        source: "browser_observed_history",
        observed_at: "2026-10-12T01:02:03.000Z",
        cache_request: { cache_version: null, cache_reset_at: null },
        cache_control: "REPLACE",
        chat_session_id: "synthetic-chat",
        messages: [
          {
            message_id: 0,
            parent_id: null,
            role: "USER",
            status: "FINISHED",
            has_pending_fragment: false,
          },
          {
            message_id: 1,
            parent_id: 0,
            role: "ASSISTANT",
            status: "FINISHED",
            has_pending_fragment: false,
          },
        ],
      },
    },
  };
}

function redactedExchange() {
  const value = exchange();
  delete value.messages[2].raw_data;
  value.source_redaction = {
    schema_version: "geo.observation.source-redaction.v1",
    policy: "reasoning-and-private-fields",
    omitted_paths: ["/messages/2/raw_data"],
  };
  return value;
}

test("close completion preserves independent exact history and terminal ordering", () => {
  for (const data of [
    {},
    { click_behavior: "retry" },
    { click_behavior: "none" },
  ]) {
    for (const order of ["close", "finish-close", "close-finish"]) {
      const value = exchange();
      value.capture_status = {
        schema_version: "geo.observation.capture-status.v1",
        transport: "complete",
        termination: "complete",
      };
      const finish = value.messages.pop();
      const close = { event: "close", data };
      value.messages.push(
        ...(order === "close"
          ? [close]
          : order === "finish-close"
            ? [finish, close]
            : [close, finish]),
      );
      assert.ok(capturedConversationCompletion(value));
      if (order === "close") {
        for (const status of [
          undefined,
          { ...value.capture_status, transport: "partial", termination: "eof" },
          { ...value.capture_status, termination: "eof" },
        ]) {
          const invalid = structuredClone(value);
          invalid.capture_status = status;
          assert.equal(capturedConversationCompletion(invalid), null);
        }
      }
      for (const mutate of [
        (v) => v.messages.push(close),
        (v) => v.messages.push({ event: "delta", data: {} }),
        (v) => v.messages.shift(),
        (v) => {
          v.lifecycle.sse_terminal = false;
        },
        (v) => {
          v.lifecycle.history.messages[1].status = "PENDING";
        },
        (v) => {
          v.lifecycle.history.messages[1].parent_id = 99;
        },
        (v) => {
          delete v.lifecycle;
        },
      ]) {
        const invalid = structuredClone(value);
        mutate(invalid);
        assert.equal(capturedConversationCompletion(invalid), null);
      }
    }
  }
  for (const data of [
    null,
    [],
    { click_behavior: null },
    { click_behavior: "unknown" },
  ]) {
    const value = exchange();
    value.messages[2] = { event: "close", data };
    assert.equal(capturedConversationCompletion(value), null);
  }
  const omitted = exchange();
  omitted.messages[2] = { event: "close" };
  omitted.source_redaction = {
    schema_version: "geo.observation.source-redaction.v1",
    policy: "reasoning-and-private-fields",
    omitted_paths: ["/messages/2/data"],
  };
  assert.equal(capturedConversationCompletion(omitted), null);
  const noMarker = exchange();
  noMarker.messages.pop();
  assert.equal(capturedConversationCompletion(noMarker), null);
});

function ownedExchange() {
  const value = exchange();
  const { creation, submission } = structuredClone(value.lifecycle);
  value.ownership = {
    schema_version: "geo.deepseek.ownership.v1",
    provider: "deepseek",
    creation,
    submission,
  };
  return value;
}

test("minimal creation/request ownership survives absent history without granting completion", () => {
  const value = ownedExchange();
  delete value.lifecycle;
  value.messages = [];
  assert.equal(validDeepSeekOwnership(value.ownership), true);
  assert.equal(
    registeredConversation(value, { provider: "deepseek" }),
    "synthetic-chat",
  );
  assert.equal(registeredConversation(value, { provider: "kimi" }), null);
  assert.equal(registeredConversation(value, { provider: "unknown" }), null);
  assert.equal(capturedConversationCompletion(value), null);
  const record = extractionCaptureRecord(value, { provider: "deepseek" });
  const document = JSON.parse(record.source_json);
  assert.deepEqual(document.ownership, value.ownership);
  assert.equal(record.completion, undefined);
  assert.equal(
    record.source_sha256,
    createHash("sha256").update(record.source_json).digest("hex"),
  );
  assert.equal(
    extractionPrompt(document).includes("geo.deepseek.ownership.v1"),
    false,
  );
  value.ownership.submission.prompt_sha256 = "b".repeat(64);
  assert.notEqual(
    extractionCaptureRecord(value).source_sha256,
    record.source_sha256,
  );
  assert.equal(document.ownership.submission.prompt_sha256, "a".repeat(64));
});

test("minimal ownership is closed at every level and all fields are required", () => {
  for (const path of [[], ["creation"], ["submission"]]) {
    const value = ownedExchange().ownership;
    const target = path.reduce((v, key) => v[key], value);
    target.extra = true;
    assert.equal(validDeepSeekOwnership(value), false);
    delete target.extra;
    for (const key of Object.keys(target)) {
      const previous = target[key];
      delete target[key];
      assert.equal(validDeepSeekOwnership(value), false, `${path}.${key}`);
      target[key] = previous;
    }
  }
  for (const mutate of [
    (v) => (v.provider = "kimi"),
    (v) => (v.schema_version = "geo.deepseek.ownership.v2"),
    (v) => (v.creation.source = "model_output"),
    (v) => (v.creation.chat_session_id = "bad/chat"),
    (v) => (v.submission.chat_session_id = "other-chat"),
    (v) => (v.submission.model_type = "bad model"),
    (v) => (v.submission.prompt_sha256 = "A".repeat(64)),
    (v) => (v.submission.parent_message_id = 0),
    (v) => (v.submission.search_enabled = false),
    (v) => (v.submission.thinking_enabled = true),
  ]) {
    const value = ownedExchange();
    delete value.lifecycle;
    mutate(value.ownership);
    assert.equal(validDeepSeekOwnership(value.ownership), false);
    assert.equal(capturedNewConversationId(value), null);
    assert.throws(
      () => observationDocument(value),
      /observation_lifecycle_invalid/u,
    );
  }
});

test("both proofs must agree, including model/hash, with no malformed-proof fallback", () => {
  const valid = ownedExchange();
  assert.deepEqual(
    capturedConversationCompletion(valid),
    capturedConversationCompletion(exchange()),
  );
  for (const mutate of [
    (v) => (v.ownership = null),
    (v) => (v.lifecycle = null),
    (v) => (v.ownership.submission.prompt_sha256 = "b".repeat(64)),
    (v) => (v.ownership.submission.model_type = "model-b"),
    (v) => {
      v.ownership.creation.chat_session_id = "other-chat";
      v.ownership.submission.chat_session_id = "other-chat";
    },
    (v) => (v.messages[0].data.model_type = "model-b"),
  ]) {
    const value = ownedExchange();
    mutate(value);
    assert.equal(capturedNewConversationId(value), null);
    assert.equal(capturedConversationCompletion(value), null);
    assert.throws(
      () => observationDocument(value),
      /observation_lifecycle_invalid/u,
    );
  }
  const value = ownedExchange();
  delete value.lifecycle;
  value.messages[0].data.model_type = "model-b";
  assert.equal(capturedNewConversationId(value), null);
});

test("saved redacted finish framing preserves completion without restoring opaque bytes", () => {
  const value = redactedExchange();
  const record = extractionCaptureRecord(value, { provider: "deepseek" });
  const saved = JSON.parse(record.source_json);
  assert.deepEqual(
    record.completion,
    capturedConversationCompletion(exchange()),
  );
  assert.deepEqual(capturedConversationCompletion(saved), record.completion);
  assert.equal(Object.hasOwn(saved.messages[2], "raw_data"), false);
  assert.equal(extractionPrompt(saved).includes("not necessarily JSON"), false);
  value.messages.unshift({ event: "unknown", data: {} });
  value.source_redaction.omitted_paths = ["/messages/3/raw_data"];
  assert.deepEqual(capturedConversationCompletion(value), record.completion);
});

test("redacted finish requires exact unique absent paths in a closed valid manifest", () => {
  const invalid = {
    "missing manifest": (v) => delete v.source_redaction,
    "null manifest": (v) => (v.source_redaction = null),
    "wrong schema": (v) => (v.source_redaction.schema_version = "unknown"),
    "wrong policy": (v) => (v.source_redaction.policy = "unknown"),
    "extra field": (v) => (v.source_redaction.extra = true),
    "missing paths": (v) => delete v.source_redaction.omitted_paths,
    "empty paths": (v) => (v.source_redaction.omitted_paths = []),
    "wrong event index": (v) =>
      (v.source_redaction.omitted_paths = ["/messages/1/raw_data"]),
    "noncanonical index": (v) =>
      (v.source_redaction.omitted_paths = ["/messages/02/raw_data"]),
    "wrong field": (v) =>
      (v.source_redaction.omitted_paths = ["/messages/2/data"]),
    duplicate: (v) =>
      v.source_redaction.omitted_paths.push("/messages/2/raw_data"),
    "non-string path": (v) => v.source_redaction.omitted_paths.push(null),
    "invalid escape": (v) =>
      v.source_redaction.omitted_paths.push("/messages/1/~2"),
    "out of bounds": (v) =>
      v.source_redaction.omitted_paths.push("/messages/3/raw_data"),
    "present field": (v) =>
      v.source_redaction.omitted_paths.push("/messages/0/data"),
    "null raw data": (v) => (v.messages[2].raw_data = null),
    "present raw data": (v) => (v.messages[2].raw_data = ""),
    "wrong event": (v) => (v.messages[2].event = "close"),
    "missing finish": (v) => v.messages.pop(),
    "missing ready": (v) => v.messages.splice(0, 1),
    "missing history": (v) => delete v.lifecycle.history,
    "modified ready ID": (v) => (v.messages[0].data.response_message_id = 2),
    "modified history ID": (v) =>
      (v.lifecycle.history.messages[1].message_id = 2),
  };
  for (const [label, mutate] of Object.entries(invalid)) {
    const value = redactedExchange();
    mutate(value);
    assert.equal(capturedConversationCompletion(value), null, label);
  }
});

test("shared source persistence retains lifecycle without exposing it as AI answer evidence", () => {
  const source = exchange();
  const document = observationDocument(source);
  assert.deepEqual(document.lifecycle, source.lifecycle);
  assert.notEqual(document.lifecycle, source.lifecycle);
  const record = extractionCaptureRecord(source, { provider: "deepseek" });
  assert.deepEqual(JSON.parse(record.source_json), document);
  assert.equal(
    record.source_sha256,
    createHash("sha256").update(record.source_json).digest("hex"),
  );
  assert.equal(record.completion.outcome, "complete");
  assert.deepEqual(record.completion.message_inventory, [
    ["0", "user"],
    ["1", "assistant"],
  ]);
  assert.equal(
    registeredConversation(source, { provider: "deepseek" }),
    "synthetic-chat",
  );
  assert.equal(
    registeredCompletion(source, { provider: "unregistered" }),
    null,
  );
  assert.equal(registeredCompletion(source, { provider: "kimi" }), null);
  assert.equal(
    extractionPrompt(document).includes("geo.deepseek.lifecycle.v1"),
    false,
  );
  source.lifecycle.history.messages[1].status = "WIP";
  assert.equal(document.lifecycle.history.messages[1].status, "FINISHED");
  assert.throws(
    () => observationDocument(source),
    /observation_lifecycle_invalid/u,
  );
});

test("full browser lifecycle and matching raw stream identify one finished graph", () => {
  const value = exchange();
  assert.equal(validDeepSeekLifecycle(value.lifecycle), true);
  assert.equal(capturedNewConversationId(value), "synthetic-chat");
  assert.deepEqual(capturedConversationCompletion(value), {
    outcome: "complete",
    message_inventory: [
      ["0", "user"],
      ["1", "assistant"],
    ],
  });
  value.lifecycle.history.messages.reverse();
  value.lifecycle.history.current_message_id = 1;
  value.lifecycle.history.messages[0].quasi_status = "FINISHED";
  value.lifecycle.history.messages[1].quasi_status = null;
  assert.equal(validDeepSeekLifecycle(value.lifecycle), true);
});

test("source content never supplies missing browser lifecycle", () => {
  const value = exchange();
  delete value.lifecycle;
  value.messages.push({ event: "message", data: exchange().lifecycle });
  assert.equal(capturedNewConversationId(value), null);
  assert.equal(capturedConversationCompletion(value), null);
  assert.equal(validDeepSeekLifecycle(null), false);
});

const mutations = {
  provider: (v) => (v.provider = "other"),
  version: (v) => (v.schema_version = "geo.deepseek.lifecycle.v2"),
  "creation source": (v) => (v.creation.source = "model_output"),
  "submission chat": (v) => (v.submission.chat_session_id = "other"),
  "history chat": (v) => (v.history.chat_session_id = "other"),
  "history source": (v) => (v.history.source = "model_output"),
  "invalid chat label": (v) => (v.creation.chat_session_id = "bad/chat"),
  "non-root submission": (v) => (v.submission.parent_message_id = 9),
  search: (v) => (v.submission.search_enabled = false),
  thinking: (v) => (v.submission.thinking_enabled = true),
  model: (v) => (v.submission.model_type = ""),
  hash: (v) => (v.submission.prompt_sha256 = "not-a-hash"),
  "unsafe ID": (v) => (v.ready.response_message_id = 9007199254740992),
  "string ID": (v) => (v.ready.response_message_id = "1"),
  "negative ID": (v) => (v.ready.request_message_id = -1),
  "same IDs": (v) => (v.ready.response_message_id = 0),
  "history ID": (v) => (v.history.messages[1].message_id = 2),
  parent: (v) => (v.history.messages[1].parent_id = null),
  "root parent": (v) => (v.history.messages[0].parent_id = 1),
  role: (v) => (v.history.messages[1].role = "USER"),
  status: (v) => (v.history.messages[1].status = "INCOMPLETE"),
  "user status": (v) => (v.history.messages[0].status = "INCOMPLETE"),
  pending: (v) => (v.history.messages[1].has_pending_fragment = true),
  quasi: (v) => (v.history.messages[1].quasi_status = "GENERATING"),
  terminal: (v) => (v.sse_terminal = false),
  incremental: (v) => (v.history.cache_control = "APPEND"),
  "cached request": (v) => (v.history.cache_request.cache_version = 1),
  "cache reset": (v) => (v.history.cache_request.cache_reset_at = 1),
  "current message": (v) => (v.history.current_message_id = 0),
  "missing node": (v) => v.history.messages.pop(),
  "extra node": (v) => v.history.messages.push({ ...v.history.messages[1] }),
  "invalid date": (v) => (v.history.observed_at = "2026-02-30T00:00:00Z"),
  "missing timezone": (v) => (v.history.observed_at = "2026-10-12T00:00:00"),
};
for (const [label, mutate] of Object.entries(mutations)) {
  test(`reject lifecycle with ${label}`, () => {
    const value = exchange();
    mutate(value.lifecycle);
    assert.equal(validDeepSeekLifecycle(value.lifecycle), false);
    assert.equal(capturedNewConversationId(value), null);
    assert.equal(capturedConversationCompletion(value), null);
  });
}

test("all lifecycle objects have closed and required keys", () => {
  const paths = [
    [],
    ["creation"],
    ["submission"],
    ["ready"],
    ["history"],
    ["history", "cache_request"],
    ["history", "messages", 0],
    ["history", "messages", 1],
  ];
  for (const path of paths) {
    const value = exchange().lifecycle;
    const target = path.reduce((v, key) => v[key], value);
    target.extra = true;
    assert.equal(validDeepSeekLifecycle(value), false, path.join("."));
    delete target.extra;
    for (const key of Object.keys(target)) {
      const previous = target[key];
      delete target[key];
      assert.equal(validDeepSeekLifecycle(value), false, `${path}.${key}`);
      target[key] = previous;
    }
  }
});

test("terminal history alone does not replace the actual ordered raw stream", () => {
  const badStreams = [
    [],
    [{ event: "finish", raw_data: "" }],
    exchange().messages.slice(0, 2),
    [...exchange().messages, exchange().messages[0]],
    [...exchange().messages, exchange().messages[2]],
    [exchange().messages[1], ...exchange().messages],
    [...exchange().messages, exchange().messages[1]],
    [...exchange().messages, null],
  ];
  for (const messages of badStreams) {
    const value = { ...exchange(), messages };
    assert.equal(capturedNewConversationId(value), "synthetic-chat");
    assert.equal(capturedConversationCompletion(value), null);
  }
  for (const [key, mismatch] of [
    ["request_message_id", 2],
    ["response_message_id", "1"],
    ["model_type", "model-b"],
  ]) {
    const value = exchange();
    value.messages[0].data[key] = mismatch;
    assert.equal(capturedConversationCompletion(value), null);
  }
});

test("completion leaves arbitrary delta content and finish bytes uninterpreted", () => {
  const value = exchange();
  value.messages[1].data = {
    chat_session_id: "not-an-ownership-claim",
    status: "GENERATING",
    body: JSON.stringify({ lifecycle: { provider: "other" } }),
  };
  delete value.messages[0].data.model_type;
  assert.notEqual(capturedConversationCompletion(value), null);
  value.messages[0].data.model_type = null;
  value.lifecycle.history.current_message_id = null;
  assert.notEqual(capturedConversationCompletion(value), null);
});

test("safe numeric IDs are rendered as canonical decimal strings", () => {
  const value = exchange();
  const id = Number.MAX_SAFE_INTEGER;
  value.messages[0].data.response_message_id = id;
  value.lifecycle.ready.response_message_id = id;
  value.lifecycle.history.messages[1].message_id = id;
  assert.deepEqual(capturedConversationCompletion(value)?.message_inventory, [
    ["0", "user"],
    ["9007199254740991", "assistant"],
  ]);
});

test("history time is bounded RFC3339 and permits a timezone offset", () => {
  const value = exchange().lifecycle;
  value.history.observed_at = "2024-02-29T01:02:03+08:00";
  assert.equal(validDeepSeekLifecycle(value), true);
  for (const observedAt of [
    "2025-02-29T01:02:03Z",
    "2024-02-29T24:02:03Z",
    "2024-02-29T01:02:03+24:00",
    `2024-02-29T01:02:03.${"0".repeat(64)}Z`,
  ]) {
    value.history.observed_at = observedAt;
    assert.equal(validDeepSeekLifecycle(value), false);
  }
});

test("raw envelopes retain the decoder's event and object-data contract", () => {
  for (const envelope of [
    { data: {} },
    { event: "delta", data: null },
    { event: "unknown", data: "not an object" },
    { event: "finish", data: {} },
  ]) {
    const value = exchange();
    value.messages.splice(1, 0, envelope);
    assert.equal(capturedConversationCompletion(value), null);
  }
  const value = exchange();
  value.messages.unshift({ event: "unknown", data: {} });
  assert.notEqual(capturedConversationCompletion(value), null);
});

function recoverySource() {
  const value = ownedExchange();
  delete value.lifecycle;
  return value;
}

test("recovery derives original IDs and verifies terminal source before joining fresh history", () => {
  const source = recoverySource();
  const history = exchange().lifecycle.history;
  assert.deepEqual(deepSeekRecoveryBinding(source, "synthetic-chat"), {
    requestMessageId: 0,
    responseMessageId: 1,
  });
  assert.deepEqual(
    recoveredDeepSeekCompletion(source, history, "synthetic-chat"),
    {
      outcome: "complete",
      message_inventory: [
        ["0", "user"],
        ["1", "assistant"],
      ],
    },
  );
  assert.equal(Object.hasOwn(source, "lifecycle"), false);
  for (const mutate of [
    (v) => delete v.ownership,
    (v) => (v.lifecycle = exchange().lifecycle),
    (v) => v.messages.shift(),
    (v) => v.messages.push(v.messages[0]),
    (v) => v.messages.pop(),
    (v) => (v.messages[0].data.response_message_id = 0),
    (v) => (v.messages[0].data.response_message_id = "1"),
    (v) => (v.messages[0].data.request_message_id = -0),
    (v) => (v.messages[0].data.model_type = "other-model"),
    (v) => {
      v.messages[2] = { event: "close", data: {} };
      v.capture_status = {
        schema_version: "geo.observation.capture-status.v1",
        transport: "partial",
        termination: "eof",
      };
    },
    (v) => {
      v.messages[2] = { event: "close", data: { click_behavior: "unknown" } };
    },
  ]) {
    const invalid = recoverySource();
    mutate(invalid);
    assert.equal(deepSeekRecoveryBinding(invalid, "synthetic-chat"), null);
    assert.equal(
      recoveredDeepSeekCompletion(invalid, history, "synthetic-chat"),
      null,
    );
  }
  assert.equal(deepSeekRecoveryBinding(source, "other-chat"), null);
  for (const [, mutate] of Object.entries(mutations)) {
    const invalid = exchange().lifecycle;
    mutate(invalid);
    if (JSON.stringify(invalid.history) === JSON.stringify(history)) continue;
    assert.equal(
      recoveredDeepSeekCompletion(source, invalid.history, "synthetic-chat"),
      null,
    );
  }
});

test("cleared terminal failure permits only the exact fresh empty inventory", () => {
  const source = recoverySource();
  source.messages = [
    source.messages[0],
    {
      event: "hint",
      data: {
        type: "error",
        finish_reason: "backend_err",
        clear_response: true,
      },
    },
    { event: "close", data: {} },
  ];
  source.capture_status = {
    schema_version: "geo.observation.capture-status.v1",
    transport: "complete",
    termination: "complete",
  };
  const history = {
    ...exchange().lifecycle.history,
    messages: [],
    current_message_id: null,
  };
  assert.deepEqual(
    recoveredDeepSeekCompletion(source, history, "synthetic-chat"),
    { outcome: "cleared_failure", message_inventory: [] },
  );
  assert.equal(
    recoveredDeepSeekCompletion(recoverySource(), history, "synthetic-chat"),
    null,
  );
  for (const mutate of [
    (v) => (v.messages[1].data.clear_response = false),
    (v) => (v.messages[1].data.type = "notice"),
    (v) => (v.messages[1].data.finish_reason = "unknown"),
    (v) => v.messages.splice(1, 0, { event: "delta", data: {} }),
    (v) => (v.capture_status.transport = "partial"),
    (v) => (v.capture_status.termination = "eof"),
  ]) {
    const invalid = structuredClone(source);
    mutate(invalid);
    assert.equal(
      recoveredDeepSeekCompletion(invalid, history, "synthetic-chat"),
      null,
    );
  }
  for (const mutate of [
    (v) => (v.chat_session_id = "other-chat"),
    (v) => (v.current_message_id = 1),
    (v) => delete v.current_message_id,
    (v) => (v.cache_control = "APPEND"),
    (v) => (v.cache_request.cache_version = 1),
    (v) => (v.cache_request.cache_reset_at = 1),
    (v) => (v.source = "model_output"),
    (v) => (v.observed_at = "2026-02-30T00:00:00Z"),
    (v) => (v.extra = true),
  ]) {
    const invalid = structuredClone(history);
    mutate(invalid);
    assert.equal(
      recoveredDeepSeekCompletion(source, invalid, "synthetic-chat"),
      null,
    );
  }
});
