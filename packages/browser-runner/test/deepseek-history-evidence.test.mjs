import assert from "node:assert/strict";
import test from "node:test";
import {
  isDeepSeekHistoryExplicitlyAbsent,
  verifyDeepSeekFirstTurnHistory,
  verifyDeepSeekEmptyHistory,
} from "../src/deepseek-history-evidence.mjs";

const expected = {
  conversationId: "synthetic-session",
  requestMessageId: "0",
  responseMessageId: "1",
};
const inventory = {
  conversationId: expected.conversationId,
  messages: [
    { id: "0", role: "user" },
    { id: "1", role: "assistant" },
  ],
};
function fixture() {
  return {
    code: 0,
    data: {
      biz_code: 0,
      biz_data: {
        chat_session: {
          id: expected.conversationId,
          title: "synthetic-discarded-metadata",
        },
        cache_control: "REPLACE",
        cache_reset_at: 123,
        chat_messages: [
          {
            message_id: 0,
            parent_id: null,
            role: "USER",
            status: "FINISHED",
            fragments: [{ type: "REQUEST", content: "synthetic-question" }],
            has_pending_fragment: false,
          },
          {
            message_id: 1,
            parent_id: 0,
            role: "ASSISTANT",
            status: "FINISHED",
            quasi_status: "FINISHED",
            fragments: [{ type: "RESPONSE", content: "synthetic-answer" }],
            has_pending_fragment: false,
          },
        ],
      },
    },
  };
}
const envelope = (value = fixture()) => ({
  status: 200,
  body: JSON.stringify(value),
});
const verify = (value, binding = expected) =>
  verifyDeepSeekFirstTurnHistory(envelope(value), binding);
const history = (value) => value.data.biz_data;
const messages = (value) => history(value).chat_messages;

test("empty inventory requires exact complete history and proves no first-turn completion", () => {
  const value = fixture();
  history(value).chat_messages = [];
  history(value).chat_session.current_message_id = null;
  assert.deepEqual(
    verifyDeepSeekEmptyHistory(envelope(value), expected.conversationId),
    {
      conversationId: expected.conversationId,
      messages: [],
    },
  );
  assert.equal(verify(value), null);
  for (const mutate of [
    (v) => {
      v.code = 1;
    },
    (v) => {
      v.data.biz_code = 1;
    },
    (v) => {
      history(v).chat_session.id = "other-session";
    },
    (v) => {
      delete history(v).chat_session.current_message_id;
    },
    (v) => {
      history(v).chat_session.current_message_id = 0;
    },
    (v) => {
      history(v).cache_control = "APPEND";
    },
    (v) => {
      history(v).chat_messages = [messages(fixture())[0]];
    },
  ]) {
    const invalid = structuredClone(value);
    mutate(invalid);
    assert.equal(
      verifyDeepSeekEmptyHistory(envelope(invalid), expected.conversationId),
      null,
    );
  }
  assert.equal(
    verifyDeepSeekEmptyHistory(
      { ...envelope(value), status: 404 },
      expected.conversationId,
    ),
    null,
  );
});

test("returns only canonical first-turn inventory, independent of response order", () => {
  const value = fixture();
  assert.deepEqual(verify(value), inventory);
  messages(value).reverse();
  assert.deepEqual(verify(value), inventory);
  assert.deepEqual(
    verify(value, { ...expected, requestMessageId: 0, responseMessageId: 1 }),
    inventory,
  );
  assert.equal(
    JSON.stringify(verify(value)).includes("synthetic-answer"),
    false,
  );
});

test("does not parse fragment text, citations or cache-reset metadata", () => {
  const value = fixture();
  messages(value)[0].fragments = [];
  messages(value)[1].fragments = [
    { content: '{"status":"WIP","message_id":999}', citations: "opaque" },
  ];
  delete history(value).cache_reset_at;
  assert.deepEqual(verify(value), inventory);
});

test("a non-null current message must identify the expected response", () => {
  const value = fixture();
  history(value).chat_session.current_message_id = null;
  assert.deepEqual(verify(value), inventory);
  history(value).chat_session.current_message_id = 1;
  assert.deepEqual(verify(value), inventory);
  for (const id of [0, 2, "1", false, -1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
    history(value).chat_session.current_message_id = id;
    assert.equal(verify(value), null);
  }
  history(value).chat_session.current_message_id = 1;
  for (const literal of ["1.0", "1e0"]) {
    const body = envelope(value).body.replace(
      '"current_message_id":1',
      `"current_message_id":${literal}`,
    );
    assert.equal(
      verifyDeepSeekFirstTurnHistory({ status: 200, body }, expected),
      null,
    );
  }
});

test("requires exact session and successful full replacement envelope", () => {
  for (const change of [
    (v) => (v.code = 1),
    (v) => (v.code = "0"),
    (v) => (v.data.biz_code = 1),
    (v) => (v.data.biz_code = 5),
    (v) => (v.data.biz_code = "0"),
    (v) => delete v.data.biz_code,
    (v) => (v.data = []),
    (v) => (v.data.biz_data = []),
    (v) => (history(v).chat_session = null),
    (v) => (history(v).chat_session.id = "synthetic-other"),
    (v) => (history(v).cache_control = "MERGE"),
    (v) => delete history(v).cache_control,
  ]) {
    const value = fixture();
    change(value);
    assert.equal(verify(value), null);
  }
});

test("rejects malformed, extra, missing and duplicate inventory nodes", () => {
  for (const change of [
    (v) => (history(v).chat_messages = null),
    (v) => (history(v).chat_messages = {}),
    (v) => (history(v).chat_messages = []),
    (v) => messages(v).pop(),
    (v) => messages(v).push({ ...messages(v)[1], message_id: 2 }),
    (v) => (messages(v)[1] = null),
    (v) => (messages(v)[1] = []),
    (v) => (messages(v)[1] = { ...messages(v)[0] }),
    (v) => (messages(v)[1].message_id = 2),
  ]) {
    const value = fixture();
    change(value);
    assert.equal(verify(value), null);
  }
});

test("rejects wrong roles and all unexpected parent branches", () => {
  for (const change of [
    (v) => (messages(v)[0].role = "ASSISTANT"),
    (v) => (messages(v)[0].role = "user"),
    (v) => (messages(v)[1].role = "SYSTEM"),
    (v) => delete messages(v)[1].role,
    (v) => (messages(v)[0].parent_id = 1),
    (v) => delete messages(v)[0].parent_id,
    (v) => (messages(v)[1].parent_id = null),
    (v) => (messages(v)[1].parent_id = 1),
    (v) => (messages(v)[1].parent_id = 99),
    (v) => (messages(v)[1].parent_id = "0"),
    (v) => delete messages(v)[1].parent_id,
  ]) {
    const value = fixture();
    change(value);
    assert.equal(verify(value), null);
  }
});

test("only actual FINISHED status proves completion for either node", () => {
  for (const status of [
    "WIP",
    "CONTENT_FILTER",
    "INCOMPLETE",
    "CONTEXT_LENGTH_EXCEEDED",
    "TIMEOUT",
    "finished",
    null,
    undefined,
    0,
  ]) {
    for (const index of [0, 1]) {
      const value = fixture();
      messages(value)[index].status = status;
      messages(value)[index].quasi_status = "FINISHED";
      assert.equal(verify(value), null);
    }
  }
});

test("quasi status never overrides completion and pending must be explicit false", () => {
  for (const index of [0, 1]) {
    for (const quasi of [undefined, null, "FINISHED"]) {
      const value = fixture();
      messages(value)[index].quasi_status = quasi;
      assert.deepEqual(verify(value), inventory);
    }
    for (const quasi of ["WIP", "TIMEOUT", "UNKNOWN", "", 0, false]) {
      const value = fixture();
      messages(value)[index].quasi_status = quasi;
      assert.equal(verify(value), null);
    }
    for (const pending of [undefined, null, true, 0, "false"]) {
      const value = fixture();
      messages(value)[index].has_pending_fragment = pending;
      assert.equal(verify(value), null);
    }
    for (const fragments of [undefined, null, {}, "answer"]) {
      const value = fixture();
      messages(value)[index].fragments = fragments;
      assert.equal(verify(value), null);
    }
  }
});

test("rejects unsafe and nonnumeric wire IDs, accepts maximum safe integer", () => {
  for (const id of [
    "1",
    -1,
    0.5,
    Number.MAX_SAFE_INTEGER + 1,
    null,
    {},
    true,
  ]) {
    const value = fixture();
    messages(value)[1].message_id = id;
    assert.equal(verify(value), null);
  }
  const value = fixture();
  messages(value)[1].message_id = Number.MAX_SAFE_INTEGER;
  assert.deepEqual(
    verify(value, { ...expected, responseMessageId: Number.MAX_SAFE_INTEGER }),
    {
      ...inventory,
      messages: [
        inventory.messages[0],
        { id: "9007199254740991", role: "assistant" },
      ],
    },
  );
  for (const literal of ["-0", "1.0", "1e0", "9007199254740990.9"]) {
    const body = envelope().body.replace(
      '"message_id":1',
      `"message_id":${literal}`,
    );
    assert.equal(
      verifyDeepSeekFirstTurnHistory(
        { status: 200, body },
        { ...expected, responseMessageId: String(JSON.parse(literal)) },
      ),
      null,
    );
  }
});

test("requires exact bounded canonical expected IDs", () => {
  for (const binding of [
    undefined,
    null,
    [],
    {},
    { ...expected, conversationId: "" },
    { ...expected, conversationId: "invalid id" },
    { ...expected, conversationId: "x".repeat(129) },
    { ...expected, requestMessageId: "00" },
    { ...expected, requestMessageId: -0 },
    { ...expected, responseMessageId: "1e0" },
    { ...expected, responseMessageId: " 1" },
    { ...expected, responseMessageId: "1.0" },
    { ...expected, responseMessageId: "9007199254740992" },
    { ...expected, responseMessageId: "0" },
  ])
    assert.equal(verifyDeepSeekFirstTurnHistory(envelope(), binding), null);
});

test("bounds raw UTF-8 bytes without retaining response data", () => {
  const body = envelope().body;
  const padded = body + " ".repeat(750_000 - Buffer.byteLength(body));
  assert.deepEqual(
    verifyDeepSeekFirstTurnHistory({ status: 200, body: padded }, expected),
    inventory,
  );
  const value = fixture();
  history(value).chat_session.title = "界".repeat(250_000);
  for (const invalid of [
    { status: 200, body: padded + " " },
    envelope(value),
  ]) {
    assert.equal(verifyDeepSeekFirstTurnHistory(invalid, expected), null);
    assert.equal(isDeepSeekHistoryExplicitlyAbsent(invalid), false);
  }
});

test("malformed bodies and non-200 HTTP statuses fail closed", () => {
  for (const invalid of [
    undefined,
    null,
    {},
    { status: "200", body: envelope().body },
    ...[201, 204, 302, 401, 403, 404, 500].map((status) => ({
      status,
      body: envelope().body,
    })),
    ...[
      "",
      "{",
      "null",
      "[]",
      '{"code":0,"data":null}',
      fixture(),
      Buffer.from("{}"),
    ].map((body) => ({ status: 200, body })),
  ]) {
    assert.equal(verifyDeepSeekFirstTurnHistory(invalid, expected), null);
    assert.equal(isDeepSeekHistoryExplicitlyAbsent(invalid), false);
  }
});

test("absence is only HTTP 200, top-level code 0, business code 1", () => {
  const absent = { code: 0, data: { biz_code: 1 } };
  assert.equal(isDeepSeekHistoryExplicitlyAbsent(envelope(absent)), true);
  assert.equal(verify(absent), null);
  for (const value of [
    fixture(),
    { code: 1, data: { biz_code: 1 } },
    { code: "0", data: { biz_code: 1 } },
    { code: 0, data: { biz_code: "1" } },
    { code: 0, data: { biz_code: 5 } },
    { code: 0, data: {} },
    { code: 0, data: { biz_code: 0, biz_data: {} } },
    { code: 0, data: { biz_code: 0, biz_data: { chat_messages: [] } } },
  ])
    assert.equal(isDeepSeekHistoryExplicitlyAbsent(envelope(value)), false);
  for (const status of [404, 403, 500])
    assert.equal(
      isDeepSeekHistoryExplicitlyAbsent({ ...envelope(absent), status }),
      false,
    );
});

test("existing rejection branches report fixed reasons without changing null or success", () => {
  for (const [change, reason] of [
    [
      (v) => {
        v.status = 403;
      },
      "response_envelope_unverified",
    ],
    [
      (v) => {
        v.body = "{";
      },
      "history_envelope_unverified",
    ],
    [
      (v) => {
        const body = fixture();
        body.data.biz_code = 5;
        v.body = JSON.stringify(body);
      },
      "history_envelope_unverified",
    ],
    [
      (v) => {
        const body = fixture();
        history(body).chat_session.id = "other-synthetic-session";
        v.body = JSON.stringify(body);
      },
      "history_binding_mismatch",
    ],
    [
      (v) => {
        const body = fixture();
        history(body).chat_session.current_message_id = 3;
        v.body = JSON.stringify(body);
      },
      "history_binding_mismatch",
    ],
    [
      (v) => {
        const body = fixture();
        history(body).cache_control = "APPEND";
        v.body = JSON.stringify(body);
      },
      "inventory_unverified",
    ],
    [
      (v) => {
        const body = fixture();
        messages(body).pop();
        v.body = JSON.stringify(body);
      },
      "inventory_unverified",
    ],
    [
      (v) => {
        v.body = v.body.replace('"message_id":1', '"message_id":1e0');
      },
      "inventory_unverified",
    ],
    [
      (v) => {
        const body = fixture();
        messages(body)[1].parent_id = 3;
        v.body = JSON.stringify(body);
      },
      "inventory_unverified",
    ],
    [
      (v) => {
        const body = fixture();
        messages(body)[1].status = "WIP";
        v.body = JSON.stringify(body);
      },
      "completion_unverified",
    ],
    [
      (v) => {
        const body = fixture();
        messages(body)[1].has_pending_fragment = true;
        v.body = JSON.stringify(body);
      },
      "completion_unverified",
    ],
  ]) {
    const value = envelope();
    change(value);
    const reasons = [];
    assert.equal(
      verifyDeepSeekFirstTurnHistory(value, expected, {
        onFailureReason: (value) => reasons.push(value),
      }),
      null,
    );
    assert.deepEqual(reasons, [reason]);
    assert.equal(verifyDeepSeekFirstTurnHistory(value, expected), null);
  }
  assert.deepEqual(
    verifyDeepSeekFirstTurnHistory(envelope(), expected, {
      onFailureReason: () =>
        assert.fail("successful inventory has no failure reason"),
    }),
    inventory,
  );
  assert.equal(
    verifyDeepSeekFirstTurnHistory(null, expected, {
      onFailureReason: () => {
        throw new Error("synthetic callback failure");
      },
    }),
    null,
  );
});
