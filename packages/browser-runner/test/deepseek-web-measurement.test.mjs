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

test("real Chromium submits once and fails closed if an immediate response outruns CDP attachment", async () => {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  let posts = 0;
  try {
    const context = await browser.newContext();
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
        await route.fulfill({ contentType: "text/event-stream", body: wire });
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
    // route.fulfill may finish before Network.streamResourceContent can attach.
    // Either an exact complete capture or null is valid; never partial success
    // and never a second submission to obtain another response.
    if (captured !== null) {
      assert.equal(captured.sse_terminal, true);
      assert.deepEqual(captured.messages[1].data, {
        p: "response",
        o: "APPEND",
        v: "雨水",
      });
    }
  } finally {
    await browser.close();
  }
});

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
  async send(method) {
    this.commands.push(method);
    if (method === "Network.streamResourceContent")
      return { bufferedData: Buffer.from(this.prefix).toString("base64") };
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
  const start = (patch = {}) => {
    session.emit("Network.requestWillBeSent", {
      requestId: "request-1",
      request: request(patch),
    });
    session.emit("Network.responseReceived", {
      requestId: "request-1",
      response: { url: endpoint, status: 200, mimeType: "text/event-stream" },
    });
  };
  const bytes = (text) =>
    session.emit("Network.dataReceived", {
      requestId: "request-1",
      data: Buffer.from(text).toString("base64"),
    });
  const end = () =>
    session.emit("Network.loadingFinished", { requestId: "request-1" });
  return { session, page, start, bytes, end };
}

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
