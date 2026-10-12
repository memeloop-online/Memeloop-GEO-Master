import { verifyDeepSeekFirstTurnHistory } from "./deepseek-history-evidence.mjs";

const ORIGIN = "https://chat.deepseek.com";
const MAX_BYTES = 750_000;
const messageId = (value) =>
  (typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value >= 0 &&
    !Object.is(value, -0)) ||
  (typeof value === "string" &&
    /^(?:0|[1-9]\d{0,15})$/u.test(value) &&
    Number.isSafeInteger(Number(value)) &&
    String(Number(value)) === value);

/**
 * Read a fresh first-turn inventory in the current authenticated document.
 * No navigation, submission, retry, answer parsing, or deletion authority.
 * Tokens remain in the browser; raw history is used transiently by the existing
 * verifier, then only its inventory and observed lifecycle metadata survive.
 * The caller still owns account identity, lease and creation/request binding.
 */
export async function readDeepSeekFirstTurnHistory(
  page,
  {
    conversationId,
    requestMessageId,
    responseMessageId,
    timeoutMs = 10_000,
    signal,
  } = {},
) {
  if (
    typeof conversationId !== "string" ||
    !/^[A-Za-z0-9_-]{1,128}$/u.test(conversationId) ||
    !messageId(requestMessageId) ||
    !messageId(responseMessageId) ||
    String(requestMessageId) === String(responseMessageId) ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 120_000 ||
    signal?.aborted
  )
    return null;
  let handle;
  let timer;
  let settled = false;
  let resolve;
  const result = new Promise((done) => (resolve = done));
  const settle = (value = null) => {
    if (settled) return;
    settled = true;
    resolve(value);
  };
  const abort = () => settle();
  const navigated = (frame) => {
    if (frame === page.mainFrame()) settle();
  };
  const dispose = (owned) => {
    // Cleanup must not hold the result hostage to a broken browser connection.
    void Promise.resolve()
      .then(() =>
        owned.evaluate((state) => {
          if (!state) return;
          clearTimeout(state.timer);
          state.controller.abort();
        }),
      )
      .catch(() => {});
    // Releasing the remote handle is independent: even the abort evaluation
    // itself can hang. Dispatch it first, but never await it before disposal.
    void Promise.resolve()
      .then(() => owned.dispose())
      .catch(() => {});
  };
  try {
    const href = page.url();
    const current = new URL(href);
    if (
      current.origin !== ORIGIN ||
      current.username ||
      current.password ||
      page.isClosed()
    )
      return null;
    const deadline = Date.now() + timeoutMs;
    timer = setTimeout(abort, timeoutMs);
    signal?.addEventListener("abort", abort, { once: true });
    page.on("framenavigated", navigated);
    page.on("close", abort);
    if (signal?.aborted) return null;
    // A handle binds the request to one actual Document and provides native
    // AbortController cancellation without exposing credentials to Node.
    const opening = page
      .evaluateHandle(
        ({ origin, href, deadline }) => {
          if (
            location.origin !== origin ||
            location.href !== href ||
            Date.now() >= deadline
          )
            return null;
          const controller = new AbortController();
          return {
            document,
            controller,
            timer: setTimeout(() => controller.abort(), deadline - Date.now()),
          };
        },
        { origin: ORIGIN, href, deadline },
      )
      .then((opened) => {
        if (settled) {
          dispose(opened);
          return null;
        }
        return opened;
      });
    handle = await Promise.race([opening, result]);
    if (!handle || settled || Date.now() >= deadline) return null;
    const envelope = await Promise.race([
      page.evaluate(
        async ({ state, origin, href, conversationId, maxBytes, deadline }) => {
          let reader;
          try {
            if (
              !state ||
              state.document !== document ||
              location.origin !== origin ||
              location.href !== href ||
              Date.now() >= deadline ||
              state.controller.signal.aborted
            )
              return null;
            const stored = JSON.parse(
              localStorage.getItem("userToken") ?? "null",
            );
            if (
              stored?.__version !== "0" ||
              typeof stored.value !== "string" ||
              !stored.value.trim()
            )
              return null;
            const url = `${origin}/api/v0/chat/history_messages?chat_session_id=${encodeURIComponent(conversationId)}&cache_version=null&cache_reset_at=null`;
            const response = await fetch(url, {
              method: "GET",
              credentials: "include",
              redirect: "error",
              cache: "no-store",
              headers: { Authorization: `Bearer ${stored.value}` },
              signal: state.controller.signal,
            });
            const length = response.headers.get("content-length");
            if (
              response.status !== 200 ||
              response.redirected ||
              response.url !== url ||
              !/^application\/json(?:\s*;|$)/iu.test(
                response.headers.get("content-type") ?? "",
              ) ||
              (length !== null &&
                (!/^\d+$/u.test(length) || Number(length) > maxBytes))
            )
              return null;
            reader = response.body?.getReader();
            if (!reader) return null;
            const decoder = new TextDecoder("utf-8", { fatal: true });
            let bytes = 0;
            let body = "";
            for (;;) {
              const { done, value } = await reader.read();
              if (done) break;
              bytes += value.byteLength;
              if (bytes > maxBytes || state.controller.signal.aborted)
                return null;
              body += decoder.decode(value, { stream: true });
            }
            body += decoder.decode();
            if (
              state.document !== document ||
              location.origin !== origin ||
              location.href !== href ||
              state.controller.signal.aborted
            )
              return null;
            return { status: response.status, body };
          } catch {
            return null;
          } finally {
            if (reader) void reader.cancel().catch(() => {});
            if (state) {
              clearTimeout(state.timer);
              state.controller.abort();
            }
          }
        },
        {
          state: handle,
          origin: ORIGIN,
          href,
          conversationId,
          maxBytes: MAX_BYTES,
          deadline,
        },
      ),
      result,
    ]);
    if (
      settled ||
      Date.now() >= deadline ||
      signal?.aborted ||
      page.isClosed() ||
      page.url() !== href
    )
      return null;
    const inventory = verifyDeepSeekFirstTurnHistory(envelope, {
      conversationId,
      requestMessageId,
      responseMessageId,
    });
    if (!inventory) return null;
    const observed = JSON.parse(envelope.body).data.biz_data;
    const history = {
      source: "browser_observed_history",
      observed_at: new Date().toISOString(),
      cache_request: { cache_version: null, cache_reset_at: null },
      cache_control: observed.cache_control,
      chat_session_id: observed.chat_session.id,
      messages: observed.chat_messages.map((message) => ({
        message_id: message.message_id,
        parent_id: message.parent_id,
        role: message.role,
        status: message.status,
        has_pending_fragment: message.has_pending_fragment,
        ...(Object.hasOwn(message, "quasi_status")
          ? { quasi_status: message.quasi_status }
          : {}),
      })),
      ...(Object.hasOwn(observed.chat_session, "current_message_id")
        ? { current_message_id: observed.chat_session.current_message_id }
        : {}),
    };
    return { inventory, history };
  } catch {
    return null;
  } finally {
    settle();
    clearTimeout(timer);
    signal?.removeEventListener("abort", abort);
    page.off?.("framenavigated", navigated);
    page.off?.("close", abort);
    if (handle) dispose(handle);
  }
}
