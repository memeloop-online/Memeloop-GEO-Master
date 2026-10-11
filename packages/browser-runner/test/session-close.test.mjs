import assert from "node:assert/strict";
import { test } from "node:test";
import { createRunner } from "../src/runner.mjs";
import { createRunnerServer } from "../src/server.mjs";

const deferred = () => Promise.withResolvers();
const errorCode = (code) => (error) => error.code === code;

function harness({
  interactive = false,
  opening,
  disposal,
  navigation,
  identify,
} = {}) {
  let closes = 0;
  let navigations = 0;
  const page = {
    url: () => "https://fixture.invalid/",
    async goto() {
      navigations++;
      await navigation?.promise;
    },
  };
  const close = async () => {
    closes++;
    await disposal?.(closes);
  };
  const context = {
    close,
    async newPage() {
      return page;
    },
  };
  const runner = createRunner({
    browserType: {
      async launch() {
        return {
          async newContext() {
            await opening?.promise;
            return context;
          },
          async close() {},
        };
      },
    },
    ...(interactive
      ? {
          interactiveRuntime: "linux-vnc",
          desktopRuntime: {
            async open({ onSession }) {
              const resource = { close };
              onSession(resource);
              await opening?.promise;
              return { ...resource, context, page };
            },
          },
        }
      : {}),
    platformAdapters: {
      fixture: {
        entry: "https://fixture.invalid/",
        connectorVersion: "fixture.v1",
        operations: [],
        async identify() {
          return identify ? identify() : null;
        },
      },
    },
  });
  return {
    runner,
    create: () =>
      runner.create({
        session_id: "fixed",
        platform: "fixture",
        ...(interactive ? {} : { storage_state: { cookies: [], origins: [] } }),
      }),
    get closes() {
      return closes;
    },
    get navigations() {
      return navigations;
    },
  };
}

for (const interactive of [false, true]) {
  test(`${interactive ? "desktop" : "headless"} pending creation is cancelled before close succeeds`, async () => {
    const opening = deferred();
    const disposed = deferred();
    const h = harness({
      interactive,
      opening,
      disposal: () => disposed.promise,
    });
    try {
      const creation = h.create();
      const rejected = assert.rejects(creation, errorCode("session_closing"));
      await assert.rejects(
        h.runner.status("fixed"),
        errorCode("session_starting"),
      );
      const closing = h.runner.close("fixed");
      const parallel = h.runner.close("fixed");
      let finished = false;
      closing.then(() => {
        finished = true;
      });
      await assert.rejects(
        h.runner.status("fixed"),
        errorCode("session_closing"),
      );
      await assert.rejects(h.create(), errorCode("session_exists"));
      assert.equal(finished, false);
      opening.resolve();
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(h.closes, 1);
      assert.equal(h.navigations, 0);
      assert.equal(finished, false);
      disposed.resolve();
      await Promise.all([closing, parallel, rejected]);
      assert.equal(h.closes, 1);
      await assert.rejects(
        h.runner.status("fixed"),
        errorCode("session_not_found"),
      );
      await assert.rejects(
        h.runner.close("fixed"),
        errorCode("session_not_found"),
      );
    } finally {
      opening.resolve();
      disposed.resolve();
      await h.runner.shutdown();
    }
  });

  test(`${interactive ? "desktop" : "headless"} failed disposal remains reserved and retryable`, async () => {
    const attempt = deferred();
    const h = harness({
      interactive,
      disposal: (count) => (count === 1 ? attempt.promise : undefined),
    });
    try {
      await h.create();
      const closing = h.runner.close("fixed");
      const parallel = h.runner.close("fixed");
      const failures = Promise.all([
        assert.rejects(closing, /synthetic_disposal_failure/),
        assert.rejects(parallel, /synthetic_disposal_failure/),
      ]);
      await assert.rejects(
        h.runner.complete("fixed"),
        errorCode("session_closing"),
      );
      attempt.reject(new Error("synthetic_disposal_failure"));
      await failures;
      assert.equal(h.closes, 1);
      await assert.rejects(
        h.runner.status("fixed"),
        errorCode("session_closing"),
      );
      await assert.rejects(h.create(), errorCode("session_exists"));
      await h.runner.close("fixed");
      assert.equal(h.closes, 2);
      await assert.rejects(
        h.runner.status("fixed"),
        errorCode("session_not_found"),
      );
    } finally {
      await h.runner.shutdown();
    }
  });
}

test("HTTP DELETE during navigation waits for disposal and sanitizes failures", async () => {
  const navigation = deferred();
  const disposal = deferred();
  const h = harness({
    navigation,
    disposal: (count) => (count === 1 ? disposal.promise : undefined),
  });
  const server = createRunnerServer({
    runner: h.runner,
    token: "fixture-token",
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}/v1/sessions/fixed`;
  const request = (method, suffix = "") =>
    fetch(`${url}${suffix}`, {
      method,
      headers: { authorization: "Bearer fixture-token" },
    });
  try {
    const creation = h.create();
    const rejected = assert.rejects(creation, /synthetic_disposal_failure/);
    assert.equal((await request("GET", "/status")).status, 409);
    const closing = request("DELETE");
    // Server event is a deterministic barrier that DELETE entered the real route.
    await new Promise((resolve) => server.once("request", resolve));
    const status = await request("GET", "/status");
    assert.deepEqual(await status.json(), { error: "session_closing" });
    navigation.resolve();
    await new Promise((resolve) => setImmediate(resolve));
    disposal.reject(new Error("synthetic_disposal_failure"));
    const failed = await closing;
    assert.equal(failed.status, 503);
    assert.deepEqual(await failed.json(), { error: "runner_unavailable" });
    await rejected;
    const retried = await request("DELETE");
    assert.equal(retried.status, 200);
    assert.deepEqual(await retried.json(), { closed: true });
    assert.equal(h.closes, 2);
    assert.equal((await request("DELETE")).status, 404);
  } finally {
    navigation.resolve();
    disposal.resolve();
    await h.runner.shutdown();
    await new Promise((resolve) => server.close(resolve));
  }
});

test("failed startup cleanup stays recoverable instead of becoming absent", async () => {
  const navigation = deferred();
  const h = harness({
    navigation,
    disposal: (count) => {
      if (count === 1) throw new Error("synthetic_disposal_failure");
    },
  });
  try {
    const creation = h.create();
    const failed = assert.rejects(creation, /synthetic_disposal_failure/);
    navigation.reject(new Error("synthetic_navigation_failure"));
    await failed;
    await assert.rejects(
      h.runner.status("fixed"),
      errorCode("session_closing"),
    );
    await h.runner.close("fixed");
    assert.equal(h.closes, 2);
    await assert.rejects(
      h.runner.status("fixed"),
      errorCode("session_not_found"),
    );
  } finally {
    await h.runner.shutdown();
  }
});

test("shutdown waits for pending creation and disposes its late resource", async () => {
  const opening = deferred();
  const h = harness({ opening });
  const creation = h.create();
  const rejected = assert.rejects(creation, errorCode("session_closing"));
  const shutdown = h.runner.shutdown();
  await assert.rejects(h.runner.status("fixed"), errorCode("session_closing"));
  opening.resolve();
  await Promise.all([shutdown, rejected]);
  assert.equal(h.closes, 1);
  await assert.rejects(h.create(), errorCode("runner_unavailable"));
});

test("desktop startup failure preserves its early cleanup handle for DELETE retry", async () => {
  const opening = deferred();
  const h = harness({
    interactive: true,
    opening,
    disposal: (count) => {
      if (count === 1) throw new Error("synthetic_disposal_failure");
    },
  });
  try {
    const failed = assert.rejects(h.create(), /synthetic_disposal_failure/);
    opening.reject(new Error("synthetic_open_failure"));
    await failed;
    await assert.rejects(
      h.runner.status("fixed"),
      errorCode("session_closing"),
    );
    await h.runner.close("fixed");
    assert.equal(h.closes, 2);
    await assert.rejects(
      h.runner.close("fixed"),
      errorCode("session_not_found"),
    );
  } finally {
    await h.runner.shutdown();
  }
});

for (const action of ["close", "shutdown"]) {
  test(
    `${action} interrupts startup identity that only settles when context closes`,
    { timeout: 2_000 },
    async () => {
      const entered = deferred();
      const identity = deferred();
      const h = harness({
        identify: () => {
          entered.resolve();
          return identity.promise;
        },
        disposal: () => identity.resolve(null),
      });
      const creation = h.create();
      const rejected = assert.rejects(creation, errorCode("session_closing"));
      await entered.promise;
      await h.runner[action]("fixed");
      await rejected;
      assert.equal(h.closes, 1);
      await assert.rejects(
        h.runner.status("fixed"),
        errorCode("session_not_found"),
      );
      await h.runner.shutdown();
    },
  );
}
