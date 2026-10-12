import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { setImmediate } from "node:timers/promises";
import { test } from "node:test";
import { measureKimi } from "../src/adapters.mjs";
import { createRunner } from "../src/runner.mjs";

// Synthetic browser/CDP and persistence transports, with production capture,
// interpretation, grounding, adapter and runner lifecycle code left intact.
// A callback acknowledgement models the durable service boundary, not a DB test.
const MODEL = "synthetic-model";
const QUESTION = "What does the synthetic source say?";
const ORIGIN = "https://www.kimi.com";
const ENDPOINT = `${ORIGIN}/apiv2/kimi.gateway.chat.v1.ChatService/Chat`;
const SOURCE_ID = "33333333-3333-4333-8333-333333333333";
const payload = {
  target_id: "11111111-1111-4111-8111-111111111111",
  account_id: "22222222-2222-4222-8222-222222222222",
  provider: "kimi",
  model: MODEL,
  surface: "consumer_web",
  search_mode: "web_search",
  protocol_version: "synthetic-v1",
  question_set_version: "synthetic-v1",
  question: QUESTION,
  market: "test-market",
  language: "en",
  scheduled_at: "2026-01-01T00:00:00Z",
  sample_ordinal: 0,
};
const messages = [
  { chat: { id: "synthetic-chat" } },
  { state: "completed" },
  { search: { count: 1 } },
  { text: "A synthetic source-backed answer." },
  { url: "https://example.org/article", reference: "synthetic-reference" },
];
const candidate = {
  completion: "complete",
  completion_evidence: [{ path: "/messages/1/state" }],
  search_used: "yes",
  search_evidence: [{ path: "/messages/2/search" }],
  answer_segments: [{ path: "/messages/3/text" }],
  citations: [
    {
      url: { path: "/messages/4/url" },
      usage: { path: "/messages/4/reference" },
    },
  ],
};
const extraction = {
  text: JSON.stringify(candidate),
  model: "synthetic-parser",
  config_version: 2,
};
const frame = (flags, value) => {
  const data = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(5);
  header[0] = flags;
  header.writeUInt32BE(data.length, 1);
  return Buffer.concat([header, data]);
};
const wireBytes = Buffer.concat([
  ...messages.map((value) => frame(0, value)),
  frame(2, {}),
]);
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => (resolve = done));
  return { promise, resolve };
};
const flush = () => setImmediate();

async function pipeline(
  t,
  { preferBrowser = false, holdSource, holdCandidate } = {},
) {
  let now = 0;
  t.mock.timers.enable({ apis: ["setTimeout"] });
  t.mock.method(performance, "now", () => now);
  const events = [];
  const stored = [];
  const apiResult = deferred();
  let sends = 0;
  let apiSignal;
  let apiStartedAt;
  let network;
  const page = new EventEmitter();
  const cdp = new EventEmitter();
  cdp.send = async (method) =>
    method === "Network.streamResourceContent"
      ? { bufferedData: wireBytes.toString("base64") }
      : {};
  cdp.detach = async () => {};
  page.goto = async () => {};
  page.url = () => `${ORIGIN}/`;
  page.keyboard = { press: async () => {} };
  page.getByTestId = (name) =>
    name === "model-option"
      ? {
          first: () => ({ waitFor: async () => {} }),
          all: async () => [
            { getAttribute: async () => MODEL, click: async () => {} },
          ],
        }
      : { click: async () => {} };
  page.getByRole = () => ({
    click: async () => {},
    getAttribute: async () => "true",
  });
  page.locator = (selector) => {
    if (selector === ".send-button-container")
      return {
        getAttribute: async () => "send-button-container",
        click: async () => {
          sends += 1;
          events.push("question");
          cdp.emit("Network.responseReceived", {
            requestId: "synthetic-request",
            response: { url: ENDPOINT },
          });
          page.emit("response", {
            url: () => ENDPOINT,
            status: () => 200,
            headers: () => ({ "content-type": "application/connect+json" }),
            request: () => ({
              method: () => "POST",
              headers: () => ({ "content-type": "application/connect+json" }),
              postDataBuffer: () =>
                frame(0, {
                  chat_id: "",
                  options: { model: MODEL },
                  message: {
                    role: "user",
                    blocks: [{ text: { content: QUESTION } }],
                  },
                  tools: [{ type: "SEARCH", search: {} }],
                }),
            }),
          });
          cdp.emit("Network.loadingFinished", {
            requestId: "synthetic-request",
          });
        },
      };
    return {
      isVisible: async () => true,
      fill: async (text) => assert.equal(text, QUESTION),
      count: async () => 0,
    };
  };
  // The real browser extraction route starts, then its navigation hangs.
  // Its production bounded route must release the configured API fallback.
  page.context = () => ({
    newCDPSession: async () => cdp,
    newPage: async () => ({
      goto: () => {
        events.push("browser");
        return new Promise(() => {});
      },
      close: async () => events.push("browser_closed"),
    }),
  });
  const runner = createRunner({
    executionTimeoutMs: 20_000,
    measurementSourceTimeoutMs: 30_000,
    measurementExecutionTimeoutMs: 120_000,
    browserType: {
      launch: async () => ({
        newContext: async () => ({
          newPage: async () => page,
          storageState: async () => ({ cookies: [], origins: [] }),
          close: async () => {},
        }),
        close: async () => {},
      }),
    },
    platformAdapters: {
      fixture: {
        connectorVersion: "synthetic.pipeline.v1",
        entry: `${ORIGIN}/`,
        operations: ["measure"],
        identify: async () => ({
          platform_account_id: "synthetic-owner",
          display_name: "Synthetic owner",
        }),
        execute: async (activePage, _operation, frozen, hooks) => {
          network = hooks;
          return measureKimi(activePage, frozen, hooks);
        },
      },
    },
  });
  t.after(() => runner.shutdown());
  await runner.create({
    session_id: "synthetic-session",
    platform: "fixture",
    storage_state: { cookies: [], origins: [] },
  });
  await runner.complete("synthetic-session");
  const input = {
    execution_id: "synthetic-execution",
    session_id: "synthetic-session",
    operation: "measure",
    payload,
    source_capture_ticket: "ab".repeat(32),
  };
  const persist = async (body) => {
    const phase = body.snapshot.phase;
    events.push(`${phase}_pending`);
    await (phase === "source" ? holdSource?.promise : holdCandidate?.promise);
    stored.push(structuredClone(body));
    events.push(`${phase}_stored`);
    return {
      schema_version: 1,
      capture_id: SOURCE_ID,
      digest_sha256: "a".repeat(64),
      stored_at: "2026-01-01T00:00:00Z",
    };
  };
  const ai = {
    policy: async (body) => {
      events.push("policy");
      assert.equal(stored[0]?.snapshot.phase, "source");
      assert.equal(body.source_capture_id, SOURCE_ID);
      assert.equal(body.source_sha256, stored[0].snapshot.source_sha256);
      return { prefer_connected_account: preferBrowser, config_version: 2 };
    },
    extract: async (body, { signal }) => {
      events.push("api");
      assert.equal(body.source_capture_id, SOURCE_ID);
      assert.equal(body.source_sha256, stored[0].snapshot.source_sha256);
      apiSignal = signal;
      apiStartedAt = now;
      return apiResult.promise;
    },
  };
  let settled = false;
  const result = runner.execute(input, persist, ai).then((value) => {
    settled = true;
    events.push("receipt");
    return value;
  });
  await flush();
  return {
    events,
    stored,
    result,
    apiResult,
    get sends() {
      return sends;
    },
    get settled() {
      return settled;
    },
    get apiSignal() {
      return apiSignal;
    },
    get apiStartedAt() {
      return apiStartedAt;
    },
    get network() {
      return network;
    },
    async advance(ms) {
      now += ms;
      t.mock.timers.tick(ms);
      await flush();
    },
    replay: () => runner.execute(input, persist, ai),
  };
}

test("durable source acknowledgement precedes policy and API extraction", async (t) => {
  const holdSource = deferred();
  const p = await pipeline(t, { holdSource });
  assert.deepEqual(p.events, ["question", "source_pending"]);
  assert.equal(p.stored.length, 0);
  assert.equal(p.settled, false);
  holdSource.resolve();
  await flush();
  assert.deepEqual(p.events, [
    "question",
    "source_pending",
    "source_stored",
    "policy",
    "api",
  ]);
  p.apiResult.resolve(extraction);
  assert.equal((await p.result).status, "completed");
  assert.equal(p.sends, 1);
});

test("real parsing can outlast the capture window and still complete", async (t) => {
  const p = await pipeline(t);
  assert.equal(p.network.sourceDeadlineAt, 30_000);
  await p.advance(35_000);
  assert.equal(p.settled, false);
  assert.equal(p.apiSignal.aborted, false);
  p.apiResult.resolve(extraction);
  const receipt = await p.result;
  assert.equal(receipt.status, "completed");
  assert.equal(receipt.raw_answer, messages[3].text);
  assert.equal(receipt.provenance, "fixture");
  assert.deepEqual(
    p.stored.map((body) => body.snapshot.phase),
    ["source", "candidate"],
  );
  assert.equal(p.sends, 1);
});

test("fallback is clipped to remaining analysis deadline and timeout preserves source without resending", async (t) => {
  const holdSource = deferred();
  const p = await pipeline(t, { preferBrowser: true, holdSource });
  await p.advance(20_000);
  holdSource.resolve();
  await flush();
  assert.ok(p.events.includes("browser"));
  assert.equal(p.apiStartedAt, undefined);
  await p.advance(45_000);
  assert.equal(p.apiStartedAt, 65_000);
  assert.ok(p.events.includes("browser_closed"));
  // Overall 120s minus the production 10s receipt reserve leaves only 45s,
  // not a fresh 60s API window after the connected-browser timeout.
  await p.advance(44_999);
  assert.equal(p.apiSignal.aborted, false);
  assert.equal(p.settled, false);
  await p.advance(1);
  const receipt = await p.result;
  assert.equal(p.apiSignal.aborted, true);
  assert.equal(receipt.status, "unknown");
  assert.deepEqual(
    p.stored.map((body) => body.snapshot.phase),
    ["source"],
  );
  assert.deepEqual(
    JSON.parse(p.stored[0].snapshot.source_json).messages,
    messages,
  );
  assert.deepEqual(await p.replay(), receipt);
  p.apiResult.resolve(extraction);
  await flush();
  assert.equal(p.stored.length, 1);
  assert.equal(p.sends, 1);
});

test("grounded candidate acknowledgement precedes successful runner receipt", async (t) => {
  const holdCandidate = deferred();
  const p = await pipeline(t, { holdCandidate });
  p.apiResult.resolve(extraction);
  await flush();
  assert.equal(p.events.at(-1), "candidate_pending");
  assert.equal(p.settled, false);
  assert.equal(p.stored.length, 1);
  holdCandidate.resolve();
  const receipt = await p.result;
  assert.equal(receipt.status, "completed");
  assert.deepEqual(p.events.slice(-2), ["candidate_stored", "receipt"]);
  assert.equal(p.stored[1].snapshot.source_capture_id, SOURCE_ID);
  assert.equal(p.stored[1].snapshot.grounding_reason, "grounded");
  assert.equal(p.sends, 1);
});
