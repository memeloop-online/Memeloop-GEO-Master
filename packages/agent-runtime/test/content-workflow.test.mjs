import assert from "node:assert/strict";
import test from "node:test";

const entry = new URL(
  "../dist/memeloop-content-workflow.bundle.mjs",
  import.meta.url,
);
const executionId = "00000000-0000-4000-8000-000000000001";
const cycleId = "00000000-0000-4000-8000-000000000002";
const itemId = "00000000-0000-4000-8000-000000000003";
const handoffId = "00000000-0000-4000-8000-000000000004";
const manifestId = "00000000-0000-4000-8000-000000000005";
const sourceId = "00000000-0000-4000-8000-000000000006";
const sourceVersionId = "00000000-0000-4000-8000-000000000007";
const chunk = (ordinal, text = `Source fact ${ordinal}`) => ({
  chunk_id: `chunk-${ordinal}`,
  ordinal,
  locator: {
    kind: "text",
    start_line: ordinal + 1,
    end_line: ordinal + 1,
    start_char: 0,
    end_char: text.length,
  },
  text,
});

async function run({
  checks = [],
  initial = "pending",
  count = 0,
  interruptAfter,
  preparedStatus = "prepared",
  sourceChunks = [chunk(0)],
  sourceRefs = [{ source_id: sourceId, source_version_id: sourceVersionId }],
  reusableEvidence = null,
  select = (input) =>
    input.chunks.map((entry) => ({
      source_version_id: entry.source_version_id,
      chunk_id: entry.chunk_id,
      exact_quote: entry.text,
    })),
  itemCount = 1,
  incomplete = false,
} = {}) {
  const item = {
    item_id: itemId,
    branch_key: "frozen-branch",
    status: initial,
    automatic_repair_count: count,
    document_key: "synthetic-document",
    source_refs: sourceRefs,
    reusable_evidence: reusableEvidence,
  };
  const items = Array.from({ length: itemCount }, (_, index) => ({
    ...item,
    item_id: index ? `second-${itemId}` : itemId,
    document_key: `synthetic-document-${index}`,
  }));
  const calls = [];
  const sourceReads = [];
  const modelRequests = [];
  const preparations = [];
  let interrupted = false;
  const read = (current = items[0]) => ({ ...current });
  const requested = (request) =>
    items.find((entry) => entry.item_id === request.item_id);
  const host = {
    executionRead: async () => ({
      execution_id: executionId,
      cycle_id: cycleId,
    }),
    itemsRead: async () => ({
      execution_id: executionId,
      total: items.length,
      items: items.map(read),
      next_cursor: null,
    }),
    async sourceRead(request) {
      sourceReads.push(request);
      const chunks = sourceChunks
        .filter((entry) => entry.ordinal > (request.after_ordinal ?? -1))
        .slice(0, request.limit);
      return {
        ...request,
        chunks,
        next_ordinal:
          chunks.length && sourceChunks.at(-1).ordinal > chunks.at(-1).ordinal
            ? chunks.at(-1).ordinal
            : null,
      };
    },
    async modelComplete(request) {
      modelRequests.push(request);
      return {
        text: JSON.stringify({
          evidence: select(JSON.parse(request.prompt), modelRequests.length),
        }),
      };
    },
    async prepare(request) {
      calls.push("prepare");
      preparations.push(request);
      requested(request).status = preparedStatus;
      return read(requested(request));
    },
    async generate(request) {
      calls.push("generate");
      requested(request).status = "drafted";
      return read(requested(request));
    },
    async check(request) {
      calls.push("check");
      const item = requested(request);
      const blocked = checks.shift();
      item.status = blocked
        ? item.automatic_repair_count === 2
          ? "blocked"
          : "needs_repair"
        : "ready";
      return read(item);
    },
    async repair(request) {
      calls.push("repair");
      const item = requested(request);
      item.status = "drafted";
      item.automatic_repair_count++;
      if (!interrupted && interruptAfter === item.automatic_repair_count) {
        interrupted = true;
        throw new Error("interrupted after persisted repair");
      }
      return read(item);
    },
    async close() {
      calls.push("close");
      if (
        items.some(
          (item) =>
            !["ready", "blocked", "deferred", "not_applicable"].includes(
              item.status,
            ),
        )
      ) {
        throw new Error("incomplete branch");
      }
      return {
        execution_id: executionId,
        handoff_id: handoffId,
        total: items.length,
      };
    },
    async distributionStart() {
      calls.push("distributionStart");
      return {
        manifest_id: manifestId,
        cycle_id: cycleId,
        content_execution_id: executionId,
        content_handoff_id: handoffId,
        expected_count: 0,
        expansion_cursor: 0,
        complete: true,
      };
    },
    async distributionResume() {
      calls.push("distributionResume");
      return host.distributionStart();
    },
    async distributionTargetsRead() {
      calls.push("distributionTargetsRead");
      return {
        manifest_id: manifestId,
        expected_count: 0,
        items: [],
        next_ordinal: null,
      };
    },
    async emit() {},
  };
  globalThis.__GEO_CONTENT_TEST_HOST__ = host;
  try {
    const { main } = await import(entry.href);
    if (interruptAfter || incomplete) {
      await assert.rejects(main({ execution_id: executionId }));
    } else {
      await main({ execution_id: executionId });
    }
    return {
      calls,
      item: read(),
      items,
      sourceReads,
      modelRequests,
      preparations,
      host,
      workflow: main,
    };
  } finally {
    delete globalThis.__GEO_CONTENT_TEST_HOST__;
  }
}

test("bounded factual repair checks each persisted revision and continues to distribution", async () => {
  const result = await run({ checks: [true, true, false] });
  assert.deepEqual(result.calls.slice(0, 8), [
    "prepare",
    "generate",
    "check",
    "repair",
    "check",
    "repair",
    "check",
    "close",
  ]);
  assert.equal(result.item.status, "ready");
  assert.equal(result.item.automatic_repair_count, 2);
  assert.ok(result.calls.includes("distributionTargetsRead"));
});

test("explicit evidence reads every source page and selects a nonprefix quote after chunk 24", async () => {
  const quote = "The useful fact is near the end.";
  const chunks = Array.from({ length: 66 }, (_, index) =>
    chunk(
      index,
      index === 65
        ? `${"irrelevant ".repeat(180)}${quote}`
        : `Unrelated text ${index}`,
    ),
  );
  const result = await run({
    checks: [false],
    sourceChunks: chunks,
    select: (input) =>
      input.chunks
        .filter((entry) => entry.chunk_id === "chunk-65")
        .map((entry) => ({
          source_version_id: entry.source_version_id,
          chunk_id: entry.chunk_id,
          exact_quote: quote,
        })),
  });
  assert.deepEqual(
    result.sourceReads.map((request) => request.after_ordinal),
    [null, 31, 63],
  );
  assert.equal(result.modelRequests.length, 3);
  assert.ok(
    result.modelRequests.every((request) =>
      request.system.includes("untrusted data"),
    ),
  );
  assert.equal(
    JSON.parse(result.modelRequests[2].prompt).chunks[1].text,
    chunks[65].text,
  );
  assert.deepEqual(result.preparations[0].evidence, [
    {
      reference: {
        source_version_id: sourceVersionId,
        chunk_id: "chunk-65",
        locator: chunks[65].locator,
      },
      exact_quote: quote,
    },
  ]);
});

test("evidence exceeding the final budget is selected by the model from all candidates", async () => {
  const chunks = Array.from({ length: 33 }, (_, index) => chunk(index));
  const result = await run({
    checks: [false],
    sourceChunks: chunks,
    select: (input) =>
      input.candidates
        ? [input.candidates.at(-1)]
        : input.chunks
            .filter((entry) => entry.ordinal < 24 || entry.ordinal === 32)
            .map((entry) => ({
              source_version_id: entry.source_version_id,
              chunk_id: entry.chunk_id,
              exact_quote: entry.text,
            })),
  });
  assert.equal(result.modelRequests.length, 3);
  assert.equal(
    JSON.parse(result.modelRequests[2].prompt).candidates.length,
    25,
  );
  assert.equal(
    result.preparations[0].evidence[0].reference.chunk_id,
    "chunk-32",
  );
});

test("large CSV sources use bounded hierarchical AI reduction and inspect every page", async () => {
  const chunks = Array.from({ length: 10000 }, (_, index) => {
    const prefix = `Fact ${index}: `;
    return {
      ...chunk(index, prefix + "z".repeat(1600 - prefix.length)),
      locator: {
        kind: "csv",
        start_row: index + 2,
        end_row: index + 2,
        start_column: 1,
        end_column: 1,
      },
    };
  });
  const result = await run({
    checks: [false],
    sourceChunks: chunks,
    // Deterministic model fixture gives later facts higher relevance. Every
    // merge must preserve the chosen exact quote rather than cut its prefix.
    select: (input) =>
      (
        input.candidates ??
        input.chunks.map((entry) => ({
          source_version_id: entry.source_version_id,
          chunk_id: entry.chunk_id,
          exact_quote: entry.text,
        }))
      )
        .toSorted(
          (left, right) =>
            Number(right.chunk_id.substring(6)) -
            Number(left.chunk_id.substring(6)),
        )
        .slice(0, 24),
  });
  const inputs = result.modelRequests.map((request) =>
    JSON.parse(request.prompt),
  );
  const pages = inputs.filter((input) => input.chunks);
  const reductions = inputs.filter((input) => input.candidates);
  assert.equal(result.sourceReads.length, Math.ceil(chunks.length / 32));
  assert.deepEqual(
    pages.flatMap((input) => input.chunks.map((entry) => entry.chunk_id)),
    chunks.map((entry) => entry.chunk_id),
  );
  assert.ok(reductions.length > 1);
  assert.ok(reductions.every((input) => input.candidates.length <= 48));
  assert.ok(
    result.modelRequests.every(
      (request) => Buffer.byteLength(request.prompt) < 90000,
    ),
  );
  const evidence = result.preparations[0].evidence;
  assert.equal(evidence.length, 24);
  assert.equal(evidence[0].reference.chunk_id, "chunk-9999");
  assert.equal(evidence[0].exact_quote, chunks[9999].text);
});

for (const [label, selection] of [
  [
    "unknown chunk",
    {
      source_version_id: sourceVersionId,
      chunk_id: "missing",
      exact_quote: "Source fact 0",
    },
  ],
  [
    "unknown version",
    {
      source_version_id: "another-version",
      chunk_id: "chunk-0",
      exact_quote: "Source fact 0",
    },
  ],
  [
    "invented quote",
    {
      source_version_id: sourceVersionId,
      chunk_id: "chunk-0",
      exact_quote: "invented",
    },
  ],
  [
    "blank quote",
    {
      source_version_id: sourceVersionId,
      chunk_id: "chunk-0",
      exact_quote: " ",
    },
  ],
  ["duplicate chunk", null],
]) {
  test(`invalid ${label} selection never writes preparation`, async () => {
    const valid = {
      source_version_id: sourceVersionId,
      chunk_id: "chunk-0",
      exact_quote: "Source fact 0",
    };
    const result = await run({
      incomplete: true,
      select: () => (selection ? [selection] : [valid, valid]),
    });
    assert.equal(result.preparations.length, 0);
    assert.equal(result.item.status, "pending");
  });
}

test("final selection cannot invent a new quote from a candidate", async () => {
  const result = await run({
    incomplete: true,
    sourceChunks: Array.from({ length: 33 }, (_, index) => chunk(index)),
    select: (input) =>
      input.candidates
        ? [{ ...input.candidates[0], exact_quote: "Source" }]
        : input.chunks
            .filter((entry) => entry.ordinal < 24 || entry.ordinal === 32)
            .map((entry) => ({
              source_version_id: entry.source_version_id,
              chunk_id: entry.chunk_id,
              exact_quote: entry.text,
            })),
  });
  assert.equal(result.preparations.length, 0);
});

test("prepared and ready replay skip source reads and selection", async () => {
  for (const initial of ["prepared", "ready"]) {
    const result = await run({ initial, checks: [false] });
    assert.equal(result.sourceReads.length, 0);
    assert.equal(result.modelRequests.length, 0);
    assert.equal(result.preparations.length, 0);
  }
});

test("pending item reuses qualified persisted evidence without a model call", async () => {
  const evidence = [
    {
      reference: {
        source_version_id: sourceVersionId,
        chunk_id: "chunk-0",
        locator: chunk(0).locator,
      },
      exact_quote: "Source fact 0",
    },
  ];
  const result = await run({
    reusableEvidence: evidence,
    preparedStatus: "ready",
    select: () => {
      throw new Error("no model configured");
    },
  });
  assert.equal(result.sourceReads.length, 0);
  assert.equal(result.modelRequests.length, 0);
  assert.deepEqual(result.preparations[0].evidence, evidence);
  assert.ok(!result.calls.includes("generate"));
});

test("partial CSV records and quotes above the limit do not reach preparation", async () => {
  const csv = {
    ...chunk(0, "Name: Sample\nDescription: Complete logical record"),
    locator: {
      kind: "csv",
      start_row: 2,
      end_row: 2,
      start_column: 1,
      end_column: 2,
    },
  };
  for (const [source, quote] of [
    [csv, "Name: Sample"],
    [chunk(0, "x".repeat(1601)), "x".repeat(1601)],
  ]) {
    const result = await run({
      incomplete: true,
      sourceChunks: [source],
      select: () => [
        {
          source_version_id: sourceVersionId,
          chunk_id: source.chunk_id,
          exact_quote: quote,
        },
      ],
    });
    assert.equal(result.preparations.length, 0);
  }
});

test("two frozen document branches independently select and prepare evidence", async () => {
  const result = await run({ itemCount: 2, checks: [false, false] });
  assert.equal(result.preparations.length, 2);
  assert.equal(
    new Set(result.sourceReads.map((request) => request.item_id)).size,
    2,
  );
  assert.deepEqual(
    result.modelRequests
      .map((request) => JSON.parse(request.prompt).document_key)
      .sort(),
    ["synthetic-document-0", "synthetic-document-1"],
  );
  assert.ok(result.items.every((item) => item.status === "ready"));
});

test("every frozen source version is read and selected references use trusted locators", async () => {
  const secondVersion = "00000000-0000-4000-8000-000000000008";
  const result = await run({
    checks: [false],
    sourceRefs: [
      { source_id: sourceId, source_version_id: sourceVersionId },
      { source_id: sourceId, source_version_id: secondVersion },
    ],
    select: (input) =>
      input.chunks.map((entry) => ({
        source_version_id: entry.source_version_id,
        chunk_id: entry.chunk_id,
        exact_quote: entry.text,
        locator: { kind: "fabricated" },
      })),
  });
  assert.deepEqual(
    result.sourceReads.map((request) => request.source_version_id),
    [sourceVersionId, secondVersion],
  );
  assert.deepEqual(
    result.preparations[0].evidence.map(
      (entry) => entry.reference.source_version_id,
    ),
    [sourceVersionId, secondVersion],
  );
  assert.ok(
    result.preparations[0].evidence.every(
      (entry) => entry.reference.locator.kind === "text",
    ),
  );
});

test("third blocking check is terminal without a third repair", async () => {
  const result = await run({ checks: [true, true, true] });
  assert.equal(result.item.status, "blocked");
  assert.equal(result.calls.filter((call) => call === "repair").length, 2);
});

test("prepare can reuse a ready revision without generation or a new check", async () => {
  const result = await run({ checks: [], preparedStatus: "ready" });
  assert.equal(result.item.status, "ready");
  assert.deepEqual(result.calls.slice(0, 2), ["prepare", "close"]);
  assert.ok(result.calls.includes("distributionTargetsRead"));
  for (const operation of ["generate", "check", "repair"]) {
    assert.ok(!result.calls.includes(operation));
  }
});

test("reentry of reused ready content only closes and prepares current-cycle coverage", async () => {
  const result = await run({ checks: [], initial: "ready" });
  assert.equal(result.calls[0], "close");
  assert.ok(result.calls.includes("distributionTargetsRead"));
  for (const operation of ["prepare", "generate", "check", "repair"]) {
    assert.ok(!result.calls.includes(operation));
  }
});

test("reentry follows persisted draft and count after interrupted repair", async () => {
  const result = await run({ checks: [true, false], interruptAfter: 1 });
  assert.equal(result.item.status, "drafted");
  assert.deepEqual(result.calls, [
    "prepare",
    "generate",
    "check",
    "repair",
    "close",
  ]);
  globalThis.__GEO_CONTENT_TEST_HOST__ = result.host;
  try {
    await result.workflow({ execution_id: executionId });
    assert.equal(result.item.automatic_repair_count, 1);
    assert.equal(result.item.status, "drafted"); // first invocation's snapshot
    assert.deepEqual(result.calls.slice(5, 7), ["check", "close"]);
  } finally {
    delete globalThis.__GEO_CONTENT_TEST_HOST__;
  }
});

test("unresolved needs_repair cannot close or prepare distribution", async () => {
  const calls = [];
  globalThis.__GEO_CONTENT_TEST_HOST__ = {
    executionRead: async () => ({
      execution_id: executionId,
      cycle_id: cycleId,
    }),
    itemsRead: async () => ({
      execution_id: executionId,
      total: 1,
      next_cursor: null,
      items: [
        {
          item_id: itemId,
          branch_key: "frozen-branch",
          status: "needs_repair",
          automatic_repair_count: 2,
        },
      ],
    }),
    async close() {
      calls.push("close");
      throw new Error("incomplete branch");
    },
    async distributionStart() {
      calls.push("distributionStart");
      throw new Error("must not reach distribution");
    },
  };
  try {
    const { main } = await import(entry.href);
    await assert.rejects(main({ execution_id: executionId }));
    assert.deepEqual(calls, ["close"]);
  } finally {
    delete globalThis.__GEO_CONTENT_TEST_HOST__;
  }
});

test("no-progress check cannot spin or prepare distribution", async () => {
  const calls = [];
  globalThis.__GEO_CONTENT_TEST_HOST__ = {
    executionRead: async () => ({
      execution_id: executionId,
      cycle_id: cycleId,
    }),
    itemsRead: async () => ({
      execution_id: executionId,
      total: 1,
      next_cursor: null,
      items: [
        {
          item_id: itemId,
          branch_key: "frozen-branch",
          status: "drafted",
          automatic_repair_count: 0,
        },
      ],
    }),
    async check() {
      calls.push("check");
      return {
        item_id: itemId,
        branch_key: "frozen-branch",
        status: "drafted",
        automatic_repair_count: 0,
      };
    },
    async close() {
      calls.push("close");
      throw new Error("incomplete branch");
    },
    async distributionStart() {
      calls.push("distributionStart");
      throw new Error("must not reach distribution");
    },
  };
  try {
    const { main } = await import(entry.href);
    await assert.rejects(main({ execution_id: executionId }));
    assert.deepEqual(calls, ["check", "close"]);
  } finally {
    delete globalThis.__GEO_CONTENT_TEST_HOST__;
  }
});
