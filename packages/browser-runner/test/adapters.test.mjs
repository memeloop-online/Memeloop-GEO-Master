import assert from "node:assert/strict";
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

let fixture;
let browser;
let context;
let page;
let origin;

before(async () => {
  fixture = createServer((request, response) => {
    const path = new URL(request.url, "http://localhost").pathname;
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
      response.end(`<!doctype html><html><body>
        <div class="WriteIndex-titleInput"><textarea></textarea></div>
        <div class="ProseMirror" contenteditable="true"></div>
        <button onclick="location.assign('/p/42')">发布</button>
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
