import assert from "node:assert/strict";
import { test } from "node:test";
import { adapters } from "../src/adapters.mjs";
import { createRunner } from "../src/runner.mjs";

test("publication operations distinguish public readback, submit-only, and unavailable adapters", () => {
  assert.deepEqual(adapters.zhihu.operations, [
    "publish",
    "lookup",
    "withdraw",
  ]);
  assert.deepEqual(adapters.baidu_creator.operations, ["publish"]);
  assert.deepEqual(adapters.xiaohongshu.operations, []);
});

test("unimplemented publication operations retain explicit unsupported results", async () => {
  for (const [platform, operation, reason] of [
    ["baidu_creator", "lookup", "public_lookup_unverified"],
    ["xiaohongshu", "publish", "media_or_article_flow_unverified"],
    ["xiaohongshu", "lookup", "media_or_article_flow_unverified"],
  ]) {
    const result = await adapters[platform].execute(null, operation, {});
    assert.equal(result.status, "unsupported");
    assert.equal(result.reason, reason);
    assert.deepEqual(result.evidence, []);
    assert.equal(result.connector_version, adapters[platform].connectorVersion);
  }
});

test("runner exposes truthful publication capabilities while retaining account login entries", async () => {
  const runner = createRunner({
    browserType: {
      async launch() {
        assert.fail("capability discovery must not launch a browser");
      },
    },
  });
  try {
    const connectors = runner.capabilities().connectors;
    for (const [platform, operations] of [
      ["zhihu", ["publish", "lookup"]],
      ["baidu_creator", ["publish"]],
      ["xiaohongshu", []],
    ]) {
      const connector = connectors.find((item) => item.platform === platform);
      assert.ok(connector, platform);
      assert.deepEqual(connector.operations, operations, platform);
      assert.equal(connector.login_entry_available, true, platform);
      assert.equal(connector.login_supported, true, platform);
      assert.equal(connector.model_discovery_supported, false, platform);
      assert.equal(connector.verified, false, platform);
      assert.equal(
        connector.connector_version,
        adapters[platform].connectorVersion,
      );
    }
  } finally {
    await runner.shutdown();
  }
});
