import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { validateAiObservation } from "../src/ai-observation-grounding.mjs";
import {
  configuredExtractionModel,
  extractionPrompt,
  extractionCaptureRecord,
  interpretObservation,
  invokeExtractionApi,
  observationDocument,
  parseExtractionJson,
  reportObservationDiagnostic,
} from "../src/ai-observation-parser.mjs";

// Synthetic transport and extraction results only; no external requests.
const exchange = {
  model: "observed-model",
  surface: "consumer_web",
  messages: [
    { conversation: "chat-synthetic" },
    { id: "answer-synthetic", state: "completed", model: "observed-model" },
    { id: "search-synthetic", owner: "answer-synthetic", result: { count: 1 } },
    { owner: "answer-synthetic", text: "The source-backed answer." },
    { url: "https://example.org/article", reference: "answer-reference-1" },
  ],
};
const extracted = {
  completion: "complete",
  completion_evidence: [{ path: "/messages/1/state" }],
  search_used: "yes",
  search_evidence: [{ path: "/messages/2/result" }],
  answer_segments: [{ path: "/messages/3/text" }],
  citations: [
    {
      url: { path: "/messages/4/url" },
      usage: { path: "/messages/4/reference" },
    },
  ],
};
const unverified = { ...extracted, search_used: "unknown" };
const config = {
  base: "https://example.org/v1",
  key: "synthetic-test-key",
  model: "extractor-exact-version",
};
const extraction = (overrides = {}) => ({
  extracted,
  model: "extraction-model",
  surface: "consumer_web",
  ...overrides,
});
const envelope = (overrides = {}) => ({
  model: "upstream-alias",
  choices: [
    {
      finish_reason: "stop",
      message: { content: JSON.stringify(extracted) },
      ...overrides,
    },
  ],
  usage: { prompt_tokens: 10, completion_tokens: 20 },
});
const api = (fetchImpl, options = {}) =>
  invokeExtractionApi("Synthetic extraction prompt", {
    config,
    fetchImpl,
    ...options,
  });

test("saved project preference selects API only after raw persistence", async () => {
  const calls = [];
  const result = await interpretObservation(exchange, {
    onEvidence: async (record) => calls.push(record.phase),
    getExtractionPolicy: async () => {
      calls.push("policy");
      return { prefer_connected_account: false, config_version: 3 };
    },
    browserExtract: async () => {
      throw new Error("browser extraction must be skipped");
    },
    apiExtract: async () => {
      calls.push("api");
      return extraction({
        surface: "model_api",
        model: "saved-project-model",
        config_version: 4,
      });
    },
  });
  assert.deepEqual(calls, ["source", "policy", "api", "candidate"]);
  assert.equal(result.audit.model, "saved-project-model");
  assert.equal(result.audit.config_version, 4);
  assert.deepEqual(result.audit.attempts, [
    { route: "configured_model_api", status: "grounded" },
  ]);
});

test("connected-account preference keeps browser first and configured fallback", async () => {
  const calls = [];
  const result = await interpretObservation(exchange, {
    getExtractionPolicy: async () => ({
      prefer_connected_account: true,
      config_version: 2,
    }),
    browserExtract: async () => {
      calls.push("browser");
      return null;
    },
    apiExtract: async () => {
      calls.push("api");
      return extraction({ config_version: 2 });
    },
  });
  assert.deepEqual(calls, ["browser", "api"]);
  assert.equal(result.audit.config_version, 2);
});

test("unavailable project policy fails closed without legacy or browser extraction", async () => {
  let called = false;
  assert.equal(
    await interpretObservation(exchange, {
      allowLegacyApi: true,
      getExtractionPolicy: async () => {
        throw new Error("private upstream failure");
      },
      browserExtract: async () => {
        called = true;
        return extraction();
      },
      apiExtract: async () => {
        called = true;
        return extraction();
      },
    }),
    null,
  );
  assert.equal(called, false);
});

test("source and extraction checkpoints share lifecycle proof outside source JSON", async () => {
  const source = {
    connect_json_terminal: true,
    messages: [
      { chat: { id: "synthetic-chat" } },
      {
        message: {
          id: "synthetic-answer",
          chat_id: "synthetic-chat",
          role: "assistant",
          status: "COMPLETED",
        },
      },
    ],
  };
  for (const terminal of [true, false, undefined]) {
    source.connect_json_terminal = terminal;
    const records = [];
    await interpretObservation(source, {
      onEvidence: async (record) => records.push(record),
      apiExtract: async () => null,
    });
    const extraction = extractionCaptureRecord(source);
    assert.equal(records.length, 1);
    assert.deepEqual(records[0].completion, extraction.completion);
    assert.equal(Object.hasOwn(extraction, "completion"), terminal === true);
    if (terminal === true)
      assert.deepEqual(extraction.completion, {
        protocol: "connect_json",
        terminal: true,
        assistant_message_ids: ["synthetic-answer"],
      });
    assert.equal(records[0].source_json, extraction.source_json);
    assert.deepEqual(JSON.parse(extraction.source_json), {
      messages: source.messages,
    });
  }
});

test("model completion claims cannot upgrade raw lifecycle proof", async () => {
  const records = [];
  await interpretObservation(exchange, {
    onEvidence: async (record) => records.push(record),
    apiExtract: async () =>
      extraction({
        completion: {
          protocol: "connect_json",
          terminal: true,
          assistant_message_ids: ["invented"],
        },
      }),
  });
  assert.ok(records.length > 0);
  assert.ok(records.every((record) => !Object.hasOwn(record, "completion")));
});

test("specific grounding diagnostics persist and emit without source values", async () => {
  const records = [];
  const diagnostics = [];
  const source = structuredClone(exchange);
  source.messages[1].state = "";
  assert.equal(
    await interpretObservation(source, {
      onEvidence: async (record) => records.push(record),
      onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
      apiExtract: async () => extraction(),
    }),
    null,
  );
  assert.equal(records[1].grounding_reason, "evidence_empty");
  assert.equal(diagnostics[0].code, "evidence_empty");
  for (const code of [
    "identifier_rejected",
    "evidence_empty",
    "answer_type_rejected",
    "answer_bounds_rejected",
    "answer_empty",
    "answer_too_large",
    "unicode_rejected",
  ]) {
    const entries = [];
    reportObservationDiagnostic(
      (entry) => entries.push(entry),
      "extraction",
      code,
    );
    assert.equal(entries[0].code, code);
  }
  const count = diagnostics.length;
  reportObservationDiagnostic(
    (entry) => diagnostics.push(entry),
    "extraction",
    "private unrecognized diagnostic",
  );
  assert.equal(diagnostics.length, count);
});

test("non-search and incomplete semantic candidates retain their observed answer before rejection", async () => {
  for (const changes of [
    { search_used: "no" },
    { search_used: "unknown" },
    { completion: "incomplete" },
    { completion: "unknown" },
    { completion: "unknown", answer_segments: [] },
  ]) {
    const candidate = { ...extracted, ...changes };
    const records = [];
    const result = await interpretObservation(exchange, {
      onEvidence: async (record) => records.push(record),
      apiExtract: async () => extraction({ extracted: candidate }),
    });
    assert.equal(result, null);
    assert.deepEqual(
      records.map((record) => record.phase),
      ["source", "candidate"],
    );
    assert.deepEqual(JSON.parse(records[1].candidate_json), candidate);
    assert.equal(records[1].grounding_reason, "model_unverified");
    assert.equal(
      JSON.parse(records[0].source_json).messages[3].text,
      exchange.messages[3].text,
    );
  }
});

test("failed extraction emits only allowlisted route diagnostics and preserves null", async () => {
  const diagnostics = [];
  const result = await interpretObservation(exchange, {
    browserExtract: async () => null,
    apiExtract: async () => extraction({ extracted: unverified }),
    onDiagnostic: (entry) => diagnostics.push(entry),
  });
  assert.equal(result, null);
  assert.deepEqual(diagnostics, [
    {
      kind: "observation_diagnostic",
      schema_version: "geo.observation.diagnostic.v1",
      stage: "extraction",
      code: "returned_none",
      route: "signed_in_browser",
    },
    {
      kind: "observation_diagnostic",
      schema_version: "geo.observation.diagnostic.v1",
      stage: "extraction",
      code: "model_unverified",
      route: "configured_model_api",
    },
  ]);
});

test("exceptions and cancellation diagnostics never retain arbitrary details", async () => {
  for (const reason of [
    new Error("synthetic-private-error"),
    new DOMException("synthetic-private-error", "TimeoutError"),
  ]) {
    const controller = new AbortController();
    const diagnostics = [];
    let apiCalls = 0;
    assert.equal(
      await interpretObservation(exchange, {
        signal: controller.signal,
        browserExtract: async () => {
          controller.abort(reason);
          throw new Error("synthetic-private-error");
        },
        apiExtract: async () => {
          apiCalls += 1;
        },
        onDiagnostic: (entry) => diagnostics.push(entry),
      }),
      null,
    );
    assert.equal(apiCalls, 0);
    assert.equal(
      diagnostics.at(-1).code,
      reason.name === "TimeoutError" ? "budget_exhausted" : "cancelled",
    );
    assert.doesNotMatch(
      JSON.stringify(diagnostics),
      /synthetic-private|example\.org|chat-synthetic|source-backed/u,
    );
  }
  const diagnostics = [];
  const collect = (entry) => diagnostics.push(entry);
  reportObservationDiagnostic(collect, "synthetic-private", "returned_none");
  reportObservationDiagnostic(collect, "extraction", "synthetic-private");
  reportObservationDiagnostic(
    collect,
    "extraction",
    "returned_none",
    "synthetic-private",
  );
  assert.equal(diagnostics.length, 1);
  assert.equal(Object.hasOwn(diagnostics[0], "route"), false);
  assert.doesNotThrow(() =>
    reportObservationDiagnostic(
      () => {
        throw new Error("synthetic-private");
      },
      "extraction",
      "returned_none",
    ),
  );
});

test("document removes nested private fields and reasoning without mutating source or indexes", () => {
  const source = structuredClone(exchange);
  source.headers = { authorization: "synthetic-secret" };
  source.messages[0].nested = [
    {
      Authorization: "synthetic-secret",
      cookies: "synthetic-secret",
      password: "synthetic-secret",
      accessToken: "synthetic-secret",
      refresh_token: "synthetic-secret",
      sessionKey: "synthetic-secret",
      api_key: "synthetic-secret",
      email: "synthetic-secret",
      phone: "synthetic-secret",
      userId: "synthetic-secret",
      account_id: "synthetic-secret",
      think: "synthetic-reasoning",
      thinking: "synthetic-reasoning",
      reasoning: "synthetic-reasoning",
      reasoning_content: "synthetic-reasoning",
      public_value: "retained",
    },
  ];
  const before = structuredClone(source);
  const document = observationDocument(source, "Displayed answer.");
  assert.deepEqual(document.messages[0].nested, [{ public_value: "retained" }]);
  assert.equal(document.messages.length, exchange.messages.length);
  assert.equal(document.messages[3].text, exchange.messages[3].text);
  assert.equal(document.rendered_text, "Displayed answer.");
  assert.equal(Object.hasOwn(document, "headers"), false);
  assert.doesNotMatch(
    JSON.stringify(document),
    /synthetic-secret|synthetic-reasoning/u,
  );
  assert.deepEqual(source, before);
  assert.equal(
    Object.hasOwn(observationDocument(source), "rendered_text"),
    false,
  );
});

test("prompt has explicit source indexes, grounding schema and untrusted-data boundary", () => {
  const document = observationDocument(exchange, "Displayed answer.");
  const prompt = extractionPrompt(document);
  for (const [index, message] of document.messages.entries()) {
    assert.ok(
      prompt.includes(`/messages/${index} = ${JSON.stringify(message)}`),
    );
  }
  assert.ok(prompt.includes('/rendered_text = "Displayed answer."'));
  assert.match(prompt, /UNTRUSTED DATA/u);
  assert.match(prompt, /RFC6901/u);
  for (const field of Object.keys(extracted))
    assert.ok(prompt.includes(`"${field}"`));
  assert.match(prompt, /not instructions/u);
  assert.match(prompt, /Search results alone are NOT answer citations/u);
  assert.throws(
    () =>
      extractionPrompt({
        messages: [{ text: "界".repeat(250_001) }],
      }),
    /observation_input_too_large/u,
  );
});

test("parses bare and fenced JSON but rejects malformed, additional prose and oversized bytes", () => {
  for (const text of [
    JSON.stringify(extracted),
    ` \n${JSON.stringify(extracted)}\n `,
    `\`\`\`json\n${JSON.stringify(extracted)}\n\`\`\``,
    `\`\`\`\n${JSON.stringify(extracted)}\n\`\`\``,
  ])
    assert.deepEqual(parseExtractionJson(text), extracted);
  for (const text of [
    undefined,
    null,
    {},
    "{",
    `Summary: ${JSON.stringify(extracted)}`,
    `${JSON.stringify(extracted)} ${JSON.stringify(extracted)}`,
    JSON.stringify({ text: "界".repeat(50_001) }),
  ])
    assert.equal(parseExtractionJson(text), null);
});

test("extraction configuration requires all fields and rejects insecure or credential-bearing URLs", () => {
  const env = {
    GEO_OBSERVATION_AI_BASE_URL: `${config.base}///`,
    GEO_OBSERVATION_AI_API_KEY: config.key,
    GEO_OBSERVATION_AI_MODEL: config.model,
  };
  assert.deepEqual(configuredExtractionModel(env), config);
  for (const key of Object.keys(env)) {
    assert.equal(configuredExtractionModel({ ...env, [key]: "" }), null);
  }
  for (const base of [
    "not a URL",
    "http://example.org/v1",
    "ftp://example.org/v1",
    "https://user:pass@example.org/v1",
    "https://example.org/v1?key=value",
    "https://example.org/v1#fragment",
  ])
    assert.equal(
      configuredExtractionModel({
        ...env,
        GEO_OBSERVATION_AI_BASE_URL: base,
      }),
      null,
    );
  for (const base of ["http://localhost:8080/v1", "http://127.0.0.1:8080/v1"]) {
    assert.equal(
      configuredExtractionModel({
        ...env,
        GEO_OBSERVATION_AI_BASE_URL: base,
      }).base,
      base,
    );
  }
});

test("API uses exact configured model, header-only key, no redirects and supplied abort signal", async () => {
  const controller = new AbortController();
  const result = await api(
    async (url, options) => {
      assert.equal(url, "https://example.org/v1/chat/completions");
      assert.equal(options.method, "POST");
      assert.equal(options.redirect, "error");
      assert.equal(options.signal, controller.signal);
      assert.equal(options.headers.Authorization, `Bearer ${config.key}`);
      assert.equal(options.headers["Content-Type"], "application/json");
      assert.equal(url.includes(config.key), false);
      assert.equal(options.body.includes(config.key), false);
      assert.deepEqual(JSON.parse(options.body), {
        model: config.model,
        messages: [{ role: "user", content: "Synthetic extraction prompt" }],
        stream: false,
        response_format: { type: "json_object" },
      });
      return Response.json(envelope());
    },
    { signal: controller.signal },
  );
  assert.deepEqual(result, {
    extracted,
    model: config.model,
    surface: "model_api",
    usage: { prompt_tokens: 10, completion_tokens: 20 },
  });
});

test("API rejects unfinished choices, tool calls, missing content and malformed responses", async () => {
  for (const finish_reason of [
    "length",
    "tool_calls",
    "content_filter",
    null,
    undefined,
  ]) {
    assert.equal(
      await api(async () => Response.json(envelope({ finish_reason }))),
      null,
    );
  }
  for (const message of [
    {
      content: JSON.stringify(extracted),
      tool_calls: [{ id: "synthetic-tool" }],
    },
    { content: null },
    { content: "not JSON" },
    {},
  ])
    assert.equal(
      await api(async () => Response.json(envelope({ message }))),
      null,
    );
  for (const body of ["not JSON", "{}", '{"choices":[]}']) {
    assert.equal(await api(async () => new Response(body)), null);
  }
});

test("API bounds response stream, cancels oversized bodies and releases the reader", async () => {
  let canceled = false;
  let released = false;
  let reads = 0;
  const result = await api(async () => ({
    ok: true,
    body: {
      getReader: () => ({
        read: async () => {
          reads += 1;
          return { done: false, value: new Uint8Array(150_001) };
        },
        cancel: async () => {
          canceled = true;
        },
        releaseLock: () => {
          released = true;
        },
      }),
    },
  }));
  assert.equal(result, null);
  assert.equal(reads, 2);
  assert.equal(canceled, true);
  assert.equal(released, true);
  assert.equal(
    await api(async () =>
      Response.json(
        envelope({
          message: { content: JSON.stringify({ text: "界".repeat(50_001) }) },
        }),
      ),
    ),
    null,
  );
});

test("API fails closed without exposing upstream error or attempting canceled requests", async () => {
  let calls = 0;
  const controller = new AbortController();
  controller.abort();
  const neverFetch = async () => {
    calls += 1;
    throw new Error("unexpected request");
  };
  assert.equal(await api(neverFetch, { signal: controller.signal }), null);
  assert.equal(await api(neverFetch, { config: null }), null);
  assert.equal(calls, 0);
  assert.equal(
    await api(async () => {
      throw new Error(`synthetic upstream detail ${config.key}`);
    }),
    null,
  );
  assert.equal(
    await api(
      async () =>
        new Response(`synthetic error ${config.key}`, {
          status: 401,
        }),
    ),
    null,
  );
  assert.equal(await api(async () => ({ ok: true, body: null })), null);
  assert.equal(
    await api(async () => {
      throw new DOMException("Synthetic cancellation", "AbortError");
    }),
    null,
  );
});

test("browser extraction succeeds first without API and records a document-bound audit", async () => {
  const before = structuredClone(exchange);
  let apiCalls = 0;
  const controller = new AbortController();
  const result = await interpretObservation(exchange, {
    signal: controller.signal,
    browserExtract: async (prompt, options) => {
      assert.equal(options.signal.aborted, false);
      assert.ok(Number.isFinite(options.deadlineAt));
      assert.equal(prompt, extractionPrompt(observationDocument(exchange)));
      return extraction();
    },
    apiExtract: async () => {
      apiCalls += 1;
      return extraction();
    },
  });
  assert.equal(apiCalls, 0);
  assert.equal(result.raw_answer, exchange.messages[3].text);
  assert.deepEqual(result.citations, ["https://example.org/article"]);
  assert.equal(result.audit.model, "extraction-model");
  assert.equal(result.audit.surface, "consumer_web");
  assert.equal(result.audit.kind, "observation_extraction");
  assert.equal(result.audit.method, "llm_grounded");
  assert.equal(result.audit.prompt_version, "geo.observation.extract.v2");
  assert.deepEqual(result.audit.attempts, [
    {
      route: "signed_in_browser",
      status: "grounded",
    },
  ]);
  assert.equal(
    result.audit.source_sha256,
    createHash("sha256")
      .update(JSON.stringify(observationDocument(exchange)))
      .digest("hex"),
  );
  assert.deepEqual(exchange, before);
  assert.equal(Object.hasOwn(result, "model"), false);
  assert.equal(Object.hasOwn(result, "surface"), false);
});

test("bounded browser route preserves API fallback on the same durable source", async () => {
  let browserSignal;
  let apiCalls = 0;
  let lateEvidence;
  const records = [];
  const diagnostics = [];
  const result = await interpretObservation(exchange, {
    browserTimeoutMs: 15,
    apiTimeoutMs: 100,
    deadlineAt: performance.now() + 500,
    onEvidence: async (record) => records.push(record),
    onDiagnostic: (entry) => diagnostics.push(entry),
    browserExtract: async (prompt, hooks) => {
      browserSignal = hooks.signal;
      lateEvidence = hooks.onEvidence;
      assert.equal(prompt, extractionPrompt(observationDocument(exchange)));
      return new Promise(() => {});
    },
    apiExtract: async (prompt, hooks) => {
      apiCalls++;
      assert.equal(browserSignal.aborted, true);
      assert.equal(hooks.signal.aborted, false);
      assert.equal(records[0].phase, "source");
      assert.equal(prompt, extractionPrompt(observationDocument(exchange)));
      return extraction({ surface: "model_api" });
    },
  });
  assert.equal(apiCalls, 1);
  assert.equal(result.raw_answer, exchange.messages[3].text);
  assert.equal(diagnostics[0].code, "timeout");
  await assert.rejects(
    lateEvidence({ phase: "extraction" }),
    /evidence_persist_failed/u,
  );
  assert.deepEqual(
    records.map((record) => record.phase),
    ["source", "candidate"],
  );
});

test("unresolved durable browser evidence blocks fallback after route timeout", async () => {
  let release;
  let apiCalls = 0;
  const result = await interpretObservation(exchange, {
    browserTimeoutMs: 15,
    onEvidence: async (record) => {
      if (record.phase === "extraction")
        await new Promise((resolve) => (release = resolve));
    },
    browserExtract: async (_prompt, hooks) => {
      await hooks.onEvidence({ phase: "extraction" });
      return null;
    },
    apiExtract: async () => {
      apiCalls++;
      return extraction();
    },
  });
  assert.equal(result, null);
  assert.equal(apiCalls, 0);
  release();
});

test("policy and API routes have independent bounded cancellation", async () => {
  for (const blocked of ["policy", "api"]) {
    let childSignal;
    let calls = 0;
    const diagnostics = [];
    const result = await interpretObservation(exchange, {
      policyTimeoutMs: 15,
      apiTimeoutMs: 15,
      getExtractionPolicy: async ({ signal }) => {
        if (blocked === "policy") {
          childSignal = signal;
          return new Promise(() => {});
        }
        return { prefer_connected_account: false };
      },
      apiExtract: async (_prompt, { signal }) => {
        calls++;
        childSignal = signal;
        return new Promise(() => {});
      },
      onDiagnostic: (entry) => diagnostics.push(entry),
    });
    assert.equal(result, null);
    assert.equal(childSignal.aborted, true);
    assert.equal(calls, blocked === "policy" ? 0 : 1);
    assert.equal(
      diagnostics.at(-1).code,
      blocked === "policy" ? "budget_exhausted" : "timeout",
    );
  }
});

test("expired interpretation cannot begin either model route", async () => {
  let calls = 0;
  const invoke = async () => {
    calls++;
    return extraction();
  };
  assert.equal(
    await interpretObservation(exchange, {
      deadlineAt: performance.now() - 1,
      browserExtract: invoke,
      apiExtract: invoke,
    }),
    null,
  );
  assert.equal(calls, 0);
});

test("source is delivered before either extractor and rejected candidates remain bounded evidence", async () => {
  const records = [];
  const diagnostics = [];
  const invalid = {
    ...extracted,
    answer_segments: [{ path: "/messages/99/missing" }],
  };
  const source = structuredClone(exchange);
  source.messages[0].authorization = "synthetic-secret";
  const result = await interpretObservation(source, {
    onEvidence: async (record) => {
      records.push(record);
    },
    onDiagnostic: (entry) => diagnostics.push(entry),
    browserExtract: async () => {
      assert.equal(records.length, 1, "source precedes any extraction request");
      return extraction({ extracted: invalid, key: "synthetic-secret" });
    },
    apiExtract: async () => extraction({ extracted: unverified }),
  });
  assert.equal(result, null);
  assert.deepEqual(
    records.map((record) => record.phase),
    ["source", "candidate", "candidate"],
  );
  assert.equal(records[1].grounding_reason, "path_rejected");
  assert.equal(records[2].grounding_reason, "model_unverified");
  assert.equal(
    records[0].source_sha256,
    createHash("sha256").update(records[0].source_json).digest("hex"),
  );
  assert.doesNotMatch(JSON.stringify(records), /synthetic-secret/u);
  assert.deepEqual(
    diagnostics.map((item) => item.code),
    ["path_rejected", "model_unverified"],
  );
  assert.doesNotMatch(
    JSON.stringify(diagnostics),
    /synthetic-secret|source-backed/u,
  );
});

test("a rejected evidence write stops extraction without another provider request", async () => {
  let calls = 0;
  const diagnostics = [];
  const result = await interpretObservation(exchange, {
    onEvidence: async () => {
      throw new Error("synthetic-secret");
    },
    browserExtract: async () => {
      calls++;
      return extraction();
    },
    apiExtract: async () => {
      calls++;
      return extraction();
    },
    onDiagnostic: (entry) => diagnostics.push(entry),
  });
  assert.equal(result, null);
  assert.equal(calls, 0);
  assert.deepEqual(
    diagnostics.map((entry) => entry.code),
    ["evidence_persist_failed"],
  );
});

test("browser failure or ungrounded output falls back to API on the same source", async () => {
  for (const browserExtract of [
    async () => {
      throw new Error("synthetic browser failure");
    },
    async () => null,
    async () => extraction({ extracted: unverified }),
    async () =>
      extraction({
        extracted: {
          ...extracted,
          answer_segments: [{ path: "/messages/99/invented" }],
        },
      }),
  ]) {
    let calls = 0;
    const result = await interpretObservation(exchange, {
      browserExtract,
      apiExtract: async (prompt) => {
        calls += 1;
        assert.equal(prompt, extractionPrompt(observationDocument(exchange)));
        return extraction({ model: config.model, surface: "model_api" });
      },
    });
    assert.equal(calls, 1);
    assert.equal(result.raw_answer, exchange.messages[3].text);
    assert.equal(result.audit.model, config.model);
    assert.equal(result.audit.surface, "model_api");
    assert.deepEqual(result.audit.attempts, [
      { route: "signed_in_browser", status: "unverified" },
      { route: "configured_model_api", status: "grounded" },
    ]);
    assert.equal(exchange.model, "observed-model");
    assert.equal(exchange.surface, "consumer_web");
  }
});

test("invented answers and citation sources from either extraction route stay unverified", async () => {
  for (const invalid of [
    {
      ...extracted,
      answer_segments: [{ path: "/messages/3/text", quote: "invented answer" }],
    },
    {
      ...extracted,
      citations: [
        { url: { path: "/invented/url" }, usage: extracted.citations[0].usage },
      ],
    },
    { ...extracted, completion_evidence: [{ path: "/invented" }] },
  ]) {
    const result = await interpretObservation(exchange, {
      browserExtract: async () => extraction({ extracted: invalid }),
      apiExtract: async () =>
        extraction({ extracted: invalid, surface: "model_api" }),
    });
    assert.equal(result, null);
  }
});

test("oversized input and canceled interpretation do not invoke either extractor", async () => {
  let calls = 0;
  const invoke = async () => {
    calls += 1;
    return extraction();
  };
  assert.equal(
    await interpretObservation(
      {
        messages: [{ text: "x".repeat(750_001) }],
      },
      { browserExtract: invoke, apiExtract: invoke },
    ),
    null,
  );
  const controller = new AbortController();
  controller.abort();
  assert.equal(
    await interpretObservation(exchange, {
      signal: controller.signal,
      browserExtract: invoke,
      apiExtract: invoke,
    }),
    null,
  );
  assert.equal(calls, 0);
});

test("cancellation during browser failure prevents API fallback", async () => {
  const controller = new AbortController();
  let apiCalls = 0;
  assert.equal(
    await interpretObservation(exchange, {
      signal: controller.signal,
      browserExtract: async () => {
        controller.abort();
        return null;
      },
      apiExtract: async () => {
        apiCalls += 1;
        return extraction();
      },
    }),
    null,
  );
  assert.equal(apiCalls, 0);
});

test("API discards a successful response canceled while fetch was pending", async () => {
  const controller = new AbortController();
  const result = await api(
    async () => {
      controller.abort();
      return Response.json(envelope());
    },
    { signal: controller.signal },
  );
  assert.equal(controller.signal.aborted, true);
  assert.equal(result, null);
});

test("API cancels and releases a body canceled during an awaited stream read", async () => {
  const controller = new AbortController();
  let canceled = false;
  let released = false;
  const result = await api(
    async () => ({
      ok: true,
      body: {
        getReader: () => ({
          read: async () => {
            controller.abort();
            return {
              done: false,
              value: Buffer.from(JSON.stringify(envelope())),
            };
          },
          cancel: async () => {
            canceled = true;
          },
          releaseLock: () => {
            released = true;
          },
        }),
      },
    }),
    { signal: controller.signal },
  );
  assert.equal(result, null);
  assert.equal(canceled, true);
  assert.equal(released, true);
});

test("interpretation discards grounded results canceled during either extractor", async () => {
  for (const route of ["browser", "api"]) {
    const controller = new AbortController();
    let apiCalls = 0;
    const result = await interpretObservation(exchange, {
      signal: controller.signal,
      browserExtract: async () => {
        if (route === "api") return null;
        controller.abort();
        return extraction();
      },
      apiExtract: async () => {
        apiCalls += 1;
        controller.abort();
        return extraction({ surface: "model_api" });
      },
    });
    assert.equal(result, null);
    assert.equal(apiCalls, route === "api" ? 1 : 0);
  }
});

test("grounded searched answers can preserve an empty citation list without fabrication", async () => {
  let apiCalls = 0;
  const result = await interpretObservation(exchange, {
    browserExtract: async () =>
      extraction({
        extracted: { ...extracted, citations: [] },
      }),
    apiExtract: async () => {
      apiCalls += 1;
      return extraction();
    },
  });
  assert.equal(result.raw_answer, exchange.messages[3].text);
  assert.deepEqual(result.citations, []);
  assert.equal(apiCalls, 0);
  assert.equal(
    result.audit.refs.some((ref) => ref.role === "citation_url"),
    false,
  );
  assert.ok(result.audit.refs.some((ref) => ref.role === "search_activity"));
  assert.deepEqual(result.audit.attempts, [
    {
      route: "signed_in_browser",
      status: "grounded",
    },
  ]);
});

test("audit preserves replayable sanitized source JSON bound to the exact digest", async () => {
  const source = structuredClone(exchange);
  source.messages[0].authorization = "synthetic-private-credential";
  source.messages[1].reasoning = "synthetic-private-reasoning";
  source.messages[2].metadata = { account_id: "synthetic-private-account" };
  const renderedText = "The source-backed answer.";
  const result = await interpretObservation(source, {
    renderedText,
    browserExtract: async () => extraction(),
    apiExtract: async () => {
      throw new Error("API fallback must not run");
    },
  });
  const expectedDocument = observationDocument(source, renderedText);
  assert.equal(result.audit.source_json, JSON.stringify(expectedDocument));
  assert.doesNotMatch(result.audit.source_json, /synthetic-private/u);
  assert.equal(
    result.audit.source_sha256,
    createHash("sha256").update(result.audit.source_json, "utf8").digest("hex"),
  );
  const replayed = validateAiObservation(
    JSON.parse(result.audit.source_json),
    extracted,
  );
  assert.equal(replayed.raw_answer, result.raw_answer);
  assert.deepEqual(replayed.citations, result.citations);
  assert.equal(replayed.chat_id, result.chat_id);
  assert.equal(replayed.message_id, result.message_id);
  assert.deepEqual(replayed.audit.refs, result.audit.refs);
});
