const ENDPOINT = "https://chat.deepseek.com/api/v0/chat_session/create";
const ORIGIN = "https://chat.deepseek.com";
const MAX_BYTES = 64 * 1024;
const validId = (value) =>
  typeof value === "string" && /^[\w-]{1,128}$/u.test(value);
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

function createdId(bytes, maxBytes) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength > maxBytes)
    return null;
  const value = JSON.parse(
    new TextDecoder("utf-8", { fatal: true }).decode(bytes),
  );
  const session = value?.data?.biz_data?.chat_session;
  if (
    !object(value) ||
    value.code !== 0 ||
    !object(value.data) ||
    value.data.biz_code !== 0 ||
    !object(value.data.biz_data) ||
    !object(session) ||
    !validId(session.id)
  )
    return null;
  return session.id;
}

/**
 * Arm before caller-owned goto/reload. Observes only exact empty-object create
 * requests from this page's main document; never fetches or navigates.
 *
 * lookup(chatSessionId) returns {chatSessionId, source:"browser_observed_create"}
 * or null. It waits for all currently observed unfinished create requests/body
 * reads, under the observer's single timeout/cancellation lifetime, so a delayed
 * response cannot hide an ambiguous duplicate. It never picks the latest ID.
 * The caller must independently verify account identity, completion-request
 * ownership and selected ID. This proof does not grant deletion permission.
 * After awaiting lookup, call isCurrent(proof) synchronously before using it:
 * navigation or a new pending create may invalidate the resolved proof before
 * the await continuation runs. This check does not wait or refresh evidence.
 *
 * status() is "observing", "overflow", "ambiguous", "timeout", "cancelled",
 * "closed", "disposed" or "invalid_options". dispose() removes all listeners.
 * Full-document navigation invalidates prior proofs and pending lookups;
 * same-document root/chat routing retains them. Late body reads are inert.
 * maxBytes bounds accepted bytes, NOT Playwright body() transport buffering.
 * Container/browser resource limits are still required.
 */
export function observeDeepSeekCreatedSessions(
  page,
  { signal, timeoutMs = 10_000, maxBytes = MAX_BYTES, maxCandidates = 16 } = {},
) {
  const mainFrame = page.mainFrame();
  let state = "observing";
  let epoch = 0;
  let navigating = false;
  let count = 0;
  let pending = 0;
  let timer;
  let requests = new WeakMap();
  const proofs = new Map();
  const waiters = new Set();
  const active = () => state === "observing";
  const current = () => {
    if (navigating || page.mainFrame() !== mainFrame) return false;
    const url = new URL(mainFrame.url());
    return (
      url.origin === ORIGIN &&
      !url.username &&
      !url.password &&
      (url.pathname === "/" ||
        /^\/a\/chat\/s\/[\w-]{1,128}$/u.test(url.pathname))
    );
  };
  const flush = (invalidate = false) => {
    if (!invalidate && active() && pending) return;
    for (const waiter of waiters) {
      waiter.resolve(
        !invalidate && active() ? (proofs.get(waiter.id) ?? null) : null,
      );
    }
    waiters.clear();
  };
  const stop = (reason) => {
    if (!active()) return;
    state = reason;
    clearTimeout(timer);
    page.off("request", onRequest);
    page.off("response", onResponse);
    page.off("requestfailed", onFailure);
    page.off("framenavigated", onNavigation);
    page.off("close", onClose);
    signal?.removeEventListener("abort", onAbort);
    requests = new WeakMap();
    proofs.clear();
    flush(true);
  };
  const dispose = () => stop("disposed");
  const onAbort = () => stop("cancelled");
  const onClose = () => stop("closed");
  const invalidate = () => {
    epoch++;
    pending = 0;
    requests = new WeakMap();
    proofs.clear();
    flush(true);
  };
  const onNavigation = (frame) => {
    if (!active() || frame !== mainFrame) return;
    navigating = false;
    try {
      if (!current()) invalidate();
    } catch {
      invalidate();
    }
  };
  const onRequest = (request) => {
    if (!active()) return;
    try {
      if (request.frame() !== mainFrame) return;
      if (request.isNavigationRequest()) {
        navigating = true;
        invalidate();
        return;
      }
      if (
        !current() ||
        request.url() !== ENDPOINT ||
        request.method() !== "POST" ||
        request.redirectedFrom() ||
        requests.has(request)
      )
        return;
      const body = request.postData();
      if (typeof body !== "string" || body.length > 1024) return;
      const parsed = JSON.parse(body);
      if (!object(parsed) || Object.keys(parsed).length) return;
      if (++count > maxCandidates) return stop("overflow");
      requests.set(request, { epoch, reading: false, done: false });
      pending++;
    } catch {
      // Requests without frame ownership and malformed bodies are not proof.
    }
  };
  const complete = (record) => {
    if (!active() || record.epoch !== epoch || record.done) return;
    record.done = true;
    pending--;
    flush();
  };
  const onFailure = (request) => {
    const record = requests.get(request);
    if (record) complete(record);
  };
  const onResponse = async (response) => {
    let record;
    let claimed = false;
    try {
      const request = response.request();
      record = requests.get(request);
      if (
        !active() ||
        !record ||
        record.done ||
        record.reading ||
        record.epoch !== epoch
      )
        return;
      record.reading = true;
      claimed = true;
      if (
        !current() ||
        request.frame() !== mainFrame ||
        request.redirectedFrom() ||
        response.url() !== ENDPOINT ||
        response.status() !== 200
      )
        return;
      const length = await response.headerValue("content-length");
      if (!active() || record.epoch !== epoch || record.done) return;
      if (
        length !== null &&
        (!/^\d+$/u.test(length) || Number(length) > maxBytes)
      )
        return;
      const bytes = await response.body();
      if (!active() || record.epoch !== epoch || record.done || !current())
        return;
      const id = createdId(bytes, maxBytes);
      if (id === null) return;
      if (proofs.has(id)) return stop("ambiguous");
      proofs.set(
        id,
        Object.freeze({ chatSessionId: id, source: "browser_observed_create" }),
      );
    } catch {
      // Never expose response bodies, headers, URLs or exception details.
    } finally {
      if (claimed) complete(record);
    }
  };
  const lookup = async (id) => {
    if (!active() || !validId(id)) return null;
    try {
      if (!current()) return null;
    } catch {
      return null;
    }
    if (!pending) return proofs.get(id) ?? null;
    // Bound waiting callers as well as request candidates.
    if (waiters.size >= 16) return null;
    return new Promise((resolve) => waiters.add({ id, resolve }));
  };
  const isCurrent = (proof) => {
    try {
      return (
        active() &&
        pending === 0 &&
        current() &&
        object(proof) &&
        proofs.get(proof.chatSessionId) === proof
      );
    } catch {
      return false;
    }
  };
  const api = { lookup, isCurrent, dispose, status: () => state };
  if (
    !Number.isSafeInteger(maxBytes) ||
    maxBytes < 1 ||
    maxBytes > MAX_BYTES ||
    !Number.isSafeInteger(maxCandidates) ||
    maxCandidates < 1 ||
    maxCandidates > 16 ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 120_000
  ) {
    stop("invalid_options");
    return api;
  }
  if (signal?.aborted) {
    stop("cancelled");
    return api;
  }
  page.on("request", onRequest);
  page.on("response", onResponse);
  page.on("requestfailed", onFailure);
  page.on("framenavigated", onNavigation);
  page.on("close", onClose);
  signal?.addEventListener("abort", onAbort, { once: true });
  timer = setTimeout(() => stop("timeout"), timeoutMs);
  if (signal?.aborted) onAbort();
  return api;
}
