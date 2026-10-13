import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { after, before, test } from "node:test";
import { chromium } from "playwright";
import {
  deleteDeepSeekConversation,
  recoverDeepSeekConversation,
} from "../src/deepseek-conversation-cleanup.mjs";

const ORIGIN = "https://chat.deepseek.com";
const CHAT = "synthetic-cleanup-session";
const ACCOUNT = "synthetic-account";
const TOKEN = "synthetic-cleanup-token";
const HISTORY = `${ORIGIN}/api/v0/chat/history_messages?chat_session_id=${CHAT}`;
const SELF = `${ORIGIN}/api/v0/users/current`;
const DELETE = `${ORIGIN}/api/v0/chat_session/delete`;
const absent = () => ({ code: 0, data: { biz_code: 1 } });
const fixture = () => ({
  code: 0,
  data: {
    biz_code: 0,
    biz_data: {
      cache_control: "REPLACE",
      chat_session: { id: CHAT, current_message_id: 20 },
      chat_messages: [
        {
          message_id: 20,
          parent_id: 10,
          role: "ASSISTANT",
          status: "FINISHED",
          has_pending_fragment: false,
          fragments: [{ content: "synthetic private answer" }],
        },
        {
          message_id: 10,
          parent_id: null,
          role: "USER",
          status: "FINISHED",
          has_pending_fragment: false,
          fragments: [{ content: "synthetic private question" }],
        },
      ],
    },
  },
});
const digest = createHash("sha256")
  .update(
    JSON.stringify([
      ["10", "user"],
      ["20", "assistant"],
    ]),
  )
  .digest("hex");
const grant = () => ({
  authorized: true,
  platform_account_id: ACCOUNT,
  external_conversation_id: CHAT,
  retained_message_inventory_sha256: digest,
  delete_not_after: new Date(Date.now() + 60_000).toISOString(),
});
const options = {
  expectedUserId: ACCOUNT,
  chatId: CHAT,
  timeoutMs: 5_000,
};
let browser;
before(async () => {
  browser = await chromium.launch(
    process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {},
  );
});
after(async () => browser?.close());

async function synthetic(t) {
  const context = await browser.newContext({ serviceWorkers: "block" });
  t.after(() => context.close());
  const requests = [];
  const state = {
    account: ACCOUNT,
    history: fixture(),
    afterDelete: absent(),
    deleteBody: { code: 0, data: { biz_code: 0, biz_data: null } },
    deleted: false,
    historyReads: 0,
    identityReads: 0,
  };
  await context.route("**/*", async (route) => {
    const request = route.request();
    if (request.isNavigationRequest())
      return route.fulfill({
        contentType: "text/html",
        body: "<title>test</title>",
      });
    requests.push(request);
    let body;
    let status = 200;
    let headers;
    let contentType = "application/json";
    if (request.url() === SELF) {
      state.identityReads++;
      await state.onIdentity?.(state.identityReads);
      body = {
        code: 0,
        data: { biz_code: 0, biz_data: { id: state.account } },
      };
    } else if (request.url() === HISTORY) {
      state.historyReads++;
      await state.onHistory?.(state.historyReads);
      body = state.deleted ? state.afterDelete : state.history;
      status = state.historyStatus ?? 200;
    } else if (request.url() === DELETE) {
      state.deleted = true;
      await state.onDelete?.();
      if (state.abortDelete) return route.abort();
      body = state.deleteBody;
      status = state.deleteStatus ?? 200;
      headers = state.deleteHeaders;
      contentType = state.deleteContentType ?? contentType;
    } else return route.abort();
    return route
      .fulfill({
        status,
        headers,
        contentType,
        body: typeof body === "string" ? body : JSON.stringify(body),
      })
      .catch(() => {});
  });
  const page = await context.newPage();
  await page.goto(`${ORIGIN}/a/chat/${CHAT}`);
  await page.evaluate((value) => {
    localStorage.setItem(
      "userToken",
      JSON.stringify({ __version: "0", value }),
    );
  }, TOKEN);
  return { page, requests, state };
}
const posts = (requests) =>
  requests.filter((request) => request.method() === "POST");
const safe = (value) => {
  const text = JSON.stringify(value);
  assert.equal(text.includes(TOKEN), false);
  assert.equal(text.includes("synthetic private"), false);
  assert.equal(text.includes("fragments"), false);
};

test("single exact authorized POST follows two fresh inventories and strict absence readback", async (t) => {
  const { page, requests } = await synthetic(t);
  let authorizationCalls = 0;
  const result = await deleteDeepSeekConversation(page, {
    ...options,
    authorizeDeletion: async (scope) => {
      authorizationCalls++;
      assert.deepEqual(scope, {
        provider: "deepseek",
        platform_account_id: ACCOUNT,
        external_conversation_id: CHAT,
      });
      assert.deepEqual(
        requests.map((request) => request.url()),
        [SELF, HISTORY],
      );
      return grant();
    },
  });
  assert.deepEqual(result, {
    status: "deleted",
    external_conversation_id: CHAT,
  });
  assert.equal(authorizationCalls, 1);
  assert.deepEqual(
    requests.map((request) => request.url()),
    [SELF, HISTORY, SELF, HISTORY, DELETE, SELF, HISTORY],
  );
  assert.equal(posts(requests).length, 1);
  assert.deepEqual(posts(requests)[0].postDataJSON(), {
    chat_session_ids: [CHAT],
  });
  assert.equal(posts(requests)[0].headers().authorization, `Bearer ${TOKEN}`);
  assert.equal(
    posts(requests)[0].headers()["content-type"],
    "application/json",
  );
  assert.equal(page.url(), `${ORIGIN}/a/chat/${CHAT}`);
  assert.equal(page.listenerCount("framenavigated"), 0);
  safe(result);
});

test("passive recovery returns sanitized inventory and never calls authorization or POST", async (t) => {
  const { page, requests } = await synthetic(t);
  const result = await recoverDeepSeekConversation(page, options);
  assert.equal(result.status, "recovered");
  assert.deepEqual(result.inventory.messages, [
    { id: "10", role: "user" },
    { id: "20", role: "assistant" },
  ]);
  assert.equal(posts(requests).length, 0);
  safe(result);
});

test("strict absence only reaches reconciliation with explicit verifyAbsence policy", async (t) => {
  const { page, requests, state } = await synthetic(t);
  state.history = absent();
  assert.equal(
    (await recoverDeepSeekConversation(page, options)).status,
    "unknown",
  );
  assert.deepEqual(
    await recoverDeepSeekConversation(page, {
      ...options,
      verifyAbsence: true,
    }),
    { status: "absent", external_conversation_id: CHAT },
  );
  assert.equal(
    (
      await deleteDeepSeekConversation(page, {
        ...options,
        authorizeDeletion: () =>
          assert.fail("absence must not request a grant"),
      })
    ).status,
    "retained",
  );
  for (const value of [
    { code: 0, data: { biz_code: 5 } },
    { code: 1, data: { biz_code: 1 } },
    { code: 0, data: { biz_code: 0, biz_data: { chat_messages: [] } } },
    {},
  ]) {
    state.history = value;
    assert.equal(
      (
        await recoverDeepSeekConversation(page, {
          ...options,
          verifyAbsence: true,
        })
      ).status,
      "unknown",
    );
  }
  state.history = absent();
  state.historyStatus = 404;
  assert.equal(
    (
      await recoverDeepSeekConversation(page, {
        ...options,
        verifyAbsence: true,
      })
    ).status,
    "unknown",
  );
  assert.equal(posts(requests).length, 0);
});

test("invalid scope, wrong origin and missing authorization never send requests", async (t) => {
  const { page, requests } = await synthetic(t);
  for (const change of [
    { chatId: "../other" },
    { expectedUserId: "" },
    { timeoutMs: 0 },
    { timeoutMs: 30_001 },
    { trustedOrigin: "https://example.invalid" },
  ])
    assert.equal(
      (
        await deleteDeepSeekConversation(page, {
          ...options,
          ...change,
          authorizeDeletion: grant,
        })
      ).reason,
      "invalid_scope",
    );
  assert.equal(
    (await deleteDeepSeekConversation(page, options)).reason,
    "authorization_required",
  );
  await page.goto("https://example.invalid/");
  assert.equal(
    (
      await deleteDeepSeekConversation(page, {
        ...options,
        authorizeDeletion: grant,
      })
    ).status,
    "unknown",
  );
  assert.equal(requests.length, 0);
});

test("identity mismatch before authorization and after awaited grant prevents POST", async (t) => {
  for (const phase of ["initial", "after-grant"]) {
    const { page, state, requests } = await synthetic(t);
    if (phase === "initial") state.account = "other-synthetic-account";
    const result = await deleteDeepSeekConversation(page, {
      ...options,
      authorizeDeletion: () => {
        assert.equal(phase, "after-grant");
        state.account = "other-synthetic-account";
        return grant();
      },
    });
    assert.equal(result.reason, "account_mismatch");
    assert.equal(posts(requests).length, 0);
    safe(result);
  }
});

test("missing/wrong retained authority and invalid or expired deadline prevent POST", async (t) => {
  for (const change of [
    { authorized: false },
    { platform_account_id: "other-synthetic-account" },
    { external_conversation_id: "other-synthetic-session" },
    { retained_message_inventory_sha256: "a".repeat(64) },
    { retained_message_inventory_sha256: "bad" },
    { delete_not_after: undefined },
    { delete_not_after: "invalid" },
    { delete_not_after: new Date(0).toISOString() },
  ]) {
    const { page, requests } = await synthetic(t);
    const result = await deleteDeepSeekConversation(page, {
      ...options,
      authorizeDeletion: () => ({ ...grant(), ...change }),
    });
    assert.equal(result.status, "retained");
    assert.equal(posts(requests).length, 0);
    safe(result);
  }
});

test("extra user turns, swapped IDs and generation after grant invalidate the final inventory", async (t) => {
  for (const change of ["extra", "different-ids", "pending", "absent"]) {
    const { page, state, requests } = await synthetic(t);
    const result = await deleteDeepSeekConversation(page, {
      ...options,
      authorizeDeletion: () => {
        const messages = state.history.data.biz_data.chat_messages;
        if (change === "extra")
          messages.push({ ...messages[1], message_id: 30, parent_id: 20 });
        if (change === "different-ids") {
          messages[1].message_id = 11;
          messages[0].parent_id = 11;
        }
        if (change === "pending") messages[0].has_pending_fragment = true;
        if (change === "absent") state.history = absent();
        return grant();
      },
    });
    assert.equal(result.reason, "message_inventory_mismatch");
    assert.equal(posts(requests).length, 0);
  }
});

test("same-URL reload, navigation and token replacement during authorization prevent POST", async (t) => {
  for (const action of ["reload", "same-origin", "cross-origin", "token"]) {
    const { page, requests } = await synthetic(t);
    const result = await deleteDeepSeekConversation(page, {
      ...options,
      authorizeDeletion: async () => {
        if (action === "reload") await page.reload();
        if (action === "same-origin") await page.goto(`${ORIGIN}/other`);
        if (action === "cross-origin")
          await page.goto("https://example.invalid/");
        if (action === "token")
          await page.evaluate(() =>
            localStorage.setItem(
              "userToken",
              JSON.stringify({
                __version: "0",
                value: "different-synthetic-token",
              }),
            ),
          );
        return grant();
      },
    });
    assert.equal(result.status, "unknown");
    assert.equal(posts(requests).length, 0);
  }
});

test("authorization throw, cancellation and timeout never allow late POST", async (t) => {
  for (const action of ["throw", "abort", "timeout"]) {
    const { page, requests } = await synthetic(t);
    const controller = new AbortController();
    let release;
    let reached;
    const entered = new Promise((resolve) => {
      reached = resolve;
    });
    const waiting = new Promise((resolve) => {
      release = resolve;
    });
    const running = deleteDeepSeekConversation(page, {
      ...options,
      timeoutMs: action === "timeout" ? 500 : 5_000,
      signal: controller.signal,
      authorizeDeletion: async () => {
        reached();
        if (action === "throw") throw new Error("synthetic private exception");
        await waiting;
        return grant();
      },
    });
    await entered;
    if (action === "abort") controller.abort();
    const result = await running;
    assert.equal(result.status, action === "throw" ? "retained" : "unknown");
    release();
    await page.evaluate(() => true);
    assert.equal(posts(requests).length, 0);
    safe(result);
  }
});

test("ambiguous POST responses never retry or infer absence from any error code", async (t) => {
  for (const response of [
    { abortDelete: true },
    { deleteBody: "{" },
    { deleteBody: { code: 0, data: { biz_code: 1 } } },
    { deleteBody: { code: 0, data: { biz_code: 5 } } },
    { deleteBody: { code: 1, data: { biz_code: 0 } } },
    { deleteBody: {} },
    { deleteStatus: 403 },
    { deleteContentType: "text/html" },
    { deleteBody: "x".repeat(128_001) },
    {
      deleteStatus: 302,
      deleteHeaders: { location: "https://example.invalid/steal" },
    },
  ]) {
    const { page, requests, state } = await synthetic(t);
    Object.assign(state, response);
    const result = await deleteDeepSeekConversation(page, {
      ...options,
      authorizeDeletion: grant,
    });
    assert.equal(result.status, "unknown");
    assert.equal(posts(requests).length, 1);
    assert.equal(state.historyReads, 2);
    assert.ok(
      requests.every((request) =>
        [SELF, HISTORY, DELETE].includes(request.url()),
      ),
    );
    safe(result);
  }
});

test("successful POST without strict absent fresh readback remains unknown", async (t) => {
  for (const readback of [
    fixture(),
    { code: 0, data: { biz_code: 0, biz_data: { chat_messages: [] } } },
    { code: 0, data: { biz_code: 5 } },
    { code: 1, data: { biz_code: 1 } },
  ]) {
    const { page, state, requests } = await synthetic(t);
    state.afterDelete = readback;
    const result = await deleteDeepSeekConversation(page, {
      ...options,
      authorizeDeletion: grant,
    });
    assert.equal(result.status, "unknown");
    assert.equal(posts(requests).length, 1);
    assert.equal(state.historyReads, 3);
  }
});

test("cancellation after POST stays unknown, with no readback or retry", async (t) => {
  const { page, state, requests } = await synthetic(t);
  const controller = new AbortController();
  let release;
  const waiting = new Promise((resolve) => {
    release = resolve;
  });
  state.onDelete = async () => {
    controller.abort();
    await waiting;
  };
  const result = await deleteDeepSeekConversation(page, {
    ...options,
    signal: controller.signal,
    authorizeDeletion: grant,
  });
  assert.equal(result.status, "unknown");
  assert.equal(posts(requests).length, 1);
  release();
  await page.evaluate(() => true);
  assert.equal(state.historyReads, 2);
});

test("grant expiry during final inventory is checked again before POST", async (t) => {
  const { page, state, requests } = await synthetic(t);
  let expires;
  state.onHistory = async (count) => {
    if (count === 2) {
      // Wait only for the synthetic grant to cross its dispatch margin.
      await new Promise((resolve) =>
        setTimeout(resolve, Math.max(0, expires - Date.now() - 7_000 + 30)),
      );
    }
  };
  const result = await deleteDeepSeekConversation(page, {
    ...options,
    authorizeDeletion: () => {
      expires = Date.now() + 7_200;
      return { ...grant(), delete_not_after: new Date(expires).toISOString() };
    },
  });
  assert.equal(result.diagnostic.code, "authorization_expired");
  assert.equal(state.historyReads, 2);
  assert.equal(posts(requests).length, 0);
});

test("delayed destructive evaluation rechecks the grant and caller deadline inside the document", async (t) => {
  for (const action of ["grant-expired", "timeout", "abort"]) {
    const { page, requests } = await synthetic(t);
    const controller = new AbortController();
    let release;
    let entered;
    const reached = new Promise((resolve) => {
      entered = resolve;
    });
    const waiting = new Promise((resolve) => {
      release = resolve;
    });
    const wrapped = {
      url: () => page.url(),
      isClosed: () => page.isClosed(),
      mainFrame: () => page.mainFrame(),
      on: (...args) => page.on(...args),
      off: (...args) => page.off(...args),
      evaluateHandle: (...args) => page.evaluateHandle(...args),
      evaluate: async (fn, arg) => {
        if (arg?.kind === "delete") {
          entered();
          await waiting;
        }
        return page.evaluate(fn, arg);
      },
    };
    const running = deleteDeepSeekConversation(wrapped, {
      ...options,
      timeoutMs: action === "timeout" ? 500 : 5_000,
      signal: controller.signal,
      authorizeDeletion: () => ({
        ...grant(),
        ...(action === "grant-expired"
          ? { delete_not_after: new Date(Date.now() + 8_000).toISOString() }
          : {}),
      }),
    });
    await reached;
    if (action === "grant-expired") {
      // Advance only the document clock after all Node-side checks have run.
      await page.evaluate(() => {
        const original = Date.now;
        Date.now = () => original() + 2_000;
      });
      release();
      const result = await running;
      assert.equal(result.diagnostic.code, "authorization_expired");
    } else {
      if (action === "abort") controller.abort();
      assert.equal((await running).status, "unknown");
      await page.evaluate(() => true);
      release();
    }
    await page.evaluate(() => true);
    assert.equal(posts(requests).length, 0);
  }
});

test("navigation during final inventory and account change after POST remain unknown", async (t) => {
  for (const phase of ["inventory", "readback"]) {
    const { page, state, requests } = await synthetic(t);
    state.onHistory = async (count) => {
      if (phase === "inventory" && count === 2) await page.reload();
    };
    state.onDelete = async () => {
      if (phase === "readback") state.account = "other-synthetic-account";
    };
    const result = await deleteDeepSeekConversation(page, {
      ...options,
      authorizeDeletion: grant,
    });
    assert.equal(result.status, "unknown");
    assert.equal(posts(requests).length, phase === "inventory" ? 0 : 1);
    assert.equal(state.historyReads, 2);
  }
});

test("a hung identity response obeys the outer deadline and cannot authorize later", async (t) => {
  const { page, state, requests } = await synthetic(t);
  let release;
  let entered;
  const waiting = new Promise((resolve) => {
    release = resolve;
  });
  const reached = new Promise((resolve) => {
    entered = resolve;
  });
  state.onIdentity = async () => {
    entered();
    await waiting;
  };
  const running = deleteDeepSeekConversation(page, {
    ...options,
    timeoutMs: 500,
    authorizeDeletion: () => assert.fail("late authorization"),
  });
  await reached;
  assert.equal((await running).status, "unknown");
  release();
  await page.evaluate(() => true);
  assert.equal(posts(requests).length, 0);
  assert.equal(state.historyReads, 0);
  assert.equal(page.listenerCount("framenavigated"), 0);
});
