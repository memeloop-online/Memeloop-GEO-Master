import { createParser } from "eventsource-parser";
import {
  ObservationPersistenceError,
  reportObservationDiagnostic,
} from "./ai-observation-parser.mjs";

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
const fail = (termination = "malformed") => {
  const error = new Error("deepseek_capture_unverified");
  error.termination = termination;
  throw error;
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

function validBinding(binding, allowUnresolvedSession = false) {
  return (
    object(binding) &&
    label(binding.model) &&
    typeof binding.question === "string" &&
    binding.question.trim().length > 0 &&
    Buffer.byteLength(binding.question) <= 16_000 &&
    ((allowUnresolvedSession && binding.chatSessionId === undefined) ||
      (typeof binding.chatSessionId === "string" &&
        /^[\w-]{1,128}$/u.test(binding.chatSessionId))) &&
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
    onError: () => fail(),
    onEvent(event) {
      if (messages.length >= maxEvents) fail("limit_exceeded");
      // The site's finish branch does not parse its payload. Preserve those
      // wire bytes without treating them as an answer or completion proof.
      if (event.event === "finish") {
        if (!ready || finished) fail();
        finished = true;
        messages.push({
          event: "finish",
          ...(event.id === undefined ? {} : { id: event.id }),
          raw_data: event.data,
        });
        return;
      }
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
      } else if (
        (!event.event ||
          event.event === "message" ||
          event.event === "delta") &&
        (!ready || finished)
      ) {
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
    snapshot() {
      return { messages: structuredClone(messages), sse_terminal: false };
    },
    push(bytes) {
      if (sealed || rejected) fail();
      try {
        if (!(bytes instanceof Uint8Array)) fail();
        size += bytes.byteLength;
        if (size > maxBytes) fail("limit_exceeded");
        const text = utf8.decode(bytes, { stream: true });
        tail = (tail + text).slice(-8);
        parser.feed(text);
      } catch (error) {
        rejected = true;
        fail(error?.termination);
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
        fail("eof");
      return { messages: structuredClone(messages), sse_terminal: true };
    },
  };
}

/**
 * Capture one browser-owned request using bounded CDP streaming. No direct
 * HTTP, headers/cookies in output, retry, PoW implementation or page injection.
 * Cancellation stops observation, NOT necessarily remote generation.
 *
 * Optional createdSessions is the caller-owned observeDeepSeekCreatedSessions()
 * handle, armed before navigation/prefetch. With it, binding.chatSessionId may
 * be omitted: the exact ordinary POST ID is looked up, never the latest create.
 * Streaming is attached immediately while lookup waits for pending creates.
 * Success revalidates the same proof after EOF; created_session is creation
 * evidence only, not account identity, successful answering or cleanup authority.
 * The caller owns the observer's timeout, cancellation and disposal.
 */
export async function captureDeepSeekExchange(
  page,
  {
    binding,
    createdSessions,
    submit,
    signal,
    timeoutMs = 30_000,
    maxBytes = MAX_BYTES,
    maxEvents = 4096,
    onDiagnostic,
    onPartialCapture,
    checkpointTimeoutMs = 5_000,
  } = {},
) {
  if (
    !validBinding(binding, createdSessions !== undefined) ||
    (createdSessions !== undefined &&
      (!object(createdSessions) ||
        typeof createdSessions.lookup !== "function" ||
        typeof createdSessions.isCurrent !== "function" ||
        typeof createdSessions.status !== "function")) ||
    typeof submit !== "function" ||
    signal?.aborted ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 120_000 ||
    !Number.isFinite(checkpointTimeoutMs) ||
    checkpointTimeoutMs <= 0 ||
    checkpointTimeoutMs > 5_000
  )
    return null;
  // Preserve the same frozen request identity throughout asynchronous recovery.
  binding = { ...binding };
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
  let stage = "capture_setup";
  let requestId;
  let requestAt;
  let submitted = false;
  let document;
  let responseSeen = false;
  let responseCharset = "";
  let streamReady = false;
  let networkEnded = false;
  let completedBeforeAttachment = false;
  let bodyReadStarted = false;
  let creationLookup;
  let finishing = false;
  let queuedBytes = 0;
  let consumedBytes = 0;
  const queued = [];
  const controller = new AbortController();
  const startedAt = new Date().toISOString();
  let resolve;
  let reject;
  let terminalCapture;
  const result = new Promise((done, failed) => {
    resolve = done;
    reject = failed;
  });
  // Event handlers may reject before the main flow starts awaiting result.
  void result.catch(() => {});
  const settle = (
    value = null,
    failureStage = stage,
    termination = "malformed",
  ) => {
    if (settled) return;
    settled = true;
    if (value === null)
      reportObservationDiagnostic(
        onDiagnostic,
        failureStage,
        "capture_unverified",
      );
    controller.abort();
    if (
      value === null &&
      responseSeen &&
      typeof onPartialCapture === "function"
    ) {
      const prefix = terminalCapture ?? decoder.snapshot();
      if (prefix.messages.length) {
        const complete = prefix.sse_terminal === true;
        let checkpointTimer;
        const checkpoint = Promise.resolve().then(() =>
          onPartialCapture({
            ...prefix,
            started_at: startedAt,
            received_at: new Date().toISOString(),
            capture_status: {
              schema_version: "geo.observation.capture-status.v1",
              transport: complete ? "complete" : "partial",
              termination: complete ? "complete" : termination,
            },
          }),
        );
        void Promise.race([
          checkpoint,
          new Promise((_, failed) => {
            checkpointTimer = setTimeout(
              () => failed(new ObservationPersistenceError()),
              checkpointTimeoutMs,
            );
          }),
        ])
          .then(
            () => resolve(null),
            (error) =>
              reject(
                error?.message === "source_privacy_unverified"
                  ? error
                  : new ObservationPersistenceError(),
              ),
          )
          .finally(() => clearTimeout(checkpointTimer));
        return;
      }
    }
    resolve(value);
  };
  const abort = () =>
    settle(
      null,
      stage,
      signal?.reason?.name === "TimeoutError" ? "timeout" : "aborted",
    );
  // CDP cannot replay commits which occurred while its session was opening.
  // Guard that gap with Playwright; after the loader is known, Page events
  // distinguish legitimate SPA navigation from replacement documents.
  const onEarlyNavigation = (frame) => {
    if (!document && frame === page.mainFrame()) settle();
  };
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
    if (consumedBytes > maxBytes) fail("limit_exceeded");
    decoder.push(bytes);
  };
  const finish = async () => {
    if (settled || finishing || !streamReady || !networkEnded) return;
    finishing = true;
    try {
      terminalCapture = decoder.finish();
      let creationProof;
      if (createdSessions !== undefined) {
        creationProof = await creationLookup;
        if (settled) return;
        if (!creationProof) return settle(null, "capture_creation");
        // Re-read after EOF, including any newly pending create responses.
        // The observer returns stable proof identities within one document.
        const current = await createdSessions.lookup(binding.chatSessionId);
        if (settled) return;
        if (
          current !== creationProof ||
          createdSessions.isCurrent(creationProof) !== true
        )
          return settle(null, "capture_creation");
      }
      settle({
        ...terminalCapture,
        capture_status: {
          schema_version: "geo.observation.capture-status.v1",
          transport: "complete",
          termination: "complete",
        },
        ...(creationProof ? { created_session: creationProof } : {}),
        started_at: startedAt,
        received_at: new Date().toISOString(),
      });
    } catch (error) {
      settle(null, stage, error?.termination ?? "malformed");
    }
  };
  const recoverCompletedBody = () => {
    if (
      settled ||
      bodyReadStarted ||
      !completedBeforeAttachment ||
      !networkEnded ||
      !responseSeen
    )
      return;
    // No second request and no concatenation with a partially streamed body.
    // Chromium's resource/session buffers were bounded before submission.
    bodyReadStarted = true;
    queued.length = 0;
    session.send("Network.getResponseBody", { requestId }).then((response) => {
      if (settled) return;
      try {
        if (
          !object(response) ||
          typeof response.body !== "string" ||
          typeof response.base64Encoded !== "boolean" ||
          consumedBytes !== 0
        )
          fail();
        if (response.base64Encoded) {
          feed(response.body);
        } else {
          // CDP has already decoded textual bodies. Require explicit UTF-8;
          // default/other charsets can silently change the source text.
          // Reject replacement/surrogate text instead of inventing bytes.
          if (
            response.body.length > maxBytes ||
            !response.body.isWellFormed() ||
            response.body.includes("\uFFFD") ||
            !["utf-8", "utf8"].includes(responseCharset) ||
            Buffer.byteLength(response.body, "utf8") > maxBytes
          )
            fail();
          decoder.push(Buffer.from(response.body, "utf8"));
        }
        streamReady = true;
        finish();
      } catch (error) {
        settle(null, stage, error?.termination ?? "malformed");
      }
    }, abort);
  };
  const onRequest = (event) => {
    if (settled) return;
    // Redirects retain the request ID but may leave the completion endpoint.
    if (requestId && event.requestId === requestId) return settle();
    if (event.request?.url !== COMPLETION) return;
    stage = "capture_request";
    let candidate = binding;
    if (createdSessions !== undefined && binding.chatSessionId === undefined) {
      try {
        if (
          typeof event.request.postData !== "string" ||
          Buffer.byteLength(event.request.postData) > 64_000
        )
          return settle();
        candidate = {
          ...binding,
          chatSessionId: JSON.parse(event.request.postData)?.chat_session_id,
        };
      } catch {
        return settle();
      }
    }
    if (
      requestId ||
      !submitted ||
      event.frameId !== document?.id ||
      event.loaderId !== document?.loaderId ||
      event.redirectResponse ||
      typeof event.requestId !== "string" ||
      !event.requestId ||
      !Number.isFinite(event.timestamp) ||
      !matchesDeepSeekSubmission(event.request, candidate)
    )
      return settle();
    requestId = event.requestId;
    requestAt = event.timestamp;
    binding = candidate;
    if (createdSessions !== undefined) {
      // Do not await here: response/data/EOF may arrive before create body().
      creationLookup = Promise.resolve()
        .then(() => createdSessions.lookup(binding.chatSessionId))
        .then((proof) => {
          if (settled) return null;
          if (
            !object(proof) ||
            proof.chatSessionId !== binding.chatSessionId ||
            proof.source !== "browser_observed_create" ||
            createdSessions.status() !== "observing"
          ) {
            return null;
          }
          return proof;
        })
        .catch(() => {
          return null;
        });
    }
  };
  const onResponse = (event) => {
    if (settled || event.requestId !== requestId) return;
    stage = "capture_response";
    const response = event.response;
    if (
      responseSeen ||
      event.frameId !== document?.id ||
      event.loaderId !== document?.loaderId ||
      !Number.isFinite(event.timestamp) ||
      event.timestamp < requestAt ||
      response?.url !== COMPLETION ||
      response.status !== 200 ||
      !/^text\/event-stream(?:\s*;|$)/iu.test(response.mimeType ?? "")
    )
      return settle();
    responseSeen = true;
    stage = "capture_stream";
    responseCharset =
      typeof response.charset === "string"
        ? response.charset.toLowerCase()
        : "";
    session.send("Network.streamResourceContent", { requestId }).then(
      ({ bufferedData }) => {
        if (settled) return;
        try {
          feed(bufferedData);
          for (const data of queued) feed(data);
          queued.length = 0;
          streamReady = true;
          finish();
        } catch (error) {
          settle(null, stage, error?.termination ?? "malformed");
        }
      },
      (error) => {
        if (settled) return;
        // Other protocol errors (unsupported command, detached session, etc.)
        // do not authorize recovering a different or still-running resource.
        if (
          typeof error?.message !== "string" ||
          !error.message.includes("already finished loading")
        )
          return settle(null, "capture_stream");
        completedBeforeAttachment = true;
        recoverCompletedBody();
      },
    );
  };
  const onData = (event) => {
    if (settled || event.requestId !== requestId || event.data === undefined)
      return;
    try {
      if (streamReady) feed(event.data);
      else {
        if (typeof event.data !== "string") fail();
        queuedBytes += event.data.length;
        if (queuedBytes > Math.ceil(maxBytes / 3) * 4) fail("limit_exceeded");
        queued.push(event.data);
      }
    } catch (error) {
      settle(null, stage, error?.termination ?? "malformed");
    }
  };
  const onFinished = (event) => {
    if (settled || event.requestId !== requestId) return;
    if (
      !responseSeen ||
      networkEnded ||
      !Number.isFinite(event.timestamp) ||
      // CDP responseReceived can be timestamped after the network's actual
      // finish (notably an intercepted fast reply). Both must follow this
      // request; event delivery order is checked separately by responseSeen.
      event.timestamp < requestAt
    )
      return settle();
    networkEnded = true;
    stage = "capture_eof";
    recoverCompletedBody();
    finish();
  };
  const onFailed = (event) => {
    if (event.requestId === requestId) settle(null, stage, "eof");
  };
  const handlers = [
    // Page.frameNavigated is a document commit, unlike navigatedWithinDocument
    // (history.pushState used by the ordinary new-conversation flow).
    [
      "Page.frameNavigated",
      ({ frame }) => {
        if (!frame?.parentId || frame.id === document?.id) settle();
      },
    ],
    [
      "Page.frameDetached",
      ({ frameId }) => {
        if (frameId === document?.id) settle();
      },
    ],
    [
      "Page.navigatedWithinDocument",
      ({ frameId, url }) => {
        if (frameId !== document?.id) return;
        try {
          if (new URL(url).origin !== DEEPSEEK_WEB_ORIGIN) settle();
        } catch {
          settle();
        }
      },
    ],
    ["Network.requestWillBeSent", onRequest],
    ["Network.responseReceived", onResponse],
    ["Network.dataReceived", onData],
    ["Network.loadingFinished", onFinished],
    ["Network.loadingFailed", onFailed],
  ];
  signal?.addEventListener("abort", abort, { once: true });
  try {
    timer = setTimeout(() => settle(null, stage, "timeout"), timeoutMs);
    if (signal?.aborted) return null;
    page.on("framenavigated", onEarlyNavigation);
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
    const command = (method, args) =>
      Promise.race([session.send(method, args), result]);
    await command("Page.enable");
    if (settled) return null;
    const initial = await command("Page.getFrameTree");
    if (settled) return null;
    document = initial?.frameTree?.frame;
    if (
      !document ||
      document.parentId ||
      typeof document.id !== "string" ||
      !document.id ||
      typeof document.loaderId !== "string" ||
      !document.loaderId ||
      new URL(document.url).origin !== DEEPSEEK_WEB_ORIGIN
    )
      return null;
    await Promise.race([
      session.send("Network.enable", {
        maxTotalBufferSize: maxBytes,
        maxResourceBufferSize: maxBytes,
        maxPostDataSize: 64_000,
      }),
      result,
    ]);
    if (settled) return null;
    const current = (await command("Page.getFrameTree"))?.frameTree?.frame;
    if (
      settled ||
      current?.id !== document.id ||
      current?.loaderId !== document.loaderId ||
      current.parentId ||
      new URL(current.url).origin !== DEEPSEEK_WEB_ORIGIN ||
      new URL(page.url()).origin !== DEEPSEEK_WEB_ORIGIN
    )
      return null;
    // A hanging UI callback must not prevent cancellation or the deadline.
    const submission = Promise.resolve().then(() => {
      if (settled) return;
      if (new URL(page.url()).origin !== DEEPSEEK_WEB_ORIGIN) return settle();
      if (
        createdSessions !== undefined &&
        createdSessions.status() !== "observing"
      )
        return settle();
      submitted = true;
      stage = "capture_submission";
      return submit(page, controller.signal);
    });
    void submission.catch(() => settle(null, "capture_submission"));
    return await result;
  } catch (error) {
    if (
      error instanceof ObservationPersistenceError ||
      error?.message === "source_privacy_unverified"
    )
      throw error;
    return null;
  } finally {
    settle();
    clearTimeout(timer);
    signal?.removeEventListener("abort", abort);
    page.off?.("framenavigated", onEarlyNavigation);
    if (session) {
      for (const [name, handler] of handlers) session.off(name, handler);
      // Do not keep the execution alive waiting on a broken CDP connection.
      void session.detach().catch(() => {});
    }
  }
}
