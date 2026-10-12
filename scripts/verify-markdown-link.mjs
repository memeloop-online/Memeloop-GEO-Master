// Synthetic, real-browser regression for the installed editor and popover.
// No API, account data, external requests or captured document text.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const requireBrowser = createRequire(
  join(root, "packages/browser-runner/package.json"),
);
const { build } = requireBrowser("esbuild");
const { chromium } = requireBrowser("playwright");
const result = await build({
  stdin: {
    resolveDir: join(root, "apps/web"),
    loader: "tsx",
    contents: `
      import React, {useState} from "react";
      import {createRoot} from "react-dom/client";
      import {FluentProvider,webLightTheme} from "@fluentui/react-components";
      import {MarkdownSourceEditor} from "./src/components/MarkdownSourceEditor";
      import {Plugin} from "@tiptap/pm/state";
      import i18n from "./src/i18n";
      await i18n.changeLanguage("zh-CN");
      window.installSyntheticAppend = editor => editor.registerPlugin(new Plugin({
        appendTransaction(transactions, oldState, newState) {
          if (transactions.some(tr => tr.getMeta("synthetic-append")))
            return newState.tr.insertText("Appended ", 1);
        }
      }));
      function App() {
        const [value,setValue] = useState("# Synthetic\\n\\n[示例链接](https://example.invalid/guide)\\n\\nOther text\\n\\n~~~\\ncode\\n~~~");
        const [updates,setUpdates] = useState(0);
        return <FluentProvider theme={webLightTheme}>
          <MarkdownSourceEditor value={value} disabled={false} onChange={next => {setValue(next);setUpdates(n=>n+1)}} />
          <output data-testid="updates">{updates}</output>
        </FluentProvider>;
      }
      createRoot(document.getElementById("root")).render(<App/>);
    `,
  },
  bundle: true,
  format: "esm",
  jsx: "automatic",
  write: false,
  loader: { ".css": "empty" },
  define: { "process.env.NODE_ENV": '"test"' },
});
const server = createServer((request, response) => {
  response.setHeader(
    "Content-Type",
    request.url === "/app.js" ? "text/javascript" : "text/html",
  );
  response.end(
    request.url === "/app.js"
      ? result.outputFiles[0].contents
      : '<!doctype html><html><body><div id="root"></div><script type="module" src="/app.js"></script></body></html>',
  );
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
let browser;
try {
  browser = await chromium.launch({
    headless: true,
    ...(process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : process.platform === "win32"
        ? { channel: "chrome" }
        : {}),
  });
  const page = await browser.newPage();
  for (const scenario of [
    "ordinary",
    "drift",
    "mapped",
    "appended",
    "code-selection",
    "unlink",
  ]) {
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    const editor = page.locator(
      "[data-testid='knowledge-visual-editor'] [contenteditable='true']",
    );
    const toolbar = page.getByRole("toolbar", { name: "资料编辑" });
    await editor.locator("a").waitFor();
    await editor.locator("a").evaluate((anchor) => {
      anchor.closest("[contenteditable]")?.focus();
      const range = document.createRange();
      range.selectNodeContents(anchor);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
    });
    await page.waitForFunction(
      () =>
        document
          .querySelector("button[aria-label='添加链接']")
          ?.getAttribute("aria-pressed") === "true",
    );
    await toolbar.getByRole("button", { name: "添加链接" }).click();
    const popover = page.locator(".source-link-popover");
    const input = popover.getByRole("textbox", { name: "链接地址" });
    assert.equal(await input.inputValue(), "https://example.invalid/guide");
    await input.fill("javascript:alert(1)");
    await popover
      .getByRole("button", { name: "添加链接", exact: true })
      .click();
    await popover.getByRole("alert").waitFor();
    assert.equal(
      await page.getByTestId("updates").textContent(),
      "0",
      "Invalid links must not change the document",
    );
    // Drain focus()'s animation-frame callback rather than hiding it with a sleep.
    await page.evaluate(
      () =>
        new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        ),
    );
    assert.equal(
      await popover.evaluate((element) =>
        element.contains(document.activeElement),
      ),
      true,
      "Rejecting an invalid URI must not asynchronously move focus out of its popover",
    );
    await input.fill("/updated");
    if (scenario !== "ordinary") {
      await editor.evaluate((element, current) => {
        // This is the installed Tiptap instance attached to the synthetic fixture.
        const instance = element.editor;
        if (current === "mapped")
          instance.commands.insertContentAt(1, "Prefix ");
        if (current === "appended") {
          window.installSyntheticAppend(instance);
          instance.view.dispatch(
            instance.state.tr.setMeta("synthetic-append", true),
          );
        }
        let position = instance.state.doc.content.size - 1;
        if (current !== "code-selection") {
          instance.state.doc.descendants((node, offset) => {
            if (node.isText && node.text === "Other text") position = offset;
          });
        }
        instance.commands.setTextSelection(position);
      }, scenario);
    }
    await popover
      .getByRole("button", {
        name: scenario === "unlink" ? "移除链接" : "添加链接",
        exact: true,
      })
      .click();
    await page.keyboard.press("Escape");
    const diagnostic = await editor.evaluate((element) => {
      const link = element.querySelector("a");
      const selection = getSelection();
      return {
        expectedHref: link?.getAttribute("href") === "/updated",
        originalHref:
          link?.getAttribute("href") === "https://example.invalid/guide",
        selectedLength: selection?.toString().length,
        selectionCollapsed: selection?.isCollapsed,
        anchorWithinLink: link?.contains(selection?.anchorNode),
        focusWithinLink: link?.contains(selection?.focusNode),
        anchorOffset: selection?.anchorOffset,
        focusOffset: selection?.focusOffset,
        editorFocused: element.contains(document.activeElement),
      };
    });
    if (scenario === "unlink")
      assert.equal(
        await editor.locator("a").count(),
        0,
        "Unlink must use the saved selection",
      );
    else
      assert.equal(
        diagnostic.expectedHref,
        true,
        `${scenario}: ${JSON.stringify(diagnostic)}`,
      );
    console.log(`Synthetic link regression passed: ${scenario}.`);
  }
} finally {
  await browser?.close();
  await new Promise((resolve) => server.close(resolve));
}
