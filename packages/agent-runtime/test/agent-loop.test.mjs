import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const bundlePath = new URL(
  "../dist/memeloop-agent-loop.bundle.mjs",
  import.meta.url,
);

function priorHistory(count = 2, padding = "") {
  return Array.from({ length: count }, (_, index) => [
    {
      message_id: `prior-user-${index}`,
      root_message_id: `prior-user-${index}`,
      sequence: index * 2 + 1,
      role: "user",
      content: `Prior question ${index}${padding}`,
    },
    {
      message_id: `prior-answer-${index}`,
      root_message_id: `prior-user-${index}`,
      sequence: index * 2 + 2,
      role: "assistant",
      content: `Prior answer ${index}${padding}`,
    },
  ]).flat();
}

const historyTurn = {
  conversation_id: "history-conversation",
  message_id: "current-user",
  turn_id: "current-turn",
  run_id: "current-run",
  timestamp: 0,
  prompt: "Current question",
};

function finalModelAnswer(text = "Current answer") {
  return {
    text,
    model: "stub-model",
    prompt_tokens: 1,
    completion_tokens: 1,
    finish_reason: "stop",
  };
}

test("native history is ordered exactly once in fresh and repeated invocations", async () => {
  const requests = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async modelComplete(request) {
      requests.push(request);
      return finalModelAnswer();
    },
  };
  try {
    const history = priorHistory();
    const { main } = await import(
      `${bundlePath.href}?historyFresh=${Date.now()}`
    );
    const input = { ...historyTurn, history, history_omitted_turns: 3 };
    for (let attempt = 0; attempt < 2; attempt++) {
      const result = await main(input);
      assert.equal(result.answer, "Current answer");
      assert.equal(result.history_omitted_turns, 3);
    }
    const fresh = await import(
      `${bundlePath.href}?historyFreshOther=${Date.now()}`
    );
    await fresh.main(input);
    for (const request of requests) {
      assert.deepEqual(
        request.messages.filter(({ role }) => role !== "system"),
        [
          ...history.map(({ role, content }) => ({ role, content })),
          { role: "user", content: input.prompt },
        ],
      );
      assert.ok(
        !request.tools.some(
          ({ function: tool }) => tool.name === "knowledge_import_attachments",
        ),
      );
      assert.match(
        request.system,
        /3 earlier completed conversation turns were omitted/u,
      );
      assert.match(request.system, /do not claim complete recall/u);
    }
    await main({ ...historyTurn, history: [] });
    assert.deepEqual(
      requests.at(-1).messages.filter(({ role }) => role !== "system"),
      [{ role: "user", content: historyTurn.prompt }],
    );
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("native history survives tool iterations crossing count and byte page boundaries", async () => {
  const requests = [];
  const history = priorHistory(20, "x".repeat(2950));
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      return { evidence: [{ quote: "e".repeat(5000) }] };
    },
    async modelComplete(request) {
      requests.push(request);
      const call = requests.length;
      return call <= 7
        ? {
            ...finalModelAnswer(""),
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: `history-search-${call}`,
                type: "function",
                function: {
                  name: "knowledge_search",
                  arguments: '{"query":"guide"}',
                },
              },
            ],
          }
        : finalModelAnswer();
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?historyTools=${Date.now()}`
    );
    assert.equal(
      (await main({ ...historyTurn, history })).answer,
      "Current answer",
    );
    assert.equal(requests.length, 8);
    for (const [index, request] of requests.entries()) {
      assert.deepEqual(
        request.messages.filter(
          ({ role, tool_calls }) =>
            role === "user" || (role === "assistant" && !tool_calls?.length),
        ),
        [
          ...history.map(({ role, content }) => ({ role, content })),
          { role: "user", content: historyTurn.prompt },
        ],
      );
      assert.equal(
        request.messages.filter(({ role }) => role === "tool").length,
        index,
      );
    }
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("history rejects partial, reordered, oversized, duplicate and authority-bearing input before model calls", async () => {
  const { main } = await import(
    `${bundlePath.href}?historyInvalid=${Date.now()}`
  );
  const pair = priorHistory(1);
  const invalid = [
    null,
    [pair[0]],
    [pair[1], pair[0]],
    [pair[0], { ...pair[1], message_id: pair[0].message_id }],
    [
      {
        ...pair[0],
        message_id: historyTurn.message_id,
        root_message_id: historyTurn.message_id,
      },
      pair[1],
    ],
    [pair[0], { ...pair[1], root_message_id: "different-root" }],
    [pair[0], { ...pair[1], sequence: pair[0].sequence }],
    [pair[0], { ...pair[1], sequence: Number.MAX_SAFE_INTEGER + 1 }],
    [{ ...pair[0], message_id: "\ninvalid-id" }, pair[1]],
    [{ ...pair[0], metadata: { attachmentReferences: [] } }, pair[1]],
    [{ ...pair[0], attachments: [] }, pair[1]],
    [{ ...pair[0], content: null }, pair[1]],
    [pair[0], { ...pair[1], content: "" }],
    priorHistory(21),
    priorHistory(1, "三".repeat(23000)),
  ];
  for (const history of invalid) {
    await assert.rejects(main({ ...historyTurn, history }), /history/u);
  }
  await assert.rejects(
    main({ ...historyTurn, history_omitted_turns: -1 }),
    /history_omitted_turns/u,
  );
});

test("canonical session paging enforces revision, cursors, byte/count budgets and detached results", async () => {
  const { createSession } = await import(
    `${bundlePath.href}?historyPaging=${Date.now()}`
  );
  const seeded = priorHistory(3).map(
    ({ message_id, root_message_id, role, content }) => ({
      messageId: message_id,
      turnId: root_message_id,
      role,
      content,
    }),
  );
  const session = createSession("paging", seeded, 100);
  const read = (options) =>
    session.storage.getFullContentMessagePage("paging", options);
  const last = await read({ limit: 2, direction: "backward" });
  assert.equal(last.items[0].messageId, "prior-user-2");
  assert.equal(last.hasMoreBefore, true);
  assert.equal(last.hasMoreAfter, false);
  assert.deepEqual(
    last.items.map(({ turnId }) => turnId),
    ["prior-user-2", "prior-user-2"],
  );
  const previous = await read({
    limit: 2,
    direction: "backward",
    before: last.startCursor,
    expectedRevision: last.revision,
  });
  assert.equal(previous.items[0].messageId, "prior-user-1");
  const forward = await read({
    limit: 2,
    after: previous.endCursor,
    expectedRevision: last.revision,
  });
  assert.deepEqual(forward.items, last.items);
  const uncovered = await read({
    afterCoveredVersion: { "geo-embedded-worker": 4 },
  });
  assert.deepEqual(uncovered.items, last.items);
  assert.equal(uncovered.hasMoreBefore, false);
  const one = await read({ limit: 1 });
  const budget = Buffer.byteLength(JSON.stringify(one));
  const bounded = await read({ limit: 50, maxBytes: budget });
  assert.equal(bounded.items.length, 1);
  assert.ok(Buffer.byteLength(JSON.stringify(bounded)) <= budget);
  one.items[0].content = "mutated";
  assert.equal((await read({ limit: 1 })).items[0].content, seeded[0].content);
  const reset = await read({ expectedRevision: "stale" });
  assert.equal(reset.reset, true);
  assert.deepEqual(reset.items, []);
  for (const options of [
    { limit: 51 },
    { maxBytes: 1 },
    { maxBytes: 256 * 1024 + 1 },
    { before: last.startCursor },
    { direction: "sideways" },
    {
      before: last.startCursor,
      after: last.endCursor,
      expectedRevision: last.revision,
    },
    {
      before: { ...last.startCursor, messageId: "absent" },
      expectedRevision: last.revision,
    },
  ]) {
    await assert.rejects(read(options), /page|cursor/u);
  }
  await assert.rejects(
    session.storage.getFullContentMessagePage("foreign"),
    /foreign/u,
  );
});

test("the generated ESM bundle runs a real MemeLoop loop through the host completion bridge", async () => {
  const emitted = [];
  const requests = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit(topic, payload) {
      emitted.push({ payload, topic });
    },
    async modelComplete(request) {
      requests.push(request);
      return {
        completion_tokens: 4,
        finish_reason: "stop",
        model: "stub-model",
        prompt_tokens: 7,
        text: "The warranty lasts two years.",
      };
    },
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
  };

  try {
    const { main } = await import(`${bundlePath.href}?smoke=${Date.now()}`);
    const completion = await main({
      conversation_id: "conversation-smoke-0001",
      prompt: "How long is the warranty?",
      run_id: "run-smoke-0001",
      timestamp: 1_700_000_000_000,
      turn_id: "turn-smoke-0001",
      history_omitted_turns: 0,
    });

    assert.equal(requests.length, 1);
    assert.match(requests[0].prompt, /user: How long is the warranty\?/u);
    assert.equal(requests[0].model, undefined);
    assert.deepEqual(completion, {
      answer: "The warranty lasts two years.",
      conversation_id: "conversation-smoke-0001",
      history_omitted_turns: 0,
      model: "stub-model",
      run_id: "run-smoke-0001",
      turn_id: "turn-smoke-0001",
    });

    assert.equal(emitted.length, 1);
    assert.equal(emitted[0].topic, "loop.completed");
    assert.deepEqual(JSON.parse(emitted[0].payload), completion);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("native model call executes knowledge.search through the MemeLoop registry and returns evidence to the model", async () => {
  const requests = [];
  const searches = [];
  const emitted = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit(topic, payload) {
      emitted.push({ topic, payload });
    },
    async knowledgeSearch(request) {
      searches.push(request);
      return {
        knowledge_release_id: null,
        evidence: [{ source_name: "Manual", quote: "Two years" }],
        capability_missing: null,
      };
    },
    async modelComplete(request) {
      requests.push(request);
      return requests.length === 1
        ? {
            text: "",
            tool_calls: [
              {
                id: "call-1",
                type: "function",
                function: {
                  name: "knowledge_search",
                  arguments: '{"query":"warranty","limit":2}',
                },
              },
            ],
            model: "stub-model",
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "tool_calls",
          }
        : {
            text: "The warranty lasts two years (Manual).",
            model: "stub-model",
            prompt_tokens: 9,
            completion_tokens: 8,
            finish_reason: "stop",
          };
    },
  };

  try {
    const { main } = await import(`${bundlePath.href}?tool=${Date.now()}`);
    const completion = await main({
      conversation_id: "conversation-tool-smoke-0001",
      prompt: "What is the warranty?",
      run_id: "run-tool-smoke-0001",
      timestamp: 1_700_000_000_000,
      turn_id: "turn-tool-smoke-0001",
    });
    assert.equal(requests.length, 2);
    assert.equal(requests[0].tools[0].function.name, "knowledge_search");
    assert.deepEqual(searches, [{ query: "warranty", limit: 2 }]);
    assert.deepEqual(requests[1].messages.at(-2).tool_calls[0].id, "call-1");
    assert.equal(requests[1].messages.at(-1).tool_call_id, "call-1");
    assert.match(requests[1].messages.at(-1).content, /Two years/u);
    assert.equal(completion.answer, "The warranty lasts two years (Manual).");
    assert.equal(emitted.length, 1);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("knowledge text tools read a scoped exact version and return the persisted revision receipt", async () => {
  const sourceId = "00000000-0000-4000-8000-000000000041";
  const baseVersionId = "00000000-0000-4000-8000-000000000042";
  const nextVersionId = "00000000-0000-4000-8000-000000000043";
  const releaseId = "00000000-0000-4000-8000-000000000044";
  const reads = [];
  const writes = [];
  const requests = [];
  const command = {
    source_id: sourceId,
    expected_revision: 2,
    idempotency_key: "stable-text-revision",
    base_version_id: baseVersionId,
    media_type: "text/markdown",
    text: "# Updated\n\nExact text.",
  };
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async knowledgeTextRead(request) {
      reads.push(request);
      return {
        source: {
          source_id: sourceId,
          revision: 2,
          current_version_id: baseVersionId,
        },
        source_version: { source_version_id: baseVersionId },
        content: {
          source_version_id: baseVersionId,
          text_basis: "exact",
          text: "# Original",
        },
      };
    },
    async knowledgeTextRevise(request) {
      writes.push(request);
      return {
        source: {
          source_id: sourceId,
          revision: 3,
          current_version_id: nextVersionId,
        },
        source_version: {
          source_version_id: nextVersionId,
          parent_version_id: baseVersionId,
        },
        knowledge_release: { knowledge_release_id: releaseId },
      };
    },
    async modelComplete(request) {
      requests.push(request);
      const calls = [
        [
          "knowledge_text_read",
          { source_id: sourceId, source_version_id: baseVersionId },
        ],
        ["knowledge_text_revise", command],
      ];
      if (requests.length > calls.length) return finalModelAnswer("Saved.");
      const [name, argumentsValue] = calls[requests.length - 1];
      return {
        ...finalModelAnswer(""),
        finish_reason: "tool_calls",
        tool_calls: [
          {
            id: `text-${requests.length}`,
            type: "function",
            function: { name, arguments: JSON.stringify(argumentsValue) },
          },
        ],
      };
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?textRevision=${Date.now()}`
    );
    assert.equal(
      (
        await main({
          conversation_id: "conversation-text-revision",
          prompt: "Update the source",
          run_id: "run-text-revision",
          turn_id: "turn-text-revision",
        })
      ).answer,
      "Saved.",
    );
    assert.deepEqual(reads, [
      { source_id: sourceId, source_version_id: baseVersionId },
    ]);
    assert.deepEqual(writes, [command]);
    assert.match(requests[1].messages.at(-1).content, /"text_basis":"exact"/u);
    assert.match(
      requests[2].messages.at(-1).content,
      new RegExp(releaseId, "u"),
    );
    const writeTool = requests[0].tools.find(
      ({ function: tool }) => tool.name === "knowledge_text_revise",
    );
    assert.deepEqual(
      writeTool.function.parameters.required,
      Object.keys(command),
    );
    assert.equal(
      writeTool.function.parameters.properties.text.maxLength,
      262144,
    );
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("chat onboarding calls scoped project tools and preserves acceptance semantics", async () => {
  const requests = [];
  const observed = [];
  const commands = [
    ["project_current", {}],
    [
      "project_revise",
      {
        expected_revision: 1,
        idempotency_key: "draft-one",
        patch: { brand_name: "Example", market: "global", language: "en" },
        source_version_ids: [],
      },
    ],
    ["project_estimate", {}],
    ["project_start", { expected_revision: 2, idempotency_key: "start-one" }],
  ];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async projectCurrent(request) {
      observed.push(["project_current", request]);
      return {
        project: { revision: 1, status: "draft" },
        missing_fields: ["brand_name"],
      };
    },
    async projectRevise(request) {
      observed.push(["project_revise", request]);
      return { project: { revision: 2, status: "draft" }, missing_fields: [] };
    },
    async projectEstimate(request) {
      observed.push(["project_estimate", request]);
      return { blockers: [], costs: { state: "estimated" } };
    },
    async projectStart(request) {
      observed.push(["project_start", request]);
      return { status: "accepted", operation_id: "operation-reference" };
    },
    async modelComplete(request) {
      requests.push(request);
      const next = commands[requests.length - 1];
      return next
        ? {
            text: "",
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: `onboarding-${requests.length}`,
                type: "function",
                function: { name: next[0], arguments: JSON.stringify(next[1]) },
              },
            ],
          }
        : finalModelAnswer("Startup accepted; background work is pending.");
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?onboarding=${Date.now()}`
    );
    const result = await main({
      ...historyTurn,
      prompt:
        "Set up Example for global English content and start using my supplied settings.",
    });
    assert.deepEqual(observed, commands);
    assert.match(result.answer, /accepted/u);
    assert.match(requests[0].system, /only for missing information/u);
    assert.match(requests[0].system, /invent.*budget/u);
    assert.match(requests[4].messages.at(-1).content, /"status":"accepted"/u);
    for (const tool of requests[0].tools.filter(({ function: fn }) =>
      fn.name.startsWith("project_"),
    )) {
      assert.equal(tool.function.parameters.additionalProperties, false);
      assert.equal(tool.function.parameters.properties.project_id, undefined);
      assert.equal(tool.function.parameters.properties.tenant_id, undefined);
    }
    const revise = requests[0].tools.find(
      ({ function: fn }) => fn.name === "project_revise",
    ).function.parameters;
    assert.equal(revise.properties.patch.additionalProperties, false);
    assert.equal(revise.properties.patch.properties.initial_sources, undefined);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("recommendations tool reads scoped optimization-safe channel suggestions", async () => {
  const calls = [];
  let modelCalls = 0;
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async sourceRecommendations(request) {
      calls.push(request);
      return {
        scope: "returned_plans_only",
        rule_version: "source-channel-rules.v1",
        items: [
          {
            platform_id: "zhihu",
            source_hosts: ["www.zhihu.com"],
            citing_answers: 1,
            publication: {
              connector_availability: "unavailable",
              account_ready: false,
            },
          },
        ],
      };
    },
    async modelComplete(request) {
      modelCalls++;
      if (modelCalls === 1) {
        const schema = request.tools.find(
          ({ function: tool }) =>
            tool.name === "source_channel_recommendations",
        ).function.parameters;
        assert.equal(schema.additionalProperties, false);
        assert.equal(schema.properties.project_id, undefined);
        return {
          ...finalModelAnswer(""),
          finish_reason: "tool_calls",
          tool_calls: [
            {
              id: "recommendation-read",
              type: "function",
              function: {
                name: "source_channel_recommendations",
                arguments: '{"limit":2}',
              },
            },
          ],
        };
      }
      assert.match(
        request.messages.at(-1).content,
        /"connector_availability":"unavailable"/u,
      );
      return finalModelAnswer("Observed, not ready to publish.");
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?recommendations=${Date.now()}`
    );
    const result = await main({
      ...historyTurn,
      prompt: "Inspect publication opportunities.",
    });
    assert.equal(result.answer, "Observed, not ready to publish.");
    assert.deepEqual(calls, [{ limit: 2 }]);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("traditional search tools preserve scoped create, detail and local reparse selectors", async () => {
  const id = "00000000-0000-4000-8000-000000000071";
  const evidence = "00000000-0000-4000-8000-000000000072";
  const at = "2026-01-01T00:00:00Z";
  const calls = [];
  const steps = [
    ["serp_read", {}],
    [
      "serp_create",
      {
        query: " rain + café% ",
        idempotency_key: "stable-search",
        scheduled_at: at,
      },
    ],
    ["serp_read", { mode: "detail", measurement_id: id }],
    [
      "serp_reparse",
      {
        measurement_id: id,
        evidence_id: evidence,
        idempotency_key: "stable-parse",
      },
    ],
  ];
  let iteration = 0;
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected knowledge search");
    },
    async serpRead(request) {
      calls.push(["serp_read", request]);
      return request.mode
        ? {
            measurements: [{ measurement_id: id }],
            observations: [{ evidence_id: evidence }],
          }
        : { server_time: at, capabilities: [{ source_key: "synthetic" }] };
    },
    async serpCreate(request) {
      calls.push(["serp_create", request]);
      return { measurement: { measurement_id: id, state: "queued" } };
    },
    async serpReparse(request) {
      calls.push(["serp_reparse", request]);
      return { observation: { evidence_id: evidence } };
    },
    async modelComplete(request) {
      for (const name of ["serp_create", "serp_read", "serp_reparse"]) {
        assert.ok(request.tools.some((tool) => tool.function.name === name));
      }
      const step = steps[iteration++];
      return step
        ? {
            text: "",
            model: "stub-model",
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: `search-${iteration}`,
                type: "function",
                function: { name: step[0], arguments: JSON.stringify(step[1]) },
              },
            ],
          }
        : {
            text: "Retained search response reparsed.",
            model: "stub-model",
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?search=${Date.now()}`);
    const result = await main({
      ...historyTurn,
      prompt: "Measure this search topic.",
    });
    assert.equal(result.answer, "Retained search response reparsed.");
    assert.deepEqual(calls, steps);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("report reduction and immutable read are exposed as separate scoped host tools", async () => {
  const cycleId = "00000000-0000-4000-8000-000000000031";
  const reportId = "00000000-0000-4000-8000-000000000032";
  const requests = [];
  const reductions = [];
  const reads = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async reportReduce(request) {
      reductions.push(request);
      return { report_id: reportId, cycle_id: cycleId, status: "partial" };
    },
    async reportGet(request) {
      reads.push(request);
      return {
        report_id: reportId,
        project_id: "00000000-0000-4000-8000-000000000033",
        evidence: [{ evidence_id: "00000000-0000-4000-8000-000000000034" }],
      };
    },
    async modelComplete(request) {
      requests.push(request);
      const calls = [
        {
          name: "report_reduce",
          arguments: "{}",
        },
        {
          name: "report_get",
          arguments: "{}",
        },
      ];
      return requests.length <= calls.length
        ? {
            text: "",
            tool_calls: [
              {
                id: `call-${requests.length}`,
                type: "function",
                function: calls[requests.length - 1],
              },
            ],
            model: "stub-model",
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "tool_calls",
          }
        : {
            text: "Report available with a coverage gap.",
            model: "stub-model",
            prompt_tokens: 9,
            completion_tokens: 8,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?report=${Date.now()}`);
    const result = await main({
      conversation_id: "conversation-report-smoke",
      prompt: "Reduce the due cycle and read the report",
      run_id: "run-report-smoke",
      turn_id: "turn-report-smoke",
    });
    assert.equal(result.answer, "Report available with a coverage gap.");
    assert.deepEqual(reductions, [{}]);
    assert.deepEqual(reads, [{}]);
    assert.deepEqual(
      requests[0].tools.map((tool) => tool.function.name),
      [
        "knowledge_search",
        "knowledge_text_read",
        "knowledge_text_revise",
        "knowledge_import_status",
        "report_get",
        "report_preview",
        "report_reduce",
        "serp_create",
        "serp_read",
        "serp_reparse",
        "source_channel_recommendations",
        "project_current",
        "project_revise",
        "project_estimate",
        "project_start",
        "channel_discover",
        "channel_plan",
        "channel_manifest_read",
        "channel_target_execute",
        "question_discover",
        "question_create",
        "question_revise",
        "measurement_options",
        "measurement_plan_create",
        "measurement_plan_read",
        "content_start",
        "content_execution_read",
        "content_media_list",
        "content_document_read",
        "content_media_insert",
        "distribution_start",
        "distribution_read",
        "distribution_resume",
        "distribution_targets_read",
        "content_distribute_request",
        "content_distribute_read",
      ],
    );
    assert.match(requests[1].messages.at(-1).content, /"status":"partial"/u);
    assert.match(requests[2].messages.at(-1).content, /"evidence_id"/u);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("report preview is independent of official read and reduce", async () => {
  const cycleId = "00000000-0000-4000-8000-000000000031";
  const calls = [];
  const requests = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async reportGet() {
      throw new Error("Preview must not read an official report");
    },
    async reportReduce() {
      throw new Error("Preview must not create an official report");
    },
    async reportPreview(request) {
      calls.push(request);
      return { kind: "preview", cycle_id: cycleId, status: "partial" };
    },
    async modelComplete(request) {
      requests.push(request);
      return requests.length <= 2
        ? {
            text: "",
            tool_calls: [
              {
                id: `preview-${requests.length}`,
                type: "function",
                function: {
                  name: "report_preview",
                  arguments:
                    requests.length === 1
                      ? "{}"
                      : JSON.stringify({ cycle_id: cycleId }),
                },
              },
            ],
            model: "stub-model",
            prompt_tokens: 3,
            completion_tokens: 3,
            finish_reason: "tool_calls",
          }
        : {
            text: "This is a temporary preview.",
            model: "stub-model",
            prompt_tokens: 3,
            completion_tokens: 3,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?preview=${Date.now()}`);
    const result = await main({
      conversation_id: "conversation-preview",
      prompt: "Preview current coverage",
      run_id: "run-preview",
      turn_id: "turn-preview",
    });
    assert.equal(result.answer, "This is a temporary preview.");
    assert.deepEqual(calls, [{}, { cycle_id: cycleId }]);
    const previewTool = requests[0].tools.find(
      (tool) => tool.function.name === "report_preview",
    );
    assert.deepEqual(Object.keys(previewTool.function.parameters.properties), [
      "kind",
      "cycle_id",
      "window",
    ]);
    assert.equal(previewTool.function.parameters.additionalProperties, false);
    assert.match(previewTool.function.description, /temporary|unsaved/u);
    assert.match(requests[1].messages.at(-1).content, /"kind":"preview"/u);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("cycle-free reports use existing tools and the exact preview window", async () => {
  const reportId = "00000000-0000-4000-8000-000000000042";
  const window = {
    start_at: "2026-10-01T00:00:00Z",
    end_at: "2026-10-08T00:00:00Z",
    report_timezone: "UTC",
  };
  const steps = [
    ["report_preview", { kind: "measurement_period" }],
    ["report_reduce", { kind: "measurement_period", window }],
    ["report_get", { kind: "measurement_period", report_id: reportId }],
    ["report_get", { kind: "measurement_period", list: true }],
  ];
  const calls = [];
  const requests = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Report does not need enterprise setup");
    },
    async reportPreview(request) {
      calls.push(["report_preview", request]);
      return {
        kind: "measurement_period_preview",
        report_window_start_at: window.start_at,
        report_window_end_at: window.end_at,
        report_timezone: window.report_timezone,
        coverage: { planned: 1 },
      };
    },
    async reportReduce(request) {
      calls.push(["report_reduce", request]);
      return {
        kind: "measurement_period",
        report_id: reportId,
        coverage: { planned: 1 },
      };
    },
    async reportGet(request) {
      calls.push(["report_get", request]);
      return request.list
        ? { kind: "measurement_period_list", items: [{ report_id: reportId }] }
        : { kind: "measurement_period", report_id: reportId };
    },
    async modelComplete(request) {
      requests.push(request);
      const step = steps[requests.length - 1];
      return step
        ? {
            text: "",
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: `period-${requests.length}`,
                type: "function",
                function: { name: step[0], arguments: JSON.stringify(step[1]) },
              },
            ],
          }
        : {
            text: "Saved the measurement report.",
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?measurement-period=${Date.now()}`
    );
    const result = await main({
      conversation_id: "conversation-period",
      prompt: "Save my topic measurement report",
      run_id: "run-period",
      turn_id: "turn-period",
    });
    assert.equal(result.answer, "Saved the measurement report.");
    assert.deepEqual(calls, steps);
    for (const name of ["report_get", "report_preview", "report_reduce"]) {
      const tool = requests[0].tools.find(
        (entry) => entry.function.name === name,
      ).function;
      assert.deepEqual(tool.parameters.properties.kind.enum, [
        "cycle",
        "measurement_period",
      ]);
      assert.match(tool.description, /measurement_period/u);
    }
    assert.match(
      requests[0].tools.find((entry) => entry.function.name === "report_reduce")
        .function.description,
      /EXACT/u,
    );
    assert.ok(
      calls.every(([, request]) => !Object.hasOwn(request, "cycle_id")),
    );
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("attachment-only turn imports bound items, searches its release, and answers from evidence", async () => {
  const textId = "00000000-0000-4000-8000-000000000001";
  const otherId = "00000000-0000-4000-8000-000000000002";
  const releaseId = "00000000-0000-4000-8000-000000000003";
  const modelRequests = [];
  const imports = [];
  const searches = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeImportAttachments(request) {
      imports.push(request);
      return {
        items: [
          {
            attachment_id: textId,
            status: "succeeded",
            source_id: "00000000-0000-4000-8000-000000000004",
            source_version_id: "00000000-0000-4000-8000-000000000005",
            knowledge_release_id: releaseId,
          },
          {
            attachment_id: otherId,
            status: "failed",
            error: "capability_missing: parser unavailable",
          },
        ],
      };
    },
    async knowledgeSearch(request) {
      searches.push(request);
      return {
        knowledge_release_id: releaseId,
        evidence: [{ source_name: "Guide", quote: "Two years" }],
      };
    },
    async modelComplete(request) {
      modelRequests.push(request);
      const index = modelRequests.length;
      if (index === 3) {
        return {
          text: "The warranty is two years (Guide). The other file could not be imported.",
          model: "stub-model",
          prompt_tokens: 9,
          completion_tokens: 8,
          finish_reason: "stop",
        };
      }
      return {
        text: "",
        tool_calls: [
          {
            id: `call-${index}`,
            type: "function",
            function:
              index === 1
                ? {
                    name: "knowledge_import_attachments",
                    arguments: JSON.stringify({
                      items: [
                        { attachment_id: textId, purpose: "internal" },
                        { attachment_id: otherId, purpose: "internal" },
                      ],
                    }),
                  }
                : {
                    name: "knowledge_search",
                    arguments: JSON.stringify({
                      query: "warranty",
                      knowledge_release_id: releaseId,
                    }),
                  },
          },
        ],
        model: "stub-model",
        prompt_tokens: 7,
        completion_tokens: 4,
        finish_reason: "tool_calls",
      };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?import=${Date.now()}`);
    const completion = await main({
      conversation_id: "conversation-import-0001",
      message_id: "message-import-0001",
      prompt: "",
      run_id: "run-import-0001",
      turn_id: "turn-import-0001",
      attachments: [
        {
          attachment_id: textId,
          object_id: "object-1",
          filename: "Guide.txt",
          media_type: "text/plain",
          size_bytes: 9,
          sha256: "a".repeat(64),
          object_version: "version-1",
        },
        {
          attachment_id: otherId,
          object_id: "object-2",
          filename: "Data.pdf",
          media_type: "application/pdf",
          size_bytes: 11,
          sha256: "b".repeat(64),
        },
      ],
    });
    assert.equal(modelRequests.length, 3);
    assert.deepEqual(
      modelRequests[0].tools.map((tool) => tool.function.name),
      [
        "knowledge_import_attachments",
        "knowledge_import_status",
        "knowledge_search",
        "knowledge_text_read",
        "knowledge_text_revise",
        "report_get",
        "report_preview",
        "report_reduce",
        "serp_create",
        "serp_read",
        "serp_reparse",
        "source_channel_recommendations",
        "project_current",
        "project_revise",
        "project_estimate",
        "project_start",
        "channel_discover",
        "channel_plan",
        "channel_manifest_read",
        "channel_target_execute",
        "question_discover",
        "question_create",
        "question_revise",
        "measurement_options",
        "measurement_plan_create",
        "measurement_plan_read",
        "content_start",
        "content_execution_read",
        "content_media_list",
        "content_document_read",
        "content_media_insert",
        "content_media_bind",
        "distribution_start",
        "distribution_read",
        "distribution_resume",
        "distribution_targets_read",
        "content_distribute_request",
        "content_distribute_read",
      ],
    );
    const importSchema = modelRequests[0].tools[0].function.parameters;
    assert.deepEqual(
      importSchema.properties.items.items.properties.attachment_id.enum,
      [textId, otherId],
    );
    assert.match(
      importSchema.properties.items.items.properties.attachment_id.description,
      /Guide\.txt, text\/plain/u,
    );
    assert.deepEqual(imports, [
      {
        items: [
          { attachment_id: textId, purpose: "internal" },
          { attachment_id: otherId, purpose: "internal" },
        ],
      },
    ]);
    assert.deepEqual(searches, [
      { query: "warranty", knowledge_release_id: releaseId },
    ]);
    assert.equal(modelRequests[0].messages.at(-1).content, "");
    assert.match(
      modelRequests[1].messages.at(-1).content,
      /capability_missing/u,
    );
    assert.match(modelRequests[2].messages.at(-1).content, /Two years/u);
    assert.equal(completion.turn_id, "turn-import-0001");
    assert.match(completion.answer, /Guide/u);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("queued attachment uses a later exact job status and release before citing evidence", async () => {
  const attachmentId = "00000000-0000-4000-8000-000000000011";
  const jobId = "00000000-0000-4000-8000-000000000012";
  const releaseId = "00000000-0000-4000-8000-000000000013";
  const seen = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeImportAttachments() {
      return {
        items: [
          {
            attachment_id: attachmentId,
            import_job_id: jobId,
            status: "queued",
          },
        ],
      };
    },
    async knowledgeImportStatus(request) {
      assert.deepEqual(request, { import_job_id: jobId, purpose: "internal" });
      return {
        import_job_id: jobId,
        status: "partial",
        stage: "release",
        source_id: "00000000-0000-4000-8000-000000000014",
        source_version_id: "00000000-0000-4000-8000-000000000015",
        knowledge_release_id: releaseId,
        completed_units: 1,
        failed_units: 1,
        error_count: 1,
        errors: [{ code: "parse_failed", page: 2 }],
      };
    },
    async knowledgeSearch(request) {
      assert.equal(request.knowledge_release_id, releaseId);
      return {
        knowledge_release_id: releaseId,
        evidence: [{ quote: "A scoped statement", page: 1 }],
      };
    },
    async modelComplete(request) {
      const index = seen.push(request);
      if (index === 4)
        return finalModelAnswer(
          "Page 1 supports the statement; page 2 failed.",
        );
      const argumentsByStep = [
        { items: [{ attachment_id: attachmentId, purpose: "internal" }] },
        { import_job_id: jobId, purpose: "internal" },
        {
          query: "statement",
          knowledge_release_id: releaseId,
          purpose: "internal",
        },
      ];
      const names = [
        "knowledge_import_attachments",
        "knowledge_import_status",
        "knowledge_search",
        "knowledge_text_read",
        "knowledge_text_revise",
      ];
      return {
        ...finalModelAnswer(""),
        finish_reason: "tool_calls",
        tool_calls: [
          {
            id: `job-${index}`,
            type: "function",
            function: {
              name: names[index - 1],
              arguments: JSON.stringify(argumentsByStep[index - 1]),
            },
          },
        ],
      };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?jobStatus=${Date.now()}`);
    const result = await main({
      conversation_id: "conversation-status",
      message_id: "message-status",
      turn_id: "turn-status",
      run_id: "run-status",
      prompt: "",
      attachments: [
        {
          attachment_id: attachmentId,
          object_id: "object-status",
          filename: "guide.pdf",
          media_type: "application/pdf",
          size_bytes: 10,
          sha256: "a".repeat(64),
        },
      ],
    });
    assert.equal(
      result.answer,
      "Page 1 supports the statement; page 2 failed.",
    );
    assert.match(seen[1].messages.at(-1).content, /"status":"queued"/u);
    assert.doesNotMatch(
      seen[1].messages.at(-1).content,
      /knowledge_release_id/u,
    );
    assert.match(seen[2].messages.at(-1).content, /"knowledge_release_id"/u);
    assert.match(seen[3].messages.at(-1).content, /A scoped statement/u);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("channel tools discover references, freeze a plan, read the manifest, and preserve deferred outcomes", async () => {
  const ids = {
    source: "00000000-0000-4000-8000-000000000101",
    version: "00000000-0000-4000-8000-000000000102",
    account: "00000000-0000-4000-8000-000000000103",
    cycle: "00000000-0000-4000-8000-000000000104",
    target: "00000000-0000-4000-8000-000000000105",
  };
  const calls = [];
  const requests = [];
  const plan = {
    cycle_id: ids.cycle,
    publications: [
      {
        source_id: ids.source,
        source_version_id: ids.version,
        platform: "public-platform",
        account_id: ids.account,
      },
    ],
    measurements: [
      {
        account_id: ids.account,
        provider: "provider",
        model: "search-model",
        surface: "web",
        search_mode: "official_search",
        protocol_version: "v1",
        question_set_version: "set-v1",
        question: "What is available?",
        market: "global",
        language: "en",
        scheduled_at: "2026-10-03T08:00:00Z",
        sample_ordinal: 0,
      },
    ],
  };
  const steps = [
    ["channel_discover", { kind: "public_sources", limit: 5 }],
    ["channel_discover", { kind: "accounts" }],
    ["channel_plan", plan],
    ["channel_manifest_read", { cycle_id: ids.cycle, revision: 1, limit: 10 }],
    ["channel_target_execute", { target_id: ids.target }],
  ];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected knowledge search");
    },
    async channelDiscover(request) {
      calls.push(["discover", request]);
      return request.kind === "public_sources"
        ? {
            kind: "public_sources",
            current_cycle_id: ids.cycle,
            items: [
              {
                kind: "public_source",
                source_id: ids.source,
                source_version_id: ids.version,
                name: "Public guide",
                media_type: "text/plain",
              },
            ],
          }
        : {
            kind: "accounts",
            current_cycle_id: ids.cycle,
            items: [
              {
                kind: "account",
                account_id: ids.account,
                platform: "public-platform",
                status: "ready",
                enabled: true,
                owner_kind: "project",
              },
            ],
          };
    },
    async channelPlan(request) {
      calls.push(["plan", request]);
      return {
        plan_id: "00000000-0000-4000-8000-000000000106",
        cycle_id: ids.cycle,
        revision: 1,
        expected_count: 2,
        dispatch_state: "pending",
      };
    },
    async channelManifestRead(request) {
      calls.push(["manifest", request]);
      return {
        revision: 1,
        targets: [{ target_id: ids.target, execution_state: "deferred" }],
      };
    },
    async channelTargetExecute(request) {
      calls.push(["execute", request]);
      return {
        target_id: ids.target,
        state: "deferred",
        deferred_reason: "account_unavailable",
      };
    },
    async modelComplete(request) {
      requests.push(request);
      const index = requests.length - 1;
      return index < steps.length
        ? {
            text: "",
            tool_calls: [
              {
                id: `channel-${index}`,
                type: "function",
                function: {
                  name: steps[index][0],
                  arguments: JSON.stringify(steps[index][1]),
                },
              },
            ],
            model: "stub-model",
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "tool_calls",
          }
        : {
            text: "The frozen plan is queued; one target is deferred, not published.",
            model: "stub-model",
            prompt_tokens: 9,
            completion_tokens: 8,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?channels=${Date.now()}`);
    const result = await main({
      conversation_id: "conversation-channel-plan",
      prompt: "Plan current public channels and inspect the target",
      run_id: "run-channel-plan",
      turn_id: "turn-channel-plan",
    });
    assert.match(result.answer, /deferred, not published/u);
    assert.equal(requests.length, 6); // Four-to-eight iteration budget permits the five-tool flow.
    assert.deepEqual(calls, [
      ["discover", { kind: "public_sources", limit: 5 }],
      ["discover", { kind: "accounts" }],
      ["plan", plan],
      ["manifest", { cycle_id: ids.cycle, revision: 1, limit: 10 }],
      ["execute", { target_id: ids.target }],
    ]);
    const definitions = Object.fromEntries(
      requests[0].tools.map(({ function: definition }) => [
        definition.name,
        definition,
      ]),
    );
    for (const name of steps.map(([name]) => name)) {
      assert.equal(definitions[name].parameters.additionalProperties, false);
    }
    assert.deepEqual(definitions.channel_plan.parameters.required, [
      "publications",
      "measurements",
    ]);
    assert.equal(
      definitions.channel_plan.parameters.properties.publications.items
        .additionalProperties,
      false,
    );
    assert.equal(
      definitions.channel_plan.parameters.properties.measurements.items
        .additionalProperties,
      false,
    );
    assert.equal(
      definitions.channel_plan.parameters.properties.bound_measurements.items
        .additionalProperties,
      false,
    );
    assert.equal(
      definitions.channel_plan.parameters.properties.bound_measurements.items
        .properties.question.additionalProperties,
      false,
    );
    assert.deepEqual(definitions.channel_target_execute.parameters.required, [
      "target_id",
    ]);
    assert.match(definitions.channel_plan.description, /automatically/u);
    assert.match(
      definitions.channel_target_execute.description,
      /never blindly resent/u,
    );
    assert.match(requests[4].messages.at(-1).content, /"deferred"/u);
    assert.match(requests[5].messages.at(-1).content, /"account_unavailable"/u);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("native question-set tools return only safe references and freeze bound targets without a purpose override", async () => {
  const ids = {
    set: "00000000-0000-4000-8000-000000000121",
    version: "00000000-0000-4000-8000-000000000122",
    question: "00000000-0000-4000-8000-000000000123",
    revision: "00000000-0000-4000-8000-000000000124",
    account: "00000000-0000-4000-8000-000000000125",
    cycle: "00000000-0000-4000-8000-000000000126",
  };
  const heldout = "HELDOUT_CANARY_DO_NOT_EXPOSE";
  const reference = {
    question_set_id: ids.set,
    question_set_version_id: ids.version,
    question_id: ids.question,
    question_revision_id: ids.revision,
  };
  const draft = {
    text: "Which options are available?",
    intent: "purchase",
    product_refs: [],
    market: "global",
    language: "en",
    source: { kind: "user_provided" },
    weight: 1,
  };
  const commands = [
    [
      "question_create",
      {
        idempotency_key: "create-1",
        name: "Customer questions",
        questions: [draft],
      },
    ],
    [
      "question_revise",
      {
        question_set_id: ids.set,
        command: {
          idempotency_key: "revise-1",
          base_version_id: ids.version,
          name: "Customer questions",
          questions: [draft],
        },
      },
    ],
    [
      "question_discover",
      { question_set_id: ids.set, question_set_version_id: ids.version },
    ],
    [
      "channel_plan",
      {
        cycle_id: ids.cycle,
        publications: [],
        measurements: [],
        bound_measurements: [
          {
            account_id: ids.account,
            provider: "search-provider",
            model: "search-model",
            surface: "web",
            search_mode: "official_search",
            protocol_version: "v1",
            question: reference,
            scheduled_at: "2026-10-06T00:00:00Z",
            sample_ordinal: 0,
          },
        ],
      },
    ],
  ];
  const seen = [];
  const completions = [];
  const receipt = {
    question_set_id: ids.set,
    question_set_version_id: ids.version,
    revision: 1,
    optimization_count: 0,
    evaluation_count: 1,
  };
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("unexpected search");
    },
    async questionCreate(request) {
      seen.push(["create", request]);
      return receipt;
    },
    async questionRevise(request) {
      seen.push(["revise", request]);
      return { ...receipt, revision: 2 };
    },
    async questionDiscover(request) {
      seen.push(["discover", request]);
      return {
        sets: [],
        versions: [],
        questions: [{ reference, purpose: "frozen_evaluation" }],
      };
    },
    async channelPlan(request) {
      seen.push(["plan", request]);
      return {
        plan_id: ids.question,
        cycle_id: ids.cycle,
        revision: 1,
        expected_count: 1,
        dispatch_state: "pending",
      };
    },
    async modelComplete(request) {
      completions.push(request);
      const step = commands[completions.length - 1];
      return step
        ? {
            ...finalModelAnswer(""),
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: `questions-${completions.length}`,
                type: "function",
                function: { name: step[0], arguments: JSON.stringify(step[1]) },
              },
            ],
          }
        : finalModelAnswer("Bound measurement queued.");
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?questions=${Date.now()}`);
    assert.match(
      (
        await main({
          conversation_id: "question-conversation",
          prompt: "Create, revise, discover and plan",
          run_id: "question-run",
          turn_id: "question-turn",
        })
      ).answer,
      /queued/u,
    );
    assert.deepEqual(
      seen.map(([op]) => op),
      ["create", "revise", "discover", "plan"],
    );
    assert.deepEqual(seen.at(-1)[1].bound_measurements[0].question, reference);
    assert.equal(
      JSON.stringify(completions.at(-1).messages).includes(heldout),
      false,
      "the host must never supply heldout text in a discovery or write receipt",
    );
    const schemas = Object.fromEntries(
      completions[0].tools.map(({ function: item }) => [
        item.name,
        item.parameters,
      ]),
    );
    assert.equal(schemas.question_discover.additionalProperties, false);
    assert.equal(schemas.question_create.additionalProperties, false);
    assert.equal(schemas.question_revise.additionalProperties, false);
    assert.equal(
      schemas.channel_plan.properties.bound_measurements.items.properties
        .question.properties.purpose,
      undefined,
    );
    assert.equal(
      schemas.channel_plan.properties.bound_measurements.items.properties
        .question.properties.text,
      undefined,
    );
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("standalone measurement discovers live models, accepts one ad-hoc question and reads durable state", async () => {
  const account = "00000000-0000-4000-8000-000000000221";
  const plan = "00000000-0000-4000-8000-000000000222";
  const target = "00000000-0000-4000-8000-000000000223";
  const commands = [
    ["measurement_options", { account_id: account }],
    [
      "measurement_plan_create",
      {
        account_id: account,
        question: "How do rain gauges work?",
        idempotency_key: "user-request-1",
      },
    ],
    ["measurement_plan_read", { plan_id: plan }],
  ];
  const seen = [];
  const completions = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("unexpected search");
    },
    async measurementOptions(input) {
      seen.push(["options", input]);
      return {
        account_id: account,
        models: [{ id: "observed-model", label: "Observed" }],
        selected_model: "observed-model",
      };
    },
    async measurementPlanCreate(input) {
      seen.push(["create", input]);
      return {
        plan_id: plan,
        target_id: target,
        account_id: account,
        model: "observed-model",
        state: "accepted",
      };
    },
    async measurementPlanRead(input) {
      seen.push(["read", input]);
      return {
        plan_id: plan,
        targets: [
          {
            target_id: target,
            state: "queued",
            outcome_status: null,
            fixture: null,
            received_at: null,
          },
        ],
      };
    },
    async modelComplete(request) {
      completions.push(request);
      const command = commands[completions.length - 1];
      return command
        ? {
            ...finalModelAnswer(""),
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: `measurement-${completions.length}`,
                type: "function",
                function: {
                  name: command[0],
                  arguments: JSON.stringify(command[1]),
                },
              },
            ],
          }
        : finalModelAnswer("The question is queued, not yet measured.");
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?standalone=${Date.now()}`
    );
    const answer = await main({
      conversation_id: "standalone-conversation",
      prompt: "Measure an arbitrary topic",
      run_id: "standalone-run",
      turn_id: "standalone-turn",
    });
    assert.match(answer.answer, /queued/u);
    assert.deepEqual(
      seen.map(([name]) => name),
      ["options", "create", "read"],
    );
    const schemas = Object.fromEntries(
      completions[0].tools.map(({ function: tool }) => [
        tool.name,
        tool.parameters,
      ]),
    );
    for (const name of commands.map(([tool]) => tool)) {
      assert.equal(schemas[name].additionalProperties, false);
    }
    for (const field of [
      "provider",
      "protocol_version",
      "cycle_id",
      "search_mode",
      "purpose",
      "question_set_version",
    ]) {
      assert.equal(
        schemas.measurement_plan_create.properties[field],
        undefined,
      );
    }
    assert.equal(
      seen[1][1].model,
      undefined,
      "Rust chooses the observed default",
    );
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("topic prediction offers real model tools and only a saved question-set receipt supports a link", async () => {
  const requests = [];
  const calls = [];
  const setId = "00000000-0000-4000-8000-000000000501";
  const generated = {
    idempotency_key: "candidate-request-1",
    name: "Suggested questions about home batteries",
    questions: [
      {
        text: "How do home batteries work?",
        intent: "explore",
        product_refs: [],
        market: "CN",
        language: "en",
        source: { kind: "generated" },
        weight: 1,
      },
      {
        text: "How do home batteries compare with portable power stations?",
        intent: "compare",
        product_refs: [],
        market: "CN",
        language: "en",
        source: { kind: "generated" },
        weight: 1,
      },
    ],
  };
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("unexpected knowledge search");
    },
    async questionCreate(command) {
      calls.push(command);
      return {
        question_set_id: setId,
        id: "00000000-0000-4000-8000-000000000502",
        question_count: 2,
        optimization_count: 1,
        evaluation_count: 1,
      };
    },
    async modelComplete(request) {
      requests.push(request);
      return requests.length === 1
        ? {
            ...finalModelAnswer(""),
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: "candidate-create-1",
                type: "function",
                function: {
                  name: "question_create",
                  arguments: JSON.stringify(generated),
                },
              },
            ],
          }
        : finalModelAnswer(`Saved suggestions: ${setId}`);
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?topic=${Date.now()}`);
    const result = await main({
      conversation_id: "candidate-conversation",
      prompt: "Suggest questions from home batteries, market CN, language en",
      run_id: "candidate-run",
      turn_id: "candidate-turn",
    });
    assert.match(requests[0].system, /seed topic/u);
    assert.match(requests[0].system, /question_create/u);
    assert.match(requests[0].system, /not as real user queries/u);
    assert.deepEqual(calls, [generated]);
    assert.match(requests[1].messages.at(-1).content, /question_set_id/u);
    assert.match(result.answer, new RegExp(setId, "u"));
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("a failed candidate-question write never completes as a saved prediction", async () => {
  const events = [];
  let modelCalls = 0;
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit(topic, payload) {
      events.push([topic, payload]);
    },
    async knowledgeSearch() {
      throw new Error("unexpected knowledge search");
    },
    async questionCreate() {
      throw new Error("capability_missing: question sets unavailable");
    },
    async modelComplete() {
      modelCalls++;
      return {
        ...finalModelAnswer(""),
        finish_reason: "tool_calls",
        tool_calls: [
          {
            id: "failed-candidate-create",
            type: "function",
            function: {
              name: "question_create",
              arguments: JSON.stringify({
                idempotency_key: "failed-request",
                name: "Suggested questions",
                questions: [
                  {
                    text: "What is this?",
                    intent: "explore",
                    market: "CN",
                    language: "en",
                    product_refs: [],
                    source: { kind: "generated" },
                    weight: 1,
                  },
                ],
              }),
            },
          },
        ],
      };
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?topicFailure=${Date.now()}`
    );
    await assert.rejects(
      main({
        conversation_id: "failed-candidates",
        prompt: "Suggest questions",
        run_id: "failed-candidates-run",
        turn_id: "failed-candidates-turn",
      }),
      /capability_missing/u,
    );
    assert.equal(modelCalls, 1);
    assert.deepEqual(events, []);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("content start accepts a reference-only current cycle and reads durable coverage", async () => {
  const executionId = "00000000-0000-4000-8000-000000000301";
  const calls = [];
  let modelCall = 0;
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("unexpected search");
    },
    async contentStart(request) {
      calls.push(["start", request]);
      return {
        execution_id: executionId,
        status: "running",
        coverage: {
          total: 2,
          ready: 0,
          blocked: 0,
          deferred: 0,
          not_applicable: 0,
          cancelled: 0,
          incomplete: 2,
        },
      };
    },
    async contentExecutionRead(request) {
      calls.push(["read", request]);
      return {
        execution_id: executionId,
        status: "closed",
        coverage: {
          total: 2,
          ready: 1,
          blocked: 1,
          deferred: 0,
          not_applicable: 0,
          cancelled: 0,
          incomplete: 0,
        },
      };
    },
    async modelComplete(request) {
      const next = modelCall++;
      if (next === 0) {
        const definitions = Object.fromEntries(
          request.tools.map((tool) => [
            tool.function.name,
            tool.function.parameters,
          ]),
        );
        assert.deepEqual(
          definitions.content_start.properties.cycle_id.format,
          "uuid",
        );
        assert.deepEqual(definitions.content_execution_read.required, [
          "execution_id",
        ]);
      }
      return next < 2
        ? {
            text: "",
            tool_calls: [
              {
                id: `content-${next}`,
                type: "function",
                function: {
                  name: next === 0 ? "content_start" : "content_execution_read",
                  arguments: JSON.stringify(
                    next === 0 ? {} : { execution_id: executionId },
                  ),
                },
              },
            ],
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "tool_calls",
          }
        : {
            text: "One ready and one blocked of two planned documents.",
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?content=${Date.now()}`);
    const result = await main({
      conversation_id: "conversation-content",
      prompt: "Start document generation",
      run_id: "run-content",
      turn_id: "turn-content",
    });
    assert.match(result.answer, /one blocked/u);
    assert.deepEqual(calls, [
      ["start", {}],
      ["read", { execution_id: executionId }],
    ]);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("media tools bind only current-turn attachments and return exact host document and draft receipt", async () => {
  const ids = {
    attachment: "00000000-0000-4000-8000-000000000701",
    previousAttachment: "00000000-0000-4000-8000-000000000702",
    binding: "00000000-0000-4000-8000-000000000703",
    execution: "00000000-0000-4000-8000-000000000704",
    item: "00000000-0000-4000-8000-000000000705",
    base: "00000000-0000-4000-8000-000000000706",
    revision: "00000000-0000-4000-8000-000000000707",
    block: "00000000-0000-4000-8000-000000000708",
  };
  const insert = {
    execution_id: ids.execution,
    item_id: ids.item,
    base_revision_id: ids.base,
    binding_id: ids.binding,
    after_block_id: ids.block,
    alt: "An annotated diagram",
    caption: "",
  };
  const receipt = {
    execution_id: ids.execution,
    item_id: ids.item,
    asset_id: "00000000-0000-4000-8000-000000000709",
    base_revision_id: ids.base,
    revision_id: ids.revision,
    block_id: "00000000-0000-4000-8000-000000000710",
    status: "draft",
  };
  const document = {
    execution_id: ids.execution,
    item_id: ids.item,
    revision_id: ids.base,
    current_revision_id: ids.base,
    reused: true,
    document: { version: 2, blocks: [{ id: ids.block, text: "Original" }] },
  };
  const calls = [];
  const requests = [];
  let verifySchema = true;
  const steps = [
    ["content_media_list", { limit: 2 }],
    ["content_media_bind", { attachment_id: ids.attachment }],
    ["content_execution_read", { execution_id: ids.execution }],
    [
      "content_document_read",
      { execution_id: ids.execution, item_id: ids.item },
    ],
    ["content_media_insert", insert],
  ];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected knowledge search");
    },
    async knowledgeImportAttachments() {
      throw new Error("Unexpected knowledge import");
    },
    async contentMediaList(request) {
      calls.push(["list", request]);
      return { items: [{ binding_id: ids.binding }], next_cursor: null };
    },
    async contentMediaBind(request) {
      calls.push(["bind", request]);
      return { binding_id: ids.binding, media_type: "image/png" };
    },
    async contentExecutionRead(request) {
      calls.push(["execution", request]);
      return { execution_id: ids.execution, items: [{ item_id: ids.item }] };
    },
    async contentDocumentRead(request) {
      calls.push(["document", request]);
      return document;
    },
    async contentMediaInsert(request) {
      calls.push(["insert", request]);
      return receipt;
    },
    async modelComplete(request) {
      requests.push(request);
      if (verifySchema) {
        verifySchema = false;
        const schemas = Object.fromEntries(
          request.tools.map((tool) => [
            tool.function.name,
            tool.function.parameters,
          ]),
        );
        for (const name of [
          "content_media_list",
          "content_media_bind",
          "content_document_read",
          "content_media_insert",
        ]) {
          assert.equal(schemas[name].additionalProperties, false);
          assert.equal(schemas[name].properties.project_id, undefined);
          assert.equal(schemas[name].properties.object_key, undefined);
        }
        assert.deepEqual(schemas.content_media_bind.required, [
          "attachment_id",
        ]);
        assert.deepEqual(
          schemas.content_media_bind.properties.attachment_id.enum,
          [ids.attachment],
        );
        assert.equal(schemas.content_media_list.properties.limit.maximum, 25);
        assert.deepEqual(schemas.content_document_read.required, [
          "execution_id",
          "item_id",
        ]);
        assert.deepEqual(
          schemas.content_media_insert.required,
          Object.keys(insert).filter((key) => key !== "after_block_id"),
        );
        assert.match(request.system, /current turn/u);
        assert.match(request.system, /not checked, ready, published/u);
        assert.ok(
          !JSON.stringify(request.tools).includes(ids.previousAttachment),
        );
      }
      const next = steps[requests.length - 1];
      return next
        ? {
            ...finalModelAnswer(""),
            finish_reason: "tool_calls",
            tool_calls: [
              {
                id: `media-${requests.length}`,
                type: "function",
                function: {
                  name: next[0],
                  arguments: JSON.stringify(next[1]),
                },
              },
            ],
          }
        : finalModelAnswer("The draft revision is saved, pending checks.");
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?mediaTools=${Date.now()}`
    );
    const result = await main({
      ...historyTurn,
      prompt: "Insert this attached image in the existing document",
      attachments: [
        {
          attachment_id: ids.attachment,
          object_id: "internal-object-not-model-visible",
          object_version: "internal-version-not-model-visible",
          filename: "diagram.png",
          media_type: "image/png",
          size_bytes: 16,
          sha256: "c".repeat(64),
        },
      ],
    });
    assert.match(result.answer, /pending checks/u);
    assert.deepEqual(calls, [
      ["list", { limit: 2 }],
      ["bind", { attachment_id: ids.attachment }],
      ["execution", { execution_id: ids.execution }],
      ["document", { execution_id: ids.execution, item_id: ids.item }],
      ["insert", insert],
    ]);
    assert.match(requests[4].messages.at(-1).content, /"reused":true/u);
    assert.deepEqual(JSON.parse(requests[5].messages.at(-1).content), receipt);
    assert.ok(
      !JSON.stringify(requests).includes("internal-object-not-model-visible"),
    );
    assert.ok(
      !JSON.stringify(requests).includes("internal-version-not-model-visible"),
    );
    requests.length = 0;
    steps.length = 0;
    await main({
      ...historyTurn,
      message_id: "next-message",
      turn_id: "next-turn",
      run_id: "next-run",
      attachments: [],
    });
    assert.ok(
      requests[0].tools.some(
        ({ function: tool }) => tool.name === "content_media_list",
      ),
    );
    assert.ok(
      !requests[0].tools.some(
        ({ function: tool }) => tool.name === "content_media_bind",
      ),
    );
    requests.length = 0;
    await main({
      ...historyTurn,
      message_id: "another-message",
      turn_id: "another-turn",
      run_id: "another-run",
      attachments: [
        {
          attachment_id: ids.previousAttachment,
          filename: "different-image.png",
          media_type: "image/png",
        },
      ],
    });
    const bindSchema = requests[0].tools.find(
      ({ function: tool }) => tool.name === "content_media_bind",
    ).function.parameters;
    assert.deepEqual(bindSchema.properties.attachment_id.enum, [
      ids.previousAttachment,
    ]);
    assert.ok(!JSON.stringify(bindSchema).includes(ids.attachment));
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("media insertion conflict fails without choosing a new base or completing the turn", async () => {
  const calls = [];
  const events = [];
  let completions = 0;
  const command = {
    execution_id: "00000000-0000-4000-8000-000000000711",
    item_id: "00000000-0000-4000-8000-000000000712",
    base_revision_id: "00000000-0000-4000-8000-000000000713",
    binding_id: "00000000-0000-4000-8000-000000000714",
    alt: "Diagram",
    caption: "",
  };
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit(...event) {
      events.push(event);
    },
    async knowledgeSearch() {
      throw new Error("Unexpected knowledge search");
    },
    async contentMediaInsert(request) {
      calls.push(request);
      throw new Error("revision_conflict: read current document");
    },
    async modelComplete() {
      completions++;
      return {
        ...finalModelAnswer(""),
        finish_reason: "tool_calls",
        tool_calls: [
          {
            id: "media-conflict",
            type: "function",
            function: {
              name: "content_media_insert",
              arguments: JSON.stringify(command),
            },
          },
        ],
      };
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?mediaConflict=${Date.now()}`
    );
    await assert.rejects(main(historyTurn), /revision_conflict/u);
    assert.deepEqual(calls, [command]);
    assert.equal(completions, 1);
    assert.deepEqual(events, []);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("formal distribution tools pass only scoped selectors and expose a paged coverage matrix", async () => {
  const manifestId = "00000000-0000-4000-8000-000000000101";
  const calls = [];
  let next = 0;
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async distributionStart(request) {
      calls.push(["start", request]);
      return {
        manifest_id: manifestId,
        expected_count: 3,
        expansion_cursor: 3,
        complete: true,
      };
    },
    async distributionRead(request) {
      calls.push(["read", request]);
      return { manifest_id: manifestId, expected_count: 3, complete: true };
    },
    async distributionTargetsRead(request) {
      calls.push(["targets", request]);
      return {
        manifest_id: manifestId,
        expected_count: 3,
        items: [
          { ordinal: 0, status: "deferred", reason: "connector_unverified" },
        ],
        next_ordinal: 0,
      };
    },
    async distributionResume(request) {
      calls.push(["resume", request]);
      return { manifest_id: manifestId, expected_count: 3, complete: true };
    },
    async modelComplete(request) {
      const definitions = Object.fromEntries(
        request.tools.map((tool) => [
          tool.function.name,
          tool.function.parameters,
        ]),
      );
      for (const name of [
        "distribution_start",
        "distribution_read",
        "distribution_resume",
        "distribution_targets_read",
      ]) {
        assert.equal(definitions[name].additionalProperties, false);
      }
      assert.deepEqual(
        definitions.distribution_start.properties.cycle_id.format,
        "uuid",
      );
      assert.deepEqual(definitions.distribution_read.required, undefined);
      assert.deepEqual(definitions.distribution_resume.required, [
        "manifest_id",
      ]);
      assert.deepEqual(definitions.distribution_targets_read.required, [
        "manifest_id",
      ]);
      const names = [
        "distribution_start",
        "distribution_read",
        "distribution_targets_read",
        "distribution_resume",
      ];
      const requests = [
        {},
        { manifest_id: manifestId },
        { manifest_id: manifestId, limit: 1 },
        { manifest_id: manifestId, after_ordinal: 0 },
      ];
      const index = next++;
      return index < names.length
        ? {
            text: "",
            tool_calls: [
              {
                id: `distribution-${index}`,
                type: "function",
                function: {
                  name: names[index],
                  arguments: JSON.stringify(requests[index]),
                },
              },
            ],
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "tool_calls",
          }
        : {
            text: "Manifest has three planned cells; one is deferred, not published.",
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?distribution=${Date.now()}`
    );
    const result = await main({
      conversation_id: "conversation-distribution",
      prompt: "Start and review the second-stage coverage matrix",
      run_id: "run-distribution",
      turn_id: "turn-distribution",
    });
    assert.match(result.answer, /not published/u);
    assert.deepEqual(calls, [
      ["start", {}],
      ["read", { manifest_id: manifestId }],
      ["targets", { manifest_id: manifestId, limit: 1 }],
      ["resume", { manifest_id: manifestId, after_ordinal: 0 }],
    ]);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("single-article distribution pins revision/account/key and reports only accepted status", async () => {
  const ids = [
    "00000000-0000-4000-8000-000000000101",
    "00000000-0000-4000-8000-000000000102",
    "00000000-0000-4000-8000-000000000103",
    "00000000-0000-4000-8000-000000000104",
  ];
  const command = {
    content_asset_id: ids[0],
    content_revision_id: ids[1],
    account_id: ids[2],
    placement_slot: "article",
    format: "markdown.v1",
    idempotency_key: "same-retry-key",
  };
  let completions = 0;
  const writes = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async contentDistributeRequest(request) {
      writes.push(request);
      return {
        request_id: ids[3],
        status: "accepted",
        publication_intent_id: null,
      };
    },
    async contentDistributeRead(request) {
      assert.deepEqual(request, { request_id: ids[3] });
      return {
        request_id: ids[3],
        status: "accepted",
        publication_intent_id: null,
      };
    },
    async modelComplete(request) {
      const schemas = Object.fromEntries(
        request.tools.map((tool) => [
          tool.function.name,
          tool.function.parameters,
        ]),
      );
      assert.deepEqual(
        schemas.content_distribute_request.required,
        Object.keys(command),
      );
      assert.equal(
        schemas.content_distribute_request.additionalProperties,
        false,
      );
      assert.deepEqual(
        schemas.content_distribute_request.properties.format.enum,
        ["markdown.v1", "rich_markdown.v2"],
      );
      assert.equal(
        schemas.content_distribute_request.properties.tenant_id,
        undefined,
      );
      assert.equal(
        schemas.content_distribute_request.properties.project_id,
        undefined,
      );
      assert.deepEqual(schemas.content_distribute_read.required, [
        "request_id",
      ]);
      const index = completions++;
      const tool = ["content_distribute_request", "content_distribute_read"][
        index
      ];
      return index < 2
        ? {
            text: "",
            tool_calls: [
              {
                id: `single-article-${index}`,
                type: "function",
                function: {
                  name: tool,
                  arguments: JSON.stringify(
                    index === 0 ? command : { request_id: ids[3] },
                  ),
                },
              },
            ],
            model: "stub-model",
            prompt_tokens: 1,
            completion_tokens: 1,
            finish_reason: "tool_calls",
          }
        : finalModelAnswer("Request accepted, not published.");
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?singleArticle=${Date.now()}`
    );
    const result = await main({
      conversation_id: "single-article",
      prompt: "Distribute this specific revision",
      run_id: "run-single-article",
      turn_id: "turn-single-article",
    });
    assert.match(result.answer, /accepted, not published/u);
    assert.deepEqual(writes, [command]);
    assert.equal(completions, 3);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("unknown execution is returned without retry or invented publication success", async () => {
  const targetId = "00000000-0000-4000-8000-000000000111";
  let executions = 0;
  let modelCalls = 0;
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async channelTargetExecute(request) {
      executions++;
      assert.deepEqual(request, { target_id: targetId });
      return { target_id: targetId, state: "unknown_result" };
    },
    async modelComplete(request) {
      modelCalls++;
      if (modelCalls === 1) {
        return {
          text: "",
          tool_calls: [
            {
              id: "unknown-1",
              type: "function",
              function: {
                name: "channel_target_execute",
                arguments: JSON.stringify({ target_id: targetId }),
              },
            },
          ],
          model: "stub-model",
          prompt_tokens: 7,
          completion_tokens: 4,
          finish_reason: "tool_calls",
        };
      }
      assert.match(request.messages.at(-1).content, /unknown_result/u);
      return {
        text: "The result is unknown and needs reconciliation; do not resend.",
        model: "stub-model",
        prompt_tokens: 7,
        completion_tokens: 4,
        finish_reason: "stop",
      };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?unknown=${Date.now()}`);
    const result = await main({
      conversation_id: "conversation-channel-unknown",
      prompt: "Check this target",
      run_id: "run-channel-unknown",
      turn_id: "turn-channel-unknown",
    });
    assert.equal(executions, 1);
    assert.match(result.answer, /unknown and needs reconciliation/u);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("replayed plan returns the same frozen receipt and completed fixture remains explicitly a fixture", async () => {
  const targetId = "00000000-0000-4000-8000-000000000121";
  const accountId = "00000000-0000-4000-8000-000000000122";
  const plan = {
    publications: [
      {
        source_id: "00000000-0000-4000-8000-000000000123",
        source_version_id: "00000000-0000-4000-8000-000000000124",
        platform: "public-platform",
        account_id: accountId,
      },
    ],
    measurements: [],
  };
  const receipt = {
    plan_id: "00000000-0000-4000-8000-000000000125",
    cycle_id: "00000000-0000-4000-8000-000000000126",
    revision: 1,
    expected_count: 1,
    dispatch_state: "pending",
  };
  const plans = [];
  const messages = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit() {},
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async channelPlan(request) {
      plans.push(request);
      return receipt;
    },
    async channelTargetExecute(request) {
      assert.deepEqual(request, { target_id: targetId });
      return {
        target_id: targetId,
        state: "completed",
        outcome_status: "failed",
        fixture: true,
      };
    },
    async modelComplete(request) {
      const index = messages.length;
      messages.push(request.messages);
      const calls = [
        ["channel_plan", plan],
        ["channel_plan", plan],
        ["channel_target_execute", { target_id: targetId }],
      ];
      return index < calls.length
        ? {
            text: "",
            tool_calls: [
              {
                id: `replay-${index}`,
                type: "function",
                function: {
                  name: calls[index][0],
                  arguments: JSON.stringify(calls[index][1]),
                },
              },
            ],
            model: "stub-model",
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "tool_calls",
          }
        : {
            text: "The plan was already frozen; the completed fixture records a failed outcome, not a successful publication.",
            model: "stub-model",
            prompt_tokens: 7,
            completion_tokens: 4,
            finish_reason: "stop",
          };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?replay=${Date.now()}`);
    const result = await main({
      conversation_id: "conversation-channel-replay",
      prompt: "Recheck the plan and target outcome",
      run_id: "run-channel-replay",
      turn_id: "turn-channel-replay",
    });
    assert.deepEqual(plans, [plan, plan]);
    assert.match(messages[2].at(-1).content, /"dispatch_state":"pending"/u);
    assert.match(messages[3].at(-1).content, /"outcome_status":"failed"/u);
    assert.match(messages[3].at(-1).content, /"fixture":true/u);
    assert.match(result.answer, /not a successful publication/u);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("a channel capability failure aborts the loop without a completion event", async () => {
  const emitted = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit(topic, payload) {
      emitted.push({ topic, payload });
    },
    async knowledgeSearch() {
      throw new Error("Unexpected search");
    },
    async channelPlan() {
      throw new Error("capability_missing: channel planner unavailable");
    },
    async modelComplete() {
      return {
        text: "",
        tool_calls: [
          {
            id: "channel-error",
            type: "function",
            function: {
              name: "channel_plan",
              arguments: '{"publications":[],"measurements":[]}',
            },
          },
        ],
        model: "stub-model",
        prompt_tokens: 7,
        completion_tokens: 4,
        finish_reason: "tool_calls",
      };
    },
  };
  try {
    const { main } = await import(
      `${bundlePath.href}?channelError=${Date.now()}`
    );
    await assert.rejects(
      main({
        conversation_id: "conversation-channel-error",
        prompt: "Plan the channel",
        run_id: "run-channel-error",
        turn_id: "turn-channel-error",
      }),
      /capability_missing/u,
    );
    assert.equal(emitted.length, 0);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("a missing knowledge capability fails without a success completion", async () => {
  const emitted = [];
  globalThis.__GEO_AGENT_TEST_HOST__ = {
    async emit(topic, payload) {
      emitted.push({ topic, payload });
    },
    async knowledgeSearch() {
      throw new Error("capability_missing: knowledge retrieval is unavailable");
    },
    async modelComplete() {
      return {
        text: "",
        tool_calls: [
          {
            id: "missing-1",
            type: "function",
            function: {
              name: "knowledge_search",
              arguments: '{"query":"warranty"}',
            },
          },
        ],
        model: "stub-model",
        prompt_tokens: 7,
        completion_tokens: 4,
        finish_reason: "tool_calls",
      };
    },
  };
  try {
    const { main } = await import(`${bundlePath.href}?missing=${Date.now()}`);
    await assert.rejects(
      main({
        conversation_id: "conversation-missing-tool-0001",
        prompt: "What is the warranty?",
        run_id: "run-missing-tool-0001",
        timestamp: 1_700_000_000_000,
        turn_id: "turn-missing-tool-0001",
      }),
      /capability_missing/u,
    );
    assert.equal(emitted.length, 0);
  } finally {
    delete globalThis.__GEO_AGENT_TEST_HOST__;
  }
});

test("the generated bundle is self-contained ESM with no static Node builtin import", async () => {
  const source = await readFile(bundlePath, "utf8");
  assert.match(source, /createAgentToolLoopRunner/u);
  assert.doesNotMatch(source, /(?:from\s*|import\s*\(\s*)["']node:/u);
});
