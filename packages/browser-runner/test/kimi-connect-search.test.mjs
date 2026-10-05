import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import { measureKimi } from "../src/adapters.mjs";
import {
  matchesKimiChatRequest,
  observeKimiConnectSearch,
  reduceKimiConnectExchange,
} from "../src/kimi-connect-search.mjs";

const QUESTION = "What does this example mean?";
const MODEL = "example-model";
const frozen = {
  target_id: "11111111-1111-4111-8111-111111111111",
  account_id: "22222222-2222-4222-8222-222222222222",
  provider: "kimi",
  model: MODEL,
  surface: "consumer_web",
  search_mode: "web_search",
  protocol_version: "v1",
  question_set_version: "v1",
  question: QUESTION,
  market: "test-market",
  language: "en",
  scheduled_at: "2026-01-01T00:00:00Z",
  sample_ordinal: 0,
};
const request = {
  tools: [{ type: "SEARCH", search: {} }],
  message: { role: "user", blocks: [{ text: { content: QUESTION } }] },
  options: { model: MODEL },
  chat_id: "",
};
const messages = [
  { chat: { id: "chat-1" } },
  { message: { id: "assistant-1", role: "assistant", status: "GENERATING" } },
  {
    block: {
      id: "search-block-1",
      message_id: "assistant-1",
      search: { keywords: ["example"], web_pages: [] },
    },
    event_offset: "4",
  },
  {
    block: {
      id: "answer-block",
      message_id: "assistant-1",
      text: { content: "An " },
    },
    op: "set",
    mask: "block",
  },
  {
    ref: {
      id: "ref-1",
      message_id: "assistant-1",
      search: {
        id: "chunk-1",
        base: { url: "https://example.org/cited", title: "Cited" },
        ref_index: 1,
      },
    },
  },
  {
    message: {
      id: "assistant-1",
      role: "assistant",
      refs: {
        used_search_chunks: [
          {
            id: "chunk-1",
            base: { url: "https://example.org/cited" },
            ref_index: 1,
          },
        ],
      },
    },
  },
  {
    block: {
      id: "answer-block",
      message_id: "assistant-1",
      text: { content: "answer." },
    },
    op: "append",
  },
  { message: { id: "assistant-1", status: "COMPLETED" } },
  { done: {} },
];
const exchange = {
  messages,
  started_at: "2026-01-01T00:00:00.000Z",
  received_at: "2026-01-01T00:00:01.000Z",
};

test("reduces one searched completed assistant message and cited chunks only", () => {
  const found = reduceKimiConnectExchange(exchange, {
    question: QUESTION,
    model: MODEL,
    request,
  });
  assert.equal(found.raw_answer, "An answer.");
  assert.deepEqual(found.citations, ["https://example.org/cited"]);
  assert.deepEqual(found.search_event, {
    kind: "official_search_event",
    source: "provider_connect_stream",
    provenance: "live",
    chat_id: "chat-1",
    message_id: "assistant-1",
    block_id: "search-block-1",
    event_offset: "4",
    observed_at: exchange.received_at,
    request_model: MODEL,
    request_question_sha256:
      "431a62b35d7b8c614e18530c9778e12a2deb822eafa5d833eb843db41f103432",
  });
});

test("rejects missing search/completion, answer from other message, failed final status, and mismatched frozen request", () => {
  const cases = [
    messages.filter((item) => !item.block?.search),
    messages.map((item) =>
      item.block?.search
        ? { ...item, block: { ...item.block, search: {} } }
        : item,
    ),
    messages.filter((item) => item.message?.status !== "COMPLETED"),
    messages.map((item) =>
      item.block?.search
        ? { ...item, block: { ...item.block, message_id: "other-assistant" } }
        : item,
    ),
    [...messages, { message: { id: "assistant-1", status: "ERROR" } }],
    messages.filter((item) => !item.chat),
    messages.map((item) =>
      item.message?.role === "assistant"
        ? { ...item, message: { ...item.message, chat_id: "other-chat" } }
        : item,
    ),
  ];
  for (const list of cases) {
    assert.equal(
      reduceKimiConnectExchange(
        { ...exchange, messages: list },
        { question: QUESTION, model: MODEL, request },
      ),
      null,
    );
  }
  assert.equal(
    reduceKimiConnectExchange(exchange, {
      question: QUESTION + "changed",
      model: MODEL,
      request,
    }),
    null,
  );
});

function frame(flag, data) {
  const bytes = Buffer.from(JSON.stringify(data), "utf8");
  const header = Buffer.alloc(5);
  header[0] = flag;
  header.writeUInt32BE(bytes.length, 1);
  return Buffer.concat([header, bytes]);
}

test("framed UI request must preserve frozen question, model, fresh chat and explicit search tool", () => {
  const clientRequest = (body) => ({
    postDataBuffer: () => frame(0, body),
    headers: () => ({ "content-type": "application/connect+json" }),
  });
  assert.equal(
    matchesKimiChatRequest(clientRequest(request), QUESTION, MODEL),
    true,
    "protobuf JSON may omit false and empty scalar defaults",
  );
  for (const wrong of [
    { ...request, chat_id: "existing-chat" },
    { ...request, tools: [] },
    { ...request, tools: [{ type: "SEARCH", search: { force: true } }] },
    { ...request, options: { model: "other" } },
    {
      ...request,
      message: {
        ...request.message,
        blocks: [...request.message.blocks, { text: { content: "extra" } }],
      },
    },
  ]) {
    assert.equal(
      matchesKimiChatRequest(clientRequest(wrong), QUESTION, MODEL),
      false,
    );
  }
  assert.equal(
    matchesKimiChatRequest(
      {
        ...clientRequest(request),
        postDataBuffer: () => Buffer.from(JSON.stringify(request), "utf8"),
      },
      QUESTION,
      MODEL,
    ),
    false,
  );
});

test("late chat identity cannot legitimize a message from another chat", () => {
  const reordered = structuredClone(messages);
  const chat = reordered.shift();
  reordered[0].message.chat_id = "different-chat";
  reordered.splice(1, 0, chat);
  assert.equal(
    reduceKimiConnectExchange(
      { ...exchange, messages: reordered },
      { question: QUESTION, model: MODEL, request },
    ),
    null,
  );
  reordered[0].message.chat_id = "chat-1";
  assert.equal(
    reduceKimiConnectExchange(
      { ...exchange, messages: reordered },
      { question: QUESTION, model: MODEL, request },
    )?.raw_answer,
    "An answer.",
  );
});

test("invalid cited URL cannot silently fall back to an earlier valid ref", () => {
  const changed = structuredClone(messages);
  const cited = changed.find((event) => event.message?.refs);
  cited.message.refs.used_search_chunks[0].base.url = "javascript:alert(1)";
  assert.equal(
    reduceKimiConnectExchange(
      { ...exchange, messages: changed },
      { question: QUESTION, model: MODEL, request },
    ),
    null,
  );
  const lateInvalid = structuredClone(messages);
  lateInvalid.push({
    ref: {
      message_id: "assistant-1",
      search: { id: "chunk-1", base: { url: "javascript:alert(1)" } },
    },
  });
  assert.equal(
    reduceKimiConnectExchange(
      { ...exchange, messages: lateInvalid },
      { question: QUESTION, model: MODEL, request },
    ),
    null,
  );
});

test("multiple search rounds are valid only when every block belongs to the same answer", () => {
  const several = structuredClone(messages);
  several.splice(3, 0, {
    block: {
      id: "search-block-2",
      message_id: "assistant-1",
      search: { keywords: ["second search"] },
    },
    event_offset: "5",
  });
  const reduce = () =>
    reduceKimiConnectExchange(
      { ...exchange, messages: several },
      { question: QUESTION, model: MODEL, request },
    );
  assert.equal(reduce()?.raw_answer, "An answer.");
  assert.equal(reduce()?.search_event.block_id, "search-block-1");
  several[3].block.message_id = "different-assistant";
  assert.equal(reduce(), null);
});

const pageHtml = `<!doctype html><html><body>
<button data-testid="model-select-trigger">Model</button>
<button data-testid="model-option" data-moon-key="example-model">Example</button>
<button data-testid="toolkit-trigger-btn">Tools</button>
<button role="menuitem">联网搜索</button>
<button role="menuitemradio" aria-checked="false">自动搜索</button>
<div role="textbox" contenteditable="true" class="chat-input-editor"></div>
<button class="send-button-container">Send</button>
<script>
  let enabled = document.querySelector('[role="menuitemradio"]').getAttribute("aria-checked") === "true";
  let selectedModel = "";
  document.querySelector('[data-testid="model-option"]').onclick = () => {
    selectedModel = "example-model";
  };
  document.querySelector('[role="menuitemradio"]').onclick = function () {
    enabled = true;
    this.setAttribute("aria-checked", "true");
  };
  document.querySelector(".send-button-container").onclick = async () => {
    const question = document.querySelector(".chat-input-editor").textContent;
    const body = {
      tools: enabled ? [{ type: "SEARCH", search: {} }] : [],
      message: { role: "user", blocks: [{ text: { content: question } }] },
      options: { model: selectedModel }
    };
    const bytes = new TextEncoder().encode(JSON.stringify(body));
    const frame = new Uint8Array(5 + bytes.length);
    new DataView(frame.buffer).setUint32(1, bytes.length);
    frame.set(bytes, 5);
    await fetch("/apiv2/kimi.gateway.chat.v1.ChatService/Chat", {
      method: "POST",
      headers: { "Content-Type": "application/connect+json" },
      body: frame
    });
  };
</script></body></html>`;

test("browser UI submits exactly once and its captured framed request creates v2 receipt", async () => {
  let sends = 0;
  let cookieSeen = false;
  let alreadyEnabled = false;
  const server = createServer((incoming, reply) => {
    if (incoming.url === "/" && incoming.method === "GET") {
      reply.writeHead(200, {
        "content-type": "text/html; charset=utf-8",
        "set-cookie": "session=fixture-only; HttpOnly; SameSite=Lax",
      });
      reply.end(
        alreadyEnabled
          ? pageHtml.replace('aria-checked="false"', 'aria-checked="true"')
          : pageHtml,
      );
      return;
    }
    if (
      incoming.url === "/apiv2/kimi.gateway.chat.v1.ChatService/Chat" &&
      incoming.method === "POST"
    ) {
      sends += 1;
      cookieSeen = incoming.headers.cookie === "session=fixture-only";
      reply.writeHead(200, { "content-type": "application/connect+json" });
      for (const event of messages) reply.write(frame(0, event));
      reply.end(frame(2, {}));
      return;
    }
    reply.writeHead(404).end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  let browser;
  try {
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
    });
    const page = await browser.newPage();
    const origin = `http://127.0.0.1:${server.address().port}`;
    let requestMatches = false;
    page.on("request", (item) => {
      if (item.url().endsWith("/Chat"))
        requestMatches = matchesKimiChatRequest(item, QUESTION, MODEL);
    });
    const observed = await observeKimiConnectSearch(page, frozen, {
      trustedOrigin: origin,
      timeoutMs: 5_000,
    });
    assert.equal(sends, 1);
    assert.equal(requestMatches, true);
    assert.equal(observed?.raw_answer, "An answer.");
    assert.equal(cookieSeen, true);
    assert.equal(JSON.stringify(observed).includes("fixture-only"), false);
    alreadyEnabled = true;
    const alreadyOn = await observeKimiConnectSearch(page, frozen, {
      trustedOrigin: origin,
      timeoutMs: 5_000,
    });
    assert.equal(alreadyOn?.raw_answer, "An answer.");
    assert.equal(sends, 2, "each independent observation submits once");
    // Runner stays fixture-stamped; a synthetic trace cannot verify a real account.
    assert.equal(
      (await measureKimi(null, frozen, { expectedAccountId: "fixture" }))
        .status,
      "unsupported",
    );
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
