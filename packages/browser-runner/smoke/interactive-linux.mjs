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
        "<style>#render-marker{position:fixed;left:80px;top:180px;" +
        "width:260px;height:120px;background:#27b5e9}</style>" +
        "<main>Synthetic account</main><input id='synthetic-input' autofocus>" +
        "<div id='render-marker' aria-hidden='true'></div>" +
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
async function waitForSyntheticMarker(client) {
  try {
    const handle = await client.waitForFunction(
      () => {
        const canvas = document.querySelector("#screen canvas");
        if (
          !window.syntheticDesktop?.connected ||
          !canvas?.width ||
          !canvas?.height
        )
          return false;
        const { data } = canvas
          .getContext("2d")
          .getImageData(0, 0, canvas.width, canvas.height);
        let count = 0;
        let minX = canvas.width;
        let minY = canvas.height;
        let maxX = 0;
        let maxY = 0;
        for (let y = 0; y < canvas.height; y += 4) {
          for (let x = 0; x < canvas.width; x += 4) {
            const index = (y * canvas.width + x) * 4;
            if (
              data[index] === 39 &&
              data[index + 1] === 181 &&
              data[index + 2] === 233
            ) {
              count++;
              minX = Math.min(minX, x);
              minY = Math.min(minY, y);
              maxX = Math.max(maxX, x);
              maxY = Math.max(maxY, y);
            }
          }
        }
        // The remote fixture's solid #27b5e9 block has thousands of sampled
        // pixels. A connected RFB socket or an otherwise valid PNG is not
        // enough to show that the remote browser is visible on this canvas.
        return count >= 1_000
          ? {
              minX,
              minY,
              maxX,
              maxY,
              width: canvas.width,
              height: canvas.height,
            }
          : false;
      },
      undefined,
      { timeout: 10_000 },
    );
    return await handle.jsonValue();
  } catch {
    throw new Error("synthetic_novnc_framebuffer_marker_missing");
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
          identityProbeResult = {
            text: visible === "Synthetic account",
            cookie: cookie.includes("fixture=ready"),
          };
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
  assert.equal(created.phase, "login_required");
  assert.equal(identityProbeResult, undefined);
  assert.equal(runner.desktopEndpoint("synthetic").host, "127.0.0.1");
  const status = await runner.status("synthetic");
  assert.ifError(identityProbeError);
  assert.deepEqual(identityProbeResult, {
    text: true,
    cookie: true,
  });
  assert.equal(status.phase, "ready_to_complete");
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
  await remotePage.bringToFront();
  const marker = await waitForSyntheticMarker(client);
  const inputBox = await remotePage.locator("#synthetic-input").boundingBox();
  assert.ok(inputBox, "synthetic_input_not_visible");
  const canvasBox = await client.locator("#screen canvas").boundingBox();
  assert.ok(canvasBox, "synthetic_novnc_canvas_not_visible");
  // Locate the synthetic input from the marker's known fixture coordinates.
  // This read-only geometry check does not use Playwright to focus/type in
  // the remote browser: the click below travels through stock noVNC pointer.
  const markerScaleX = (marker.maxX - marker.minX + 4) / 260;
  const markerScaleY = (marker.maxY - marker.minY + 4) / 120;
  const remoteX =
    marker.minX + (inputBox.x + inputBox.width / 2 - 80) * markerScaleX;
  const remoteY =
    marker.minY + (inputBox.y + inputBox.height / 2 - 180) * markerScaleY;
  const clickX = canvasBox.x + (remoteX / marker.width) * canvasBox.width;
  const clickY = canvasBox.y + (remoteY / marker.height) * canvasBox.height;
  assert.ok(
    clickX >= canvasBox.x &&
      clickX < canvasBox.x + canvasBox.width &&
      clickY >= canvasBox.y &&
      clickY < canvasBox.y + canvasBox.height,
    "synthetic_input_outside_remote_framebuffer",
  );
  await client.mouse.click(clickX, clickY);
  try {
    await remotePage.waitForFunction(
      () =>
        document.hasFocus() && document.activeElement?.id === "synthetic-input",
      undefined,
      { timeout: 10_000 },
    );
  } catch {
    const focus = await remotePage.evaluate(() => ({
      document: document.hasFocus(),
      active: document.activeElement?.id ?? null,
    }));
    throw new Error(
      `synthetic_remote_pointer_focus_failed: ${JSON.stringify(focus)}`,
    );
  }
  await client.keyboard.type("AbC123", { delay: 40 });
  await expectSyntheticInput("AbC123", "ascii");
  await client.keyboard.press("Backspace");
  await expectSyntheticInput("AbC12", "backspace");
  // The clipboard is transmitted through stock RFB; this exact assertion
  // fails if the legacy Latin-1 clipboard path substitutes '?' for Unicode.
  await client.evaluate(() => window.syntheticDesktop.paste("中文😀"));
  await client.keyboard.press("Control+V");
  await expectSyntheticInput("AbC12中文😀", "unicode_clipboard");
  // The identity probe only establishes the fixture's DOM and cookie.
  // Separately prove the actual noVNC-rendered desktop is screenshotable;
  // the headed remote Chromium's CDP capture is not part of identity.
  await client.bringToFront();
  await waitForSyntheticMarker(client);
  let desktopImage;
  try {
    desktopImage = await client.locator("#screen canvas").screenshot({
      type: "png",
      timeout: 10_000,
    });
  } catch (error) {
    throw new Error("synthetic_novnc_render_screenshot_failed", {
      cause: error,
    });
  }
  assert.ok(
    desktopImage.subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex")),
    "synthetic_novnc_render_invalid_png",
  );
  const receipt = await runner.complete("synthetic");
  assert.ifError(identityProbeError);
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
