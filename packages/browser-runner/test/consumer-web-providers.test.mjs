import assert from "node:assert/strict";
import { test } from "node:test";
import { adapters } from "../src/adapters.mjs";
import { consumerWebProviders } from "../src/consumer-web-providers.mjs";
import { createRunner } from "../src/runner.mjs";

const pending = ["doubao", "deepseek"];

test("consumer web catalog locks HTTPS entries without conflating identity or measurement", () => {
  assert.ok(Object.isFrozen(consumerWebProviders));
  assert.deepEqual(Object.keys(consumerWebProviders), [
    "kimi",
    ...pending,
    "glm",
  ]);
  for (const [platform, provider] of Object.entries(consumerWebProviders)) {
    assert.ok(Object.isFrozen(provider));
    const url = new URL(provider.entry);
    assert.equal(url.protocol, "https:");
    assert.equal(url.origin, provider.origin);
    assert.equal(url.username, "");
    assert.equal(url.password, "");
    assert.equal(url.search, "");
    assert.equal(url.hash, "");
    assert.equal(adapters[platform].entry, provider.entry);
    assert.equal(provider.loginEntryAvailable, true);
    assert.equal(provider.loginSupported, true);
    assert.deepEqual(
      adapters[platform].operations,
      platform === "kimi" ? ["measure"] : [],
    );
  }
});

test("capabilities expose entry availability separately from supported account completion", async () => {
  const runner = createRunner();
  try {
    const connectors = runner.capabilities().connectors;
    for (const platform of pending) {
      const connector = connectors.find((item) => item.platform === platform);
      assert.equal(connector.login_entry_available, true);
      assert.equal(connector.login_supported, true);
      assert.equal(connector.verified, false);
      assert.deepEqual(connector.operations, []);
    }
    const kimi = connectors.find((item) => item.platform === "kimi");
    assert.equal(kimi.login_supported, true);
    assert.deepEqual(kimi.operations, ["measure"]);
    const glm = connectors.find((item) => item.platform === "glm");
    assert.equal(glm.login_supported, true);
    assert.deepEqual(glm.operations, []);
  } finally {
    await runner.shutdown();
  }
});

test("installed identity probes do not fabricate identity or measurement on missing responses", async () => {
  const page = {
    async evaluate() {
      return null;
    },
  };
  for (const platform of pending) {
    assert.equal(await adapters[platform].identify(page), null);
    const result = await adapters[platform].execute(page, "measure", {});
    assert.equal(result.status, "unsupported");
    assert.equal(result.reason, "measurement_adapter_unavailable");
    assert.deepEqual(result.evidence, []);
    assert.equal(result.connector_version, adapters[platform].connectorVersion);
    assert.equal(adapters[platform].inspectMeasurementOptions, undefined);
  }
});

for (const platform of pending) {
  test(`${platform} entry reuses isolated remote-session lifecycle without saving unverified state`, async () => {
    const events = [];
    const proxy = { server: "http://127.0.0.1:12345" };
    let currentUrl = "about:blank";
    const runner = createRunner({
      interactiveRuntime: "linux-vnc",
      browserType: {
        async launch() {
          throw new Error("fresh login must use shared desktop lifecycle");
        },
      },
      desktopRuntime: {
        async open(options) {
          events.push(["open", options.proxy]);
          return {
            page: {
              url: () => currentUrl,
              async goto(url) {
                currentUrl = url;
                events.push(["goto", url]);
              },
            },
            context: {
              async storageState() {
                throw new Error("unverified account must not be persisted");
              },
            },
            endpoint: () => ({ host: "127.0.0.1", port: 65432 }),
            async closeInput() {
              events.push(["closeInput"]);
            },
            async close() {
              events.push(["close"]);
            },
          };
        },
      },
    });
    try {
      assert.deepEqual(
        await runner.create({
          session_id: "synthetic-session",
          platform,
          proxy,
        }),
        {
          session_id: "synthetic-session",
          phase: "login_required",
        },
      );
      assert.deepEqual(events, [
        ["open", proxy],
        ["goto", consumerWebProviders[platform].entry],
      ]);
      await assert.rejects(
        runner.complete("synthetic-session"),
        /login_required/,
      );
      assert.deepEqual(runner.desktopEndpoint("synthetic-session"), {
        host: "127.0.0.1",
        port: 65432,
      });
      assert.equal(
        events.some(([event]) => event === "closeInput"),
        false,
      );
      await runner.close("synthetic-session");
      assert.deepEqual(events.at(-1), ["close"]);
    } finally {
      await runner.shutdown();
    }
  });
}
