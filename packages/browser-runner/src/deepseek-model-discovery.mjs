import { probeDeepseekAccount } from "./consumer-web-identity.mjs";
import { observeDeepSeekModelConfiguration } from "./deepseek-web-configuration.mjs";

const ORIGIN = "https://chat.deepseek.com";

/**
 * Discover options on the runner-owned page, without selecting a model,
 * toggling search, filling the composer or submitting a conversation.
 * Configuration is observed from this navigation, never synthesized.
 */
export async function inspectDeepSeekMeasurementOptions(
  page,
  { timeoutMs = 15_000, signal, expectedAccountId } = {},
) {
  if (
    !Number.isFinite(timeoutMs) ||
    timeoutMs <= 0 ||
    timeoutMs > 120_000 ||
    (expectedAccountId !== undefined &&
      (typeof expectedAccountId !== "string" || !expectedAccountId.trim()))
  )
    return null;
  const deadline = performance.now() + timeoutMs;
  const controller = new AbortController();
  const abort = () => controller.abort();
  signal?.addEventListener("abort", abort, { once: true });
  if (signal?.aborted) abort();
  let observer;
  let snapshot;
  let timer;
  let invalidated = false;
  let watching = false;
  const navigation = (frame) => {
    if (watching && frame === page.mainFrame()) invalidated = true;
  };
  const check = () => {
    if (
      controller.signal.aborted ||
      performance.now() >= deadline ||
      invalidated ||
      new URL(page.url()).origin !== ORIGIN
    )
      throw new Error("discovery_unavailable");
  };
  const interrupted = new Promise((_, reject) => {
    controller.signal.addEventListener(
      "abort",
      () => reject(new Error("discovery_unavailable")),
      { once: true },
    );
    timer = setTimeout(abort, timeoutMs);
  });
  // Attach rejection handling even when an already-aborted input exits early.
  interrupted.catch(() => {});
  const run = async (operation) => {
    check();
    const result = await Promise.race([
      Promise.resolve().then(() => {
        check();
        return operation();
      }),
      interrupted,
    ]);
    check();
    return result;
  };
  try {
    page.on("framenavigated", navigation);
    const before = await run(() => probeDeepseekAccount(page));
    if (!before?.platform_account_id) return null;
    if (
      expectedAccountId !== undefined &&
      before.platform_account_id !== expectedAccountId
    )
      return null;
    observer = observeDeepSeekModelConfiguration(page, {
      timeoutMs: Math.max(1, deadline - performance.now()),
      signal: controller.signal,
    });
    await run(() =>
      page.goto(`${ORIGIN}/`, {
        waitUntil: "domcontentloaded",
        timeout: Math.max(1, deadline - performance.now()),
      }),
    );
    if (page.url() !== `${ORIGIN}/`) return null;
    watching = true;
    const configuration = await run(() => observer.result);
    if (!configuration) return null;
    const after = await run(() => probeDeepseekAccount(page));
    if (after?.platform_account_id !== before.platform_account_id) return null;
    snapshot = await run(() =>
      page.waitForFunction(
        ({ origin, configs }) => {
          if (location.href !== `${origin}/`) return null;
          const visible = (node) =>
            !node.closest("[aria-hidden='true'], [data-role='measure']") &&
            node.getClientRects().length > 0 &&
            !["hidden", "collapse"].includes(getComputedStyle(node).visibility);
          const enabled = (node) =>
            !node.matches(":disabled, [aria-disabled='true']") &&
            ![...node.classList].some((name) => name.endsWith("--disabled"));
          const composers = [
            ...document.querySelectorAll('textarea[name="search"]'),
          ].filter(visible);
          if (
            composers.length !== 1 ||
            !enabled(composers[0]) ||
            composers[0].readOnly
          )
            return null;
          const search = [...document.querySelectorAll("div.ds-toggle-button")]
            .filter(visible)
            .filter((node) =>
              /^(?:Search|智能搜索)$/u.test(node.textContent.trim()),
            );
          if (
            search.length !== 1 ||
            !enabled(search[0]) ||
            !["true", "false"].includes(search[0].getAttribute("aria-pressed"))
          )
            return null;
          const available = configs.filter(
            (item) => item.enabled && item.switchable,
          );
          const radios = [
            ...document.querySelectorAll('[role="radio"][data-model-type]'),
          ].filter(visible);
          const models = [];
          let selected = null;
          for (const config of available) {
            const matches = radios.filter(
              (node) =>
                node.getAttribute("data-model-type") === config.model_type,
            );
            if (
              matches.length === 0 &&
              radios.length === 0 &&
              available.length === 1
            ) {
              models.push({ id: config.model_type, label: config.model_type });
              continue;
            }
            if (matches.length !== 1) return null;
            const radio = matches[0];
            if (!enabled(radio)) continue;
            const checked = radio.getAttribute("aria-checked");
            const label = radio.textContent.trim().replace(/\s+/gu, " ");
            if (
              !["true", "false"].includes(checked) ||
              !label ||
              label.length > 200
            )
              return null;
            models.push({ id: config.model_type, label });
            if (checked === "true") {
              if (selected !== null) return null;
              selected = config.model_type;
            }
          }
          return models.length ? { models, selected_model: selected } : null;
        },
        { origin: ORIGIN, configs: configuration.model_configs },
        { timeout: Math.max(1, deadline - performance.now()), polling: 100 },
      ),
    );
    const result = await run(() => snapshot.jsonValue());
    const finalIdentity = await run(() => probeDeepseekAccount(page));
    return finalIdentity?.platform_account_id === before.platform_account_id
      ? result
      : null;
  } catch {
    // Page/transport errors may contain private URLs, tokens or page text.
    return null;
  } finally {
    clearTimeout(timer);
    observer?.dispose();
    snapshot?.dispose().catch(() => {});
    page.off("framenavigated", navigation);
    signal?.removeEventListener("abort", abort);
    controller.abort();
  }
}
