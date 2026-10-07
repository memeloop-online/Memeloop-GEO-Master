import { captureConnectExchange } from "./connect-browser-capture.mjs";
import { parseExtractionJson } from "./ai-observation-parser.mjs";

const CHAT_PATH = "/apiv2/kimi.gateway.chat.v1.ChatService/Chat";
const ASSISTANT =
  '.chat-content-item-assistant, [data-message-author-role="assistant"], [data-role="assistant"]';

// Only correlate the browser's own request. Never interpret provider response
// fields here: the signed-in model performs that interpretation.
function containsPrompt(request, prompt) {
  try {
    const bytes = request.postDataBuffer();
    if (
      !bytes ||
      bytes.length > 4 * 1024 * 1024 ||
      bytes.length < 7 ||
      bytes[0] !== 0 ||
      bytes.readUInt32BE(1) !== bytes.length - 5 ||
      !/^application\/connect\+json(?:\s*;|$)/iu.test(
        request.headers()["content-type"] ?? "",
      )
    )
      return false;
    const value = JSON.parse(
      new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(5)),
    );
    const pending = [value];
    while (pending.length) {
      const item = pending.pop();
      if (typeof item === "string" && item === prompt) return true;
      if (item && typeof item === "object")
        pending.push(...Object.values(item));
    }
    return false;
  } catch {
    return false;
  }
}

/**
 * A separate conversation in the existing authenticated browser context.
 * This is an extraction model call, never a substitute measurement sample.
 * deadlineAt uses the runner's monotonic performance.now() clock.
 */
export async function extractWithSignedInBrowser(
  page,
  prompt,
  {
    model,
    trustedOrigin,
    deadlineAt = performance.now() + 60_000,
    signal,
    configureModel,
  } = {},
) {
  let extractionPage;
  let timer;
  let closePromise;
  const controller = new AbortController();
  let stop;
  const stopped = new Promise((resolve) => (stop = resolve));
  const remaining = () => Math.max(0, deadlineAt - performance.now());
  const active = () => !controller.signal.aborted && remaining() > 0;
  const close = () => {
    if (extractionPage && !closePromise)
      closePromise = Promise.resolve()
        .then(() => extractionPage.close())
        .catch(() => {});
    return closePromise;
  };
  const abort = () => {
    controller.abort();
    stop(null);
    void close();
  };
  try {
    if (
      !page ||
      typeof prompt !== "string" ||
      !prompt.trim() ||
      Buffer.byteLength(prompt) > 2 * 1024 * 1024 ||
      typeof model !== "string" ||
      !model ||
      typeof configureModel !== "function" ||
      !Number.isFinite(deadlineAt) ||
      remaining() <= 0 ||
      signal?.aborted
    )
      return null;
    const origin = new URL(trustedOrigin);
    if (
      origin.origin !== trustedOrigin ||
      origin.username ||
      origin.password ||
      (origin.protocol !== "https:" &&
        !(
          origin.protocol === "http:" &&
          ["localhost", "127.0.0.1", "[::1]"].includes(origin.hostname)
        ))
    )
      return null;
    signal?.addEventListener("abort", abort, { once: true });
    timer = setTimeout(abort, remaining());
    const run = async () => {
      extractionPage = await page.context().newPage();
      if (!active()) {
        await close();
        return null;
      }
      await extractionPage.goto(`${trustedOrigin}/`, {
        waitUntil: "domcontentloaded",
        timeout: Math.max(1, remaining()),
      });
      if (!active() || new URL(extractionPage.url()).origin !== trustedOrigin)
        return null;
      if ((await configureModel(extractionPage, model)) !== true || !active())
        return null;
      // A redirected/restored previous conversation is not a blank extraction
      // session. Refuse it rather than choosing a convenient historical JSON.
      const answers = extractionPage.locator(ASSISTANT);
      if ((await answers.count()) !== 0 || !active()) return null;
      const composer = extractionPage.locator(
        '[role="textbox"][contenteditable="true"].chat-input-editor',
      );
      if (!(await composer.isVisible()) || !active()) return null;
      await composer.fill(prompt, { timeout: Math.max(1, remaining()) });
      if (!active()) return null;
      const exchange = await captureConnectExchange(extractionPage, {
        endpoint: `${trustedOrigin}${CHAT_PATH}`,
        signal: controller.signal,
        timeoutMs: Math.max(1, remaining()),
        maxMessages: 32_768,
        matchRequest: (request) => containsPrompt(request, prompt),
        submit: async (_page, captureSignal) => {
          if (!active() || captureSignal.aborted) return;
          const send = extractionPage.locator(".send-button-container");
          const classes = await send.getAttribute("class");
          if (
            !active() ||
            captureSignal.aborted ||
            classes?.split(/\s+/u).includes("disabled")
          )
            throw new Error("extraction_unavailable");
          await send.click({ timeout: Math.max(1, remaining()) });
        },
      });
      if (!exchange || !active()) return null;
      // DOM rendering can trail the final transport frame. Poll only the
      // assistant surface; never parse the page body or user prompt echo.
      while (active()) {
        if ((await answers.count()) === 1) {
          // The official surface separates tool/reasoning markdown from the
          // final markdown. Read only the latter, not surrounding action labels.
          const markdown = answers.locator(
            ".markdown-container:not(.toolcall-content-text) > .markdown",
          );
          const content =
            (await markdown.count()) > 0 ? markdown.last() : answers;
          const code = content.locator("pre code");
          const body = (await code.count()) === 1 ? code : content;
          const text = await body.innerText({
            timeout: Math.max(1, remaining()),
          });
          const extracted = parseExtractionJson(text);
          if (
            active() &&
            extracted !== null &&
            typeof extracted === "object" &&
            !Array.isArray(extracted)
          )
            return { extracted, model, surface: "signed_in_browser" };
        }
        await new Promise((resolve) =>
          setTimeout(resolve, Math.min(50, remaining())),
        );
      }
      return null;
    };
    return await Promise.race([run(), stopped]);
  } catch {
    // Browser errors can contain account URLs, source material or credentials.
    return null;
  } finally {
    controller.abort();
    clearTimeout(timer);
    signal?.removeEventListener("abort", abort);
    await close();
  }
}
