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
      if (args.selfPath)
        return { user: { id: state.account, nickname: "Fixture" } };
      if (args.path.endsWith("GetChat"))
        return state.missing
          ? { kind: "http_error", status: 404 }
          : { kind: "ok", data: { chat: { id: chat } } };
      if (args.path.endsWith("ListMessages"))
        return {
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
        };
      if (args.path.endsWith("DeleteChat"))
        return state.lost
          ? { kind: "transport_unknown" }
          : { kind: "ok", data: { chat_id: chat } };
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
    assert.equal(
      (await runner.cleanupConversation("session", input("delete"))).status,
      "retained",
    );
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
    assert.equal(
      (await runner.cleanupConversation("session", input())).status,
      "unknown",
    );
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
      assert.equal((await pending).status, "unknown");
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
