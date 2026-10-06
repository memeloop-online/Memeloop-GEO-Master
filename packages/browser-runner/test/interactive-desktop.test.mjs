import assert from "node:assert/strict";
import { test } from "node:test";
import { createRunner } from "../src/runner.mjs";

function harness({
  clock = () => Date.now(),
  sessionIdleMs = 60_000,
  identify,
  failGoto = false,
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
          return { host: "127.0.0.1", port: 65432, password: "internal-only" };
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
      async launch() {
        throw new Error("shared browser forbidden");
      },
    },
    platformAdapters: {
      fixture: {
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

test("interactive login pending retains input, then revokes input before final identity and storage", async () => {
  const { runner, events, state, login } = harness();
  try {
    const created = await runner.create({
      session_id: "desktop",
      platform: "fixture",
      proxy: { server: "http://127.0.0.1:9876" },
      storage_state: { cookies: [], origins: [] },
    });
    assert.equal(created.phase, "login_required");
    assert.deepEqual(runner.desktopEndpoint("desktop"), {
      host: "127.0.0.1",
      port: 65432,
      password: "internal-only",
    });
    await assert.rejects(
      runner.complete("desktop"),
      (e) => e.code === "login_required",
    );
    assert.equal(events.includes("closeInput"), false);
    login();
    const receipt = await runner.complete("desktop");
    assert.equal(receipt.identity.platform_account_id, "synthetic");
    assert.equal(receipt.storage_state, state);
    assert.deepEqual(events.filter((e) => typeof e === "string").slice(-4), [
      "identify",
      "closeInput",
      "identify",
      "storageState",
    ]);
    assert.deepEqual(events.find((e) => Array.isArray(e))[1].proxy, {
      server: "http://127.0.0.1:9876",
    });
    assert.throws(
      () => runner.desktopEndpoint("desktop"),
      (e) => e.code === "desktop_unavailable",
    );
    await assert.rejects(
      runner.snapshot("desktop"),
      (e) => e.code === "desktop_session_required",
    );
    await assert.rejects(
      runner.action("desktop", { kind: "click", x: 1, y: 1 }),
      (e) => e.code === "desktop_session_required",
    );
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
    now = 101;
    await runner.reap();
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
