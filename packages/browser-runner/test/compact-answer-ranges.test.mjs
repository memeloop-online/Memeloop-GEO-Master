import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { validateAiObservation } from "../src/ai-observation-grounding.mjs";
import { groundSavedObservation } from "../src/saved-observation-grounding.mjs";

const range = (
  start_index,
  end_index,
  item_path = "/response/content/text",
) => ({
  array_path: "/messages",
  start_index,
  end_index,
  item_path,
});
const candidate = (answer_segments) => ({
  completion: "complete",
  completion_evidence: [{ path: "/messages/0/done" }],
  search_used: "yes",
  search_evidence: [{ path: "/messages/0/searched" }],
  answer_segments,
  citations: [],
});
const fragment = (text = "合成回答片段") => ({
  response: { content: { text } },
});
const saved = (
  document,
  proposal,
  versions = {
    prompt_version: "geo.observation.extract.v4",
    parser_version: "geo.observation.extract.v6",
  },
) => {
  const source_json = JSON.stringify(document);
  return groundSavedObservation({
    source_json,
    source_sha256: createHash("sha256").update(source_json).digest("hex"),
    candidate_json: JSON.stringify(proposal),
    protocol: {},
    ...versions,
  });
};

test("800 AI-selected fragments around interruptions retain every concrete audit and compact candidate", () => {
  const document = {
    messages: [
      { done: true, searched: true },
      ...Array.from({ length: 400 }, () => fragment()),
      { reasoning: "not an answer", results: [] },
      ...Array.from({ length: 400 }, () => fragment()),
    ],
  };
  const proposal = candidate([range(1, 401), range(402, 802)]);
  const singleton = candidate([
    ...Array.from({ length: 400 }, (_, i) => ({
      path: `/messages/${i + 1}/response/content/text`,
    })),
    ...Array.from({ length: 400 }, (_, i) => ({
      path: `/messages/${i + 402}/response/content/text`,
    })),
  ]);
  const grounded = validateAiObservation(document, proposal);
  assert.deepEqual(grounded, validateAiObservation(document, singleton));
  assert.equal(grounded.raw_answer, "合成回答片段".repeat(800));
  assert.equal(grounded.audit.refs.length, 1602);
  assert.ok(Buffer.byteLength(JSON.stringify(grounded.audit)) > 150_000);
  const result = saved(document, proposal);
  assert.equal(result.outcome.status, "grounded");
  assert.equal(result.candidate_json, JSON.stringify(proposal));
  assert.equal(
    result.outcome.audit.prompt_version,
    "geo.observation.extract.v4",
  );
  assert.deepEqual(result.outcome.audit.refs, grounded.audit.refs);
  assert.equal(
    validateAiObservation(document, candidate([range(1, 802)])),
    null,
  );
});

test("ranges require exact shape, safe nonempty bounds, allowed pointer roots and every string", () => {
  const document = {
    messages: [{ done: true, searched: true }, fragment("x"), fragment("y")],
  };
  for (const invalid of [
    { ...range(1, 3), quote: "x" },
    { ...range(1, 3), path: "/messages/1" },
    { ...range(1, 3), extra: true },
    { ...range(1, 3), item_path: undefined },
    range(-1, 2),
    range(1, 1),
    range(2, 1),
    range(1, 4),
    range(1.5, 2),
    range("1", 2),
    range(1, Number.MAX_SAFE_INTEGER + 1),
    range(NaN, 2),
    range(1, 2, null),
    range(1, 2, "response/content/text"),
    range(1, 2, "/response/~2"),
    range(1, 2, "/constructor"),
    range(1, 2, "/__proto__"),
    range(1, 2, "/prototype"),
    range(1, 2, "/response/content/missing"),
    { ...range(0, 1), array_path: "/protocol/messages" },
    { ...range(0, 1), array_path: "/messages/01" },
    { ...range(0, 1), array_path: "/messages/1/response" },
    { ...range(0, 1), array_path: "/messages/~2" },
  ]) {
    assert.equal(
      validateAiObservation(document, candidate([invalid])),
      null,
      JSON.stringify(invalid),
    );
  }
  for (const value of [null, "", false, 0, {}, []]) {
    const changed = structuredClone(document);
    changed.messages[2] = fragment(value);
    assert.equal(
      validateAiObservation(changed, candidate([range(1, 3)])),
      null,
    );
  }
  for (const role of ["completion_evidence", "search_evidence"]) {
    assert.equal(
      validateAiObservation(document, {
        ...candidate([range(1, 3)]),
        [role]: [range(1, 3)],
      }),
      null,
    );
  }
  for (const role of ["url", "usage"]) {
    assert.equal(
      validateAiObservation(document, {
        ...candidate([range(1, 3)]),
        citations: [
          {
            url: { path: "/messages/0/done" },
            usage: { path: "/messages/0/searched" },
            [role]: range(1, 3),
          },
        ],
      }),
      null,
    );
  }
});

test("empty suffix, escaped keys, duplicates, and UTF-8/expanded-count limits", () => {
  const document = {
    messages: [{ done: true, searched: true }, "中", { "a/b": { "~": "文" } }],
  };
  const proposal = candidate([
    range(1, 2, ""),
    range(2, 3, "/a~1b/~0"),
    range(1, 2, ""),
  ]);
  assert.equal(validateAiObservation(document, proposal).raw_answer, "中文中");
  const large = {
    messages: [{ done: true, searched: true }, ...Array(32768).fill("x")],
  };
  assert.ok(validateAiObservation(large, candidate([range(1, 32769, "")])));
  assert.equal(
    validateAiObservation(
      large,
      candidate([range(1, 32769, ""), range(1, 2, "")]),
    ),
    null,
  );
  large.messages = [{ done: true, searched: true }, "中".repeat(33333), "a"];
  assert.equal(
    validateAiObservation(large, candidate([range(1, 3, "")])).raw_answer
      .length,
    33334,
  );
  large.messages[2] = "中";
  assert.equal(
    validateAiObservation(large, candidate([range(1, 3, "")])),
    null,
  );
});

test("saved ranges require explicit current versions and reject historical versions", () => {
  const document = {
    messages: [{ done: true, searched: true }, fragment("x")],
  };
  const compact = candidate([range(1, 2)]);
  const singleton = candidate([{ path: "/messages/1/response/content/text" }]);
  assert.equal(saved(document, singleton).outcome.status, "grounded");
  assert.equal(saved(document, compact).outcome.status, "grounded");
  for (const versions of [
    {},
    {
      prompt_version: "geo.observation.extract.v2",
      parser_version: "geo.observation.extract.v4",
    },
    {
      prompt_version: "geo.observation.extract.v3",
      parser_version: "geo.observation.extract.v5",
    },
    { prompt_version: "geo.observation.extract.v3" },
    { parser_version: "geo.observation.extract.v5" },
    { prompt_version: null, parser_version: null },
    {
      prompt_version: "geo.observation.extract.v3",
      parser_version: "geo.observation.extract.v4",
    },
    { prompt_version: "future", parser_version: "geo.observation.extract.v5" },
  ])
    assert.equal(
      saved(document, compact, versions).outcome.reason,
      "version_rejected",
    );
});

test("compact candidates cannot bypass the 512000-byte expanded saved audit cap", () => {
  const document = {
    messages: [
      { done: true, searched: true },
      ...Array.from({ length: 4000 }, () => fragment("x")),
    ],
  };
  const proposal = candidate([range(1, 4001)]);
  assert.ok(
    Buffer.byteLength(
      JSON.stringify(validateAiObservation(document, proposal).audit),
    ) > 512_000,
  );
  const result = saved(document, proposal);
  assert.equal(result.outcome.reason, "audit_too_large");
  assert.equal(result.candidate_json, JSON.stringify(proposal));
});
