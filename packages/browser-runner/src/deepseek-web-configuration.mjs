const ORIGIN = "https://chat.deepseek.com";
const MAX_BYTES = 128 * 1024;
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const validLimit = (value) =>
  Number.isSafeInteger(value) && value > 0 && value <= MAX_BYTES;

/**
 * Pure configuration-envelope validation, not answer interpretation.
 * Strip everything except model identifiers and verified boolean flags.
 * A default in site configuration does not prove the page's actual selection.
 */
export function parseDeepSeekModelConfiguration(
  bytes,
  { maxBytes = MAX_BYTES } = {},
) {
  try {
    if (
      !validLimit(maxBytes) ||
      !(bytes instanceof Uint8Array) ||
      bytes.byteLength > maxBytes
    )
      return null;
    const envelope = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    );
    const data = envelope?.data;
    const payload = data?.biz_data;
    const configs = payload?.settings?.model_configs;
    if (
      !object(envelope) ||
      envelope.code !== 0 ||
      !object(data) ||
      data.biz_code !== 0 ||
      !object(payload) ||
      !Number.isFinite(payload.version) ||
      !object(payload.settings) ||
      !object(configs) ||
      !Number.isFinite(configs.id) ||
      !Array.isArray(configs.value) ||
      configs.value.length === 0 ||
      configs.value.length > 64
    )
      return null;
    const model_configs = [];
    const selectable = new Set();
    for (const item of configs.value) {
      if (
        !object(item) ||
        typeof item.model_type !== "string" ||
        !/^[a-zA-Z0-9_.-]{1,128}$/u.test(item.model_type) ||
        typeof item.enabled !== "boolean" ||
        typeof item.switchable !== "boolean"
      )
        return null;
      const normalized = {
        model_type: item.model_type,
        enabled: item.enabled,
        switchable: item.switchable,
      };
      for (const key of [
        "is_default",
        "think_feature",
        "search_feature",
        "file_feature",
      ]) {
        if (Object.hasOwn(item, key) && typeof item[key] === "boolean") {
          normalized[key] = item[key];
        }
      }
      if (item.enabled && item.switchable) {
        if (selectable.has(item.model_type)) return null;
        selectable.add(item.model_type);
      }
      model_configs.push(normalized);
    }
    if (!selectable.size) return null;
    return { model_configs, selected_model: null };
  } catch {
    return null;
  }
}

function matches(response) {
  const url = new URL(response.url());
  return (
    url.origin === ORIGIN &&
    url.pathname === "/api/v0/client/settings" &&
    !url.username &&
    !url.password &&
    !url.hash &&
    url.searchParams.getAll("scope").length === 1 &&
    url.searchParams.get("scope") === "model" &&
    response.request().method() === "GET"
  );
}

/**
 * Arm before caller-owned navigation. Never navigates, reloads or fetches.
 * First matching response wins, including rejection; output never includes
 * URLs, headers, opaque query identifiers, raw bodies or exception details.
 *
 * Playwright body() buffers the response; maxBytes bounds accepted/parsed
 * bytes, not browser/Playwright transport memory. Deployment resource limits
 * remain required. This standalone observer does not enable the adapter.
 */
export function observeDeepSeekModelConfiguration(
  page,
  { signal, timeoutMs = 10_000, maxBytes = MAX_BYTES } = {},
) {
  let resolve;
  const result = new Promise((done) => {
    resolve = done;
  });
  let settled = false;
  let claimed = false;
  let timer;
  let epoch = 0;
  let navigating = false;
  const requests = new WeakMap();
  const mainFrame = page.mainFrame();
  const current = () =>
    !navigating &&
    page.mainFrame() === mainFrame &&
    new URL(mainFrame.url()).origin === ORIGIN;
  const finish = (value = null) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    page.off("response", onResponse);
    page.off("request", onRequest);
    page.off("framenavigated", onNavigation);
    page.off("close", dispose);
    signal?.removeEventListener("abort", dispose);
    resolve(value);
  };
  const dispose = () => finish();
  const invalidate = () => {
    epoch++;
    // A claimed response belongs to the preceding document. Fail closed;
    // never allow its delayed completion to overwrite a later document.
    if (claimed) finish();
  };
  const onNavigation = (frame) => {
    if (!settled && frame === mainFrame) {
      navigating = false;
      invalidate();
    }
  };
  const onRequest = (request) => {
    if (settled) return;
    try {
      if (request.frame() !== mainFrame) return;
      // Invalidate at navigation start as well as commit, including reloads
      // whose final URL is identical. Pre-arm requests have no WeakMap entry.
      if (request.isNavigationRequest()) {
        navigating = true;
        invalidate();
        return;
      }
      if (current()) requests.set(request, epoch);
    } catch {
      // Service-worker requests may have no frame; they are not page proof.
    }
  };
  const onResponse = async (response) => {
    if (settled || claimed) return;
    try {
      const request = response.request();
      const requestEpoch = requests.get(request);
      if (
        requestEpoch === undefined ||
        requestEpoch !== epoch ||
        request.frame() !== mainFrame ||
        !current() ||
        !matches(response)
      )
        return;
      claimed = true;
      if (response.status() !== 200) return finish();
      const length = await response.headerValue("content-length");
      if (settled) return;
      if (requestEpoch !== epoch || !current()) return finish();
      if (
        length !== null &&
        (!/^\d+$/u.test(length) || Number(length) > maxBytes)
      )
        return finish();
      const bytes = await response.body();
      if (settled) return;
      if (requestEpoch !== epoch || !current()) return finish();
      finish(parseDeepSeekModelConfiguration(bytes, { maxBytes }));
    } catch {
      finish();
    }
  };
  if (
    !validLimit(maxBytes) ||
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 120_000 ||
    signal?.aborted
  ) {
    finish();
    return { result, dispose };
  }
  page.on("response", onResponse);
  page.on("request", onRequest);
  page.on("framenavigated", onNavigation);
  page.on("close", dispose);
  signal?.addEventListener("abort", dispose, { once: true });
  timer = setTimeout(dispose, timeoutMs);
  if (signal?.aborted) dispose();
  return { result, dispose };
}
