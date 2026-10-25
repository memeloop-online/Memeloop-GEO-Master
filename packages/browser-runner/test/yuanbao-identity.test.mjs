import assert from "node:assert/strict";
import { test } from "node:test";
import { runInNewContext } from "node:vm";
import { chromium } from "playwright";
import {
  adapters,
  yuanbaoIdentity,
  probeYuanbaoAccount,
} from "../src/adapters.mjs";

const origin = "https://yuanbao.tencent.com";
const endpoint = `${origin}/api/getuserinfo`;
const own = () => ({
  userId: "synthetic-account",
  nickName: "Synthetic user",
  anonUser: { isAnon: false },
  token: "synthetic-private-token",
  mobile: "synthetic-private-mobile",
});
const expected = {
  platform_account_id: "synthetic-account",
  display_name: "Synthetic user",
};

test("Yuanbao accepts its own account and excludes anonymous or malformed identities", () => {
  assert.deepEqual(yuanbaoIdentity(own()), expected);
  assert.deepEqual(
    yuanbaoIdentity({ ...own(), anonUser: undefined }),
    expected,
  );
  assert.deepEqual(yuanbaoIdentity({ ...own(), nickName: "" }), {
    ...expected,
    display_name: "Account · ccount",
  });
  for (const userId of [undefined, null, "", " ", 1, {}, [], "a_synthetic"]) {
    assert.equal(yuanbaoIdentity({ ...own(), userId }), null);
  }
  for (const isAnon of [true, null, "false", 0, {}]) {
    assert.equal(yuanbaoIdentity({ ...own(), anonUser: { isAnon } }), null);
  }
  for (const data of [null, {}, { error: { code: "synthetic-error" } }]) {
    assert.equal(yuanbaoIdentity(data), null);
  }
  assert.equal(adapters.yuanbao.identify, probeYuanbaoAccount);
});

test("Yuanbao locks its origin and response URL while retaining only identity fields", async () => {
  let calls = 0;
  const probe = (pageOrigin = origin, responseUrl = endpoint) =>
    probeYuanbaoAccount({
      async evaluate(fn, args) {
        const result = await runInNewContext(`(${fn.toString()})`, {
          location: { origin: pageOrigin },
          AbortSignal,
          TextDecoder,
          fetch: async (path, options) => {
            calls++;
            assert.equal(path, "/api/getuserinfo");
            assert.equal(options.method, "GET");
            assert.equal(options.credentials, "same-origin");
            assert.equal(options.redirect, "error");
            assert.deepEqual(Object.keys(options.headers), []);
            const response = new Response(JSON.stringify(own()), {
              headers: { "content-type": "application/json" },
            });
            Object.defineProperty(response, "url", { value: responseUrl });
            return response;
          },
        })(args);
        assert.equal(
          JSON.stringify(result)?.includes("synthetic-private"),
          false,
        );
        return result;
      },
    });
  assert.equal(await probe("https://other.example"), null);
  assert.equal(calls, 0);
  assert.deepEqual(await probe(), expected);
  for (const url of [
    `${endpoint}?extra=1`,
    `${endpoint}#`,
    `${endpoint}/`,
    endpoint.replace("https:", "http:"),
    endpoint.replace("yuanbao.tencent.com", "other.example"),
  ]) {
    assert.equal(await probe(origin, url), null);
  }
});

test("Yuanbao browser probe reads the existing cookie session without changing the page", async () => {
  const browser = await chromium.launch(
    process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {},
  );
  try {
    const context = await browser.newContext();
    await context.addCookies([
      { name: "synthetic_session", value: "synthetic-cookie", url: origin },
    ]);
    let status = 200;
    let body = own();
    let calls = 0;
    await context.route("**/*", async (route) => {
      if (route.request().url() === `${origin}/`) {
        return route.fulfill({
          contentType: "text/html",
          body: '<input id="draft" value="unsent">',
        });
      }
      assert.equal(route.request().url(), endpoint);
      assert.match(
        (await route.request().allHeaders()).cookie,
        /synthetic-cookie/,
      );
      calls++;
      await route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    });
    const page = await context.newPage();
    await page.goto(origin);
    assert.deepEqual(await adapters.yuanbao.identify(page), expected);
    status = 401;
    assert.equal(await adapters.yuanbao.identify(page), null);
    status = 200;
    body = { ...own(), anonUser: { isAnon: true } };
    assert.equal(await adapters.yuanbao.identify(page), null);
    assert.equal(calls, 3);
    assert.equal(page.url(), `${origin}/`);
    assert.equal(await page.locator("#draft").inputValue(), "unsent");
  } finally {
    await browser.close();
  }
});
