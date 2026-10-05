import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import {
  adapters,
  extractKimiCandidateObservation,
  kimiIdentity,
  measureKimi,
  observeKimiSearchAttempt,
  probeKimiAccount,
} from "../src/adapters.mjs";
import { createRunner } from "../src/runner.mjs";

test("candidate collector waits for explicit completion and removes listeners", async () => {
  const page = new EventEmitter();
  const response = (requestId) => ({
    url: () => "https://example.test/event",
    status: () => 200,
    headers: () => ({ "content-type": "application/json" }),
    body: async () =>
      Buffer.from(
        JSON.stringify({ event_id: "event-1", request_id: requestId }),
      ),
  });
  const flow = {
    trustedOrigin: "https://example.test",
    submit: async () => {},
    readAnswer: async () => ({
      raw_answer: "Synthetic answer",
      request_id: "request-1",
      citations: [],
    }),
    decodeEvent: (body) => body,
    waitForCompletion: async () => {
      await new Promise((resolve) => setImmediate(resolve));
      page.emit("response", response("unrelated"));
      page.emit("response", response("request-1"));
    },
  };
  const result = await observeKimiSearchAttempt(page, "Question", flow);
  assert.equal(result.search_verified, false);
  assert.equal(result.candidate_search_event.request_id, "request-1");
  assert.equal(page.listenerCount("response"), 0);
  assert.equal(
    await observeKimiSearchAttempt(page, "Question", {
      ...flow,
      waitForCompletion: async () => {
        page.emit("response", response("request-1"));
        page.emit("response", response("request-1"));
      },
    }),
    null,
  );
  assert.equal(page.listenerCount("response"), 0);
  assert.equal(
    await observeKimiSearchAttempt(page, "Question", {
      ...flow,
      decodeEvent: () => {
        throw new Error("Malformed fixture");
      },
    }),
    null,
  );
  assert.equal(page.listenerCount("response"), 0);
});

test("candidate collector bounds stalled work and aborts flow without verification", async () => {
  const page = new EventEmitter();
  let signal;
  const result = await observeKimiSearchAttempt(page, "Question", {
    timeoutMs: 10,
    submit: async (_page, _question, abortSignal) => {
      signal = abortSignal;
      await new Promise(() => {});
    },
    decodeEvent: () => null,
    readAnswer: async () => {
      throw new Error("must not read after timeout");
    },
  });
  assert.equal(result, null);
  assert.equal(signal.aborted, true);
  assert.equal(page.listenerCount("response"), 0);
});

test("source-derived Kimi self probe requires authenticated browser storage and own user JSON", async () => {
  const server = createServer((request, response) => {
    if (request.url === "/") {
      response.writeHead(200, { "content-type": "text/html" });
      response.end("<!doctype html><html><body>Fixture</body></html>");
    } else if (
      request.url ===
        "/apiv2/kimi.gateway.account.v1.UserService/GetCurrentUser" &&
      request.method === "POST" &&
      request.headers.authorization === "Bearer fixture-access"
    ) {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({ user: { id: "own-123", nickname: "Fixture owner" } }),
      );
    } else {
      response.writeHead(401, { "content-type": "application/json" });
      response.end("{}");
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  let browser;
  try {
    browser = await chromium.launch({
      ...(process.env.GEO_TEST_CHROMIUM_PATH
        ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
        : {}),
    });
    const anonymous = await browser.newContext();
    const anonymousPage = await anonymous.newPage();
    assert.equal(
      await probeKimiAccount(anonymousPage, { trustedOrigin: origin }),
      null,
    );
    await anonymous.close();
    const authenticated = await browser.newContext();
    await authenticated.addInitScript(() => {
      localStorage.setItem("access_token", "fixture-access");
      localStorage.setItem("refresh_token", "fixture-refresh");
    });
    const authenticatedPage = await authenticated.newPage();
    assert.deepEqual(
      await probeKimiAccount(authenticatedPage, { trustedOrigin: origin }),
      { platform_account_id: "own-123", display_name: "Fixture owner" },
    );
    await authenticated.close();
    assert.equal(
      kimiIdentity({ user: { id: "own-123", nickname: "" } }),
      null,
      "an ID alone is insufficient to mark the session connected",
    );
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
});

test("a candidate answer retains raw text, source URLs and request id but does not prove search", () => {
  assert.deepEqual(
    extractKimiCandidateObservation({
      raw_answer: "An unmodified fixture answer [1].",
      citations: [{ url: "https://example.org/article", title: "Reference" }],
      request_id: "fixture-request-1",
    }),
    {
      raw_answer: "An unmodified fixture answer [1].",
      citations: [{ url: "https://example.org/article", title: "Reference" }],
      request_id: "fixture-request-1",
      search_verified: false,
    },
  );
});

test("candidate extractor rejects malformed provenance and unsafe citation URLs", () => {
  const candidate = {
    raw_answer: "Example",
    citations: [{ url: "https://example.org/reference" }],
  };
  assert.equal(
    extractKimiCandidateObservation({ ...candidate, raw_answer: "" }),
    null,
  );
  assert.equal(
    extractKimiCandidateObservation({
      ...candidate,
      citations: [{ url: "https://user:password@example.org/" }],
    }),
    null,
  );
  assert.equal(
    extractKimiCandidateObservation({
      ...candidate,
      citations: [{ url: "http://localhost/private" }],
    }),
    null,
  );
  assert.equal(
    extractKimiCandidateObservation({ ...candidate, request_id: "not valid" }),
    null,
  );
  assert.equal(
    extractKimiCandidateObservation({ ...candidate, citations: "none" }),
    null,
  );
  assert.deepEqual(extractKimiCandidateObservation(candidate), {
    raw_answer: "Example",
    citations: [{ url: "https://example.org/reference" }],
    search_verified: false,
  });
});

test("Kimi web measurement cannot report fixture data or plain chat as a verified search", async () => {
  assert.equal(
    adapters.kimi.allowLoginControl(new URL("https://www.kimi.com/")),
    true,
    "the Kimi homepage hosts the login modal",
  );
  assert.equal(
    adapters.kimi.allowLoginControl(
      new URL("https://www.kimi.com/other/account"),
    ),
    false,
    "pixel login controls must stop outside the login surface",
  );
  assert.equal(
    adapters.kimi.allowLoginControl(new URL("https://www.kimi.com.evil.test/")),
    false,
  );
  assert.deepEqual(await adapters.kimi.execute(null, "measure", {}), {
    status: "unsupported",
    reason: "invalid_measurement_payload",
    evidence: [],
    connector_version: "live_unverified.source_derived.v1",
  });
});

test("installed measurement capability is explicitly unverified", async () => {
  const runner = createRunner();
  try {
    const kimi = runner
      .capabilities()
      .connectors.find((entry) => entry.platform === "kimi");
    assert.deepEqual(kimi, {
      platform: "kimi",
      placement_slot: "primary",
      connector_version: "live_unverified.source_derived.v1",
      operations: ["measure"],
      verified: false,
    });
  } finally {
    await runner.shutdown();
  }
});

const frozenMeasurement = Object.freeze({
  target_id: "11111111-1111-4111-8111-111111111111",
  account_id: "22222222-2222-4222-8222-222222222222",
  provider: "kimi",
  model: "sample-model",
  surface: "consumer_web",
  search_mode: "web_search",
  protocol_version: "v1",
  question_set_version: "v1",
  question: "What is a test?",
  market: "test-market",
  language: "en",
  scheduled_at: "2026-01-01T00:00:00Z",
  sample_ordinal: 0,
});

test("frozen web-search input is required; no injected content or mode fallback", async () => {
  for (const invalid of [
    { ...frozenMeasurement, question: "" },
    { ...frozenMeasurement, provider: "other" },
    { ...frozenMeasurement, surface: "api" },
    { ...frozenMeasurement, search_mode: "standard" },
    { ...frozenMeasurement, url: "https://example.org" },
    { ...frozenMeasurement, sample_ordinal: -1 },
  ]) {
    assert.equal(
      (await measureKimi(null, invalid, { expectedAccountId: "own-123" }))
        .reason,
      "invalid_measurement_payload",
    );
  }
  assert.equal(
    (await measureKimi(null, frozenMeasurement)).reason,
    "account_identity_unverified",
  );
  assert.deepEqual(
    await measureKimi(null, frozenMeasurement, {
      expectedAccountId: "own-123",
    }),
    {
      status: "unsupported",
      reason: "official_web_search_unverified",
      evidence: [],
      connector_version: "live_unverified.source_derived.v1",
    },
  );
});

test("browser capture matches provider event and answer request ID but never verifies fixtures", async () => {
  const server = createServer((request, response) => {
    if (request.url === "/") {
      response.writeHead(200, { "content-type": "text/html" });
      response.end("<!doctype html><html><body>Local test page</body></html>");
    } else if (request.url === "/event") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          kind: "official_search_event",
          event_id: "event-1",
          request_id: "request-1",
        }),
      );
    } else {
      response.writeHead(404);
      response.end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  let browser;
  try {
    browser = await chromium.launch({
      ...(process.env.GEO_TEST_CHROMIUM_PATH
        ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
        : {}),
    });
    const page = await browser.newPage();
    await page.goto(origin);
    const searchFlow = {
      trustedOrigin: origin,
      submit: async (browserPage, question) => {
        assert.equal(question, frozenMeasurement.question);
        await browserPage.evaluate(async () => {
          await fetch("/event");
        });
      },
      decodeEvent: (body) =>
        body.kind === "official_search_event" ? body : null,
      readAnswer: async () => ({
        raw_answer: "Fixture answer",
        citations: [{ url: "https://example.org/answer#paragraph" }],
        request_id: "request-1",
      }),
    };
    const captured = await observeKimiSearchAttempt(
      page,
      frozenMeasurement.question,
      searchFlow,
    );
    assert.match(captured.candidate_search_event.received_at, /^\d{4}-/u);
    assert.deepEqual(
      {
        ...captured,
        candidate_search_event: {
          ...captured.candidate_search_event,
          received_at: "timestamp",
        },
      },
      {
        raw_answer: "Fixture answer",
        citations: [{ url: "https://example.org/answer#paragraph" }],
        request_id: "request-1",
        search_verified: false,
        candidate_search_event: {
          event_id: "event-1",
          request_id: "request-1",
          received_at: "timestamp",
        },
      },
    );
    assert.deepEqual(
      await measureKimi(page, frozenMeasurement, {
        expectedAccountId: "own-123",
        searchFlow,
      }),
      {
        status: "unknown",
        reason: "official_search_provenance_unverified",
        stage: "measure",
        evidence: [],
        connector_version: "live_unverified.source_derived.v1",
      },
    );
    assert.equal(
      await observeKimiSearchAttempt(page, frozenMeasurement.question, {
        ...searchFlow,
        readAnswer: async () => ({
          raw_answer: "Answer with unrelated request",
          citations: [],
          request_id: "other-request",
        }),
      }),
      null,
    );
    assert.equal(
      await observeKimiSearchAttempt(page, frozenMeasurement.question, {
        ...searchFlow,
        readAnswer: async () => ({
          raw_answer: "Answer without a request",
          citations: [],
        }),
      }),
      null,
    );
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
