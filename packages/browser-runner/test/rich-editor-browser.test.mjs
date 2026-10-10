import assert from "node:assert/strict";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { build } from "esbuild";
import { chromium } from "playwright";
import { richPublicationToEditor } from "../src/rich-editor-bridge.mjs";
import { imageBytes, richEditorFixture } from "./fixtures/rich-editor.mjs";

test("real Tiptap editor HTML roundtrip preserves frozen structure and repeated media", async () => {
  const bundle = await build({
    stdin: {
      contents: `
        import { Editor } from "@tiptap/core";
        import { richPublicationExtensions, serializeRichEditorDocument } from "./src/rich-editor-schema.mjs";
        window.roundtrip = (content) => {
          const original = new Editor({ element: document.querySelector("#original"), extensions: richPublicationExtensions(), content });
          const html = serializeRichEditorDocument(content, document);
          const restored = new Editor({ element: document.querySelector("#restored"), extensions: richPublicationExtensions(), content: html, parseOptions: { preserveWhitespace: "full" } });
          return { html, original: original.getJSON(), restored: restored.getJSON(), secondHtml: restored.getHTML() };
        };
      `,
      resolveDir: fileURLToPath(new URL("..", import.meta.url)),
      loader: "js",
    },
    bundle: true,
    write: false,
    format: "iife",
    platform: "browser",
  });
  const server = createServer((request, response) => {
    if (request.url === "/bundle.js") {
      response.writeHead(200, { "content-type": "text/javascript" });
      response.end(bundle.outputFiles[0].contents);
    } else if (request.url === "/image.png") {
      response.writeHead(200, { "content-type": "image/png" });
      response.end(imageBytes);
    } else {
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      response.end(
        '<!doctype html><div id="original"></div><div id="restored"></div><script src="/bundle.js"></script>',
      );
    }
  });
  let browser;
  try {
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    browser = await chromium.launch(
      process.env.GEO_TEST_CHROMIUM_PATH
        ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
        : {},
    );
    const page = await browser.newPage();
    // Fixture must never fetch external assets or platforms.
    await page.route("**/*", (route) =>
      new URL(route.request().url()).origin === origin
        ? route.continue()
        : route.abort(),
    );
    await page.goto(origin);
    const { payload, mapping } = richEditorFixture(`${origin}/image.png`);
    const prepared = richPublicationToEditor(payload, mapping);
    const result = await page.evaluate(
      (content) => window.roundtrip(content),
      prepared.content,
    );
    assert.deepEqual(result.restored, result.original);
    assert.match(result.html, /<h3>/);
    assert.match(result.html, /<ol start="4">/);
    assert.match(result.html, /<ol start="8">/);
    assert.match(result.html, /<pre><code class="language-js">/);
    assert.match(result.html, /colspan="2"/);
    assert.match(result.html, /rowspan="2"/);
    assert.match(result.html, /title="Link &lt;title&gt;"/);
    assert.ok(!result.html.includes("<script>"));
    assert.equal(await page.locator("#restored figure").count(), 2);
    assert.deepEqual(
      await page.locator("#restored figcaption").allTextContents(),
      payload.media.map((item) => item.caption),
    );
    assert.deepEqual(
      await page
        .locator("#restored img")
        .evaluateAll((images) => images.map((image) => image.alt)),
      payload.media.map((item) => item.alt),
    );
    await page.waitForFunction(() =>
      [...document.querySelectorAll("#restored img")].every(
        (image) => image.complete && image.naturalWidth === 1,
      ),
    );
    assert.ok(!result.html.includes(mapping[0].object.object_id));
    assert.ok(!result.html.includes(mapping[0].binding_id));
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
