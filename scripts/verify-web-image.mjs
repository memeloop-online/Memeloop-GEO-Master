import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(
  new URL("../packages/browser-runner/package.json", import.meta.url),
);
const { chromium } = require("playwright");
const origin = process.env.GEO_WEB_IMAGE_URL ?? "http://127.0.0.1:18081";
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const response = await page.goto(`${origin}/login`);
  assert.equal(response.status(), 200);
  // Exercise the production module graph, not just Nginx or the HTML shell.
  // With no API configured the application must render its unavailable state.
  await page.waitForFunction(
    () => Boolean(document.querySelector("#root")?.textContent?.trim()),
    undefined,
    { timeout: 15_000 },
  );
  assert.deepEqual(errors, [], "Production frontend has uncaught errors");
  assert.ok((await page.locator("#root").innerText()).trim());
  console.log("Production web image renders without uncaught module errors.");
} finally {
  await browser.close();
}
