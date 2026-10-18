import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { createRunner } from "../src/runner.mjs";

const sourceJson = JSON.stringify({
  messages: [{ text: "Synthetic source." }],
});
const sourceSha256 = createHash("sha256").update(sourceJson).digest("hex");

async function runCapture(platform, receipts, completion, extraction = {}) {
  const stored = [];
  const runner = createRunner({
    browserType: {
      async launch() {
        return {
          async newContext() {
            return {
              async newPage() {
                return { url: () => "https://example.org/", async goto() {} };
              },
              async storageState() {
                return { cookies: [], origins: [] };
              },
              async close() {},
            };
          },
          async close() {},
        };
      },
    },
    platformAdapters: {
      [platform]: {
        connectorVersion: "fixture.provider-binding.v1",
        entry: "https://example.org/",
        operations: ["measure"],
        async identify() {
          return {
            platform_account_id: "fixture-account",
            display_name: "Synthetic account",
          };
        },
        async execute(_page, _operation, _payload, hooks) {
          for (const receipt of receipts)
            assert.deepEqual(hooks.onConversationCaptured(receipt), {
              durable: false,
            });
          for (const phase of ["source", "extraction"]) {
            await hooks.onEvidence({
              kind: "observation_capture",
              schema_version: "geo.observation.capture.v1",
              phase,
              source_json: sourceJson,
              source_sha256: sourceSha256,
              ...(completion ? { completion } : {}),
            });
            if (phase === "source") await extraction.inspectHooks?.(hooks);
          }
          return { status: "completed", evidence: [] };
        },
      },
    },
  });
  try {
    await runner.create({
      session_id: "fixture-session",
      platform,
      storage_state: { cookies: [], origins: [] },
    });
    await runner.complete("fixture-session");
    const input = {
      execution_id: "fixture-execution",
      session_id: "fixture-session",
      operation: "measure",
      payload: {},
      source_capture_ticket: "aabb",
    };
    const persist = async (body) => {
      stored.push(body);
      return {
        schema_version: 1,
        capture_id: "33333333-3333-4333-8333-333333333333",
        digest_sha256: sourceSha256,
        stored_at: "2026-01-01T00:00:00Z",
      };
    };
    const result = await runner.execute(
      input,
      persist,
      extraction.observationAi,
    );
    assert.equal(result.status, "completed");
    await runner.execute(input, persist);
    assert.equal(stored.length, 2, "same execution must not be resubmitted");
    return stored;
  } finally {
    await runner.shutdown();
  }
}

test("extraction forwards cancellation but no synchronous wait budget", async (t) => {
  const controller = new AbortController();
  let calls = 0;
  t.mock.method(performance, "now", () => 1_000);
  const stored = await runCapture("kimi", [], undefined, {
    async inspectHooks(hooks) {
      const pending = hooks.apiExtract("must not cross callback boundary", {
        deadlineAt: 57_001.75,
        signal: controller.signal,
      });
      controller.abort();
      assert.equal(await pending, null);
      assert.equal(
        await hooks.apiExtract("expired", { deadlineAt: 999 }),
        null,
      );
    },
    observationAi: {
      async extract(body, { signal }) {
        calls++;
        assert.deepEqual(body, {
          schema_version: 1,
          prompt_version: "geo.observation.extract.v4",
          capture_ticket: "aabb",
          source_capture_id: "33333333-3333-4333-8333-333333333333",
          source_sha256: sourceSha256,
        });
        assert.equal(signal, controller.signal);
        await new Promise((resolve) =>
          signal.addEventListener("abort", resolve, { once: true }),
        );
        assert.equal(signal.aborted, true);
        return null;
      },
    },
  });
  assert.equal(calls, 1);
  assert.equal(stored.length, 2);
  assert.ok(stored.every((body) => !Object.hasOwn(body, "wait_budget_ms")));
});

test("extraction accepts only the durable typed ACK, never legacy model output", async () => {
  const intent_id = "99999999-9999-4999-8999-999999999999";
  const accepted = { status: "accepted", intent_id };
  let result;
  await runCapture("kimi", [], undefined, {
    async inspectHooks(hooks) {
      for (const malformed of [
        null,
        {},
        { status: "accepted" },
        { status: "accepted", intent_id: 123 },
        { status: "accepted", intent_id: "invalid" },
        { status: "completed", intent_id },
        { ...accepted, text: "{}" },
        { text: "{}", model: "fixture-model", config_version: 1 },
      ]) {
        result = malformed;
        assert.equal(await hooks.apiExtract("not sent"), null);
      }
      result = accepted;
      assert.deepEqual(await hooks.apiExtract("not sent"), {
        status: "extraction_pending",
        intent_id,
      });
    },
    observationAi: {
      async extract(body) {
        assert.equal(Object.hasOwn(body, "wait_budget_ms"), false);
        return result;
      },
    },
  });
});

test("source and extraction callbacks forward the same normalized inventory receipt", async () => {
  const completion = {
    outcome: "complete",
    message_inventory: [
      ["answer", "assistant"],
      ["question", "user"],
    ],
  };
  const stored = await runCapture(
    "kimi",
    [
      {
        provider: "kimi",
        purpose: "measurement",
        external_conversation_id: "selected-source",
      },
      {
        provider: "kimi",
        purpose: "extraction",
        external_conversation_id: "selected-extraction",
      },
    ],
    completion,
  );
  assert.deepEqual(
    stored.map((record) => record.completion),
    [completion, completion],
  );
});

for (const platform of ["kimi", "deepseek"]) {
  test(`${platform} measurement ownership binds to the session platform and browser extraction stays Kimi`, async () => {
    const receipts = [
      {
        provider: platform,
        purpose: "measurement",
        external_conversation_id: "selected-source",
      },
      {
        provider: "kimi",
        purpose: "extraction",
        external_conversation_id: "selected-extraction",
      },
      {
        provider: platform === "kimi" ? "deepseek" : "kimi",
        purpose: "measurement",
        external_conversation_id: "wrong-source",
      },
      {
        provider: "deepseek",
        purpose: "extraction",
        external_conversation_id: "wrong-extraction",
      },
    ];
    const stored = await runCapture(platform, receipts);
    assert.deepEqual(stored[0].owned_conversation, {
      provider: platform,
      purpose: "measurement",
      external_conversation_id: "selected-source",
      correlation: "create_response",
    });
    assert.deepEqual(stored[1].owned_conversation, {
      provider: "kimi",
      purpose: "extraction",
      external_conversation_id: "selected-extraction",
      correlation: "create_response",
    });
  });

  test(`${platform} rejects ownership claimed only by another provider`, async () => {
    const stored = await runCapture(platform, [
      {
        provider: platform === "kimi" ? "deepseek" : "kimi",
        purpose: "measurement",
        external_conversation_id: "wrong-source",
      },
      {
        provider: "deepseek",
        purpose: "extraction",
        external_conversation_id: "wrong-extraction",
      },
    ]);
    assert.equal(stored[0].owned_conversation, undefined);
    assert.equal(stored[1].owned_conversation, undefined);
  });
}
