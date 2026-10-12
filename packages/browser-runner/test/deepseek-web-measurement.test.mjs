import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import { observeDeepSeekCreatedSessions } from "../src/deepseek-created-sessions.mjs";
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
const { chatSessionId: _sessionId, ...unresolvedBinding } = binding;

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
      on: (...args) => page.on(...args),
      off: (...args) => page.off(...args),
      mainFrame: () => page.mainFrame(),
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
              if (method === "Page.getFrameTree") {
                const frame = result.frameTree.frame;
                assert.equal(new URL(frame.url).origin, localOrigin);
                return {
                  ...result,
                  frameTree: {
                    ...result.frameTree,
                    frame: {
                      ...frame,
                      url: `${origin}${new URL(frame.url).pathname}`,
                    },
                  },
                };
              }
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

test("real Chromium correlates a prefetched exact create ID instead of the latest session", async () => {
  await completedBrowserRecovery(true, true);
});

async function completedBrowserRecovery(declaredUtf8, freshSession = false) {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  let posts = 0;
  let creates = 0;
  let createdSessions;
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
            ${
              freshSession
                ? `Promise.all([0,1].map(() => fetch("/api/v0/chat_session/create",
                  {method:"POST",body:"{}"}).then(r => r.json())))
                  .then(results => window.prefetchedId = results[0].data.biz_data.chat_session.id);`
                : ""
            }
            document.getElementById("send").onclick = () => fetch(
              ${JSON.stringify(endpoint)},
              {method:"POST",headers:{"content-type":"application/json"},
               body:JSON.stringify({...${JSON.stringify(body)},
                 chat_session_id:${freshSession ? "window.prefetchedId" : JSON.stringify(binding.chatSessionId)}})}
            ).then(response => response.text());
          </script>`,
        });
      } else if (
        freshSession &&
        route.request().url() === `${origin}/api/v0/chat_session/create`
      ) {
        creates++;
        await route.fulfill({
          contentType: "application/json",
          body: JSON.stringify({
            code: 0,
            data: {
              biz_code: 0,
              biz_data: {
                chat_session: {
                  id: creates === 1 ? binding.chatSessionId : "other-prefetch",
                },
              },
            },
          }),
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
    if (freshSession)
      createdSessions = observeDeepSeekCreatedSessions(page, {
        timeoutMs: 5000,
      });
    await page.goto(`${origin}/`);
    if (freshSession) await page.waitForFunction(() => window.prefetchedId);
    const captured = await captureDeepSeekExchange(page, {
      binding: freshSession ? unresolvedBinding : binding,
      createdSessions,
      submit: () => page.locator("#send").click(),
      timeoutMs: 5000,
    });
    assert.equal(posts, 1);
    if (freshSession) {
      assert.equal(creates, 2);
      assert.deepEqual(captured?.created_session, {
        chatSessionId: binding.chatSessionId,
        source: "browser_observed_create",
      });
    }
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
    assert.deepEqual(
      commands.find((item) => item.method === "Network.enable"),
      {
        method: "Network.enable",
        args: {
          maxTotalBufferSize: 750_000,
          maxResourceBufferSize: 750_000,
          maxPostDataSize: 64_000,
        },
      },
    );
  } finally {
    createdSessions?.dispose();
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

test("finish payload is opaque and unnamed deltas obey ready/finish ordering", () => {
  const unnamed = 'data: {"p":"response","o":"SET","v":{"synthetic":true}}\n\n';
  for (const payload of ["", "not-json", '{"reason":"synthetic"}']) {
    const terminal = `event: finish\ndata: ${payload}\n\n`;
    const output = decode(ready + unnamed + terminal);
    assert.equal(output.sse_terminal, true);
    assert.deepEqual(output.messages.at(-1), {
      event: "finish",
      raw_data: payload,
    });
    assert.equal(output.completion, undefined);
    assert.equal(output.raw_answer, undefined);
    assert.throws(() => decode(terminal));
    assert.throws(() => decode(unnamed + ready + terminal));
    assert.throws(() => decode(ready + terminal + unnamed));
    assert.throws(() => decode(ready + terminal + terminal));
  }
});

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
  frame = { id: "main", loaderId: "document-1", url: `${origin}/` };
  detached = 0;
  commands = [];
  prefix = "";
  calls = [];
  streamError = null;
  recoveredBody = { body: wire, base64Encoded: false };
  async send(method, args) {
    this.commands.push(method);
    this.calls.push({ method, args });
    if (method === "Page.enable") return {};
    if (method === "Page.getFrameTree")
      return { frameTree: { frame: { ...this.frame } } };
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
  const page = Object.assign(new EventEmitter(), {
    url: () => `${origin}/`,
    mainFrame: () => page,
    context: () => ({ newCDPSession: async () => session }),
  });
  const start = (patch = {}, responsePatch = {}) => {
    session.emit("Network.requestWillBeSent", {
      requestId: "request-1",
      frameId: "main",
      loaderId: "document-1",
      timestamp: 1,
      request: request(patch),
    });
    session.emit("Network.responseReceived", {
      requestId: "request-1",
      frameId: "main",
      loaderId: "document-1",
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

test("decoder snapshots retain only validated bounded events and cannot mutate state", () => {
  const decoder = createDeepSeekEvidenceDecoder({ model: binding.model });
  decoder.push(Buffer.from(ready + delta));
  const snapshot = decoder.snapshot();
  assert.equal(snapshot.sse_terminal, false);
  snapshot.messages[0].data.request_message_id = 999;
  snapshot.messages.pop();
  assert.equal(decoder.snapshot().messages.length, 2);
  assert.equal(decoder.snapshot().messages[0].data.request_message_id, 1);
  assert.throws(() => decoder.push(Buffer.from("data: {broken}\n\n")));
  assert.equal(decoder.snapshot().messages.length, 2);
  assert.throws(() => decoder.finish());
  const limited = createDeepSeekEvidenceDecoder({
    model: binding.model,
    maxEvents: 1,
  });
  assert.throws(() => limited.push(Buffer.from(ready + delta)));
  assert.equal(limited.snapshot().messages.length, 1);
});

test("capture awaits exactly one correlated prefix checkpoint on EOF, malformed, timeout and limits", async () => {
  for (const termination of ["eof", "malformed", "timeout", "limit_exceeded"]) {
    const f = fixture();
    f.session.prefix =
      ready + (termination === "malformed" ? "data: {broken}\n\n" : delta);
    let callbacks = 0;
    let submits = 0;
    let checkpointDone = false;
    const result = await captureDeepSeekExchange(f.page, {
      binding,
      timeoutMs: 40,
      ...(termination === "limit_exceeded" ? { maxEvents: 1 } : {}),
      onPartialCapture: async (prefix) => {
        callbacks++;
        assert.equal(prefix.capture_status.transport, "partial");
        assert.equal(prefix.capture_status.termination, termination);
        assert.equal(prefix.sse_terminal, false);
        assert.ok(prefix.messages.length > 0);
        f.end();
        await new Promise((resolve) => setImmediate(resolve));
        checkpointDone = true;
      },
      submit: () => {
        submits++;
        f.start();
        if (termination !== "timeout") f.end();
      },
    });
    assert.equal(result, null);
    assert.equal(callbacks, 1);
    assert.equal(submits, 1);
    assert.equal(checkpointDone, true);
  }
});

test("missing creation proof keeps the same stream through EOF and checkpoints complete ownerless transport", async () => {
  const f = fixture();
  f.session.prefix = ready;
  const captures = [];
  let submits = 0;
  const result = await captureDeepSeekExchange(f.page, {
    binding,
    createdSessions: {
      lookup: async () => null,
      status: () => "observing",
      isCurrent: () => false,
    },
    onPartialCapture: async (prefix) => captures.push(prefix),
    submit: async () => {
      submits++;
      f.start();
      await new Promise((resolve) => setImmediate(resolve));
      f.bytes(delta + finished);
      f.end();
    },
  });
  assert.equal(result, null);
  assert.equal(submits, 1);
  assert.equal(captures.length, 1);
  assert.equal(captures[0].capture_status.transport, "complete");
  assert.equal(captures[0].capture_status.termination, "complete");
  assert.equal(captures[0].created_session, undefined);
  assert.equal(captures[0].messages.length, 3);
});

test("uncorrelated responses never checkpoint and failed checkpoints propagate", async () => {
  const f = fixture();
  f.session.prefix = ready;
  await captureDeepSeekExchange(f.page, {
    binding,
    onPartialCapture: () => assert.fail("unmatched response"),
    submit: () => f.start({ prompt: "unrelated" }),
  });
  const g = fixture();
  g.session.prefix = ready;
  await assert.rejects(
    captureDeepSeekExchange(g.page, {
      binding,
      onPartialCapture: async () => {
        throw new Error("private failure");
      },
      submit: () => {
        g.start();
        g.end();
      },
    }),
    { message: "evidence_persist_failed" },
  );
  const h = fixture();
  h.session.prefix = ready;
  await assert.rejects(
    captureDeepSeekExchange(h.page, {
      binding,
      checkpointTimeoutMs: 10,
      onPartialCapture: () => new Promise(() => {}),
      submit: () => {
        h.start();
        h.end();
      },
    }),
    { message: "evidence_persist_failed" },
  );
  assert.equal(h.session.detached, 1);
});

test("failed captures report only fixed stages without exception or request data", async () => {
  for (const failure of [
    "setup",
    "submission",
    "request",
    "creation",
    "response",
    "stream",
    "eof",
  ]) {
    const f = fixture();
    const diagnostics = [];
    let submits = 0;
    if (failure === "setup")
      f.page.context = () => {
        throw new Error("private-token");
      };
    if (failure === "stream") f.session.streamError = "private-token";
    if (failure === "eof") f.session.prefix = "not an SSE stream";
    else f.session.prefix = wire;
    const captured = await captureDeepSeekExchange(f.page, {
      binding,
      timeoutMs: 1000,
      onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
      ...(failure === "creation"
        ? {
            createdSessions: {
              lookup: async () => null,
              isCurrent: () => false,
              status: () => "observing",
            },
          }
        : {}),
      submit: async () => {
        submits++;
        if (failure === "submission") throw new Error("private-token");
        f.start(
          failure === "request" ? { prompt: "private-token" } : {},
          failure === "response" ? { status: 403 } : {},
        );
        f.end();
      },
    });
    assert.equal(captured, null);
    assert.equal(submits, failure === "setup" ? 0 : 1);
    assert.deepEqual(diagnostics, [
      {
        kind: "observation_diagnostic",
        schema_version: "geo.observation.diagnostic.v1",
        stage: `capture_${failure}`,
        code: "capture_unverified",
      },
    ]);
    assert.equal(JSON.stringify(diagnostics).includes("private-token"), false);
  }
});

function createdFixture(f, options = {}) {
  const observer = observeDeepSeekCreatedSessions(f.page, options);
  const create = (id) => {
    const request = {
      frame: () => f.page,
      isNavigationRequest: () => false,
      url: () => `${origin}/api/v0/chat_session/create`,
      method: () => "POST",
      redirectedFrom: () => null,
      postData: () => "{}",
    };
    let release;
    const body = new Promise((resolve) => (release = resolve));
    f.page.emit("request", request);
    f.page.emit("response", {
      request: () => request,
      url: request.url,
      status: () => 200,
      headerValue: async () => null,
      body: () => body,
    });
    return () =>
      release(
        Buffer.from(
          JSON.stringify({
            code: 0,
            data: {
              biz_code: 0,
              biz_data: { chat_session: { id } },
            },
          }),
        ),
      );
  };
  return { observer, create };
}

test("exact fresh-session lookup waits across live streaming or recovered EOF without resubmission", async () => {
  for (const recover of [false, true]) {
    const f = fixture();
    const { observer, create } = createdFixture(f);
    const releaseSelected = create(binding.chatSessionId);
    const releaseOther = create("newest-unrelated");
    releaseOther();
    if (recover) f.session.streamError = finishedLoading;
    let submits = 0;
    let completed = false;
    try {
      const capture = captureDeepSeekExchange(f.page, {
        binding: unresolvedBinding,
        createdSessions: observer,
        submit: async () => {
          submits++;
          f.start();
          if (!recover) f.bytes(wire);
          f.end();
          await new Promise(setImmediate);
          assert.equal(completed, false, "EOF cannot bypass pending proof");
          assert.ok(
            f.session.commands.includes("Network.streamResourceContent"),
          );
          if (recover)
            assert.ok(f.session.commands.includes("Network.getResponseBody"));
          releaseSelected();
        },
      }).then((result) => {
        completed = true;
        return result;
      });
      const captured = await capture;
      assert.equal(submits, 1);
      assert.deepEqual(captured?.messages, decode(wire).messages);
      assert.equal(
        captured.created_session,
        await observer.lookup(binding.chatSessionId),
      );
      assert.equal(observer.status(), "observing", "caller owns observer");
      assert.equal(f.session.detached, 1);
    } finally {
      observer.dispose();
    }
  }
});

test("fresh-session correlation retains every ordinary-submission and document check", async () => {
  for (const patch of [
    { prompt: "other" },
    { model_type: "other" },
    { search_enabled: false },
    { thinking_enabled: true },
    { parent_message_id: 1 },
    { ref_file_ids: ["file"] },
    { action: "regenerate" },
    { child_message_id: 2 },
    { message_id: 1 },
    { chat_session_id: "unobserved" },
    { chat_session_id: null },
    { chat_session_id: "invalid/id" },
  ]) {
    const f = fixture();
    const { observer, create } = createdFixture(f);
    create(binding.chatSessionId)();
    try {
      assert.equal(
        await captureDeepSeekExchange(f.page, {
          binding: unresolvedBinding,
          createdSessions: observer,
          submit: () => {
            f.start(patch);
            f.bytes(wire);
            f.end();
          },
        }),
        null,
        JSON.stringify(patch),
      );
    } finally {
      observer.dispose();
    }
  }
  for (const field of ["frameId", "loaderId"]) {
    const f = fixture();
    const { observer, create } = createdFixture(f);
    create(binding.chatSessionId)();
    try {
      assert.equal(
        await captureDeepSeekExchange(f.page, {
          binding: unresolvedBinding,
          createdSessions: observer,
          submit: () =>
            f.session.emit("Network.requestWillBeSent", {
              requestId: "request-1",
              frameId: "main",
              loaderId: "document-1",
              timestamp: 1,
              request: request(),
              [field]: "different",
            }),
        }),
        null,
      );
    } finally {
      observer.dispose();
    }
  }
});

test("pending fresh-session proof cannot outlive cancellation, navigation, duplicates or deadlines", async () => {
  for (const scenario of [
    "cancel",
    "navigate",
    "duplicate",
    "ambiguous",
    "observer-disposed",
    "observer-timeout",
    "capture-timeout",
  ]) {
    const f = fixture();
    const { observer, create } = createdFixture(f, {
      timeoutMs: scenario === "observer-timeout" ? 20 : 1000,
    });
    const release = create(binding.chatSessionId);
    const controller = new AbortController();
    let submits = 0;
    try {
      const result = await captureDeepSeekExchange(f.page, {
        binding: unresolvedBinding,
        createdSessions: observer,
        signal: controller.signal,
        timeoutMs: scenario === "capture-timeout" ? 20 : 1000,
        submit: () => {
          submits++;
          f.start();
          f.bytes(wire);
          f.end();
          if (scenario === "cancel") controller.abort();
          if (scenario === "navigate")
            f.session.emit("Page.frameNavigated", {
              frame: { ...f.session.frame, loaderId: "replacement" },
            });
          if (scenario === "duplicate")
            f.session.emit("Network.requestWillBeSent", {
              requestId: "second",
              request: request(),
            });
          if (scenario === "ambiguous") {
            create(binding.chatSessionId)();
            release();
          }
          if (scenario === "observer-disposed") observer.dispose();
        },
      });
      assert.equal(result, null, scenario);
      assert.equal(submits, 1, scenario);
      release();
      await new Promise(setImmediate);
      assert.equal(f.session.eventNames().length, 0, scenario);
      assert.equal(f.session.detached, 1, scenario);
    } finally {
      observer.dispose();
    }
  }
});

test("proof is checked again after EOF and invalidation cannot be replaced with a new proof", async () => {
  for (const scenario of ["duplicate-create", "navigation", "disposed"]) {
    const f = fixture();
    const { observer, create } = createdFixture(f);
    create(binding.chatSessionId)();
    let firstProof;
    const lookup = observer.lookup;
    let lookups = 0;
    observer.lookup = async (id) => {
      const proof = await lookup(id);
      if (++lookups === 1) firstProof = proof;
      return proof;
    };
    try {
      const result = await captureDeepSeekExchange(f.page, {
        binding: unresolvedBinding,
        createdSessions: observer,
        submit: async () => {
          f.start();
          f.bytes(wire);
          await new Promise(setImmediate);
          assert.ok(firstProof);
          if (scenario === "duplicate-create") create(binding.chatSessionId)();
          if (scenario === "navigation") {
            // Observer invalidates on navigation request, before CDP commit.
            f.page.emit("request", {
              frame: () => f.page,
              isNavigationRequest: () => true,
            });
            f.page.emit("framenavigated", f.page);
            create(binding.chatSessionId)();
          }
          if (scenario === "disposed") observer.dispose();
          f.end();
        },
      });
      assert.equal(result, null, scenario);
      assert.equal(lookups, 2, scenario);
    } finally {
      observer.dispose();
    }
  }
});

test("navigation after final lookup resolves but before capture resumes invalidates success", async () => {
  const f = fixture();
  const { observer, create } = createdFixture(f);
  create(binding.chatSessionId)();
  const unrelated = {
    frame: () => f.page,
    isNavigationRequest: () => false,
    url: () => `${origin}/api/v0/chat_session/create`,
    method: () => "POST",
    redirectedFrom: () => null,
    postData: () => "{}",
  };
  let lookups = 0;
  let submits = 0;
  const lookup = observer.lookup;
  observer.lookup = (id) => {
    const result = lookup(id);
    if (++lookups === 2) {
      // Resolves the final lookup with the old proof, then clears that proof
      // synchronously before capture's await continuation. No commit is needed
      // yet: the observer already knows a navigation request has begun.
      f.page.emit("requestfailed", unrelated);
      f.page.emit("request", {
        frame: () => f.page,
        isNavigationRequest: () => true,
      });
    }
    return result;
  };
  try {
    const captured = await captureDeepSeekExchange(f.page, {
      binding: unresolvedBinding,
      createdSessions: observer,
      submit: async () => {
        submits++;
        f.start();
        f.bytes(wire);
        await new Promise(setImmediate);
        f.page.emit("request", unrelated);
        f.end();
      },
    });
    assert.equal(captured, null);
    assert.equal(lookups, 2);
    assert.equal(submits, 1);
    assert.equal(observer.status(), "observing");
    assert.equal(f.session.detached, 1);
  } finally {
    observer.dispose();
  }
});

test("unresolved or inactive resolver options cannot submit and resolver errors fail closed", async () => {
  for (const createdSessions of [
    undefined,
    null,
    {},
    { lookup() {}, status: () => "observing" },
    { lookup() {}, isCurrent: () => false, status: () => "disposed" },
  ]) {
    const f = fixture();
    assert.equal(
      await captureDeepSeekExchange(f.page, {
        binding: unresolvedBinding,
        createdSessions,
        submit: () => assert.fail("invalid options cannot submit"),
      }),
      null,
    );
  }
  for (const lookup of [
    () => Promise.reject(new Error("synthetic resolver failure")),
    async () => ({ chatSessionId: "wrong", source: "browser_observed_create" }),
    async () => ({ chatSessionId: binding.chatSessionId, source: "guessed" }),
  ]) {
    const f = fixture();
    assert.equal(
      await captureDeepSeekExchange(f.page, {
        binding: unresolvedBinding,
        createdSessions: {
          lookup,
          isCurrent: () => true,
          status: () => "observing",
        },
        submit: () => {
          f.start();
          f.bytes(wire);
          f.end();
        },
      }),
      null,
    );
  }
});

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
    "Page.enable",
    "Page.getFrameTree",
    "Network.enable",
    "Page.getFrameTree",
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

test("capture rejects subframe, replaced-document and redirected requests", async () => {
  for (const scenario of [
    "subframe",
    "loader",
    "replacement",
    "redirect-away",
  ]) {
    const f = fixture();
    let submits = 0;
    const result = await captureDeepSeekExchange(f.page, {
      binding,
      submit: () => {
        submits++;
        if (scenario === "replacement")
          f.session.emit("Page.frameNavigated", {
            frame: { ...f.session.frame, loaderId: "replacement" },
          });
        f.session.emit("Network.requestWillBeSent", {
          requestId: "request-1",
          timestamp: 1,
          frameId: scenario === "subframe" ? "child" : "main",
          loaderId: scenario === "loader" ? "other" : "document-1",
          request: request(),
        });
        if (scenario === "redirect-away")
          f.session.emit("Network.requestWillBeSent", {
            requestId: "request-1",
            timestamp: 2,
            request: { ...request(), url: `${origin}/redirected` },
            redirectResponse: { status: 302 },
          });
      },
      timeoutMs: 100,
    });
    assert.equal(result, null, scenario);
    assert.equal(submits, 1);
  }
});

test("document is revalidated after Network.enable without rejecting same-document paths", async () => {
  for (const scenario of [
    "commit",
    "silent-loader",
    "origin",
    "same-document",
  ]) {
    const f = fixture();
    let submits = 0;
    const send = f.session.send.bind(f.session);
    f.session.send = async (method, args) => {
      const result = await send(method, args);
      if (method === "Network.enable") {
        if (scenario === "commit")
          f.session.emit("Page.frameNavigated", {
            frame: { ...f.session.frame, loaderId: "new" },
          });
        if (scenario === "silent-loader") f.session.frame.loaderId = "new";
        if (scenario === "origin") f.page.url = () => "https://example.org/";
        if (scenario === "same-document") {
          f.session.frame.url = `${origin}/a/new-conversation`;
          f.session.emit("Page.navigatedWithinDocument", {
            frameId: "main",
            url: f.session.frame.url,
          });
        }
      }
      return result;
    };
    const result = await captureDeepSeekExchange(f.page, {
      binding,
      submit: () => {
        submits++;
        f.start();
        f.bytes(wire);
        f.end();
      },
    });
    assert.equal(
      result?.sse_terminal ?? false,
      scenario === "same-document",
      scenario,
    );
    assert.equal(submits, scenario === "same-document" ? 1 : 0, scenario);
  }
});

test("deadline and cancellation bound CDP opening and every setup command", async () => {
  for (const stage of [
    "open",
    "Page.enable",
    "Page.getFrameTree",
    "Network.enable",
  ]) {
    for (const cancel of [false, true]) {
      const f = fixture();
      const controller = new AbortController();
      const hang = () => {
        if (cancel) controller.abort();
        return new Promise(() => {});
      };
      if (stage === "open") f.page.context = () => ({ newCDPSession: hang });
      else {
        const send = f.session.send.bind(f.session);
        f.session.send = (method, args) =>
          method === stage ? hang() : send(method, args);
      }
      assert.equal(
        await captureDeepSeekExchange(f.page, {
          binding,
          signal: controller.signal,
          timeoutMs: 10,
          submit: () => assert.fail("unverified document cannot submit"),
        }),
        null,
        `${stage}/${cancel}`,
      );
    }
  }
});

test("main-frame navigation during CDP opening cannot silently rebind the document", async () => {
  const f = fixture();
  f.page.context = () => ({
    newCDPSession: async () => {
      f.page.emit("framenavigated", f.page);
      return f.session;
    },
  });
  assert.equal(
    await captureDeepSeekExchange(f.page, {
      binding,
      submit: () => assert.fail("changed document cannot submit"),
    }),
    null,
  );
  assert.equal(f.session.detached, 1);
  assert.equal(f.page.listenerCount("framenavigated"), 0);
});

test("real Chromium rejects subframe requests and document replacement but accepts SPA navigation", async () => {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  try {
    for (const scenario of [
      "subframe",
      "network-navigation",
      "replacement",
      "same-document",
    ]) {
      const context = await browser.newContext();
      let posts = 0;
      let observePost;
      const postObserved = new Promise((resolve) => (observePost = resolve));
      await context.route("**/*", async (route) => {
        if (route.request().url() === endpoint) {
          posts++;
          observePost();
          await route.fulfill({
            contentType: "text/event-stream; charset=utf-8",
            body: wire,
          });
        } else {
          await route.fulfill({
            contentType: "text/html",
            body: `<button id="send">Send fixture</button><script>
            document.getElementById("send").onclick=()=>{
              ${scenario === "same-document" ? 'history.pushState({}, "", "/a/created");' : ""}
              fetch("/api/v0/chat/completion",{method:"POST",headers:{"content-type":"application/json"},body:${JSON.stringify(JSON.stringify(body))}});
            };</script>`,
          });
        }
      });
      const page = await context.newPage();
      await page.goto(`${origin}/`);
      if (scenario === "subframe") {
        await page.evaluate(() => {
          const frame = document.createElement("iframe");
          frame.src = "/child";
          document.body.append(frame);
        });
        await page.frameLocator("iframe").locator("#send").waitFor();
      }
      if (scenario === "network-navigation") {
        const open = context.newCDPSession.bind(context);
        context.newCDPSession = async (target) => {
          const session = await open(target);
          const send = session.send.bind(session);
          session.send = async (method, args) => {
            const result = await send(method, args);
            if (method === "Network.enable")
              await page.goto(`${origin}/replacement`);
            return result;
          };
          return session;
        };
      }
      let submits = 0;
      const result = await captureDeepSeekExchange(page, {
        binding,
        timeoutMs: 3000,
        submit: async () => {
          submits++;
          if (scenario === "replacement")
            await page.goto(`${origin}/replacement`);
          else if (scenario === "subframe")
            await page.frameLocator("iframe").locator("#send").click();
          else await page.locator("#send").click();
        },
      });
      assert.equal(
        result?.sse_terminal ?? false,
        scenario === "same-document",
        scenario,
      );
      assert.equal(
        submits,
        scenario === "network-navigation" ? 0 : 1,
        scenario,
      );
      if (["subframe", "same-document"].includes(scenario)) await postObserved;
      assert.equal(
        posts,
        ["subframe", "same-document"].includes(scenario) ? 1 : 0,
        scenario,
      );
      await context.close();
    }
  } finally {
    await browser.close();
  }
});
