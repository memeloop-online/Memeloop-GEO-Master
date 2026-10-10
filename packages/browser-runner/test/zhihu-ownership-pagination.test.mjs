import assert from "node:assert/strict";
import { createServer, request as httpRequest } from "node:http";
import { after, before, test } from "node:test";
import { chromium } from "playwright";
import { readbackZhihu } from "../src/adapters.mjs";

let server;
let browser;
let context;
let page;
let origin;
let mode;
let listOffsets;
let escapedRequests;
let selfReads;

before(async () => {
  server = createServer((request, response) => {
    const url = new URL(request.url, origin ?? "http://localhost");
    if (url.pathname === "/self") {
      selfReads++;
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          id: mode === "switch-account" && selfReads > 1 ? "other" : "owner",
          name: "Owner",
          url_token: "owner-token",
        }),
      );
      return;
    }
    if (url.pathname === "/api/v4/members/owner-token/articles") {
      assert.equal(request.headers.cookie, "sid=ready");
      assert.equal(url.searchParams.get("limit"), "20");
      const offset = Number(url.searchParams.get("offset"));
      listOffsets.push(offset);
      if (mode === "stall") return;
      if (mode === "redirect") {
        response.writeHead(302, { location: `${origin}/escaped` });
        response.end();
        return;
      }
      const data =
        mode === "match-second" && offset === 20
          ? [{ id: 42, title: "Expected title" }]
          : Array.from({ length: 20 }, (_, index) => ({
              id: offset + index + 100,
              title: "Another article",
            }));
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          data,
          paging: {
            // These untrusted locations must never be navigated.
            is_end:
              mode === "end" || (mode === "match-second" && offset === 20),
            next: `${origin}/escaped?offset=0`,
          },
        }),
      );
      return;
    }
    if (url.pathname === "/escaped") escapedRequests++;
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    response.end(
      "<!doctype html><html><body><h1>Expected title</h1><article>Expected body</article></body></html>",
    );
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({
    ...(process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {}),
  });
  context = await browser.newContext();
  await context.addCookies([{ name: "sid", value: "ready", url: origin }]);
  page = await context.newPage();
});

after(async () => {
  await context?.close();
  await browser?.close();
  await new Promise((resolve) => server?.close(resolve));
});

async function checkOwnership(
  testMode,
  expectedAccountId = "owner",
  articlesOrigin = origin,
) {
  mode = testMode;
  listOffsets = [];
  escapedRequests = 0;
  selfReads = 0;
  const result = await readbackZhihu(
    page,
    `${origin}/p/42`,
    { title: "Expected title", body: "Expected body" },
    {
      postOrigin: origin,
      selfUrl: `${origin}/self`,
      articlesOrigin,
      expectedAccountId,
    },
  );
  assert.equal(escapedRequests, 0);
  return result;
}

test("ownership on page two uses the same account and computed fixed offsets", async () => {
  const result = await checkOwnership("match-second");
  assert.equal(result.status, "completed");
  assert.deepEqual(listOffsets, [0, 20]);
  assert.equal(selfReads, 2);
  assert.equal(result.evidence[0].owned_by_account, true);
});

test("own probes and public readback use the assigned proxy without sharing cookies", async () => {
  const proxyEvents = [];
  const proxyServer = createServer((incoming, outgoing) => {
    const destination = new URL(incoming.url);
    proxyEvents.push({
      path: `${destination.pathname}${destination.search}`,
      cookie: incoming.headers.cookie ?? null,
    });
    const upstream = httpRequest({
      hostname: "127.0.0.1",
      port: server.address().port,
      path: `${destination.pathname}${destination.search}`,
      method: incoming.method,
      headers: {
        ...incoming.headers,
        host: `127.0.0.1:${server.address().port}`,
      },
    });
    upstream.on("error", (error) => {
      outgoing.writeHead(502);
      outgoing.end(error.message);
    });
    upstream.on("response", (response) => {
      outgoing.writeHead(response.statusCode, response.headers);
      response.pipe(outgoing);
    });
    incoming.pipe(upstream);
  });
  await new Promise((resolve) => proxyServer.listen(0, "127.0.0.1", resolve));
  const proxy = {
    server: `http://127.0.0.1:${proxyServer.address().port}`,
  };
  // An unproxied request to this reserved hostname cannot reach the fixture.
  const proxiedOrigin = "http://ownership-proof.invalid";
  mode = "match-second";
  listOffsets = [];
  selfReads = 0;
  let proxiedContext;
  try {
    proxiedContext = await browser.newContext({ proxy });
    await proxiedContext.addCookies([
      { name: "sid", value: "ready", url: proxiedOrigin },
    ]);
    const proxiedPage = await proxiedContext.newPage();
    const result = await readbackZhihu(
      proxiedPage,
      `${proxiedOrigin}/p/42`,
      { title: "Expected title", body: "Expected body" },
      {
        postOrigin: proxiedOrigin,
        selfUrl: `${proxiedOrigin}/self`,
        articlesOrigin: proxiedOrigin,
        expectedAccountId: "owner",
        proxy,
      },
    );
    const publicRequests = proxyEvents.filter(
      (event) => event.path === "/p/42",
    );
    assert.equal(publicRequests.length, 1);
    assert.equal(publicRequests[0].cookie, null);
    const accountRequests = proxyEvents.filter(
      (event) =>
        event.path === "/self" ||
        event.path.startsWith("/api/v4/members/owner-token/articles?"),
    );
    assert.deepEqual(
      accountRequests.map((event) => event.path),
      [
        "/self",
        "/api/v4/members/owner-token/articles?limit=20&offset=0",
        "/self",
        "/api/v4/members/owner-token/articles?limit=20&offset=20",
      ],
    );
    assert.ok(accountRequests.every((event) => event.cookie === "sid=ready"));
    assert.equal(result.status, "completed", JSON.stringify(result));
  } finally {
    await proxiedContext?.close();
    await new Promise((resolve) => proxyServer.close(resolve));
  }
});

test("end of list and account switch remain unverified", async () => {
  const ended = await checkOwnership("end");
  assert.equal(ended.status, "unknown");
  assert.equal(ended.reason, "account_ownership_unverified");
  assert.deepEqual(listOffsets, [0]);

  const switched = await checkOwnership("switch-account");
  assert.equal(switched.status, "unknown");
  assert.deepEqual(listOffsets, [0]);
});

test("malicious next and cyclic paging remain limited to ten fixed pages", async () => {
  const result = await checkOwnership("cycle");
  assert.equal(result.status, "unknown");
  assert.equal(result.reason, "account_ownership_unverified");
  assert.deepEqual(
    listOffsets,
    Array.from({ length: 10 }, (_, index) => index * 20),
  );
  assert.equal(selfReads, 10);
});

test("article-list redirect to an unapproved path is blocked", async () => {
  const result = await checkOwnership("redirect");
  assert.equal(result.status, "unknown");
  assert.deepEqual(listOffsets, [0]);
  assert.equal(escapedRequests, 0);
});

test("article-list origin cannot include an injected path", async () => {
  const result = await checkOwnership(
    "match-second",
    "owner",
    `${origin}/escaped`,
  );
  assert.equal(result.status, "unknown");
  assert.equal(result.reason, "account_ownership_unverified");
  assert.deepEqual(listOffsets, []);
  assert.equal(escapedRequests, 0);
  assert.equal(selfReads, 0);
});

test(
  "a stalled article-list request ends as unverified without another page",
  { timeout: 20_000 },
  async () => {
    const result = await checkOwnership("stall");
    assert.equal(result.status, "unknown");
    assert.deepEqual(listOffsets, [0]);
  },
);
