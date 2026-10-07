import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { after, before, test } from "node:test";
import { chromium } from "playwright";
import {
  baiduIdentity,
  probeOwnAccount,
  publishBaidu,
  publishZhihu,
  readbackZhihu,
  xiaohongshuIdentity,
  zhihuIdentity,
} from "../src/adapters.mjs";
import { createRunner } from "../src/runner.mjs";

let fixture;
let browser;
let context;
let page;
let origin;
let zhihuSubmissions = 0;

before(async () => {
  fixture = createServer((request, response) => {
    const requestUrl = new URL(request.url, "http://localhost");
    const path = requestUrl.pathname;
    if (path === "/submit") {
      zhihuSubmissions++;
      response.writeHead(302, {
        location: requestUrl.searchParams.get("to") ?? "/p/42",
      });
      response.end();
      return;
    }
    if (path === "/self" || path === "/baidu/self") {
      if (!request.headers.cookie?.includes("sid=ready")) {
        response.writeHead(302, { location: "/signin" });
        response.end();
        return;
      }
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify(
          path === "/self"
            ? { id: "account-123", name: "Owner", url_token: "fixture-user" }
            : { data: { user: { userid: 123, name: "Owner" } } },
        ),
      );
      return;
    }
    if (path === "/badself") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ id: "unrelated", name: "" }));
      return;
    }
    if (path === "/api/v4/members/fixture-user/articles") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          data: [
            { id: 42, title: "Fixture title" },
            { id: 45, title: "Fixture title" },
          ],
        }),
      );
      return;
    }
    if (path === "/p/45" && !request.headers.cookie?.includes("sid=ready")) {
      response.writeHead(302, { location: "/signin" });
      response.end();
      return;
    }
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    if (path === "/write") {
      const destination = requestUrl.searchParams.get("to") ?? "/p/42";
      response.end(`<!doctype html><html><body>
        <div class="WriteIndex-titleInput"><textarea></textarea></div>
        <div class="ProseMirror" contenteditable="true"></div>
        <button onclick="location.assign('/submit?to=${encodeURIComponent(destination)}')">发布</button>
      </body></html>`);
    } else if (path === "/p/42") {
      response.end(
        "<!doctype html><html><body><h1>Fixture title</h1><article>Fixture body</article></body></html>",
      );
    } else if (path === "/p/43") {
      response.end(
        "<!doctype html><html><body><h1>Fixture title</h1><article>Unrelated body</article></body></html>",
      );
    } else if (path === "/p/45") {
      response.end(
        "<!doctype html><html><body><h1>Fixture title</h1><article>Fixture body</article></body></html>",
      );
    } else if (path === "/baidu/edit") {
      response.end(`<!doctype html><html><body>
        <input data-testid="news-title-input"><div id="body"></div>
        <button onclick="document.body.dataset.submitted='yes'">发布</button>
        <script>window.UE = {instants:{only:{setContent(html){
          document.querySelector('#body').innerHTML=html;
        }}}};</script>
      </body></html>`);
    } else if (path === "/baidu/modern") {
      response.end(`<!doctype html><html><body>
        <div class="FeEditorApp-title" contenteditable="true"></div>
        <div class="FeEditorApp-body" contenteditable="true"></div>
        <button onclick="document.body.dataset.submitted='yes'">发布</button>
      </body></html>`);
    } else if (path === "/new/note-manager") {
      response.end(`<!doctype html><html><body>
        <div class="main-container"><div class="user">
        <a href="/user/profile/creator-321"><span class="user-name">Own creator</span></a>
        </div></div></body></html>`);
    } else if (path === "/publish/lookalike") {
      response.end(`<!doctype html><html><body>
        <a href="/user/profile/unrelated">Public author</a></body></html>`);
    } else {
      response.end("<!doctype html><html><body>Login</body></html>");
    }
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
  await page.goto(origin);
});

after(async () => {
  await context?.close();
  await browser?.close();
  await new Promise((resolve) => fixture?.close(resolve));
});

test("self identity requires browser-context credentials and correct own JSON shape", async () => {
  assert.deepEqual(
    await probeOwnAccount(page, `${origin}/self`, zhihuIdentity),
    {
      platform_account_id: "account-123",
      display_name: "Owner",
    },
  );
  assert.deepEqual(
    await probeOwnAccount(page, `${origin}/baidu/self`, baiduIdentity),
    {
      platform_account_id: "123",
      display_name: "Owner",
    },
  );
  assert.equal(
    await probeOwnAccount(page, `${origin}/badself`, zhihuIdentity),
    null,
  );
  const anonymous = await browser.newContext();
  try {
    const anonymousPage = await anonymous.newPage();
    assert.equal(
      await probeOwnAccount(anonymousPage, `${origin}/self`, zhihuIdentity),
      null,
    );
  } finally {
    await anonymous.close();
  }
});

test("creator dashboard identity never accepts an unrelated public author link", async () => {
  await page.goto(`${origin}/new/note-manager`);
  assert.equal(await xiaohongshuIdentity(page), null);
  assert.deepEqual(await xiaohongshuIdentity(page, { trustedOrigin: origin }), {
    platform_account_id: "creator-321",
    display_name: "Own creator",
  });
  await page.goto(`${origin}/publish/lookalike`);
  assert.equal(
    await xiaohongshuIdentity(page, { trustedOrigin: origin }),
    null,
  );
});

test("source-derived editor can complete only after exact public readback and own-list match", async () => {
  const submissions = zhihuSubmissions;
  const result = await publishZhihu(
    page,
    { title: "Fixture title", body: "Fixture body" },
    {
      editorUrl: `${origin}/write`,
      postOrigin: origin,
      selfUrl: `${origin}/self`,
      articlesOrigin: origin,
    },
  );
  assert.equal(result.status, "completed", JSON.stringify(result));
  assert.equal(result.public_url, `${origin}/p/42`);
  assert.equal(
    result.evidence[0].expected_sha256,
    result.evidence[0].readback_sha256,
  );
  assert.equal(result.evidence[0].owned_by_account, true);
  assert.equal(zhihuSubmissions, submissions + 1);
  assert.equal(
    result.evidence.some((item) => item.kind === "publication_candidate"),
    false,
  );
  assert.ok(Date.parse(result.occurred_at));
  assert.equal(
    (
      await readbackZhihu(
        page,
        `${origin}/p/43`,
        { title: "Fixture title", body: "Fixture body" },
        {
          postOrigin: origin,
          selfUrl: `${origin}/self`,
          articlesOrigin: origin,
        },
      )
    ).status,
    "unknown",
  );
  assert.equal(
    (
      await readbackZhihu(
        page,
        `${origin}/p/45`,
        { title: "Fixture title", body: "Fixture body" },
        {
          postOrigin: origin,
          selfUrl: `${origin}/self`,
          articlesOrigin: origin,
        },
      )
    ).status,
    "unknown",
    "account-only readback is not public proof",
  );
  assert.equal(
    (
      await readbackZhihu(
        page,
        `${origin}/p/42`,
        { title: "Fixture title", body: "Fixture body" },
        {
          postOrigin: origin,
          selfUrl: `${origin}/self`,
          articlesOrigin: origin,
          expectedAccountId: "different-account",
        },
      )
    ).status,
    "unknown",
    "a different authenticated account cannot own the receipt",
  );
});

test("post-submit public navigation leaves a typed unverified candidate when readback fails", async () => {
  const submissions = zhihuSubmissions;
  const before = Date.now();
  const result = await publishZhihu(
    page,
    { title: "Fixture title", body: "Fixture body" },
    {
      editorUrl: `${origin}/write?to=${encodeURIComponent("/p/43")}`,
      postOrigin: origin,
    },
  );
  const after = Date.now();
  assert.equal(result.status, "unknown");
  assert.equal(result.reason, "public_content_mismatch");
  assert.equal(result.stage, "readback");
  assert.equal(Object.hasOwn(result, "public_url"), false);
  assert.equal(zhihuSubmissions, submissions + 1);
  assert.equal(result.evidence.length, 1);
  const candidate = result.evidence[0];
  assert.deepEqual(Object.keys(candidate).sort(), [
    "expected_sha256",
    "kind",
    "observed_at",
    "schema_version",
    "source",
    "url",
  ]);
  assert.equal(candidate.kind, "publication_candidate");
  assert.equal(candidate.schema_version, "geo.publication.candidate.v1");
  assert.equal(candidate.url, `${origin}/p/43`);
  assert.equal(
    candidate.expected_sha256,
    createHash("sha256")
      .update("Fixture title\nFixture body", "utf8")
      .digest("hex"),
  );
  assert.equal(candidate.source, "post_submit_navigation");
  assert.equal(result.occurred_at, candidate.observed_at);
  assert.ok(Date.parse(candidate.observed_at) >= before);
  assert.ok(Date.parse(candidate.observed_at) <= after);

  const lookup = await readbackZhihu(
    page,
    `${origin}/p/43`,
    { title: "Fixture title", body: "Fixture body" },
    { postOrigin: origin },
  );
  assert.equal(lookup.status, "unknown");
  assert.deepEqual(lookup.evidence, [], "lookup never invents submit evidence");
  assert.equal(zhihuSubmissions, submissions + 1, "lookup never resubmits");
});

test("runner preserves the unknown candidate observation time without upgrading provenance", async () => {
  const submissions = zhihuSubmissions;
  const runner = createRunner({
    browserType: {
      async launch() {
        return {
          newContext: (options) => browser.newContext(options),
          async close() {},
        };
      },
    },
    platformAdapters: {
      fixture: {
        connectorVersion: "fixture.source_derived.v1",
        entry: `${origin}/write`,
        operations: ["publish"],
        async identify() {
          return {
            platform_account_id: "fixture-account",
            display_name: "Fixture owner",
          };
        },
        execute: (runnerPage, _operation, payload, network) =>
          publishZhihu(runnerPage, payload, {
            ...network,
            editorUrl: `${origin}/write?to=${encodeURIComponent("/p/43")}`,
            postOrigin: origin,
          }),
      },
    },
  });
  try {
    await runner.create({
      session_id: "candidate",
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    });
    await runner.complete("candidate");
    const result = await runner.execute({
      execution_id: "candidate-execution",
      session_id: "candidate",
      operation: "publish",
      payload: { title: "Fixture title", body: "Fixture body" },
    });
    assert.equal(result.status, "unknown");
    assert.equal(result.provenance, "fixture");
    assert.equal(result.connector_version, "fixture.source_derived.v1");
    assert.equal(Object.hasOwn(result, "public_url"), false);
    assert.equal(result.evidence[0].kind, "publication_candidate");
    assert.equal(result.occurred_at, result.evidence[0].observed_at);
    assert.equal(zhihuSubmissions, submissions + 1);
  } finally {
    await runner.shutdown();
  }
});

test("post-submit navigation cannot preserve query or fragment as candidate evidence", async () => {
  const submissions = zhihuSubmissions;
  for (const destination of ["/p/43?tracking=opaque", "/p/43#section"]) {
    const result = await publishZhihu(
      page,
      { title: "Fixture title", body: "Fixture body" },
      {
        editorUrl: `${origin}/write?to=${encodeURIComponent(destination)}`,
        postOrigin: origin,
      },
    );
    assert.equal(result.status, "unknown");
    assert.equal(Object.hasOwn(result, "public_url"), false);
    assert.deepEqual(result.evidence, [], destination);
  }
  assert.equal(zhihuSubmissions, submissions + 2);
});

test("candidate requires the fixed production origin and a credential-free canonical path", async () => {
  for (const [destination, postOrigin] of [
    [
      "https://example:placeholder@zhuanlan.zhihu.com/p/42",
      "https://zhuanlan.zhihu.com",
    ],
    ["https://zhuanlan.zhihu.com/p/42/", "https://zhuanlan.zhihu.com"],
    ["https://zhuanlan.zhihu.com:8443/p/42", "https://zhuanlan.zhihu.com:8443"],
    [
      "https://not-the-platform.example/p/42",
      "https://not-the-platform.example",
    ],
  ]) {
    let currentUrl = "https://zhuanlan.zhihu.com/write";
    let clicks = 0;
    const field = {
      first() {
        return this;
      },
      async isVisible() {
        return true;
      },
      async fill() {},
    };
    const simulatedPage = {
      async goto() {},
      url: () => currentUrl,
      locator: () => field,
      getByRole: () => ({
        async click() {
          clicks++;
          currentUrl = destination;
        },
      }),
      async waitForURL(predicate) {
        if (!predicate(new URL(currentUrl))) throw new Error("not a post");
      },
      context: () => ({
        browser: () => ({
          async newContext() {
            throw new Error("public readback unavailable");
          },
        }),
      }),
    };
    const result = await publishZhihu(
      simulatedPage,
      {
        title: "Fixture title",
        body: "Fixture body",
      },
      { postOrigin },
    );
    assert.equal(result.status, "unknown");
    assert.equal(Object.hasOwn(result, "public_url"), false);
    assert.deepEqual(result.evidence, [], destination);
    assert.equal(clicks, 1, "candidate validation cannot retry the submit");
  }
});

test("non-public post-submit navigation stays unknown without candidate or another submit", async () => {
  const submissions = zhihuSubmissions;
  const result = await publishZhihu(
    page,
    { title: "Fixture title", body: "Fixture body" },
    {
      editorUrl: `${origin}/write?to=${encodeURIComponent("/draft/42")}`,
      postOrigin: origin,
    },
  );
  assert.equal(result.status, "unknown");
  assert.equal(result.reason, "submission_outcome_unverified");
  assert.equal(Object.hasOwn(result, "public_url"), false);
  assert.deepEqual(result.evidence, []);
  assert.equal(zhihuSubmissions, submissions + 1);
});

test("moderated creator submit remains unknown, never verified from a click", async () => {
  const result = await publishBaidu(
    page,
    { title: "Fixture title", body: "Fixture <body>" },
    { editorUrl: `${origin}/baidu/edit` },
  );
  assert.equal(result.status, "unknown");
  assert.equal(result.stage, "submit", JSON.stringify(result));
  assert.equal(await page.locator("#body").innerText(), "Fixture <body>");
  assert.equal(
    await page.locator("body").getAttribute("data-submitted"),
    "yes",
  );
  const modern = await publishBaidu(
    page,
    { title: "Modern title", body: "Modern body" },
    { editorUrl: `${origin}/baidu/modern` },
  );
  assert.equal(modern.status, "unknown");
  assert.equal(
    await page.locator(".FeEditorApp-body").innerText(),
    "Modern body",
  );
  assert.equal(
    await page.locator("body").getAttribute("data-submitted"),
    "yes",
  );
});
