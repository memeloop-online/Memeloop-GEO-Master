import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import { adapters, glmIdentity, probeGlmAccount } from "../src/adapters.mjs";

const own = () => ({
  status: 0,
  result: {
    _id: "synthetic-account",
    username: "Synthetic user",
    is_guest: false,
  },
});

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
