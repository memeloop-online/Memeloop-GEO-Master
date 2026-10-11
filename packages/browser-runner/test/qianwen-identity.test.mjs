import assert from "node:assert/strict";
import { test } from "node:test";
import { runInNewContext } from "node:vm";
import { chromium } from "playwright";
import {
  adapters,
  qianwenIdentity,
  probeQianwenAccount,
} from "../src/adapters.mjs";

const origin = "https://www.qianwen.com";
const endpoint = "https://api.qianwen.com/growth/user/benefit/user/member/info";
const own = () => ({
  success: true,
  failed: false,
  data: {
    userId: "synthetic-account",
    userNick: "Synthetic user",
    userAvatar: "synthetic-private-avatar",
    email: "synthetic-private-email",
    mobile: "synthetic-private-mobile",
    token: "synthetic-private-token",
  },
});
const expected = {
  platform_account_id: "synthetic-account",
  display_name: "Synthetic user",
};

test("Qianwen requires explicit success and a nonempty string ID, never contact fallback", () => {
  assert.deepEqual(qianwenIdentity(own()), expected);
  for (const value of [undefined, null, false, "true", 1, {}]) {
    assert.equal(qianwenIdentity({ ...own(), success: value }), null);
  }
  for (const value of [undefined, null, true, "false", 0, {}]) {
    assert.equal(qianwenIdentity({ ...own(), failed: value }), null);
  }
  for (const userId of [undefined, null, "", " ", 1, {}, []]) {
    assert.equal(
      qianwenIdentity({ ...own(), data: { ...own().data, userId } }),
      null,
    );
  }
  for (const body of [null, {}, { ...own(), data: null }])
    assert.equal(qianwenIdentity(body), null);
  assert.deepEqual(
    qianwenIdentity({ ...own(), data: { ...own().data, userNick: "" } }),
    { ...expected, display_name: "Account · ccount" },
  );
});

test("Qianwen locks both origins and exact response URL without an override", async () => {
  let requests = 0;
  const probe = (
    responseUrl = endpoint,
    pageOrigin = origin,
    redirected = false,
  ) =>
    probeQianwenAccount(
      {
        evaluate: async (fn, args) =>
          runInNewContext(`(${fn.toString()})`, {
            location: { origin: pageOrigin },
            AbortSignal,
            TextDecoder,
            fetch: async (url, options) => {
              requests++;
              assert.equal(url, endpoint);
              assert.equal(options.method, "POST");
              assert.equal(options.body, '{"clientChannel":"PC"}');
              assert.equal(options.credentials, "include");
              assert.equal(options.redirect, "error");
              assert.deepEqual(Object.keys(options.headers), ["Content-Type"]);
              assert.equal(options.headers["Content-Type"], "application/json");
              const response = new Response(JSON.stringify(own()), {
                headers: { "content-type": "application/json" },
              });
              Object.defineProperty(response, "url", { value: responseUrl });
              Object.defineProperty(response, "redirected", {
                value: redirected,
              });
              return response;
            },
          })(args),
      },
      { trustedOrigin: pageOrigin, endpoint: responseUrl },
    );
  for (const wrongOrigin of [
    "https://other.example",
    "http://www.qianwen.com",
    "https://api.qianwen.com",
  ])
    assert.equal(await probe(endpoint, wrongOrigin), null);
  assert.equal(requests, 0);
  assert.deepEqual(await probe(), expected);
  for (const url of [
    `${endpoint}?extra=1`,
    `${endpoint}#`,
    `${endpoint}/`,
    endpoint.replace("api.qianwen.com", "other.example"),
    endpoint.replace("https:", "http:"),
    endpoint.replace("/member/", "/other/"),
    endpoint.replace("https://", "https://synthetic@"),
  ])
    assert.equal(await probe(url), null);
  assert.equal(await probe(endpoint, origin, true), null);
});

test("Qianwen browser probe is bounded, credential-confined and preserves the current page", async () => {
  const browser = await chromium.launch(
    process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {},
  );
  const state = {
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(own()),
  };
  let calls = 0;
  async function setup(context) {
    await context.route("**/*", async (route) => {
      const request = route.request();
      if (request.url() === `${origin}/`) {
        await route.fulfill({
          contentType: "text/html",
          body: '<input id="draft" value="unsent">',
        });
        return;
      }
      assert.equal(
        request.url(),
        endpoint,
        "no other endpoint or redirect may be contacted",
      );
      assert.equal(request.method(), "POST");
      assert.equal(request.postData(), '{"clientChannel":"PC"}');
      assert.match(
        (await request.allHeaders()).cookie ?? "",
        /synthetic_session=synthetic-cookie/,
      );
      calls++;
      await route.fulfill({
        ...state,
        headers: {
          "access-control-allow-origin": origin,
          "access-control-allow-credentials": "true",
          ...(state.status === 302
            ? { location: `${origin}/must-not-follow` }
            : {}),
        },
      });
    });
  }
  try {
    const context = await browser.newContext();
    await setup(context);
    await context.addCookies([
      {
        name: "synthetic_session",
        value: "synthetic-cookie",
        domain: ".qianwen.com",
        path: "/",
        secure: true,
        httpOnly: true,
        sameSite: "None",
      },
    ]);
    const page = await context.newPage();
    await page.goto(origin);
    const guardedPage = {
      async evaluate(fn, args) {
        const result = await page.evaluate(fn, args);
        assert.equal(
          JSON.stringify(result)?.includes("synthetic-private"),
          false,
        );
        return result;
      },
    };
    assert.deepEqual(await probeQianwenAccount(guardedPage), expected);
    const restored = await browser.newContext({
      storageState: await context.storageState(),
    });
    await setup(restored);
    const restoredPage = await restored.newPage();
    await restoredPage.goto(origin);
    assert.deepEqual(await probeQianwenAccount(restoredPage), expected);
    await restored.close();
    const padded = { ...own(), padding: "" };
    const remaining = 128_000 - Buffer.byteLength(JSON.stringify(padded));
    state.body = JSON.stringify({ ...padded, padding: "x".repeat(remaining) });
    assert.deepEqual(await probeQianwenAccount(guardedPage), expected);
    for (const invalid of [
      {
        body: JSON.stringify({ ...padded, padding: "x".repeat(remaining + 1) }),
      },
      { body: JSON.stringify({ ...padded, padding: "界".repeat(43_000) }) },
      { body: "{" },
      { body: JSON.stringify({ ...own(), success: false }) },
      { contentType: "text/html" },
      { contentType: "application/json-invalid" },
      { status: 401 },
      { status: 403 },
      { status: 500 },
      { status: 302 },
    ]) {
      Object.assign(
        state,
        {
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(own()),
        },
        invalid,
      );
      assert.equal(await probeQianwenAccount(guardedPage), null);
    }
    assert.ok(calls >= 3);
    assert.equal(page.url(), `${origin}/`);
    assert.equal(await page.locator("#draft").inputValue(), "unsent");
    assert.equal(context.pages().length, 1);
    assert.deepEqual(adapters.qianwen.operations, []);
    assert.equal(adapters.qianwen.inspectMeasurementOptions, undefined);
  } finally {
    await browser.close();
  }
});
