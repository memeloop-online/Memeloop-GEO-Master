import { EventEmitter } from "node:events";
import { createServer } from "node:http";
import assert from "node:assert/strict";
import test from "node:test";
import { chromium } from "playwright";
import { captureConnectExchange } from "../src/connect-browser-capture.mjs";

const endpoint = "https://example.invalid/stream";
const frame = (flags, value) => {
  const bytes = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(5);
  header[0] = flags;
  header.writeUInt32BE(bytes.length, 1);
  return Buffer.concat([header, bytes]);
};
const body = Buffer.concat([
  frame(0, { message: { id: "message-1", text: "中文" } }),
  frame(2, {}),
]);
const response = (overrides = {}) => ({
  url: () => endpoint,
  request: () => ({ method: () => "POST", postData: () => "question-1" }),
  status: () => 200,
  headers: () => ({ "content-type": "application/connect+json" }),
  body: async () => body,
  ...overrides,
});
const options = (submit, extra = {}) => ({
  endpoint,
  matchRequest: (request) => request.postData() === "question-1",
  submit,
  timeoutMs: 50,
  ...extra,
});

test("captures only the exact browser request without metadata or search claims", async () => {
  const page = new EventEmitter();
  let submissions = 0;
  const result = await captureConnectExchange(
    page,
    options(async () => {
      submissions += 1;
      page.emit(
        "response",
        response({ url: () => `${endpoint}?token=ignored` }),
      );
      page.emit(
        "response",
        response({
          request: () => ({ method: () => "POST", postData: () => "other" }),
        }),
      );
      page.emit("response", response());
    }),
  );
  assert.equal(submissions, 1);
  assert.deepEqual(result.messages, [
    { message: { id: "message-1", text: "中文" } },
  ]);
  assert.deepEqual(Object.keys(result).sort(), [
    "messages",
    "received_at",
    "started_at",
  ]);
  assert.ok(Date.parse(result.received_at) >= Date.parse(result.started_at));
  assert.equal(page.listenerCount("response"), 0);
});

test("duplicate matching responses remain ambiguous even with identical bytes", async () => {
  const page = new EventEmitter();
  assert.equal(
    await captureConnectExchange(
      page,
      options(async () => {
        page.emit("response", response());
        page.emit("response", response());
      }),
    ),
    null,
  );
  assert.equal(page.listenerCount("response"), 0);
});

test("status, content type, incomplete streams and server errors cannot complete", async () => {
  for (const override of [
    { status: () => 401 },
    { headers: () => ({ "content-type": "application/json" }) },
    { body: async () => frame(0, { text: "answer without final frame" }) },
    {
      body: async () =>
        Buffer.concat([
          frame(0, { text: "partial" }),
          frame(2, {
            error: { code: "unavailable", message: "private detail" },
          }),
        ]),
    },
  ]) {
    const page = new EventEmitter();
    assert.equal(
      await captureConnectExchange(
        page,
        options(async () => page.emit("response", response(override))),
      ),
      null,
    );
    assert.equal(page.listenerCount("response"), 0);
  }
});

test("rejects oversized declared body before buffering and bounds undeclared body", async () => {
  const page = new EventEmitter();
  let reads = 0;
  const oversized = response({
    headers: () => ({
      "content-type": "application/connect+json",
      "content-length": "9999",
    }),
    body: async () => {
      reads += 1;
      return body;
    },
  });
  assert.equal(
    await captureConnectExchange(
      page,
      options(async () => page.emit("response", oversized), {
        maxTotalBytes: 32,
      }),
    ),
    null,
  );
  assert.equal(reads, 0);
  assert.equal(
    await captureConnectExchange(
      page,
      options(async () => page.emit("response", response()), {
        maxTotalBytes: 32,
      }),
    ),
    null,
  );
});

test("deadline and external cancellation detach listeners and stop the submission", async () => {
  for (const external of [false, true]) {
    const page = new EventEmitter();
    const controller = new AbortController();
    let childSignal;
    const result = await captureConnectExchange(
      page,
      options(
        async (_page, signal) => {
          childSignal = signal;
          if (external) controller.abort();
          await new Promise(() => {});
        },
        { signal: controller.signal, timeoutMs: 10 },
      ),
    );
    assert.equal(result, null);
    assert.equal(childSignal.aborted, true);
    assert.equal(page.listenerCount("response"), 0);
  }
});

test("late body resolution after timeout cannot produce a result or retry", async () => {
  const page = new EventEmitter();
  let release;
  let submissions = 0;
  const result = await captureConnectExchange(
    page,
    options(
      async () => {
        submissions += 1;
        page.emit(
          "response",
          response({
            body: () => new Promise((resolve) => (release = resolve)),
          }),
        );
      },
      { timeoutMs: 10 },
    ),
  );
  assert.equal(result, null);
  release(body);
  await Promise.resolve();
  assert.equal(page.listenerCount("response"), 0);
  assert.equal(submissions, 1);
});

test("invalid or pre-aborted capture never submits", async () => {
  const controller = new AbortController();
  controller.abort();
  for (const extra of [
    { signal: controller.signal },
    { endpoint: "https://user:password@example.invalid/stream" },
    { endpoint: `${endpoint}?token=x` },
    { timeoutMs: Infinity },
  ]) {
    assert.equal(
      await captureConnectExchange(
        new EventEmitter(),
        options(async () => assert.fail("must not submit"), extra),
      ),
      null,
    );
  }
});

test("captures a real browser UI fetch without copying its session or submitting twice", async () => {
  // Force a length-header high byte that ordinary text-decoded response.body()
  // corrupts under application/connect+json.
  const message = {
    message: { id: "message-1", text: "中文" + "x".repeat(150) },
  };
  let sent = 0;
  let cookieSeen = false;
  const server = createServer((request, reply) => {
    if (request.method === "GET" && request.url === "/") {
      reply.writeHead(200, {
        "content-type": "text/html",
        "set-cookie": "session=fixture-browser-session; HttpOnly; SameSite=Lax",
      });
      reply.end(
        `<button id="submit" onclick="fetch('/stream',{method:'POST',body:'question-1'})">Send fixture</button>`,
      );
      return;
    }
    if (request.method === "POST" && request.url === "/stream") {
      sent += 1;
      cookieSeen = request.headers.cookie === "session=fixture-browser-session";
      reply.writeHead(200, { "content-type": "application/connect+json" });
      const encoded = frame(0, message);
      reply.write(encoded.subarray(0, 3));
      setTimeout(() => {
        reply.write(encoded.subarray(3));
        reply.end(
          frame(2, { metadata: { "private-fixture": ["not-a-public-field"] } }),
        );
      }, 30);
      return;
    }
    reply.writeHead(404).end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  let browser;
  try {
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
    });
    const page = await browser.newPage();
    const origin = `http://127.0.0.1:${server.address().port}`;
    await page.goto(origin);
    const result = await captureConnectExchange(page, {
      ...options(async (page) => page.locator("#submit").click()),
      endpoint: `${origin}/stream`,
      timeoutMs: 5000,
    });
    assert.deepEqual(result?.messages, [message]);
    assert.equal(sent, 1);
    assert.equal(cookieSeen, true);
    assert.equal(
      JSON.stringify(result).includes("fixture-browser-session"),
      false,
    );
    assert.equal(JSON.stringify(result).includes("not-a-public-field"), false);
    assert.equal(page.listenerCount("response"), 0);
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
