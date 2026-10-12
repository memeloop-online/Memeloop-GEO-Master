import assert from "node:assert/strict";
import test from "node:test";
import { projectDeepseekSource } from "../src/deepseek-source-projection.mjs";

const event = (data) => ({ event: "message", data });
const snapshot = (fragments) =>
  event({ v: { response: { fragments, status: "WIP" } } });
const answer = (content = "Public answer.") => ({ type: "RESPONSE", content });
const think = (content = "private-thought") => ({ type: "THINK", content });
const project = (...messages) => projectDeepseekSource({ messages });

test("retains answer and search raw fields without semantic extraction", () => {
  const messages = [
    {
      event: "ready",
      data: {
        response_message_id: 2,
        request_message_id: 1,
        model_type: "test",
      },
    },
    snapshot([
      answer(),
      {
        type: "SEARCH",
        queries: ["Synthetic query"],
        results: [{ title: "Source", url: "https://example.org/source" }],
      },
    ]),
    event({ p: "response/status", o: "SET", v: "FINISHED" }),
    { event: "finish", raw_data: "opaque-terminal-payload" },
  ];
  const source = projectDeepseekSource({
    messages,
    received_at: "2026-01-01T00:00:00Z",
    sse_terminal: true,
    unrelated_raw_body: "must-not-copy",
  });
  assert.deepEqual(source.exchange.messages.slice(0, 3), messages.slice(0, 3));
  assert.deepEqual(source.exchange.messages[3], { event: "finish" });
  assert.equal(source.exchange.sse_terminal, true);
  assert.equal(source.exchange.received_at, "2026-01-01T00:00:00Z");
  assert.equal(source.exchange.unrelated_raw_body, undefined);
  assert.deepEqual(source.redaction.omitted_paths, ["/messages/3/raw_data"]);
  assert.equal(source.exchange.answer, undefined);
  assert.equal(source.exchange.citations, undefined);
  assert.equal(source.exchange.completion, undefined);
});

test("removes every THINK field, including unknown payload keys", () => {
  const input = snapshot([
    { type: "THINK", content: "secret-a", future_field: { text: "secret-b" } },
    answer(),
  ]);
  const original = structuredClone(input);
  const source = project(input);
  assert.deepEqual(source.exchange.messages[0].data.v.response.fragments, [
    { type: "THINK" },
    answer(),
  ]);
  assert.deepEqual(source.redaction.omitted_paths, [
    "/messages/0/data/v/response/fragments/0/content",
    "/messages/0/data/v/response/fragments/0/future_field",
  ]);
  assert.deepEqual(input, original);
  assert.doesNotMatch(JSON.stringify(source), /secret-/u);
});

test("resolves independent inherited paths and operations without adding fields", () => {
  const source = project(
    snapshot([think(), answer()]),
    event({ p: "response/fragments/0/content", o: "APPEND", v: "secret-a" }),
    event({ v: "secret-b" }),
    event({ p: "response/fragments/1/content", v: " continued" }),
    event({ v: " again" }),
  );
  assert.deepEqual(source.exchange.messages[1].data, {
    p: "response/fragments/0/content",
    o: "APPEND",
  });
  assert.deepEqual(source.exchange.messages[2].data, {});
  assert.deepEqual(source.exchange.messages[3].data, {
    p: "response/fragments/1/content",
    v: " continued",
  });
  assert.deepEqual(source.exchange.messages[4].data, { v: " again" });
  assert.doesNotMatch(JSON.stringify(source), /secret-/u);
});

test("nested BATCH inherits among siblings with a fresh scope per batch", () => {
  const batch = {
    p: "response",
    o: "BATCH",
    v: [
      { p: "fragments/0", o: "BATCH", v: [{ p: "content", v: "secret-a" }] },
      { v: [{ p: "content", v: "secret-b" }] },
      {
        p: "fragments/1",
        v: [{ p: "content", o: "APPEND", v: " A" }, { v: " B" }],
      },
    ],
  };
  const source = project(
    snapshot([think(), answer()]),
    event(batch),
    // Parent parser still inherits BATCH and response, not the child scope.
    event({ v: [{ p: "status", v: "FINISHED" }] }),
  );
  assert.ok(source);
  const output = source.exchange.messages[1].data;
  assert.deepEqual(output.v[0].v[0], { p: "content" });
  assert.deepEqual(output.v[1].v[0], { p: "content" });
  assert.deepEqual(output.v[2], batch.v[2]);
  assert.deepEqual(source.exchange.messages[2].data, {
    v: [{ p: "status", v: "FINISHED" }],
  });
  assert.doesNotMatch(JSON.stringify(source), /secret-/u);
});

test("BATCH child operation does not persist into a subsequent batch", () => {
  const source = project(
    snapshot([answer()]),
    event({
      p: "response/fragments",
      o: "BATCH",
      v: [{ p: "", o: "APPEND", v: [think()] }],
    }),
    event({ v: [{ p: "0", v: answer("Replacement") }] }),
    event({ p: "response/fragments/1/content", o: "APPEND", v: "secret" }),
  );
  assert.ok(source);
  assert.deepEqual(source.exchange.messages[3].data, {
    p: "response/fragments/1/content",
    o: "APPEND",
  });
});

test("array insertions shift the privacy index and preserve event positions", () => {
  const source = project(
    snapshot([answer(), think()]),
    event({ p: "response/fragments/0", o: "APPEND", v: [think("inserted")] }),
    event({ p: "response/fragments/1/content", v: "public continuation" }),
    event({ p: "response/fragments/2/content", v: "private continuation" }),
  );
  assert.ok(source);
  assert.equal(source.exchange.messages.length, 4);
  assert.equal(source.exchange.messages[2].data.v, "public continuation");
  assert.equal(Object.hasOwn(source.exchange.messages[3].data, "v"), false);
});

test("negative indices and splice clamping match fragment privacy ordering", () => {
  const source = project(
    snapshot([answer(), think()]),
    event({ p: "response/fragments/-1/content", o: "APPEND", v: "secret-a" }),
    event({ p: "response/fragments/-1", v: [answer("Inserted")] }),
    event({ p: "response/fragments/-1/content", v: "secret-b" }),
    event({ p: "response/fragments/-2/content", v: "safe" }),
    event({ p: "response/fragments/-99", v: [think("secret-c")] }),
    event({ p: "response/fragments/0/content", v: "secret-d" }),
  );
  assert.ok(source);
  assert.equal(source.exchange.messages[4].data.v, "safe");
  assert.doesNotMatch(JSON.stringify(source), /secret-/u);
});

test("element replacement, append and whole-array replacement reset types", () => {
  const source = project(
    snapshot([think()]),
    event({ p: "response/fragments/-1", o: "SET", v: answer() }),
    event({ p: "response/fragments/0/content", o: "APPEND", v: "safe" }),
    event({ p: "response/fragments", v: [think("secret-a")] }),
    event({ p: "response/fragments/-1/unknown_field", v: "secret-b" }),
    event({ p: "response/fragments", o: "SET", v: [think("secret-c")] }),
    event({ p: "response/fragments/0/content", o: "APPEND", v: "secret-d" }),
  );
  assert.ok(source);
  assert.equal(source.exchange.messages[2].data.v, "safe");
  assert.doesNotMatch(JSON.stringify(source), /secret-/u);
});

test("privacy key removal applies to explicit paths, nested data and inheritance", () => {
  const source = project(
    snapshot([
      { ...answer(), authorization: "secret-a", reasoning: "secret-b" },
    ]),
    event({ p: "response/reasoning_content", o: "APPEND", v: "secret-c" }),
    event({ v: "secret-d" }),
    event({ p: "response/access_token", v: "secret-e" }),
  );
  assert.ok(source);
  assert.doesNotMatch(JSON.stringify(source), /secret-/u);
  assert.deepEqual(source.exchange.messages[2].data, {});
});

test("typed reasoning nested in otherwise retained values is removed", () => {
  const source = project(
    snapshot([{ ...answer(), metadata: { type: "THINK", novel: "secret" } }]),
  );
  assert.ok(source);
  assert.deepEqual(
    source.exchange.messages[0].data.v.response.fragments[0].metadata,
    { type: "THINK" },
  );
});

for (const [name, updates] of [
  [
    "direct nested content append",
    [
      event({
        p: "response/fragments/0/metadata/content",
        o: "APPEND",
        v: "private-later",
      }),
    ],
  ],
  [
    "inherited nested content append",
    [
      event({ p: "response/fragments/0/metadata/content", o: "APPEND", v: "" }),
      event({ v: "private-later" }),
    ],
  ],
  [
    "nested BATCH content append",
    [
      event({
        p: "response/fragments",
        o: "BATCH",
        v: [
          {
            p: "0/metadata",
            o: "BATCH",
            v: [{ p: "content", o: "APPEND", v: "" }, { v: "private-later" }],
          },
        ],
      }),
    ],
  ],
  [
    "nested discriminator transition",
    [
      event({
        p: "response/fragments/0/metadata/type",
        o: "SET",
        v: "RESPONSE",
      }),
      event({
        p: "response/fragments/0/metadata/content",
        o: "APPEND",
        v: "private-later",
      }),
    ],
  ],
  [
    "unverified direct metadata append",
    [
      event({
        p: "response/fragments/0/metadata",
        o: "APPEND",
        v: "private-later",
      }),
    ],
  ],
  [
    "nested negative index after insertion",
    [
      event({
        p: "response/fragments/0",
        o: "APPEND",
        v: [answer("Inserted")],
      }),
      event({
        p: "response/fragments/-1/metadata/content",
        v: "private-later",
      }),
    ],
  ],
]) {
  test(`rejects ${name} rather than losing nested privacy provenance`, () => {
    assert.equal(
      project(
        snapshot([
          {
            ...answer(),
            metadata: { type: "THINK", content: "private-initial" },
          },
        ]),
        ...updates,
      ),
      null,
    );
  });
}

test("rejects nested privacy updates outside the fragment array", () => {
  assert.equal(
    project(
      event({ p: "response", v: { fragments: [answer()], feedback: think() } }),
      event({
        p: "response/feedback/content",
        o: "APPEND",
        v: "private-later",
      }),
    ),
    null,
  );
});

test("rejects object-valued content before a later direct content append", () => {
  assert.equal(
    project(
      snapshot([{ type: "RESPONSE", content: think() }]),
      event({
        p: "response/fragments/0/content",
        o: "APPEND",
        v: "private-later",
      }),
    ),
    null,
  );
});

test("rejects a direct content type transition and nested search-result mutation", () => {
  assert.equal(
    project(
      snapshot([answer()]),
      event({ p: "response/fragments/0/content", v: think() }),
      event({ o: "APPEND", v: "private-later" }),
    ),
    null,
  );
  assert.equal(
    project(
      snapshot([{ type: "SEARCH", results: [think()] }]),
      event({ p: "response/fragments/0/results/0/type", v: "RESPONSE" }),
      event({
        p: "response/fragments/0/results/0/content",
        o: "APPEND",
        v: "private-later",
      }),
    ),
    null,
  );
});

test("retains proven direct content and whole search-field updates unchanged", () => {
  const messages = [
    snapshot([answer(), { type: "SEARCH", results: [], queries: [] }]),
    event({ p: "response/fragments/0/content", o: "APPEND", v: " More." }),
    event({
      p: "response/fragments/1/results",
      v: [{ url: "https://example.org/source", title: "Source" }],
    }),
    event({ p: "response/fragments/1/queries", v: ["Synthetic query"] }),
    event({ p: "response/fragments/1/status", o: "SET", v: "FINISHED" }),
  ];
  assert.deepEqual(project(...messages).exchange.messages, messages);
});

for (const [name, messages] of [
  [
    "unknown fragment type",
    [snapshot([{ type: "FUTURE", content: "unknown" }])],
  ],
  ["untyped fragment", [snapshot([{ content: "unknown" }])]],
  [
    "missing fragment provenance",
    [event({ p: "response/fragments/0/content", v: "unknown" })],
  ],
  [
    "out of range fragment",
    [
      snapshot([answer()]),
      event({ p: "response/fragments/9/content", v: "unknown" }),
    ],
  ],
  ["unsupported operation", [event({ p: "response", o: "MERGE", v: {} })]],
  ["unknown root path", [event({ p: "future", v: "unknown" })]],
  ["unknown response field", [event({ p: "response/future", v: "unknown" })]],
  ["unknown event", [{ event: "debug", data: { content: "unknown" } }]],
  ["opaque delta", [{ event: "message", raw_data: "unknown" }]],
  ["malformed batch", [event({ o: "BATCH", v: {} })]],
  [
    "discriminator mutation",
    [
      snapshot([answer()]),
      event({ p: "response/fragments/0/type", v: "THINK" }),
    ],
  ],
  [
    "snapshot reset cannot reuse index",
    [
      snapshot([answer()]),
      event({ p: "response", v: { status: "WIP" } }),
      event({ p: "response/fragments/0/content", v: "unknown" }),
    ],
  ],
]) {
  test(`rejects ${name} without returning partial source`, () => {
    assert.equal(project(...messages), null);
  });
}

test("bounds depth, message count, total bytes and circular inputs", () => {
  let nested = { p: "content", v: "text" };
  for (let index = 0; index < 40; index++) nested = { o: "BATCH", v: [nested] };
  assert.equal(project(event(nested)), null);
  assert.equal(project(snapshot([answer("x".repeat(750_001))])), null);
  assert.equal(
    projectDeepseekSource({ messages: Array(10_001).fill({}) }),
    null,
  );
  const circular = {};
  circular.self = circular;
  assert.equal(project(circular), null);
});
