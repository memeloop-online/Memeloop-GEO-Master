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

async function run({
  checks,
  initial = "pending",
  count = 0,
  interruptAfter,
} = {}) {
  const item = {
    item_id: itemId,
    branch_key: "frozen-branch",
    status: initial,
    automatic_repair_count: count,
  };
  const calls = [];
  let interrupted = false;
  const read = () => ({ ...item });
  const host = {
    executionRead: async () => ({
      execution_id: executionId,
      cycle_id: cycleId,
    }),
    itemsRead: async () => ({
      execution_id: executionId,
      total: 1,
      items: [read()],
      next_cursor: null,
    }),
    async prepare() {
      calls.push("prepare");
      item.status = "prepared";
      return read();
    },
    async generate() {
      calls.push("generate");
      item.status = "drafted";
      return read();
    },
    async check() {
      calls.push("check");
      const blocked = checks.shift();
      item.status = blocked
        ? item.automatic_repair_count === 2
          ? "blocked"
          : "needs_repair"
        : "ready";
      return read();
    },
    async repair() {
      calls.push("repair");
      item.status = "drafted";
      item.automatic_repair_count++;
      if (!interrupted && interruptAfter === item.automatic_repair_count) {
        interrupted = true;
        throw new Error("interrupted after persisted repair");
      }
      return read();
    },
    async close() {
      calls.push("close");
      if (
        !["ready", "blocked", "deferred", "not_applicable"].includes(
          item.status,
        )
      ) {
        throw new Error("incomplete branch");
      }
      return { execution_id: executionId, handoff_id: handoffId, total: 1 };
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
    if (interruptAfter) {
      await assert.rejects(main({ execution_id: executionId }));
    } else {
      await main({ execution_id: executionId });
    }
    return { calls, item: read(), host, workflow: main };
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

test("third blocking check is terminal without a third repair", async () => {
  const result = await run({ checks: [true, true, true] });
  assert.equal(result.item.status, "blocked");
  assert.equal(result.calls.filter((call) => call === "repair").length, 2);
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
