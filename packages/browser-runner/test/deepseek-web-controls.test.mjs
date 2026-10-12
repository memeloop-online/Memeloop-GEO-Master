import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import {
  configureDeepSeekPage,
  submitDeepSeekQuestion,
} from "../src/deepseek-web-controls.mjs";

const options = {
  model: "fixture-b",
  modelConfigs: ["fixture-a", "fixture-b"].map((model_type) => ({
    model_type,
    enabled: true,
    switchable: true,
  })),
  searchEnabled: true,
  thinkingEnabled: false,
};
function html(chinese = false) {
  return `
    <textarea name="user query">Do not edit</textarea>
    <textarea name="search"></textarea>
    <div role="radio" data-model-type="fixture-a" aria-checked="true">A</div>
    <div role="radio" data-model-type="fixture-b" aria-checked="false">B</div>
    <div role="radio" data-model-type="fixture-b" data-role="measure"
      aria-hidden="true" aria-checked="false">Hidden measuring duplicate</div>
    <div class="ds-toggle-button" aria-pressed="false">${chinese ? "智能搜索" : "Search"}</div>
    <div class="ds-toggle-button" aria-pressed="true">${chinese ? "深度思考" : "DeepThink"}</div>
    <script>
    window.sends = 0; window.clicks = 0;
    document.querySelectorAll('[role=radio]').forEach(node => {
      node.onclick = () => {
        window.clicks++;
        document.querySelectorAll('[role=radio]').forEach(
          item => item.setAttribute('aria-checked', String(item === node)));
      };
    });
    document.querySelectorAll('.ds-toggle-button').forEach(node => {
      node.onclick = () => {
        window.clicks++;
        node.setAttribute('aria-pressed', String(node.getAttribute('aria-pressed') !== 'true'));
      };
    });
    document.querySelector('[name=search]').onkeydown = event => {
      if(event.key === 'Enter') { event.preventDefault(); window.sends++; }
    };
    </script>`;
}
async function fixture(run) {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  try {
    const page = await browser.newPage();
    await page.setContent(html());
    const facade = {
      url: () => "https://chat.deepseek.com/",
      locator: page.locator.bind(page),
    };
    await run(page, facade);
  } finally {
    await browser.close();
  }
}

test("real browser configures localized div toggles, excludes measuring duplicate and sends once", async () => {
  await fixture(async (page, facade) => {
    for (const chinese of [false, true]) {
      await page.setContent(html(chinese));
      assert.deepEqual(await configureDeepSeekPage(facade, options), {
        model: options.model,
        modelVerification: "checked",
        searchEnabled: true,
        thinkingEnabled: false,
      });
      assert.equal(await page.evaluate(() => window.clicks), 3);
      await configureDeepSeekPage(facade, options);
      assert.equal(await page.evaluate(() => window.clicks), 3);
      await submitDeepSeekQuestion(facade, { question: "Synthetic question?" });
      assert.equal(await page.evaluate(() => window.sends), 1);
      assert.equal(
        await page.locator('[name="user query"]').inputValue(),
        "Do not edit",
      );
    }
  });
});

test("missing, duplicate, disabled, unchanged and editing-only controls fail closed", async () => {
  await fixture(async (page, facade) => {
    for (const mutation of [
      () => document.querySelector("[name=search]").remove(),
      () =>
        document.querySelector("[name=search]").setAttribute("disabled", ""),
      () =>
        document.body.append(
          document.querySelector("[name=search]").cloneNode(),
        ),
      () =>
        (document.querySelector('[data-model-type="fixture-b"]').onclick =
          null),
      () => (document.querySelector(".ds-toggle-button").onclick = null),
      () =>
        document
          .querySelector(".ds-toggle-button")
          .classList.add("ds-toggle-button--disabled"),
    ]) {
      await page.setContent(html());
      await page.evaluate(mutation);
      await assert.rejects(configureDeepSeekPage(facade, options));
      assert.equal(await page.evaluate(() => window.sends), 0);
    }
    await page.setContent('<textarea name="user query">Original</textarea>');
    await assert.rejects(
      submitDeepSeekQuestion(facade, { question: "Wrong target" }),
    );
    assert.equal(await page.locator("textarea").inputValue(), "Original");
  });
});

test("singleton absent selector explicitly requires actual request model verification", async () => {
  await fixture(async (page, facade) => {
    await page
      .locator("[role=radio]")
      .evaluateAll((nodes) => nodes.forEach((node) => node.remove()));
    const result = await configureDeepSeekPage(facade, {
      ...options,
      modelConfigs: options.modelConfigs.slice(1),
    });
    assert.equal(result.modelVerification, "request_verification_required");
    await assert.rejects(configureDeepSeekPage(facade, options));
  });
});

test("later toggle changes cannot silently invalidate an earlier verified choice", async () => {
  await fixture(async (page, facade) => {
    await page
      .locator(".ds-toggle-button")
      .last()
      .evaluate((node) => {
        const original = node.onclick;
        node.onclick = () => {
          original();
          document
            .querySelector('[data-model-type="fixture-b"]')
            .setAttribute("aria-checked", "false");
          document
            .querySelector('[data-model-type="fixture-a"]')
            .setAttribute("aria-checked", "true");
        };
      });
    await assert.rejects(
      configureDeepSeekPage(facade, options),
      /model_unverified/,
    );
    assert.equal(await page.evaluate(() => window.sends), 0);
  });
});

test("readonly and disabled composers cannot submit, and cancellation after Enter never retries", async () => {
  await fixture(async (page, facade) => {
    for (const attribute of ["disabled", "readonly"]) {
      await page.setContent(html());
      await page
        .locator("[name=search]")
        .evaluate((node, attr) => node.setAttribute(attr, ""), attribute);
      await assert.rejects(
        submitDeepSeekQuestion(facade, { question: "Synthetic question?" }),
      );
      assert.equal(await page.evaluate(() => window.sends), 0);
    }
    await page.setContent(html());
    const controller = new AbortController();
    await page.exposeFunction("cancelAfterSend", () => controller.abort());
    await page.locator("[name=search]").evaluate((node) => {
      const original = node.onkeydown;
      node.onkeydown = (event) => {
        original(event);
        window.cancelAfterSend();
      };
    });
    await assert.rejects(
      submitDeepSeekQuestion(facade, {
        question: "Synthetic question?",
        signal: controller.signal,
      }),
      /cancelled/,
    );
    assert.equal(await page.evaluate(() => window.sends), 1);
  });
});

test("origin and cancellation are checked before actions and after awaited reads", async () => {
  await fixture(async (page, facade) => {
    const controller = new AbortController();
    controller.abort();
    await assert.rejects(
      configureDeepSeekPage(facade, { ...options, signal: controller.signal }),
      /cancelled/,
    );
    await assert.rejects(
      submitDeepSeekQuestion(
        { ...facade, url: () => "https://example.org/" },
        { question: "No send" },
      ),
      /origin/,
    );
    let reads = 0;
    await assert.rejects(
      configureDeepSeekPage(
        {
          ...facade,
          url: () => (++reads < 4 ? facade.url() : "https://example.org/"),
        },
        options,
      ),
      /origin/,
    );
    assert.equal(await page.evaluate(() => window.clicks + window.sends), 0);
    const pending = {
      ...facade,
      locator: () => ({ count: () => new Promise(() => {}) }),
    };
    await assert.rejects(
      configureDeepSeekPage(pending, { ...options, timeoutMs: 20 }),
      /deadline/,
    );
    const during = new AbortController();
    const result = configureDeepSeekPage(pending, {
      ...options,
      signal: during.signal,
    });
    during.abort();
    await assert.rejects(result, /cancelled/);
  });
});

test("detachment before real browser fill returns only a bounded generic error without question text", async () => {
  await fixture(async (page, facade) => {
    const question = "SYNTHETIC_QUESTION_SENTINEL_NOT_FOR_DIAGNOSTICS";
    let fillAttempts = 0;
    const detachingPage = {
      ...facade,
      locator(selector) {
        const locator = page.locator(selector);
        return {
          count: () => locator.count(),
          async elementHandle() {
            const handle = await locator.elementHandle();
            // Deterministically detach after the final pre-fill read, then
            // exercise Playwright's real fill rejection, not a made-up error.
            return {
              isVisible: () => handle.isVisible(),
              evaluate: (...args) => handle.evaluate(...args),
              async isEditable() {
                const editable = await handle.isEditable();
                await handle.evaluate((node) => node.remove());
                return editable;
              },
              fill(...args) {
                fillAttempts++;
                return handle.fill(...args);
              },
            };
          },
        };
      },
    };
    await assert.rejects(
      submitDeepSeekQuestion(detachingPage, { question }),
      (error) => {
        assert.equal(error.message, "deepseek_ui_operation_failed");
        assert.equal(error.cause, undefined);
        assert.equal(String(error.stack).includes(question), false);
        assert.equal(JSON.stringify(error).includes(question), false);
        return true;
      },
    );
    assert.equal(fillAttempts, 1);
    assert.equal(await page.evaluate(() => window.sends), 0);
  });
});
