import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import test from "node:test";
import { chromium } from "playwright";
import { createRunner } from "../src/runner.mjs";
import {
  kimiRecoveryBinding,
  recoveredKimiCompletion,
} from "../src/kimi-conversation-evidence.mjs";

const chat = "synthetic-owned-chat";
const account = "synthetic-account";
const service = "/apiv2/kimi.gateway.chat.v1.ChatService/";
const digest = (value) => createHash("sha256").update(value).digest("hex");
const source = () => ({
  messages: [
    { chat: { id: chat } },
    { message: { id: "system", role: "system" } },
    { message: { id: "question", role: "user", parentId: "system" } },
    {
      message: {
        id: "answer",
        role: "assistant",
        parentId: "question",
        status: "MESSAGE_STATUS_GENERATING",
      },
    },
  ],
  capture_status: { transport: "partial", termination: "timeout" },
});
const messages = () => [
  {
    id: "answer",
    role: "assistant",
    status: "MESSAGE_STATUS_COMPLETED",
    blocks: [{ text: { content: "Complete original answer." } }],
    references: [
      { items: [{ search: { url: "https://example.org/source" } }] },
    ],
  },
  { id: "question", role: "user", blocks: [{ text: { content: "Question" } }] },
  { id: "system", role: "system", status: "MESSAGE_STATUS_UNSPECIFIED" },
];
const history = () => ({
  status: "recovered",
  external_conversation_id: chat,
  chat: { chat: { id: chat } },
  message_pages: [{ messages: messages() }],
});
const inventory = [
  ["answer", "assistant"],
  ["question", "user"],
  ["system", "system"],
];
const input = (value = source()) => {
  const source_json = JSON.stringify(value);
  return {
    execution_id: randomUUID(),
    expected_identity: { provider: "kimi", platform_account_id: account },
    external_conversation_id: chat,
    source_capture_id: randomUUID(),
    source_receipt_sha256: "a".repeat(64),
    source_json,
    source_sha256: digest(source_json),
  };
};

test("Kimi recovery binds incomplete original transport to exact message identities", () => {
  const original = source();
  const binding = kimiRecoveryBinding(original, chat);
  assert.deepEqual(binding, { message_inventory: inventory });
  assert.deepEqual(recoveredKimiCompletion(binding, history(), chat), {
    outcome: "complete",
    message_inventory: inventory,
  });
  assert.equal(
    original.messages[3].message.status,
    "MESSAGE_STATUS_GENERATING",
  );
  assert.equal(kimiRecoveryBinding(original, "other-chat"), null);
  for (const mutate of [
    (value) => value.messages.shift(),
    (value) => (value.messages[0].chat.id = "other-chat"),
    (value) => (value.messages[3].message.chat_id = "other-chat"),
    (value) => delete value.messages[3].message.id,
    (value) => (value.messages[3].message.role = "unknown"),
    (value) => value.messages.push({ message: { id: "answer", role: "user" } }),
    (value) =>
      value.messages.push({ message: { id: "personal", role: "user" } }),
  ]) {
    const invalid = source();
    mutate(invalid);
    assert.equal(kimiRecoveryBinding(invalid, chat), null);
  }
});

test("Kimi recovered completion rejects unfinished, changed, added and missing turns", () => {
  const binding = kimiRecoveryBinding(source(), chat);
  for (const mutate of [
    (value) => (value.external_conversation_id = "other-chat"),
    (value) => (value.chat.chat.id = "other-chat"),
    (value) =>
      (value.message_pages[0].messages[0].status = "MESSAGE_STATUS_GENERATING"),
    (value) => delete value.message_pages[0].messages[0].status,
    (value) => (value.message_pages[0].messages[0].id = "other-answer"),
    (value) => (value.message_pages[0].messages[0].role = "user"),
    (value) => value.message_pages[0].messages.pop(),
    (value) => value.message_pages[0].messages.push(messages()[0]),
    (value) =>
      value.message_pages[0].messages.push({ id: "personal", role: "user" }),
    (value) => (value.message_pages[0].messages[2].status = "unknown"),
  ]) {
    const invalid = history();
    mutate(invalid);
    assert.equal(recoveredKimiCompletion(binding, invalid, chat), null);
  }
});

async function fixture(run) {
  const calls = [];
  const state = { account, messages: messages() };
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
              assert.equal(url.origin, "https://www.kimi.com");
              if (request.isNavigationRequest())
                return route.fulfill({
                  contentType: "text/html",
                  body: "<title>Fixture</title>",
                });
              const body = request.postDataJSON();
              calls.push({ path: url.pathname, body });
              assert.equal(request.method(), "POST");
              let response;
              if (url.pathname.endsWith("UserService/GetCurrentUser"))
                response = { user: { id: state.account, nickname: "Fixture" } };
              else if (url.pathname === `${service}GetChat`) {
                assert.equal(body.chat_id, chat);
                response = { chat: { id: chat } };
              } else {
                assert.equal(url.pathname, `${service}ListMessages`);
                assert.equal(body.chat_id, chat);
                response =
                  body.page_token === ""
                    ? {
                        messages: state.messages.slice(0, 1),
                        next_page_token: "next",
                        has_more: true,
                      }
                    : { messages: state.messages.slice(1), has_more: false };
              }
              if (state.drift && url.pathname.endsWith("ListMessages"))
                state.account = "other-account";
              return route.fulfill({
                contentType: "application/json",
                body: JSON.stringify(response),
              });
            });
            return context;
          },
        };
      },
    },
    platformAdapters: {
      kimi: {
        entry: "https://www.kimi.com",
        connectorVersion: "fixture.v1",
        operations: ["measure"],
        async identify() {
          return {
            platform_account_id: state.account,
            display_name: "Fixture",
          };
        },
      },
    },
  });
  await runner.create({
    session_id: "session",
    platform: "kimi",
    storage_state: {
      cookies: [],
      origins: [
        {
          origin: "https://www.kimi.com",
          localStorage: [
            { name: "access_token", value: "synthetic-access" },
            { name: "refresh_token", value: "synthetic-refresh" },
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

test("Kimi runner retains full paginated original history through the existing recovery contract", async () => {
  await fixture(async ({ runner, calls }) => {
    const request = input();
    const result = await runner.recoverConversationCompletion(
      "session",
      request,
    );
    assert.equal(result.status, "recovered");
    assert.deepEqual(result.completion, {
      outcome: "complete",
      message_inventory: inventory,
    });
    assert.deepEqual(result.recovery_identity, request.expected_identity);
    assert.equal(result.source_capture_id, request.source_capture_id);
    assert.equal(result.source_sha256, request.source_sha256);
    assert.equal(result.source_receipt_sha256, request.source_receipt_sha256);
    assert.equal(result.history_sha256, digest(result.history_json));
    const saved = JSON.parse(result.history_json);
    assert.equal(saved.observed_at, result.observed_at);
    assert.deepEqual(
      saved.message_pages.flatMap((page) => page.messages),
      messages(),
    );
    assert.equal(JSON.stringify(result).includes("synthetic-access"), false);
    assert.deepEqual(
      await runner.recoverConversationCompletion("session", request),
      result,
    );
    assert.deepEqual(
      calls.map(({ path }) => path.split("/").at(-1)),
      ["GetCurrentUser", "GetChat", "ListMessages", "ListMessages"],
    );
  });
});

test("Kimi runner preserves incomplete and personal history and rejects changed account", async () => {
  await fixture(async ({ runner, state, calls }) => {
    state.messages[0].status = "MESSAGE_STATUS_GENERATING";
    const incomplete = await runner.recoverConversationCompletion(
      "session",
      input(),
    );
    assert.equal(incomplete.status, "retained");
    assert.equal(incomplete.completion, undefined);
    state.messages = [...messages(), { id: "personal", role: "user" }];
    const appended = await runner.recoverConversationCompletion(
      "session",
      input(),
    );
    assert.equal(appended.status, "retained");
    assert.equal(appended.history_json, undefined);
    state.messages = messages();
    state.drift = true;
    const changed = await runner.recoverConversationCompletion(
      "session",
      input(),
    );
    assert.equal(changed.status, "retained");
    assert.equal(changed.diagnostic.code, "account_mismatch");
    assert.equal(changed.completion, undefined);
    assert.equal(
      calls.some(({ path }) => /Delete|Resume|Create|ChatStream/.test(path)),
      false,
    );
  });
});

test("Kimi runner refuses source ownership mismatch before reading history", async () => {
  await fixture(async ({ runner, calls }) => {
    const invalid = source();
    invalid.messages[0].chat.id = "other-chat";
    const result = await runner.recoverConversationCompletion(
      "session",
      input(invalid),
    );
    assert.equal(result.status, "retained");
    assert.deepEqual(result.diagnostic, {
      stage: "source",
      code: "source_ineligible",
    });
    assert.equal(calls.length, 0);
  });
});
