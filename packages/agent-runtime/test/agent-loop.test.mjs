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
        "knowledge_import_status",
        "report_get",
        "report_preview",
        "report_reduce",
        "channel_discover",
        "channel_plan",
        "channel_manifest_read",
        "channel_target_execute",
        "question_discover",
        "question_create",
        "question_revise",
        "content_start",
        "content_execution_read",
        "distribution_start",
        "distribution_read",
        "distribution_resume",
        "distribution_targets_read",
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
      "cycle_id",
    ]);
    assert.equal(previewTool.function.parameters.additionalProperties, false);
    assert.match(previewTool.function.description, /temporary|unsaved/u);
    assert.match(requests[1].messages.at(-1).content, /"kind":"preview"/u);
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
        "report_get",
        "report_preview",
        "report_reduce",
        "channel_discover",
        "channel_plan",
        "channel_manifest_read",
        "channel_target_execute",
        "question_discover",
        "question_create",
        "question_revise",
        "content_start",
        "content_execution_read",
        "distribution_start",
        "distribution_read",
        "distribution_resume",
        "distribution_targets_read",
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
