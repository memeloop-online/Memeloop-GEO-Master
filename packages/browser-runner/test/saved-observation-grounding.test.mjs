import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { groundSavedObservation } from "../src/saved-observation-grounding.mjs";
import { createRunnerServer } from "../src/server.mjs";

const source = {
  messages: [
    {
      state: "completed",
      searched: true,
      text: "A synthetic source-backed answer.",
      url: "https://example.org/article",
      used: true,
    },
  ],
};
const candidate = {
  completion: "complete",
  completion_evidence: [{ path: "/messages/0/state" }],
  search_used: "yes",
  search_evidence: [{ path: "/messages/0/searched" }],
  answer_segments: [{ path: "/messages/0/text" }],
  citations: [
    {
      url: { path: "/messages/0/url" },
      usage: { path: "/messages/0/used" },
    },
  ],
};
const digest = (text) => createHash("sha256").update(text).digest("hex");
const request = (overrides = {}) => {
  const source_json = JSON.stringify(source);
  return {
    source_json,
    source_sha256: digest(source_json),
    candidate_json: JSON.stringify(candidate),
    protocol: { model: "synthetic-model", search_mode: "official_search" },
    ...overrides,
  };
};

const recommendationAnswer =
  "First choice: Sample Air 300, because it is quiet. Also consider Example Bistro for dinner.";
const recommendationItems = [
  {
    name: "Sample Air 300",
    kind: "product",
    mention_order: 1,
    rank: 1,
    reason: "because it is quiet",
    evidence_quote: "First choice: Sample Air 300, because it is quiet.",
    citation_urls: [source.messages[0].url],
  },
  {
    name: "Example Bistro",
    kind: "restaurant",
    mention_order: 2,
    rank: null,
    reason: null,
    evidence_quote: "Also consider Example Bistro for dinner.",
    citation_urls: [],
  },
];
function recommendationRequest(items = recommendationItems) {
  const source_json = JSON.stringify({
    messages: [{ ...source.messages[0], text: recommendationAnswer }],
  });
  return request({
    source_json,
    source_sha256: digest(source_json),
    candidate_json: JSON.stringify({
      ...candidate,
      ...(items === undefined ? {} : { recommendations: items }),
    }),
    prompt_version: "geo.observation.extract.v4",
    parser_version: "geo.observation.extract.v6",
  });
}

test("new replay grounds recommended products and restaurants without inventing unranked positions", () => {
  const result = groundSavedObservation(recommendationRequest());
  assert.equal(result.outcome.status, "grounded");
  assert.deepEqual(result.outcome.audit.recommendations, {
    version: "geo.recommendations.v1",
    status: "verified",
    items: recommendationItems,
  });
});

test("invalid recommendation proposals preserve the grounded answer and citations", () => {
  const invalidItems = [
    null,
    [{ ...recommendationItems[0], name: "Invented product" }],
    [{ ...recommendationItems[0], evidence_quote: "Invented source text" }],
    [{ ...recommendationItems[0], reason: "an invented rationale" }],
    [{ ...recommendationItems[0], rank: 0 }],
    [{ ...recommendationItems[0], mention_order: 2 }],
    [
      {
        ...recommendationItems[0],
        citation_urls: ["https://example.org/unrelated"],
      },
    ],
    [{ ...recommendationItems[0], kind: "unknown-kind" }],
    [{ ...recommendationItems[0], private_field: "not allowed" }],
    [recommendationItems[0], { ...recommendationItems[0], mention_order: 2 }],
    Array.from({ length: 101 }, () => recommendationItems[0]),
  ];
  for (const items of invalidItems) {
    const result = groundSavedObservation(recommendationRequest(items));
    assert.equal(result.outcome.status, "grounded");
    assert.equal(result.outcome.raw_answer, recommendationAnswer);
    assert.deepEqual(result.outcome.citations, [source.messages[0].url]);
    assert.deepEqual(result.outcome.audit.recommendations, {
      version: "geo.recommendations.v1",
      status: "unverified",
      items: [],
    });
  }
});

test("empty, missing and historical recommendation analysis remain distinct", () => {
  assert.equal(
    groundSavedObservation(recommendationRequest([])).outcome.audit
      .recommendations.status,
    "verified",
  );
  const missing = recommendationRequest();
  missing.candidate_json = JSON.stringify(candidate);
  assert.equal(
    groundSavedObservation(missing).outcome.audit.recommendations.status,
    "unverified",
  );
  for (const [prompt_version, parser_version] of [
    ["geo.observation.extract.v2", "geo.observation.extract.v4"],
    ["geo.observation.extract.v3", "geo.observation.extract.v5"],
  ]) {
    const historical = request({ prompt_version, parser_version });
    assert.equal(
      groundSavedObservation(historical).outcome.audit.recommendations,
      undefined,
    );
    historical.candidate_json = recommendationRequest().candidate_json;
    assert.equal(
      groundSavedObservation(historical).outcome.status,
      "unverified",
    );
  }
});

test("saved grounding normalizes fenced candidates and binds exact source bytes", () => {
  const input = request({
    candidate_json: `\`\`\`json\n${JSON.stringify(candidate)}\n\`\`\``,
  });
  const result = groundSavedObservation(input);
  assert.equal(result.candidate_json, JSON.stringify(candidate));
  assert.equal(result.outcome.status, "grounded");
  assert.equal(result.outcome.raw_answer, source.messages[0].text);
  assert.deepEqual(result.outcome.citations, [source.messages[0].url]);
  assert.equal(result.outcome.audit.source_json, undefined);
  assert.equal(result.outcome.audit.source_sha256, input.source_sha256);
  assert.deepEqual(result.outcome.audit.protocol, input.protocol);
});

test("saved grounding rejects invalid shapes, digest, JSON, and byte limits", () => {
  for (const [overrides, reason] of [
    [{ source_sha256: "0".repeat(64) }, "source_digest_mismatch"],
    [{ source_sha256: "A".repeat(64) }, "shape_rejected"],
    [{ protocol: [] }, "shape_rejected"],
    [{ session_id: "synthetic-session" }, "shape_rejected"],
    [{ source_json: "{", source_sha256: digest("{") }, "source_invalid_json"],
    [{ candidate_json: "{secret" }, "candidate_invalid_json"],
    [{ candidate_json: "null" }, "candidate_invalid_json"],
    [{ candidate_json: "中".repeat(50_001) }, "candidate_too_large"],
    [{ source_json: "中".repeat(250_001) }, "source_too_large"],
    [{ protocol: { model: "x".repeat(8193) } }, "protocol_too_large"],
  ]) {
    assert.deepEqual(groundSavedObservation(request(overrides)), {
      candidate_json: null,
      outcome: { status: "unverified", reason },
    });
  }
});

test("saved grounding cannot accept fabricated references or legacy identifier schema", () => {
  for (const proposed of [
    { ...candidate, answer_segments: [{ path: "/messages/99/text" }] },
    { ...candidate, decision: "searched_answer" },
    { ...candidate, completion: "unknown" },
    { ...candidate, search_used: "no" },
  ]) {
    const result = groundSavedObservation(
      request({ candidate_json: JSON.stringify(proposed) }),
    );
    assert.equal(result.outcome.status, "unverified");
    assert.equal(typeof result.outcome.reason, "string");
    assert.equal(result.candidate_json, JSON.stringify(proposed));
    assert.equal(result.outcome.raw_answer, undefined);
    assert.equal(result.outcome.audit, undefined);
  }
});

test("saved grounding sanitizes retained candidates without upgrading rejected proposals", () => {
  for (const proposed of [
    {
      ...candidate,
      reasoning: "synthetic-private-reasoning",
      credentials: { password: "synthetic-private-password" },
    },
    {
      ...candidate,
      answer_segments: [
        {
          ...candidate.answer_segments[0],
          thinking: "synthetic-private-thinking",
          access_token: "synthetic-private-token",
        },
      ],
    },
  ]) {
    const result = groundSavedObservation(
      request({ candidate_json: JSON.stringify(proposed) }),
    );
    assert.deepEqual(result.outcome, {
      status: "unverified",
      reason: "shape_rejected",
    });
    assert.equal(result.candidate_json, JSON.stringify(candidate));
    assert.doesNotMatch(JSON.stringify(result), /synthetic-private/u);
  }
});

test("saved grounding bounds amplified audit references", () => {
  const source_json = JSON.stringify({
    messages: [{ ...source.messages[0], text: "x" }],
  });
  const result = groundSavedObservation(
    request({
      source_json,
      source_sha256: digest(source_json),
      candidate_json: JSON.stringify({
        ...candidate,
        answer_segments: Array.from({ length: 4000 }, () => ({
          path: "/messages/0/text",
        })),
      }),
    }),
  );
  assert.deepEqual(result.outcome, {
    status: "unverified",
    reason: "audit_too_large",
  });
});

test("service endpoint is bearer-only and does not touch runner/session methods", async (t) => {
  const runner = new Proxy(
    {},
    {
      get() {
        throw new Error("runner methods must not be accessed");
      },
    },
  );
  const server = createRunnerServer({
    token: "synthetic-service-token",
    runner,
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const url = `http://127.0.0.1:${server.address().port}/v1/observation-analyses/ground`;
  const headers = {
    authorization: "Bearer synthetic-service-token",
    "content-type": "application/json",
  };
  const denied = await fetch(url, { method: "POST", body: "{}" });
  assert.equal(denied.status, 401);
  const response = await fetch(url, {
    method: "POST",
    headers,
    body: JSON.stringify(request()),
  });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.equal((await response.json()).outcome.status, "grounded");
  const source_json = JSON.stringify({
    ...source,
    padding: '"'.repeat(300_000),
  });
  const largeBody = JSON.stringify(
    request({
      source_json,
      source_sha256: digest(source_json),
    }),
  );
  assert.ok(Buffer.byteLength(largeBody) > 1024 * 1024);
  const escaped = await fetch(url, {
    method: "POST",
    headers,
    body: largeBody,
  });
  assert.equal(escaped.status, 200);
  assert.equal((await escaped.json()).outcome.status, "grounded");
  const invalid = await fetch(url, { method: "POST", headers, body: "{" });
  assert.equal(invalid.status, 400);
  assert.deepEqual(await invalid.json(), { error: "invalid_json" });
  const oversized = await fetch(url, {
    method: "POST",
    headers,
    body: " ".repeat(6_000_001),
  });
  assert.equal(oversized.status, 413);
  assert.deepEqual(await oversized.json(), { error: "payload_too_large" });
});
