import assert from "node:assert/strict";
import test from "node:test";
import { projectDeepseekSource } from "../src/deepseek-source-projection.mjs";
import { createDeepSeekEvidenceDecoder } from "../src/deepseek-web-measurement.mjs";
import { reportObservationDiagnostic } from "../src/ai-observation-parser.mjs";
import { sourceRedaction } from "../src/observation-source-redaction.mjs";

const event = (data) => ({ event: "message", data });
const snapshot = (fragments) =>
  event({ v: { response: { fragments, status: "WIP" } } });
const answer = (content = "Public answer.") => ({ type: "RESPONSE", content });
const think = (content = "private-thought") => ({ type: "THINK", content });
const project = (...messages) => projectDeepseekSource({ messages });

test("rejection diagnostics contain only fixed codes and cannot change null outcomes", () => {
  const throwing = {
    get messages() {
      throw new Error("synthetic-private-exception");
    },
  };
  for (const [input, code] of [
    [null, "shape_rejected"],
    [throwing, "shape_rejected"],
    [
      { messages: [{ event: "synthetic-private-event" }] },
      "source_event_rejected",
    ],
    [
      {
        messages: [
          event({ p: "synthetic-private-path", v: "synthetic-private-value" }),
        ],
      },
      "path_rejected",
    ],
    [
      {
        messages: [{ event: "finish", id: { secret: "synthetic-private-id" } }],
      },
      "identifier_rejected",
    ],
  ]) {
    const diagnostics = [];
    assert.equal(
      projectDeepseekSource(input, (...args) => diagnostics.push(args)),
      null,
    );
    assert.deepEqual(diagnostics, [[code]]);
    assert.equal(
      projectDeepseekSource(input, () => {
        throw new Error("synthetic-private-callback");
      }),
      null,
    );
  }
});

test("structural rejection categories cross only the closed diagnostic wire", () => {
  for (const [messages, code] of [
    [
      [event({ v: { "synthetic-private-key": "synthetic-private-value" } })],
      "source_root_rejected",
    ],
    [
      [
        event({
          p: "response",
          v: "synthetic-private-value",
        }),
      ],
      "source_response_rejected",
    ],
    [
      [
        {
          event: "ready",
          data: { "synthetic-private-key": "synthetic-private-value" },
        },
      ],
      "source_event_rejected",
    ],
    [
      [{ event: "finish", "synthetic-private-key": "synthetic-private-value" }],
      "source_event_rejected",
    ],
    [
      [event({ v: {}, "synthetic-private-key": "synthetic-private-value" })],
      "source_delta_rejected",
    ],
    [[snapshot([null])], "source_fragment_rejected"],
    [
      [
        snapshot([answer()]),
        event({
          p: "response/fragments/0",
          v: "synthetic-private-value",
        }),
      ],
      "source_fragment_rejected",
    ],
    [
      [
        snapshot([answer()]),
        event({ p: "response/fragments/0/content", v: {} }),
      ],
      "source_fragment_rejected",
    ],
    [[event({ o: "BATCH", v: {} })], "source_delta_rejected"],
  ]) {
    const diagnostics = [];
    const input = { messages };
    const before = structuredClone(input);
    assert.equal(
      projectDeepseekSource(input, (rejection) =>
        reportObservationDiagnostic(
          (record) => {
            diagnostics.push(record);
            throw new Error("synthetic-private-callback");
          },
          "capture",
          rejection,
        ),
      ),
      null,
    );
    assert.deepEqual(input, before);
    assert.deepEqual(diagnostics, [
      {
        kind: "observation_diagnostic",
        schema_version: "geo.observation.diagnostic.v1",
        stage: "capture",
        code,
      },
    ]);
    assert.equal(
      JSON.stringify(diagnostics).includes("synthetic-private"),
      false,
    );
    reportObservationDiagnostic(
      (record) => diagnostics.push(record),
      "capture",
      `${code}:synthetic-private-detail`,
    );
    assert.equal(diagnostics.length, 1);
  }
});

test("omits unknown response snapshot fields wholly with valid exact pointers", () => {
  for (const root of [true, false]) {
    const response = {
      status: "WIP",
      message_id: 2,
      future_metadata: {
        public_looking: "synthetic-private-value",
        credentials: { token: "synthetic-private-token" },
        nested: { type: "THINK", novel: "synthetic-private-thought" },
        fragments: [{ type: "UNREVIEWED", content: {} }],
      },
      "future/~metadata": "synthetic-private-string",
      authorization: "synthetic-private-authorization",
      reasoning_content: "synthetic-private-reasoning",
      fragments: [answer(), think("synthetic-private-fragment")],
      feedback: { status: "known", email: "synthetic-private-email" },
    };
    const input = {
      messages: [
        root
          ? event({ v: { response } })
          : event({ p: "response", v: response }),
        event({ p: "response/fragments/0/content", o: "APPEND", v: " More." }),
        event({ v: " Again." }),
        event({
          p: "response/fragments/1/content",
          v: "synthetic-private-delta",
        }),
        event({ v: "synthetic-private-inherited" }),
      ],
    };
    const before = structuredClone(input);
    const diagnostics = [];
    const source = projectDeepseekSource(input, (code) =>
      diagnostics.push(code),
    );
    assert.ok(source);
    const at = `/messages/0/data/v${root ? "/response" : ""}`;
    const projected = source.exchange.messages[0].data.v;
    assert.deepEqual(root ? projected.response : projected, {
      status: "WIP",
      message_id: 2,
      fragments: [answer(), { type: "THINK" }],
      feedback: { status: "known" },
    });
    assert.deepEqual(
      source.exchange.messages.slice(1, 3),
      input.messages.slice(1, 3),
    );
    assert.deepEqual(source.exchange.messages[3].data, {
      p: "response/fragments/1/content",
    });
    assert.deepEqual(source.exchange.messages[4].data, {});
    assert.deepEqual(source.redaction.omitted_paths, [
      `${at}/future_metadata`,
      `${at}/future~1~0metadata`,
      `${at}/fragments/1/content`,
      `${at}/authorization`,
      `${at}/reasoning_content`,
      `${at}/feedback/email`,
      "/messages/3/data/v",
      "/messages/4/data/v",
    ]);
    assert.deepEqual(
      sourceRedaction(source.redaction, source.exchange),
      source.redaction,
    );
    assert.doesNotMatch(JSON.stringify(source), /synthetic-private/u);
    assert.deepEqual(diagnostics, []);
    assert.deepEqual(input, before);
  }
});

test("nonobject response snapshots retain the fixed rejection diagnostic", () => {
  for (const value of [null, [], "synthetic-private-value", 1, true]) {
    for (const message of [
      event({ v: { response: value } }),
      event({ p: "response", v: value }),
    ]) {
      const diagnostics = [];
      assert.equal(
        projectDeepseekSource({ messages: [message] }, (code) =>
          diagnostics.push(code),
        ),
        null,
      );
      assert.deepEqual(diagnostics, ["source_response_rejected"]);
    }
  }
});

test("valid projection and empty capture compatibility are unchanged with diagnostics", () => {
  for (const input of [
    { messages: [snapshot([answer(), think()])] },
    { messages: [] },
  ]) {
    const diagnostics = [];
    assert.deepEqual(
      projectDeepseekSource(input, (code) => diagnostics.push(code)),
      projectDeepseekSource(input),
    );
    assert.deepEqual(diagnostics, []);
  }
});

test("projects actual decoder transport IDs without interpreting auxiliary content", () => {
  const decoder = createDeepSeekEvidenceDecoder({ model: "test" });
  decoder.push(
    Buffer.from(
      "id: synthetic-transport\n" +
        'event: ready\ndata: {"request_message_id":1,"response_message_id":2}\n\n' +
        'event: debug\ndata: {"opaque":"synthetic-private-value"}\n\n',
    ),
  );
  assert.throws(() => decoder.finish());
  const source = projectDeepseekSource(decoder.snapshot());
  assert.ok(source);
  assert.equal(source.exchange.sse_terminal, false);
  assert.deepEqual(source.exchange.messages, [
    { event: "ready", data: { request_message_id: 1, response_message_id: 2 } },
    { event: "debug" },
  ]);
  assert.deepEqual(source.redaction.omitted_paths, [
    "/messages/0/id",
    "/messages/1/data",
  ]);
});

test("omits transport IDs and opaque auxiliary payloads without changing delta state", () => {
  const input = [
    snapshot([answer("First")]),
    event({ p: "response/fragments/0/content", o: "APPEND", v: " second" }),
    ...["debug", "update_parent_message", "update_file"].map((name) => ({
      event: name,
      id: "synthetic-private-transport-id",
      data: {
        signed_path: "synthetic-private-path",
        chat_message: { type: "THINK", content: "synthetic-private-thought" },
      },
    })),
    { ...event({ v: " third" }), id: "synthetic-id" },
  ];
  const before = structuredClone(input);
  const source = project(...input);
  assert.ok(source);
  assert.deepEqual(source.exchange.messages.slice(0, 2), input.slice(0, 2));
  assert.deepEqual(source.exchange.messages.slice(2, 5), [
    { event: "debug" },
    { event: "update_parent_message" },
    { event: "update_file" },
  ]);
  assert.deepEqual(source.exchange.messages[5], event({ v: " third" }));
  assert.deepEqual(source.redaction.omitted_paths, [
    "/messages/2/id",
    "/messages/2/data",
    "/messages/3/id",
    "/messages/3/data",
    "/messages/4/id",
    "/messages/4/data",
    "/messages/5/id",
  ]);
  assert.equal(JSON.stringify(source).includes("synthetic-private"), false);
  assert.deepEqual(input, before);
});

test("auxiliary omission does not accept unknown events, fields, or opaque wire data", () => {
  for (const message of [
    { event: "unknown", data: {} },
    { event: "debug", data: {}, extra: "private" },
    { event: "debug", raw_data: "private" },
    { event: "debug", data: {}, id: {} },
  ]) {
    assert.equal(project(message), null);
  }
});

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
  ["nonobject fragment", [snapshot(["unknown"])]],
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
  ["unknown event", [{ event: "unrecognized", data: { content: "unknown" } }]],
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

test("snapshot omissions preserve positions and exact manifests without retaining unknown values", () => {
  const input = snapshot([
    answer(),
    { type: "FUTURE", content: "private-unknown", "future/~key": think() },
    { content: "private-untyped" },
    {
      ...answer(),
      content: think(),
      status: {},
      pattern: [],
      queries: {},
      results: "private-results",
      references: false,
      result: [],
    },
    { type: "SEARCH", results: [], queries: ["Public query"] },
  ]);
  const before = structuredClone(input);
  const source = project(input);
  assert.ok(source);
  assert.deepEqual(source.exchange.messages[0].data.v.response.fragments, [
    answer(),
    {},
    {},
    { type: "RESPONSE" },
    { type: "SEARCH", results: [], queries: ["Public query"] },
  ]);
  assert.deepEqual(source.redaction.omitted_paths, [
    ...["type", "content", "future~1~0key"].map(
      (key) => `/messages/0/data/v/response/fragments/1/${key}`,
    ),
    "/messages/0/data/v/response/fragments/2/content",
    ...[
      "content",
      "status",
      "pattern",
      "queries",
      "results",
      "references",
      "result",
    ].map((key) => `/messages/0/data/v/response/fragments/3/${key}`),
  ]);
  assert.deepEqual(
    sourceRedaction(source.redaction, source.exchange),
    source.redaction,
  );
  assert.doesNotMatch(JSON.stringify(source.exchange), /private-|FUTURE/u);
  assert.deepEqual(input, before);
});

test("only a validated SET clears its own field taint", () => {
  const initial = snapshot([
    { ...answer(), content: think(), queries: think() },
  ]);
  const safeSet = event({
    p: "response/fragments/0/content",
    o: "SET",
    v: "Recovered",
  });
  const append = event({
    p: "response/fragments/0/content",
    o: "APPEND",
    v: " public",
  });
  const source = project(initial, safeSet, append);
  assert.ok(source);
  assert.deepEqual(source.exchange.messages.slice(1), [safeSet, append]);
  assert.doesNotMatch(JSON.stringify(source), /private-thought/u);
  assert.equal(
    project(
      initial,
      safeSet,
      event({
        p: "response/fragments/0/queries",
        o: "APPEND",
        v: ["private-later"],
      }),
    ),
    null,
  );
  assert.equal(
    project(
      initial,
      event({
        p: "response/fragments/0/content",
        o: "SET",
        v: think(),
      }),
      append,
    ),
    null,
  );
});

test("array mutations carry taint and unknown provenance until verified replacement", () => {
  for (const slot of [
    { ...answer(), content: think() },
    { type: "FUTURE", content: "private-initial" },
    {},
  ]) {
    for (const setup of [
      [snapshot([slot])],
      [
        snapshot([]),
        event({ p: "response/fragments", o: "APPEND", v: [slot] }),
      ],
      [
        snapshot([answer()]),
        event({ p: "response/fragments/-1", o: "SET", v: slot }),
      ],
      [
        snapshot([answer()]),
        event({ p: "response/fragments", o: "SET", v: [slot] }),
      ],
      [
        snapshot([answer()]),
        event({ p: "response/fragments/-99", o: "APPEND", v: [slot] }),
      ],
      [
        snapshot([slot]),
        event({ p: "response/fragments/0", o: "APPEND", v: [answer()] }),
      ],
    ]) {
      const shifted = setup.at(-1).data.p === "response/fragments/0";
      const index = shifted ? "-1" : "0";
      const append = event({
        p: `response/fragments/${index}/content`,
        o: "APPEND",
        v: "private-later",
      });
      assert.equal(project(...setup, append), null);
      if (slot.type !== "RESPONSE")
        assert.equal(
          project(
            ...setup,
            event({
              p: `response/fragments/${index}/content`,
              o: "SET",
              v: "unverified",
            }),
          ),
          null,
        );
      const replacement = event({
        p: `response/fragments/${index}`,
        o: "SET",
        v: answer(),
      });
      const recovered = project(...setup, replacement, {
        ...append,
        data: { ...append.data, v: " public" },
      });
      assert.ok(recovered);
      assert.deepEqual(
        sourceRedaction(recovered.redaction, recovered.exchange),
        recovered.redaction,
      );
      assert.doesNotMatch(
        JSON.stringify(recovered.exchange),
        /private-|FUTURE/u,
      );
    }
  }
});

test("unknown slots reject every field delta and nested private paths remain closed", () => {
  for (const field of [
    "content",
    "type",
    "authorization",
    "reasoning_content",
    "metadata/content",
  ]) {
    assert.equal(
      project(
        snapshot([{ type: "FUTURE", content: "private-initial" }]),
        event({ p: `response/fragments/0/${field}`, v: "private-later" }),
      ),
      null,
    );
  }
  assert.equal(
    project(
      snapshot([think()]),
      event({
        p: "response/fragments/0/metadata/authorization",
        v: "private-later",
      }),
    ),
    null,
  );
  assert.equal(
    project(
      snapshot([{ ...answer(), content: think() }]),
      event({
        p: "response/fragments/0",
        o: "BATCH",
        v: [{ p: "content", o: "APPEND", v: "" }, { v: "private-later" }],
      }),
    ),
    null,
  );
});

test("omission compatibility still bounds fragment arrays, bytes and clean traversal", () => {
  assert.equal(project(snapshot(Array(10_001).fill({ type: "FUTURE" }))), null);
  assert.equal(
    project(
      snapshot(Array(10_000).fill({})),
      event({ p: "response/fragments", o: "APPEND", v: [{}] }),
    ),
    null,
  );
  assert.equal(
    project(snapshot([{ type: "FUTURE", content: "x".repeat(750_001) }])),
    null,
  );
  let nested = {};
  for (let index = 0; index < 40; index++) nested = { child: nested };
  assert.equal(project(snapshot([{ ...answer(), metadata: nested }])), null);
});
