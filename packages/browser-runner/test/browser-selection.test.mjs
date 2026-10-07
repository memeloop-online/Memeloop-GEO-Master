import assert from "node:assert/strict";
import { test } from "node:test";
import { createRunner } from "../src/runner.mjs";

function captureBrowser() {
  const launches = [];
  const contexts = [];
  const page = {
    url: () => "https://fixture.invalid/login",
    async goto() {},
  };
  const browserType = {
    async launch(options) {
      launches.push(options);
      return {
        async newContext(options) {
          contexts.push(options);
          return {
            async newPage() {
              return page;
            },
            async close() {},
          };
        },
        async close() {},
      };
    },
  };
  return { browserType, launches, contexts };
}

function fixtureAdapter() {
  return {
    entry: "https://fixture.invalid/login",
    async identify() {
      return null;
    },
  };
}

test("deployment browser selection is bounded and validated before launch", () => {
  const capture = captureBrowser();
  assert.throws(
    () =>
      createRunner({
        browserType: capture.browserType,
        browserChannel: "firefox",
        platformAdapters: { fixture: fixtureAdapter() },
      }),
    /invalid_browser_channel/,
  );
  assert.equal(capture.launches.length, 0);
});

test("bundled chromium remains the default when no channel is configured", async () => {
  const capture = captureBrowser();
  const runner = createRunner({
    browserType: capture.browserType,
    browserChannel: undefined,
    platformAdapters: { fixture: fixtureAdapter() },
  });
  try {
    await runner.create({
      session_id: "default",
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    });
    assert.deepEqual(capture.launches, [{ headless: true }]);
  } finally {
    await runner.shutdown();
  }
});

test("configured browsers use only their allowlisted Playwright channels", async () => {
  for (const browserChannel of ["chromium", "chrome", "msedge"]) {
    const capture = captureBrowser();
    const runner = createRunner({
      browserType: capture.browserType,
      browserChannel,
      platformAdapters: { fixture: fixtureAdapter() },
    });
    try {
      await runner.create({
        session_id: browserChannel,
        platform: "fixture",
        storage_state: { cookies: [], origins: [] },
        proxy: {
          server: "http://127.0.0.1:9999",
          username: "fixture-user",
          password: "fixture-password",
        },
      });
      assert.deepEqual(capture.launches, [
        { headless: true, channel: browserChannel },
      ]);
      assert.deepEqual(capture.contexts[0].proxy, {
        server: "http://127.0.0.1:9999",
        username: "fixture-user",
        password: "fixture-password",
      });
    } finally {
      await runner.shutdown();
    }
  }
});

test("session requests cannot choose a browser or executable", async () => {
  const capture = captureBrowser();
  const runner = createRunner({
    browserType: capture.browserType,
    browserChannel: "chrome",
    platformAdapters: { fixture: fixtureAdapter() },
  });
  try {
    for (const [sessionId, requestConfig] of [
      ["request-channel", { channel: "msedge" }],
      ["request-path", { executable_path: "/untrusted/browser" }],
      ["request-camel-path", { executablePath: "/untrusted/browser" }],
    ]) {
      await assert.rejects(
        runner.create({
          session_id: sessionId,
          platform: "fixture",
          ...requestConfig,
        }),
        (error) => error.code === "invalid_session",
      );
    }
    assert.equal(capture.launches.length, 0);
  } finally {
    await runner.shutdown();
  }
});
