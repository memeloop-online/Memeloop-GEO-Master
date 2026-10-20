import assert from "node:assert/strict";
import { test } from "node:test";
import { adapters } from "../src/adapters.mjs";
import { createRunner } from "../src/runner.mjs";

const unidentifiedPage = {
  context() {
    return {
      async newPage() {
        return { async close() {} };
      },
    };
  },
};

test("one platform login advertises separate article and short publication placements", async () => {
  const runner = createRunner({
    browserType: {
      async launch() {
        assert.fail("capability discovery must not start a browser");
      },
    },
  });
  try {
    const connectors = runner.capabilities().connectors;
    const publication = connectors.filter((item) => item.platform === "zhihu");
    assert.deepEqual(
      publication.map((item) => item.placement_slot),
      ["primary", "short_post"],
    );
    for (const connector of publication) {
      assert.deepEqual(connector.operations, ["publish", "lookup"]);
      assert.equal(connector.login_entry_available, true);
      assert.equal(connector.login_supported, true);
      assert.equal(connector.verified, false);
      assert.equal(
        connector.connector_version,
        adapters.zhihu.connectorVersion,
      );
    }
    assert.equal(
      connectors.some((item) => item.operations.includes("withdraw")),
      false,
    );
    assert.equal(
      Object.keys(adapters).some((key) => key.includes("short_post")),
      false,
    );
    assert.deepEqual(
      connectors
        .filter((item) => item.platform === "baidu_creator")
        .map((item) => item.placement_slot),
      ["primary"],
    );
  } finally {
    await runner.shutdown();
  }
});

test("short publication routes to its body editor and never falls back to an article", async () => {
  const outcome = await adapters.zhihu.execute(unidentifiedPage, "publish", {
    placement_slot: "short_post",
    body: "Synthetic short body",
  });
  assert.equal(outcome.status, "unknown");
  assert.equal(outcome.reason, "account_ownership_unverified");

  const titled = await adapters.zhihu.execute(null, "publish", {
    placement_slot: "short_post",
    title: "Synthetic article title",
    body: "Synthetic short body",
  });
  assert.equal(titled.status, "unsupported");
  assert.equal(titled.reason, "short_post_title_unverified");

  const unsupported = await adapters.zhihu.execute(null, "publish", {
    placement_slot: "unavailable",
    title: "Synthetic title",
    body: "Synthetic body",
  });
  assert.equal(unsupported.status, "unsupported");
  assert.equal(unsupported.reason, "placement_not_supported");
});

test("short lookup resolves only an exact canonical short URL or its matching external ID", async () => {
  for (const reference of [
    { public_url: "https://www.zhihu.com/pin/42" },
    { public_url: "https://www.zhihu.com/pin/42/", external_id: "42" },
    { external_id: "42" },
  ]) {
    const outcome = await adapters.zhihu.execute(unidentifiedPage, "lookup", {
      placement_slot: "short_post",
      body: "Synthetic short body",
      ...reference,
    });
    assert.equal(outcome.status, "unknown");
    assert.equal(outcome.reason, "account_ownership_unverified");
  }
  for (const public_url of [
    "https://zhuanlan.zhihu.com/p/42",
    "https://www.zhihu.com/pin/42?source=untrusted",
    "https://www.zhihu.com/pin/42#item",
    "https://untrusted.invalid/pin/42",
    "https://user@www.zhihu.com/pin/42",
    "https://www.zhihu.com/pin/43",
  ]) {
    const outcome = await adapters.zhihu.execute(null, "lookup", {
      placement_slot: "short_post",
      body: "Synthetic short body",
      external_id: "42",
      public_url,
    });
    assert.equal(outcome.status, "unsupported", public_url);
    assert.equal(outcome.reason, "invalid_lookup_payload", public_url);
  }
});

test("explicit primary placement retains the existing article payload for article platforms", async () => {
  for (const platform of ["zhihu", "baidu_creator"]) {
    let navigations = 0;
    const page = {
      async goto() {
        navigations++;
      },
      url: () => "https://example.invalid/signin/login",
    };
    const payload = {
      placement_slot: "primary",
      title: "Synthetic title",
      body: "Synthetic body",
    };
    const outcome = await adapters[platform].execute(page, "publish", payload);
    assert.equal(outcome.status, "login_required", platform);
    assert.equal(navigations, 1, platform);
  }
});

async function fixtureRunner() {
  const calls = [];
  const runner = createRunner({
    platformAdapters: {
      fixture: {
        connectorVersion: "fixture.placement.v1",
        entry: "https://example.invalid/",
        operations: ["publish", "lookup"],
        placements: {
          primary: { operations: ["publish"] },
          short_post: { operations: ["lookup"] },
        },
        async identify() {
          return {
            platform_account_id: "fixture-owner",
            display_name: "Owner",
          };
        },
        async execute(_page, operation, payload, network) {
          calls.push({ operation, payload, network });
          return { status: "completed", evidence: [] };
        },
      },
    },
    browserType: {
      async launch() {
        return {
          async newContext() {
            return {
              async newPage() {
                return {
                  url: () => "https://example.invalid/",
                  async goto() {},
                };
              },
              async storageState() {
                return { cookies: [], origins: [] };
              },
              async close() {},
            };
          },
          async close() {},
        };
      },
    },
  });
  await runner.create({
    session_id: "fixture-session",
    platform: "fixture",
    storage_state: { cookies: [], origins: [] },
  });
  await runner.complete("fixture-session");
  return { runner, calls };
}

test("runner checks the selected placement operations and retains its JSON execution envelope", async () => {
  const { runner, calls } = await fixtureRunner();
  const execute = (id, operation, payload) =>
    runner.execute({
      execution_id: id,
      session_id: "fixture-session",
      operation,
      payload,
    });
  try {
    const shortPayload = {
      placement_slot: "short_post",
      body: "Synthetic body",
      external_id: "42",
    };
    const outcome = await execute("short-lookup", "lookup", shortPayload);
    assert.equal(outcome.status, "completed");
    assert.equal(outcome.provenance, "fixture");
    assert.deepEqual(calls[0].payload, shortPayload);
    assert.equal(calls[0].network.expectedAccountId, "fixture-owner");
    assert.deepEqual(
      runner.capabilities().connectors.map((item) => item.operations),
      [["publish"], ["lookup"]],
    );

    for (const [id, operation, placement_slot] of [
      ["short-publish", "publish", "short_post"],
      ["article-lookup", "lookup", "primary"],
      ["unknown-placement", "publish", "unknown"],
      ["null-placement", "publish", null],
      ["object-placement", "publish", {}],
      ["prototype-placement", "publish", "__proto__"],
    ]) {
      const rejected = await execute(id, operation, {
        placement_slot,
        body: "Synthetic body",
      });
      assert.equal(rejected.status, "unsupported");
      assert.equal(rejected.reason, "operation_not_supported");
    }
    assert.equal(calls.length, 1);

    assert.equal(
      (
        await execute("article-publish", "publish", {
          placement_slot: "primary",
          title: "Synthetic title",
          body: "Synthetic body",
        })
      ).status,
      "completed",
    );
    assert.equal(
      (
        await execute("default-publish", "publish", {
          title: "Synthetic title",
          body: "Synthetic body",
        })
      ).status,
      "completed",
    );
    await assert.rejects(
      execute("short-lookup", "lookup", {
        ...shortPayload,
        placement_slot: "primary",
      }),
      /execution_conflict/,
    );
    await assert.rejects(
      runner.execute({
        execution_id: "bad-envelope",
        session_id: "fixture-session",
        operation: "publish",
        placement_slot: "short_post",
        payload: { body: "Synthetic body" },
      }),
      /invalid_execution/,
    );
  } finally {
    await runner.shutdown();
  }
});
