import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { createRunner } from "../src/runner.mjs";

const sourceJson = JSON.stringify({
  messages: [{ text: "Synthetic source." }],
});
const sourceSha256 = createHash("sha256").update(sourceJson).digest("hex");

async function runCapture(platform, receipts) {
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
          for (const phase of ["source", "extraction"])
            await hooks.onEvidence({
              kind: "observation_capture",
              schema_version: "geo.observation.capture.v1",
              phase,
              source_json: sourceJson,
              source_sha256: sourceSha256,
            });
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
    const result = await runner.execute(input, persist);
    assert.equal(result.status, "completed");
    await runner.execute(input, persist);
    assert.equal(stored.length, 2, "same execution must not be resubmitted");
    return stored;
  } finally {
    await runner.shutdown();
  }
}

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
