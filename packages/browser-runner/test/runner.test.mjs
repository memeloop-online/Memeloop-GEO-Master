import assert from "node:assert/strict";
import { createServer } from "node:http";
import { after, before, test } from "node:test";
import { chromium } from "playwright";
import { createRunner, RunnerError } from "../src/runner.mjs";
import { createRunnerServer } from "../src/server.mjs";

let fixture;
let url;
let runner;
let api;
let apiUrl;
const testBrowserOptions = process.env.GEO_TEST_CHROMIUM_PATH
  ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
  : {};

before(async () => {
  fixture = createServer((request, response) => {
    response.writeHead(200, { "content-type": "text/html" });
    response.end(`<!doctype html><html><body>
      <input id="account" aria-label="Account">
      <button id="connect" onclick="document.body.dataset.connected =
        document.querySelector('#account').value">Connect</button>
      <div id="identity"></div>
      <script>
        const stored = localStorage.getItem("account");
        if (stored) document.body.dataset.connected = stored;
      </script>
    </body></html>`);
  });
  await new Promise((resolve) => fixture.listen(0, "127.0.0.1", resolve));
  url = `http://127.0.0.1:${fixture.address().port}/`;
  const fixtureAdapter = {
    connectorVersion: "fixture.v1",
    entry: url,
    operations: ["publish", "measure", "lookup"],
    async inspectMeasurementOptions() {
      return {
        models: [{ id: "fixture-model", label: "Fixture model" }],
        selected_model: null,
      };
    },
    async identify(page) {
      const name = await page.locator("body").getAttribute("data-connected");
      return name
        ? { platform_account_id: `fixture:${name}`, display_name: name }
        : null;
    },
    async execute(page, operation, payload) {
      if (operation === "measure") throw new Error("outcome_lost_after_submit");
      if (operation !== "lookup") {
        return { status: "unsupported", reason: "fixture_only", evidence: [] };
      }
      return {
        execution_id: "spoofed-execution",
        connector_version: "spoofed-verified.v9",
        provenance: "live",
        fixture: false,
        status: "completed",
        evidence: [
          {
            kind: "readback",
            value: await page.locator("body").getAttribute("data-connected"),
          },
        ],
        result: { query: payload.query },
      };
    },
  };
  runner = createRunner({
    browserType: {
      launch: (options) =>
        chromium.launch({ ...options, ...testBrowserOptions }),
    },
    platformAdapters: { fixture: fixtureAdapter },
  });
  api = createRunnerServer({ token: "internal-test-token", runner });
  await new Promise((resolve) => api.listen(0, "127.0.0.1", resolve));
  apiUrl = `http://127.0.0.1:${api.address().port}`;
});

after(async () => {
  await runner?.shutdown();
  await new Promise((resolve) => api?.close(resolve));
  await new Promise((resolve) => fixture?.close(resolve));
});

async function request(
  path,
  method = "GET",
  body,
  token = "internal-test-token",
) {
  const response = await fetch(`${apiUrl}${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return { status: response.status, body: await response.json() };
}

test("startup rejects missing internal token", () => {
  assert.throws(
    () => createRunnerServer({ token: "", runner }),
    /TOKEN_required/,
  );
});

test("only the unmodified installed runner has live execution provenance", async () => {
  const installed = createRunner();
  const injectedAdapters = createRunner({ platformAdapters: {} });
  const injectedBrowser = createRunner({
    browserType: {
      async launch() {
        throw new Error("not_executed");
      },
    },
  });
  const explicitDefaultAdapters = createRunner({ platformAdapters: undefined });
  try {
    assert.equal(installed.executionProvenance, "live");
    assert.equal(injectedAdapters.executionProvenance, "fixture");
    assert.equal(injectedBrowser.executionProvenance, "fixture");
    assert.equal(explicitDefaultAdapters.executionProvenance, "fixture");
  } finally {
    await Promise.all([
      installed.shutdown(),
      injectedAdapters.shutdown(),
      injectedBrowser.shutdown(),
      explicitDefaultAdapters.shutdown(),
    ]);
  }
});

test("authenticated capabilities describe the running adapters without verification claims", async () => {
  assert.deepEqual(
    await request("/v1/capabilities", "GET", undefined, "wrong"),
    { status: 401, body: { error: "unauthorized" } },
  );
  assert.deepEqual(await request("/v1/capabilities"), {
    status: 200,
    body: {
      connectors: [
        {
          platform: "fixture",
          placement_slot: "primary",
          connector_version: "fixture.v1",
          operations: ["publish", "measure", "lookup"],
          verified: false,
        },
      ],
    },
  });
  assert.deepEqual(
    await request("/v1/capabilities?connector_version=verified", "GET"),
    await request("/v1/capabilities"),
    "caller cannot override running adapter metadata",
  );
});

test("auth, fixed platform list, and retired control routes", async () => {
  assert.equal(
    (await request("/v1/sessions", "POST", {}, "wrong")).status,
    401,
  );
  assert.deepEqual(
    (
      await request("/v1/sessions", "POST", {
        session_id: "bad",
        platform: "example",
        url,
      })
    ).body,
    { error: "invalid_session" },
  );
  assert.deepEqual(
    (
      await request("/v1/sessions", "POST", {
        session_id: "bad",
        platform: "example",
      })
    ).body,
    { error: "unsupported_platform" },
  );
  assert.deepEqual(
    await request("/v1/sessions", "POST", {
      session_id: "headless-login",
      platform: "fixture",
    }),
    { status: 503, body: { error: "capability_missing" } },
  );
  await request("/v1/sessions", "POST", {
    session_id: "control",
    platform: "fixture",
    storage_state: { cookies: [], origins: [] },
  });
  assert.deepEqual(
    (
      await request("/v1/sessions/control/actions", "POST", {
        kind: "navigate",
        url,
      })
    ).body,
    { error: "not_found" },
  );
  assert.deepEqual(
    (
      await request("/v1/sessions/control/actions", "POST", {
        kind: "key",
        key: "ControlOrMeta+L",
      })
    ).body,
    { error: "not_found" },
  );
  assert.deepEqual((await request("/v1/sessions/control/snapshot")).body, {
    error: "not_found",
  });
  assert.deepEqual(
    (await request("/v1/sessions/control/complete", "POST")).body,
    {
      error: "login_required",
    },
  );
});

test("headless sessions require storage state and preserve the execution ledger", async () => {
  const created = await request("/v1/sessions", "POST", {
    session_id: "connected",
    platform: "fixture",
    storage_state: {
      cookies: [],
      origins: [
        {
          origin: new URL(url).origin,
          localStorage: [{ name: "account", value: "Fixture user" }],
        },
      ],
    },
  });
  assert.deepEqual(created, {
    status: 201,
    body: { session_id: "connected", phase: "ready_to_complete" },
  });
  const completed = await request("/v1/sessions/connected/complete", "POST");
  assert.equal(completed.status, 200);
  assert.deepEqual(completed.body.identity, {
    platform_account_id: "fixture:Fixture user",
    display_name: "Fixture user",
  });
  assert.ok(Array.isArray(completed.body.storage_state.cookies));
  const options = await request("/v1/sessions/connected/measurement-options");
  assert.equal(options.status, 200);
  assert.deepEqual(options.body, {
    models: [{ id: "fixture-model", label: "Fixture model" }],
    selected_model: null,
  });
  assert.equal(
    (
      await request(
        "/v1/sessions/connected/measurement-options",
        "GET",
        undefined,
        "wrong",
      )
    ).status,
    401,
  );
  const execution = {
    execution_id: "lookup-1",
    session_id: "connected",
    operation: "lookup",
    payload: { query: "x" },
  };
  const first = await request("/v1/executions", "POST", execution);
  assert.deepEqual(first.body, {
    execution_id: "lookup-1",
    connector_version: "fixture.v1",
    provenance: "fixture",
    status: "completed",
    evidence: [{ kind: "readback", value: "Fixture user" }],
    result: { query: "x" },
  });
  assert.deepEqual(
    (await request("/v1/executions", "POST", execution)).body,
    first.body,
  );
  assert.deepEqual(
    (
      await request("/v1/executions", "POST", {
        ...execution,
        payload: { query: "other" },
      })
    ).body,
    { error: "execution_conflict" },
  );
  assert.deepEqual(
    (
      await request("/v1/executions", "POST", {
        execution_id: "publish-1",
        session_id: "connected",
        operation: "publish",
        payload: {},
      })
    ).body,
    {
      execution_id: "publish-1",
      connector_version: "fixture.v1",
      provenance: "fixture",
      status: "unsupported",
      reason: "fixture_only",
      evidence: [],
    },
  );
  assert.deepEqual(
    (
      await request("/v1/executions", "POST", {
        execution_id: "measure-1",
        session_id: "connected",
        operation: "measure",
        payload: {},
      })
    ).body,
    {
      execution_id: "measure-1",
      connector_version: "fixture.v1",
      provenance: "fixture",
      status: "unknown",
      evidence: [],
    },
  );
  assert.deepEqual((await request("/v1/sessions/connected", "DELETE")).body, {
    closed: true,
  });
  assert.deepEqual((await request("/v1/sessions/connected/status")).body, {
    error: "session_not_found",
  });
});

test("restored Kimi identity waits on the same page for website hydration", async () => {
  let ready = false;
  let newPages = 0;
  let navigations = 0;
  let attempts = 0;
  const originalUrl = "https://www.kimi.com/";
  const userInput = "unfinished input";
  const state = { cookies: [], origins: [] };
  const page = {
    draftInput: userInput,
    url: () => originalUrl,
    async goto() {
      navigations++;
    },
  };
  const restored = createRunner({
    restoredKimiIdentityWaitMs: 800,
    browserType: {
      async launch() {
        return {
          async newContext() {
            return {
              async newPage() {
                newPages++;
                return page;
              },
              async storageState() {
                return state;
              },
              async close() {},
            };
          },
          async close() {},
        };
      },
    },
    platformAdapters: {
      kimi: {
        entry: originalUrl,
        async identify(received) {
          assert.equal(received, page);
          attempts++;
          return ready
            ? { platform_account_id: "own", display_name: "Own account" }
            : null;
        },
      },
    },
  });
  try {
    assert.deepEqual(
      await restored.create({
        session_id: "hydrate",
        platform: "kimi",
        storage_state: state,
      }),
      { session_id: "hydrate", phase: "login_required" },
    );
    setTimeout(() => {
      ready = true;
    }, 100);
    assert.deepEqual(await restored.complete("hydrate"), {
      identity: { platform_account_id: "own", display_name: "Own account" },
      storage_state: state,
    });
    assert.ok(
      attempts >= 3,
      "completion retried the transient missing identity",
    );
    assert.equal(newPages, 1);
    assert.equal(navigations, 1);
    assert.equal(page.url(), originalUrl);
    assert.equal(page.draftInput, "unfinished input");
  } finally {
    await restored.shutdown();
  }
});

test("restored Kimi identity stops at the deadline and never retries a different account", async () => {
  let identity = null;
  let attempts = 0;
  let pages = 0;
  let navigations = 0;
  const page = {
    url: () => "https://www.kimi.com/",
    async goto() {
      navigations++;
    },
  };
  const restored = createRunner({
    restoredKimiIdentityWaitMs: 90,
    browserType: {
      async launch() {
        return {
          async newContext() {
            return {
              async newPage() {
                pages++;
                return page;
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
    platformAdapters: {
      kimi: {
        entry: "https://www.kimi.com/",
        async identify() {
          attempts++;
          return identity;
        },
      },
    },
  });
  try {
    await restored.create({
      session_id: "never-ready",
      platform: "kimi",
      storage_state: { cookies: [], origins: [] },
    });
    const began = performance.now();
    await assert.rejects(
      restored.complete("never-ready"),
      (error) =>
        error instanceof RunnerError && error.code === "login_required",
    );
    assert.ok(performance.now() - began < 1_000);
    assert.ok(attempts >= 3 && attempts <= 5);
    assert.equal(pages, 1);
    assert.equal(navigations, 1);

    identity = { platform_account_id: "own", display_name: "Own account" };
    await restored.create({
      session_id: "changed",
      platform: "kimi",
      storage_state: { cookies: [], origins: [] },
    });
    identity = { platform_account_id: "other", display_name: "Other account" };
    const before = attempts;
    await assert.rejects(
      restored.complete("changed"),
      (error) =>
        error instanceof RunnerError && error.code === "account_mismatch",
    );
    assert.equal(
      attempts,
      before + 1,
      "a mismatched identity is never retried",
    );
    assert.equal(pages, 2);
    assert.equal(navigations, 2);
  } finally {
    await restored.shutdown();
  }
});

test("proxy is passed to isolated browser context without direct retry", async () => {
  const proxy = {
    server: "http://127.0.0.1:9999",
    username: "user",
    password: "secret",
  };
  const contexts = [];
  const fakeBrowser = {
    async newContext(options) {
      contexts.push(options);
      return {
        async newPage() {
          return {
            async goto() {
              throw new Error("proxy_unavailable");
            },
          };
        },
        async close() {},
      };
    },
    async close() {},
  };
  const isolated = createRunner({
    browserType: {
      async launch() {
        return fakeBrowser;
      },
    },
    platformAdapters: { fixture: { entry: url } },
  });
  await assert.rejects(
    isolated.create({
      session_id: "proxied",
      platform: "fixture",
      proxy,
      storage_state: { cookies: [], origins: [] },
    }),
    /proxy_unavailable/,
  );
  assert.equal(contexts.length, 1);
  assert.deepEqual(contexts[0].proxy, proxy);
  await isolated.shutdown();
});

test("the host stamps every execution outcome and ignores adapter metadata", async () => {
  let pageUrl = "https://fixture.invalid/";
  const adapter = {
    connectorVersion: "fixture.original.v1",
    entry: pageUrl,
    operations: ["lookup", "measure"],
    async identify() {
      return { platform_account_id: "own-1", display_name: "Owner" };
    },
    async execute(_page, operation) {
      if (operation === "measure") throw new Error("outcome_lost");
      return {
        execution_id: "forged-id",
        connector_version: "verified.forged.v9",
        provenance: "live",
        fixture: false,
        status: "completed",
        evidence: [],
      };
    },
  };
  const isolated = createRunner({
    browserType: {
      async launch() {
        return {
          async newContext() {
            return {
              async newPage() {
                return { url: () => pageUrl, async goto() {} };
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
    platformAdapters: { fixture: adapter },
  });
  try {
    await isolated.create({
      session_id: "receipt",
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    });
    await isolated.complete("receipt");
    adapter.connectorVersion = "fixture.changed.v2";
    const run = (execution_id, operation) =>
      isolated.execute({
        execution_id,
        session_id: "receipt",
        operation,
        payload: {},
      });
    const stamp = {
      connector_version: "fixture.original.v1",
      provenance: "fixture",
    };
    assert.deepEqual(await run("success", "lookup"), {
      execution_id: "success",
      ...stamp,
      status: "completed",
      evidence: [],
    });
    assert.deepEqual(await run("failure", "measure"), {
      execution_id: "failure",
      ...stamp,
      status: "unknown",
      evidence: [],
    });
    assert.deepEqual(await run("unsupported", "publish"), {
      execution_id: "unsupported",
      ...stamp,
      status: "unsupported",
      reason: "operation_not_supported",
      evidence: [],
    });
    pageUrl = "https://fixture.invalid/challenge";
    assert.deepEqual(await run("challenged", "lookup"), {
      execution_id: "challenged",
      ...stamp,
      status: "challenge",
      evidence: [],
    });
  } finally {
    await isolated.shutdown();
  }
});

test("idle contexts and settled cache expire, but active execution is never reaped", async () => {
  let now = 0;
  let closed = 0;
  let calls = 0;
  let finishFirst;
  const page = {
    url: () => "http://127.0.0.1/",
    async goto() {},
  };
  const context = {
    async newPage() {
      return page;
    },
    async storageState() {
      return { cookies: [], origins: [] };
    },
    async close() {
      closed++;
    },
  };
  const expiring = createRunner({
    browserType: {
      async launch() {
        return {
          async newContext() {
            return context;
          },
          async close() {},
        };
      },
    },
    platformAdapters: {
      fixture: {
        connectorVersion: "fixture.cache.v1",
        entry: "http://127.0.0.1/",
        async identify() {
          return { platform_account_id: "own-1", display_name: "Owner" };
        },
        async execute() {
          calls++;
          if (calls === 1) {
            await new Promise((resolve) => {
              finishFirst = resolve;
            });
          }
          return { status: "completed", evidence: [] };
        },
        operations: ["lookup"],
      },
    },
    clock: () => now,
    sessionIdleMs: 100,
    executionRetentionMs: 10,
    maintenanceIntervalMs: 10_000,
  });
  try {
    await expiring.create({
      session_id: "ttl",
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    });
    await expiring.complete("ttl");
    const input = {
      execution_id: "ttl-execution",
      session_id: "ttl",
      operation: "lookup",
      payload: {},
    };
    const first = expiring.execute(input);
    await new Promise((resolve) => setImmediate(resolve));
    now = 1000;
    await expiring.reap();
    assert.equal(closed, 0);
    await assert.rejects(
      expiring.close("ttl"),
      (error) => error.code === "session_busy",
    );
    await assert.rejects(
      expiring.execute({ ...input, execution_id: "different-in-flight" }),
      (error) => error.code === "session_busy",
    );
    const duplicate = expiring.execute(input);
    finishFirst();
    assert.deepEqual(await duplicate, await first);
    assert.deepEqual(await first, {
      execution_id: "ttl-execution",
      connector_version: "fixture.cache.v1",
      provenance: "fixture",
      status: "completed",
      evidence: [],
    });
    assert.equal(calls, 1);
    now = 1011;
    await expiring.reap();
    assert.equal((await expiring.execute(input)).status, "completed");
    assert.equal(calls, 2);
    now = 2000;
    await expiring.reap();
    assert.equal(closed, 1);
    await assert.rejects(
      expiring.status("ttl"),
      (error) => error.code === "session_not_found",
    );
  } finally {
    await expiring.shutdown();
  }
});

test("a stalled execution expires as unknown and closes its context", async () => {
  let closed = 0;
  const stalled = createRunner({
    browserType: {
      async launch() {
        return {
          async newContext() {
            return {
              async newPage() {
                return {
                  url: () => "http://127.0.0.1/",
                  async goto() {},
                };
              },
              async storageState() {
                return { cookies: [], origins: [] };
              },
              async close() {
                closed++;
              },
            };
          },
          async close() {},
        };
      },
    },
    platformAdapters: {
      fixture: {
        connectorVersion: "fixture.timeout.v1",
        entry: "http://127.0.0.1/",
        operations: ["publish"],
        async identify() {
          return { platform_account_id: "own-1", display_name: "Owner" };
        },
        async execute() {
          return new Promise(() => {});
        },
      },
    },
    executionTimeoutMs: 20,
  });
  try {
    await stalled.create({
      session_id: "stalled",
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    });
    await stalled.complete("stalled");
    const result = await stalled.execute({
      execution_id: "stalled-attempt",
      session_id: "stalled",
      operation: "publish",
      payload: { title: "T", body: "B" },
    });
    assert.deepEqual(result, {
      execution_id: "stalled-attempt",
      connector_version: "fixture.timeout.v1",
      provenance: "fixture",
      status: "unknown",
      reason: "execution_deadline",
      evidence: [],
    });
    assert.equal(closed, 1);
    await assert.rejects(
      stalled.status("stalled"),
      (error) => error.code === "session_not_found",
    );
  } finally {
    await stalled.shutdown();
  }
});
