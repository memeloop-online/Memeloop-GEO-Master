import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const bundlePath = new URL(
  "../dist/memeloop-agent-loop.bundle.mjs",
  import.meta.url,
);

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
    });

    assert.equal(requests.length, 1);
    assert.match(requests[0].prompt, /user: How long is the warranty\?/u);
    assert.equal(requests[0].model, undefined);
    assert.deepEqual(completion, {
      answer: "The warranty lasts two years.",
      conversation_id: "conversation-smoke-0001",
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
