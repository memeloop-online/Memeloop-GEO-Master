import assert from "node:assert/strict";
import { createServer } from "node:http";
import { test } from "node:test";
import { chromium } from "playwright";
import { measureKimi } from "../src/adapters.mjs";
import {
  matchesKimiChatRequest,
  observeKimiConnectSearch as observeCapturedSearch,
  reduceKimiConnectExchange,
} from "../src/kimi-connect-search.mjs";

// This suite exercises transport/UI ownership with a deterministic fixture
// interpreter. Live response interpretation is tested in the AI suites.
const observeKimiConnectSearch = (page, payload, options) =>
  observeCapturedSearch(page, payload, {
    ...options,
    interpret: async (exchange, context) =>
      reduceKimiConnectExchange(exchange, context),
  });

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

test("unknown adapter evidence keeps safe stage diagnostics without successful observation", async () => {
  let navigations = 0;
  const outcome = await measureKimi(
    {
      goto: async () => {
        navigations += 1;
        throw new Error(
          "synthetic-private-credential https://example.org/private",
        );
      },
    },
    frozen,
    { expectedAccountId: "synthetic-private-account" },
  );
  assert.equal(navigations, 1);
  assert.equal(outcome.status, "unknown");
  assert.equal(outcome.reason, "official_search_observation_unverified");
  assert.equal(Object.hasOwn(outcome, "raw_answer"), false);
  assert.deepEqual(outcome.evidence, [
    {
      kind: "observation_diagnostic",
      schema_version: "geo.observation.diagnostic.v1",
      stage: "navigation",
      code: "unexpected_exception",
    },
  ]);
  assert.doesNotMatch(
    JSON.stringify(outcome),
    /synthetic-private|example\.org/u,
  );
});

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
  const currentClient = {
    ...request,
    tools: [
      { type: "TOOL_TYPE_SEARCH", search: { force: false } },
      { type: "TOOL_TYPE_CRON_JOB" },
    ],
  };
  assert.equal(
    matchesKimiChatRequest(clientRequest(currentClient), QUESTION, MODEL),
    true,
    "the observed current UI includes a scheduling tool beside explicit search",
  );
  for (const tools of [
    [{ type: "TOOL_TYPE_SEARCH", search: {} }],
    [
      { type: "TOOL_TYPE_SEARCH", search: { force: true } },
      { type: "TOOL_TYPE_CRON_JOB" },
    ],
    [{ type: "TOOL_TYPE_SEARCH" }, { type: "TOOL_TYPE_CRON_JOB" }],
    [{ type: "TOOL_TYPE_CRON_JOB" }, { type: "TOOL_TYPE_SEARCH", search: {} }],
    [{ type: "TOOL_TYPE_SEARCH", search: {} }, { type: "OTHER_TOOL" }],
    [
      { type: "TOOL_TYPE_SEARCH", search: {} },
      { type: "TOOL_TYPE_CRON_JOB", search: {} },
    ],
    [
      { type: "TOOL_TYPE_SEARCH", search: {} },
      { type: "TOOL_TYPE_CRON_JOB" },
      { type: "OTHER_TOOL" },
    ],
  ]) {
    assert.equal(
      matchesKimiChatRequest(
        clientRequest({ ...currentClient, tools }),
        QUESTION,
        MODEL,
      ),
      false,
      "unobserved or unsafe tool combinations remain unverified",
    );
  }
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
  let responseDelayMs = 0;
  let omitEnd = false;
  let navigationDelayMs = 0;
  let abortOnRequest;
  const server = createServer((incoming, reply) => {
    if (incoming.url === "/" && incoming.method === "GET") {
      reply.writeHead(200, {
        "content-type": "text/html; charset=utf-8",
        "set-cookie": "session=fixture-only; HttpOnly; SameSite=Lax",
      });
      const html = alreadyEnabled
        ? pageHtml.replace('aria-checked="false"', 'aria-checked="true"')
        : pageHtml;
      if (navigationDelayMs)
        setTimeout(() => reply.end(html), navigationDelayMs);
      else reply.end(html);
      return;
    }
    if (
      incoming.url === "/apiv2/kimi.gateway.chat.v1.ChatService/Chat" &&
      incoming.method === "POST"
    ) {
      sends += 1;
      cookieSeen = incoming.headers.cookie === "session=fixture-only";
      if (abortOnRequest) setTimeout(() => abortOnRequest.abort(), 0);
      reply.writeHead(200, { "content-type": "application/connect+json" });
      const finish = () => {
        for (const event of messages) reply.write(frame(0, event));
        reply.end(omitEnd ? undefined : frame(2, {}));
      };
      if (responseDelayMs) setTimeout(finish, responseDelayMs);
      else finish();
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
    responseDelayMs = 75;
    const afterDefaultCapture = await observeKimiConnectSearch(page, frozen, {
      trustedOrigin: origin,
      timeoutMs: 10,
      deadlineAt: performance.now() + 6_000,
    });
    assert.equal(afterDefaultCapture?.raw_answer, "An answer.");
    assert.equal(sends, 3, "the runner's remaining budget bounds capture");
    navigationDelayMs = 150;
    assert.equal(
      await observeKimiConnectSearch(page, frozen, {
        trustedOrigin: origin,
        deadlineAt: performance.now() + 30,
      }),
      null,
      "a budget consumed during UI setup cannot submit",
    );
    navigationDelayMs = 0;
    assert.equal(sends, 3);
    assert.equal(
      await observeKimiConnectSearch(page, frozen, {
        trustedOrigin: origin,
        deadlineAt: performance.now() - 1,
      }),
      null,
      "an expired runner budget cannot submit another question",
    );
    assert.equal(sends, 3);
    const cancelledBeforeSetup = new AbortController();
    cancelledBeforeSetup.abort();
    assert.equal(
      await observeKimiConnectSearch(page, frozen, {
        trustedOrigin: origin,
        signal: cancelledBeforeSetup.signal,
      }),
      null,
    );
    assert.equal(sends, 3, "an aborted runner cannot submit");
    responseDelayMs = 0;
    omitEnd = true;
    const captureDiagnostics = [];
    assert.equal(
      await observeKimiConnectSearch(page, frozen, {
        trustedOrigin: origin,
        deadlineAt: performance.now() + 1_000,
        onDiagnostic: (entry) => captureDiagnostics.push(entry),
      }),
      null,
      "a streamed answer without the Connect end frame is not evidence",
    );
    assert.equal(captureDiagnostics.at(-1).stage, "capture");
    assert.ok(
      ["capture_unverified", "budget_exhausted"].includes(
        captureDiagnostics.at(-1).code,
      ),
    );
    assert.equal(sends, 4);
    omitEnd = false;
    responseDelayMs = 150;
    abortOnRequest = new AbortController();
    const responseListeners = page.listenerCount("response");
    assert.equal(
      await observeKimiConnectSearch(page, frozen, {
        trustedOrigin: origin,
        deadlineAt: performance.now() + 6_000,
        signal: abortOnRequest.signal,
      }),
      null,
      "a cancelled capture cannot accept a later valid response",
    );
    assert.equal(abortOnRequest.signal.aborted, true);
    assert.equal(sends, 5);
    assert.equal(page.listenerCount("response"), responseListeners);
    abortOnRequest = undefined;
    responseDelayMs = 0;
    const aiObserved = await observeCapturedSearch(page, frozen, {
      trustedOrigin: origin,
      timeoutMs: 5_000,
      interpret: async (captured, context) => {
        const found = reduceKimiConnectExchange(captured, context);
        return {
          raw_answer: found.raw_answer,
          citations: found.citations,
          chat_id: found.search_event.chat_id,
          message_id: found.search_event.message_id,
          block_id: found.search_event.block_id,
          audit: {
            kind: "observation_extraction",
            method: "llm_grounded",
            model: "extraction-model",
            prompt_version: "extract.v1",
            source_sha256: "b".repeat(64),
          },
        };
      },
    });
    assert.equal(aiObserved.raw_answer, "An answer.");
    assert.equal(aiObserved.search_event.source, "provider_connect_stream_ai");
    assert.equal(aiObserved.search_event.request_model, MODEL);
    assert.equal(aiObserved.search_event.extraction_model, "extraction-model");
    assert.equal(aiObserved.search_event.source_sha256, "b".repeat(64));
    assert.equal("event_offset" in aiObserved.search_event, false);
    assert.equal(sends, 6);
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
