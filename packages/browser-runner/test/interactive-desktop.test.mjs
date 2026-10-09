import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { test } from "node:test";
import { createRunner } from "../src/runner.mjs";

function harness({
  clock = () => Date.now(),
  sessionIdleMs = 60_000,
  identify,
  failGoto = false,
  platform = "fixture",
  restoredKimiIdentityWaitMs,
} = {}) {
  const events = [];
  let loggedIn = false;
  const state = {
    cookies: [{ name: "fixture", value: "present" }],
    origins: [],
  };
  const page = {
    url: () => "https://fixture.invalid/login",
    async goto() {
      events.push("goto");
      if (failGoto) throw new Error("synthetic_navigation_failure");
    },
  };
  const context = {
    async storageState() {
      events.push("storageState");
      return state;
    },
    async close() {
      events.push("contextClose");
    },
  };
  const desktopRuntime = {
    async open(options) {
      events.push(["open", options]);
      return {
        context,
        page,
        endpoint() {
          events.push("endpoint");
          return { host: "127.0.0.1", port: 65432 };
        },
        async closeInput() {
          events.push("closeInput");
        },
        async close() {
          events.push("desktopClose");
        },
      };
    },
  };
  const runner = createRunner({
    interactiveRuntime: "linux-vnc",
    desktopRuntime,
    browserType: {
      async launch(options) {
        events.push(["launch", options]);
        return {
          async newContext(contextOptions) {
            events.push(["newContext", contextOptions]);
            return {
              ...context,
              async newPage() {
                return page;
              },
            };
          },
          async close() {
            events.push("browserClose");
          },
        };
      },
    },
    platformAdapters: {
      [platform]: {
        entry: "https://fixture.invalid/login",
        connectorVersion: "fixture.v1",
        operations: ["measure"],
        async identify() {
          events.push("identify");
          if (identify) return identify();
          return loggedIn
            ? { platform_account_id: "synthetic", display_name: "Synthetic" }
            : null;
        },
      },
    },
    clock,
    sessionIdleMs,
    ...(restoredKimiIdentityWaitMs === undefined
      ? {}
      : { restoredKimiIdentityWaitMs }),
  });
  return {
    runner,
    events,
    state,
    login() {
      loggedIn = true;
    },
  };
}

test("saved state on an interactive runner uses a headless context with its proxy", async () => {
  const { runner, events, state, login } = harness();
  const proxy = { server: "http://127.0.0.1:9876" };
  login();
  try {
    assert.deepEqual(
      await runner.create({
        session_id: "restored",
        platform: "fixture",
        proxy,
        storage_state: state,
      }),
      { session_id: "restored", phase: "ready_to_complete" },
    );
    assert.deepEqual(await runner.complete("restored"), {
      identity: {
        platform_account_id: "synthetic",
        display_name: "Synthetic",
      },
      storage_state: state,
    });
    assert.equal(
      events.filter((event) => Array.isArray(event) && event[0] === "open")
        .length,
      0,
    );
    assert.deepEqual(
      events.find((event) => Array.isArray(event) && event[0] === "launch")[1],
      { headless: true },
    );
    assert.deepEqual(
      events.find(
        (event) => Array.isArray(event) && event[0] === "newContext",
      )[1],
      { viewport: { width: 1280, height: 800 }, proxy, storageState: state },
    );
    assert.throws(
      () => runner.desktopEndpoint("restored"),
      (error) => error.code === "desktop_unavailable",
    );
  } finally {
    await runner.shutdown();
  }
});

test("restored Kimi uses headless context and waits for hydration on its page", async () => {
  let ready = false;
  const { runner, events, state } = harness({
    platform: "kimi",
    restoredKimiIdentityWaitMs: 800,
    identify: () =>
      ready ? { platform_account_id: "own", display_name: "Owner" } : null,
  });
  try {
    const created = await runner.create({
      session_id: "restored-kimi-desktop",
      platform: "kimi",
      storage_state: state,
    });
    assert.equal(created.phase, "login_required");
    setTimeout(() => {
      ready = true;
    }, 100);
    const completed = await runner.complete("restored-kimi-desktop");
    assert.equal(completed.identity.platform_account_id, "own");
    assert.equal(completed.storage_state, state);
    assert.equal(events.filter((event) => event === "goto").length, 1);
    assert.ok(events.filter((event) => event === "identify").length >= 2);
    assert.equal(events.filter((event) => event === "closeInput").length, 0);
    assert.equal(
      events.filter((event) => Array.isArray(event) && event[0] === "open")
        .length,
      0,
    );
    assert.deepEqual(
      events.find((event) => Array.isArray(event) && event[0] === "launch")[1],
      { headless: true },
    );
    assert.deepEqual(
      events.find(
        (event) => Array.isArray(event) && event[0] === "newContext",
      )[1].storageState,
      state,
    );
    assert.deepEqual(
      events.filter((event) => typeof event === "string").slice(-2),
      ["identify", "storageState"],
    );
  } finally {
    await runner.shutdown();
  }
});

test("restored Kimi headless session rejects missing or changed identity", async () => {
  let identity = null;
  const { runner, events, state } = harness({
    platform: "kimi",
    restoredKimiIdentityWaitMs: 80,
    identify: () => identity,
  });
  try {
    await runner.create({
      session_id: "missing-kimi-desktop",
      platform: "kimi",
      storage_state: state,
    });
    const begun = performance.now();
    await assert.rejects(
      runner.complete("missing-kimi-desktop"),
      (error) => error.status === 503 && error.code === "identity_not_ready",
    );
    assert.ok(performance.now() - begun < 1_000);
    assert.equal(events.includes("closeInput"), false);

    identity = { platform_account_id: "own", display_name: "Owner" };
    await runner.create({
      session_id: "changed-kimi-desktop",
      platform: "kimi",
      storage_state: state,
    });
    identity = { platform_account_id: "other", display_name: "Other" };
    const before = events.filter((event) => event === "identify").length;
    await assert.rejects(
      runner.complete("changed-kimi-desktop"),
      (error) => error.code === "account_mismatch",
    );
    assert.equal(
      events.filter((event) => event === "identify").length,
      before + 1,
      "a changed account is rejected without retry",
    );
    assert.equal(events.includes("closeInput"), false);
    assert.equal(events.filter((event) => event === "goto").length, 2);
    assert.equal(
      events.filter((event) => Array.isArray(event) && event[0] === "open")
        .length,
      0,
    );
  } finally {
    await runner.shutdown();
  }
});

test("interactive login pending retains input, then revokes input before final identity and storage", async () => {
  const { runner, events, state, login } = harness();
  try {
    const created = await runner.create({
      session_id: "desktop",
      platform: "fixture",
      proxy: { server: "http://127.0.0.1:9876" },
    });
    assert.equal(created.phase, "login_required");
    assert.deepEqual(runner.desktopEndpoint("desktop"), {
      host: "127.0.0.1",
      port: 65432,
    });
    const connection = new EventEmitter();
    connection.terminate = () => {
      events.push("websocketTerminated");
      connection.emit("close");
    };
    runner.attachDesktopClient("desktop", connection);
    await assert.rejects(
      runner.complete("desktop"),
      (e) => e.code === "login_required",
    );
    assert.equal(events.includes("closeInput"), false);
    login();
    const receipt = await runner.complete("desktop");
    assert.equal(receipt.identity.platform_account_id, "synthetic");
    assert.equal(receipt.storage_state, state);
    assert.deepEqual(events.filter((e) => typeof e === "string").slice(-5), [
      "identify",
      "websocketTerminated",
      "closeInput",
      "identify",
      "storageState",
    ]);
    assert.deepEqual(events.find((e) => Array.isArray(e))[1].proxy, {
      server: "http://127.0.0.1:9876",
    });
    assert.equal(
      events.filter((event) => Array.isArray(event) && event[0] === "launch")
        .length,
      0,
    );
    assert.throws(
      () => runner.desktopEndpoint("desktop"),
      (e) => e.code === "desktop_unavailable",
    );
    assert.equal(runner.snapshot, undefined);
    assert.equal(runner.action, undefined);
  } finally {
    await runner.shutdown();
  }
  assert.equal(events.filter((e) => e === "desktopClose").length, 1);
});

test("idle reap, close, failed navigation and shutdown dispose isolated desktops", async () => {
  let now = 0;
  const { runner, events } = harness({ clock: () => now, sessionIdleMs: 100 });
  try {
    await runner.create({ session_id: "expire", platform: "fixture" });
    const connection = new EventEmitter();
    connection.terminate = () => {
      events.push("websocketTerminated");
      connection.emit("close");
    };
    runner.attachDesktopClient("expire", connection);
    now = 101;
    await runner.reap();
    assert.ok(
      events.indexOf("websocketTerminated") < events.indexOf("desktopClose"),
    );
    assert.equal(events.filter((e) => e === "desktopClose").length, 1);
    assert.throws(
      () => runner.desktopEndpoint("expire"),
      (e) => e.code === "session_not_found",
    );
    await runner.create({ session_id: "close", platform: "fixture" });
    await runner.close("close");
    assert.equal(events.filter((e) => e === "desktopClose").length, 2);
    await runner.create({ session_id: "shutdown", platform: "fixture" });
  } finally {
    await runner.shutdown();
  }
  assert.equal(events.filter((e) => e === "desktopClose").length, 3);
});

test("failed navigation and changed identity after input revocation close the browser", async () => {
  const navigation = harness({ failGoto: true });
  try {
    await assert.rejects(
      navigation.runner.create({
        session_id: "navigation",
        platform: "fixture",
      }),
      /synthetic_navigation_failure/,
    );
    assert.equal(
      navigation.events.filter((event) => event === "desktopClose").length,
      1,
    );
  } finally {
    await navigation.runner.shutdown();
  }
  let count = 0;
  const changed = harness({
    identify: () =>
      ++count <= 2
        ? { platform_account_id: "original", display_name: "Synthetic" }
        : { platform_account_id: "different", display_name: "Synthetic" },
  });
  try {
    await changed.runner.create({ session_id: "changed", platform: "fixture" });
    await assert.rejects(
      changed.runner.complete("changed"),
      (error) => error.code === "account_mismatch",
    );
    assert.equal(
      changed.events.filter((event) => event === "desktopClose").length,
      1,
    );
    assert.throws(
      () => changed.runner.desktopEndpoint("changed"),
      (error) => error.code === "session_not_found",
    );
  } finally {
    await changed.runner.shutdown();
  }
});

test("interactive runtime selection rejects unsupported deployments", () => {
  assert.throws(
    () => createRunner({ interactiveRuntime: "unknown" }),
    /invalid_interactive_runtime/,
  );
});
