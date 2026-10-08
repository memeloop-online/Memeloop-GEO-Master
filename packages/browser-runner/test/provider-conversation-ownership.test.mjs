import assert from "node:assert/strict";
import test from "node:test";
import {
  capturedNewConversationId,
  reportCapturedConversation,
} from "../src/provider-conversation-ownership.mjs";

test("only a consistent transport chat envelope identifies the new conversation", () => {
  const exchange = {
    messages: [
      { message: { chat_id: "new-chat" } },
      { chat: { id: "new-chat" } },
    ],
  };
  assert.equal(capturedNewConversationId(exchange), "new-chat");
  for (const messages of [
    [{ message: { chat_id: "new-chat" } }],
    [{ chat: { id: "new-chat" } }, { message: { chat_id: "other-chat" } }],
    [{ chat: { id: "new-chat" } }, { chat: { id: "other-chat" } }],
    [{ chat: { id: "../unsafe" } }],
    [{ chat: { id: "new-chat" } }, { error: { code: "bad" } }],
    [{ chat: { id: "new-chat" }, message: { chat_id: "new-chat" } }],
    [{ chat: {} }],
  ])
    assert.equal(capturedNewConversationId({ messages }), null);
});

test("unknown capture never invokes the optional ownership hook", async () => {
  const receipts = [];
  await reportCapturedConversation(
    { messages: [{ message: { chat_id: "unverified" } }] },
    "measurement",
    (receipt) => receipts.push(receipt),
  );
  assert.deepEqual(receipts, []);
});

test("only an explicit durable ledger acknowledgement marks ownership receipt persisted", async () => {
  const exchange = { messages: [{ chat: { id: "new-chat" } }] };
  assert.equal(
    await reportCapturedConversation(exchange, "measurement", async () => {}),
    "unpersisted",
  );
  assert.equal(
    await reportCapturedConversation(exchange, "measurement", async () => ({
      durable: true,
    })),
    "persisted",
  );
  assert.equal(
    await reportCapturedConversation(exchange, "measurement", undefined),
    "untracked",
  );
});
