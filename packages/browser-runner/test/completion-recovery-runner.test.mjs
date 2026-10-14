import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { test } from "node:test";
import { chromium } from "playwright";
import { createRunner } from "../src/runner.mjs";
import { createRunnerServer } from "../src/server.mjs";
import { providerCompletionRecoveryAdapter } from "../src/provider-completion-recovery.mjs";

const account = "synthetic-account";
const chat = "synthetic-chat";
const input = () => ({
  execution_id: randomUUID(),
  expected_identity: { provider: "deepseek", platform_account_id: account },
  external_conversation_id: chat,
  request_message_id: "1",
  response_message_id: "2",
});

async function fixture(run, options = {}) {
  const calls = [];
  const state = { account, incomplete: false, later: false, blocked: null };
  const runner = createRunner({
    browserType: {
      async launch() {
        const browser = await chromium.launch(
          process.env.GEO_TEST_CHROMIUM_PATH
            ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
            : {},
        );
        return {
          close: () => browser.close(),
          async newContext(config) {
            const context = await browser.newContext({
              ...config,
              serviceWorkers: "block",
            });
            await context.route("**/*", async (route) => {
              const request = route.request();
              const url = new URL(request.url());
              assert.equal(url.origin, "https://chat.deepseek.com");
              if (request.isNavigationRequest())
                return route.fulfill({
                  contentType: "text/html",
                  body: "<title>Fixture</title>",
                });
              calls.push(url.pathname);
              assert.equal(request.method(), "GET");
              assert.equal(url.pathname, "/api/v0/chat/history_messages");
              assert.equal(url.searchParams.get("chat_session_id"), chat);
              assert.equal(url.searchParams.has("cache_version"), false);
              assert.equal(url.searchParams.has("cache_reset_at"), false);
              if (state.blocked) await state.blocked;
              if (state.drift) state.account = "other-account";
              await route.fulfill({
                contentType: "application/json",
                body: JSON.stringify({
                  code: 0,
                  data: {
                    biz_code: 0,
                    biz_data: {
                      cache_control: "REPLACE",
                      chat_session: {
                        id: chat,
                        current_message_id: state.later ? 3 : 2,
                      },
                      chat_messages: [
                        {
                          message_id: 1,
                          parent_id: null,
                          role: "USER",
                          status: "FINISHED",
                          has_pending_fragment: false,
                          fragments: [],
                        },
                        {
                          message_id: 2,
                          parent_id: 1,
                          role: "ASSISTANT",
                          status: state.incomplete ? "WIP" : "FINISHED",
                          has_pending_fragment: false,
                          fragments: [],
                        },
                      ],
                    },
                  },
                }),
              });
            });
            return context;
          },
        };
      },
    },
    platformAdapters: {
      deepseek: {
        entry: "https://chat.deepseek.com",
        connectorVersion: "fixture.v1",
        operations: ["measure"],
        async identify() {
          if (state.identityBlocked) await state.identityBlocked;
          return state.account
            ? { platform_account_id: state.account, display_name: "Fixture" }
            : null;
        },
      },
    },
    ...options,
  });
  await runner.create({
    session_id: "session",
    platform: "deepseek",
    storage_state: {
      cookies: [],
      origins: [
        {
          origin: "https://chat.deepseek.com",
          localStorage: [
            {
              name: "userToken",
              value: JSON.stringify({
                value: "synthetic-token",
                __version: "0",
              }),
            },
          ],
        },
      ],
    },
  });
  await runner.complete("session");
  try {
    await run({ runner, state, calls });
  } finally {
    await runner.shutdown();
  }
}

test("completion recovery uses exact read-only history and caches same execution", async () => {
  await fixture(async ({ runner, calls }) => {
    const request = input();
    const result = await runner.recoverConversationCompletion(
      "session",
      request,
    );
    assert.equal(result.status, "recovered");
    assert.deepEqual(result.recovery_identity, request.expected_identity);
    assert.equal(
      result.history_sha256,
      createHash("sha256").update(result.history_json).digest("hex"),
    );
    const history = JSON.parse(result.history_json);
    assert.equal(history.observed_at, result.observed_at);
    assert.equal(history.chat_session_id, chat);
    assert.deepEqual(
      await runner.recoverConversationCompletion("session", request),
      result,
    );
    assert.equal(calls.length, 1);
    await assert.rejects(
      runner.recoverConversationCompletion("session", {
        ...request,
        response_message_id: "3",
      }),
      /execution_conflict/,
    );
    assert.equal(calls.length, 1);
    assert.equal(JSON.stringify(result).includes("synthetic-token"), false);
  });
});

test("completion recovery rejects changed IDs, later/incomplete history and account drift", async () => {
  await fixture(async ({ runner, state, calls }) => {
    for (const patch of [
      { response_message_id: "3" },
      { request_message_id: "3" },
    ]) {
      const result = await runner.recoverConversationCompletion("session", {
        ...input(),
        ...patch,
      });
      assert.equal(result.status, "retained");
      assert.equal(result.history_json, undefined);
      assert.deepEqual(result.diagnostic, {
        stage: "history",
        code: "completion_unverified",
        reason: "history_binding_mismatch",
      });
    }
    for (const flag of ["incomplete", "later"]) {
      state[flag] = true;
      const request = input();
      const result = await runner.recoverConversationCompletion(
        "session",
        request,
      );
      assert.equal(result.status, "retained");
      assert.equal(
        result.diagnostic.reason,
        flag === "incomplete"
          ? "completion_unverified"
          : "history_binding_mismatch",
      );
      const reads = calls.length;
      assert.deepEqual(
        await runner.recoverConversationCompletion("session", request),
        result,
      );
      assert.equal(calls.length, reads);
      state[flag] = false;
    }
    const count = calls.length;
    state.account = null;
    assert.equal(
      (await runner.recoverConversationCompletion("session", input())).status,
      "needs_login",
    );
    state.account = "other-account";
    assert.equal(
      (await runner.recoverConversationCompletion("session", input()))
        .diagnostic.code,
      "account_mismatch",
    );
    assert.equal(calls.length, count);
    state.account = account;
    state.drift = true;
    const drift = await runner.recoverConversationCompletion(
      "session",
      input(),
    );
    assert.equal(drift.diagnostic.code, "account_mismatch");
    assert.equal(drift.diagnostic.reason, undefined);
    assert.equal(drift.history_json, undefined);
  });
});

test("completion recovery validates canonical scope before any provider read", async () => {
  assert.equal(providerCompletionRecoveryAdapter("__proto__"), null);
  assert.equal(providerCompletionRecoveryAdapter("kimi"), null);
  await fixture(async ({ runner, calls }) => {
    for (const patch of [
      { request_message_id: "01" },
      { response_message_id: 2 },
      { request_message_id: "2" },
      { external_conversation_id: "../other" },
      { prompt: "not allowed" },
      { execution_id: "bad" },
    ])
      await assert.rejects(
        runner.recoverConversationCompletion("session", {
          ...input(),
          ...patch,
        }),
        /invalid_completion_recovery/,
      );
    assert.equal(calls.length, 0);
  });
});

test("deadline keeps blocked identity work fenced and prevents late history dispatch", async () => {
  await fixture(
    async ({ runner, state, calls }) => {
      let release;
      state.identityBlocked = new Promise((resolve) => {
        release = resolve;
      });
      const request = input();
      const result = await runner.recoverConversationCompletion(
        "session",
        request,
      );
      assert.equal(result.status, "unknown");
      assert.equal(result.diagnostic.code, "deadline_exceeded");
      assert.equal(result.diagnostic.reason, undefined);
      await assert.rejects(
        runner.recoverConversationCompletion("session", input()),
        /session_busy/,
      );
      await assert.rejects(runner.close("session"), /session_busy/);
      assert.deepEqual(
        await runner.recoverConversationCompletion("session", request),
        result,
      );
      release();
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(calls.length, 0);
    },
    { executionTimeoutMs: 30 },
  );
});

test("completion recovery HTTP route requires service authorization without deletion callback", async () => {
  await fixture(async ({ runner, calls }) => {
    const server = createRunnerServer({
      runner,
      token: "synthetic-service-token",
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const url = `http://127.0.0.1:${server.address().port}/v1/sessions/session/recover-conversation-completion`;
    try {
      assert.equal((await fetch(url, { method: "POST" })).status, 401);
      assert.equal(calls.length, 0);
      const response = await fetch(url, {
        method: "POST",
        headers: {
          authorization: "Bearer synthetic-service-token",
          "content-type": "application/json",
        },
        body: JSON.stringify(input()),
      });
      assert.equal(response.status, 200);
      assert.equal((await response.json()).status, "recovered");
      assert.equal(calls.length, 1);
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  });
});
