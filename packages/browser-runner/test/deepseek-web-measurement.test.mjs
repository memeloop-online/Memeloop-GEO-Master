import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import {
  captureDeepSeekExchange,
  createDeepSeekEvidenceDecoder,
  inspectDeepSeekMeasurementOptions,
  matchesDeepSeekSubmission,
} from "../src/deepseek-web-measurement.mjs";

const origin = "https://chat.deepseek.com";
const endpoint = `${origin}/api/v0/chat/completion`;
const binding = {
  question: "Synthetic rainfall question?",
  model: "observed-model",
  chatSessionId: "synthetic-session",
  parentMessageId: null,
  searchEnabled: true,
  thinkingEnabled: false,
};
const body = {
  chat_session_id: binding.chatSessionId,
  parent_message_id: null,
  model_type: binding.model,
  prompt: binding.question,
  ref_file_ids: [],
  search_enabled: true,
  thinking_enabled: false,
};
const request = (patch = {}) => ({
  url: endpoint,
  method: "POST",
  postData: JSON.stringify({ ...body, ...patch }),
});
const event = (type, data) =>
  `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
const ready = event("ready", {
  request_message_id: 1,
  response_message_id: 2,
  model_type: binding.model,
});
// Raw delta remains opaque: the test asserts no answer assembly or search proof.
const delta = event("delta", { p: "response", o: "APPEND", v: "雨水" });
const finished = event("finish", { reason: "synthetic" });
const wire = ready + delta + finished;

test("real Chromium captures buffered and subsequent chunks from a local streaming HTTP server", async () => {
  let posts = 0;
  let continueResponse;
  let streamResponse;
  const server = createServer((incoming, response) => {
    if (incoming.method === "GET" && incoming.url === "/") {
      response.writeHead(200, { "content-type": "text/html" });
      response.end(`<button id="send">Send fixture</button><script>
        document.getElementById("send").onclick = () => fetch(
          "/api/v0/chat/completion",
          {method:"POST",headers:{"content-type":"application/json"},
           body:${JSON.stringify(JSON.stringify(body))}}
        ).then(response => response.text());
      </script>`);
    } else if (
      incoming.method === "POST" &&
      incoming.url === "/api/v0/chat/completion"
    ) {
      posts++;
      incoming.resume();
      streamResponse = response;
      response.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
      });
      response.write(ready);
      // Synchronize with actual CDP streaming attachment, not a guessed sleep.
      continueResponse = () => {
        response.write(delta);
        setImmediate(() => response.end(finished));
      };
    } else {
      response.writeHead(404);
      response.end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const localOrigin = `http://127.0.0.1:${server.address().port}`;
  const localEndpoint = `${localOrigin}/api/v0/chat/completion`;
  let browser;
  let buffered = "";
  let streamedEvents = 0;
  try {
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
    });
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.goto(localOrigin);
    // Production fixes the provider's origin. This test-only facade maps the
    // local fixture URL; request IDs, POST body, response headers and every byte
    // and ordering event still come from real Chromium/CDP, not Session doubles.
    const localPage = {
      url: () => `${origin}/`,
      context: () => ({
        async newCDPSession() {
          const session = await context.newCDPSession(page);
          const listeners = new Map();
          return {
            on(name, listener) {
              const mapped = (input) => {
                let observed = input;
                if (
                  name === "Network.requestWillBeSent" &&
                  input.request.url === localEndpoint
                ) {
                  observed = {
                    ...input,
                    request: { ...input.request, url: endpoint },
                  };
                } else if (
                  name === "Network.responseReceived" &&
                  input.response.url === localEndpoint
                ) {
                  observed = {
                    ...input,
                    response: { ...input.response, url: endpoint },
                  };
                } else if (
                  name === "Network.dataReceived" &&
                  input.data !== undefined
                ) {
                  streamedEvents++;
                }
                listener(observed);
              };
              listeners.set(listener, mapped);
              session.on(name, mapped);
            },
            off(name, listener) {
              session.off(name, listeners.get(listener));
              listeners.delete(listener);
            },
            async send(method, args) {
              const result = await session.send(method, args);
              if (method === "Network.streamResourceContent") {
                buffered = Buffer.from(
                  result.bufferedData,
                  "base64",
                ).toString();
                setImmediate(() => continueResponse());
              }
              return result;
            },
            detach: () => session.detach(),
          };
        },
      }),
    };
    const captured = await captureDeepSeekExchange(localPage, {
      binding,
      submit: () => page.locator("#send").click(),
      timeoutMs: 5000,
    });
    assert.equal(posts, 1);
    assert.equal(buffered, ready);
    assert.ok(streamedEvents > 0, "real CDP delivered bytes after attachment");
    assert.equal(
      captured?.sse_terminal,
      true,
      "a normal live stream must succeed",
    );
    assert.deepEqual(captured.messages, decode(wire).messages);
    assert.equal(captured.raw_answer, undefined);
    assert.equal(captured.search_event, undefined);
  } finally {
    streamResponse?.destroy();
    await browser?.close();
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
});

test("real Chromium recovers a completed response by its original request ID without another submission", async () => {
  await completedBrowserRecovery(true);
  await completedBrowserRecovery(false);
});

async function completedBrowserRecovery(declaredUtf8) {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  let posts = 0;
  const commands = [];
  try {
    const context = await browser.newContext();
    const openSession = context.newCDPSession.bind(context);
    context.newCDPSession = async (page) => {
      const session = await openSession(page);
      const finishedRequests = new Set();
      let requested;
      let release;
      const finishedRequest = new Promise((resolve) => (release = resolve));
      session.on("Network.loadingFinished", (event) => {
        finishedRequests.add(event.requestId);
        if (event.requestId === requested) release();
      });
      const send = session.send.bind(session);
      session.send = async (method, args) => {
        commands.push({ method, args });
        if (method === "Network.streamResourceContent") {
          // Deliberately attach after actual network EOF: real CDP produces
          // the error, and getResponseBody must recover the same real bytes.
          requested = args.requestId;
          if (!finishedRequests.has(requested)) await finishedRequest;
        }
        return send(method, args);
      };
      return session;
    };
    await context.route("**/*", async (route) => {
      if (route.request().url() === `${origin}/`) {
        await route.fulfill({
          contentType: "text/html",
          body: `<button id="send">Send fixture</button><script>
            document.getElementById("send").onclick = () => fetch(
              ${JSON.stringify(endpoint)},
              {method:"POST",headers:{"content-type":"application/json"},
               body:${JSON.stringify(JSON.stringify(body))}}
            ).then(response => response.text());
          </script>`,
        });
      } else if (route.request().url() === endpoint) {
        posts++;
        await route.fulfill({
          contentType: declaredUtf8
            ? "text/event-stream; charset=utf-8"
            : "text/event-stream",
          body: wire,
        });
      } else {
        await route.abort();
      }
    });
    const page = await context.newPage();
    await page.goto(`${origin}/`);
    const captured = await captureDeepSeekExchange(page, {
      binding,
      submit: () => page.locator("#send").click(),
      timeoutMs: 5000,
    });
    assert.equal(posts, 1);
    if (declaredUtf8) {
      assert.equal(captured?.sse_terminal, true);
      assert.deepEqual(captured.messages, decode(wire).messages);
    } else {
      // CDP's text body without a declared encoding may contain mojibake.
      // Do not mislabel that decoded text as the original UTF-8 source bytes.
      assert.equal(captured, null);
    }
    const stream = commands.filter(
      (item) => item.method === "Network.streamResourceContent",
    );
    const recovery = commands.filter(
      (item) => item.method === "Network.getResponseBody",
    );
    assert.equal(stream.length, 1);
    assert.equal(recovery.length, 1);
    assert.equal(recovery[0].args.requestId, stream[0].args.requestId);
    assert.deepEqual(commands[0], {
      method: "Network.enable",
      args: {
        maxTotalBufferSize: 750_000,
        maxResourceBufferSize: 750_000,
        maxPostDataSize: 64_000,
      },
    });
  } finally {
    await browser.close();
  }
}

test("model discovery requires current trusted page configuration without guessed defaults", async () => {
  const page = { url: () => `${origin}/` };
  assert.equal(await inspectDeepSeekMeasurementOptions(page), null);
  const readConfiguration = async (actualPage) => {
    assert.equal(actualPage, page);
    return {
      model_configs: [
        { model_type: "observed-model", enabled: true, switchable: true },
        { model_type: "hidden-model", enabled: false, switchable: true },
      ],
      selected_model: "observed-model",
    };
  };
  assert.deepEqual(
    await inspectDeepSeekMeasurementOptions(page, { readConfiguration }),
    {
      models: [{ id: "observed-model", label: "observed-model" }],
      selected_model: "observed-model",
    },
  );
  for (const config of [
    { model_configs: [] },
    { model_configs: [{ model_type: "guessed" }] },
    {
      model_configs: [{ model_type: "real", enabled: true, switchable: true }],
      selected_model: "unobserved",
    },
    {
      model_configs: Array(2).fill({
        model_type: "duplicate",
        enabled: true,
        switchable: true,
      }),
    },
  ]) {
    assert.equal(
      await inspectDeepSeekMeasurementOptions(page, {
        readConfiguration: async () => config,
      }),
      null,
    );
  }
  let reads = 0;
  assert.equal(
    await inspectDeepSeekMeasurementOptions(
      { url: () => "https://example.org/" },
      { readConfiguration: async () => reads++ },
    ),
    null,
  );
  assert.equal(reads, 0);
});

test("request binding rejects wrong question/model/session/search, reuse, attachments and replay actions", () => {
  assert.equal(matchesDeepSeekSubmission(request(), binding), true);
  for (const patch of [
    { prompt: "different" },
    { model_type: "different" },
    { chat_session_id: "different" },
    { parent_message_id: 1 },
    { search_enabled: false },
    { thinking_enabled: true },
    { ref_file_ids: ["attachment"] },
    { action: "regenerate" },
    { child_message_id: 2 },
  ]) {
    assert.equal(matchesDeepSeekSubmission(request(patch), binding), false);
  }
  assert.equal(
    matchesDeepSeekSubmission(
      { ...request(), url: `${endpoint}?other=1` },
      binding,
    ),
    false,
  );
  assert.equal(
    matchesDeepSeekSubmission({ ...request(), method: "GET" }, binding),
    false,
  );
});

function decode(text, options = {}) {
  const decoder = createDeepSeekEvidenceDecoder({
    model: binding.model,
    ...options,
  });
  // Fragment both SSE framing and UTF-8, never assume network chunk boundaries.
  for (const byte of Buffer.from(text)) decoder.push(Uint8Array.of(byte));
  return decoder.finish();
}

test("maintained SSE decoder retains bounded raw events and requires ready/finish/framed EOF", () => {
  const output = decode(wire);
  assert.equal(output.sse_terminal, true);
  assert.deepEqual(
    output.messages.map((item) => item.event),
    ["ready", "delta", "finish"],
  );
  assert.deepEqual(output.messages[1].data, {
    p: "response",
    o: "APPEND",
    v: "雨水",
  });
  assert.equal(output.raw_answer, undefined);
  assert.equal(output.search_event, undefined);
  assert.equal(output.completion, undefined);
  assert.deepEqual(decode(wire.replace(/\n/gu, "\r\n")), output);
  for (const incomplete of [
    ready + delta,
    ready + event("close", {}),
    finished,
    ready + finished.slice(0, -1),
    wire + "event: delta\ndata: {",
    ready + ready + finished,
    ready + finished + delta,
    ready + finished + finished,
    event("ready", {
      request_message_id: 1,
      response_message_id: 2,
      model_type: "other",
    }) + finished,
    ready + "event: delta\ndata: invalid-json\n\n" + finished,
  ]) {
    assert.throws(() => decode(incomplete));
  }
  assert.throws(() => decode(wire, { maxBytes: 10 }));
  assert.throws(() => decode(wire, { maxEvents: 2 }));
  const decoder = createDeepSeekEvidenceDecoder({ model: binding.model });
  assert.throws(() => decoder.push(Uint8Array.of(255)));
  assert.throws(() => decoder.finish());
});

class Session extends EventEmitter {
  detached = 0;
  commands = [];
  prefix = "";
  calls = [];
  streamError = null;
  recoveredBody = { body: wire, base64Encoded: false };
  async send(method, args) {
    this.commands.push(method);
    this.calls.push({ method, args });
    if (method === "Network.streamResourceContent") {
      if (this.streamError) throw new Error(this.streamError);
      return { bufferedData: Buffer.from(this.prefix).toString("base64") };
    }
    if (method === "Network.getResponseBody") return await this.recoveredBody;
    if (method !== "Network.enable")
      throw new Error("unexpected browser command");
  }
  async detach() {
    this.detached++;
  }
}
function fixture() {
  const session = new Session();
  const page = {
    url: () => `${origin}/`,
    context: () => ({ newCDPSession: async () => session }),
  };
  const start = (patch = {}, responsePatch = {}) => {
    session.emit("Network.requestWillBeSent", {
      requestId: "request-1",
      timestamp: 1,
      request: request(patch),
    });
    session.emit("Network.responseReceived", {
      requestId: "request-1",
      timestamp: 2,
      response: {
        url: endpoint,
        status: 200,
        mimeType: "text/event-stream",
        charset: "utf-8",
        ...responsePatch,
      },
    });
  };
  const bytes = (text) =>
    session.emit("Network.dataReceived", {
      requestId: "request-1",
      data: Buffer.from(text).toString("base64"),
    });
  const end = (patch = {}) =>
    session.emit("Network.loadingFinished", {
      requestId: "request-1",
      timestamp: 3,
      ...patch,
    });
  return { session, page, start, bytes, end };
}

const finishedLoading =
  "Protocol error: Request with the provided ID has already finished loading";

test("completed-body recovery waits for validated EOF and reads exactly the original request once", async () => {
  for (const base64Encoded of [false, true]) {
    const f = fixture();
    f.session.streamError = finishedLoading;
    f.session.recoveredBody = {
      body: base64Encoded ? Buffer.from(wire).toString("base64") : wire,
      base64Encoded,
    };
    let submits = 0;
    const captured = await captureDeepSeekExchange(f.page, {
      binding,
      submit: async () => {
        submits++;
        f.start({}, base64Encoded ? { charset: "" } : {});
        await Promise.resolve();
        assert.equal(
          f.session.commands.includes("Network.getResponseBody"),
          false,
        );
        f.end({ requestId: "unrelated" });
        assert.equal(
          f.session.commands.includes("Network.getResponseBody"),
          false,
        );
        f.end();
      },
    });
    assert.equal(submits, 1);
    assert.deepEqual(captured.messages, decode(wire).messages);
    assert.deepEqual(
      f.session.calls.filter(
        (call) => call.method === "Network.getResponseBody",
      ),
      [{ method: "Network.getResponseBody", args: { requestId: "request-1" } }],
    );
    assert.equal(f.session.detached, 1);
  }
});

test("recovery rejects truncated, oversized, lossy or invalid bodies and non-terminal failures", async () => {
  for (const scenario of [
    "truncated",
    "oversized",
    "invalid-base64",
    "invalid-utf8",
    "lossy",
    "surrogate",
    "wrong-model",
    "wrong-type",
    "unknown-charset",
    "other-charset",
    "bad-time",
    "failed",
    "other-error",
    "missing-eof",
  ]) {
    const f = fixture();
    const controller = new AbortController();
    f.session.streamError =
      scenario === "other-error" ? "Method not found" : finishedLoading;
    const replacements = {
      truncated: { body: ready + delta, base64Encoded: false },
      oversized: { body: wire + "x".repeat(1000), base64Encoded: false },
      "invalid-base64": { body: "?!", base64Encoded: true },
      "invalid-utf8": {
        body: Buffer.from([255]).toString("base64"),
        base64Encoded: true,
      },
      lossy: { body: wire.replace("雨水", "\uFFFD"), base64Encoded: false },
      surrogate: { body: wire.replace("雨水", "\uD800"), base64Encoded: false },
      "wrong-model": {
        body: wire.replace("observed-model", "wrong-model"),
        base64Encoded: false,
      },
      "wrong-type": { body: wire, base64Encoded: "false" },
    };
    if (replacements[scenario])
      f.session.recoveredBody = replacements[scenario];
    const captured = await captureDeepSeekExchange(f.page, {
      binding,
      signal: controller.signal,
      maxBytes: 1000,
      submit: async () => {
        f.start(
          {},
          scenario === "unknown-charset"
            ? { charset: "" }
            : scenario === "other-charset"
              ? { charset: "utf-16le" }
              : {},
        );
        if (scenario === "failed") {
          f.session.emit("Network.loadingFailed", { requestId: "request-1" });
        } else if (scenario === "missing-eof") {
          await Promise.resolve();
          controller.abort();
        } else {
          f.end(scenario === "bad-time" ? { timestamp: 0 } : {});
        }
      },
    });
    assert.equal(captured, null, scenario);
    if (["bad-time", "failed", "other-error", "missing-eof"].includes(scenario))
      assert.equal(
        f.session.commands.includes("Network.getResponseBody"),
        false,
        scenario,
      );
    assert.equal(f.session.detached, 1);
  }
});

test("cancellation or a competing submission invalidates an in-flight body recovery", async () => {
  for (const scenario of ["cancel", "duplicate", "timeout", "body-error"]) {
    const f = fixture();
    const controller = new AbortController();
    f.session.streamError = finishedLoading;
    let release;
    f.session.recoveredBody = new Promise((resolve) => (release = resolve));
    const send = f.session.send.bind(f.session);
    f.session.send = async (method, args) => {
      const pending = send(method, args);
      if (method === "Network.getResponseBody") {
        if (scenario === "cancel") controller.abort();
        if (scenario === "duplicate")
          f.session.emit("Network.requestWillBeSent", {
            requestId: "second",
            timestamp: 4,
            request: request(),
          });
        if (scenario === "body-error") {
          release({ body: wire, base64Encoded: false });
          throw new Error("resource evicted from bounded inspector cache");
        }
      }
      return pending;
    };
    assert.equal(
      await captureDeepSeekExchange(f.page, {
        binding,
        signal: controller.signal,
        timeoutMs: 20,
        submit: () => {
          f.start();
          f.end();
        },
      }),
      null,
      scenario,
    );
    release({ body: wire, base64Encoded: false });
    await Promise.resolve();
    assert.equal(
      f.session.commands.filter(
        (method) => method === "Network.getResponseBody",
      ).length,
      1,
    );
    assert.equal(f.session.detached, 1);
    assert.equal(f.session.eventNames().length, 0);
  }
});

test("capture binds browser request, orders buffered bytes before live chunks and submits once", async () => {
  const f = fixture();
  f.session.prefix = ready;
  let submitted = 0;
  const result = await captureDeepSeekExchange(f.page, {
    binding,
    submit: async () => {
      submitted++;
      f.start();
      // Data and EOF arrive while streamResourceContent is still resolving.
      f.bytes(delta + finished);
      f.end();
    },
  });
  assert.equal(submitted, 1);
  assert.equal(result.messages.length, 3);
  assert.equal(result.sse_terminal, true);
  assert.equal(f.session.detached, 1);
  assert.equal(f.session.eventNames().length, 0);
  assert.deepEqual(f.session.commands, [
    "Network.enable",
    "Network.streamResourceContent",
  ]);
  assert.equal(result.request, undefined);
  assert.equal(result.headers, undefined);
});

test("capture refuses wrong bindings, duplicate requests, redirects, truncated or failed streams", async () => {
  for (const scenario of [
    "wrong",
    "duplicate",
    "redirect",
    "truncated",
    "failed",
    "overflow",
  ]) {
    const f = fixture();
    const result = await captureDeepSeekExchange(f.page, {
      binding,
      maxBytes: scenario === "overflow" ? 10 : 750_000,
      submit: async () => {
        f.start(scenario === "wrong" ? { prompt: "other" } : {});
        if (scenario === "duplicate" || scenario === "redirect") {
          f.session.emit("Network.requestWillBeSent", {
            requestId: "request-2",
            request: request(),
            ...(scenario === "redirect" ? { redirectResponse: {} } : {}),
          });
        }
        f.bytes(scenario === "truncated" ? ready + delta : wire);
        if (scenario === "failed")
          f.session.emit("Network.loadingFailed", { requestId: "request-1" });
        else f.end();
      },
    });
    assert.equal(result, null, scenario);
    assert.equal(f.session.detached, 1);
    assert.equal(f.session.eventNames().length, 0);
  }
});

test("cancellation and deadline settle even when UI submission never resolves", async () => {
  for (const cancelled of [true, false]) {
    const f = fixture();
    const controller = new AbortController();
    let submitted = 0;
    const result = await captureDeepSeekExchange(f.page, {
      binding,
      signal: controller.signal,
      timeoutMs: 10,
      submit: () => {
        submitted++;
        if (cancelled) controller.abort();
        return new Promise(() => {});
      },
    });
    assert.equal(result, null);
    assert.equal(submitted, 1);
    assert.equal(f.session.detached, 1);
    assert.equal(f.session.eventNames().length, 0);
  }
  const f = fixture();
  const controller = new AbortController();
  controller.abort();
  assert.equal(
    await captureDeepSeekExchange(f.page, {
      binding,
      signal: controller.signal,
      submit: () => assert.fail("cancelled capture cannot submit"),
    }),
    null,
  );
  assert.equal(f.session.commands.length, 0);
});
