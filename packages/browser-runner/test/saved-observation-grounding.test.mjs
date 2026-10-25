import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import {
  groundSavedObservation,
  prepareSavedObservation,
} from "../src/saved-observation-grounding.mjs";
import { extractionPrompt } from "../src/ai-observation-parser.mjs";
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
    prompt_version: "geo.observation.extract.v5",
    parser_version: "geo.observation.extract.v7",
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
    brand_attribution: null,
  },
  {
    name: "Example Bistro",
    kind: "restaurant",
    mention_order: 2,
    rank: null,
    reason: null,
    evidence_quote: "Also consider Example Bistro for dinner.",
    citation_urls: [],
    brand_attribution: null,
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
    prompt_version: "geo.observation.extract.v5",
    parser_version: "geo.observation.extract.v7",
  });
}

test("new replay grounds recommended products and restaurants without inventing unranked positions", () => {
  const result = groundSavedObservation(recommendationRequest());
  assert.equal(result.outcome.status, "grounded");
  assert.deepEqual(result.outcome.audit.recommendations, {
    version: "geo.recommendations.v2",
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
      version: "geo.recommendations.v2",
      status: "unverified",
      items: [],
    });
  }
});

test("explicit product brand attribution preserves exact answer evidence", () => {
  const text =
    "Sample Air 300 is made by Example Brand. First choice: Sample Air 300, because it is quiet.";
  const source_json = JSON.stringify({
    messages: [{ ...source.messages[0], text }],
  });
  const item = {
    ...recommendationItems[0],
    brand_attribution: {
      name: "Example Brand",
      evidence_quote: "Sample Air 300 is made by Example Brand.",
    },
  };
  const input = recommendationRequest([item]);
  input.source_json = source_json;
  input.source_sha256 = digest(source_json);
  const result = groundSavedObservation(input);
  assert.deepEqual(result.outcome.audit.recommendations.items, [item]);
  assert.equal(result.outcome.audit.recommendations.items[0].rank, 1);
});

test("missing or invalid brand attribution invalidates the whole collection", () => {
  const valid = recommendationItems[0];
  const { brand_attribution, ...missing } = valid;
  const invalid = [
    missing,
    { ...valid, brand_attribution: undefined },
    ...[
      false,
      [],
      {},
      { name: "Sample" },
      { name: "Sample", evidence_quote: null },
      { name: "", evidence_quote: recommendationAnswer },
      { name: "x".repeat(501), evidence_quote: recommendationAnswer },
      { name: "Sample", evidence_quote: "x".repeat(8001) },
      { name: "Sample", evidence_quote: "Sample Air 300 invented quote" },
      { name: "Sample", evidence_quote: "Sample" },
      { name: "Absent", evidence_quote: recommendationAnswer },
      { name: "Sample", evidence_quote: recommendationAnswer, extra: true },
    ].map((value) => ({ ...valid, brand_attribution: value })),
    {
      ...recommendationItems[1],
      mention_order: 1,
      brand_attribution: {
        name: "Example",
        evidence_quote: "Also consider Example Bistro for dinner.",
      },
    },
  ];
  for (const item of invalid) {
    const result = groundSavedObservation(recommendationRequest([item]));
    assert.equal(result.outcome.status, "grounded");
    assert.deepEqual(result.outcome.audit.recommendations, {
      version: "geo.recommendations.v2",
      status: "unverified",
      items: [],
    });
  }
});

test("brand relationship meaning belongs to the model, source binding to code", () => {
  // No prefix or language heuristic decides ownership in application code.
  const item = {
    ...recommendationItems[0],
    brand_attribution: {
      name: "Sample",
      evidence_quote: recommendationItems[0].evidence_quote,
    },
  };
  assert.equal(
    groundSavedObservation(recommendationRequest([item])).outcome.audit
      .recommendations.status,
    "verified",
  );
});

test("optional recommendation audit overflow preserves bounded answer provenance", () => {
  const text = `Sample Air 300 ${"中".repeat(2500)}`;
  const items = [
    {
      ...recommendationItems[0],
      rank: null,
      reason: text,
      evidence_quote: text,
    },
  ];
  for (const [count, expected] of [
    [2800, "verified"],
    [2900, "unverified"],
    [3000, "audit_too_large"],
  ]) {
    const source_json = JSON.stringify({
      messages: [
        {
          ...source.messages[0],
          text,
          fragments: Array.from({ length: count }, () => ({ text: "x" })),
        },
      ],
    });
    const proposal = {
      ...candidate,
      answer_segments: [
        ...candidate.answer_segments,
        {
          array_path: "/messages/0/fragments",
          start_index: 0,
          end_index: count,
          item_path: "/text",
        },
      ],
      recommendations: items,
    };
    const input = {
      ...recommendationRequest(),
      source_json,
      source_sha256: digest(source_json),
      candidate_json: JSON.stringify(proposal),
    };
    const base = groundSavedObservation({
      ...input,
      candidate_json: JSON.stringify({ ...proposal, recommendations: [] }),
    });
    const result = groundSavedObservation(input);
    assert.equal(result.candidate_json, input.candidate_json);
    if (expected === "audit_too_large") {
      assert.equal(base.outcome.reason, expected);
      assert.equal(result.outcome.reason, expected);
      continue;
    }
    assert.equal(result.outcome.status, "grounded");
    assert.equal(result.outcome.raw_answer, text + "x".repeat(count));
    assert.deepEqual(result.outcome.citations, base.outcome.citations);
    const { recommendations, ...audit } = result.outcome.audit;
    const { recommendations: _, ...baseAudit } = base.outcome.audit;
    assert.deepEqual(audit, baseAudit);
    assert.deepEqual(recommendations, {
      version: "geo.recommendations.v2",
      status: expected,
      items: expected === "verified" ? items : [],
    });
    assert.ok(
      Buffer.byteLength(JSON.stringify(result.outcome.audit)) <= 512_000,
    );
    if (expected === "unverified") {
      // Even the empty analysis marker must not displace a valid base audit.
      const protocol = { padding: "" };
      const exactAudit = { ...audit, protocol };
      protocol.padding = "x".repeat(
        512_000 - Buffer.byteLength(JSON.stringify(exactAudit)),
      );
      const atLimit = groundSavedObservation({ ...input, protocol });
      assert.equal(atLimit.outcome.status, "grounded");
      assert.equal(atLimit.outcome.audit.recommendations, undefined);
      assert.deepEqual(atLimit.outcome.audit, exactAudit);
      assert.equal(
        Buffer.byteLength(JSON.stringify(atLimit.outcome.audit)),
        512_000,
      );
    }
  }
});

test("empty and missing recommendation analysis remain distinct; historical versions are rejected", () => {
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
    ["geo.observation.extract.v4", "geo.observation.extract.v6"],
  ]) {
    const historical = request({ prompt_version, parser_version });
    assert.equal(
      groundSavedObservation(historical).outcome.reason,
      "version_rejected",
    );
    historical.candidate_json = recommendationRequest().candidate_json;
    assert.equal(
      groundSavedObservation(historical).outcome.status,
      "unverified",
    );
  }
});

const prepareRequest = (overrides = {}) => {
  const { candidate_json, protocol, ...input } = request();
  return { ...input, ...overrides };
};

test("saved prompt preparation reuses current prompt with exact saved indices and metadata", () => {
  const document = {
    messages: [
      { state: "completed", text: "合成片段一" },
      { arbitrary: { value: "合成片段二" } },
    ],
    rendered_text: "合成完整可见回答",
    capture_status: {
      schema_version: "geo.observation.capture-status.v1",
      transport: "partial",
      termination: "eof",
    },
    source_redaction: {
      schema_version: "geo.observation.source-redaction.v1",
      policy: "reasoning-and-private-fields",
      omitted_paths: ["/messages/0/reasoning"],
    },
  };
  const source_json = JSON.stringify(document, null, 2);
  const input = prepareRequest({
    source_json,
    source_sha256: digest(source_json),
  });
  const before = structuredClone(input);
  const result = prepareSavedObservation(input);
  assert.deepEqual(input, before);
  assert.deepEqual(result, {
    prompt: extractionPrompt(document),
    source_sha256: digest(source_json),
    prompt_version: "geo.observation.extract.v5",
    parser_version: "geo.observation.extract.v7",
  });
  for (const key of ["rendered_text", "capture_status", "source_redaction"])
    assert.ok(
      result.prompt.includes(`/${key} = ${JSON.stringify(document[key])}`),
    );
  for (const [index, message] of document.messages.entries())
    assert.ok(
      result.prompt.includes(`/messages/${index} = ${JSON.stringify(message)}`),
    );
});

test("saved prompt preparation rejects malformed, oversized and unsupported input with bounded reasons", () => {
  for (const [overrides, error] of [
    [{ source_sha256: "A".repeat(64) }, "shape_rejected"],
    [{ source_sha256: "0".repeat(64) }, "source_digest_mismatch"],
    [{ source_json: "{", source_sha256: digest("{") }, "source_invalid_json"],
    [{ source_json: "中".repeat(250_001) }, "source_too_large"],
    [
      {
        prompt_version: "geo.observation.extract.v2",
        parser_version: "geo.observation.extract.v5",
      },
      "version_rejected",
    ],
    [
      {
        prompt_version: "geo.observation.extract.v3",
        parser_version: "geo.observation.extract.v5",
      },
      "version_rejected",
    ],
    [{ parser_version: "geo.observation.extract.v5" }, "version_rejected"],
    [{ prompt_version: null }, "version_rejected"],
    [{ candidate_json: "{}" }, "shape_rejected"],
    [{ protocol: {} }, "shape_rejected"],
    [{ session_id: "synthetic-session" }, "shape_rejected"],
  ])
    assert.deepEqual(prepareSavedObservation(prepareRequest(overrides)), {
      error,
    });
  for (const document of [
    null,
    [],
    {},
    { messages: null },
    { messages: {} },
    { messages: [], rendered_text: 42 },
  ]) {
    const source_json = JSON.stringify(document);
    assert.deepEqual(
      prepareSavedObservation(
        prepareRequest({ source_json, source_sha256: digest(source_json) }),
      ),
      { error: "shape_rejected" },
    );
  }
  for (const omitted of [
    "source_json",
    "source_sha256",
    "prompt_version",
    "parser_version",
  ]) {
    const input = prepareRequest();
    delete input[omitted];
    assert.deepEqual(prepareSavedObservation(input), {
      error: "shape_rejected",
    });
  }
  const source_json = JSON.stringify({ messages: Array(100_000).fill(null) });
  assert.ok(Buffer.byteLength(source_json) <= 750_000);
  assert.deepEqual(
    prepareSavedObservation(
      prepareRequest({ source_json, source_sha256: digest(source_json) }),
    ),
    { error: "prompt_too_large" },
  );
});

test("saved grounding requires both explicit current extraction versions", () => {
  for (const versions of [
    {},
    { prompt_version: "geo.observation.extract.v5" },
    { parser_version: "geo.observation.extract.v7" },
    { prompt_version: null, parser_version: null },
    {
      prompt_version: "geo.observation.extract.v3",
      parser_version: "geo.observation.extract.v7",
    },
  ]) {
    const input = request();
    delete input.prompt_version;
    delete input.parser_version;
    assert.deepEqual(groundSavedObservation({ ...input, ...versions }), {
      candidate_json: null,
      outcome: { status: "unverified", reason: "version_rejected" },
    });
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

test("prompt preparation endpoint is authenticated, bounded and independent of runner and model", async (t) => {
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
  const url = `http://127.0.0.1:${server.address().port}/v1/observation-analyses/prepare`;
  const headers = {
    authorization: "Bearer synthetic-service-token",
    "content-type": "application/json",
  };
  const denied = await fetch(url, {
    method: "POST",
    body: JSON.stringify(prepareRequest()),
  });
  assert.equal(denied.status, 401);
  const input = prepareRequest();
  const response = await fetch(url, {
    method: "POST",
    headers,
    body: JSON.stringify(input),
  });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.deepEqual(await response.json(), prepareSavedObservation(input));
  for (const [body, error] of [
    ["{", "invalid_json"],
    [
      JSON.stringify({ ...input, source_sha256: "0".repeat(64) }),
      "source_digest_mismatch",
    ],
    [JSON.stringify({ ...input, candidate_json: "{}" }), "shape_rejected"],
    [
      JSON.stringify({
        ...input,
        prompt_version: "geo.observation.extract.v3",
        parser_version: "geo.observation.extract.v5",
      }),
      "version_rejected",
    ],
    [" ".repeat(6_000_001), "payload_too_large"],
  ]) {
    const rejected = await fetch(url, { method: "POST", headers, body });
    assert.equal(rejected.status, 400);
    assert.deepEqual(await rejected.json(), { error });
  }
});
