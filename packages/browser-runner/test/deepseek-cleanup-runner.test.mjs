import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { test } from "node:test";
import { chromium } from "playwright";
import { createRunner } from "../src/runner.mjs";

test("real runner dispatches DeepSeek cleanup, preserves authority and deduplicates deletion", async () => {
  const origin = "https://chat.deepseek.com";
  const account = "synthetic-account";
  const chat = "synthetic-session";
  const token = "synthetic-token";
  const calls = [];
  let deleted = false;
  const history = {
    code: 0,
    data: {
      biz_code: 0,
      biz_data: {
        cache_control: "REPLACE",
        chat_session: { id: chat, current_message_id: 2 },
        chat_messages: [
          {
            message_id: 1,
            parent_id: null,
            role: "USER",
            status: "FINISHED",
            has_pending_fragment: false,
            fragments: [{ content: "synthetic private question" }],
          },
          {
            message_id: 2,
            parent_id: 1,
            role: "ASSISTANT",
            status: "FINISHED",
            has_pending_fragment: false,
            fragments: [{ content: "synthetic private answer" }],
          },
        ],
      },
    },
  };
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
          async newContext(options) {
            const context = await browser.newContext({
              ...options,
              serviceWorkers: "block",
            });
            await context.route("**/*", async (route) => {
              const request = route.request();
              const url = new URL(request.url());
              assert.equal(url.origin, origin);
              if (request.isNavigationRequest())
                return route.fulfill({
                  contentType: "text/html",
                  body: "<title>Synthetic account</title>",
                });
              calls.push({ path: url.pathname, method: request.method() });
              let body;
              if (url.pathname === "/api/v0/users/current")
                body = {
                  code: 0,
                  data: { biz_code: 0, biz_data: { id: account } },
                };
              else if (url.pathname === "/api/v0/chat/history_messages") {
                assert.equal(url.searchParams.get("chat_session_id"), chat);
                assert.equal(url.searchParams.get("cache_version"), "null");
                assert.equal(url.searchParams.get("cache_reset_at"), "null");
                body = deleted ? { code: 0, data: { biz_code: 1 } } : history;
              } else if (url.pathname === "/api/v0/chat_session/delete") {
                assert.equal(request.method(), "POST");
                assert.deepEqual(request.postDataJSON(), {
                  chat_session_ids: [chat],
                });
                assert.equal(
                  request.headers().authorization,
                  `Bearer ${token}`,
                );
                deleted = true;
                body = { code: 0, data: { biz_code: 0 } };
              } else return route.abort();
              return route.fulfill({
                contentType: "application/json",
                body: JSON.stringify(body),
              });
            });
            return context;
          },
        };
      },
    },
  });
  try {
    await runner.create({
      session_id: "synthetic-runner-session",
      platform: "deepseek",
      storage_state: {
        cookies: [],
        origins: [
          {
            origin,
            localStorage: [
              {
                name: "userToken",
                value: JSON.stringify({ __version: "0", value: token }),
              },
            ],
          },
        ],
      },
    });
    await runner.complete("synthetic-runner-session");
    const request = {
      execution_id: randomUUID(),
      expected_identity: {
        provider: "deepseek",
        platform_account_id: account,
      },
      external_conversation_id: chat,
      action: "delete",
      authorization_ticket: "aabb",
    };
    const beforeWrongProvider = calls.length;
    const mismatch = await runner.cleanupConversation(
      "synthetic-runner-session",
      {
        ...request,
        expected_identity: { ...request.expected_identity, provider: "kimi" },
      },
    );
    assert.equal(mismatch.status, "retained");
    assert.equal(calls.length, beforeWrongProvider);
    let authorizations = 0;
    const authorize = async (scope) => {
      authorizations++;
      assert.deepEqual(scope, {
        schema_version: 1,
        authorization_ticket: "aabb",
        runner_session_id: "synthetic-runner-session",
        platform_account_id: account,
        external_conversation_id: chat,
      });
      return {
        authorized: true,
        platform_account_id: account,
        external_conversation_id: chat,
        retained_message_inventory_sha256: createHash("sha256")
          .update(
            JSON.stringify([
              ["1", "user"],
              ["2", "assistant"],
            ]),
          )
          .digest("hex"),
        delete_not_after: new Date(Date.now() + 60_000).toISOString(),
      };
    };
    const result = await runner.cleanupConversation(
      "synthetic-runner-session",
      request,
      authorize,
    );
    assert.equal(result.status, "deleted");
    const countAfterDelete = calls.length;
    assert.deepEqual(
      await runner.cleanupConversation(
        "synthetic-runner-session",
        request,
        authorize,
      ),
      result,
    );
    assert.equal(calls.length, countAfterDelete);
    assert.equal(authorizations, 1);
    const reconcile = await runner.cleanupConversation(
      "synthetic-runner-session",
      {
        ...request,
        execution_id: randomUUID(),
        action: "reconcile",
        authorization_ticket: undefined,
      },
    );
    assert.equal(reconcile.status, "absent");
    assert.equal(calls.filter((call) => call.method === "POST").length, 1);
    assert.equal(JSON.stringify([result, reconcile]).includes(token), false);
    assert.equal(
      JSON.stringify([result, reconcile]).includes("private"),
      false,
    );
  } finally {
    await runner.shutdown();
  }
});
