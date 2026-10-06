import assert from "node:assert/strict";
import { createServer } from "node:http";
import { constants } from "node:fs";
import { access, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { createRunner } from "../src/runner.mjs";

if (process.platform !== "linux") throw new Error("linux_smoke_required");
assert.notEqual(process.getuid(), 0, "desktop must not run as root");
assert.ok(isAbsolute(process.env.HOME), "runner HOME must be absolute");
await access(process.env.HOME, constants.W_OK | constants.X_OK);
await access(tmpdir(), constants.W_OK | constants.X_OK);
const writable = await mkdtemp(join(tmpdir(), "geo-desktop-preflight-"));
try {
  await writeFile(join(writable, "writable"), "synthetic", { mode: 0o600 });
} finally {
  await rm(writable, { recursive: true, force: true });
}
const fixture = createServer((_request, response) => {
  response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
  response.end(
    "<!doctype html><title>Isolated login fixture</title>" +
      "<main>Synthetic account</main><script>document.cookie='fixture=ready; SameSite=Lax'</script>",
  );
});
await new Promise((resolve) => fixture.listen(0, "127.0.0.1", resolve));
const runner = createRunner({
  interactiveRuntime: "linux-vnc",
  platformAdapters: {
    fixture: {
      entry: `http://127.0.0.1:${fixture.address().port}/login`,
      connectorVersion: "fixture.v1",
      operations: [],
      async identify(page) {
        const visible = await page.locator("main").textContent();
        const cookie = await page.evaluate(() => document.cookie);
        const image = await page.screenshot({ type: "png" });
        assert.ok(
          image.subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex")),
        );
        return visible === "Synthetic account" &&
          cookie.includes("fixture=ready")
          ? { platform_account_id: "fixture-only", display_name: "Synthetic" }
          : null;
      },
    },
  },
});
try {
  const created = await runner.create({
    session_id: "synthetic",
    platform: "fixture",
  });
  assert.equal(created.phase, "ready_to_complete");
  const endpoint = runner.desktopEndpoint("synthetic");
  assert.equal(endpoint.host, "127.0.0.1");
  // Confirm the actual websockify -> Xvnc RFB path without implementing RFB.
  const ws = new WebSocket(`ws://${endpoint.host}:${endpoint.port}/`);
  const greeting = await Promise.race([
    new Promise((resolve, reject) => {
      ws.binaryType = "arraybuffer";
      ws.addEventListener("message", (event) =>
        resolve(Buffer.from(event.data)),
      );
      ws.addEventListener("error", () =>
        reject(new Error("websocket_unavailable")),
      );
    }),
    new Promise((_resolve, reject) =>
      setTimeout(() => reject(new Error("rfb_timeout")), 5_000),
    ),
  ]);
  assert.match(greeting.toString("ascii"), /^RFB 00/);
  ws.close();
  const receipt = await runner.complete("synthetic");
  assert.equal(receipt.identity.platform_account_id, "fixture-only");
  assert.ok(
    receipt.storage_state.cookies.some((cookie) => cookie.name === "fixture"),
  );
  assert.throws(
    () => runner.desktopEndpoint("synthetic"),
    (error) => error.code === "desktop_unavailable",
  );
  await runner.close("synthetic");
  process.stdout.write("interactive Linux synthetic desktop smoke passed\n");
} finally {
  await runner.shutdown();
  await new Promise((resolve) => fixture.close(resolve));
}
