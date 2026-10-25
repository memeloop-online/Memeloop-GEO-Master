import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import http from "node:http";
import https from "node:https";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
const modules = new Map();
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
  if (asset.endsWith(".js")) modules.set(asset, identity.body.toString());
  assert.ok(compressed.body.length < identity.body.length);
  console.log(
    `Static asset: ${identity.body.length} bytes identity, ${compressed.body.length} bytes gzip.`,
  );
}

// Follow the built lazy module graph: the PDF font must be content addressed,
// even though visiting the application does not request it.
const pending = [...modules.keys()];
const fonts = new Set();
for (const path of pending) {
  const source = modules.get(path);
  for (const match of source.matchAll(
    /["'](\/assets\/NotoSansCJKsc-Regular-[\w-]+\.otf)["']/g,
  ))
    fonts.add(match[1]);
  for (const match of source.matchAll(
    /["']((?:\.\/|\/?assets\/)[^"'\s]+\.js)["']/g,
  )) {
    const dependency = match[1].startsWith("assets/")
      ? `/${match[1]}`
      : new URL(match[1], new URL(path, origin)).pathname;
    if (modules.has(dependency)) continue;
    const response = await get(dependency);
    assert.equal(response.status, 200);
    modules.set(dependency, response.body.toString());
    pending.push(dependency);
  }
}
assert.equal(
  fonts.size,
  1,
  "Both PDF exporters must use one hashed font asset",
);
const fontPath = [...fonts][0];
const fontSource = await readFile(
  new URL(
    "../apps/web/src/assets/fonts/NotoSansCJKsc-Regular.otf",
    import.meta.url,
  ),
);
const fontIdentity = await get(fontPath, { "Accept-Encoding": "identity" });
const fontCompressed = await get(fontPath, {
  "Accept-Encoding": "gzip",
  Via: "1.1 image-smoke",
});
for (const response of [fontIdentity, fontCompressed]) {
  assert.equal(response.status, 200);
  assert.equal(response.headers["content-type"], "font/otf");
  assert.equal(
    response.headers["cache-control"],
    "public, max-age=31536000, immutable",
  );
}
assert.equal(fontIdentity.headers["content-encoding"], undefined);
assert.equal(fontCompressed.headers["content-encoding"], "gzip");
assert.match(fontCompressed.headers.vary, /(?:^|,\s*)Accept-Encoding(?:,|$)/i);
assert.deepEqual(fontIdentity.body, fontSource);
assert.deepEqual(gunzipSync(fontCompressed.body), fontSource);
assert.ok(fontCompressed.body.length < fontSource.length);
console.log(
  `PDF font: ${fontSource.length} bytes identity, ${fontCompressed.body.length} bytes gzip; SHA-256 ${createHash("sha256").update(fontSource).digest("hex")}.`,
);
const missingFont = await get("/assets/NotoSansCJKsc-Regular-missing.otf");
assert.equal(missingFont.status, 404);
assert.doesNotMatch(missingFont.headers["cache-control"] ?? "", /immutable/);
const missing = await get("/assets/missing-image-smoke.js");
assert.equal(
  missing.status,
  404,
  "Missing assets must not return the HTML shell",
);
assert.doesNotMatch(missing.headers["cache-control"] ?? "", /immutable/);

// A normal persistent profile exercises the disk HTTP cache, including fonts
// larger than Chromium's incognito in-memory cache entry limit.
const profile = await mkdtemp(join(tmpdir(), "geo-web-image-"));
const browser = await chromium.launchPersistentContext(profile, {
  headless: true,
  ...(process.env.GEO_TEST_CHROMIUM_PATH
    ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
    : {}),
});
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
  assert.equal(
    await page.evaluate(() =>
      performance
        .getEntriesByType("resource")
        .some((entry) => new URL(entry.name).pathname.endsWith(".otf")),
    ),
    false,
    "Opening the application must not download the PDF font",
  );
  async function fetchFont() {
    return page.evaluate(async (path) => {
      performance.clearResourceTimings();
      const started = performance.now();
      const response = await fetch(path, { credentials: "same-origin" });
      const bytes = await response.arrayBuffer();
      const timing = performance
        .getEntriesByName(new URL(path, location.href).href)
        .at(-1);
      return {
        status: response.status,
        bytes: bytes.byteLength,
        milliseconds: Math.round(performance.now() - started),
        transferredBytes: timing?.transferSize,
        encodedBytes: timing?.encodedBodySize,
      };
    }, fontPath);
  }
  const cold = await fetchFont();
  // A page reload drops application memory; the warm fetch must use HTTP cache.
  await page.reload();
  const warm = await fetchFont();
  assert.equal(cold.status, 200);
  assert.equal(warm.status, 200);
  assert.equal(cold.bytes, fontSource.length);
  assert.equal(warm.bytes, fontSource.length);
  assert.ok(cold.transferredBytes > 0, "Cold font fetch must transfer bytes");
  assert.ok(
    cold.encodedBytes < fontSource.length,
    "Browser must receive compressed font",
  );
  assert.equal(
    warm.transferredBytes,
    0,
    "Warm font fetch must avoid network transfer",
  );
  console.log(`PDF font browser delivery: ${JSON.stringify({ cold, warm })}`);
  console.log("Production web image renders without uncaught module errors.");
} finally {
  await browser.close();
  await rm(profile, { recursive: true, force: true });
}
