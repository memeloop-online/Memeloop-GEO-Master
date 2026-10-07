import { createHash } from "node:crypto";
import { captureConnectExchange } from "./connect-browser-capture.mjs";
import { interpretObservation } from "./ai-observation-parser.mjs";
import { extractWithSignedInBrowser } from "./browser-ai-extraction.mjs";

const ORIGIN = "https://www.kimi.com";
const CHAT_PATH = "/apiv2/kimi.gateway.chat.v1.ChatService/Chat";
const ID = /^[\w-]{1,128}$/u;
const MAX_ANSWER = 100_000;

const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const id = (value) => typeof value === "string" && ID.test(value);
const offset = (value) => {
  const text = String(value);
  return /^(?:0|[1-9]\d*)$/u.test(text) && BigInt(text) <= 18446744073709551615n
    ? text
    : null;
};

function requestBody(request, question, model) {
  try {
    const post = request.postDataBuffer();
    if (
      !post ||
      post.length < 7 ||
      post.length > 64_000 ||
      post[0] !== 0 ||
      post.readUInt32BE(1) !== post.length - 5 ||
      !/^application\/connect\+json(?:\s*;|$)/iu.test(
        request.headers()["content-type"] ?? "",
      )
    )
      return null;
    const body = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(post.subarray(5)),
    );
    // The current client includes its scheduling tool alongside the search
    // tool. This is still only request intent: the response must independently
    // prove an actual search, answer ownership, completion and citations.
    const search =
      body?.tools?.length === 1 && body.tools[0]?.type === "SEARCH"
        ? body.tools[0].search
        : body?.tools?.length === 2 &&
            body.tools[0]?.type === "TOOL_TYPE_SEARCH" &&
            body.tools[1]?.type === "TOOL_TYPE_CRON_JOB" &&
            !Object.hasOwn(body.tools[1], "search")
          ? body.tools[0].search
          : null;
    if (
      !object(body) ||
      body.options?.model !== model ||
      !Array.isArray(body.message?.blocks) ||
      body.message.blocks.length !== 1 ||
      !["user", 2].includes(body.message.role) ||
      body.message.blocks[0]?.text?.content !== question ||
      !Array.isArray(body.tools) ||
      !object(search) ||
      (search.force !== undefined && search.force !== false) ||
      (body.chat_id !== undefined && body.chat_id !== "")
    ) {
      return null;
    }
    return { ...body, chat_id: body.chat_id ?? "" };
  } catch {
    return null;
  }
}

export function matchesKimiChatRequest(request, question, model) {
  return requestBody(request, question, model) !== null;
}

function citationUrl(value) {
  if (typeof value !== "string" || Buffer.byteLength(value, "utf8") > 2048)
    return null;
  try {
    const url = new URL(value);
    if (
      !["http:", "https:"].includes(url.protocol) ||
      !url.hostname ||
      url.username ||
      url.password ||
      url.hostname === "localhost" ||
      url.hostname.endsWith(".localhost") ||
      /^(?:127\.|10\.|192\.168\.|169\.254\.|172\.(?:1[6-9]|2\d|3[01])\.)/u.test(
        url.hostname,
      ) ||
      ["[::1]", "[::]"].includes(url.hostname)
    )
      return null;
    return value;
  } catch {
    return null;
  }
}

function mergedText(envelope, previous = "") {
  const block = envelope.block;
  const text = block?.text?.content;
  if (typeof text !== "string") return null;
  // This is the client's block mask semantics, not a generic text delta:
  // only a full block set replaces prior content.
  if (
    (envelope.op === "set" || envelope.op === 1) &&
    typeof envelope.mask === "string" &&
    envelope.mask.split(",")[0] === "block"
  )
    return text;
  return previous + text;
}

/**
 * Interpret one bounded, successfully decoded Connect exchange. A search
 * setting or a transport end frame alone is never proof of a searched answer.
 * All identifiers come from this exchange; no provider request ID is inferred.
 */
export function reduceKimiConnectExchange(
  exchange,
  { question, model, request },
) {
  if (
    !Array.isArray(exchange?.messages) ||
    !request ||
    request.options?.model !== model ||
    request.message?.blocks?.[0]?.text?.content !== question ||
    !Number.isFinite(Date.parse(exchange.received_at))
  )
    return null;
  let chatId = request.chat_id || null;
  let sawChat = false;
  let answerId = null;
  let answerStatus = null;
  let search = null;
  const searches = new Map();
  const blocks = new Map();
  const citations = new Map();
  const citedIds = new Set();
  for (const envelope of exchange.messages) {
    if (!object(envelope)) return null;
    const payloads = ["chat", "message", "block", "ref", "done"].filter(
      (key) => envelope[key] != null,
    );
    if (payloads.length > 1 || envelope.error != null) return null;
    if (envelope.chat) {
      const observedId = envelope.chat.id;
      if (observedId === undefined) continue;
      if (!id(observedId) || (chatId && chatId !== observedId)) return null;
      chatId = observedId;
      sawChat = true;
    } else if (envelope.message) {
      const message = envelope.message;
      if (
        !id(message.id) ||
        (message.chat_id &&
          (!id(message.chat_id) || (chatId && message.chat_id !== chatId)))
      )
        return null;
      // A message may arrive before the chat envelope. Bind its declared
      // chat now so a later envelope cannot silently relabel that answer.
      chatId ??= message.chat_id || null;
      if (
        message.role === "assistant" ||
        message.role === 3 ||
        (answerId && message.id === answerId && message.role === undefined)
      ) {
        if (answerId && answerId !== message.id) return null;
        answerId = message.id;
        if (
          message.status !== undefined &&
          message.status !== "UNSPECIFIED" &&
          message.status !== 0
        )
          answerStatus = message.status;
        if (
          message.refs?.used_search_chunks !== undefined &&
          !Array.isArray(message.refs.used_search_chunks)
        )
          return null;
        if (Array.isArray(message.refs?.used_search_chunks)) {
          for (const chunk of message.refs.used_search_chunks) {
            if (!id(chunk?.id)) return null;
            citedIds.add(chunk.id);
            const url = citationUrl(chunk.base?.url);
            if (chunk.base?.url !== undefined && !url) return null;
            if (url) citations.set(chunk.id, { message_id: message.id, url });
          }
        }
      }
    } else if (envelope.block) {
      const block = envelope.block;
      if (!id(block.id) || !id(block.message_id)) return null;
      if (block.search) {
        const previousSearch = searches.get(block.id);
        if (
          !object(block.search) ||
          (block.search.keywords !== undefined &&
            !Array.isArray(block.search.keywords)) ||
          (block.search.web_pages !== undefined &&
            !Array.isArray(block.search.web_pages)) ||
          (previousSearch && previousSearch.message_id !== block.message_id)
        )
          return null;
        const hasActivity = Boolean(
          block.search.keywords?.some(
            (keyword) => typeof keyword === "string" && keyword.trim(),
          ) || block.search.web_pages?.length,
        );
        const eventOffset = offset(envelope.event_offset ?? 0);
        if (eventOffset === null) return null;
        const observedSearch = {
          block_id: block.id,
          message_id: block.message_id,
          event_offset: eventOffset,
        };
        searches.set(block.id, observedSearch);
        // Multi-step answers can execute several search rounds. Retain a
        // real active block as evidence, and validate every round's owner.
        if (hasActivity) search ??= observedSearch;
      } else if (block.text) {
        const current = blocks.get(block.id);
        if (current && current.message_id !== block.message_id) return null;
        const next = mergedText(envelope, current?.text);
        if (next === null || Buffer.byteLength(next, "utf8") > MAX_ANSWER)
          return null;
        blocks.set(block.id, { message_id: block.message_id, text: next });
      }
    } else if (envelope.ref) {
      const ref = envelope.ref;
      if (!id(ref.message_id)) return null;
      const source = ref.search;
      if (source?.id && id(source.id)) {
        const url = citationUrl(source.base?.url);
        if (source.base?.url !== undefined && !url) citations.delete(source.id);
        if (url) {
          citations.set(source.id, {
            message_id: ref.message_id,
            url,
            ...(typeof source.base?.title === "string" &&
            source.base.title.length <= 500
              ? { title: source.base.title }
              : {}),
          });
        }
      }
    }
  }
  if (
    !sawChat ||
    !id(chatId) ||
    !id(answerId) ||
    (answerStatus !== "COMPLETED" && answerStatus !== 2) ||
    !search
  )
    return null;
  if ([...searches.values()].some((item) => item.message_id !== answerId))
    return null;
  const answer = [...blocks.values()]
    .filter((block) => block.message_id === answerId)
    .map((block) => block.text)
    .join("");
  if (!answer.trim() || Buffer.byteLength(answer, "utf8") > MAX_ANSWER)
    return null;
  if (citedIds.size > 50) return null;
  const cited = [...citedIds]
    .map((sourceId) => citations.get(sourceId))
    .filter((value) => value?.message_id === answerId)
    .map(({ url }) => url);
  if (cited.length !== citedIds.size) return null;
  return {
    raw_answer: answer,
    citations: [...new Set(cited)],
    search_event: {
      kind: "official_search_event",
      source: "provider_connect_stream",
      provenance: "live",
      chat_id: chatId,
      message_id: answerId,
      block_id: search.block_id,
      event_offset: search.event_offset,
      observed_at: exchange.received_at,
      request_model: model,
      request_question_sha256: createHash("sha256")
        .update(question, "utf8")
        .digest("hex"),
    },
  };
}

/** Read the authenticated website menu without submitting a conversation. */
export async function inspectKimiMeasurementOptions(page) {
  const models = [];
  let selectedModel = null;
  const trigger = page.getByTestId("model-select-trigger");
  try {
    await trigger.click({ timeout: 5_000 });
    await page.getByTestId("model-option").first().waitFor({
      state: "visible",
      timeout: 5_000,
    });
    const options = await page.getByTestId("model-option").all();
    if (options.length > 64) return null;
    for (const option of options) {
      if (!(await option.isVisible())) continue;
      const id = await option.getAttribute("data-moon-key");
      const label = (await option.innerText()).trim().replace(/\s+/gu, " ");
      if (
        !/^[\w.-]{1,128}$/u.test(id ?? "") ||
        !label ||
        label.length > 200 ||
        models.some((model) => model.id === id)
      )
        return null;
      models.push({ id, label });
      const selected =
        (await option.getAttribute("aria-selected")) === "true" ||
        (await option.getAttribute("aria-checked")) === "true" ||
        (await option.getAttribute("data-state")) === "checked";
      if (selected) {
        if (selectedModel !== null) return null;
        selectedModel = id;
      }
    }
    return models.length ? { models, selected_model: selectedModel } : null;
  } catch (error) {
    if (error?.name === "TimeoutError") return null;
    throw error;
  } finally {
    await page.keyboard.press("Escape").catch(() => {});
  }
}

export async function configureKimiSearch(page, model, searchEnabled = true) {
  const selector = page.getByTestId("model-select-trigger");
  await selector.click({ timeout: 5_000 });
  await page.getByTestId("model-option").first().waitFor({
    state: "visible",
    timeout: 5_000,
  });
  let chosen = null;
  for (const option of await page.getByTestId("model-option").all()) {
    if ((await option.getAttribute("data-moon-key")) !== model) continue;
    if (chosen) return false;
    chosen = option;
  }
  if (!chosen) return "requested_model_unavailable";
  await chosen.click({ timeout: 5_000 });
  const toolkit = page.getByTestId("toolkit-trigger-btn");
  await toolkit.click({ timeout: 5_000 });
  await page.getByRole("menuitem", { name: "联网搜索" }).click({
    timeout: 5_000,
  });
  const auto = page.getByRole("menuitemradio", {
    name: searchEnabled ? "自动搜索" : "关闭搜索",
  });
  if ((await auto.getAttribute("aria-checked")) !== "true") {
    await auto.click({ timeout: 5_000 });
    await page.keyboard.press("Escape");
    await toolkit.click({ timeout: 5_000 });
    await page.getByRole("menuitem", { name: "联网搜索" }).click({
      timeout: 5_000,
    });
  }
  const searchOn = await auto.getAttribute("aria-checked").catch(() => null);
  await page.keyboard.press("Escape");
  return searchOn === "true";
}

/** Browser UI owns the request and cookies; the observer only checks it. */
export async function observeKimiConnectSearch(
  page,
  payload,
  {
    trustedOrigin = ORIGIN,
    timeoutMs = 30_000,
    deadlineAt,
    signal,
    interpret = interpretObservation,
  } = {},
) {
  if (
    !page ||
    !payload ||
    typeof payload.model !== "string" ||
    !/^[\w.-]{1,128}$/u.test(payload.model)
  )
    return null;
  let request = null;
  try {
    const origin = new URL(trustedOrigin);
    if (
      origin.origin !== trustedOrigin ||
      (trustedOrigin !== ORIGIN &&
        !(
          origin.protocol === "http:" &&
          ["localhost", "127.0.0.1", "[::1]"].includes(origin.hostname)
        ))
    )
      return null;
    await page.goto(`${trustedOrigin}/`, {
      waitUntil: "domcontentloaded",
      timeout: 12_000,
    });
    if (new URL(page.url()).origin !== trustedOrigin) return null;
    const configured = await configureKimiSearch(page, payload.model);
    if (configured !== true)
      return configured === "requested_model_unavailable"
        ? { reason: configured }
        : null;
    const composer = page.locator(
      '[role="textbox"][contenteditable="true"].chat-input-editor',
    );
    if (!(await composer.isVisible())) return null;
    if (signal?.aborted) return null;
    await composer.fill(payload.question);
    // The runner's monotonic deadline includes its identity and UI setup.
    // Standalone fixture calls retain their explicit/default capture timeout.
    if (deadlineAt !== undefined) {
      const remaining = deadlineAt - performance.now();
      if (!Number.isFinite(remaining) || remaining <= 0) return null;
      timeoutMs = Math.min(120_000, Math.max(1, Math.floor(remaining)));
    }
    if (signal?.aborted) return null;
    const captured = await captureConnectExchange(page, {
      endpoint: `${trustedOrigin}${CHAT_PATH}`,
      timeoutMs,
      signal,
      maxMessages: 32_768,
      matchRequest(candidate) {
        const body = requestBody(candidate, payload.question, payload.model);
        if (!body) return false;
        request = body;
        return true;
      },
      submit: async () => {
        const send = page.locator(".send-button-container");
        if (
          (await send.getAttribute("class"))?.split(/\s+/u).includes("disabled")
        )
          throw new Error("send disabled");
        await send.click({ timeout: 5_000 });
      },
    });
    if (!captured || !request || signal?.aborted) return null;
    const renderedAnswer = page.locator(
      ".chat-content-item-assistant .markdown-container:not(.toolcall-content-text) > .markdown",
    );
    const renderedText =
      (await renderedAnswer.count()) > 0
        ? await renderedAnswer.last().innerText()
        : undefined;
    const observation = await interpret(captured, {
      renderedText,
      signal,
      // Test fixtures may inject an interpreter; live execution always uses
      // grounded AI extraction, never the historical schema-specific reducer.
      question: payload.question,
      model: payload.model,
      request,
      browserExtract: (prompt) =>
        extractWithSignedInBrowser(page, prompt, {
          model: payload.model,
          trustedOrigin,
          // Leave part of the overall execution window for the configured
          // model fallback if the signed-in parsing conversation is slow.
          deadlineAt: Math.min(
            deadlineAt ?? performance.now() + 60_000,
            performance.now() +
              Math.max(
                1,
                ((deadlineAt ?? performance.now() + 90_000) -
                  performance.now()) *
                  0.65,
              ),
          ),
          signal,
          configureModel: (parserPage, model) =>
            configureKimiSearch(parserPage, model, false),
        }),
    });
    if (!observation || signal?.aborted) return null;
    return {
      raw_answer: observation.raw_answer,
      citations: observation.citations,
      extraction_audit: observation.audit,
      search_event: observation.search_event ?? {
        kind: "official_search_event",
        source: "provider_connect_stream_ai",
        provenance: "live",
        chat_id: observation.chat_id,
        message_id: observation.message_id,
        block_id: observation.block_id,
        extraction_model: observation.audit.model,
        extraction_prompt_version: observation.audit.prompt_version,
        source_sha256: observation.audit.source_sha256,
        observed_at: captured.received_at,
        request_model: payload.model,
        request_question_sha256: createHash("sha256")
          .update(payload.question, "utf8")
          .digest("hex"),
      },
    };
  } catch {
    return null;
  }
}
