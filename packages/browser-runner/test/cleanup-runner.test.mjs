import assert from "node:assert/strict";
import { randomUUID, createHash } from "node:crypto";
import { createServer } from "node:http";
import { test } from "node:test";
import { createRunner } from "../src/runner.mjs";
import { createRunnerServer } from "../src/server.mjs";

const account = "fixture-account";
const chat = "fixture-chat";
const inventory = createHash("sha256")
  .update(
    JSON.stringify([
      ["a", "assistant"],
      ["u", "user"],
    ]),
  )
  .digest("hex");
const input = (action = "reconcile") => ({
  execution_id: randomUUID(),
  expected_identity: { provider: "kimi", platform_account_id: account },
  external_conversation_id: chat,
  action,
  ...(action === "delete" ? { authorization_ticket: "aabb" } : {}),
});

async function fixture(run, options = {}) {
  const calls = [];
  const state = { account, missing: false, lost: false, blocked: null };
  const page = {
    goto: async () => {},
    url: () => "https://www.kimi.com/",
    async evaluate(_fn, args) {
      calls.push(args.selfPath ?? args.path);
      if (state.blocked) await state.blocked;
      if (state.throwEvaluate && !args.selfPath)
        throw new Error("private-exception-body");
      if (args.selfPath)
        return { user: { id: state.account, nickname: "Fixture" } };
      if (args.path.endsWith("GetChat"))
        return (
          state.chatResponse ??
          (state.missing
            ? { kind: "http_error", status: 404 }
            : { kind: "ok", data: { chat: { id: chat } } })
        );
      if (args.path.endsWith("ListMessages"))
        return (
          state.messagesResponse ?? {
            kind: "ok",
            data: {
              messages: [
                { id: "u", role: "user" },
                {
                  id: "a",
                  role: "assistant",
                  status: "COMPLETED",
                  text: "private-body",
                },
              ],
            },
          }
        );
      if (args.path.endsWith("DeleteChat"))
        return (
          state.deleteResponse ??
          (state.lost
            ? { kind: "transport_unknown" }
            : { kind: "ok", data: { chat_id: chat } })
        );
      throw new Error("unexpected operation");
    },
  };
  const runner = createRunner({
    browserType: {
      launch: async () => ({
        newContext: async () => ({
          newPage: async () => page,
          storageState: async () => ({ cookies: [], origins: [] }),
          close: async () => {},
        }),
        close: async () => {},
      }),
    },
    platformAdapters: {
      kimi: {
        entry: page.url(),
        identify: async () =>
          state.account
            ? { platform_account_id: state.account, display_name: "Fixture" }
            : null,
        operations: ["measure"],
        connectorVersion: "fixture.v1",
      },
    },
    ...options,
  });
  await runner.create({
    session_id: "session",
    platform: "kimi",
    storage_state: { cookies: [], origins: [] },
  });
  await runner.complete("session");
  try {
    await run({ runner, state, calls });
  } finally {
    await runner.shutdown();
  }
}

test("cleanup validates exact scope, needs authority, and never leaks recovered bodies", async () => {
  await fixture(async ({ runner, state, calls }) => {
    await assert.rejects(
      runner.cleanupConversation("session", {
        ...input(),
        authorized: true,
      }),
      /invalid_cleanup/,
    );
    await assert.rejects(
      runner.cleanupConversation("session", {
        ...input(),
        execution_id: "invalid",
      }),
      /invalid_cleanup/,
    );
    const unauthorized = await runner.cleanupConversation(
      "session",
      input("delete"),
    );
    assert.equal(unauthorized.status, "retained");
    assert.deepEqual(unauthorized.diagnostic, {
      stage: "authorization",
      code: "authorization_required",
    });
    assert.equal(calls.length, 0);
    const request = input();
    assert.deepEqual(await runner.cleanupConversation("session", request), {
      execution_id: request.execution_id,
      external_conversation_id: chat,
      status: "present",
    });
    assert.equal(
      calls.some((path) => path.endsWith("DeleteChat")),
      false,
    );
    state.missing = true;
    const missing = await runner.cleanupConversation("session", input());
    assert.equal(missing.status, "unknown");
    assert.deepEqual(missing.diagnostic, {
      stage: "inspection",
      code: "http_error",
    });
    state.account = "other-account";
    assert.equal(
      (await runner.cleanupConversation("session", input())).status,
      "retained",
    );
    state.account = null;
    assert.equal(
      (await runner.cleanupConversation("session", input())).status,
      "needs_login",
    );
  });
});

test("cleanup returns bounded diagnostics for existing failure branches without extra deletes", async () => {
  await fixture(async ({ runner, state, calls }) => {
    const authority = () => ({
      authorized: true,
      platform_account_id: account,
      external_conversation_id: chat,
      delete_not_after: new Date(Date.now() + 60_000).toISOString(),
      retained_message_inventory_sha256: inventory,
    });
    const cases = [
      {
        patch: { account: "other-account" },
        status: "retained",
        stage: "identity",
        code: "account_mismatch",
      },
      {
        patch: { account: null },
        status: "needs_login",
        stage: "identity",
        code: "reauth_required",
      },
      {
        patch: { missing: true },
        status: "unknown",
        stage: "inspection",
        code: "http_error",
      },
      {
        patch: {
          chatResponse: { kind: "ok", data: { chat: { id: "other-chat" } } },
        },
        status: "unknown",
        stage: "inspection",
        code: "chat_mismatch",
      },
      {
        patch: {
          chatResponse: {
            kind: "ok",
            data: { chat: { id: chat, status: "generating" } },
          },
        },
        status: "retained",
        stage: "inspection",
        code: "generating",
      },
      {
        authorize: async () => {
          throw new Error("private-callback-body");
        },
        status: "retained",
        stage: "authorization",
        code: "authorization_required",
      },
      {
        authorize: async () => ({
          ...authority(),
          platform_account_id: "other-account",
        }),
        status: "retained",
        stage: "authorization",
        code: "authorization_required",
      },
      {
        authorize: async () => ({
          ...authority(),
          retained_message_inventory_sha256: undefined,
        }),
        status: "retained",
        stage: "authorization",
        code: "authorization_required",
      },
      {
        authorize: async () => ({
          ...authority(),
          delete_not_after: new Date(0).toISOString(),
        }),
        status: "retained",
        stage: "authorization",
        code: "authorization_expired",
      },
      {
        authorize: async () => ({
          ...authority(),
          delete_not_after: new Date(Date.now() + 1_000).toISOString(),
        }),
        status: "retained",
        stage: "authorization",
        code: "authorization_expired",
      },
      {
        patch: { messagesResponse: { kind: "ok", data: { messages: null } } },
        status: "retained",
        stage: "messages",
        code: "unverified_messages",
      },
      {
        patch: {
          messagesResponse: {
            kind: "ok",
            data: { messages: [], has_more: true },
          },
        },
        status: "retained",
        stage: "messages",
        code: "pagination_incomplete",
      },
      {
        patch: { messagesResponse: { kind: "ok", data: { messages: [] } } },
        status: "retained",
        stage: "inventory",
        code: "message_inventory_mismatch",
      },
      {
        patch: {
          deleteResponse: { kind: "ok", data: { chat_id: "other-chat" } },
        },
        status: "unknown",
        stage: "delete",
        code: "unverified_delete_response",
        deletes: 1,
      },
      {
        patch: { lost: true },
        status: "unknown",
        stage: "delete",
        code: "transport_unknown",
        deletes: 1,
      },
    ];
    for (const kind of [
      "wrong_origin",
      "invalid_response",
      "http_error",
      "too_large",
      "transport_unknown",
      "reauth_required",
    ]) {
      for (const [field, stage, status] of [
        ["chatResponse", "inspection", "unknown"],
        ["messagesResponse", "messages", "retained"],
        ["deleteResponse", "delete", "unknown"],
      ])
        cases.push({
          patch: { [field]: { kind, detail: "private-response-body" } },
          status: kind === "reauth_required" ? "needs_login" : status,
          stage,
          code: kind,
          deletes: stage === "delete" ? 1 : 0,
        });
    }
    for (const example of cases) {
      Object.assign(state, {
        account,
        missing: false,
        lost: false,
        chatResponse: undefined,
        messagesResponse: undefined,
        deleteResponse: undefined,
        ...example.patch,
      });
      calls.length = 0;
      const result = await runner.cleanupConversation(
        "session",
        input("delete"),
        example.authorize ?? (async () => authority()),
      );
      assert.equal(result.status, example.status, JSON.stringify(example));
      assert.deepEqual(result.diagnostic, {
        stage: example.stage,
        code: example.code,
      });
      assert.deepEqual(Object.keys(result).sort(), [
        "diagnostic",
        "execution_id",
        "external_conversation_id",
        "status",
      ]);
      assert.equal(
        calls.filter((path) => path.endsWith("DeleteChat")).length,
        example.deletes ?? 0,
      );
      assert.equal(JSON.stringify(result).includes("private-"), false);
    }
  });
});

test("cleanup never forwards arbitrary provider reasons or exception messages", async () => {
  await fixture(async ({ runner, state, calls }) => {
    state.chatResponse = { kind: "private-response-body" };
    const unknown = await runner.cleanupConversation("session", input());
    assert.equal(unknown.status, "unknown");
    assert.equal(unknown.diagnostic, undefined);
    assert.equal(JSON.stringify(unknown).includes("private-"), false);
    state.throwEvaluate = true;
    const thrown = await runner.cleanupConversation("session", input());
    assert.equal(thrown.status, "unknown");
    assert.deepEqual(thrown.diagnostic, {
      stage: "inspection",
      code: "transport_unknown",
    });
    assert.equal(JSON.stringify(thrown).includes("private-"), false);
    assert.equal(
      calls.some((path) => path.endsWith("DeleteChat")),
      false,
    );
  });
});

test("runner requests strict absence proof only for reconciliation", async () => {
  await fixture(async ({ runner, state, calls }) => {
    state.chatResponse = { kind: "not_found" };
    const reconciled = await runner.cleanupConversation("session", input());
    assert.equal(reconciled.status, "absent");
    assert.equal(reconciled.diagnostic, undefined);
    const deleted = await runner.cleanupConversation(
      "session",
      input("delete"),
      async () => {
        throw new Error("absence must not invoke delete authorization");
      },
    );
    assert.equal(deleted.status, "unknown");
    assert.equal(
      calls.some((path) => path.endsWith("DeleteChat")),
      false,
    );
    assert.equal(
      calls.some((path) => path.endsWith("ListMessages")),
      false,
    );
  });
});

test("delete authorizes exact session and does not retry ambiguous outcomes", async () => {
  await fixture(async ({ runner, state, calls }) => {
    let authorizations = 0;
    const authorize = async (body) => {
      authorizations++;
      assert.deepEqual(body, {
        schema_version: 1,
        authorization_ticket: "aabb",
        runner_session_id: "session",
        platform_account_id: account,
        external_conversation_id: chat,
      });
      return {
        authorized: true,
        platform_account_id: account,
        external_conversation_id: chat,
        delete_not_after: new Date(Date.now() + 60_000).toISOString(),
        retained_message_inventory_sha256: inventory,
      };
    };
    assert.equal(
      (
        await runner.cleanupConversation(
          "session",
          input("delete"),
          async () => ({
            authorized: true,
            platform_account_id: "wrong",
            external_conversation_id: chat,
            delete_not_after: new Date(Date.now() + 60_000).toISOString(),
            retained_message_inventory_sha256: inventory,
          }),
        )
      ).status,
      "retained",
    );
    for (const delete_not_after of [undefined, new Date(0).toISOString()]) {
      assert.equal(
        (
          await runner.cleanupConversation(
            "session",
            input("delete"),
            async () => ({
              authorized: true,
              platform_account_id: account,
              external_conversation_id: chat,
              delete_not_after,
            }),
          )
        ).status,
        "retained",
      );
    }
    assert.equal(
      calls.some((path) => path.endsWith("DeleteChat")),
      false,
    );
    state.lost = true;
    const request = input("delete");
    assert.equal(
      (await runner.cleanupConversation("session", request, authorize)).status,
      "unknown",
    );
    assert.equal(
      (await runner.cleanupConversation("session", request, authorize)).status,
      "unknown",
    );
    assert.equal(authorizations, 1);
    assert.equal(calls.filter((path) => path.endsWith("DeleteChat")).length, 1);
    await assert.rejects(
      runner.cleanupConversation("session", {
        ...request,
        action: "reconcile",
      }),
      /execution_conflict/,
    );
  });
});

test("deadline retains the session reservation until underlying work settles", async () => {
  await fixture(
    async ({ runner, state, calls }) => {
      let release;
      state.blocked = new Promise((resolve) => {
        release = resolve;
      });
      const request = input("delete");
      let authorized = false;
      const pending = runner.cleanupConversation(
        "session",
        request,
        async () => {
          authorized = true;
          return {
            authorized: true,
            platform_account_id: account,
            external_conversation_id: chat,
          };
        },
      );
      const expired = await pending;
      assert.equal(expired.status, "unknown");
      assert.deepEqual(expired.diagnostic, {
        stage: "deadline",
        code: "deadline_exceeded",
      });
      await assert.rejects(runner.close("session"), /session_busy/);
      await assert.rejects(
        runner.cleanupConversation("session", input()),
        /session_busy/,
      );
      release();
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(authorized, false);
      assert.equal(
        calls.some((path) => path.endsWith("DeleteChat")),
        false,
      );
      await runner.close("session");
    },
    { executionTimeoutMs: 20 },
  );
});

test("an early cleanup timer wakeup cannot report deadline expiry", async (t) => {
  await fixture(
    async ({ runner, state, calls }) => {
      let now = 1_000;
      const timers = [];
      const cleared = [];
      t.mock.method(performance, "now", () => now);
      t.mock.method(globalThis, "setTimeout", (callback, delay) => {
        const timer = { callback, delay };
        timers.push(timer);
        return timer;
      });
      t.mock.method(globalThis, "clearTimeout", (timer) => cleared.push(timer));
      let release;
      state.blocked = new Promise((resolve) => (release = resolve));
      let authorized = false;
      const pending = runner.cleanupConversation(
        "session",
        input("delete"),
        async () => {
          authorized = true;
          return { authorized: true };
        },
      );
      let settled = false;
      void pending.then(() => (settled = true));
      try {
        await new Promise((resolve) => setImmediate(resolve));
        assert.ok(calls.length > 0, "inspection is blocked in page.evaluate");
        assert.equal(timers.length, 1);
        assert.equal(timers[0].delay, 20);
        // Exercise an early wakeup without relying on host timer precision.
        now = 1_019.75;
        timers[0].callback();
        await new Promise((resolve) => setImmediate(resolve));
        assert.equal(settled, false);
        assert.equal(timers.length, 2);
        assert.equal(timers[1].delay, 1);
        await assert.rejects(runner.close("session"), /session_busy/);
        now = 1_020;
        timers[1].callback();
        const expired = await pending;
        assert.equal(expired.status, "unknown");
        assert.deepEqual(expired.diagnostic, {
          stage: "deadline",
          code: "deadline_exceeded",
        });
        await assert.rejects(runner.close("session"), /session_busy/);
        await assert.rejects(
          runner.cleanupConversation("session", input()),
          /session_busy/,
        );
        release();
        await new Promise((resolve) => setImmediate(resolve));
        assert.equal(authorized, false);
        assert.equal(
          calls.some((path) => path.endsWith("DeleteChat")),
          false,
        );
        assert.ok(cleared.includes(timers[1]));
        await runner.close("session");
      } finally {
        release();
      }
    },
    { executionTimeoutMs: 20 },
  );
});

test("HTTP cleanup is service authenticated and uses only fixed authorization callback", async () => {
  await fixture(async ({ runner }) => {
    const callbackRequests = [];
    const callback = createServer(async (request, response) => {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      callbackRequests.push({
        path: request.url,
        token: request.headers.authorization,
        body: JSON.parse(Buffer.concat(chunks)),
      });
      response.setHeader("content-type", "application/json");
      response.end(
        JSON.stringify({
          authorized: true,
          platform_account_id: account,
          external_conversation_id: chat,
          delete_not_after: new Date(Date.now() + 60_000).toISOString(),
          retained_message_inventory_sha256: inventory,
        }),
      );
    });
    await new Promise((resolve) => callback.listen(0, "127.0.0.1", resolve));
    const server = createRunnerServer({
      runner,
      token: "fixture-token",
      callbackOrigin: `http://127.0.0.1:${callback.address().port}`,
      callbackToken: "x".repeat(32),
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const url = `http://127.0.0.1:${server.address().port}/v1/sessions/session/cleanup-conversation`;
    try {
      assert.equal((await fetch(url, { method: "POST" })).status, 401);
      const response = await fetch(url, {
        method: "POST",
        headers: {
          authorization: "Bearer fixture-token",
          "content-type": "application/json",
        },
        body: JSON.stringify(input("delete")),
      });
      assert.equal(response.status, 200);
      assert.equal((await response.json()).status, "deleted");
      assert.equal(callbackRequests.length, 1);
      assert.equal(
        callbackRequests[0].path,
        "/internal/v1/provider-conversation-cleanup/authorize",
      );
      assert.equal(callbackRequests[0].token, `Bearer ${"x".repeat(32)}`);
      assert.equal(callbackRequests[0].body.runner_session_id, "session");
    } finally {
      await new Promise((resolve) => server.close(resolve));
      await new Promise((resolve) => callback.close(resolve));
    }
  });
});
