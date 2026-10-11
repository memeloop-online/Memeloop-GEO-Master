import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { writeFile, access } from "node:fs/promises";
import { test } from "node:test";
import { createLinuxDesktopRuntime } from "../src/interactive-desktop.mjs";
import { createRunner } from "../src/runner.mjs";

test("desktop disposal propagates browser/process failures and retries actual resources", async () => {
  // Exercise the real lifecycle with synthetic subprocess/browser resources.
  // No X server, network, browser launch or account is used.
  const platform = Object.getOwnPropertyDescriptor(process, "platform");
  Object.defineProperty(process, "platform", { value: "linux" });
  const children = [];
  let port = 12000;
  let browserCloses = 0;
  let failBrowser = true;
  let failProcess = true;
  let resource;
  let directory;
  const runtime = createLinuxDesktopRuntime({
    reservePort: async () => ++port,
    runCommand: async (_command, args) => writeFile(args[1], ""),
    waitReady: async () => {},
    spawnProcess(command, _args, env) {
      directory = env.XDG_RUNTIME_DIR;
      const child = new EventEmitter();
      Object.assign(child, {
        command,
        pid: 1,
        exitCode: null,
        signalCode: null,
        stops: 0,
        kill() {},
      });
      children.push(child);
      return child;
    },
    async stopProcess(child) {
      if (!child) return;
      child.stops++;
      if (failProcess && child.command === "Xtigervnc")
        throw new Error("synthetic_process_failure");
    },
  });
  try {
    const desktop = await runtime.open({
      browserType: {
        async launch() {
          return {
            async newContext() {
              return {
                async newPage() {
                  return {};
                },
              };
            },
            async close() {
              browserCloses++;
              if (failBrowser) throw new Error("synthetic_browser_failure");
            },
          };
        },
      },
      viewport: { width: 1280, height: 800 },
      onSession: (value) => {
        resource = value;
      },
    });
    assert.equal(resource, desktop);
    const first = desktop.close();
    assert.equal(desktop.close(), first);
    await assert.rejects(first, /desktop_close_failed/);
    assert.equal(browserCloses, 1);
    assert.equal(
      children.every((child) => child.stops === 1),
      true,
    );
    await access(directory);
    assert.throws(() => desktop.endpoint(), /desktop_input_closed/);
    failBrowser = false;
    await assert.rejects(desktop.close(), /desktop_close_failed/);
    assert.equal(browserCloses, 2);
    failProcess = false;
    await desktop.close();
    assert.equal(browserCloses, 3);
    assert.equal(
      children.every((child) => child.stops === 3),
      true,
    );
    await assert.rejects(access(directory), { code: "ENOENT" });
    await desktop.close();
    assert.equal(browserCloses, 3);
  } finally {
    failBrowser = false;
    failProcess = false;
    await resource?.close();
    Object.defineProperty(process, "platform", platform);
  }
});

for (const action of ["close", "shutdown"]) {
  test(
    `desktop ${action} interrupts context creation after browser acquisition`,
    { timeout: 2_000 },
    async () => {
      const platform = Object.getOwnPropertyDescriptor(process, "platform");
      Object.defineProperty(process, "platform", { value: "linux" });
      const entered = Promise.withResolvers();
      const context = Promise.withResolvers();
      let port = 13000;
      let closes = 0;
      const runtime = createLinuxDesktopRuntime({
        reservePort: async () => ++port,
        runCommand: async (_command, args) => writeFile(args[1], ""),
        waitReady: async () => {},
        spawnProcess() {
          return Object.assign(new EventEmitter(), {
            pid: 1,
            exitCode: null,
            signalCode: null,
            kill() {},
          });
        },
        stopProcess: async () => {},
      });
      const runner = createRunner({
        interactiveRuntime: "linux-vnc",
        desktopRuntime: runtime,
        browserType: {
          async launch() {
            return {
              async newContext() {
                entered.resolve();
                return context.promise;
              },
              async close() {
                closes++;
                context.reject(new Error("synthetic_browser_closed"));
              },
            };
          },
        },
        platformAdapters: { fixture: { entry: "https://fixture.invalid/" } },
      });
      try {
        const rejected = assert.rejects(
          runner.create({ session_id: "fixed", platform: "fixture" }),
          /synthetic_browser_closed/,
        );
        await entered.promise;
        await runner[action]("fixed");
        await rejected;
        assert.equal(closes, 1);
        await assert.rejects(
          runner.status("fixed"),
          (error) => error.code === "session_not_found",
        );
      } finally {
        await runner.shutdown();
        Object.defineProperty(process, "platform", platform);
      }
    },
  );
}
