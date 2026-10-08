import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import {
  deleteKimiConversation,
  recoverKimiConversation,
} from "../src/provider-conversation-cleanup.mjs";

const SELF = "/apiv2/kimi.gateway.account.v1.UserService/GetCurrentUser";
const CHAT = "/apiv2/kimi.gateway.chat.v1.ChatService/";
const SCOPE = { expectedUserId: "own-1", chatId: "task-chat-1" };

async function fixture(run) {
  const calls = [];
  const state = {
    account: "own-1",
    chat: { chat: { id: SCOPE.chatId, status: "complete" } },
    messages: ({ page_token }) => ({
      chat_id: SCOPE.chatId,
      messages: [{ chat_id: SCOPE.chatId, text: `answer-${page_token}` }],
    }),
    deletion: { chat_id: SCOPE.chatId },
  };
  const server = createServer(async (request, response) => {
    if (request.url === "/") {
      response.writeHead(200, { "content-type": "text/html" });
      response.end('<!doctype html><input id="draft" value="unsent">');
      return;
    }
    if (request.method !== "POST") {
      response.writeHead(404);
      response.end();
      return;
    }
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    calls.push({ path: request.url, body });
    assert.equal(request.method, "POST");
    assert.equal(request.headers.authorization, "Bearer fixture-access");
    assert.equal(request.headers["connect-protocol-version"], "1");
    let data;
    if (request.url === SELF) {
      data = { user: { id: state.account, nickname: "Fixture user" } };
    } else if (request.url === `${CHAT}GetChat`) {
      if (state.getChatStatus) {
        response.writeHead(state.getChatStatus, {
          "content-type": "application/json",
        });
        response.end("{}");
        return;
      }
      data = state.chat;
    } else if (request.url === `${CHAT}ListMessages`) {
      data = state.messages(body);
    } else if (request.url === `${CHAT}DeleteChat`) {
      if (state.lostDeleteResponse) {
        response.writeHead(200, { "content-type": "application/json" });
        // The server accepted the mutation but the acknowledgement body is
        // unreadable; a client cannot infer deletion from this response.
        response.end('{"chat_id":');
        return;
      }
      data = state.deletion;
    } else {
      response.writeHead(404, { "content-type": "application/json" });
      response.end("{}");
      return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(data));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const trustedOrigin = `http://127.0.0.1:${server.address().port}`;
  let browser;
  try {
    browser = await chromium.launch({
      ...(process.env.GEO_TEST_CHROMIUM_PATH
        ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
        : {}),
    });
    const context = await browser.newContext();
    await context.addInitScript(() => {
      localStorage.setItem("access_token", "fixture-access");
      localStorage.setItem("refresh_token", "fixture-refresh");
    });
    const page = await context.newPage();
    await page.goto(`${trustedOrigin}/`);
    context.newPage = () => {
      throw new Error("must not visit the provider history UI");
    };
    await run({ page, state, calls, trustedOrigin });
    assert.equal(page.url(), `${trustedOrigin}/`);
    assert.equal(await page.locator("#draft").inputValue(), "unsent");
    assert.equal(context.pages().length, 1);
    assert.equal(
      calls.some(({ path }) => path.includes("ResumeChat")),
      false,
    );
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
}

function authority() {
  return {
    authorized: true,
    platform_account_id: SCOPE.expectedUserId,
    external_conversation_id: SCOPE.chatId,
  };
}

test("passive recovery paginates bounded message reads, without deletion or generation", async () => {
  await fixture(async ({ page, state, calls, trustedOrigin }) => {
    state.messages = ({ page_token }) =>
      page_token
        ? {
            chat_id: SCOPE.chatId,
            messages: [{ chat_id: SCOPE.chatId, text: "second" }],
          }
        : {
            chat_id: SCOPE.chatId,
            messages: [{ chat_id: SCOPE.chatId, text: "first" }],
            next_page_token: "next-page",
          };
    const result = await recoverKimiConversation(page, {
      ...SCOPE,
      trustedOrigin,
    });
    assert.equal(result.status, "recovered");
    assert.equal(result.message_pages.length, 2);
    assert.deepEqual(
      calls
        .filter(({ path }) => path === `${CHAT}ListMessages`)
        .map(({ body }) => body.page_token),
      ["", "next-page"],
    );
    assert.equal(
      calls.some(({ path }) => path === `${CHAT}DeleteChat`),
      false,
    );
    assert.equal(
      (
        await recoverKimiConversation(page, {
          ...SCOPE,
          trustedOrigin,
          maxPages: 1,
        })
      ).status,
      "retained",
    );
  });
});

test("generation and mismatched chat remain untouched", async () => {
  await fixture(async ({ page, state, calls, trustedOrigin }) => {
    state.chat = { chat: { id: SCOPE.chatId, status: "generating" } };
    assert.deepEqual(
      await recoverKimiConversation(page, { ...SCOPE, trustedOrigin }),
      { status: "retained", reason: "generating" },
    );
    assert.equal(
      (
        await deleteKimiConversation(page, {
          ...SCOPE,
          trustedOrigin,
          authorizeDeletion: async () => authority(),
        })
      ).status,
      "retained",
    );
    state.chat = { chat: { id: "someone-else", status: "complete" } };
    assert.deepEqual(
      await deleteKimiConversation(page, {
        ...SCOPE,
        trustedOrigin,
        authorizeDeletion: async () => authority(),
      }),
      { status: "unknown", reason: "chat_mismatch" },
    );
    assert.equal(
      calls.some(({ path }) => path === `${CHAT}DeleteChat`),
      false,
    );
    assert.equal(
      calls.some(({ path }) => path === `${CHAT}ListMessages`),
      false,
    );
  });
});

test("a GetChat 404 is unknown, never proof of a prior deletion", async () => {
  await fixture(async ({ page, state, calls, trustedOrigin }) => {
    state.getChatStatus = 404;
    assert.deepEqual(
      await deleteKimiConversation(page, {
        ...SCOPE,
        trustedOrigin,
        authorizeDeletion: async () => authority(),
      }),
      { status: "unknown", reason: "http_error" },
    );
    assert.equal(
      calls.some(({ path }) => path === `${CHAT}DeleteChat`),
      false,
    );
  });
});

test("missing durable authorization or switched account cannot delete", async () => {
  await fixture(async ({ page, state, calls, trustedOrigin }) => {
    assert.deepEqual(
      await deleteKimiConversation(page, { ...SCOPE, trustedOrigin }),
      { status: "retained", reason: "authorization_required" },
    );
    assert.equal(
      (
        await deleteKimiConversation(page, {
          ...SCOPE,
          trustedOrigin,
          authorizeDeletion: async () => ({ durable: true }),
        })
      ).status,
      "retained",
      "a saved receipt alone is not deletion authorization",
    );
    assert.equal(
      (
        await deleteKimiConversation(page, {
          ...SCOPE,
          trustedOrigin,
          authorizeDeletion: async () => {
            state.account = "another-account";
            return authority();
          },
        })
      ).status,
      "retained",
    );
    assert.equal(
      calls.some(({ path }) => path === `${CHAT}DeleteChat`),
      false,
    );
  });
});

test("only exact delete acknowledgement succeeds; lost or mismatched response is unknown", async () => {
  await fixture(async ({ page, state, calls, trustedOrigin }) => {
    const options = {
      ...SCOPE,
      trustedOrigin,
      authorizeDeletion: async () => authority(),
    };
    state.deletion = { chat_id: "someone-else" };
    assert.deepEqual(await deleteKimiConversation(page, options), {
      status: "unknown",
      reason: "unverified_delete_response",
    });
    state.lostDeleteResponse = true;
    assert.deepEqual(await deleteKimiConversation(page, options), {
      status: "unknown",
      reason: "transport_unknown",
    });
    assert.equal(
      calls.filter(({ path }) => path === `${CHAT}DeleteChat`).length,
      2,
      "one attempt per explicitly authorized invocation; no automatic retry",
    );
    state.lostDeleteResponse = false;
    state.deletion = { chat_id: SCOPE.chatId };
    assert.equal(
      (await deleteKimiConversation(page, options)).status,
      "deleted",
    );
  });
});
