import assert from "node:assert/strict";
import test from "node:test";
import {
  capturedNewConversationId,
  capturedConversationCompletion,
  reportCapturedConversation,
} from "../src/provider-conversation-ownership.mjs";

const completedExchange = () => ({
  connect_json_terminal: true,
  messages: [
    { chat: { id: "new-chat" } },
    {
      message: {
        id: "answer-1",
        chat_id: "new-chat",
        role: "assistant",
        status: "GENERATING",
      },
    },
    { message: { id: "answer-1", status: "COMPLETED" } },
  ],
});

test("completion uses final observed statuses for every owned assistant message", () => {
  const exchange = completedExchange();
  exchange.messages.push({
    message: { id: "answer-2", chat_id: "new-chat", role: 3, status: 2 },
  });
  assert.deepEqual(capturedConversationCompletion(exchange), {
    protocol: "connect_json",
    terminal: true,
    assistant_message_ids: ["answer-1", "answer-2"],
  });
  exchange.messages.push({ message: { id: "answer-1", text: "tail" } });
  assert.equal(capturedConversationCompletion(exchange)?.terminal, true);
});

test("legacy, ambiguous, incomplete and unknown statuses never prove completion", () => {
  for (const change of [
    (exchange) => delete exchange.connect_json_terminal,
    (exchange) => (exchange.connect_json_terminal = false),
    (exchange) => (exchange.connect_json_terminal = "true"),
    (exchange) => (exchange.messages = [{ chat: { id: "new-chat" } }]),
    (exchange) => delete exchange.messages[1].message.chat_id,
    (exchange) => (exchange.messages[1].message.chat_id = "other-chat"),
    (exchange) => delete exchange.messages[1].message.role,
    (exchange) => delete exchange.messages[1].message.id,
    (exchange) => (exchange.messages[1].message.role = "future-role"),
    (exchange) => (exchange.messages[2].message.role = "user"),
    (exchange) => {
      delete exchange.messages[1].message.status;
      delete exchange.messages[2].message.status;
    },
    (exchange) => exchange.messages.push({ error: { code: "fixture-error" } }),
    (exchange) =>
      exchange.messages.push({
        message: { id: "answer-2", role: "assistant", chat_id: "new-chat" },
      }),
    ...[
      "GENERATING",
      "UNSPECIFIED",
      "completed",
      "SUCCESS",
      "2",
      0,
      1,
      9,
      null,
    ].map(
      (status) => (exchange) =>
        exchange.messages.push({ message: { id: "answer-1", status } }),
    ),
  ]) {
    const exchange = completedExchange();
    change(exchange);
    assert.equal(capturedConversationCompletion(exchange), null);
  }
});

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
