import assert from "node:assert/strict";
import { test } from "node:test";
import { providerCleanupAdapter } from "../src/provider-cleanup-registry.mjs";
import {
  recoverKimiConversation,
  deleteKimiConversation,
} from "../src/provider-conversation-cleanup.mjs";
import {
  recoverDeepSeekConversation,
  deleteDeepSeekConversation,
} from "../src/deepseek-conversation-cleanup.mjs";

test("cleanup registry selects exact installed provider adapters", () => {
  assert.equal(providerCleanupAdapter("kimi").recover, recoverKimiConversation);
  assert.equal(providerCleanupAdapter("kimi").delete, deleteKimiConversation);
  assert.equal(
    providerCleanupAdapter("deepseek").recover,
    recoverDeepSeekConversation,
  );
  assert.equal(
    providerCleanupAdapter("deepseek").delete,
    deleteDeepSeekConversation,
  );
  assert.ok(Object.isFrozen(providerCleanupAdapter("kimi")));
  assert.ok(Object.isFrozen(providerCleanupAdapter("deepseek")));
});

test("unknown and inherited names cannot borrow a cleanup adapter", () => {
  for (const provider of [
    undefined,
    null,
    {},
    ["kimi"],
    "",
    "Kimi",
    "DeepSeek",
    "unknown",
    "__proto__",
    "constructor",
    "toString",
  ])
    assert.equal(providerCleanupAdapter(provider), null);
});
