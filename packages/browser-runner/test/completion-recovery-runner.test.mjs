import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { test } from "node:test";
import { chromium } from "playwright";
import { createRunner } from "../src/runner.mjs";
import { createRunnerServer } from "../src/server.mjs";
import { providerCompletionRecoveryAdapter } from "../src/provider-completion-recovery.mjs";

const account = "synthetic-account";
const chat = "synthetic-chat";
const digest = (value) => createHash("sha256").update(value).digest("hex");
const source = () => ({
  messages: [
    {
      event: "ready",
      data: {
        request_message_id: 1,
        response_message_id: 2,
        model_type: "synthetic-model",
      },
    },
    { event: "delta", data: { text: "synthetic answer" } },
    { event: "finish", raw_data: "" },
  ],
  ownership: {
    schema_version: "geo.deepseek.ownership.v1",
    provider: "deepseek",
    creation: { source: "browser_observed_create", chat_session_id: chat },
    submission: {
      chat_session_id: chat,
      parent_message_id: null,
      model_type: "synthetic-model",
      prompt_sha256: "a".repeat(64),
      search_enabled: true,
      thinking_enabled: false,
    },
  },
});
const withSource = (value) => {
  const source_json = typeof value === "string" ? value : JSON.stringify(value);
  return { source_json, source_sha256: digest(source_json) };
};
const failedSource = () => ({
  ...source(),
  messages: [
    source().messages[0],
    {
      event: "hint",
      data: {
        type: "error",
        finish_reason: "backend_err",
        clear_response: true,
      },
    },
    { event: "close", data: {} },
  ],
  capture_status: {
    schema_version: "geo.observation.capture-status.v1",
    transport: "complete",
    termination: "complete",
  },
});
const input = () => ({
  execution_id: randomUUID(),
  expected_identity: { provider: "deepseek", platform_account_id: account },
  external_conversation_id: chat,
  source_capture_id: randomUUID(),
  source_receipt_sha256: "a".repeat(64),
  ...withSource(source()),
});

async function fixture(run, options = {}) {
  const { platform = "deepseek", ...runnerOptions } = options;
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
                        current_message_id: state.empty
                          ? null
                          : state.later
                            ? 3
                            : 2,
                      },
                      chat_messages: state.empty
                        ? []
                        : [
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
      [platform]: {
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
    ...runnerOptions,
  });
  await runner.create({
    session_id: "session",
    platform,
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
    assert.deepEqual(result.completion, {
      outcome: "complete",
      message_inventory: [
        ["1", "user"],
        ["2", "assistant"],
      ],
    });
    assert.equal(result.source_capture_id, request.source_capture_id);
    assert.equal(result.source_sha256, request.source_sha256);
    assert.equal(result.source_receipt_sha256, request.source_receipt_sha256);
    assert.ok(Number.isFinite(Date.parse(result.observed_at)));
    assert.equal(result.history_sha256, digest(result.history_json));
    assert.equal(
      JSON.parse(result.history_json).observed_at,
      result.observed_at,
    );
    assert.equal(JSON.parse(result.history_json).chat_session_id, chat);
    assert.deepEqual(
      await runner.recoverConversationCompletion("session", request),
      result,
    );
    assert.equal(calls.length, 1);
    await assert.rejects(
      runner.recoverConversationCompletion("session", {
        ...request,
        source_receipt_sha256: "b".repeat(64),
      }),
      /execution_conflict/,
    );
    assert.equal(calls.length, 1);
    assert.equal(JSON.stringify(result).includes("synthetic-token"), false);
  });
});

test("recovery returns an empty inventory only for a verified cleared failure", async () => {
  await fixture(async ({ runner, calls, state }) => {
    state.empty = true;
    const result = await runner.recoverConversationCompletion("session", {
      ...input(),
      ...withSource(failedSource()),
    });
    assert.equal(result.status, "recovered");
    assert.deepEqual(result.completion, {
      outcome: "cleared_failure",
      message_inventory: [],
    });
    assert.equal(result.history_sha256, digest(result.history_json));
    assert.deepEqual(JSON.parse(result.history_json).messages, []);
    assert.equal(calls.length, 1);
    const ordinary = await runner.recoverConversationCompletion(
      "session",
      input(),
    );
    assert.equal(ordinary.status, "retained");
    assert.equal(ordinary.completion, undefined);
  });
});

test("completion recovery rejects changed IDs, later/incomplete history and account drift", async () => {
  await fixture(async ({ runner, state, calls }) => {
    for (const patch of [
      { response_message_id: 3 },
      { request_message_id: 3 },
    ]) {
      const changed = source();
      Object.assign(changed.messages[0].data, patch);
      const result = await runner.recoverConversationCompletion("session", {
        ...input(),
        ...withSource(changed),
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
      { source_capture_id: "bad" },
      { source_sha256: "b".repeat(64) },
      { source_receipt_sha256: "bad" },
      { source_json: "" },
      { source_json: "x".repeat(750_001) },
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

test("unsupported recovery capability is retained through the authenticated HTTP route", async () => {
  await fixture(
    async ({ runner, calls }) => {
      const server = createRunnerServer({
        runner,
        token: "synthetic-service-token",
      });
      await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
      try {
        const request = {
          ...input(),
          expected_identity: { provider: "kimi", platform_account_id: account },
        };
        const response = await fetch(
          `http://127.0.0.1:${server.address().port}/v1/sessions/session/recover-conversation-completion`,
          {
            method: "POST",
            headers: {
              authorization: "Bearer synthetic-service-token",
              "content-type": "application/json",
            },
            body: JSON.stringify(request),
          },
        );
        assert.equal(response.status, 200);
        const result = await response.json();
        assert.equal(result.status, "retained");
        assert.deepEqual(result.diagnostic, {
          stage: "scope",
          code: "unsupported_platform",
        });
        assert.equal(result.source_capture_id, request.source_capture_id);
        assert.equal(result.source_sha256, request.source_sha256);
        assert.equal(
          result.source_receipt_sha256,
          request.source_receipt_sha256,
        );
        assert.equal(calls.length, 0);
      } finally {
        await new Promise((resolve) => server.close(resolve));
      }
    },
    { platform: "kimi" },
  );
});

test("immutable original source ineligibility is distinct from recoverable live history", async () => {
  await fixture(async ({ runner, calls, state }) => {
    const invalidSources = [source(), source()];
    delete invalidSources[0].ownership;
    invalidSources[1].messages.pop();
    for (const value of invalidSources) {
      const request = { ...input(), ...withSource(value) };
      const result = await runner.recoverConversationCompletion(
        "session",
        request,
      );
      assert.equal(result.status, "retained");
      assert.deepEqual(result.diagnostic, {
        stage: "source",
        code: "source_ineligible",
      });
      assert.equal(result.history_json, undefined);
      assert.equal(result.completion, undefined);
      assert.deepEqual(
        await runner.recoverConversationCompletion("session", request),
        result,
      );
    }
    assert.equal(calls.length, 0);
    const request = input();
    state.incomplete = true;
    const pending = await runner.recoverConversationCompletion(
      "session",
      request,
    );
    assert.deepEqual(pending.diagnostic, {
      stage: "history",
      code: "completion_unverified",
      reason: "completion_unverified",
    });
    state.incomplete = false;
    const recovered = await runner.recoverConversationCompletion("session", {
      ...request,
      execution_id: randomUUID(),
    });
    assert.equal(recovered.status, "recovered");
    assert.equal(calls.length, 2);
  });
});

test("the provider strategy rejects missing or noncanonical original proof before any history request", async () => {
  const adapter = providerCompletionRecoveryAdapter("deepseek");
  const untouchedPage = new Proxy(
    {},
    {
      get() {
        assert.fail("invalid source must not reach the browser");
      },
    },
  );
  const invalidSources = [
    "not JSON",
    "null",
    "[]",
    ...["1e0", "1.0", "-0", '"1"', "9007199254740992"].map((id) =>
      JSON.stringify(source()).replace(
        '"request_message_id":1',
        `"request_message_id":${id}`,
      ),
    ),
  ];
  for (const mutate of [
    (v) => delete v.ownership,
    (v) => v.messages.pop(),
    (v) => v.messages.push(v.messages[0]),
    (v) => (v.ownership.creation.chat_session_id = "other-chat"),
    (v) => (v.messages[0].data.model_type = "other-model"),
    (v) =>
      (v.source_redaction = {
        schema_version: "geo.observation.source-redaction.v1",
        policy: "reasoning-and-private-fields",
        omitted_paths: ["/messages/2/raw_data"],
      }),
  ]) {
    const value = source();
    mutate(value);
    invalidSources.push(JSON.stringify(value));
  }
  for (const source_json of invalidSources) {
    let reason;
    assert.equal(
      await adapter.read(
        untouchedPage,
        { ...input(), ...withSource(source_json) },
        {
          onFailureReason: (value) => {
            reason = value;
          },
        },
      ),
      null,
    );
    assert.equal(reason, "source_ineligible");
  }
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
