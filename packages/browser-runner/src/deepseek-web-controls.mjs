const ORIGIN = "https://chat.deepseek.com";
class ControlError extends Error {
  constructor(reason) {
    super(`deepseek_ui_${reason}`);
  }
}
const fail = (reason) => {
  throw new ControlError(reason);
};

function guard(page, { signal, timeoutMs = 10_000 }) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > 120_000)
    fail("invalid_deadline");
  const deadline = performance.now() + timeoutMs;
  const check = () => {
    if (signal?.aborted) fail("cancelled");
    if (performance.now() >= deadline) fail("deadline");
    let origin;
    try {
      origin = new URL(page.url()).origin;
    } catch {
      fail("origin");
    }
    if (origin !== ORIGIN) fail("origin");
  };
  return {
    check,
    async run(operation) {
      check();
      let timer;
      let abort;
      try {
        const interrupted = new Promise((_, reject) => {
          abort = () => reject(new ControlError("cancelled"));
          signal?.addEventListener("abort", abort, { once: true });
          timer = setTimeout(
            () => reject(new ControlError("deadline")),
            Math.max(1, deadline - performance.now()),
          );
        });
        const result = await Promise.race([
          operation(Math.max(1, deadline - performance.now())),
          interrupted,
        ]);
        check();
        return result;
      } catch (error) {
        // Playwright action errors can include filled text and page content.
        // Never return their message, cause, stack or other original fields.
        if (error instanceof ControlError) throw error;
        check();
        fail("operation_failed");
      } finally {
        clearTimeout(timer);
        signal?.removeEventListener("abort", abort);
      }
    },
  };
}

async function unique(page, selector, gate) {
  const locator = page.locator(selector);
  if ((await gate.run(() => locator.count())) !== 1) fail("target_ambiguous");
  // Pin the node: a replacement must not silently become the click target.
  const handle = await gate.run(() => locator.elementHandle());
  if (!handle || !(await gate.run(() => handle.isVisible())))
    fail("target_missing");
  return handle;
}

async function state(handle, attribute, gate) {
  const value = await gate.run(() => handle.getAttribute(attribute));
  if (value !== "true" && value !== "false") fail("state_unverified");
  return value === "true";
}

async function enabled(handle, gate) {
  const disabled = await gate.run(() =>
    handle.evaluate(
      (element) =>
        element.matches(":disabled, [aria-disabled='true']") ||
        [...element.classList].some((name) => name.endsWith("--disabled")),
    ),
  );
  if (disabled) fail("disabled");
}

const visible =
  ":visible:not([aria-hidden='true']):not([aria-hidden='true'] *):not([data-role='measure']):not([data-role='measure'] *)";
const radios = `[role="radio"][data-model-type]${visible}`;
const composerSelector = `textarea[name="search"]${visible}`;

/**
 * Source-grounded UI adapter only; the caller owns current configuration,
 * account/session authorization and request binding. No measurement proof.
 * Cancellation prevents subsequent operations, not an already dispatched
 * Playwright action. Never retry this operation to resolve an unknown send.
 */
export async function configureDeepSeekPage(
  page,
  { model, modelConfigs, searchEnabled, thinkingEnabled, ...limits } = {},
) {
  const gate = guard(page, limits);
  gate.check();
  if (
    typeof model !== "string" ||
    !/^[\w.-]{1,128}$/u.test(model) ||
    !Array.isArray(modelConfigs) ||
    modelConfigs.length > 64 ||
    typeof searchEnabled !== "boolean" ||
    typeof thinkingEnabled !== "boolean"
  )
    fail("invalid_configuration");
  const models = modelConfigs.filter(
    (item) => item?.enabled === true && item?.switchable === true,
  );
  if (
    !models.some((item) => item.model_type === model) ||
    new Set(models.map((item) => item.model_type)).size !== models.length
  )
    fail("model_unavailable");
  const composer = await unique(page, composerSelector, gate);
  await enabled(composer, gate);
  let modelVerification = "checked";
  const radioCount = await gate.run(() => page.locator(radios).count());
  if (radioCount === 0 && models.length === 1) {
    // The website omits its selector in this case. Configuration availability
    // alone does not prove which model the next request will actually use.
    modelVerification = "request_verification_required";
  } else {
    const radio = await unique(
      page,
      `${radios}[data-model-type="${model}"]`,
      gate,
    );
    await enabled(radio, gate);
    if (!(await state(radio, "aria-checked", gate)))
      await gate.run((timeout) => radio.click({ timeout }));
    if (!(await state(radio, "aria-checked", gate))) fail("model_unverified");
    if (
      (await gate.run(() =>
        page.locator(`${radios}[aria-checked="true"]`).count(),
      )) !== 1
    )
      fail("model_ambiguous");
  }
  const configuredToggles = [];
  for (const [labels, requested] of [
    ["Search|智能搜索", searchEnabled],
    ["DeepThink|深度思考", thinkingEnabled],
  ]) {
    const selector = `div.ds-toggle-button${visible}`;
    const candidates = page.locator(selector).filter({
      hasText: new RegExp(`^(?:${labels})$`, "u"),
    });
    if ((await gate.run(() => candidates.count())) !== 1)
      fail("toggle_ambiguous");
    const handle = await gate.run(() => candidates.elementHandle());
    await enabled(handle, gate);
    if ((await state(handle, "aria-pressed", gate)) !== requested)
      await gate.run((timeout) => handle.click({ timeout }));
    if ((await state(handle, "aria-pressed", gate)) !== requested)
      fail("toggle_unverified");
    configuredToggles.push([handle, requested]);
  }
  // Later controls may rerender or reset earlier choices; verify the final
  // combination, not just each transient state immediately after its click.
  for (const [handle, requested] of configuredToggles) {
    await enabled(handle, gate);
    if (
      !(await gate.run(() => handle.isVisible())) ||
      (await state(handle, "aria-pressed", gate)) !== requested
    )
      fail("toggle_unverified");
  }
  if (modelVerification === "checked") {
    const checked = await unique(page, `${radios}[aria-checked="true"]`, gate);
    if (
      (await gate.run(() => checked.getAttribute("data-model-type"))) !== model
    )
      fail("model_unverified");
  }
  return { model, modelVerification, searchEnabled, thinkingEnabled };
}

/** One invocation performs at most one Enter press via the website handler.
 * The caller must install request capture first and must not retry on error.
 * This does not establish acceptance, fresh-session ownership or completion.
 */
export async function submitDeepSeekQuestion(
  page,
  { question, ...limits } = {},
) {
  const gate = guard(page, limits);
  gate.check();
  if (
    typeof question !== "string" ||
    !question.trim() ||
    Buffer.byteLength(question) > 16_000
  )
    fail("invalid_question");
  const composer = await unique(page, composerSelector, gate);
  await enabled(composer, gate);
  if (!(await gate.run(() => composer.isEditable()))) fail("not_editable");
  await gate.run((timeout) => composer.fill(question, { timeout }));
  if ((await gate.run(() => composer.inputValue())) !== question)
    fail("question_unverified");
  await enabled(composer, gate);
  await gate.run((timeout) => composer.press("Enter", { timeout }));
  return { enterDispatched: true };
}
