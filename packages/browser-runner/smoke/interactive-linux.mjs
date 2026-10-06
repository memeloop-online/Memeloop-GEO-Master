import assert from "node:assert/strict";
import { createServer } from "node:http";
import { constants } from "node:fs";
import { access, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
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
// Only the upstream noVNC ES modules are exposed, on this private fixture
// loopback. The smoke never adds an RFB implementation or a text-input bridge.
const noVncPackage = dirname(
  dirname(fileURLToPath(import.meta.resolve("@novnc/novnc"))),
);
let desktopUrl;
const fixture = createServer(async (request, response) => {
  const pathname = new URL(request.url, "http://127.0.0.1").pathname;
  if (pathname === "/login") {
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    response.end(
      "<!doctype html><title>Isolated login fixture</title>" +
        "<main>Synthetic account</main><input id='synthetic-input' autofocus>" +
        "<script>document.cookie='fixture=ready; SameSite=Lax'</script>",
    );
    return;
  }
  if (pathname === "/client" && desktopUrl) {
    response.writeHead(200, {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-store",
    });
    response.end(
      "<!doctype html><meta charset='utf-8'><title>Synthetic noVNC client</title>" +
        "<style>html,body,#screen{margin:0;width:100%;height:100%;overflow:hidden}</style>" +
        "<div id='screen'></div><script type='module'>" +
        "import RFB from '/novnc/core/rfb.js';" +
        `const rfb = new RFB(document.querySelector('#screen'), ${JSON.stringify(desktopUrl)});` +
        "window.syntheticDesktop = { connected: false, disconnected: false, clean: false," +
        " paste(text) { rfb.clipboardPasteFrom(text); } };" +
        "rfb.addEventListener('connect', () => { window.syntheticDesktop.connected = true; });" +
        "rfb.addEventListener('disconnect', event => {" +
        " window.syntheticDesktop.disconnected = true;" +
        " window.syntheticDesktop.clean = event.detail.clean;" +
        "});" +
        "</script>",
    );
    return;
  }
  if (
    /^\/novnc\/(?:core|vendor)\/(?:[a-z0-9_-]+\/)*[a-z0-9_-]+\.js$/i.test(
      pathname,
    )
  ) {
    try {
      const source = await readFile(join(noVncPackage, pathname.slice(7)));
      response.writeHead(200, {
        "content-type": "text/javascript; charset=utf-8",
      });
      response.end(source);
    } catch {
      response.writeHead(404).end();
    }
    return;
  }
  response.writeHead(404).end();
});
await new Promise((resolve) => fixture.listen(0, "127.0.0.1", resolve));
let identityProbeError;
let identityProbeResult;
let remotePage;
let clientBrowser;
async function expectSyntheticInput(expected, stage) {
  try {
    await remotePage.waitForFunction(
      (value) => document.querySelector("#synthetic-input")?.value === value,
      expected,
      { timeout: 10_000 },
    );
  } catch {
    // The fixture never loads accounts or third-party sites. Keep the
    // diagnostic limited to this synthetic input, not screenshots or URLs.
    const actual = await remotePage.locator("#synthetic-input").inputValue();
    throw new Error(
      `synthetic_${stage}_mismatch: ${JSON.stringify(actual)} != ${JSON.stringify(expected)}`,
    );
  }
}
const runner = createRunner({
  interactiveRuntime: "linux-vnc",
  platformAdapters: {
    fixture: {
      entry: `http://127.0.0.1:${fixture.address().port}/login`,
      connectorVersion: "fixture.v1",
      operations: [],
      async identify(page) {
        try {
          remotePage = page;
          const visible = await page.locator("main").textContent();
          const cookie = await page.evaluate(() => document.cookie);
          const image = await page.screenshot({ type: "png" });
          identityProbeResult = {
            text: visible === "Synthetic account",
            cookie: cookie.includes("fixture=ready"),
            screenshot: image
              .subarray(0, 8)
              .equals(Buffer.from("89504e470d0a1a0a", "hex")),
          };
          assert.ok(identityProbeResult.screenshot);
          return identityProbeResult.text && identityProbeResult.cookie
            ? { platform_account_id: "fixture-only", display_name: "Synthetic" }
            : null;
        } catch (error) {
          // This isolated fixture never opens a real platform or account.
          // Surface its suppressed probe failure rather than diagnosing only
          // the resulting login_required status.
          identityProbeError = error;
          throw error;
        }
      },
    },
  },
});
try {
  const created = await runner.create({
    session_id: "synthetic",
    platform: "fixture",
  });
  assert.ifError(identityProbeError);
  assert.deepEqual(identityProbeResult, {
    text: true,
    cookie: true,
    screenshot: true,
  });
  assert.equal(created.phase, "ready_to_complete");
  const endpoint = runner.desktopEndpoint("synthetic");
  assert.equal(endpoint.host, "127.0.0.1");
  assert.ok(remotePage, "synthetic remote page must exist");
  desktopUrl = `ws://${endpoint.host}:${endpoint.port}/`;
  // A second Chromium hosts the unmodified noVNC input stack, and does not
  // share the login browser's context, display, or Playwright keyboard.
  clientBrowser = await chromium.launch({ headless: true });
  const client = await clientBrowser.newPage({
    viewport: { width: 1280, height: 720 },
  });
  const clientErrors = [];
  client.on("pageerror", (error) => clientErrors.push(error.message));
  await client.goto(`http://127.0.0.1:${fixture.address().port}/client`);
  await client.waitForFunction(
    () => window.syntheticDesktop?.connected === true,
    undefined,
    { timeout: 15_000 },
  );
  assert.deepEqual(
    clientErrors,
    [],
    "upstream noVNC module must load without errors",
  );
  await remotePage.waitForFunction(
    () => document.activeElement?.id === "synthetic-input",
  );
  await client.evaluate(() => document.querySelector("#screen canvas").focus());
  await client.keyboard.type("AbC123", { delay: 40 });
  await expectSyntheticInput("AbC123", "ascii");
  await client.keyboard.press("Backspace");
  await expectSyntheticInput("AbC12", "backspace");
  // The clipboard is transmitted through stock RFB; this exact assertion
  // fails if the legacy Latin-1 clipboard path substitutes '?' for Unicode.
  await client.evaluate(() => window.syntheticDesktop.paste("中文😀"));
  await client.waitForTimeout(300);
  await client.keyboard.press("Control+V");
  await expectSyntheticInput("AbC12中文😀", "unicode_clipboard");
  const receipt = await runner.complete("synthetic");
  await client.waitForFunction(
    () => window.syntheticDesktop.disconnected,
    undefined,
    { timeout: 10_000 },
  );
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
  await clientBrowser?.close();
  await runner.shutdown();
  await new Promise((resolve) => fixture.close(resolve));
}
