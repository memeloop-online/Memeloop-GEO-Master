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
    "connect_json_terminal",
    "messages",
    "received_at",
    "started_at",
  ]);
  assert.equal(result.connect_json_terminal, true);
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

test("unavailable raw-byte support never submits or falls back to text bodies", async () => {
  const page = new EventEmitter();
  const session = new EventEmitter();
  let detached = 0;
  session.send = async () => {
    throw new Error("fixture raw transport unavailable");
  };
  session.detach = async () => {
    detached += 1;
  };
  page.context = () => ({ newCDPSession: async () => session });
  const result = await captureConnectExchange(
    page,
    options(async () => assert.fail("must not submit")),
  );
  assert.equal(result, null);
  assert.equal(detached, 1);
  assert.equal(page.listenerCount("response"), 0);
});

test("deadline also bounds raw-byte initialization and closes a late session", async () => {
  const page = new EventEmitter();
  const session = new EventEmitter();
  let finishEnable;
  let detached = 0;
  session.send = () => new Promise((resolve) => (finishEnable = resolve));
  session.detach = async () => {
    detached += 1;
  };
  page.context = () => ({ newCDPSession: async () => session });
  const result = await captureConnectExchange(
    page,
    options(async () => assert.fail("must not submit"), { timeoutMs: 10 }),
  );
  assert.equal(result, null);
  assert.equal(page.listenerCount("response"), 0);
  finishEnable({});
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(detached, 1);
});

test("raw capture joins buffered and streamed bytes and refuses unrelated requests or oversized bytes", async () => {
  for (const scenario of ["valid", "mismatch", "oversized", "broken"]) {
    const page = new EventEmitter();
    const session = new EventEmitter();
    let detached = 0;
    session.send = async (method) => {
      if (method === "Network.enable") return {};
      assert.equal(method, "Network.streamResourceContent");
      return { bufferedData: body.subarray(0, 3).toString("base64") };
    };
    session.detach = async () => {
      detached += 1;
    };
    page.context = () => ({ newCDPSession: async () => session });
    const result = await captureConnectExchange(
      page,
      options(
        async () => {
          session.emit("Network.responseReceived", {
            requestId: "raw-1",
            response: { url: endpoint },
          });
          page.emit(
            "response",
            response({
              request: () => ({
                method: () => "POST",
                postData: () =>
                  scenario === "mismatch" ? "other-question" : "question-1",
              }),
              body: () => assert.fail("never use text-decoded body"),
            }),
          );
          session.emit("Network.dataReceived", {
            requestId: "raw-1",
            data: body.subarray(3).toString("base64"),
          });
          session.emit(
            scenario === "broken"
              ? "Network.loadingFailed"
              : "Network.loadingFinished",
            { requestId: "raw-1" },
          );
        },
        { maxTotalBytes: scenario === "oversized" ? 32 : 4096 },
      ),
    );
    if (scenario === "valid") {
      assert.deepEqual(result?.messages, [
        { message: { id: "message-1", text: "中文" } },
      ]);
    } else {
      assert.equal(result, null, scenario);
    }
    assert.equal(detached, 1);
    assert.equal(page.listenerCount("response"), 0);
  }
});

test("raw capture accepts a complete buffered body when loading finishes before the stream command", async () => {
  const page = new EventEmitter();
  const session = new EventEmitter();
  let finishStream;
  let detached = 0;
  let submissions = 0;
  session.send = (method) => {
    if (method === "Network.enable") return Promise.resolve({});
    assert.equal(method, "Network.streamResourceContent");
    return new Promise((resolve) => {
      finishStream = resolve;
    });
  };
  session.detach = async () => {
    detached++;
  };
  page.context = () => ({ newCDPSession: async () => session });
  const result = await captureConnectExchange(
    page,
    options(async () => {
      submissions++;
      session.emit("Network.responseReceived", {
        requestId: "buffered-1",
        response: { url: endpoint },
      });
      page.emit(
        "response",
        response({ body: () => assert.fail("no decoded-body fallback") }),
      );
      session.emit("Network.loadingFinished", { requestId: "buffered-1" });
      finishStream({ bufferedData: body.toString("base64") });
    }),
  );
  assert.deepEqual(result?.messages, [
    { message: { id: "message-1", text: "中文" } },
  ]);
  assert.equal(submissions, 1);
  assert.equal(detached, 1);
  assert.equal(page.listenerCount("response"), 0);
});

test("captures a real browser UI fetch without copying its session or submitting twice", async () => {
  // Force a length-header high byte that ordinary text-decoded response.body()
  // corrupts under application/connect+json.
  const message = {
    message: { id: "message-1", text: "中文" + "x".repeat(150) },
  };
  let sent = 0;
  let cookieSeen = false;
  let releaseTail;
  const streamReady = new Promise((resolve) => {
    releaseTail = resolve;
  });
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
      void streamReady.then(() => {
        reply.write(encoded.subarray(3));
        reply.end(
          frame(2, { metadata: { "private-fixture": ["not-a-public-field"] } }),
        );
      });
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
    const context = await browser.newContext();
    const page = await context.newPage();
    const newCDPSession = context.newCDPSession.bind(context);
    // Release the real server's tail only after Chromium enables raw streaming.
    // A wall-clock gap races the CDP command under a loaded CI event loop.
    context.newCDPSession = async (...args) => {
      const session = await newCDPSession(...args);
      const send = session.send.bind(session);
      session.send = async (method, ...parameters) => {
        const result = await send(method, ...parameters);
        if (method === "Network.streamResourceContent") releaseTail();
        return result;
      };
      return session;
    };
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
