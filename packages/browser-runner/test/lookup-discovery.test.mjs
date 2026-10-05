import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createServer, request as httpRequest } from "node:http";
import { after, before, test } from "node:test";
import { chromium } from "playwright";
import { adapters } from "../src/adapters.mjs";

const title = "Frozen title";
const body = "Frozen body";
let browser;
let context;
let page;
let fixture;
let origin;
let mode;
let offsets;
let selfReads;
let publicReads;
let externalReads;

function sendJson(response, data) {
  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify(data));
}

function listFor(offset) {
  if (mode === "missing") return { data: [], paging: { is_end: true } };
  const data =
    offset === 0
      ? Array.from({ length: 20 }, (_, index) => ({
          id: index + 100,
          title: "Other title",
        }))
      : [{ id: mode === "different-body-only" ? 43 : 42, title }];
  if ((mode === "different-body" || mode === "inaccessible") && offset === 20)
    data.push({ id: 43, title });
  if (mode === "ambiguous" && offset === 20) data.push({ id: 44, title });
  if (mode === "duplicate" && offset === 20)
    data.push({ id: 100, title: "Other title" });
  if (mode === "unbounded") {
    return {
      data: Array.from({ length: 20 }, (_, index) => ({
        id: 100 + offset + index,
        title: offset === 20 && index === 0 ? title : "Other title",
      })),
      paging: { is_end: false, next: `${origin}/escape` },
    };
  }
  const isEnd = offset === 20;
  const paging = { is_end: isEnd, next: `${origin}/escape` };
  if (mode === "no-end") delete paging.is_end;
  if (mode === "short" && offset === 0) data.pop();
  if (mode === "malformed" && offset === 20) data.push({ title });
  return { data, paging };
}

before(async () => {
  fixture = createServer((request, response) => {
    const url = new URL(request.url, origin ?? "http://127.0.0.1");
    if (
      mode === "proxy" &&
      (url.pathname === "/self" ||
        url.pathname.startsWith("/api/v4/members/") ||
        url.pathname.startsWith("/p/")) &&
      request.headers["x-fixture-proxy"] !== "yes"
    ) {
      response.writeHead(502).end();
      return;
    }
    if (url.pathname === "/self") {
      selfReads++;
      if (!request.headers.cookie?.includes("sid=ready")) {
        response.writeHead(403).end();
        return;
      }
      sendJson(response, {
        id: mode === "switch" && selfReads > 1 ? "someone-else" : "owner",
        name: "Own account",
        url_token:
          mode === "token-switch" && selfReads > 1
            ? "other-token"
            : "owner-token",
      });
      return;
    }
    if (url.pathname.startsWith("/api/v4/members/")) {
      assert.equal(request.headers.cookie, "sid=ready");
      assert.equal(url.pathname, "/api/v4/members/owner-token/articles");
      assert.equal(url.searchParams.get("limit"), "20");
      offsets.push(Number(url.searchParams.get("offset")));
      if (mode === "stalled") return;
      sendJson(response, listFor(Number(url.searchParams.get("offset"))));
      return;
    }
    if (url.pathname.startsWith("/p/")) {
      publicReads.push({
        path: url.pathname,
        cookie: request.headers.cookie ?? null,
      });
      if (mode === "inaccessible" && url.pathname === "/p/43") {
        response.writeHead(403).end();
        return;
      }
      response.writeHead(200, { "content-type": "text/html" });
      response.end(
        `<!doctype html><h1>${title}</h1><article>${url.pathname === "/p/43" ? "Different body" : body}</article>`,
      );
      return;
    }
    if (url.pathname === "/escape") externalReads++;
    response.writeHead(404).end();
  });
  await new Promise((resolve) => fixture.listen(0, "127.0.0.1", resolve));
  origin = `http://127.0.0.1:${fixture.address().port}`;
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
  await new Promise((resolve) => fixture?.close(resolve));
});

async function lookup(testMode, overrides = {}) {
  mode = testMode;
  offsets = [];
  selfReads = 0;
  publicReads = [];
  externalReads = 0;
  const outcome = await adapters.zhihu.execute(
    page,
    "lookup",
    { title, body },
    {
      expectedAccountId: "owner",
      selfUrl: `${origin}/self`,
      articlesOrigin: origin,
      postOrigin: origin,
      ...overrides,
    },
  );
  assert.equal(externalReads, 0, "never follows untrusted paging.next");
  return outcome;
}

test("discovers the sole exact public asset on a later complete list page", async () => {
  const outcome = await lookup("unique");
  assert.equal(outcome.status, "completed", JSON.stringify(outcome));
  assert.equal(outcome.public_url, `${origin}/p/42`);
  assert.deepEqual(offsets, [0, 20]);
  assert.equal(selfReads, 2);
  assert.deepEqual(publicReads, [{ path: "/p/42", cookie: null }]);
  assert.deepEqual(
    outcome.evidence.map((item) => item.kind),
    ["public_readback", "own_account_list_discovery"],
  );
  const proof = outcome.evidence[1];
  assert.deepEqual(Object.keys(proof).sort(), [
    "exact_match_count",
    "expected_sha256",
    "kind",
    "list_complete",
    "observed_at",
    "public_url",
    "schema_version",
  ]);
  assert.equal(proof.schema_version, "geo.publication.discovery.v1");
  assert.equal(proof.list_complete, true);
  assert.equal(proof.exact_match_count, 1);
  assert.equal(proof.observed_at, outcome.occurred_at);
  assert.equal(
    proof.expected_sha256,
    createHash("sha256").update(`${title}\n${body}`).digest("hex"),
  );
  assert.equal(JSON.stringify(outcome).includes("owner-token"), false);
  assert.equal(JSON.stringify(outcome).includes('"owner"'), false);
});

test("same-title different-body asset is not counted as an exact match", async () => {
  const outcome = await lookup("different-body");
  assert.equal(outcome.status, "completed");
  assert.equal(outcome.public_url, `${origin}/p/42`);
  assert.deepEqual(
    publicReads.map(({ path }) => path),
    ["/p/42", "/p/43"],
  );
  const mismatchOnly = await lookup("different-body-only");
  assert.equal(mismatchOnly.status, "unknown");
  assert.equal(mismatchOnly.reason, "own_account_list_no_match");
  assert.deepEqual(publicReads, [{ path: "/p/43", cookie: null }]);
});

test("two exact assets are ambiguous, including when a later page matches", async () => {
  const outcome = await lookup("ambiguous");
  assert.equal(outcome.status, "unknown");
  assert.equal(outcome.reason, "own_account_list_ambiguous");
  assert.deepEqual(outcome.evidence, []);
  assert.equal(Object.hasOwn(outcome, "public_url"), false);
});

test("inaccessible title-matched asset keeps even another exact asset unknown", async () => {
  const outcome = await lookup("inaccessible");
  assert.equal(outcome.status, "unknown");
  assert.equal(outcome.reason, "public_readback_unavailable");
  assert.deepEqual(outcome.evidence, []);
});

test("missing expected identity and switched accounts fail without opening assets", async () => {
  const missing = await lookup("unique", { expectedAccountId: undefined });
  assert.equal(missing.reason, "own_account_list_unverified");
  assert.deepEqual(offsets, []);
  const switched = await lookup("switch");
  assert.equal(switched.reason, "own_account_list_unverified");
  assert.deepEqual(offsets, [0]);
  const tokenSwitched = await lookup("token-switch");
  assert.equal(tokenSwitched.reason, "own_account_list_unverified");
  assert.deepEqual(offsets, [0]);
  assert.deepEqual(publicReads, []);
});

test("malformed, truncated, duplicate and missing end remain unknown", async () => {
  for (const variant of [
    "malformed",
    "short",
    "no-end",
    "duplicate",
    "unbounded",
  ]) {
    const outcome = await lookup(variant);
    assert.equal(outcome.status, "unknown", variant);
    assert.equal(outcome.reason, "own_account_list_unverified", variant);
    assert.deepEqual(publicReads, [], variant);
    assert.ok(offsets.length <= 10, variant);
  }
  const empty = await lookup("missing");
  assert.equal(empty.reason, "own_account_list_no_match");
  assert.deepEqual(publicReads, []);
});

test(
  "timed-out own-account list cannot establish discovery",
  { timeout: 20_000 },
  async () => {
    const outcome = await lookup("stalled");
    assert.equal(outcome.status, "unknown");
    assert.equal(outcome.reason, "own_account_list_unverified");
    assert.deepEqual(offsets, [0]);
    assert.deepEqual(publicReads, []);
  },
);

test("discovery keeps authenticated list and cookie-free public readback on one proxy", async () => {
  const traffic = [];
  const proxyServer = createServer((incoming, outgoing) => {
    const destination = new URL(incoming.url);
    traffic.push({
      path: `${destination.pathname}${destination.search}`,
      cookie: incoming.headers.cookie ?? null,
    });
    const upstream = httpRequest({
      hostname: "127.0.0.1",
      port: fixture.address().port,
      path: `${destination.pathname}${destination.search}`,
      headers: {
        ...incoming.headers,
        host: `127.0.0.1:${fixture.address().port}`,
        "x-fixture-proxy": "yes",
      },
    });
    upstream.on("error", () => outgoing.writeHead(502).end());
    upstream.on("response", (response) => {
      outgoing.writeHead(response.statusCode, response.headers);
      response.pipe(outgoing);
    });
    incoming.pipe(upstream);
  });
  await new Promise((resolve) => proxyServer.listen(0, "127.0.0.1", resolve));
  const proxy = { server: `http://127.0.0.1:${proxyServer.address().port}` };
  const proxiedOrigin = origin;
  mode = "proxy";
  offsets = [];
  selfReads = 0;
  publicReads = [];
  externalReads = 0;
  let proxiedContext;
  try {
    proxiedContext = await browser.newContext({ proxy });
    await proxiedContext.addCookies([
      { name: "sid", value: "ready", url: proxiedOrigin },
    ]);
    const proxiedPage = await proxiedContext.newPage();
    const outcome = await adapters.zhihu.execute(
      proxiedPage,
      "lookup",
      { title, body },
      {
        expectedAccountId: "owner",
        selfUrl: `${proxiedOrigin}/self`,
        articlesOrigin: proxiedOrigin,
        postOrigin: proxiedOrigin,
        proxy,
      },
    );
    assert.equal(outcome.status, "completed", JSON.stringify(outcome));
    assert.deepEqual(
      traffic.map(({ path }) => path),
      [
        "/self",
        "/api/v4/members/owner-token/articles?limit=20&offset=0",
        "/self",
        "/api/v4/members/owner-token/articles?limit=20&offset=20",
        "/p/42",
      ],
    );
    assert.ok(
      traffic.slice(0, 4).every((event) => event.cookie === "sid=ready"),
    );
    assert.equal(traffic[4].cookie, null);
    assert.deepEqual(publicReads, [{ path: "/p/42", cookie: null }]);
  } finally {
    await proxiedContext?.close();
    await new Promise((resolve) => proxyServer.close(resolve));
  }
});
