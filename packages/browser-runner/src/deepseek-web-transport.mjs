export const DEEPSEEK_ORIGIN = "https://chat.deepseek.com";
export const validDeepSeekSessionId = (value) =>
  typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/u.test(value);
const MAX_BYTES = 750_000;
const DELETE_CLOSE_MARGIN_MS = 2_000;

/**
 * One bounded operation in one actual authenticated Document. This is a thin
 * adapter over Playwright and browser fetch, not an alternate HTTP client.
 * Cancellation settles even if CDP hangs; all late continuations stay inert.
 * Raw envelopes are transient internal evidence, never caller-facing output.
 */
export async function withDeepSeekDocument(
  page,
  { timeoutMs = 10_000, signal } = {},
  operation,
) {
  if (
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
  const stopped = new Promise((done) => (resolve = done));
  const stop = () => {
    if (settled) return;
    settled = true;
    resolve(null);
  };
  const navigated = (frame) => {
    if (frame === page.mainFrame()) stop();
  };
  const dispose = (owned) => {
    void Promise.resolve()
      .then(() =>
        owned.evaluate((state) => {
          if (!state) return;
          clearTimeout(state.timer);
          state.controller.abort();
        }),
      )
      .catch(() => {});
    void Promise.resolve()
      .then(() => owned.dispose())
      .catch(() => {});
  };
  try {
    const href = page.url();
    const current = new URL(href);
    if (
      current.origin !== DEEPSEEK_ORIGIN ||
      current.username ||
      current.password ||
      page.isClosed()
    )
      return null;
    const deadline = Date.now() + timeoutMs;
    const active = () =>
      !settled &&
      Date.now() < deadline &&
      !signal?.aborted &&
      !page.isClosed() &&
      page.url() === href;
    timer = setTimeout(stop, timeoutMs);
    signal?.addEventListener("abort", stop, { once: true });
    page.on("framenavigated", navigated);
    page.on("close", stop);
    if (signal?.aborted) return null;
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
          // This snapshot never leaves the browser. A same-URL login change
          // invalidates the entire operation, not only the next request.
          return {
            document,
            credential: localStorage.getItem("userToken"),
            controller,
            timer: setTimeout(() => controller.abort(), deadline - Date.now()),
          };
        },
        { origin: DEEPSEEK_ORIGIN, href, deadline },
      )
      .then((opened) => {
        if (settled) {
          dispose(opened);
          return null;
        }
        return opened;
      });
    handle = await Promise.race([opening, stopped]);
    if (!handle || !active()) return null;
    const check = async () => {
      if (!active()) return false;
      const valid = await Promise.race([
        handle.evaluate(
          (state, { origin, href, deadline }) =>
            !!state &&
            state.document === document &&
            location.origin === origin &&
            location.href === href &&
            Date.now() < deadline &&
            !state.controller.signal.aborted &&
            localStorage.getItem("userToken") === state.credential,
          { origin: DEEPSEEK_ORIGIN, href, deadline },
        ),
        stopped,
      ]);
      return active() && valid === true;
    };
    const request = async (kind, conversationId, { deleteNotAfterMs } = {}) => {
      if (
        !active() ||
        !["history", "delete"].includes(kind) ||
        !validDeepSeekSessionId(conversationId)
      )
        return null;
      const envelope = await Promise.race([
        page.evaluate(
          async ({
            state,
            origin,
            href,
            conversationId,
            kind,
            maxBytes,
            deadline,
            timeoutMs,
            deleteNotAfterMs,
            closeMarginMs,
          }) => {
            let reader;
            try {
              const valid = () =>
                !!state &&
                state.document === document &&
                location.origin === origin &&
                location.href === href &&
                Date.now() < deadline &&
                !state.controller.signal.aborted &&
                localStorage.getItem("userToken") === state.credential;
              if (!valid()) return null;
              const stored = JSON.parse(state.credential ?? "null");
              if (
                stored?.__version !== "0" ||
                typeof stored.value !== "string" ||
                !stored.value.trim()
              )
                return null;
              if (
                kind === "delete" &&
                (!Number.isFinite(deleteNotAfterMs) ||
                  deleteNotAfterMs - Date.now() < timeoutMs + closeMarginMs)
              )
                return { kind: "authorization_expired" };
              const url =
                kind === "history"
                  ? `${origin}/api/v0/chat/history_messages?chat_session_id=${encodeURIComponent(conversationId)}&cache_version=null&cache_reset_at=null`
                  : `${origin}/api/v0/chat_session/delete`;
              const response = await fetch(url, {
                method: kind === "history" ? "GET" : "POST",
                credentials: "include",
                redirect: "error",
                cache: "no-store",
                headers: {
                  Authorization: `Bearer ${stored.value}`,
                  ...(kind === "delete"
                    ? { "Content-Type": "application/json" }
                    : {}),
                },
                ...(kind === "delete"
                  ? {
                      body: JSON.stringify({
                        chat_session_ids: [conversationId],
                      }),
                    }
                  : {}),
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
                if (bytes > maxBytes || !valid()) return null;
                body += decoder.decode(value, { stream: true });
              }
              body += decoder.decode();
              if (!valid()) return null;
              return { status: response.status, body };
            } catch {
              return null;
            } finally {
              if (reader) void reader.cancel().catch(() => {});
            }
          },
          {
            state: handle,
            origin: DEEPSEEK_ORIGIN,
            href,
            conversationId,
            kind,
            maxBytes: kind === "history" ? MAX_BYTES : 128_000,
            deadline,
            timeoutMs,
            deleteNotAfterMs,
            closeMarginMs: DELETE_CLOSE_MARGIN_MS,
          },
        ),
        stopped,
      ]);
      return active() ? envelope : null;
    };
    const result = await Promise.race([
      operation({ active, check, request }),
      stopped,
    ]);
    return active() ? result : null;
  } catch {
    return null;
  } finally {
    stop();
    clearTimeout(timer);
    signal?.removeEventListener("abort", stop);
    page.off?.("framenavigated", navigated);
    page.off?.("close", stop);
    if (handle) dispose(handle);
  }
}
