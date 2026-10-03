import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import {
  adapters,
  extractKimiCandidateObservation,
  kimiIdentity,
  probeKimiAccount,
} from "../src/adapters.mjs";

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
  assert.deepEqual(await adapters.kimi.execute(), {
    status: "unsupported",
    reason: "official_web_search_unverified",
    evidence: [],
    connector_version: "live_unverified.source_derived.v1",
  });
});
