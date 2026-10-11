import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { runInNewContext } from "node:vm";
import { chromium } from "playwright";
import {
  adapters,
  glmIdentity,
  probeGlmAccount,
  deepseekIdentity,
  doubaoIdentity,
  probeDeepseekAccount,
  probeDoubaoAccount,
} from "../src/adapters.mjs";

const own = () => ({
  status: 0,
  result: {
    _id: "synthetic-account",
    username: "Synthetic user",
    is_guest: false,
  },
});

const consumerCases = [
  {
    provider: "deepseek",
    probe: probeDeepseekAccount,
    identity: deepseekIdentity,
    path: "/api/v0/users/current",
    own: () => ({
      code: 0,
      data: {
        biz_code: 0,
        biz_data: {
          id: "synthetic-account",
          id_profile: { name: "Synthetic user" },
          email: "synthetic-private-email",
          mobile: "synthetic-private-mobile",
          token: "synthetic-private-token",
        },
      },
    }),
    invalid: [
      { code: 40002 },
      { code: 0, data: { biz_code: 1, biz_data: { id: "synthetic" } } },
      ...[true, null, "false", {}].map((is_guest) => ({
        code: 0,
        data: { biz_code: 0, biz_data: { id: "synthetic", is_guest } },
      })),
      {
        code: 0,
        data: { biz_code: 0, biz_data: { email: "not-an-identity" } },
      },
    ],
  },
  {
    provider: "doubao",
    probe: probeDoubaoAccount,
    identity: doubaoIdentity,
    path: "/passport/account/info/v2/?aid=497858&account_sdk_source=web&sdk_version=2.2.11-doubao.0&device_platform=web",
    own: () => ({
      message: "success",
      data: {
        user_id_str: "synthetic-account",
        sec_user_id: "synthetic-secondary",
        name: "Synthetic user",
        session_key: "synthetic-private-token",
        email: "synthetic-private-email",
        mobile: "synthetic-private-mobile",
      },
    }),
    invalid: [
      { message: "error", data: { error_code: 13 } },
      {
        message: "success",
        data: {
          error_code: 13,
          user_id_str: "synthetic",
          sec_user_id: "secondary",
        },
      },
      {
        message: "success",
        data: { user_id_str: "synthetic", name: "Name only" },
      },
      {
        message: "success",
        data: { sec_user_id: "secondary", name: "Name only" },
      },
      ...[true, null, "false", {}].map((is_guest) => ({
        message: "success",
        data: { user_id_str: "synthetic", sec_user_id: "secondary", is_guest },
      })),
    ],
  },
];

test("Doubao accepts only website signature additions without weakening response URL binding", async () => {
  const example = consumerCases.find((item) => item.provider === "doubao");
  const origin = "https://www.doubao.com";
  const expected = `${origin}${example.path}`;
  const withQuery = (change) => {
    const url = new URL(expected);
    change(url.searchParams);
    return url.href;
  };
  const probeUrl = async (url) =>
    probeDoubaoAccount({
      evaluate: async (fn, args) => {
        const response = new Response(JSON.stringify(example.own()), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
        Object.defineProperty(response, "url", { value: url });
        return runInNewContext(`(${fn.toString()})`, {
          location: { origin },
          URL,
          AbortSignal,
          TextDecoder,
          // Synthetic credentials stay confined to this isolated fixture.
          localStorage: {
            getItem: () =>
              JSON.stringify({ __version: "0", value: "synthetic" }),
          },
          document: { cookie: "chatglm_token=synthetic" },
          fetch: async (requested, options) => {
            assert.equal(requested, example.path);
            assert.equal(options.method, "GET");
            assert.equal(options.credentials, "same-origin");
            assert.equal(options.redirect, "error");
            return response;
          },
        })(args);
      },
    });
  for (const url of [
    expected,
    `${expected}&msToken=synthetic-token`,
    `${expected}&a_bogus=synthetic-signature`,
    `${expected}&msToken=synthetic-token&a_bogus=synthetic-signature`,
    withQuery((params) => {
      params.append("a_bogus", "synthetic-signature");
      params.sort();
    }),
  ])
    assert.deepEqual(await probeUrl(url), example.identity(example.own()));

  const invalid = [
    `${expected}&unrecognized=synthetic`,
    `${expected}&msToken=one&msToken=two`,
    `${expected}&a_bogus=one&a_bogus=two`,
    `${expected}&msToken=one&%6DsToken=two`,
    `${expected}&MS_TOKEN=synthetic`,
    `${expected}#fragment`,
    `${expected}#`,
    expected.replace("https://www.doubao.com", "https://other.example"),
    expected.replace("https://", "http://"),
    expected.replace("https://", "https://synthetic@"),
    expected.replace("https://", "https://synthetic:synthetic@"),
    expected.replace("/info/v2/", "/info/v3/"),
    expected.replace("/info/v2/", "/info/v2"),
    "not a URL",
  ];
  for (const [key] of new URL(expected).searchParams) {
    invalid.push(
      withQuery((params) => params.delete(key)),
      withQuery((params) => params.set(key, "tampered")),
      withQuery((params) => params.append(key, params.get(key))),
    );
  }
  for (const url of invalid)
    assert.equal(
      await probeUrl(url),
      null,
      "unverified response URL must fail closed",
    );

  // Other providers retain exact full-URL matching. A Doubao-shaped response
  // would fail their identity parser anyway; assert URL rejection occurs
  // before the body is consumed in the separate fixture below.
  for (const [probe, providerOrigin, path] of [
    [
      probeDeepseekAccount,
      "https://chat.deepseek.com",
      "/api/v0/users/current",
    ],
    [probeGlmAccount, "https://chatglm.cn", "/chatglm/user-api/user/info"],
  ]) {
    let bodyReads = 0;
    const page = {
      evaluate: async (fn, args) =>
        runInNewContext(`(${fn.toString()})`, {
          location: { origin: providerOrigin },
          URL,
          AbortSignal,
          TextDecoder,
          localStorage: {
            getItem: () =>
              JSON.stringify({ __version: "0", value: "synthetic" }),
          },
          document: { cookie: "chatglm_token=synthetic" },
          fetch: async () => ({
            status: 200,
            url: `${providerOrigin}${path}?msToken=synthetic&a_bogus=synthetic`,
            headers: new Headers({ "content-type": "application/json" }),
            get body() {
              bodyReads++;
              return null;
            },
          }),
        })(args),
    };
    assert.equal(await probe(page), null);
    assert.equal(bodyReads, 0);
  }
});

for (const example of consumerCases) {
  test(`${example.provider} identity excludes guest/error/name-only records and private contact fallback`, () => {
    assert.deepEqual(example.identity(example.own()), {
      platform_account_id: "synthetic-account",
      display_name: "Synthetic user",
    });
    for (const body of [null, {}, ...example.invalid])
      assert.equal(example.identity(body), null);
    const nameless = example.own();
    if (example.provider === "deepseek")
      delete nameless.data.biz_data.id_profile;
    else delete nameless.data.name;
    assert.deepEqual(example.identity(nameless), {
      platform_account_id: "synthetic-account",
      display_name: "Account · ccount",
    });
  });

  test(`${example.provider} shared probe is bounded, scoped, resumable and transfers no credentials`, async () => {
    const requests = [];
    const state = {
      status: 200,
      type: "application/json",
      body: JSON.stringify(example.own()),
      redirect: false,
    };
    const server = createServer((request, response) => {
      if (request.url === "/" || request.url === "/favicon.ico") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end('<!doctype html><input id="draft" value="unsent">');
        return;
      }
      requests.push(request.url);
      assert.equal(
        request.url,
        example.path,
        "never follow redirect or invoke another endpoint",
      );
      assert.equal(request.method, "GET");
      if (example.provider === "deepseek")
        assert.equal(request.headers.authorization, "Bearer synthetic-token");
      else {
        assert.equal(request.headers["agw-js-conv"], "str");
        assert.match(
          request.headers.cookie ?? "",
          /synthetic_session=synthetic-cookie/,
        );
        assert.equal(request.headers.authorization, undefined);
      }
      response.writeHead(state.status, {
        "content-type": state.type,
        ...(state.redirect ? { location: "/must-not-follow" } : {}),
      });
      response.end(state.body);
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    let browser;
    try {
      browser = await chromium.launch(
        process.env.GEO_TEST_CHROMIUM_PATH
          ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
          : {},
      );
      const context = await browser.newContext();
      const page = await context.newPage();
      await page.goto(origin);
      const options = { trustedOrigin: origin };
      if (example.provider === "deepseek") {
        for (const value of [
          null,
          "bad-json",
          JSON.stringify({ value: "synthetic-token", __version: "unknown" }),
          JSON.stringify({ value: 1, __version: "0" }),
        ]) {
          await page.evaluate(
            (value) =>
              value === null
                ? localStorage.removeItem("userToken")
                : localStorage.setItem("userToken", value),
            value,
          );
          assert.equal(await example.probe(page, options), null);
        }
        assert.equal(requests.length, 0);
        await page.evaluate(() =>
          localStorage.setItem(
            "userToken",
            JSON.stringify({ value: "synthetic-token", __version: "0" }),
          ),
        );
      } else {
        await context.addCookies([
          {
            name: "synthetic_session",
            value: "synthetic-cookie",
            url: origin,
            httpOnly: true,
          },
        ]);
      }
      assert.equal(await example.probe(page), null);
      assert.equal(requests.length, 0);
      const guardedPage = {
        async evaluate(fn, args) {
          const projected = await page.evaluate(fn, args);
          assert.equal(
            JSON.stringify(projected)?.includes("synthetic-private"),
            false,
          );
          return projected;
        },
      };
      assert.deepEqual(
        await example.probe(guardedPage, options),
        example.identity(example.own()),
      );
      const restored = await browser.newContext({
        storageState: await context.storageState(),
      });
      const restoredPage = await restored.newPage();
      await restoredPage.goto(origin);
      assert.deepEqual(
        await example.probe(restoredPage, options),
        example.identity(example.own()),
      );
      await restored.close();
      const emptyPadding = { ...example.own(), padding: "" };
      const remaining =
        128_000 - Buffer.byteLength(JSON.stringify(emptyPadding));
      state.body = JSON.stringify({
        ...emptyPadding,
        padding: "x".repeat(remaining),
      });
      assert.equal(Buffer.byteLength(state.body), 128_000);
      assert.deepEqual(
        await example.probe(guardedPage, options),
        example.identity(example.own()),
      );
      state.body = JSON.stringify({
        ...emptyPadding,
        padding: "x".repeat(remaining + 1),
      });
      assert.equal(await example.probe(guardedPage, options), null);
      for (const invalid of [
        ...example.invalid.map((body) => ({ body: JSON.stringify(body) })),
        {
          body: JSON.stringify({
            ...example.own(),
            padding: "界".repeat(43_000),
          }),
        },
        { body: "{" },
        { type: "text/html" },
        { type: "application/json-invalid" },
        { status: 401 },
        { status: 403 },
        { status: 500 },
        { status: 302, redirect: true },
      ]) {
        Object.assign(
          state,
          {
            status: 200,
            type: "application/json",
            body: JSON.stringify(example.own()),
            redirect: false,
          },
          invalid,
        );
        assert.equal(await example.probe(guardedPage, options), null);
      }
      assert.equal(await page.locator("#draft").inputValue(), "unsent");
      assert.equal(page.url(), `${origin}/`);
      assert.equal(context.pages().length, 1);
      assert.deepEqual(adapters[example.provider].operations, []);
    } finally {
      await browser?.close();
      await new Promise((resolve) => server.close(resolve));
    }
  });
}

test("GLM identity requires a stable server ID and explicit non-guest success", () => {
  assert.deepEqual(glmIdentity(own()), {
    platform_account_id: "synthetic-account",
    display_name: "Synthetic user",
  });
  for (const data of [
    null,
    {},
    { ...own(), status: 401 },
    ...[undefined, null, true, "false"].map((is_guest) => ({
      ...own(),
      result: { ...own().result, is_guest },
    })),
    ...[undefined, null, "", " ", 123].map((_id) => ({
      ...own(),
      result: { ...own().result, _id },
    })),
    { ...own(), result: { ...own().result, username: "" } },
  ])
    assert.equal(glmIdentity(data), null);
  assert.deepEqual(adapters.glm.operations, []);
  assert.equal(adapters.glm.inspectMeasurementOptions, undefined);
});

test("GLM probe keeps credentials on the fixed-origin page and rejects unverified responses", async () => {
  const requests = [];
  const state = {
    status: 200,
    type: "application/json",
    body: JSON.stringify(own()),
    redirect: false,
  };
  const server = createServer((request, response) => {
    if (request.url === "/favicon.ico") {
      response.writeHead(204);
      response.end();
      return;
    }
    if (request.url === "/") {
      response.writeHead(200, { "content-type": "text/html" });
      response.end('<!doctype html><input id="draft" value="unsent">');
      return;
    }
    requests.push(request.url);
    if (request.url !== "/chatglm/user-api/user/info") {
      response.writeHead(500);
      response.end();
      return;
    }
    assert.equal(request.method, "GET");
    assert.equal(request.headers.authorization, "Bearer synthetic-token");
    assert.equal(request.headers["app-name"], "chatglm");
    response.writeHead(state.status, {
      "content-type": state.type,
      ...(state.redirect ? { location: "/redirect-must-not-run" } : {}),
    });
    response.end(state.body);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  let browser;
  try {
    browser = await chromium.launch(
      process.env.GEO_TEST_CHROMIUM_PATH
        ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
        : {},
    );
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.goto(origin);
    const options = { trustedOrigin: origin };
    assert.equal(await probeGlmAccount(page, options), null);
    assert.equal(requests.length, 0, "missing token cannot query identity");
    await context.addCookies([
      { name: "chatglm_token", value: "synthetic-token", url: origin },
    ]);
    assert.equal(await probeGlmAccount(page), null);
    assert.equal(
      requests.length,
      0,
      "wrong origin cannot read or send a token",
    );
    assert.deepEqual(await probeGlmAccount(page, options), glmIdentity(own()));
    const restored = await browser.newContext({
      storageState: await context.storageState(),
    });
    const restoredPage = await restored.newPage();
    await restoredPage.goto(origin);
    assert.deepEqual(
      await probeGlmAccount(restoredPage, options),
      glmIdentity(own()),
    );
    await restored.close();
    for (const example of [
      {
        body: JSON.stringify({
          ...own(),
          result: { ...own().result, is_guest: true },
        }),
      },
      { body: JSON.stringify({ ...own(), status: 40001 }) },
      {
        body: JSON.stringify({
          status: 0,
          result: { username: "Synthetic user", is_guest: false },
        }),
      },
      { body: JSON.stringify({ ...own(), padding: "界".repeat(43_000) }) },
      { body: '{"status":0' },
      { type: "text/html" },
      { type: "application/json-invalid" },
      { status: 401 },
      { status: 403 },
      { status: 500 },
      { status: 302, redirect: true },
    ]) {
      Object.assign(
        state,
        {
          status: 200,
          type: "application/json",
          body: JSON.stringify(own()),
          redirect: false,
        },
        example,
      );
      assert.equal(await probeGlmAccount(page, options), null);
    }
    assert.ok(requests.every((path) => path === "/chatglm/user-api/user/info"));
    assert.equal(await page.locator("#draft").inputValue(), "unsent");
    assert.equal(page.url(), `${origin}/`);
    assert.equal(context.pages().length, 1);
    assert.equal(
      (await adapters.glm.execute(page, "measure", {})).status,
      "unsupported",
    );
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
