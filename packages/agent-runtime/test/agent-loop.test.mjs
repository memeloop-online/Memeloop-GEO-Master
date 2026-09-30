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

test("the generated bundle is self-contained ESM with no static Node builtin import", async () => {
  const source = await readFile(bundlePath, "utf8");
  assert.match(source, /createAgentToolLoopRunner/u);
  assert.doesNotMatch(source, /(?:from\s*|import\s*\(\s*)["']node:/u);
});
