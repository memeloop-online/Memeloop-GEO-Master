import assert from "node:assert/strict";
import test from "node:test";
import {
  capturedNewConversationId,
  capturedConversationCompletion,
  reportCapturedConversation,
} from "../src/provider-conversation-ownership.mjs";

const kimi = Object.freeze({ provider: "kimi" });

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
  assert.deepEqual(capturedConversationCompletion(exchange, kimi), {
    protocol: "connect_json",
    terminal: true,
    assistant_message_ids: ["answer-1", "answer-2"],
  });
  exchange.messages.push({ message: { id: "answer-1", text: "tail" } });
  assert.equal(capturedConversationCompletion(exchange, kimi)?.terminal, true);
});

test("legacy, ambiguous, incomplete and unknown statuses never prove completion", () => {
  for (const change of [
    (exchange) => delete exchange.connect_json_terminal,
    (exchange) => (exchange.connect_json_terminal = false),
    (exchange) => (exchange.connect_json_terminal = "true"),
    (exchange) => (exchange.messages = [{ chat: { id: "new-chat" } }]),
    (exchange) =>
      ([exchange.messages[0], exchange.messages[1]] = [
        exchange.messages[1],
        exchange.messages[0],
      ]),
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
      "MESSAGE_STATUS_GENERATING",
      "MESSAGE_STATUS_UNKNOWN",
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
    assert.equal(capturedConversationCompletion(exchange, kimi), null);
  }
});

test("preceding chat scopes system, user and incremental assistant lifecycle envelopes", () => {
  const exchange = {
    connect_json_terminal: true,
    messages: [
      { chat: { id: "synthetic-chat" } },
      {
        message: {
          id: "synthetic-system",
          role: "system",
          status: "MESSAGE_STATUS_COMPLETED",
        },
      },
      {
        message: {
          id: "synthetic-user",
          role: "user",
          status: "MESSAGE_STATUS_COMPLETED",
        },
      },
      {
        message: {
          id: "synthetic-assistant",
          role: "assistant",
          status: "MESSAGE_STATUS_GENERATING",
        },
      },
      { message: { id: "synthetic-assistant", refs: {} } },
      {
        message: {
          id: "synthetic-assistant",
          status: "MESSAGE_STATUS_COMPLETED",
        },
      },
    ],
  };
  assert.deepEqual(capturedConversationCompletion(exchange, kimi), {
    protocol: "connect_json",
    terminal: true,
    assistant_message_ids: ["synthetic-assistant"],
  });
  for (const change of [
    (source) => source.messages.splice(0, 1),
    (source) => source.messages.push(source.messages.shift()),
    (source) => (source.messages[2].message.chat_id = "other-chat"),
    (source) => (source.messages[1].message.role = 1),
    (source) =>
      (source.messages[5].message.status = "MESSAGE_STATUS_GENERATING"),
    (source) => (source.messages[5].message.role = "user"),
  ]) {
    const invalid = structuredClone(exchange);
    change(invalid);
    assert.equal(capturedConversationCompletion(invalid, kimi), null);
  }
});

test("completion metadata stays bounded even with oversized structural fixtures", () => {
  const exchange = completedExchange();
  for (let index = 2; index <= 257; index++)
    exchange.messages.push({
      message: {
        id: `answer-${index}`,
        role: "assistant",
        status: "COMPLETED",
      },
    });
  assert.equal(capturedConversationCompletion(exchange, kimi), null);
});

test("only a consistent transport chat envelope identifies the new conversation", () => {
  const exchange = {
    messages: [
      { message: { chat_id: "new-chat" } },
      { chat: { id: "new-chat" } },
    ],
  };
  assert.equal(capturedNewConversationId(exchange, kimi), "new-chat");
  for (const messages of [
    [{ message: { chat_id: "new-chat" } }],
    [{ chat: { id: "new-chat" } }, { message: { chat_id: "other-chat" } }],
    [{ chat: { id: "new-chat" } }, { chat: { id: "other-chat" } }],
    [{ chat: { id: "../unsafe" } }],
    [{ chat: { id: "new-chat" } }, { error: { code: "bad" } }],
    [{ chat: { id: "new-chat" }, message: { chat_id: "new-chat" } }],
    [{ chat: {} }],
  ])
    assert.equal(capturedNewConversationId({ messages }, kimi), null);
});

test("unknown capture never invokes the optional ownership hook", async () => {
  const receipts = [];
  await reportCapturedConversation(
    { messages: [{ message: { chat_id: "unverified" } }] },
    "measurement",
    (receipt) => receipts.push(receipt),
    kimi,
  );
  assert.deepEqual(receipts, []);
});

test("only an explicit durable ledger acknowledgement marks ownership receipt persisted", async () => {
  const exchange = { messages: [{ chat: { id: "new-chat" } }] };
  assert.equal(
    await reportCapturedConversation(
      exchange,
      "measurement",
      async () => {},
      kimi,
    ),
    "unpersisted",
  );
  assert.equal(
    await reportCapturedConversation(
      exchange,
      "measurement",
      async () => ({ durable: true }),
      kimi,
    ),
    "persisted",
  );
  assert.equal(
    await reportCapturedConversation(exchange, "measurement", undefined, kimi),
    "untracked",
  );
});

test("only explicit registered providers can identify or prove captured conversations", async () => {
  const exchange = {
    ...completedExchange(),
    provider: "kimi",
    completion: {
      protocol: "connect_json",
      terminal: true,
      assistant_message_ids: ["invented"],
    },
  };
  for (const provider of [
    undefined,
    null,
    "",
    "Kimi",
    "deepseek",
    "doubao",
    "glm",
    "__proto__",
    "constructor",
    "toString",
    {},
    ["kimi"],
  ]) {
    const context = { provider };
    assert.equal(capturedNewConversationId(exchange, context), null);
    assert.equal(capturedConversationCompletion(exchange, context), null);
    assert.equal(
      await reportCapturedConversation(
        exchange,
        "measurement",
        () => {
          assert.fail("unregistered provider must not invoke ownership hook");
        },
        context,
      ),
      "unidentified",
    );
  }
  assert.equal(capturedNewConversationId(exchange), null);
  assert.equal(capturedConversationCompletion(exchange), null);
  assert.equal(
    await reportCapturedConversation(exchange, "measurement", undefined),
    "untracked",
  );
  exchange.provider = "deepseek";
  const receipts = [];
  assert.equal(
    await reportCapturedConversation(
      exchange,
      "measurement",
      async (receipt) => {
        receipts.push(receipt);
        return { durable: true };
      },
      kimi,
    ),
    "persisted",
  );
  assert.deepEqual(receipts, [
    {
      provider: "kimi",
      purpose: "measurement",
      external_conversation_id: "new-chat",
    },
  ]);
  assert.deepEqual(capturedConversationCompletion(exchange, kimi), {
    protocol: "connect_json",
    terminal: true,
    assistant_message_ids: ["answer-1"],
  });
  assert.equal(
    await reportCapturedConversation(
      exchange,
      "extraction",
      async () => {
        throw new Error("synthetic private callback failure");
      },
      kimi,
    ),
    "unpersisted",
  );
});
