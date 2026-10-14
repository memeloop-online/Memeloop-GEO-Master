import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { after, before, test } from "node:test";
import { chromium } from "playwright";
import {
  readDeepSeekFirstTurnHistory,
  readDeepSeekCleanupHistory,
  projectDeepSeekCleanupHistory,
} from "../src/deepseek-web-history.mjs";
import { validDeepSeekLifecycle } from "../src/deepseek-conversation-evidence.mjs";

const ORIGIN = "https://chat.deepseek.com";
const expected = {
  conversationId: "synthetic-session",
  requestMessageId: 1,
  responseMessageId: 2,
};
const historyUrl = `${ORIGIN}/api/v0/chat/history_messages?chat_session_id=synthetic-session`;
const fixture = () => ({
  code: 0,
  data: {
    biz_code: 0,
    biz_data: {
      cache_control: "REPLACE",
      chat_session: { id: expected.conversationId, current_message_id: 2 },
      chat_messages: [
        {
          message_id: 2,
          parent_id: 1,
          role: "ASSISTANT",
          status: "FINISHED",
          has_pending_fragment: false,
          quasi_status: null,
          fragments: [{ content: "synthetic answer omitted from projection" }],
        },
        {
          message_id: 1,
          parent_id: null,
          role: "USER",
          status: "FINISHED",
          has_pending_fragment: false,
          fragments: [],
        },
      ],
    },
  },
});
let browser;
before(async () => {
  browser = await chromium.launch(
    process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {},
  );
});
after(async () => {
  await browser?.close();
});

async function synthetic(t, reply = {}) {
  const context = await browser.newContext({ serviceWorkers: "block" });
  t.after(() => context.close());
  const requests = [];
  const state = { body: JSON.stringify(fixture()), ...reply };
  await context.route("**/*", async (route) => {
    const request = route.request();
    if (request.isNavigationRequest())
      return route.fulfill({
        contentType: "text/html",
        body: "<title>test</title>",
      });
    requests.push(request);
    if (request.url() !== historyUrl) return route.abort();
    if (state.wait) await state.wait;
    return route
      .fulfill({
        status: state.status ?? 200,
        contentType: state.contentType ?? "application/json",
        headers: state.headers,
        body: state.body,
      })
      .catch(() => {});
  });
  const page = await context.newPage();
  await page.goto(`${ORIGIN}/a/chat/synthetic-session`);
  await page.evaluate(() =>
    localStorage.setItem(
      "userToken",
      JSON.stringify({ __version: "0", value: "synthetic-history-token" }),
    ),
  );
  return { context, page, requests, state };
}

test("actual Chromium GET omits null cache query and returns only verified observed metadata", async (t) => {
  const { page, requests } = await synthetic(t);
  const href = page.url();
  const answer = await readDeepSeekFirstTurnHistory(page, expected);
  assert.deepEqual(answer.inventory, {
    conversationId: expected.conversationId,
    messages: [
      { id: "1", role: "user" },
      { id: "2", role: "assistant" },
    ],
  });
  assert.equal(requests.length, 1);
  assert.equal(requests[0].method(), "GET");
  assert.equal(requests[0].url(), historyUrl);
  const requestedUrl = new URL(requests[0].url());
  assert.equal(
    requestedUrl.search,
    `?chat_session_id=${encodeURIComponent(expected.conversationId)}`,
  );
  assert.equal(requestedUrl.searchParams.has("cache_version"), false);
  assert.equal(requestedUrl.searchParams.has("cache_reset_at"), false);
  assert.equal(
    requests[0].headers().authorization,
    "Bearer synthetic-history-token",
  );
  assert.equal(requests[0].postData(), null);
  assert.equal(page.url(), href);
  assert.equal(answer.history.source, "browser_observed_history");
  assert.equal(answer.history.current_message_id, 2);
  assert.equal(answer.history.messages[0].message_id, 2);
  assert.equal(Object.hasOwn(answer.history.messages[0], "quasi_status"), true);
  assert.equal(
    Object.hasOwn(answer.history.messages[1], "quasi_status"),
    false,
  );
  assert.deepEqual(answer.history.cache_request, {
    cache_version: null,
    cache_reset_at: null,
  });
  assert.equal(
    new Date(answer.history.observed_at).toISOString(),
    answer.history.observed_at,
  );
  assert.equal(
    JSON.stringify(answer).includes("synthetic-history-token"),
    false,
  );
  assert.equal(JSON.stringify(answer).includes("synthetic answer"), false);
  assert.equal(JSON.stringify(answer).includes("fragments"), false);
  assert.equal(
    validDeepSeekLifecycle({
      schema_version: "geo.deepseek.lifecycle.v1",
      provider: "deepseek",
      creation: {
        source: "browser_observed_create",
        chat_session_id: expected.conversationId,
      },
      submission: {
        chat_session_id: expected.conversationId,
        parent_message_id: null,
        model_type: "synthetic-model",
        prompt_sha256: "a".repeat(64),
        search_enabled: true,
        thinking_enabled: false,
      },
      ready: { request_message_id: 1, response_message_id: 2 },
      sse_terminal: true,
      history: answer.history,
    }),
    true,
  );
});

test("absent optional current message remains absent rather than fabricated", async (t) => {
  const body = fixture();
  delete body.data.biz_data.chat_session.current_message_id;
  const { page } = await synthetic(t, { body: JSON.stringify(body) });
  const answer = await readDeepSeekFirstTurnHistory(page, expected);
  assert.equal(Object.hasOwn(answer.history, "current_message_id"), false);
});

test("missing, malformed, blank and wrong-version auth never issue a request", async (t) => {
  const { page, requests } = await synthetic(t);
  for (const stored of [
    null,
    "{",
    JSON.stringify({ __version: "1", value: "synthetic" }),
    JSON.stringify({ __version: "0", value: "" }),
    JSON.stringify({ __version: "0", value: "  " }),
    JSON.stringify({ __version: "0", value: 7 }),
  ]) {
    await page.evaluate((value) => {
      if (value === null) localStorage.removeItem("userToken");
      else localStorage.setItem("userToken", value);
    }, stored);
    assert.equal(await readDeepSeekFirstTurnHistory(page, expected), null);
  }
  assert.equal(requests.length, 0);
});

test("untrusted origin, malformed identifiers and invalid deadlines cannot fetch", async (t) => {
  const { page, requests } = await synthetic(t);
  for (const change of [
    { conversationId: "../other" },
    { requestMessageId: -0 },
    { requestMessageId: "01" },
    { responseMessageId: 1 },
    { timeoutMs: 0 },
    { timeoutMs: Infinity },
    { timeoutMs: 120_001 },
  ])
    assert.equal(
      await readDeepSeekFirstTurnHistory(page, { ...expected, ...change }),
      null,
    );
  await page.goto("https://example.invalid/");
  assert.equal(await readDeepSeekFirstTurnHistory(page, expected), null);
  assert.equal(requests.length, 0);
});

test("redirects never follow, and HTTP/MIME/malformed responses fail closed", async (t) => {
  const { page, requests, state } = await synthetic(t);
  for (const reply of [
    { status: 302, headers: { location: "https://example.invalid/steal" } },
    { status: 403 },
    { contentType: "text/html" },
    { body: "{" },
  ]) {
    Object.assign(
      state,
      {
        status: 200,
        headers: {},
        contentType: "application/json",
        body: JSON.stringify(fixture()),
      },
      reply,
    );
    const reasons = [];
    assert.equal(
      await readDeepSeekFirstTurnHistory(page, {
        ...expected,
        onFailureReason: (reason) => reasons.push(reason),
      }),
      null,
    );
    assert.deepEqual(reasons, [
      reply.status === 302
        ? "transport_unavailable"
        : reply.body === "{"
          ? "history_envelope_unverified"
          : "response_envelope_unverified",
    ]);
  }
  assert.equal(requests.length, 4);
  assert.ok(requests.every((request) => request.url() === historyUrl));
});

test("byte bounds include UTF-8 bytes, accepting exact cap and rejecting excess", async (t) => {
  const { page, state } = await synthetic(t);
  const body = { ...fixture(), padding: "" };
  const remaining = 750_000 - Buffer.byteLength(JSON.stringify(body));
  body.padding = "x".repeat(remaining);
  state.body = JSON.stringify(body);
  assert.ok(await readDeepSeekFirstTurnHistory(page, expected));
  body.padding += "é";
  state.body = JSON.stringify(body);
  assert.equal(await readDeepSeekFirstTurnHistory(page, expected), null);
});

test("response diagnostics are bounded Node categories without response payload or extra requests", async (t) => {
  const { page, requests } = await synthetic(t);
  const logs = [];
  t.mock.method(console, "info", (entry) => logs.push(entry));
  const cases = [
    [{ status: 401 }, "http_unauthorized"],
    [{ status: 403 }, "http_forbidden"],
    [{ status: 404 }, "http_not_found"],
    [{ status: 429 }, "http_rate_limited"],
    [{ status: 500 }, "http_server_error"],
    [{ status: 599 }, "http_server_error"],
    [{ status: 204 }, "http_other"],
    [{ status: 302 }, "http_other"],
    [{ redirected: true }, "redirect_unverified"],
    [{ url: "https://example.invalid/synthetic-private" }, "url_unverified"],
    [
      { contentType: "text/html; synthetic-private" },
      "content_type_unverified",
    ],
    [{ contentType: "" }, "content_type_unverified"],
    [{ contentLength: "synthetic-private" }, "content_length_invalid"],
    [{ contentLength: "750001" }, "content_length_exceeded"],
    [{ kind: "no-body" }, "body_unavailable"],
    [{ kind: "locked-body" }, "body_unavailable", "transport_unavailable"],
    [{ kind: "read-error" }, "body_read_failed", "transport_unavailable"],
    [{ kind: "oversize" }, "byte_limit_exceeded"],
    [{ kind: "invalid-utf8" }, "utf8_invalid"],
    [{ kind: "incomplete-utf8" }, "utf8_invalid"],
  ];
  for (const [
    reply,
    category,
    reason = "response_envelope_unverified",
  ] of cases) {
    await page.evaluate(
      ({ reply, body }) => {
        globalThis.syntheticHistoryFetches = 0;
        globalThis.syntheticHistoryReads = 0;
        globalThis.fetch = async (url) => {
          globalThis.syntheticHistoryFetches++;
          const stream = new ReadableStream({
            start(controller) {
              if (reply.kind === "read-error") {
                controller.error(new Error("synthetic-private-body-error"));
                return;
              }
              const bytes =
                reply.kind === "oversize"
                  ? new Uint8Array(750_001)
                  : reply.kind === "invalid-utf8"
                    ? new Uint8Array([0xff])
                    : reply.kind === "incomplete-utf8"
                      ? new Uint8Array([0xc3])
                      : new TextEncoder().encode(body);
              controller.enqueue(bytes);
              controller.close();
            },
          });
          const reader = stream.getReader.bind(stream);
          stream.getReader = () => {
            globalThis.syntheticHistoryReads++;
            if (reply.kind === "locked-body")
              throw new Error("synthetic-private-reader-error");
            return reader();
          };
          return {
            status: reply.status ?? 200,
            redirected: reply.redirected ?? false,
            url: reply.url ?? url,
            headers: new Headers({
              "content-type": reply.contentType ?? "application/json",
              ...(reply.contentLength !== undefined
                ? { "content-length": reply.contentLength }
                : {}),
              "x-synthetic-private": "synthetic-private-header",
            }),
            body: reply.kind === "no-body" ? null : stream,
          };
        };
      },
      { reply, body: JSON.stringify(fixture()) },
    );
    logs.length = 0;
    const reasons = [];
    assert.equal(
      await readDeepSeekFirstTurnHistory(page, {
        ...expected,
        onFailureReason: (value) => reasons.push(value),
      }),
      null,
    );
    assert.deepEqual(reasons, [reason]);
    assert.deepEqual(logs, [
      JSON.stringify({ stage: "history_response", category }),
    ]);
    assert.equal(
      await page.evaluate(() => globalThis.syntheticHistoryFetches),
      1,
    );
    assert.equal(
      await page.evaluate(() => globalThis.syntheticHistoryReads),
      reply.kind && reply.kind !== "no-body" ? 1 : 0,
    );
  }
  assert.equal(requests.length, 0);
});

test("native browser streams enforce actual byte cap and fatal UTF-8 without a content length", async (t) => {
  const { page, requests } = await synthetic(t);
  for (const kind of ["oversize", "invalid-utf8", "hanging"]) {
    await page.evaluate(
      ({ kind, url }) => {
        globalThis.syntheticCancelled = false;
        globalThis.fetch = async (target, options) => {
          if (
            target !== url ||
            options.method !== "GET" ||
            options.credentials !== "include" ||
            options.redirect !== "error" ||
            options.cache !== "no-store"
          )
            throw new Error("unexpected request contract");
          const body = new ReadableStream({
            start(controller) {
              if (kind === "oversize") {
                controller.enqueue(new Uint8Array(400_000));
                controller.enqueue(new Uint8Array(350_001));
              } else if (kind === "invalid-utf8") {
                controller.enqueue(new Uint8Array([0xff]));
              }
              options.signal.addEventListener(
                "abort",
                () => {
                  controller.error(new DOMException("aborted", "AbortError"));
                },
                { once: true },
              );
            },
            cancel() {
              globalThis.syntheticCancelled = true;
            },
          });
          const response = new Response(body, {
            headers: { "content-type": "application/json" },
          });
          Object.defineProperty(response, "url", { value: url });
          return response;
        };
      },
      { kind, url: historyUrl },
    );
    const reasons = [];
    assert.equal(
      await readDeepSeekFirstTurnHistory(page, {
        ...expected,
        timeoutMs: kind === "hanging" ? 50 : 1000,
        onFailureReason: (reason) => reasons.push(reason),
      }),
      null,
    );
    assert.deepEqual(reasons, [
      kind === "hanging"
        ? "read_deadline_exceeded"
        : "response_envelope_unverified",
    ]);
    if (kind !== "hanging")
      assert.equal(
        await page.evaluate(() => globalThis.syntheticCancelled),
        true,
      );
  }
  assert.equal(requests.length, 0);
});

test("all nodes are verified; extra messages, incomplete graph and wrong binding are rejected", async (t) => {
  const { page, state } = await synthetic(t);
  for (const mutate of [
    (data) =>
      data.chat_messages.push({ ...data.chat_messages[0], message_id: 3 }),
    (data) => {
      data.chat_messages[0].has_pending_fragment = true;
    },
    (data) => {
      data.chat_messages[0].status = "RUNNING";
    },
    (data) => {
      data.chat_session.id = "other-synthetic-session";
    },
    (data) => {
      data.cache_control = "APPEND";
    },
    (data) => {
      data.chat_messages[0].parent_id = 3;
    },
  ]) {
    const body = fixture();
    mutate(body.data.biz_data);
    state.body = JSON.stringify(body);
    assert.equal(await readDeepSeekFirstTurnHistory(page, expected), null);
  }
});

test("navigation and caller cancellation invalidate in-flight browser requests", async (t) => {
  for (const action of [
    "same-origin-navigation",
    "cross-origin-navigation",
    "abort",
  ]) {
    let release;
    const wait = new Promise((resolve) => {
      release = resolve;
    });
    const { page } = await synthetic(t, { wait });
    const controller = new AbortController();
    const seen = page.waitForRequest(historyUrl);
    const reading = readDeepSeekFirstTurnHistory(page, {
      ...expected,
      signal: controller.signal,
    });
    await seen;
    if (action === "abort") controller.abort();
    else
      await page.goto(
        action === "same-origin-navigation"
          ? `${ORIGIN}/other`
          : "https://example.invalid/",
      );
    assert.equal(await reading, null);
    release();
  }
});

test("delayed browser evaluation after timeout or abort never sends a request", async (t) => {
  for (const action of ["timeout", "abort"]) {
    const { page, requests } = await synthetic(t);
    const controller = new AbortController();
    let release;
    let entered;
    const delayed = new Promise((resolve) => {
      release = resolve;
    });
    const reached = new Promise((resolve) => {
      entered = resolve;
    });
    const wrapped = {
      url: () => page.url(),
      isClosed: () => page.isClosed(),
      mainFrame: () => page.mainFrame(),
      on: (...args) => page.on(...args),
      off: (...args) => page.off(...args),
      evaluateHandle: (...args) => page.evaluateHandle(...args),
      evaluate: async (...args) => {
        entered();
        await delayed;
        return page.evaluate(...args);
      },
    };
    const reading = readDeepSeekFirstTurnHistory(wrapped, {
      ...expected,
      signal: controller.signal,
      timeoutMs: action === "timeout" ? 100 : 1000,
    });
    await reached;
    if (action === "abort") controller.abort();
    assert.equal(await reading, null);
    await page.evaluate(() => true);
    release();
    await page.evaluate(() => true);
    assert.equal(requests.length, 0);
  }
});

class FakePage extends EventEmitter {
  constructor() {
    super();
    this.handle = {
      evaluate: async () => {},
      dispose: async () => {
        this.disposed = true;
      },
    };
  }
  url() {
    return `${ORIGIN}/`;
  }
  isClosed() {
    return false;
  }
  mainFrame() {
    return this;
  }
  async evaluateHandle() {
    return this.handle;
  }
  evaluate() {
    return new Promise(() => {});
  }
}
const clean = (page) => {
  assert.equal(page.listenerCount("framenavigated"), 0);
  assert.equal(page.listenerCount("close"), 0);
};

test("unknown diagnostic carriers and logger failure cannot change history failure", async (t) => {
  const logs = [];
  const logger = t.mock.method(console, "info", (entry) => logs.push(entry));
  for (const category of [
    undefined,
    null,
    { secret: "synthetic-private" },
    "synthetic-private",
  ]) {
    const page = new FakePage();
    page.evaluate = async () => [
      null,
      "response_envelope_unverified",
      category,
    ];
    const reasons = [];
    assert.equal(
      await readDeepSeekFirstTurnHistory(page, {
        ...expected,
        onFailureReason: (reason) => reasons.push(reason),
      }),
      null,
    );
    assert.deepEqual(reasons, ["response_envelope_unverified"]);
  }
  assert.deepEqual(logs, []);
  logger.mock.mockImplementation(() => {
    throw new Error("synthetic-private-log-error");
  });
  const page = new FakePage();
  page.evaluate = async () => [
    null,
    "response_envelope_unverified",
    "url_unverified",
  ];
  const reasons = [];
  assert.equal(
    await readDeepSeekFirstTurnHistory(page, {
      ...expected,
      onFailureReason: (reason) => reasons.push(reason),
    }),
    null,
  );
  assert.deepEqual(reasons, ["response_envelope_unverified"]);
});

test("hung evaluate and handle acquisition settle by outer deadline without awaiting cleanup", async () => {
  for (const stage of ["evaluate", "handle", "cleanup"]) {
    const page = new FakePage();
    if (stage === "handle") page.evaluateHandle = () => new Promise(() => {});
    if (stage === "cleanup") page.handle.evaluate = () => new Promise(() => {});
    const started = Date.now();
    const reasons = [];
    assert.equal(
      await readDeepSeekFirstTurnHistory(page, {
        ...expected,
        timeoutMs: 20,
        onFailureReason: (reason) => reasons.push(reason),
      }),
      null,
    );
    assert.deepEqual(reasons, ["read_deadline_exceeded"]);
    assert.ok(Date.now() - started < 1000);
    clean(page);
    if (stage !== "handle") {
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(page.disposed, true);
    }
  }
});

test("abort, close, navigation and late rejected evaluate remain inert and clean listeners", async () => {
  for (const action of ["abort", "close", "navigation"]) {
    const page = new FakePage();
    let reject;
    page.evaluate = () =>
      new Promise((_, fail) => {
        reject = fail;
      });
    const controller = new AbortController();
    const reasons = [];
    const reading = readDeepSeekFirstTurnHistory(page, {
      ...expected,
      signal: controller.signal,
      onFailureReason: (reason) => reasons.push(reason),
    });
    await new Promise((resolve) => setImmediate(resolve));
    if (action === "abort") controller.abort();
    if (action === "close") page.emit("close");
    if (action === "navigation") page.emit("framenavigated", page);
    assert.equal(await reading, null);
    reject(new Error("synthetic private error must remain suppressed"));
    await new Promise((resolve) => setImmediate(resolve));
    clean(page);
    assert.deepEqual(reasons, ["document_unavailable"]);
  }
});

test("cancellation during delayed handle acquisition cannot start a later fetch", async () => {
  const page = new FakePage();
  const controller = new AbortController();
  let release;
  page.evaluateHandle = () =>
    new Promise((resolve) => {
      release = resolve;
    });
  page.evaluate = () => assert.fail("read must not begin after cancellation");
  const reading = readDeepSeekFirstTurnHistory(page, {
    ...expected,
    signal: controller.signal,
  });
  controller.abort();
  assert.equal(await reading, null);
  release(page.handle);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(page.disposed, true);
  clean(page);
});

test("already-aborted caller signal never opens the browser handle", async () => {
  const page = new FakePage();
  page.evaluateHandle = () => assert.fail("already cancelled");
  const controller = new AbortController();
  controller.abort();
  assert.equal(
    await readDeepSeekFirstTurnHistory(page, {
      ...expected,
      signal: controller.signal,
    }),
    null,
  );
  clean(page);
});

test("cleanup fresh reader derives candidates but measurement still requires expected IDs", async (t) => {
  const { page, requests } = await synthetic(t);
  const result = await readDeepSeekCleanupHistory(page, {
    conversationId: expected.conversationId,
  });
  assert.deepEqual(result, {
    status: "verified",
    inventory: {
      conversationId: expected.conversationId,
      messages: [
        { id: "1", role: "user" },
        { id: "2", role: "assistant" },
      ],
    },
  });
  assert.equal(
    await readDeepSeekFirstTurnHistory(page, {
      conversationId: expected.conversationId,
    }),
    null,
  );
  assert.equal(requests.length, 1);
  assert.equal(JSON.stringify(result).includes("synthetic answer"), false);
});

test("cleanup reader distinguishes strict business absence from empty/error history", async (t) => {
  const { page, state } = await synthetic(t);
  for (const [response, status] of [
    [{ code: 0, data: { biz_code: 1 } }, "absent"],
    [{ code: 0, data: { biz_code: 5 } }, "unknown"],
    [{ code: 1, data: { biz_code: 1 } }, "unknown"],
    [{ code: 0, data: { biz_code: "1" } }, "unknown"],
    [
      { code: 0, data: { biz_code: 0, biz_data: { chat_messages: [] } } },
      "unknown",
    ],
  ]) {
    state.body = JSON.stringify(response);
    assert.deepEqual(
      await readDeepSeekCleanupHistory(page, {
        conversationId: expected.conversationId,
      }),
      { status },
    );
  }
  state.status = 404;
  state.body = JSON.stringify({ code: 0, data: { biz_code: 1 } });
  assert.deepEqual(
    await readDeepSeekCleanupHistory(page, {
      conversationId: expected.conversationId,
    }),
    { status: "unknown" },
  );
});

test("candidate IDs cannot bypass raw numeric, complete graph or terminal verification", () => {
  const raw = JSON.stringify(fixture());
  for (const body of [
    raw.replace('"message_id":2', '"message_id":2.0'),
    raw.replace('"message_id":2', '"message_id":2e0'),
    raw.replace('"message_id":2', '"message_id":"2"'),
    raw.replace('"parent_id":1', '"parent_id":0'),
    raw.replace('"has_pending_fragment":false', '"has_pending_fragment":true'),
    raw.replace('"status":"FINISHED"', '"status":"RUNNING"'),
    raw.replace('"cache_control":"REPLACE"', '"cache_control":"APPEND"'),
    raw.replace('"id":"synthetic-session"', '"id":"other-session"'),
  ])
    assert.deepEqual(
      projectDeepSeekCleanupHistory(
        { status: 200, body },
        expected.conversationId,
      ),
      { status: "unknown" },
    );
});

test("document and browser transport failures expose only one reason while remaining null", async (t) => {
  const { page, requests } = await synthetic(t);
  for (const kind of ["auth", "fetch"]) {
    await page.evaluate((kind) => {
      if (kind === "auth") localStorage.setItem("userToken", "{");
      else {
        localStorage.setItem(
          "userToken",
          JSON.stringify({ __version: "0", value: "synthetic-token" }),
        );
        globalThis.fetch = async () => {
          throw new Error("synthetic private transport detail");
        };
      }
    }, kind);
    const reasons = [];
    assert.equal(
      await readDeepSeekFirstTurnHistory(page, {
        ...expected,
        onFailureReason: (reason) => reasons.push(reason),
      }),
      null,
    );
    assert.deepEqual(reasons, [
      kind === "auth" ? "document_unavailable" : "transport_unavailable",
    ]);
  }
  assert.equal(requests.length, 0);
});

test("late successful evaluation cannot change a settled deadline reason or start cleanup", async () => {
  const page = new FakePage();
  let release;
  page.evaluate = () =>
    new Promise((resolve) => {
      release = resolve;
    });
  const reasons = [];
  assert.equal(
    await readDeepSeekFirstTurnHistory(page, {
      ...expected,
      timeoutMs: 20,
      onFailureReason: (reason) => reasons.push(reason),
    }),
    null,
  );
  assert.deepEqual(reasons, ["read_deadline_exceeded"]);
  release({ status: 200, body: JSON.stringify(fixture()) });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(reasons, ["read_deadline_exceeded"]);
  clean(page);
});
