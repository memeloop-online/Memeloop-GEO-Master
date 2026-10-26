import assert from "node:assert/strict";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { test } from "node:test";
import fontkit from "@pdf-lib/fontkit";
import { createRunnerServer } from "../src/server.mjs";

const token = "font-subset-test-token";

async function service(t, options = {}) {
  // Any account/browser operation is a test failure. Font work needs neither.
  const runner = new Proxy(
    {},
    {
      get() {
        throw new Error("Font delivery must not use browser sessions");
      },
    },
  );
  const server = createRunnerServer({ token, runner, ...options });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const url = `http://127.0.0.1:${server.address().port}/v1/font-subset`;
  return {
    server,
    request: (body, headers = {}) =>
      fetch(url, {
        method: "POST",
        headers: {
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
          ...headers,
        },
        body: typeof body === "string" ? body : JSON.stringify(body),
      }),
    url,
  };
}

test("serves the fixed packaged font subset under existing bearer authentication", async (t) => {
  const { request } = await service(t);
  const unauthorized = await request(
    { codepoints: [65] },
    { authorization: "Bearer incorrect" },
  );
  assert.equal(unauthorized.status, 401);
  const response = await request({
    codepoints: [..."中文繁體龘𠮷A"].map((character) =>
      character.codePointAt(0),
    ),
  });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "font/otf");
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.equal(response.headers.get("x-content-type-options"), "nosniff");
  const bytes = new Uint8Array(await response.arrayBuffer());
  assert.ok(bytes.length < 1024 * 1024);
  assert.equal(Number(response.headers.get("content-length")), bytes.length);
  const font = fontkit.create(bytes);
  const source = await readFile(
    new URL("../assets/fonts/NotoSansCJKsc-Regular.otf", import.meta.url),
  );
  assert.equal(
    createHash("sha256").update(source).digest("hex"),
    "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b",
  );
  const original = fontkit.create(source);
  assert.equal(
    font.glyphForCodePoint(0x20bb7).id,
    original.glyphForCodePoint(0x20bb7).id,
  );
});

test("returns generic 400 for malformed JSON and Unicode input", async (t) => {
  const { request } = await service(t);
  for (const body of [
    "{",
    null,
    {},
    { codepoints: "中文" },
    { codepoints: [0xd800] },
  ]) {
    const response = await request(body);
    assert.equal(response.status, 400);
    assert.match(
      (await response.json()).error,
      /^(invalid_json|invalid_codepoints)$/,
    );
  }
});

test("returns 413 at the font route's 8 MiB body boundary", async (t) => {
  const { request } = await service(t);
  const response = await request(
    JSON.stringify({ codepoints: [65], padding: "x".repeat(8 * 1024 * 1024) }),
  );
  assert.equal(response.status, 413);
  assert.deepEqual(await response.json(), { error: "payload_too_large" });
});

test("returns generic 503 without serializing service failures", async (t) => {
  const { request } = await service(t, {
    subsetPdfFont: async () => {
      throw new Error("private diagnostic");
    },
  });
  const response = await request({ codepoints: [65] });
  assert.equal(response.status, 503);
  assert.deepEqual(await response.json(), { error: "font_subset_unavailable" });
});

test("aborts pending subset work when the requesting connection closes", async (t) => {
  let notifyStarted;
  let notifyAborted;
  const started = new Promise((resolve) => {
    notifyStarted = resolve;
  });
  const aborted = new Promise((resolve) => {
    notifyAborted = resolve;
  });
  const { url } = await service(t, {
    subsetPdfFont: async ({ signal }) => {
      notifyStarted();
      await once(signal, "abort");
      notifyAborted();
      signal.throwIfAborted();
    },
  });
  const controller = new AbortController();
  const pending = fetch(url, {
    method: "POST",
    headers: { authorization: `Bearer ${token}` },
    body: JSON.stringify({ codepoints: [65] }),
    signal: controller.signal,
  });
  await started;
  controller.abort();
  await assert.rejects(pending, { name: "AbortError" });
  await Promise.race([
    aborted,
    new Promise((_, reject) => {
      const timeout = setTimeout(
        () => reject(new Error("Disconnect did not cancel subset work")),
        2000,
      );
      timeout.unref();
    }),
  ]);
});
