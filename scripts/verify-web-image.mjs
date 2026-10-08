import assert from "node:assert/strict";
import http from "node:http";
import https from "node:https";
import { createRequire } from "node:module";
import { gunzipSync } from "node:zlib";

const require = createRequire(
  new URL("../packages/browser-runner/package.json", import.meta.url),
);
const { chromium } = require("playwright");
const origin = process.env.GEO_WEB_IMAGE_URL ?? "http://127.0.0.1:18081";

// Keep the wire bytes: fetch transparently decompresses gzip responses.
function get(path, headers = {}) {
  const url = new URL(path, origin);
  const transport = url.protocol === "https:" ? https : http;
  return new Promise((resolve, reject) => {
    const request = transport.get(url, { headers }, (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("error", reject);
      response.on("end", () =>
        resolve({
          status: response.statusCode,
          headers: response.headers,
          body: Buffer.concat(chunks),
        }),
      );
    });
    request.setTimeout(15_000, () =>
      request.destroy(new Error("Static delivery check timed out")),
    );
    request.on("error", reject);
  });
}

const shell = await get("/login");
assert.equal(shell.status, 200);
assert.equal(shell.headers["cache-control"], "no-cache");
const index = await get("/index.html");
assert.equal(index.status, 200);
assert.equal(index.headers["cache-control"], "no-cache");
const assets = [
  ...shell.body
    .toString()
    .matchAll(/(?:src|href)="(\/assets\/[^"]+\.(?:js|css))"/g),
].map((match) => match[1]);
assert.ok(assets.some((asset) => asset.endsWith(".js")));
assert.ok(assets.some((asset) => asset.endsWith(".css")));
for (const asset of assets) {
  const identity = await get(asset, { "Accept-Encoding": "identity" });
  const compressed = await get(asset, {
    "Accept-Encoding": "gzip",
    Via: "1.1 image-smoke",
  });
  assert.equal(identity.status, 200);
  assert.equal(compressed.status, 200);
  assert.equal(identity.headers["content-encoding"], undefined);
  assert.equal(compressed.headers["content-encoding"], "gzip");
  assert.match(compressed.headers.vary, /(?:^|,\s*)Accept-Encoding(?:,|$)/i);
  for (const response of [identity, compressed]) {
    assert.equal(
      response.headers["cache-control"],
      "public, max-age=31536000, immutable",
    );
  }
  assert.deepEqual(gunzipSync(compressed.body), identity.body);
  assert.ok(compressed.body.length < identity.body.length);
  console.log(
    `Static asset: ${identity.body.length} bytes identity, ${compressed.body.length} bytes gzip.`,
  );
}
const missing = await get("/assets/missing-image-smoke.js");
assert.equal(
  missing.status,
  404,
  "Missing assets must not return the HTML shell",
);
assert.doesNotMatch(missing.headers["cache-control"] ?? "", /immutable/);

const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const response = await page.goto(`${origin}/login`);
  assert.equal(response.status(), 200);
  // Exercise the production module graph, not just Nginx or the HTML shell.
  // With no API configured the application must render its unavailable state.
  await page.waitForFunction(
    () => Boolean(document.querySelector("#root")?.textContent?.trim()),
    undefined,
    { timeout: 15_000 },
  );
  assert.deepEqual(errors, [], "Production frontend has uncaught errors");
  assert.ok((await page.locator("#root").innerText()).trim());
  console.log("Production web image renders without uncaught module errors.");
} finally {
  await browser.close();
}
