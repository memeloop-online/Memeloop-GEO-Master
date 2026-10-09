import { createParser } from "eventsource-parser";

// Source-derived from the public website bundle, not live-account acceptance.
// The browser retains its own authentication, PoW and request implementation.
export const DEEPSEEK_WEB_ORIGIN = "https://chat.deepseek.com";
const COMPLETION = `${DEEPSEEK_WEB_ORIGIN}/api/v0/chat/completion`;
const MAX_BYTES = 750_000;
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const label = (value) =>
  typeof value === "string" && /^[a-zA-Z0-9_.-]{1,128}$/u.test(value);
const messageId = (value) => Number.isSafeInteger(value) && value >= 0;
const fail = () => {
  throw new Error("deepseek_capture_unverified");
};

/**
 * The integration supplies a reader of the *current page's* model configuration,
 * not a hard-coded model list or a copied default. Missing reader is unsupported.
 * Public website code filters model_configs by enabled && switchable.
 * This function does not open menus, change settings or submit a question.
 */
export async function inspectDeepSeekMeasurementOptions(
  page,
  { readConfiguration } = {},
) {
  try {
    if (
      new URL(page.url()).origin !== DEEPSEEK_WEB_ORIGIN ||
      typeof readConfiguration !== "function"
    )
      return null;
    const config = await readConfiguration(page);
    if (
      new URL(page.url()).origin !== DEEPSEEK_WEB_ORIGIN ||
      !object(config) ||
      !Array.isArray(config.model_configs) ||
      config.model_configs.length > 64
    )
      return null;
    const models = [];
    for (const item of config.model_configs) {
      if (!object(item)) return null;
      if (item.enabled !== true || item.switchable !== true) continue;
      if (
        !label(item.model_type) ||
        models.some((model) => model.id === item.model_type)
      )
        return null;
      // No guessed friendly name. The observed model_type is itself a label.
      models.push({ id: item.model_type, label: item.model_type });
    }
    const selected = config.selected_model ?? null;
    if (
      !models.length ||
      (selected !== null && !models.some((model) => model.id === selected))
    )
      return null;
    return { models, selected_model: selected };
  } catch {
    return null;
  }
}

function validBinding(binding) {
  return (
    object(binding) &&
    label(binding.model) &&
    typeof binding.question === "string" &&
    binding.question.trim().length > 0 &&
    Buffer.byteLength(binding.question) <= 16_000 &&
    typeof binding.chatSessionId === "string" &&
    /^[\w-]{1,128}$/u.test(binding.chatSessionId) &&
    binding.parentMessageId === null &&
    typeof binding.searchEnabled === "boolean" &&
    typeof binding.thinkingEnabled === "boolean"
  );
}

/**
 * Match only a fresh ordinary submission from the browser. Regenerate,
 * continue, attachments and parent-message reuse are outside this first slice.
 * The session ID must come from the integration's observed new-session flow;
 * matching it does not itself establish fresh-conversation ownership.
 */
export function matchesDeepSeekSubmission(request, binding) {
  if (!validBinding(binding)) return false;
  try {
    if (
      request.url !== COMPLETION ||
      request.method !== "POST" ||
      typeof request.postData !== "string" ||
      Buffer.byteLength(request.postData) > 64_000
    )
      return false;
    const body = JSON.parse(request.postData);
    return (
      object(body) &&
      body.chat_session_id === binding.chatSessionId &&
      body.parent_message_id === null &&
      body.model_type === binding.model &&
      body.prompt === binding.question &&
      body.search_enabled === binding.searchEnabled &&
      body.thinking_enabled === binding.thinkingEnabled &&
      Array.isArray(body.ref_file_ids) &&
      body.ref_file_ids.length === 0 &&
      (body.action === undefined || body.action === null) &&
      body.child_message_id === undefined &&
      body.message_id === undefined
    );
  } catch {
    return false;
  }
}

/**
 * Maintained SSE parser handles framing, UTF-8 is decoded strictly here.
 * Keep raw event JSON. Do not merge deltas, extract answer text or interpret
 * citations/search semantics. ready + finish + EOF prove transport boundaries
 * only, not successful answering, web search, or authorization to clean up.
 */
export function createDeepSeekEvidenceDecoder({
  model,
  maxBytes = MAX_BYTES,
  maxEvents = 4096,
} = {}) {
  if (
    !label(model) ||
    !Number.isSafeInteger(maxBytes) ||
    maxBytes < 1 ||
    maxBytes > MAX_BYTES ||
    !Number.isSafeInteger(maxEvents) ||
    maxEvents < 1 ||
    maxEvents > 32_768
  )
    fail();
  const messages = [];
  const utf8 = new TextDecoder("utf-8", { fatal: true });
  let size = 0;
  let tail = "";
  let ready = false;
  let finished = false;
  let sealed = false;
  let rejected = false;
  const parser = createParser({
    maxBufferSize: maxBytes,
    onError: fail,
    onEvent(event) {
      if (messages.length >= maxEvents) fail();
      const data = JSON.parse(event.data);
      if (!object(data)) fail();
      if (event.event === "ready") {
        if (
          ready ||
          finished ||
          !messageId(data.request_message_id) ||
          !messageId(data.response_message_id) ||
          data.request_message_id === data.response_message_id ||
          (data.model_type !== undefined &&
            data.model_type !== null &&
            data.model_type !== model)
        )
          fail();
        ready = true;
      } else if (event.event === "finish") {
        if (!ready || finished) fail();
        finished = true;
      } else if (event.event === "delta" && (!ready || finished)) {
        fail();
      }
      messages.push({
        event: event.event ?? "message",
        ...(event.id === undefined ? {} : { id: event.id }),
        data,
      });
    },
  });
  return {
    push(bytes) {
      if (sealed || rejected) fail();
      try {
        if (!(bytes instanceof Uint8Array)) fail();
        size += bytes.byteLength;
        if (size > maxBytes) fail();
        const text = utf8.decode(bytes, { stream: true });
        tail = (tail + text).slice(-8);
        parser.feed(text);
      } catch {
        rejected = true;
        fail();
      }
    },
    finish() {
      if (sealed || rejected) fail();
      sealed = true;
      const rest = utf8.decode();
      tail = (tail + rest).slice(-8);
      parser.feed(rest);
      // Do not flush incomplete SSE data into a synthetic terminal event.
      if (
        !ready ||
        !finished ||
        !tail.replace(/\r\n/gu, "\n").replace(/\r/gu, "\n").endsWith("\n\n")
      )
        fail();
      return { messages, sse_terminal: true };
    },
  };
}

/**
 * Capture one browser-owned request using bounded CDP streaming. No direct
 * HTTP, headers/cookies in output, retry, PoW implementation or page injection.
 * Cancellation stops observation, NOT necessarily remote generation.
 */
export async function captureDeepSeekExchange(
  page,
  {
    binding,
    submit,
    signal,
    timeoutMs = 30_000,
    maxBytes = MAX_BYTES,
    maxEvents = 4096,
  } = {},
) {
  if (
    !validBinding(binding) ||
    typeof submit !== "function" ||
    signal?.aborted ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 120_000
  )
    return null;
  let decoder;
  try {
    if (new URL(page.url()).origin !== DEEPSEEK_WEB_ORIGIN) return null;
    decoder = createDeepSeekEvidenceDecoder({
      model: binding.model,
      maxBytes,
      maxEvents,
    });
  } catch {
    return null;
  }
  let session;
  let timer;
  let settled = false;
  let requestId;
  let responseSeen = false;
  let streamReady = false;
  let networkEnded = false;
  let queuedBytes = 0;
  let consumedBytes = 0;
  const queued = [];
  const controller = new AbortController();
  const startedAt = new Date().toISOString();
  let resolve;
  const result = new Promise((done) => (resolve = done));
  const settle = (value = null) => {
    if (settled) return;
    settled = true;
    controller.abort();
    resolve(value);
  };
  const abort = () => settle();
  const feed = (data) => {
    if (
      typeof data !== "string" ||
      data.length > Math.ceil(maxBytes / 3) * 4 ||
      !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(
        data,
      )
    )
      fail();
    const bytes = Buffer.from(data, "base64");
    consumedBytes += bytes.length;
    if (consumedBytes > maxBytes) fail();
    decoder.push(bytes);
  };
  const finish = () => {
    if (settled || !streamReady || !networkEnded) return;
    try {
      settle({
        ...decoder.finish(),
        started_at: startedAt,
        received_at: new Date().toISOString(),
      });
    } catch {
      settle();
    }
  };
  const onRequest = (event) => {
    if (settled || event.request?.url !== COMPLETION) return;
    if (
      requestId ||
      event.redirectResponse ||
      !matchesDeepSeekSubmission(event.request, binding)
    )
      return settle();
    requestId = event.requestId;
  };
  const onResponse = (event) => {
    if (settled || event.requestId !== requestId) return;
    const response = event.response;
    if (
      responseSeen ||
      response?.url !== COMPLETION ||
      response.status !== 200 ||
      !/^text\/event-stream(?:\s*;|$)/iu.test(response.mimeType ?? "")
    )
      return settle();
    responseSeen = true;
    session
      .send("Network.streamResourceContent", { requestId })
      .then(({ bufferedData }) => {
        if (settled) return;
        try {
          feed(bufferedData);
          for (const data of queued) feed(data);
          queued.length = 0;
          streamReady = true;
          finish();
        } catch {
          settle();
        }
      }, abort);
  };
  const onData = (event) => {
    if (settled || event.requestId !== requestId || event.data === undefined)
      return;
    try {
      if (streamReady) feed(event.data);
      else {
        if (typeof event.data !== "string") fail();
        queuedBytes += event.data.length;
        if (queuedBytes > Math.ceil(maxBytes / 3) * 4) fail();
        queued.push(event.data);
      }
    } catch {
      settle();
    }
  };
  const onFinished = (event) => {
    if (event.requestId !== requestId) return;
    networkEnded = true;
    finish();
  };
  const onFailed = (event) => {
    if (event.requestId === requestId) settle();
  };
  const handlers = [
    ["Network.requestWillBeSent", onRequest],
    ["Network.responseReceived", onResponse],
    ["Network.dataReceived", onData],
    ["Network.loadingFinished", onFinished],
    ["Network.loadingFailed", onFailed],
  ];
  signal?.addEventListener("abort", abort, { once: true });
  try {
    timer = setTimeout(abort, timeoutMs);
    if (signal?.aborted) return null;
    const opening = page
      .context()
      .newCDPSession(page)
      .then(async (opened) => {
        if (settled) {
          await opened.detach().catch(() => {});
          return null;
        }
        return opened;
      });
    session = await Promise.race([opening, result]);
    if (!session || settled) return null;
    for (const [name, handler] of handlers) session.on(name, handler);
    await Promise.race([session.send("Network.enable"), result]);
    if (settled) return null;
    // A hanging UI callback must not prevent cancellation or the deadline.
    const submission = Promise.resolve().then(() =>
      settled ? undefined : submit(page, controller.signal),
    );
    void submission.catch(abort);
    return await result;
  } catch {
    return null;
  } finally {
    settle();
    clearTimeout(timer);
    signal?.removeEventListener("abort", abort);
    if (session) {
      for (const [name, handler] of handlers) session.off(name, handler);
      // Do not keep the execution alive waiting on a broken CDP connection.
      void session.detach().catch(() => {});
    }
  }
}
