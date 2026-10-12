import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { inspectDeepSeekMeasurementOptions } from "../src/deepseek-model-discovery.mjs";
import { configureDeepSeekPage } from "../src/deepseek-web-controls.mjs";

const origin = "https://chat.deepseek.com";
const configs = [
  {
    model_type: "fixture-a",
    enabled: true,
    switchable: true,
    search_feature: true,
  },
  {
    model_type: "fixture-b",
    enabled: true,
    switchable: true,
    search_feature: false,
  },
  { model_type: "fixture-disabled", enabled: false, switchable: true },
  { model_type: "fixture-unselectable", enabled: true, switchable: false },
];

async function fixture(run) {
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.GEO_TEST_CHROMIUM_PATH,
  });
  try {
    const page = await browser.newPage();
    const state = {
      configs,
      identityReads: 0,
      mismatch: false,
      missing: false,
      noSearch: false,
      requests: [],
      controls: true,
      delayedControls: false,
    };
    await page.route("**/*", async (route) => {
      const request = route.request();
      state.requests.push([request.method(), new URL(request.url()).pathname]);
      const path = new URL(request.url()).pathname;
      if (path === "/api/v0/users/current") {
        state.identityReads++;
        return route.fulfill({
          json: {
            code: 0,
            data: {
              biz_code: 0,
              biz_data: {
                id:
                  state.mismatch && state.identityReads > 1
                    ? "other"
                    : "synthetic",
                id_profile: { name: "Fixture" },
                is_guest: false,
              },
            },
          },
        });
      }
      if (path === "/api/v0/client/settings") {
        return route.fulfill({
          json: state.missing
            ? {}
            : {
                code: 0,
                data: {
                  biz_code: 0,
                  biz_data: {
                    version: 1,
                    settings: {
                      model_configs: { id: 1, value: state.configs },
                    },
                  },
                },
              },
        });
      }
      if (path !== "/") return route.abort();
      return route.fulfill({
        contentType: "text/html",
        body: `
        <textarea name="search"></textarea>
        ${
          state.controls
            ? `
          <div role="radio" data-model-type="fixture-a" aria-checked="true">Model A</div>
          <div role="radio" data-model-type="fixture-b" aria-checked="false">Model B</div>`
            : ""
        }
        <div class="ds-toggle-button" aria-pressed="false"
          ${state.noSearch ? 'aria-disabled="true"' : ""}>Search</div>
        <div class="ds-toggle-button" aria-pressed="false">DeepThink</div>
        <script>
          window.actions = 0;
          localStorage.setItem("userToken", JSON.stringify({__version:"0",value:"synthetic"}));
          document.addEventListener("click", () => window.actions++);
          document.addEventListener("keydown", () => window.actions++);
          document.querySelectorAll("[role=radio]").forEach(node => node.onclick = () => {
            document.querySelectorAll("[role=radio]").forEach(item =>
              item.setAttribute("aria-checked", String(item === node)));
          });
          document.querySelectorAll(".ds-toggle-button").forEach(node => node.onclick = () =>
            node.setAttribute("aria-pressed", String(node.getAttribute("aria-pressed") !== "true")));
          if (${state.delayedControls}) {
            const nodes = [...document.querySelectorAll("textarea, [role=radio], .ds-toggle-button")];
            nodes.forEach(node => node.hidden = true);
            setTimeout(() => nodes.forEach(node => node.hidden = false), 400);
          }
          fetch("/api/v0/client/settings?scope=model").then(() => window.configurationLoaded = true);
        </script>`,
      });
    });
    await page.goto(`${origin}/`);
    await page.waitForFunction(() => window.configurationLoaded === true);
    await run(page, state);
    assert.ok(state.requests.every(([method]) => method === "GET"));
    assert.ok(
      state.requests.every(([, path]) =>
        ["/", "/api/v0/users/current", "/api/v0/client/settings"].includes(
          path,
        ),
      ),
    );
  } finally {
    await browser.close();
  }
}

test("real Chrome discovers fresh selectable search models without any UI action", async () => {
  await fixture(async (page) => {
    assert.deepEqual(await inspectDeepSeekMeasurementOptions(page), {
      models: [
        { id: "fixture-a", label: "Model A" },
        { id: "fixture-b", label: "Model B" },
      ],
      selected_model: "fixture-a",
    });
    assert.equal(await page.evaluate(() => window.actions), 0);
    assert.equal(await page.locator("textarea").inputValue(), "");
    // IDs from discovery are exactly those consumed by the production controls.
    const configured = await configureDeepSeekPage(page, {
      model: "fixture-b",
      modelConfigs: configs,
      searchEnabled: true,
      thinkingEnabled: false,
    });
    assert.equal(configured.model, "fixture-b");
    assert.equal(await page.locator("textarea").inputValue(), "");
  });
});

test("single configured model never invents current selection", async () => {
  await fixture(async (page, state) => {
    state.configs = [configs[0]];
    state.controls = false;
    assert.deepEqual(await inspectDeepSeekMeasurementOptions(page), {
      models: [{ id: "fixture-a", label: "fixture-a" }],
      selected_model: null,
    });
    assert.equal(await page.evaluate(() => window.actions), 0);
  });
});

test("missing config, unavailable search and changed identity fail closed without send", async () => {
  for (const mode of ["missing", "noSearch", "mismatch"]) {
    await fixture(async (page, state) => {
      state[mode] = true;
      assert.equal(
        await inspectDeepSeekMeasurementOptions(page, { timeoutMs: 1000 }),
        null,
      );
      assert.equal(await page.evaluate(() => window.actions), 0);
    });
  }
});

test("waits within deadline for SPA controls after configuration has arrived", async () => {
  await fixture(async (page, state) => {
    state.delayedControls = true;
    const result = await inspectDeepSeekMeasurementOptions(page);
    assert.deepEqual(
      result.models.map((model) => model.id),
      ["fixture-a", "fixture-b"],
    );
    assert.equal(result.selected_model, "fixture-a");
    assert.equal(await page.evaluate(() => window.actions), 0);
  });
});

test("aborted and expired discovery perform no navigation or requests", async () => {
  await fixture(async (page, state) => {
    const count = state.requests.length;
    assert.equal(
      await inspectDeepSeekMeasurementOptions(page, {
        signal: AbortSignal.abort(),
      }),
      null,
    );
    assert.equal(
      await inspectDeepSeekMeasurementOptions(page, { timeoutMs: 0 }),
      null,
    );
    assert.equal(state.requests.length, count);
  });
});

test("deadline bounds stalled identity and cleans observers without sending", async () => {
  await fixture(async (page) => {
    await page.route("**/api/v0/users/current", () => {});
    const started = performance.now();
    assert.equal(
      await inspectDeepSeekMeasurementOptions(page, { timeoutMs: 40 }),
      null,
    );
    assert.ok(performance.now() - started < 1000);
    assert.equal(page.listenerCount("framenavigated"), 0);
    assert.equal(await page.evaluate(() => window.actions), 0);
  });
});

test("deadline bounds missing configuration and removes response observers", async () => {
  await fixture(async (page) => {
    await page.route("**/api/v0/client/settings?scope=model", () => {});
    const events = ["request", "response", "framenavigated", "close"];
    const counts = events.map((event) => page.listenerCount(event));
    const started = performance.now();
    assert.equal(
      await inspectDeepSeekMeasurementOptions(page, { timeoutMs: 300 }),
      null,
    );
    assert.ok(performance.now() - started < 1500);
    assert.deepEqual(
      events.map((event) => page.listenerCount(event)),
      counts,
    );
    assert.equal(await page.evaluate(() => window.actions), 0);
  });
});

test("persisted account mismatch returns no models and never navigates or submits", async () => {
  await fixture(async (page, state) => {
    const count = state.requests.length;
    assert.equal(
      await inspectDeepSeekMeasurementOptions(page, {
        expectedAccountId: "another-synthetic-account",
      }),
      null,
    );
    assert.deepEqual(state.requests.slice(count), [
      ["GET", "/api/v0/users/current"],
    ]);
    assert.equal(await page.evaluate(() => window.actions), 0);
  });
});

test("missing credentials cannot navigate or discover unauthenticated options", async () => {
  await fixture(async (page, state) => {
    await page.evaluate(() => localStorage.clear());
    const count = state.requests.length;
    assert.equal(await inspectDeepSeekMeasurementOptions(page), null);
    assert.equal(state.requests.length, count);
    assert.equal(await page.evaluate(() => window.actions), 0);
  });
});
