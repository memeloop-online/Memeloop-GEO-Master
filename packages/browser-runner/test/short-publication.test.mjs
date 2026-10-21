import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { after, before, beforeEach, test } from "node:test";
import { chromium } from "playwright";
import {
  lookupShortPost,
  publishShortPost,
  withdrawShortPost,
} from "../src/short-publication.mjs";

let server;
let browser;
let context;
let page;
let origin;
let mode;
let submits;
let deleteClicks;
let confirmations;
let decoyConfirmations;
let deleteRequests;
let deleted;
let pendingDelete;
let pendingCreate;
let createCalls;
let ownReads;
let publicCookies;
const BODY = "Synthetic thought body\nSecond paragraph";
const escape = (text) =>
  text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");

function createPayload() {
  const data = {
    publish: { traceId: "synthetic-trace" },
    commentsPermission: { comment_permission: "all" },
    extra_info: { view_permission: "all", publisher: "pc" },
    draft: { disabled: 1 },
    hybrid: {
      html: BODY.split("\n")
        .map((line) => `<p>${escape(line)}</p>`)
        .join(""),
      textLength: BODY.length,
    },
  };
  if (mode === "edit-request") data.draft.isPublished = true;
  if (mode === "existing-draft-id") data.draft.id = "42";
  if (mode === "scheduled-request") data.schedule = { publishAt: 1234567890 };
  if (mode === "wrong-request-body")
    data.hybrid.html = "<p>Other synthetic body</p>";
  return { action: mode === "wrong-action" ? "article" : "pin", data };
}

function createResponse() {
  const id =
    mode === "wrong-created-id" ||
    (mode === "old-response-with-new-create" && createCalls > 1)
      ? "99"
      : "42";
  const created = {
    id:
      mode === "numeric-created-id"
        ? 42
        : mode === "invalid-created-id"
          ? "42/../99"
          : mode === "zero-created-id"
            ? "0"
            : mode === "long-created-id"
              ? "1".repeat(41)
              : id,
    author: {
      id: mode === "wrong-created-author" ? "other-owner" : "synthetic-account",
      url_token: "synthetic-owner",
    },
  };
  return mode === "wrong-create-envelope"
    ? created
    : {
        data: {
          result: JSON.stringify(created),
          ...(mode === "write-blocked" ? { write: { instruction: true } } : {}),
        },
      };
}

function card(id = "42", body = BODY, token = "synthetic-owner") {
  return `<section class="PinItem" data-fixture-id="${id}">
    <div class="PinItem-authorInfo"><a class="UserLink-link" href="/people/${token}">Synthetic author</a></div>
    <div class="RichContent"><div><div class="RichContent-inner"><div><div><span class="RichText ztext CopyrightRichText-richText">${escape(body).replaceAll("\n", "<br>")}</span></div></div></div></div>
    <div class="PinItem-remainContentRichText">${mode === "nonempty-remainder" ? "Additional synthetic content" : ""}</div>
    <a class="ContentItem-time" href="/pin/${id}">Synthetic timestamp</a>
    <div class="PinToolbar-actions"><button aria-label="设置" aria-haspopup="true" onclick="openMenu('${id}')">Settings</button></div>
    </div>
  </section>`;
}

before(async () => {
  server = createServer(async (request, response) => {
    const url = new URL(request.url, origin ?? "http://localhost");
    if (url.pathname === "/api/v4/me") {
      ownReads++;
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          id:
            mode === "account-switch" && ownReads > 1
              ? "other-owner"
              : "synthetic-account",
          name: "Synthetic owner",
          url_token: "synthetic-owner",
        }),
      );
      return;
    }
    if (url.pathname === "/fixture-submit") {
      submits++;
      pendingCreate?.writeHead(200, { "content-type": "application/json" });
      pendingCreate?.end(JSON.stringify(createResponse()));
      pendingCreate = undefined;
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ ok: true }));
      return;
    }
    if (url.pathname === "/fixture-create-redirect") {
      response.writeHead(307, { location: "/api/v4/content/publish" });
      response.end();
      return;
    }
    if (["/api/v4/content/publish", "/fixture-create"].includes(url.pathname)) {
      createCalls++;
      if (
        [
          "preexisting-create-response",
          "old-response-with-new-create",
        ].includes(mode) &&
        createCalls === 1
      ) {
        pendingCreate = response;
        return;
      }
      if (
        mode === "redirect-create" &&
        url.pathname === "/api/v4/content/publish"
      ) {
        response.writeHead(302, { location: "/fixture-create" });
        response.end();
        return;
      }
      response.writeHead(mode === "failed-create" ? 500 : 200, {
        "content-type": "application/json",
      });
      response.end(
        mode === "invalid-create-json"
          ? "invalid"
          : JSON.stringify(createResponse()),
      );
      return;
    }
    if (url.pathname.startsWith("/api/v4/pins/")) {
      deleteRequests.push({
        id: url.pathname.split("/").at(-1),
        method: request.method,
      });
      deleted = mode !== "failed-delete";
      if (mode === "preexisting-delete-response") {
        pendingDelete = response;
        return;
      }
      response.writeHead(mode === "failed-delete" ? 500 : 204);
      response.end();
      return;
    }
    if (url.pathname === "/fixture-delete") {
      deleteClicks.push(url.searchParams.get("id"));
      response.writeHead(200);
      response.end();
      return;
    }
    if (url.pathname === "/fixture-confirm") {
      confirmations++;
      pendingDelete?.writeHead(204);
      pendingDelete?.end();
      pendingDelete = undefined;
      response.writeHead(200);
      response.end();
      return;
    }
    if (url.pathname === "/fixture-decoy") {
      decoyConfirmations++;
      response.writeHead(200);
      response.end();
      return;
    }
    response.writeHead(
      (mode === "public-404" ||
        mode === "no-navigation-unreadable" ||
        deleted ||
        [
          "failed-delete",
          "wrong-delete-id",
          "wrong-delete-method",
          "missing-delete-response",
          "optimistic-absence",
        ].includes(mode)) &&
        !request.headers.cookie &&
        url.pathname.startsWith("/pin/")
        ? 404
        : 200,
      { "content-type": "text/html; charset=utf-8" },
    );
    if (url.pathname === "/") {
      const existing = mode === "preexisting" ? card() : "";
      const draft = mode === "existing-draft" ? "Synthetic unsent draft" : "";
      response.end(`<!doctype html><html><body>
        ${existing}<button onclick="openComposer()">发想法</button><div id="composer"></div>
        <script>
          ${["preexisting-create-response", "old-response-with-new-create"].includes(mode) ? `fetch('/api/v4/content/publish', {method:'POST',headers:{'Content-Type':'application/json'},body:${JSON.stringify(JSON.stringify(createPayload()))}});` : ""}
          function openComposer() {
            setTimeout(() => {
              document.querySelector('#composer').innerHTML = '<div class="WritePinV2-Form"><div class="TitleArea"><textarea></textarea></div><div class="public-DraftEditor-content" contenteditable="true">${draft}</div><div class="WritePinToolbar"><button onclick="submitThought()">发布</button></div></div>';
            }, 40);
          }
          async function submitThought() {
            await fetch('/fixture-submit', {method:'POST'});
            ${mode === "early-navigation" ? `fetch('/api/v4/content/publish', {method:'POST',headers:{'Content-Type':'application/json'},body:${JSON.stringify(JSON.stringify(createPayload()))}}).then(() => location.assign('/pin/42')); return;` : ""}
            ${["old-navigation", "preexisting-create-response", "no-candidate", "insert-item", "preexisting"].includes(mode) ? "" : `await fetch(${JSON.stringify(mode === "wrong-create-endpoint" ? "/fixture-create" : mode === "redirected-create-request" ? "/fixture-create-redirect" : "/api/v4/content/publish")}, {method:${JSON.stringify(mode === "wrong-create-method" ? "PUT" : "POST")},headers:{'Content-Type':'application/json'},body:${JSON.stringify(JSON.stringify(createPayload()))}}).then(response => response.json()).catch(() => null);`}
            ${mode === "duplicate-create" ? `await fetch('/api/v4/content/publish', {method:'POST',headers:{'Content-Type':'application/json'},body:${JSON.stringify(JSON.stringify(createPayload()))}}).then(response => response.json());` : ""}
            ${["no-candidate", "no-navigation", "no-navigation-unreadable"].includes(mode) ? "return;" : mode === "insert-item" || mode === "preexisting" ? `document.body.insertAdjacentHTML('beforeend', ${JSON.stringify(card())});` : "location.assign('/pin/42');"}
          }
        </script></body></html>`);
      return;
    }
    if (url.pathname.startsWith("/pin/")) {
      if (!request.headers.cookie)
        publicCookies.push(request.headers.cookie ?? null);
      const body = mode === "body-mismatch" ? "Different synthetic body" : BODY;
      const token =
        mode === "foreign-author" ? "synthetic-stranger" : "synthetic-owner";
      const id =
        mode === "wrong-item-id" ? "99" : url.pathname.split("/").at(-1);
      response.end(`<!doctype html><html><body>
        ${deleted && !request.headers.cookie ? "" : card(id, body, token)}
        ${mode === "ambiguous-item" ? card("99") : ""}
        <div class="ConfirmModal" role="dialog"><h2>Unrelated synthetic confirmation</h2><button onclick="fetch('/fixture-decoy')">确认</button></div>
        ${mode === "preexisting-confirmation" ? '<div class="ConfirmModal"><h2>你确定要删除自己的想法吗？</h2><button>确认</button></div>' : ""}
        <script>
          ${mode === "preexisting-delete-response" ? "fetch('/api/v4/pins/42', {method:'DELETE'});" : ""}
          function openMenu(id) {
            const menu = document.createElement('div');
            menu.className = 'Popover-content PinToolbar-menuContainer';
            const remove = document.createElement('button');
            remove.textContent = '删除';
            remove.onclick = () => deleteThought(id);
            menu.append(remove);
            document.body.append(menu);
          }
          async function deleteThought(id) {
            await fetch('/fixture-delete?id=' + id, {method:'POST'});
            ${mode === "optimistic-absence" ? "document.querySelector('.PinItem').remove(); document.body.insertAdjacentHTML('beforeend','<div>想法已删除</div>'); return;" : ""}
            const dialog = document.createElement('div');
            dialog.className = 'ConfirmModal';
            dialog.setAttribute('role','dialog');
            const title = document.createElement('h2');
            title.textContent = ${JSON.stringify(mode === "wrong-confirmation" ? "Unrecognized synthetic title" : "你确定要删除自己的想法吗？")};
            const confirm = document.createElement('button');
            confirm.textContent = '确认';
            confirm.onclick = async () => {
              dialog.remove();
              await fetch('/fixture-confirm', {method:'POST'});
              document.querySelector('.PinItem')?.remove();
              document.body.insertAdjacentHTML('beforeend','<div>想法已删除</div>');
              ${["missing-delete-response", "preexisting-delete-response"].includes(mode) ? "return;" : ""}
              await fetch('/api/v4/pins/' + ${mode === "wrong-delete-id" ? "'99'" : "id"}, {method:${JSON.stringify(mode === "wrong-delete-method" ? "POST" : "DELETE")}});
            };
            dialog.append(title, confirm);
            document.body.append(dialog);
            ${mode === "ambiguous-confirmation" ? "document.body.append(dialog.cloneNode(true));" : ""}
          }
        </script></body></html>`);
      return;
    }
    response.end("<!doctype html><body>Unrelated fixture page</body>");
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch(
    process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {},
  );
});

beforeEach(async () => {
  await context?.close();
  context = await browser.newContext();
  await context.addCookies([
    { name: "sid", value: "synthetic-session", url: origin },
  ]);
  page = await context.newPage();
  mode = "navigation";
  submits = 0;
  deleteClicks = [];
  confirmations = 0;
  decoyConfirmations = 0;
  deleteRequests = [];
  deleted = false;
  pendingDelete = undefined;
  pendingCreate = undefined;
  createCalls = 0;
  ownReads = 0;
  publicCookies = [];
});

after(async () => {
  await context?.close();
  await browser?.close();
  await new Promise((resolve) => server?.close(resolve));
});

function network(timeoutMs = 2_000) {
  return {
    postOrigin: origin,
    expectedAccountId: "synthetic-account",
    timeoutMs,
  };
}

test("body-only publish waits for the asynchronously mounted composer and submits once", async () => {
  const outcome = await publishShortPost(page, { body: BODY }, network());
  assert.equal(outcome.status, "completed", JSON.stringify(outcome));
  assert.equal(outcome.external_id, "42");
  assert.equal(outcome.public_url, `${origin}/pin/42`);
  assert.equal(submits, 1);
  assert.equal(outcome.evidence[0].source, "post_submit_navigation");
  assert.deepEqual(Object.keys(outcome.evidence[0]).sort(), [
    "expected_sha256",
    "kind",
    "observed_at",
    "schema_version",
    "source",
    "url",
  ]);
  assert.equal(
    outcome.evidence[1].expected_sha256,
    createHash("sha256")
      .update("\nSynthetic thought body Second paragraph")
      .digest("hex"),
  );
  assert.deepEqual(publicCookies, [null]);
  const created = outcome.evidence[2];
  assert.ok(created);
  assert.deepEqual(Object.keys(created).sort(), [
    "expected_sha256",
    "external_id",
    "http_status",
    "kind",
    "observed_at",
    "request_method",
    "request_url",
    "schema_version",
    "url",
  ]);
  assert.equal(created.kind, "publication_created");
  assert.equal(created.schema_version, "geo.publication.created.v1");
  assert.equal(created.external_id, "42");
  assert.equal(created.request_method, "POST");
  assert.equal(created.request_url, `${origin}/api/v4/content/publish`);
  assert.equal(created.url, outcome.public_url);
  assert.equal(created.expected_sha256, outcome.evidence[0].expected_sha256);
  assert.equal(created.http_status, 200);
  assert.equal(page.listenerCount("request"), 0);
});

test("old identical navigation and an earlier request response remain read-only candidates", async () => {
  for (const testMode of ["old-navigation", "preexisting-create-response"]) {
    mode = testMode;
    const outcome = await publishShortPost(page, { body: BODY }, network(250));
    assert.equal(outcome.evidence[0]?.kind, "publication_candidate", testMode);
    assert.equal(outcome.evidence[1]?.content_matched, true, testMode);
    assert.equal(
      outcome.evidence.some((entry) => entry.kind === "publication_created"),
      false,
      testMode,
    );
    assert.deepEqual(deleteClicks, []);
    assert.equal(page.listenerCount("request"), 0);
  }
});

test("creation response is retained before immediate navigation without resending", async () => {
  mode = "early-navigation";
  const outcome = await publishShortPost(page, { body: BODY }, network());
  assert.equal(outcome.status, "completed");
  assert.equal(outcome.evidence[2]?.kind, "publication_created");
  assert.equal(submits, 1);
  assert.equal(createCalls, 1);
  assert.equal(page.listenerCount("request"), 0);
});

test("unavailable browser response capture prevents submission", async () => {
  const createSession = context.newCDPSession.bind(context);
  context.newCDPSession = (target) =>
    target === page
      ? Promise.reject(new Error("Synthetic capture unavailable"))
      : createSession(target);
  try {
    const outcome = await publishShortPost(page, { body: BODY }, network());
    assert.equal(outcome.status, "unsupported");
    assert.equal(outcome.reason, "short_post_create_capture_unavailable");
    assert.equal(
      outcome.evidence.some((entry) => entry.kind === "publication_created"),
      false,
    );
    assert.equal(createCalls, 0);
    assert.equal(submits, 0);
    assert.equal(page.listenerCount("request"), 0);
  } finally {
    context.newCDPSession = createSession;
  }
});

test("unsupported or editing request payloads are stopped before the backend", async () => {
  for (const testMode of [
    "wrong-create-method",
    "redirected-create-request",
    "edit-request",
    "existing-draft-id",
    "scheduled-request",
    "wrong-action",
    "wrong-request-body",
  ]) {
    mode = testMode;
    createCalls = 0;
    const outcome = await publishShortPost(page, { body: BODY }, network(350));
    assert.equal(outcome.status, "unsupported", testMode);
    assert.equal(
      outcome.reason,
      "short_post_create_request_unverified",
      testMode,
    );
    assert.equal(createCalls, 0, testMode);
    assert.equal(outcome.evidence.length, 0, testMode);
    assert.equal(page.listenerCount("request"), 0, testMode);
  }
});

test("creation proof survives a separate anonymous readback failure", async () => {
  for (const testMode of ["public-404", "body-mismatch", "foreign-author"]) {
    mode = testMode;
    createCalls = 0;
    const outcome = await publishShortPost(page, { body: BODY }, network());
    assert.equal(outcome.status, "unknown", testMode);
    assert.equal(createCalls, 1, testMode);
    assert.equal(outcome.evidence[0]?.kind, "publication_candidate", testMode);
    assert.equal(outcome.evidence[1]?.kind, "publication_created", testMode);
  }
});

test("the exact created response ID wins over unrelated older navigation", async () => {
  for (const testMode of ["wrong-created-id", "old-response-with-new-create"]) {
    mode = testMode;
    createCalls = 0;
    const outcome = await publishShortPost(page, { body: BODY }, network());
    const created = outcome.evidence.find(
      (entry) => entry.kind === "publication_created",
    );
    assert.equal(outcome.external_id, "99", testMode);
    assert.equal(outcome.public_url, `${origin}/pin/99`, testMode);
    assert.equal(created?.external_id, "99", testMode);
    assert.equal(created?.url, `${origin}/pin/99`, testMode);
    assert.equal(outcome.evidence[0]?.url, `${origin}/pin/42`, testMode);
    assert.deepEqual(deleteClicks, [], testMode);
  }
});

test("a valid response creates authority without navigation or public readability", async () => {
  for (const testMode of [
    "no-navigation",
    "no-navigation-unreadable",
    "duplicate-create",
  ]) {
    mode = testMode;
    createCalls = 0;
    const outcome = await publishShortPost(page, { body: BODY }, network());
    const created = outcome.evidence.find(
      (entry) => entry.kind === "publication_created",
    );
    assert.equal(created?.external_id, "42", testMode);
    assert.equal(created?.url, `${origin}/pin/42`, testMode);
    assert.equal(createCalls, 1, testMode);
    assert.equal(
      outcome.status,
      testMode === "no-navigation-unreadable" ? "unknown" : "completed",
      testMode,
    );
    assert.equal(
      outcome.evidence.some((entry) => entry.kind === "publication_candidate"),
      false,
      testMode,
    );
  }
});

test("creation authority requires the exact fresh request, wire envelope, ID, author, and readback", async () => {
  for (const testMode of [
    "wrong-create-endpoint",
    "zero-created-id",
    "invalid-created-id",
    "long-created-id",
    "wrong-created-author",
    "numeric-created-id",
    "wrong-create-envelope",
    "invalid-create-json",
    "failed-create",
    "redirect-create",
    "write-blocked",
  ]) {
    mode = testMode;
    createCalls = 0;
    const outcome = await publishShortPost(page, { body: BODY }, network(350));
    assert.equal(outcome.evidence[0]?.kind, "publication_candidate", testMode);
    assert.equal(
      outcome.evidence.some((entry) => entry.kind === "publication_created"),
      false,
      testMode,
    );
    assert.equal(page.listenerCount("request"), 0, testMode);
    assert.deepEqual(deleteClicks, [], testMode);
  }
});

test("matching preview with additional content is neither verified nor deleted", async () => {
  mode = "nonempty-remainder";
  const payload = { external_id: "42", body: BODY };
  const lookup = await lookupShortPost(page, payload, network());
  assert.equal(lookup.status, "unknown");
  const withdrawal = await withdrawShortPost(page, payload, network());
  assert.notEqual(withdrawal.status, "completed");
  assert.deepEqual(deleteClicks, []);
});

test("newly visible matching feed item alone cannot attribute a submission", async () => {
  mode = "insert-item";
  const outcome = await publishShortPost(page, { body: BODY }, network(200));
  assert.equal(outcome.status, "unknown", JSON.stringify(outcome));
  assert.equal(outcome.reason, "submission_outcome_unverified");
  assert.equal(submits, 1);
});

test("unknown submit outcome is never retried or replaced by a historical match", async () => {
  for (const testMode of ["no-candidate", "preexisting"]) {
    mode = testMode;
    submits = 0;
    const outcome = await publishShortPost(page, { body: BODY }, network(200));
    assert.equal(outcome.status, "unknown");
    assert.equal(outcome.reason, "submission_outcome_unverified");
    assert.equal(submits, 1);
  }
});

test("nonempty title and unrecognized fields do not silently change posted text", async () => {
  for (const payload of [
    { title: "Internal asset title", body: BODY },
    { body: BODY, article: true },
  ]) {
    const outcome = await publishShortPost(page, payload, network());
    assert.equal(outcome.status, "unsupported");
  }
  assert.equal(submits, 0);
  assert.equal(ownReads, 0);
});

test("account change before submit leaves the result unknown without sending", async () => {
  mode = "account-switch";
  const outcome = await publishShortPost(page, { body: BODY }, network());
  assert.equal(outcome.reason, "account_ownership_unverified");
  assert.equal(submits, 0);
});

test("an existing unsent draft is preserved without submitting or replacing it", async () => {
  mode = "existing-draft";
  const outcome = await publishShortPost(page, { body: BODY }, network());
  assert.equal(outcome.reason, "short_post_existing_draft");
  assert.equal(submits, 0);
  assert.equal(
    await page.locator(".public-DraftEditor-content").innerText(),
    "Synthetic unsent draft",
  );
});

test("lookup reads only the exact numeric ID anonymously and requires exact author and body", async () => {
  for (const testMode of [
    "navigation",
    "body-mismatch",
    "foreign-author",
    "ambiguous-item",
    "public-404",
  ]) {
    mode = testMode;
    const outcome = await lookupShortPost(
      page,
      { external_id: "42", body: BODY },
      network(),
    );
    assert.equal(
      outcome.status,
      testMode === "navigation" ? "completed" : "unknown",
      testMode,
    );
  }
  assert.ok(publicCookies.every((cookie) => cookie === null));
  assert.equal(submits, 0);
  assert.deepEqual(deleteClicks, []);
});

test("invalid ID and untrusted origin are rejected before any browser request", async () => {
  for (const external_id of [
    "",
    "0",
    "42/../99",
    "https://untrusted.invalid/pin/42",
    42,
  ]) {
    assert.equal(
      (await lookupShortPost(page, { external_id, body: BODY }, network()))
        .status,
      "unsupported",
    );
  }
  assert.equal(
    (
      await lookupShortPost(
        page,
        { external_id: "42", body: BODY },
        { postOrigin: "https://untrusted.invalid" },
      )
    ).status,
    "unsupported",
  );
  assert.equal(ownReads, 0);
});

test("withdrawal confirms only the exact item and known modal, then correlates DELETE and anonymous absence", async () => {
  const outcome = await withdrawShortPost(
    page,
    { external_id: "42", body: BODY },
    network(),
  );
  assert.equal(outcome.status, "completed", JSON.stringify(outcome));
  assert.deepEqual(deleteClicks, ["42"]);
  assert.equal(confirmations, 1);
  assert.equal(decoyConfirmations, 0);
  assert.deepEqual(deleteRequests, [{ id: "42", method: "DELETE" }]);
  assert.deepEqual(publicCookies, [null]);
  assert.equal(outcome.external_id, "42");
  assert.equal(outcome.evidence[0].withdrawal_verified, true);
  assert.equal(outcome.evidence[1].http_status, 204);
  assert.equal(outcome.evidence[2].absence_verified, true);
});

test("unknown, pre-existing, or ambiguous confirmations are never clicked", async () => {
  for (const testMode of [
    "wrong-confirmation",
    "preexisting-confirmation",
    "ambiguous-confirmation",
  ]) {
    mode = testMode;
    const outcome = await withdrawShortPost(
      page,
      { external_id: "42", body: BODY },
      network(200),
    );
    assert.equal(outcome.status, "unknown", testMode);
    assert.equal(confirmations, 0, testMode);
    assert.equal(decoyConfirmations, 0, testMode);
    assert.deepEqual(deleteRequests, [], testMode);
  }
});

test("failed, absent, or mismatched DELETE cannot be rescued by optimistic DOM, toast, or public 404", async () => {
  for (const testMode of [
    "failed-delete",
    "wrong-delete-id",
    "wrong-delete-method",
    "missing-delete-response",
    "optimistic-absence",
    "preexisting-delete-response",
  ]) {
    mode = testMode;
    deleted = false;
    const outcome = await withdrawShortPost(
      page,
      { external_id: "42", body: BODY },
      network(250),
    );
    assert.equal(outcome.status, "unknown", testMode);
    assert.equal(outcome.evidence[0].withdrawal_verified, false, testMode);
    assert.equal(
      outcome.evidence.some((entry) => entry.kind === "public_absence"),
      false,
      testMode,
    );
  }
  assert.deepEqual(publicCookies, []);
});

test("successful deletion response still requires exact anonymous absence", async () => {
  await page.route("**/api/v4/pins/42", (route) =>
    route.fulfill({ status: 204 }),
  );
  const outcome = await withdrawShortPost(
    page,
    { external_id: "42", body: BODY },
    network(),
  );
  assert.equal(outcome.status, "unknown");
  assert.equal(outcome.reason, "short_post_absence_unverified");
  assert.equal(outcome.evidence[1].http_status, 204);
  assert.equal(outcome.evidence[2].http_status, 200);
  assert.deepEqual(publicCookies, [null]);
});

test("foreign author, wrong body, and ambiguous items never reach a destructive control", async () => {
  for (const testMode of [
    "foreign-author",
    "body-mismatch",
    "ambiguous-item",
    "wrong-item-id",
  ]) {
    mode = testMode;
    const outcome = await withdrawShortPost(
      page,
      { external_id: "42", body: BODY },
      network(),
    );
    assert.equal(outcome.status, "unknown");
    assert.deepEqual(deleteClicks, []);
    assert.equal(await page.locator(".PinToolbar-menuContainer").count(), 0);
  }
});
