import { createConnectJsonDecoder } from "./connect-json.mjs";

// Chromium's ordinary getResponseBody path can UTF-8-decode +json MIME,
// replacing binary frame-header bytes. Network streamResourceContent exposes
// base64 bytes without changing the page's fetch, cookies, or response.
async function observeWireBytes(page, endpoint, maxBytes) {
  const session = await page.context().newCDPSession(page);
  let resolve;
  const body = new Promise((done) => (resolve = done));
  let requestId;
  let ended = false;
  let enabled = false;
  let closed = false;
  let size = 0;
  let prefix = Buffer.alloc(0);
  const chunks = [];
  const fail = () => {
    closed = true;
    resolve(null);
  };
  const decode = (data) => {
    if (typeof data !== "string" || data.length > Math.ceil(maxBytes / 3) * 4)
      return null;
    const bytes = Buffer.from(data, "base64");
    size += bytes.length;
    return size <= maxBytes ? bytes : null;
  };
  const finish = () => {
    if (!closed && enabled && ended) {
      closed = true;
      resolve(Buffer.concat([prefix, ...chunks]));
    }
  };
  session.on("Network.responseReceived", (event) => {
    if (event.response.url !== endpoint || closed) return;
    if (requestId) {
      fail();
      return;
    }
    requestId = event.requestId;
    session
      .send("Network.streamResourceContent", { requestId })
      .then((result) => {
        if (closed) return;
        prefix = decode(result.bufferedData);
        if (!prefix) return fail();
        enabled = true;
        finish();
      }, fail);
  });
  session.on("Network.dataReceived", (event) => {
    if (closed || event.requestId !== requestId || event.data === undefined)
      return;
    const bytes = decode(event.data);
    if (!bytes) return fail();
    chunks.push(bytes);
  });
  session.on("Network.loadingFinished", (event) => {
    if (event.requestId !== requestId) return;
    ended = true;
    finish();
  });
  session.on("Network.loadingFailed", (event) => {
    if (event.requestId === requestId) fail();
  });
  try {
    await session.send("Network.enable");
  } catch {
    await session.detach().catch(() => {});
    throw new Error("Connect raw-byte observation unavailable");
  }
  return {
    body,
    async close() {
      fail();
      await session.detach().catch(() => {});
    },
  };
}

/**
 * Capture the response to one browser-owned UI submission. No independent
 * HTTP client, copied credentials, request retry or provider-ID synthesis.
 * The caller validates the exact outgoing request and interprets provider
 * messages; a valid Connect stream alone is NOT proof of official search.
 *
 * Real Chromium pages use raw CDP stream bytes, never response.body().
 * Byte bounds apply to the observer, not Chromium's overall process memory.
 */
export async function captureConnectExchange(
  page,
  {
    endpoint,
    submit,
    matchRequest,
    signal,
    timeoutMs = 30_000,
    maxFrameBytes = 1024 * 1024,
    maxTotalBytes = 8 * 1024 * 1024,
    maxMessages = 256,
  },
) {
  if (
    signal?.aborted ||
    typeof submit !== "function" ||
    typeof matchRequest !== "function" ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0
  )
    return null;
  try {
    const url = new URL(endpoint);
    if (
      !["http:", "https:"].includes(url.protocol) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash
    )
      return null;
    createConnectJsonDecoder({ maxFrameBytes, maxTotalBytes, maxMessages });
  } catch {
    return null;
  }

  let closed = false;
  let matches = 0;
  let timer;
  let wire;
  let deliverResponse;
  let stopWaiting;
  const controller = new AbortController();
  const firstResponse = new Promise((resolve) => {
    deliverResponse = resolve;
  });
  const stopped = new Promise((resolve) => {
    stopWaiting = resolve;
  });
  const abort = () => {
    controller.abort();
    stopWaiting(null);
  };
  const startedAt = new Date().toISOString();
  const onResponse = (response) => {
    if (closed || controller.signal.aborted) return;
    try {
      if (response.url() !== endpoint) return;
      if (
        response.request().method() !== "POST" ||
        matchRequest(response.request()) !== true
      ) {
        // Raw-byte observation deliberately permits only one endpoint
        // exchange. Never pair another request's bytes with a matched request.
        if (wire) abort();
        return;
      }
    } catch {
      return;
    }
    matches += 1;
    // Retried or competing matching submissions are ambiguous. Do not choose
    // a convenient successful response and hide the other request.
    if (matches !== 1) {
      abort();
      return;
    }
    const capture = (async () => {
      try {
        if (
          response.status() !== 200 ||
          !/^application\/connect\+json(?:\s*;|$)/i.test(
            response.headers()["content-type"] ?? "",
          )
        )
          return null;
        const length = response.headers()["content-length"];
        if (
          length !== undefined &&
          (!/^\d+$/.test(length) ||
            !Number.isSafeInteger(Number(length)) ||
            Number(length) > maxTotalBytes)
        )
          return null;
        const body = wire ? await wire.body : await response.body();
        if (!body) return null;
        if (closed || controller.signal.aborted) return null;
        const decoder = createConnectJsonDecoder({
          maxFrameBytes,
          maxTotalBytes,
          maxMessages,
        });
        decoder.push(body);
        const decoded = decoder.finish();
        return {
          messages: decoded.messages,
          // These timestamps are local observation times, not provider time.
          started_at: startedAt,
          received_at: new Date().toISOString(),
        };
      } catch {
        // Never expose payloads, browser errors, URLs or credentials.
        return null;
      }
    })();
    deliverResponse(capture);
  };

  page.on("response", onResponse);
  signal?.addEventListener("abort", abort, { once: true });
  try {
    if (signal?.aborted) return null;
    timer = setTimeout(abort, timeoutMs);
    // Structural doubles exercise lifecycle handling without a browser. A
    // real page must support byte-preserving observation; no text fallback.
    if (typeof page.context === "function") {
      const opening = observeWireBytes(page, endpoint, maxTotalBytes).then(
        async (observer) => {
          if (closed || controller.signal.aborted) {
            await observer.close();
            return null;
          }
          return observer;
        },
      );
      wire = await Promise.race([opening, stopped]);
      if (!wire || controller.signal.aborted) return null;
    }
    return await Promise.race([
      (async () => {
        await submit(page, controller.signal);
        if (closed || controller.signal.aborted) return null;
        const captured = await firstResponse;
        return matches === 1 && !controller.signal.aborted ? captured : null;
      })(),
      stopped,
    ]);
  } catch {
    return null;
  } finally {
    closed = true;
    clearTimeout(timer);
    controller.abort();
    page.off("response", onResponse);
    signal?.removeEventListener("abort", abort);
    await wire?.close();
  }
}
