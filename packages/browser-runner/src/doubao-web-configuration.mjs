const ORIGIN = "https://www.doubao.com";
const MAX_BYTES = 128 * 1024;
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const identifier = (value) =>
  typeof value === "string" && /^[a-zA-Z0-9_.-]{1,128}$/u.test(value);
const name = (value) =>
  typeof value === "string" &&
  value.trim().length > 0 &&
  value.length <= 256 &&
  !/[\u0000-\u001f\u007f]/u.test(value);
const validLimit = (value) =>
  Number.isSafeInteger(value) && value > 0 && value <= MAX_BYTES;
const gate = (value) => (typeof value === "boolean" ? value : null);

function models(list) {
  if (
    !object(list) ||
    !Array.isArray(list.item_list) ||
    list.item_list.length > 64
  )
    throw new Error("invalid configuration");
  const seen = new Set();
  return list.item_list.map((item) => {
    if (!object(item) || !name(item.name))
      throw new Error("invalid configuration");
    const hasKey = Object.hasOwn(item, "model_item_key");
    const hasId = Object.hasOwn(item, "item_id");
    if (
      (hasKey && !identifier(item.model_item_key)) ||
      (hasId &&
        !identifier(item.item_id) &&
        !(Number.isSafeInteger(item.item_id) && item.item_id >= 0)) ||
      (!hasKey && !hasId)
    )
      throw new Error("invalid configuration");
    const selector = hasKey ? item.model_item_key : String(item.item_id);
    if (seen.has(selector)) throw new Error("invalid configuration");
    seen.add(selector);
    return {
      selector,
      ...(hasKey ? { model_item_key: item.model_item_key } : {}),
      ...(hasId ? { item_id: item.item_id } : {}),
      name: item.name,
      need_login: gate(item.need_login),
      need_upgrade: gate(item.subscribe_config?.need_upgrade),
    };
  });
}

/**
 * Projects an explicit menu_conf_v2 value, NOT a raw HTTP response envelope.
 * These are configuration candidates and scoped UI selectors, not foundation
 * model identities, account entitlements, actual selection or capabilities.
 * Missing/unrecognized gates remain unknown; all unrelated fields are stripped.
 */
export function parseDoubaoMenuConfiguration(
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
    const menu = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    );
    if (
      !object(menu) ||
      typeof menu.use_mode_model_list !== "boolean" ||
      !object(menu.mode_list) ||
      !Array.isArray(menu.mode_list.item_list) ||
      menu.mode_list.item_list.length > 64
    )
      return null;
    const seen = new Set();
    const mode_configs = menu.mode_list.item_list.map((mode) => {
      if (
        !object(mode) ||
        !identifier(mode.mode_id) ||
        !name(mode.name) ||
        seen.has(mode.mode_id)
      )
        throw new Error("invalid configuration");
      seen.add(mode.mode_id);
      return {
        mode_id: mode.mode_id,
        name: mode.name,
        need_login: gate(mode.need_login),
        ...(Object.hasOwn(mode, "model_list")
          ? { model_configs: models(mode.model_list) }
          : {}),
      };
    });
    if (
      menu.use_mode_model_list &&
      mode_configs.some((mode) => !Object.hasOwn(mode, "model_configs"))
    )
      return null;
    const model_configs = models(menu.model_list);
    if (
      menu.use_mode_model_list
        ? !mode_configs.some((mode) => mode.model_configs.length)
        : !model_configs.length
    )
      return null;
    return {
      use_mode_model_list: menu.use_mode_model_list,
      mode_configs,
      model_configs,
    };
  } catch {
    return null;
  }
}

/**
 * Raw response contract: the site's interceptor retains response.data and
 * requires numeric (code ?? status_code) === 0; its converter then consumes
 * data.entry_list. Use the evidenced action key, not guessed numeric enums.
 */
export function parseDoubaoActionBarConfiguration(
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
    const body = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    );
    if (
      !object(body) ||
      (body.code ?? body.status_code) !== 0 ||
      !object(body.data) ||
      !Array.isArray(body.data.entry_list) ||
      body.data.entry_list.length > 64
    )
      return null;
    const candidates = body.data.entry_list.filter(
      (entry) => object(entry) && entry.action_bar_key === "coco_deep_thinking",
    );
    if (candidates.length !== 1) return null;
    const active = candidates[0].active_switch_conf;
    if (!object(active) || active.use_menu_conf_v2 !== true) return null;
    return parseDoubaoMenuConfiguration(
      new TextEncoder().encode(JSON.stringify(active.menu_conf_v2)),
      { maxBytes },
    );
  } catch {
    return null;
  }
}

function matches(response) {
  const url = new URL(response.url());
  return (
    url.origin === ORIGIN &&
    url.pathname === "/alice/slot/action_bar_v3/brief_list" &&
    !url.username &&
    !url.password &&
    !url.hash &&
    response.request().method() === "POST"
  );
}

/**
 * Passive candidate observer; arm before caller-owned navigation. Never sends,
 * navigates, reloads or enables an adapter. First matching invalid response
 * settles null. body() buffers transport bytes; maxBytes bounds acceptance and
 * parsing, not browser/Playwright transport memory.
 */
export function observeDoubaoMenuConfiguration(
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
      if (request.isNavigationRequest()) {
        navigating = true;
        invalidate();
        return;
      }
      if (current()) requests.set(request, epoch);
    } catch {
      // Service-worker requests have no document provenance.
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
      if (!(bytes instanceof Uint8Array) || bytes.byteLength > maxBytes)
        return finish();
      finish(parseDoubaoActionBarConfiguration(bytes, { maxBytes }));
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
