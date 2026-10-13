import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { chromium } from "playwright";
import { observeDeepSeekWebSearch } from "../src/deepseek-web-search.mjs";
import { projectDeepseekSource } from "../src/deepseek-source-projection.mjs";
import {
  capturedConversationCompletion,
  capturedNewConversationId,
} from "../src/deepseek-conversation-evidence.mjs";

const origin = "https://chat.deepseek.com";
const payload = {
  model: "fixture-model",
  question: "Synthetic rainfall question?",
};
const modelConfigs = [
  { model_type: payload.model, enabled: true, switchable: true },
];
const source = {
  messages: [
    { state: "completed", search: "source found", text: "Synthetic answer." },
  ],
  created_session: {
    chatSessionId: "fixture-selected",
    source: "browser_observed_create",
  },
  received_at: "2026-01-01T00:00:00.000Z",
};
const candidate = {
  completion: "complete",
  completion_evidence: [{ path: "/messages/0/state" }],
  search_used: "yes",
  search_evidence: [{ path: "/messages/0/search" }],
  answer_segments: [{ path: "/messages/0/text" }],
  citations: [],
};
function fixture() {
  const calls = [];
  const page = {
    url: () => `${origin}/`,
    goto: async (url) => {
      calls.push("goto");
      assert.equal(url, `${origin}/`);
    },
    close: () => assert.fail("runner owns page disposal"),
  };
  const options = {
    expectedAccountId: "fixture-account",
    observeConfiguration: (_page, limits) => {
      calls.push("arm-config");
      assert.ok(limits.timeoutMs > 0);
      return {
        result: Promise.resolve({ model_configs: modelConfigs }),
        dispose: () => calls.push("dispose-config"),
      };
    },
    observeCreatedSessions: (_page, limits) => {
      calls.push("arm-create");
      assert.ok(limits.timeoutMs > 0);
      return { dispose: () => calls.push("dispose-create") };
    },
    identify: async () => ({ platform_account_id: "fixture-account" }),
    configure: async (_page, controls) => {
      calls.push("configure");
      assert.equal(controls.searchEnabled, true);
      assert.equal(controls.thinkingEnabled, false);
      return controls;
    },
    capture: async (_page, captureOptions) => {
      calls.push("capture");
      assert.equal(
        Object.hasOwn(captureOptions.binding, "chatSessionId"),
        false,
      );
      await captureOptions.submit(page, captureOptions.signal);
      return source;
    },
    submitQuestion: async () => calls.push("send"),
    prepareSource: (exchange) => ({
      exchange,
      redaction: {
        schema_version: "geo.observation.source-redaction.v1",
        policy: "reasoning-and-private-fields",
        omitted_paths: [],
      },
    }),
    persistConversation: async (receipt) => {
      calls.push("ownership");
      assert.equal(receipt.createdSession, source.created_session);
      assert.equal(receipt.captured, source);
      assert.equal(receipt.expectedAccountId, "fixture-account");
      return { durable: true };
    },
    onEvidence: async (record) => calls.push(record.phase),
    apiExtract: async () => {
      calls.push("extract");
      return {
        extracted: candidate,
        model: "fixture-parser",
        surface: "model_api",
      };
    },
  };
  return { page, calls, options };
}

function historyFixture() {
  const f = fixture();
  const exchange = {
    ...source,
    sse_terminal: true,
    messages: [
      {
        event: "ready",
        data: {
          request_message_id: 0,
          response_message_id: 2,
          model_type: payload.model,
        },
      },
      {
        event: "message",
        data: {
          p: "response",
          o: "SET",
          v: {
            status: "FINISHED",
            fragments: [
              { type: "RESPONSE", content: "Synthetic answer." },
              { type: "SEARCH", queries: ["synthetic rainfall"] },
              { type: "THINK", content: "Synthetic private reasoning" },
            ],
          },
        },
      },
      { event: "finish", raw_data: "[DONE]" },
    ],
  };
  const history = {
    source: "browser_observed_history",
    observed_at: "2026-01-01T00:00:01.000Z",
    cache_request: { cache_version: null, cache_reset_at: null },
    cache_control: "REPLACE",
    chat_session_id: source.created_session.chatSessionId,
    current_message_id: 2,
    messages: [
      {
        message_id: 0,
        parent_id: null,
        role: "USER",
        status: "FINISHED",
        has_pending_fragment: false,
      },
      {
        message_id: 2,
        parent_id: 0,
        role: "ASSISTANT",
        status: "FINISHED",
        has_pending_fragment: false,
      },
    ],
  };
  const originalObserver = f.options.observeCreatedSessions;
  f.options.observeCreatedSessions = (...args) => ({
    ...originalObserver(...args),
    isCurrent: (proof) => proof === exchange.created_session,
  });
  f.options.capture = async (_page, limits) => {
    f.calls.push("capture");
    await limits.submit(f.page, limits.signal);
    return exchange;
  };
  f.options.prepareSource = projectDeepseekSource;
  f.options.readHistory = async (_page, limits) => {
    f.calls.push("history");
    assert.equal(limits.conversationId, "fixture-selected");
    assert.equal(limits.requestMessageId, 0);
    assert.equal(limits.responseMessageId, 2);
    return { history, inventory: { conversationId: "fixture-selected" } };
  };
  f.options.persistConversation = async (receipt) => {
    f.calls.push("ownership");
    f.receipt = receipt;
    return { durable: true };
  };
  f.options.onEvidence = async (record) => {
    f.calls.push(record.phase);
    if (record.phase === "source") f.evidence = record;
  };
  f.options.apiExtract = async () => {
    f.calls.push("extract");
    const path = `/messages/${exchange.messages.findIndex((message) => message.event === "message")}/data/v`;
    return {
      model: "fixture-parser",
      surface: "model_api",
      extracted: {
        completion: "complete",
        completion_evidence: [{ path: `${path}/status` }],
        search_used: "yes",
        search_evidence: [{ path: `${path}/fragments/1/queries` }],
        answer_segments: [{ path: `${path}/fragments/0/content` }],
        citations: [],
      },
    };
  };
  return Object.assign(f, { exchange, history });
}

test("partial capture persists privacy-projected source without interpretation, ownership or another send", async () => {
  for (const mode of ["eof", "cancel", "persist-failed"]) {
    const f = historyFixture();
    const controller = new AbortController();
    let record;
    f.options.signal = controller.signal;
    f.options.capture = async (_page, limits) => {
      await limits.submit(f.page, limits.signal);
      if (mode === "cancel") controller.abort();
      await limits.onPartialCapture({
        messages: [
          {
            event: "ready",
            data: {
              request_message_id: 1,
              response_message_id: 2,
              future_metadata: {
                nested: { content: "private-synthetic-event-metadata" },
              },
            },
          },
          {
            event: "message",
            data: {
              p: "response",
              o: "SET",
              v: {
                fragments: [
                  { type: "RESPONSE", content: "Synthetic partial answer" },
                  { type: "THINK", content: "Private synthetic thought" },
                  { type: "FUTURE", content: "private-synthetic-unknown" },
                  {
                    type: "RESPONSE",
                    content: {
                      type: "THINK",
                      content: "private-synthetic-shaped",
                    },
                  },
                ],
                access_token: "private-synthetic-token",
                future_metadata: {
                  nested: {
                    type: "THINK",
                    content: "private-synthetic-metadata",
                  },
                  credentials: "private-synthetic-credentials",
                },
              },
            },
          },
        ],
        sse_terminal: false,
        capture_status: {
          schema_version: "geo.observation.capture-status.v1",
          transport: "partial",
          termination: mode === "cancel" ? "aborted" : "eof",
        },
      });
      return null;
    };
    f.options.onEvidence = async (value) => {
      await new Promise((resolve) => setImmediate(resolve));
      record = value;
      if (mode === "persist-failed") throw new Error("synthetic failure");
    };
    const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
    assert.equal(
      result.reason,
      mode === "cancel"
        ? "cancelled"
        : mode === "persist-failed"
          ? "evidence_persist_failed"
          : "capture_unverified",
    );
    assert.equal(record.phase, "source");
    assert.equal(record.completion, undefined);
    assert.ok(record.source_json.includes("Synthetic partial answer"));
    assert.ok(!record.source_json.includes("Private synthetic thought"));
    assert.ok(!record.source_json.includes("private-synthetic-token"));
    assert.ok(!record.source_json.includes("private-synthetic-metadata"));
    assert.ok(!record.source_json.includes("private-synthetic-credentials"));
    assert.ok(!record.source_json.includes("private-synthetic-unknown"));
    assert.ok(!record.source_json.includes("private-synthetic-shaped"));
    assert.ok(!record.source_json.includes("private-synthetic-event-metadata"));
    assert.ok(
      JSON.parse(record.source_json).source_redaction.omitted_paths.includes(
        "/messages/0/data/future_metadata",
      ),
    );
    assert.deepEqual(
      JSON.parse(record.source_json).messages[1].data.v.fragments.slice(2),
      [{}, { type: "RESPONSE" }],
    );
    assert.ok(
      JSON.parse(record.source_json).source_redaction.omitted_paths.includes(
        "/messages/1/data/v/future_metadata",
      ),
    );
    assert.equal(JSON.parse(record.source_json).ownership, undefined);
    assert.equal(f.calls.filter((call) => call === "send").length, 1);
    assert.ok(!f.calls.includes("extract"));
    assert.ok(!f.calls.includes("ownership"));
    assert.ok(!f.calls.includes("history"));
  }
});

test("full capture persists omitted fragment fields before extraction without shifting known evidence", async () => {
  const f = historyFixture();
  f.exchange.messages[0].data.future_metadata = {
    nested: { content: "private-synthetic-event-metadata" },
  };
  const fragments = f.exchange.messages[1].data.v.fragments;
  fragments.push(
    { type: "FUTURE", content: "private-synthetic-unknown" },
    {
      type: "RESPONSE",
      content: { type: "THINK", content: "private-synthetic-shaped" },
    },
  );
  delete f.options.prepareSource;
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.raw_answer, "Synthetic answer.");
  const saved = JSON.parse(f.evidence.source_json);
  assert.deepEqual(saved.messages[1].data.v.fragments.slice(3), [
    {},
    { type: "RESPONSE" },
  ]);
  assert.doesNotMatch(f.evidence.source_json, /private-synthetic/u);
  assert.ok(
    saved.source_redaction.omitted_paths.includes(
      "/messages/0/data/future_metadata",
    ),
  );
  assert.ok(
    saved.source_redaction.omitted_paths.includes(
      "/messages/1/data/v/fragments/3/type",
    ),
  );
  assert.ok(
    saved.source_redaction.omitted_paths.includes(
      "/messages/1/data/v/fragments/4/content",
    ),
  );
  assert.ok(f.calls.indexOf("source") < f.calls.indexOf("extract"));
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("hanging partial checkpoints report persistence failure across cancellation and source deadlines", async () => {
  for (const cancel of [true, false]) {
    const f = historyFixture();
    const controller = new AbortController();
    f.options.timeoutMs = 80;
    f.options.signal = controller.signal;
    f.options.onEvidence = () => new Promise(() => {});
    f.options.capture = async (_page, limits) => {
      await limits.submit(f.page, limits.signal);
      if (cancel) controller.abort();
      else
        await new Promise((resolve) =>
          limits.signal.addEventListener("abort", resolve, { once: true }),
        );
      await limits.onPartialCapture({
        messages: [
          {
            event: "ready",
            data: { request_message_id: 1, response_message_id: 2 },
          },
        ],
        capture_status: {
          schema_version: "geo.observation.capture-status.v1",
          transport: "partial",
          termination: cancel ? "aborted" : "timeout",
        },
      });
      return null;
    };
    const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
    assert.equal(result.reason, "evidence_persist_failed");
    assert.equal(f.calls.filter((call) => call === "send").length, 1);
    assert.ok(!f.calls.includes("extract"));
  }
});

test("capture failure forwards its fixed diagnostic without persistence or resubmission", async () => {
  const f = fixture();
  const diagnostics = [];
  const diagnostic = {
    kind: "observation_diagnostic",
    schema_version: "geo.observation.diagnostic.v1",
    stage: "capture_creation",
    code: "capture_unverified",
  };
  const result = await observeDeepSeekWebSearch(f.page, payload, {
    ...f.options,
    onDiagnostic: (entry) => diagnostics.push(entry),
    capture: async (_page, options) => {
      await options.submit(f.page, options.signal);
      options.onDiagnostic(diagnostic);
      return null;
    },
  });
  assert.equal(result.reason, "capture_unverified");
  assert.deepEqual(diagnostics, [diagnostic]);
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
  assert.equal(f.calls.includes("ownership"), false);
  assert.equal(f.calls.includes("source"), false);
  assert.equal(f.calls.includes("extract"), false);
});

test("fresh first-turn history is bound to transport IDs and retained in durable source and hash", async () => {
  const f = historyFixture();
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.raw_answer, "Synthetic answer.");
  const saved = JSON.parse(f.evidence.source_json);
  const lifecycle = f.receipt.captured.lifecycle;
  assert.equal(lifecycle.creation.chat_session_id, "fixture-selected");
  assert.deepEqual(lifecycle.ready, {
    request_message_id: 0,
    response_message_id: 2,
  });
  assert.equal(
    lifecycle.submission.prompt_sha256,
    createHash("sha256").update(payload.question).digest("hex"),
  );
  assert.deepEqual(saved.lifecycle, lifecycle);
  assert.deepEqual(saved.ownership.creation, lifecycle.creation);
  assert.deepEqual(saved.ownership.submission, lifecycle.submission);
  assert.deepEqual(
    capturedConversationCompletion(f.receipt.captured),
    capturedConversationCompletion(saved),
  );
  assert.deepEqual(capturedConversationCompletion(saved), {
    protocol: "deepseek_history_json",
    terminal: true,
    assistant_message_ids: ["2"],
  });
  assert.equal(
    f.evidence.source_sha256,
    createHash("sha256").update(f.evidence.source_json).digest("hex"),
  );
  assert.equal(result.extraction_audit.source_sha256, f.evidence.source_sha256);
  assert.equal(
    f.evidence.source_json.includes("Synthetic private reasoning"),
    false,
  );
  assert.ok(f.calls.indexOf("history") < f.calls.indexOf("ownership"));
  assert.ok(f.calls.indexOf("source") < f.calls.indexOf("extract"));
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("unavailable, failed or bounded-out history preserves sanitized evidence without a second submission", async () => {
  for (const scenario of ["unavailable", "failure", "timeout"]) {
    const f = historyFixture();
    let late;
    let historySignal;
    f.options.readHistory = async (_page, limits) => {
      f.calls.push("history");
      historySignal = limits.signal;
      assert.ok(limits.timeoutMs > 0 && limits.timeoutMs <= 100, scenario);
      if (scenario === "failure") throw new Error("synthetic private error");
      if (scenario === "timeout")
        return new Promise((resolve) => {
          late = resolve;
        });
      return null;
    };
    const result = await observeDeepSeekWebSearch(f.page, payload, {
      ...f.options,
      deadlineAt: performance.now() + 400,
    });
    assert.equal(result.raw_answer, "Synthetic answer.", scenario);
    assert.ok(historySignal.aborted, scenario);
    assert.equal(f.receipt.captured.lifecycle, undefined, scenario);
    const saved = JSON.parse(f.evidence.source_json);
    assert.equal(
      capturedNewConversationId(saved),
      "fixture-selected",
      scenario,
    );
    assert.equal(capturedConversationCompletion(saved), null, scenario);
    assert.equal(f.evidence.completion, undefined, scenario);
    assert.equal(
      saved.ownership.submission.prompt_sha256,
      createHash("sha256").update(payload.question).digest("hex"),
      scenario,
    );
    assert.equal(
      f.evidence.source_sha256,
      createHash("sha256").update(f.evidence.source_json).digest("hex"),
      scenario,
    );
    assert.equal(
      JSON.parse(f.evidence.source_json).lifecycle,
      undefined,
      scenario,
    );
    assert.equal(
      f.evidence.source_json.includes("Synthetic private reasoning"),
      false,
      scenario,
    );
    assert.equal(f.calls.filter((call) => call === "send").length, 1, scenario);
    if (late) {
      late({ history: f.history });
      await new Promise(setImmediate);
      assert.equal(f.receipt.captured.lifecycle, undefined);
      assert.equal(f.calls.filter((call) => call === "ownership").length, 1);
    }
  }
});

test("history respects the source deadline while leaving the overall persistence budget available", async () => {
  const f = historyFixture();
  const sourceDeadlineAt = performance.now() + 40;
  let historySignal;
  f.options.readHistory = (_page, limits) => {
    historySignal = limits.signal;
    assert.ok(limits.timeoutMs <= sourceDeadlineAt - performance.now() + 2);
    return new Promise(() => {});
  };
  const result = await observeDeepSeekWebSearch(f.page, payload, {
    ...f.options,
    sourceDeadlineAt,
    deadlineAt: performance.now() + 2_000,
  });
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.ok(historySignal.aborted);
  assert.equal(f.receipt.captured.lifecycle, undefined);
  assert.ok(f.evidence.source_json);
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("invalid transport, mismatched history and expired creation proof cannot attach lifecycle", async () => {
  for (const scenario of [
    "duplicate-ready",
    "wrong-model",
    "no-eof",
    "history-id",
    "history-private-field",
    "navigation-before",
    "navigation-after",
  ]) {
    const f = historyFixture();
    if (scenario === "duplicate-ready")
      f.exchange.messages.unshift(f.exchange.messages[0]);
    if (scenario === "wrong-model")
      f.exchange.messages[0].data.model_type = "another-model";
    if (scenario === "no-eof") f.exchange.sse_terminal = false;
    if (scenario === "history-id") f.history.messages[1].message_id = 3;
    if (scenario === "history-private-field")
      f.history.private_field = "synthetic private metadata";
    if (scenario.startsWith("navigation-")) {
      let current = scenario !== "navigation-before";
      const observe = f.options.observeCreatedSessions;
      f.options.observeCreatedSessions = (...args) => ({
        ...observe(...args),
        isCurrent: () => current,
      });
      const readHistory = f.options.readHistory;
      f.options.readHistory = async (...args) => {
        const result = await readHistory(...args);
        current = false;
        return result;
      };
    }
    const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
    assert.equal(result.raw_answer, "Synthetic answer.", scenario);
    assert.equal(f.receipt.captured.lifecycle, undefined, scenario);
    assert.equal(
      JSON.parse(f.evidence.source_json).lifecycle,
      undefined,
      scenario,
    );
    assert.equal(f.calls.filter((call) => call === "send").length, 1, scenario);
    if (
      [
        "duplicate-ready",
        "wrong-model",
        "no-eof",
        "navigation-before",
      ].includes(scenario)
    )
      assert.equal(f.calls.includes("history"), false, scenario);
  }
});

test("arms both observers before fresh navigation, persists before shared AI extraction, disposes only observers", async () => {
  const f = fixture();
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.equal(result.ownership_receipt_status, "persisted");
  assert.deepEqual(f.calls, [
    "arm-config",
    "arm-create",
    "goto",
    "configure",
    "capture",
    "send",
    "ownership",
    "source",
    "extract",
    "candidate",
    "dispose-config",
    "dispose-create",
  ]);
  assert.equal(result.search_event.request_model, payload.model);
  assert.equal(result.extraction_audit.method, "llm_grounded");
  assert.equal(result.source_redaction.omitted_paths.length, 0);
});

test("unavailable ownership or evidence integration is rejected before navigation or sending", async () => {
  for (const [key, reason] of [
    ["persistConversation", "ownership_unavailable"],
    ["onEvidence", "evidence_persistence_unavailable"],
  ]) {
    const f = fixture();
    f.options[key] = undefined;
    assert.deepEqual(
      await observeDeepSeekWebSearch(f.page, payload, f.options),
      { reason },
    );
    assert.deepEqual(f.calls, []);
  }
});

test("missing identity, model or control verification cannot send", async () => {
  for (const scenario of ["identity", "model", "controls", "root"]) {
    const f = fixture();
    if (scenario === "identity") f.options.identify = async () => null;
    if (scenario === "model")
      f.options.observeConfiguration = () => ({
        result: Promise.resolve({ model_configs: [] }),
        dispose() {},
      });
    if (scenario === "controls") f.options.configure = async () => false;
    if (scenario === "root") f.page.url = () => `${origin}/a/chat/s/existing`;
    const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
    assert.ok(result.reason, scenario);
    assert.equal(f.calls.includes("send"), false, scenario);
    assert.ok(f.calls.includes("dispose-create"), scenario);
  }
});

test("failed ownership checkpoint retains source and never invents cleanup authority", async () => {
  const f = fixture();
  f.options.persistConversation = async () => {
    throw new Error("synthetic private ledger details");
  };
  f.options.onConversationCaptured = () =>
    assert.fail("legacy registry must not be called");
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.equal(result.ownership_receipt_status, "unpersisted");
  assert.ok(f.calls.indexOf("source") < f.calls.indexOf("extract"));
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
  assert.equal(
    JSON.stringify(result).includes("synthetic private ledger details"),
    false,
  );
  assert.equal(result.completion, undefined);
});

test("source persistence rejection prevents extraction and never resends", async () => {
  const f = fixture();
  f.options.onEvidence = async () => {
    throw new Error("synthetic persistence failure");
  };
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.reason, "evidence_persist_failed");
  assert.equal(f.calls.includes("extract"), false);
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("source checkpoint must resolve before extraction, and unsafe source cannot be persisted", async () => {
  const f = fixture();
  let release;
  let sourceStarted;
  const started = new Promise((resolve) => {
    sourceStarted = resolve;
  });
  f.options.onEvidence = async (record) => {
    f.calls.push(record.phase);
    if (record.phase === "source") {
      sourceStarted();
      await new Promise((resolve) => {
        release = resolve;
      });
    }
  };
  const running = observeDeepSeekWebSearch(f.page, payload, f.options);
  await started;
  assert.equal(f.calls.includes("extract"), false);
  release();
  assert.equal((await running).raw_answer, "Synthetic answer.");

  const unsafe = fixture();
  unsafe.options.prepareSource = () => null;
  const result = await observeDeepSeekWebSearch(
    unsafe.page,
    payload,
    unsafe.options,
  );
  assert.equal(result.reason, "source_privacy_unverified");
  assert.equal(unsafe.calls.includes("source"), false);
  assert.equal(unsafe.calls.includes("ownership"), false);
  assert.equal(unsafe.calls.includes("extract"), false);
  assert.equal(unsafe.calls.filter((call) => call === "send").length, 1);
});

test("default projection reports fixed capture diagnostics for full and partial sources", async () => {
  for (const partial of [false, true]) {
    for (const throws of [false, true]) {
      const f = fixture();
      const diagnostics = [];
      delete f.options.prepareSource;
      f.options.onDiagnostic = (record) => {
        diagnostics.push(record);
        if (throws) throw new Error("synthetic-private-callback");
      };
      const unsafe = {
        ...source,
        messages: [
          {
            event: "message",
            data: {
              p: "response",
              v: "synthetic-private-value",
            },
          },
        ],
      };
      f.options.capture = async (_page, options) => {
        await options.submit(f.page, options.signal);
        if (partial) await options.onPartialCapture(unsafe);
        return unsafe;
      };
      const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
      assert.equal(result.reason, "source_privacy_unverified");
      assert.deepEqual(diagnostics[0], {
        kind: "observation_diagnostic",
        schema_version: "geo.observation.diagnostic.v1",
        stage: "capture",
        code: "source_response_rejected",
      });
      assert.equal(
        JSON.stringify(diagnostics).includes("synthetic-private"),
        false,
      );
      assert.equal(f.calls.includes("source"), false);
      assert.equal(f.calls.includes("ownership"), false);
      assert.equal(f.calls.includes("extract"), false);
      assert.equal(f.calls.filter((call) => call === "send").length, 1);
    }
  }
});

test("injected source preparation retains its one-argument API without default diagnostics", async () => {
  const f = fixture();
  const original = f.options.prepareSource;
  const diagnostics = [];
  let prepared = 0;
  f.options.prepareSource = (...args) => {
    assert.deepEqual(args, [source]);
    prepared++;
    return original(...args);
  };
  f.options.onDiagnostic = (record) => diagnostics.push(record);
  const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.equal(prepared, 1);
  assert.equal(
    diagnostics.some((record) => record.stage === "capture"),
    false,
  );
});

test("a hanging ownership hook leaves time for source persistence", async () => {
  const f = fixture();
  f.options.persistConversation = () => new Promise(() => {});
  const result = await observeDeepSeekWebSearch(f.page, payload, {
    ...f.options,
    deadlineAt: performance.now() + 200,
  });
  assert.equal(result.raw_answer, "Synthetic answer.");
  assert.equal(result.ownership_receipt_status, "unpersisted");
  assert.ok(f.calls.includes("source"));
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("unknown capture, thrown send and repeated submit callbacks never resend", async () => {
  for (const scenario of ["capture", "send", "duplicate"]) {
    const f = fixture();
    f.options.capture = async (_page, options) => {
      await options.submit(f.page, options.signal);
      if (scenario === "duplicate")
        await options.submit(f.page, options.signal);
      return null;
    };
    if (scenario === "send")
      f.options.submitQuestion = async () => {
        f.calls.push("send");
        throw new Error("synthetic send failed after dispatch");
      };
    const result = await observeDeepSeekWebSearch(f.page, payload, f.options);
    assert.ok(result.reason, scenario);
    assert.equal(f.calls.filter((call) => call === "send").length, 1, scenario);
    assert.equal(f.calls.includes("extract"), false, scenario);
  }
});

test("source deadline bounds navigation, identity, configuration and capture with matching observer TTL", async () => {
  for (const stage of ["goto", "identity", "configuration", "capture"]) {
    const f = fixture();
    const expires = performance.now() + 35;
    const observedTtls = [];
    for (const method of ["observeConfiguration", "observeCreatedSessions"]) {
      const original = f.options[method];
      f.options[method] = (page, limits) => {
        observedTtls.push(limits.timeoutMs);
        assert.ok(limits.timeoutMs <= expires - performance.now() + 2);
        return original(page, limits);
      };
    }
    const hang = () => new Promise(() => {});
    if (stage === "goto") f.page.goto = hang;
    if (stage === "identity") f.options.identify = hang;
    if (stage === "configuration") f.options.configure = hang;
    if (stage === "capture") f.options.capture = hang;
    const result = await observeDeepSeekWebSearch(f.page, payload, {
      ...f.options,
      sourceDeadlineAt: expires,
      deadlineAt: performance.now() + 5000,
    });
    assert.equal(result.reason, "budget_exhausted", stage);
    assert.equal(observedTtls[0], observedTtls[1], stage);
    assert.equal(f.calls.includes("send"), false, stage);
    assert.ok(f.calls.includes("dispose-create"), stage);
  }
});

test("cancellation and expired budgets prevent all sends", async () => {
  const f = fixture();
  const controller = new AbortController();
  controller.abort();
  assert.equal(
    (
      await observeDeepSeekWebSearch(f.page, payload, {
        ...f.options,
        signal: controller.signal,
      })
    ).reason,
    "cancelled",
  );
  assert.equal(
    (
      await observeDeepSeekWebSearch(f.page, payload, {
        ...f.options,
        deadlineAt: performance.now() - 1,
      })
    ).reason,
    "budget_exhausted",
  );
  assert.deepEqual(f.calls, []);
});

test("hanging durable source hook is bounded and cannot start late extraction", async () => {
  const f = fixture();
  let release;
  f.options.onEvidence = () =>
    new Promise((resolve) => {
      release = resolve;
    });
  const result = await observeDeepSeekWebSearch(f.page, payload, {
    ...f.options,
    deadlineAt: performance.now() + 40,
  });
  assert.equal(result.reason, "budget_exhausted");
  release();
  await new Promise(setImmediate);
  assert.equal(f.calls.includes("extract"), false);
  assert.equal(f.calls.filter((call) => call === "send").length, 1);
});

test("real Chrome runs fresh navigation, dynamic controls, exact prefetched session, capture and source-first AI parsing", async () => {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  const calls = [];
  let sends = 0;
  let creates = 0;
  let historyReads = 0;
  let savedSource;
  let savedSourceHash;
  const event = (name, data) =>
    `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`;
  const wire =
    event("ready", {
      request_message_id: 1,
      response_message_id: 2,
      model_type: payload.model,
    }) +
    event("message", {
      p: "response",
      o: "SET",
      v: {
        fragments: [
          { type: "RESPONSE", content: "Synthetic answer." },
          {
            type: "SEARCH",
            results: [
              { url: "https://example.org/source", title: "Synthetic source" },
            ],
            queries: ["synthetic rainfall"],
          },
          {
            type: "THINK",
            content: "Synthetic private reasoning must not persist.",
          },
        ],
        status: "FINISHED",
        future_metadata: {
          content: "Synthetic private metadata must not persist.",
          nested: {
            type: "THINK",
            content: "Synthetic private nested metadata",
          },
        },
      },
    }) +
    "event: finish\ndata: [DONE]\n\n";
  try {
    const context = await browser.newContext();
    await context.route("**/*", async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      assert.equal(url.origin, origin);
      if (url.pathname === "/") {
        await route.fulfill({
          contentType: "text/html",
          body: `<textarea name="search"></textarea>
            <div role="radio" data-model-type="${payload.model}" aria-checked="true">Fixture model</div>
            <div class="ds-toggle-button" aria-pressed="false">Search</div>
            <div class="ds-toggle-button" aria-pressed="true">DeepThink</div>
            <script>
            localStorage.setItem("userToken",JSON.stringify({__version:"0",value:"synthetic-token"}));
            const prefetched = Promise.all([0,1].map(() =>
              fetch("/api/v0/chat_session/create",{method:"POST",body:"{}"}).then(r=>r.json())));
            fetch("/api/v0/client/settings?scope=model");
            document.querySelectorAll(".ds-toggle-button").forEach(node => node.onclick = () =>
              node.setAttribute("aria-pressed",String(node.getAttribute("aria-pressed") !== "true")));
            document.querySelector("textarea").onkeydown = async event => {
              if(event.key !== "Enter") return;
              event.preventDefault();
              const session = (await prefetched)[0].data.biz_data.chat_session.id;
              history.pushState({},"","/a/chat/s/"+session);
              fetch("/api/v0/chat/completion",{
                method:"POST",headers:{"content-type":"application/json"},
                body:JSON.stringify({
                  chat_session_id:session,parent_message_id:null,model_type:"${payload.model}",
                  prompt:document.querySelector("textarea").value,ref_file_ids:[],
                  search_enabled:document.querySelectorAll(".ds-toggle-button")[0].getAttribute("aria-pressed") === "true",
                  thinking_enabled:document.querySelectorAll(".ds-toggle-button")[1].getAttribute("aria-pressed") === "true"
                })
              }).then(r=>r.text());
            };
            </script>`,
        });
      } else if (url.pathname === "/api/v0/users/current") {
        await route.fulfill({
          json: {
            code: 0,
            data: { biz_code: 0, biz_data: { id: "fixture-account" } },
          },
        });
      } else if (url.pathname === "/api/v0/client/settings") {
        await route.fulfill({
          json: {
            code: 0,
            data: {
              biz_code: 0,
              biz_data: {
                version: 1,
                settings: { model_configs: { id: 1, value: modelConfigs } },
              },
            },
          },
        });
      } else if (url.pathname === "/api/v0/chat_session/create") {
        creates++;
        await route.fulfill({
          json: {
            code: 0,
            data: {
              biz_code: 0,
              biz_data: {
                chat_session: {
                  id: creates === 1 ? "fixture-selected" : "fixture-unselected",
                },
              },
            },
          },
        });
      } else if (url.pathname === "/api/v0/chat/completion") {
        sends++;
        assert.equal(
          request.postDataJSON().chat_session_id,
          "fixture-selected",
        );
        await route.fulfill({
          contentType: "text/event-stream; charset=utf-8",
          body: wire,
        });
      } else if (url.pathname === "/api/v0/chat/history_messages") {
        historyReads++;
        assert.equal(request.method(), "GET");
        assert.equal(
          url.searchParams.get("chat_session_id"),
          "fixture-selected",
        );
        assert.equal(url.searchParams.has("cache_version"), false);
        assert.equal(url.searchParams.has("cache_reset_at"), false);
        await route.fulfill({
          json: {
            code: 0,
            data: {
              biz_code: 0,
              biz_data: {
                cache_control: "REPLACE",
                chat_session: {
                  id: "fixture-selected",
                  current_message_id: 2,
                },
                chat_messages: [
                  {
                    message_id: 1,
                    parent_id: null,
                    role: "USER",
                    status: "FINISHED",
                    has_pending_fragment: false,
                    fragments: [],
                  },
                  {
                    message_id: 2,
                    parent_id: 1,
                    role: "ASSISTANT",
                    status: "FINISHED",
                    has_pending_fragment: false,
                    fragments: [
                      {
                        type: "THINK",
                        content: "Synthetic private history reasoning",
                      },
                    ],
                  },
                ],
              },
            },
          },
        });
      } else await route.abort();
    });
    const page = await context.newPage();
    const result = await observeDeepSeekWebSearch(page, payload, {
      expectedAccountId: "fixture-account",
      deadlineAt: performance.now() + 10_000,
      sourceDeadlineAt: performance.now() + 8_000,
      persistConversation: async ({
        createdSession,
        captured,
        binding,
        expectedAccountId,
      }) => {
        calls.push("ownership");
        assert.equal(createdSession.chatSessionId, "fixture-selected");
        assert.equal(createdSession.source, "browser_observed_create");
        assert.equal(expectedAccountId, "fixture-account");
        assert.equal(
          JSON.stringify(captured).includes("Synthetic private reasoning"),
          false,
        );
        assert.equal(binding.model, payload.model);
        assert.equal(binding.searchEnabled, true);
        assert.equal(binding.thinkingEnabled, false);
        assert.match(binding.questionSha256, /^[a-f0-9]{64}$/u);
        assert.equal(
          captured.lifecycle.creation.chat_session_id,
          "fixture-selected",
        );
        assert.deepEqual(capturedConversationCompletion(captured), {
          protocol: "deepseek_history_json",
          terminal: true,
          assistant_message_ids: ["2"],
        });
        return { durable: true };
      },
      onEvidence: async (record) => {
        calls.push(record.phase);
        if (record.phase === "source") {
          savedSource = JSON.parse(record.source_json);
          savedSourceHash = record.source_sha256;
          assert.equal(
            savedSourceHash,
            createHash("sha256").update(record.source_json).digest("hex"),
          );
        }
      },
      apiExtract: async (prompt) => {
        calls.push("extract");
        assert.ok(savedSource, "source checkpoint completed before extraction");
        assert.ok(
          prompt.includes(
            `/source_redaction = ${JSON.stringify(savedSource.source_redaction)}`,
          ),
          "extraction receives the persisted privacy metadata",
        );
        for (const [index, message] of savedSource.messages.entries())
          assert.ok(
            prompt.includes(`/messages/${index} = ${JSON.stringify(message)}`),
          );
        assert.equal(prompt.includes("Synthetic private reasoning"), false);
        assert.equal(
          savedSource.messages[1].data.v.fragments[0].content,
          "Synthetic answer.",
        );
        return {
          model: "fixture-parser",
          surface: "model_api",
          extracted: {
            completion: "complete",
            completion_evidence: [{ path: "/messages/1/data/v/status" }],
            search_used: "yes",
            search_evidence: [
              { path: "/messages/1/data/v/fragments/1/results" },
            ],
            answer_segments: [
              { path: "/messages/1/data/v/fragments/0/content" },
            ],
            citations: [],
          },
        };
      },
    });
    assert.equal(result.raw_answer, "Synthetic answer.");
    assert.equal(result.ownership_receipt_status, "persisted");
    assert.deepEqual(calls, ["ownership", "source", "extract", "candidate"]);
    assert.equal(sends, 1);
    assert.equal(creates, 2);
    assert.equal(historyReads, 1);
    assert.equal(page.isClosed(), false);
    assert.equal(page.listenerCount("response"), 0);
    assert.equal(page.listenerCount("request"), 0);
    assert.equal(page.listenerCount("framenavigated"), 0);
    assert.equal(
      JSON.stringify(savedSource).includes("synthetic-token"),
      false,
    );
    assert.equal(
      JSON.stringify(savedSource).includes("Synthetic private reasoning"),
      false,
    );
    assert.equal(
      JSON.stringify(savedSource).includes("Synthetic private metadata"),
      false,
    );
    assert.equal(
      JSON.stringify(savedSource).includes("Synthetic private nested metadata"),
      false,
    );
    assert.ok(
      result.source_redaction.omitted_paths.includes(
        "/messages/1/data/v/future_metadata",
      ),
    );
    assert.ok(result.source_redaction.omitted_paths.length > 0);
    assert.deepEqual(savedSource.source_redaction, result.source_redaction);
    assert.equal(result.extraction_audit.source_sha256, savedSourceHash);
    assert.equal(capturedNewConversationId(savedSource), "fixture-selected");
    assert.deepEqual(
      savedSource.ownership.creation,
      savedSource.lifecycle.creation,
    );
    assert.deepEqual(
      savedSource.ownership.submission,
      savedSource.lifecycle.submission,
    );
    assert.equal(savedSource.lifecycle.history.messages.length, 2);
    assert.equal(
      JSON.stringify(savedSource).includes(
        "Synthetic private history reasoning",
      ),
      false,
    );
    await context.close();
  } finally {
    await browser.close();
  }
});
