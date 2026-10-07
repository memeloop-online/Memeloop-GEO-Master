// Real local-memory API + real Vite + installed Chromium. No route interception,
// external provider requests, account connections, or reusable credentials.
// Set GEO_SMOKE_APP_BINARY and GEO_SMOKE_OUTPUT_DIR (outside the repository).
// Optional: PLAYWRIGHT_BROWSERS_PATH, GEO_SMOKE_TMP_DIR,
// GEO_SMOKE_API_PORT, GEO_SMOKE_WEB_PORT. GEO_SMOKE_CONTENT=1 additionally
// requires approved local bundles, an unused GEO_SMOKE_PROVIDER_PORT, and
// Python 3 (override its executable with GEO_SMOKE_PYTHON) for ZIP inspection.
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { deflateSync } from "node:zlib";
import { execFile, spawn } from "node:child_process";
import { createRequire } from "node:module";
import { createServer as createHttpServer } from "node:http";
import { createServer } from "node:net";
import { mkdir, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const webRoot = join(repository, "apps", "web");
function configuredPort(name, fallback) {
  const value = process.env[name] ?? String(fallback);
  const port = Number(value);
  if (
    !/^\d+$/.test(value) ||
    !Number.isInteger(port) ||
    port < 1 ||
    port > 65535
  )
    throw new Error(`Invalid ${name}`);
  return port;
}
const apiPort = configuredPort("GEO_SMOKE_API_PORT", 8080);
const webPort = configuredPort("GEO_SMOKE_WEB_PORT", 5173);
if (apiPort === webPort) throw new Error("Smoke API and web ports must differ");
const contentMode = process.env.GEO_SMOKE_CONTENT === "1";
if (
  process.env.GEO_SMOKE_CONTENT &&
  !["0", "1"].includes(process.env.GEO_SMOKE_CONTENT)
)
  throw new Error("GEO_SMOKE_CONTENT must be 0 or 1");
const providerPort = contentMode
  ? configuredPort("GEO_SMOKE_PROVIDER_PORT", 18081)
  : null;
if (contentMode && [apiPort, webPort].includes(providerPort))
  throw new Error("Smoke provider port must differ from API and web ports");
const apiUrl = `http://127.0.0.1:${apiPort}`;
const webUrl = `http://127.0.0.1:${webPort}`;
const browserCache = process.env.PLAYWRIGHT_BROWSERS_PATH;
const scratch = resolve(process.env.GEO_SMOKE_TMP_DIR ?? tmpdir());
const binary = process.env.GEO_SMOKE_APP_BINARY;
const outputRoot = process.env.GEO_SMOKE_OUTPUT_DIR;
const requireBrowser = createRequire(
  join(repository, "packages", "browser-runner", "package.json"),
);
const requireWeb = createRequire(join(webRoot, "package.json"));
const runZipReader = promisify(execFile);
const processes = [];
const visualIssues = [];
let browser;
let browserServer;
let providerServer;
let deadline;
let screenshotsDirectory;
let ephemeralPassword;
let ephemeralProviderKey;

const evidenceSentence = "Synthetic Acme sample widget has a blue cover.";
const evidenceTitle = "Synthetic Acme sample widget";
const uuidPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function syntheticPng() {
  const crcTable = Array.from({ length: 256 }, (_, index) => {
    let value = index;
    for (let bit = 0; bit < 8; bit++)
      value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    return value >>> 0;
  });
  const chunk = (type, bytes) => {
    const label = Buffer.from(type, "ascii");
    const length = Buffer.alloc(4);
    length.writeUInt32BE(bytes.length);
    let crc = 0xffffffff;
    for (const byte of Buffer.concat([label, bytes]))
      crc = crcTable[(crc ^ byte) & 255] ^ (crc >>> 8);
    const checksum = Buffer.alloc(4);
    checksum.writeUInt32BE((crc ^ 0xffffffff) >>> 0);
    return Buffer.concat([length, label, bytes, checksum]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(2, 0);
  header.writeUInt32BE(2, 4);
  header[8] = 8; // RGB, eight bits per channel, non-interlaced.
  header[9] = 2;
  const pixels = Buffer.from([
    0, 255, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 0,
  ]);
  return Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(pixels)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

async function readZipEntries(path) {
  // Python's maintained standard-library ZIP reader validates the directory
  // and CRCs. Inspect the browser download in place without extracting paths.
  const code = [
    "import base64,json,sys,zipfile",
    "with zipfile.ZipFile(sys.argv[1]) as archive:",
    " names=archive.namelist()",
    " if len(names)!=2 or any(archive.getinfo(name).file_size>1000000 for name in names): raise ValueError('unexpected archive entries')",
    " print(json.dumps({name:base64.b64encode(archive.read(name)).decode('ascii') for name in names}))",
  ].join("\n");
  const python =
    process.env.GEO_SMOKE_PYTHON ??
    (process.platform === "win32" ? "python" : "python3");
  const { stdout } = await runZipReader(python, ["-c", code, path], {
    env: safeEnvironment(),
    maxBuffer: 3_000_000,
  });
  return Object.fromEntries(
    Object.entries(JSON.parse(stdout)).map(([name, value]) => [
      name,
      Buffer.from(value, "base64"),
    ]),
  );
}

async function approvedBundle(name) {
  const path = join(repository, "packages", "agent-runtime", "dist", name);
  const data = await readFile(path);
  assert(data.length > 0, `Approved bundle ${name} is empty`);
  return { path, digest: createHash("sha256").update(data).digest("hex") };
}

function fixtureCompletion(body, model) {
  assert(
    body?.model === model && Array.isArray(body.messages),
    "Fixture received an unsupported model request",
  );
  const system = body.messages.find(
    (entry) => entry.role === "system",
  )?.content;
  const user = body.messages.findLast(
    (entry) => entry.role === "user",
  )?.content;
  assert(
    typeof system === "string" && typeof user === "string",
    "Fixture received an incomplete model request",
  );
  const input = JSON.parse(user);
  assert(
    Array.isArray(input.evidence),
    "Fixture requires actual supplied source evidence",
  );
  const quotes = input.evidence.filter(
    (entry) =>
      uuidPattern.test(entry.chunk_id) && typeof entry.quote === "string",
  );
  let content;
  if (system.includes("source-grounded content generator")) {
    const source = quotes.find((entry) =>
      entry.quote.includes(evidenceSentence),
    );
    assert(
      source,
      "Generated content must cite the imported public source quote",
    );
    content = {
      title: evidenceTitle,
      blocks: [
        {
          kind: "heading",
          text: evidenceTitle,
          items: [],
          citation_ids: [source.chunk_id],
        },
        {
          kind: "paragraph",
          text: evidenceSentence,
          items: [],
          citation_ids: [source.chunk_id],
        },
      ],
    };
  } else if (system.includes("independent factual checker")) {
    const document = input.document;
    assert(
      uuidPattern.test(input.title_check_id) && Array.isArray(document?.blocks),
      "Checker needs the persisted title and blocks",
    );
    const claims = [
      {
        block_id: input.title_check_id,
        text: document.title,
        citation_ids: quotes.map((entry) => entry.chunk_id),
      },
      ...document.blocks.map((block) => ({
        block_id: block.block_id,
        text: block.text,
        citation_ids: block.citation_ids,
      })),
    ];
    assert(
      claims.length >= 3 &&
        claims.every((claim) => uuidPattern.test(claim.block_id)),
      "Checker must receive title and every block",
    );
    content = {
      checks: claims.map((claim) => {
        const exact = quotes.find(
          (entry) =>
            claim.citation_ids.includes(entry.chunk_id) &&
            entry.quote.includes(claim.text),
        );
        return {
          block_id: claim.block_id,
          verdict: exact ? "supported" : "unsupported",
          citation_ids: exact ? [exact.chunk_id] : [],
          detail: exact
            ? "Claim occurs in the supplied exact source quote"
            : "Claim absent from the supplied exact source quotes",
        };
      }),
    };
    assert(
      content.checks.every((entry) => entry.verdict === "supported"),
      "Fixture refuses claims absent from supplied evidence",
    );
  } else {
    throw new Error("Fixture refuses non-content model requests");
  }
  return {
    id: randomUUID(),
    model,
    choices: [
      {
        message: { role: "assistant", content: JSON.stringify(content) },
        finish_reason: "stop",
      },
    ],
    usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
  };
}

async function startFixtureProvider(port, key) {
  const model = "synthetic-content-smoke";
  const server = createHttpServer(async (request, response) => {
    try {
      assert(
        request.method === "POST" && request.url === "/v1/chat/completions",
        "Fixture accepts only local completions",
      );
      assert(
        request.headers.authorization === `Bearer ${key}`,
        "Fixture rejected authentication",
      );
      const chunks = [];
      let size = 0;
      for await (const chunk of request) {
        size += chunk.length;
        assert(size <= 1_000_000, "Fixture request exceeded limit");
        chunks.push(chunk);
      }
      const result = fixtureCompletion(
        JSON.parse(Buffer.concat(chunks).toString("utf8")),
        model,
      );
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify(result));
    } catch {
      // Never print incoming prompts, credentials, or response bodies.
      response.writeHead(400, { "Content-Type": "application/json" });
      response.end(
        '{"error":{"message":"unsupported synthetic fixture request"}}',
      );
    }
  });
  await new Promise((accept, reject) => {
    server.once("error", reject);
    server.listen({ host: "127.0.0.1", port, exclusive: true }, accept);
  });
  providerServer = server;
  return {
    GEO_AI_BASE_URL: `http://127.0.0.1:${port}/v1/`,
    GEO_AI_API_KEY: key,
    GEO_AI_MODEL: model,
  };
}

async function publicApi(page, path, options = {}) {
  return page.evaluate(
    async ({ path, options }) => {
      const { apiFetch } = await import("/src/api/client.ts");
      return apiFetch(path, options);
    },
    { path, options },
  );
}

async function verifyGeneratedRichContent(
  page,
  base,
  tenantId,
  projectId,
  runDir,
) {
  await page.setViewportSize({ width: 1440, height: 900 });
  // Public import/plan APIs create real source evidence and a frozen input;
  // neither generated assets nor revision/check state are seeded directly.
  const imported = await publicApi(page, "/knowledge/imports", {
    method: "POST",
    tenantId,
    projectId,
    body: {
      items: [
        {
          client_item_id: randomUUID(),
          kind: "text",
          name: "Synthetic public content evidence",
          purpose: "public",
          text: evidenceSentence,
        },
      ],
    },
  });
  const acceptance = imported.items?.[0];
  assert(
    acceptance?.status === "succeeded" &&
      acceptance.release?.knowledge_release_id,
    "Synthetic public source was not imported and released",
  );
  const cycle = await publicApi(page, `/projects/${projectId}/cycles/current`, {
    tenantId,
    projectId,
  });
  const manifestId = cycle?.document_manifest?.manifest_id;
  assert(
    uuidPattern.test(cycle?.cycle_id) && uuidPattern.test(manifestId),
    "Started project lacks a current cycle and document manifest handle",
  );
  const plan = await publicApi(page, "/knowledge/document-manifests/plan", {
    method: "POST",
    tenantId,
    projectId,
    body: {
      manifest_id: manifestId,
      knowledge_release_id: acceptance.release.knowledge_release_id,
    },
  });
  assert(
    plan?.sealed &&
      plan.items?.some(
        (item) =>
          item.state === "planned" &&
          item.source_version_refs.includes(
            acceptance.source_version.source_version_id,
          ),
      ),
    "Public source did not enter the sealed generation manifest",
  );

  await page.goto(`${base}/content`);
  await page.getByRole("heading", { name: "本轮内容", exact: true }).waitFor();
  const started = page.waitForResponse(
    (response) =>
      /\/document-executions$/.test(new URL(response.url()).pathname) &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "启动正文生成" }).click();
  const startResponse = await started;
  assert(
    startResponse.status() === 202,
    "Rust content workflow did not accept generation",
  );
  const execution = await startResponse.json();
  assert(
    uuidPattern.test(execution.execution_id),
    "Generation returned no execution identity",
  );
  const itemsPath = `/projects/${projectId}/document-executions/${execution.execution_id}/items`;
  let generated;
  const end = Date.now() + 80_000;
  while (Date.now() < end) {
    const items = await publicApi(page, itemsPath, { tenantId, projectId });
    generated = items.find(
      (item) => item.status === "ready" && uuidPattern.test(item.asset_id),
    );
    if (generated) break;
    assert(
      !items.every((item) =>
        ["blocked", "deferred", "not_applicable", "cancelled"].includes(
          item.status,
        ),
      ),
      "No generated content branch reached ready",
    );
    await page.waitForTimeout(650);
  }
  assert(
    generated,
    "Source-backed Rust generation and independent checks did not complete",
  );
  const assetId = generated.asset_id;
  await page.goto(`${base}/content/${assetId}`);
  const input = page.getByRole("textbox", { name: "结构化正文" });
  await input.waitFor();
  const revisionsPath = `/projects/${projectId}/contents/${assetId}/revisions`;
  const original = (
    await publicApi(page, revisionsPath, { tenantId, projectId })
  ).find((revision) => revision.revision === 1);
  assert(
    original?.document?.title === evidenceTitle &&
      original.document.blocks.length === 2 &&
      original.document.blocks.every(
        (block) =>
          block.citation_ids.length && uuidPattern.test(block.block_id),
      ) &&
      original.findings?.length === 3,
    "Persisted model generation lacks exact citations or independent title/block checks",
  );
  console.log(
    "Content: source-backed generation and title/block checks persisted",
  );
  const contentParagraph = input
    .locator("p")
    .filter({ hasText: evidenceSentence });
  await contentParagraph.click({ clickCount: 3 });
  let selectedText = await page.evaluate(() => getSelection()?.toString());
  if (selectedText?.trim() !== evidenceSentence) {
    // Browser triple-click selection depends on line wrapping. Select exactly
    // the visible paragraph via the DOM Selection API; formatting still runs
    // through the installed Tiptap toolbar and its real persistence path.
    await contentParagraph.evaluate((paragraph) => {
      const selection = document.getSelection();
      const range = document.createRange();
      range.selectNodeContents(paragraph);
      selection?.removeAllRanges();
      selection?.addRange(range);
    });
    selectedText = await page.evaluate(() => getSelection()?.toString());
  }
  assert(
    selectedText?.trim() === evidenceSentence,
    "Could not select source-grounded paragraph",
  );
  const toolbar = page.getByRole("toolbar", { name: "正文格式" });
  const boldSaved = page.waitForResponse(
    (response) =>
      /\/contents\/[^/]+\/revisions$/.test(new URL(response.url()).pathname) &&
      response.request().method() === "POST" &&
      response
        .request()
        .postDataJSON()
        .document?.blocks?.some((block) =>
          JSON.stringify(block.rich?.node ?? null).includes('"type":"bold"'),
        ),
    { timeout: 20_000 },
  );
  await toolbar.getByRole("button", { name: "加粗" }).click();
  assert(
    (await input
      .locator("strong")
      .filter({ hasText: evidenceSentence })
      .count()) > 0,
    "Tiptap bold mark was not applied",
  );
  console.log("Content: Tiptap bold mark visible; awaiting autosave");
  assert((await boldSaved).status() === 201, "Rich mark autosave failed");
  console.log("Content: rich mark autosaved");
  await page.reload();
  await input.locator("strong").waitFor();
  const listSaved = page.waitForResponse(
    (response) =>
      /\/contents\/[^/]+\/revisions$/.test(new URL(response.url()).pathname) &&
      response.request().method() === "POST" &&
      response
        .request()
        .postDataJSON()
        .document?.blocks?.some(
          (block) => block.rich?.node?.type === "bulletList",
        ),
    { timeout: 20_000 },
  );
  await input.locator("p").filter({ hasText: evidenceSentence }).click();
  await toolbar.getByRole("button", { name: "列表", exact: true }).click();
  assert(
    (await input.locator("ul li").count()) > 0,
    "Tiptap list was not created",
  );
  assert((await listSaved).status() === 201, "Rich list autosave failed");
  console.log("Content: rich list autosaved");
  await page.reload();
  await input.locator("ul li").waitFor();
  const tableInserted = page.waitForResponse(
    (response) =>
      /\/contents\/[^/]+\/revisions$/.test(new URL(response.url()).pathname) &&
      response.request().method() === "POST" &&
      response
        .request()
        .postDataJSON()
        .document?.blocks?.some((block) => block.rich?.node?.type === "table"),
    { timeout: 20_000 },
  );
  await input.locator("h2").first().click();
  await page.keyboard.press("End");
  await toolbar.getByRole("button", { name: "插入表格" }).click();
  await input.locator("table").waitFor();
  assert(
    (await input.getAttribute("contenteditable")) === "true",
    "Table editor was explicitly disabled",
  );
  assert(
    (await page.getByText(/本地编辑仍保留/).count()) === 0,
    "Tiptap table insertion triggered an unsupported local draft",
  );
  assert(
    (await tableInserted).status() === 201,
    "Tiptap table insertion did not autosave",
  );
  await page.reload();
  await input.locator("table th").first().waitFor();
  const autosaved = page.waitForResponse(
    (response) => {
      if (
        !/\/contents\/[^/]+\/revisions$/.test(
          new URL(response.url()).pathname,
        ) ||
        response.request().method() !== "POST"
      )
        return false;
      try {
        const payload = response.request().postDataJSON();
        return payload.document?.blocks?.some(
          (block) =>
            block.rich?.node?.type === "table" &&
            JSON.stringify(block.rich.node).includes(evidenceTitle),
        );
      } catch {
        return false;
      }
    },
    { timeout: 20_000 },
  );
  await input.locator("table th").first().click();
  await page.keyboard.insertText(evidenceTitle);
  assert(
    (await input.locator("table th").first().textContent()).includes(
      evidenceTitle,
    ),
    "Accepted table cell input was immediately lost",
  );
  console.log("Content: Tiptap table text inserted; awaiting autosave");
  const save = await autosaved;
  assert(
    save.status() === 201,
    "Rich Tiptap autosave did not persist a new revision",
  );
  let saved = await save.json();
  assert(
    saved.revision_id !== original.revision_id &&
      saved.document.schema_version === 2 &&
      saved.document.blocks.some(
        (block) =>
          block.block_id === original.document.blocks[1].block_id &&
          JSON.stringify(block.citation_ids) ===
            JSON.stringify(original.document.blocks[1].citation_ids),
      ) &&
      saved.document.blocks.some(
        (block) => block.rich?.node?.type === "bulletList",
      ) &&
      saved.document.blocks.some(
        (block) => block.rich?.node?.type === "table",
      ) &&
      saved.document.blocks.some((block) =>
        JSON.stringify(block.rich?.node ?? null).includes('"type":"bold"'),
      ),
    "Autosaved rich document lost marks, list, table, or schema version",
  );
  await page.reload();
  await input.locator("table").waitFor();
  assert(
    (await input.locator("ul li").count()) > 0 &&
      (await input.locator("strong").count()) > 0,
    "Rich structure did not survive page reload",
  );
  const secondTableSaved = page.waitForResponse(
    (response) =>
      /\/contents\/[^/]+\/revisions$/.test(new URL(response.url()).pathname) &&
      response.request().method() === "POST" &&
      response
        .request()
        .postDataJSON()
        .document?.blocks?.filter((block) => block.rich?.node?.type === "table")
        .length >= 2,
    { timeout: 20_000 },
  );
  await input.locator("h2").first().click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await toolbar.getByRole("button", { name: "插入表格" }).click();
  assert(
    (await input.locator("table").count()) === 2,
    "Table insertion after a new paragraph lost structure",
  );
  assert(
    (await page.getByText(/本地编辑仍保留/).count()) === 0,
    "Heading-to-table editing produced an unsupported draft",
  );
  const secondTableResponse = await secondTableSaved;
  assert(
    secondTableResponse.status() === 201,
    "Heading-to-table autosave failed",
  );
  saved = await secondTableResponse.json();
  await page.reload();
  await input.locator("table").first().waitFor();
  assert(
    (await input.locator("table").count()) === 2 &&
      (await input.locator("table").allTextContents()).some((value) =>
        value.includes(evidenceTitle),
      ),
    "Table insertion after an empty paragraph changed previously saved rich text",
  );
  const persisted = await publicApi(page, revisionsPath, {
    tenantId,
    projectId,
  });
  assert(
    persisted.some(
      (entry) =>
        entry.revision_id === saved.revision_id &&
        JSON.stringify(entry.document) === JSON.stringify(saved.document),
    ),
    "Reloaded revision changed block IDs or citation bindings",
  );
  const unchanged = persisted.find(
    (entry) => entry.revision_id === original.revision_id,
  );
  assert(
    JSON.stringify(unchanged?.document) === JSON.stringify(original.document) &&
      JSON.stringify(unchanged?.findings) === JSON.stringify(original.findings),
    "Editing mutated original immutable revision or checks",
  );
  await screenshot(page, runDir, "content-rich-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "content-rich-narrow", {
    width: 390,
    height: 844,
  });
  await toolbar.getByRole("button", { name: "加粗" }).focus();
  await page.keyboard.press("ArrowRight");
  assert(
    await toolbar
      .getByRole("button", { name: "斜体" })
      .evaluate((element) => element === document.activeElement),
    "Rich formatting toolbar is not keyboard navigable",
  );
  const history = page.locator("details.content-history");
  await history
    .locator("summary")
    .getByText("版本历史", { exact: true })
    .click();
  assert(
    await history.getByRole("button", { name: "下载 Markdown" }).isVisible(),
    "Immutable export controls did not open with version history",
  );
  const verifyDownload = async (revision, format) => {
    const path = `/projects/${projectId}/contents/${assetId}/revisions/${revision.revision_id}/export?format=${format}`;
    const expected = await publicApi(page, path, { tenantId, projectId });
    assert(
      expected.revision_id === revision.revision_id &&
        expected.format === format &&
        expected.content.includes(evidenceTitle),
      "Selected immutable export endpoint returned unexpected revision or content",
    );
    const responsePromise = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname.endsWith(
          `/revisions/${revision.revision_id}/export`,
        ) && new URL(response.url()).searchParams.get("format") === format,
    );
    const downloadPromise = page.waitForEvent("download");
    await history
      .getByRole("button", {
        name: format === "html" ? "下载 HTML" : "下载 Markdown",
      })
      .click();
    const [response, download] = await Promise.all([
      responsePromise,
      downloadPromise,
    ]);
    assert(
      response.status() === 200 &&
        JSON.stringify(await response.json()) === JSON.stringify(expected) &&
        download.suggestedFilename() === expected.filename,
      "Browser downloaded a different revision or format",
    );
    const stream = await download.createReadStream();
    let bytes = "";
    for await (const chunk of stream) bytes += chunk.toString("utf8");
    assert(
      bytes === expected.content,
      "Downloaded bytes differ from exact selected immutable revision",
    );
  };
  await verifyDownload(saved, "markdown");
  await verifyDownload(saved, "html");
  await history.getByRole("button", { name: /^v1 ·/ }).click();
  await page.getByRole("heading", { name: "历史版本 v1" }).waitFor();
  await verifyDownload(original, "markdown");
  await verifyDownload(original, "html");
  await history
    .getByRole("button", { name: new RegExp(`^v${saved.revision} ·`) })
    .click();
  await input.waitFor();
  assert(
    (await input.getAttribute("contenteditable")) === "true",
    "Returning to the current text revision did not restore its editor",
  );
  const png = syntheticPng();
  const digest = createHash("sha256").update(png).digest("hex");
  const alt = "Synthetic two-by-two color sample";
  const caption = "Synthetic image for local verification";
  const picker = page.getByRole("group", { name: "插入图片" });
  await page.getByRole("button", { name: "插入图片" }).click();
  await picker.locator('input[type="file"]').setInputFiles({
    name: "synthetic-colors.png",
    mimeType: "image/png",
    buffer: png,
  });
  await picker.getByRole("textbox", { name: "图片说明（无障碍）" }).fill(alt);
  await picker.getByRole("textbox", { name: "图片标题（可选）" }).fill(caption);
  const completed = page.waitForResponse(
    (response) =>
      /\/agent\/attachments\/upload-sessions\/[^/]+\/complete$/.test(
        new URL(response.url()).pathname,
      ) && response.request().method() === "POST",
  );
  const bound = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname.endsWith(
        `/projects/${projectId}/content-media/bindings`,
      ) && response.request().method() === "POST",
  );
  const preview = page.waitForResponse(
    (response) =>
      /\/content-media\/bindings\/[^/]+\/bytes$/.test(
        new URL(response.url()).pathname,
      ) && response.request().method() === "GET",
  );
  const mediaSaved = page.waitForResponse(
    (response) => {
      if (
        !/\/contents\/[^/]+\/revisions$/.test(
          new URL(response.url()).pathname,
        ) ||
        response.request().method() !== "POST"
      )
        return false;
      try {
        return response
          .request()
          .postDataJSON()
          .document?.blocks?.some(
            (block) => block.rich?.node?.type === "media",
          );
      } catch {
        return false;
      }
    },
    { timeout: 20_000 },
  );
  await picker.getByRole("button", { name: "上传并插入" }).click();
  const uploadResponse = await completed;
  assert(uploadResponse.status() === 200, "Image upload did not complete");
  const attachment = await uploadResponse.json();
  assert(
    uuidPattern.test(attachment.object_id) &&
      /^\d+$/.test(attachment.object_version) &&
      Number.isSafeInteger(Number(attachment.object_version)) &&
      Number(attachment.object_version) > 0 &&
      attachment.sha256 === digest,
    "Committed image bytes do not match the upload receipt",
  );
  const bindingResponse = await bound;
  assert(bindingResponse.status() === 201, "Image binding was not created");
  const binding = await bindingResponse.json();
  const key = {
    object_id: attachment.object_id,
    object_version: Number(attachment.object_version),
    sha256: digest,
  };
  assert(
    uuidPattern.test(binding.binding_id) &&
      binding.state === "active" &&
      JSON.stringify(binding.image?.key) === JSON.stringify(key) &&
      binding.image.media_type === "image/png" &&
      binding.image.byte_len === png.length &&
      binding.image.width === 2 &&
      binding.image.height === 2,
    "Bound image identity, detected format, bytes, or dimensions differ from the upload",
  );
  const mediaResponse = await mediaSaved;
  assert(mediaResponse.status() === 201, "Media insertion did not autosave");
  const mediaRevision = await mediaResponse.json();
  const mediaBlock = mediaRevision.document?.blocks?.find(
    (block) => block.rich?.node?.type === "media",
  );
  assert(
    mediaRevision.revision_id !== saved.revision_id &&
      mediaBlock &&
      uuidPattern.test(mediaBlock.block_id) &&
      !saved.document.blocks.some(
        (block) => block.block_id === mediaBlock.block_id,
      ) &&
      mediaBlock.citation_ids.length === 0 &&
      JSON.stringify(mediaBlock.rich.node.attrs) ===
        JSON.stringify({ ...key, alt, caption }) &&
      saved.document.blocks.every((block) =>
        mediaRevision.document.blocks.some(
          (candidate) =>
            candidate.block_id === block.block_id &&
            JSON.stringify(candidate.citation_ids) ===
              JSON.stringify(block.citation_ids),
        ),
      ),
    "Media autosave changed existing citations/IDs or stored the wrong image reference",
  );
  await page
    .getByText(new RegExp(`正在编辑 v${mediaRevision.revision}[。.]`))
    .waitFor();
  assert(
    (await input.getAttribute("contenteditable")) === "true" &&
      (await page
        .getByRole("heading", {
          name: `历史版本 v${saved.revision}`,
        })
        .count()) === 0,
    "Media autosave unmounted the live current editor after history selection",
  );
  const previewResponse = await preview;
  assert(
    previewResponse.status() === 200 &&
      new URL(previewResponse.url()).pathname.endsWith(
        `/content-media/bindings/${binding.binding_id}/bytes`,
      ),
    "Authenticated image preview did not return the bound image",
  );
  assert(
    previewResponse.headers()["content-type"] === "image/png" &&
      previewResponse.headers()["x-content-type-options"] === "nosniff" &&
      previewResponse.headers()["cache-control"] === "no-store",
    "Authenticated image preview lacks its detected type or effective no-cache headers",
  );
  const previewBytes = await page.evaluate(
    async ({ tenantId, projectId, bindingId }) => {
      const { readContentMediaBytes } =
        await import("/src/api/contentMedia.ts");
      const blob = await readContentMediaBytes(tenantId, projectId, bindingId);
      return [...new Uint8Array(await blob.arrayBuffer())];
    },
    { tenantId, projectId, bindingId: binding.binding_id },
  );
  assert(
    previewBytes.length === png.length &&
      previewBytes.every((byte, index) => png[index] === byte),
    "Authenticated browser image read differed from committed PNG bytes",
  );
  const mediaImage = input.locator(".content-media-node img");
  await mediaImage.waitFor();
  await mediaImage.evaluate((image) => image.decode());
  await page.reload();
  await mediaImage.waitFor();
  await mediaImage.evaluate((image) => image.decode());
  assert(
    (await mediaImage.getAttribute("alt")) === alt &&
      (
        await mediaImage.evaluate((image) => [
          image.naturalWidth,
          image.naturalHeight,
        ])
      ).join("x") === "2x2" &&
      (await input.locator("figcaption").textContent()) === caption,
    "Editor image did not render the authenticated two-by-two PNG",
  );
  const thumbnailResponse = page.waitForResponse(
    (response) =>
      /\/content-media\/bindings\/[^/]+\/thumbnail$/.test(
        new URL(response.url()).pathname,
      ) && response.request().method() === "GET",
  );
  await page.getByRole("button", { name: "插入图片" }).click();
  const existingImage = picker.getByRole("button", {
    name: /image\/png.*2.*2/,
  });
  await existingImage.scrollIntoViewIfNeeded();
  await picker
    .locator(".content-media-thumbnail img")
    .evaluate((image) => image.decode());
  const thumbnail = await thumbnailResponse;
  assert(
    thumbnail.ok() &&
      thumbnail.headers()["content-type"] === "image/png" &&
      thumbnail.headers()["cache-control"] === "no-store",
    "Media picker did not use the authenticated PNG thumbnail endpoint",
  );
  assert(
    (
      await picker
        .locator(".content-media-thumbnail img")
        .evaluate((image) => [image.naturalWidth, image.naturalHeight])
    ).join("x") === "2x2",
    "Media thumbnail unexpectedly upscaled the source image",
  );
  await screenshot(page, runDir, "content-media-picker-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "content-media-picker-narrow", {
    width: 390,
    height: 844,
  });
  await page.getByRole("button", { name: "插入图片" }).click();
  const mediaPersisted = (
    await publicApi(page, revisionsPath, { tenantId, projectId })
  ).find((revision) => revision.revision_id === mediaRevision.revision_id);
  assert(
    JSON.stringify(mediaPersisted?.document) ===
      JSON.stringify(mediaRevision.document) &&
      (await mediaImage.getAttribute("alt")) === alt,
    "Image revision changed during reload or its preview disappeared",
  );
  const titleSaved = page.waitForResponse(
    (response) =>
      /\/contents\/[^/]+\/revisions$/.test(new URL(response.url()).pathname) &&
      response.request().method() === "POST" &&
      response.request().postDataJSON().document?.title ===
        `${evidenceTitle} (with image)`,
    { timeout: 20_000 },
  );
  await page
    .getByRole("textbox", { name: "标题", exact: true })
    .fill(`${evidenceTitle} (with image)`);
  assert((await titleSaved).status() === 201, "Image title edit did not save");
  await page.reload();
  await mediaImage.waitFor();
  await history
    .locator("summary")
    .getByText("版本历史", { exact: true })
    .click();
  await history
    .getByRole("button", { name: new RegExp(`^v${mediaRevision.revision} ·`) })
    .click();
  await page
    .getByRole("heading", {
      name: `历史版本 v${mediaRevision.revision}`,
    })
    .waitFor();
  const historicalImage = page
    .locator(".panel-card")
    .filter({
      has: page.getByRole("heading", {
        name: `历史版本 v${mediaRevision.revision}`,
      }),
    })
    .locator(".content-media-node img");
  await historicalImage.waitFor();
  await historicalImage.evaluate((image) => image.decode());
  assert(
    (await historicalImage.getAttribute("alt")) === alt &&
      (
        await historicalImage.evaluate((image) => [
          image.naturalWidth,
          image.naturalHeight,
        ])
      ).join("x") === "2x2" &&
      (await page.locator(".panel-card [contenteditable='true']").count()) ===
        0 &&
      (await page.getByRole("button", { name: "保存新版本" }).count()) === 0 &&
      (await history
        .getByRole("button", { name: "下载 Markdown", exact: true })
        .count()) === 0 &&
      (await history
        .getByRole("button", { name: "下载 HTML", exact: true })
        .count()) === 0,
    "Historical media revision was editable, lost preview, or offered incomplete plain exports",
  );
  const verifyMediaBundle = async (format) => {
    const button = history.getByRole("button", {
      name:
        format === "html"
          ? "下载 HTML 与图片（ZIP）"
          : "下载 Markdown 与图片（ZIP）",
    });
    assert(await button.isVisible(), "Media version lacks a ZIP export action");
    const bundlePath = `/revisions/${mediaRevision.revision_id}/export-bundle`;
    const responsePromise = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname.endsWith(bundlePath) &&
        new URL(response.url()).searchParams.get("format") === format,
    );
    const downloadPromise = page
      .waitForEvent("download")
      .catch((error) => error);
    await button.click();
    const response = await responsePromise;
    if (response.status() !== 200) {
      const failure = await response.json();
      throw new Error(
        `Media bundle API returned ${response.status()} / ${failure.error?.code ?? "unknown"}: ${failure.error?.message ?? "no reason"}`,
      );
    }
    assert(
      response.headers()["content-type"] === "application/zip",
      "Media bundle API did not return a ZIP content type",
    );
    const download = await downloadPromise;
    assert(
      !(download instanceof Error) &&
        download.suggestedFilename() ===
          `${mediaRevision.revision_id}-${format}.zip`,
      "Browser did not download the selected immutable media ZIP",
    );
    // Chromium is connected through a BrowserServer, so its remote download
    // path cannot be read directly by this Node process.
    const archivePath = join(
      runDir,
      `content-media-${mediaRevision.revision_id}-${format}.zip`,
    );
    await download.saveAs(archivePath);
    const entries = await readZipEntries(archivePath);
    const documentName = `${mediaRevision.revision_id}.${format === "html" ? "html" : "md"}`;
    const imageName = `media/${key.object_id}-${key.object_version}.png`;
    assert(
      Object.keys(entries).sort().join("|") ===
        [documentName, imageName].sort().join("|") &&
        entries[imageName]?.equals(png),
      "Media ZIP omitted or changed the exact bound PNG bytes",
    );
    const document = entries[documentName].toString("utf8");
    const readableDocument =
      format === "markdown"
        ? document.replace(/\\([\\`*_{}\[\]()#+\-.!|>])/g, "$1")
        : document;
    assert(
      readableDocument.includes(evidenceTitle) &&
        !readableDocument.includes(`${evidenceTitle} (with image)`) &&
        readableDocument.includes(evidenceSentence) &&
        readableDocument.includes(imageName) &&
        readableDocument.includes(alt) &&
        readableDocument.includes(caption),
      "Media ZIP document differs from the selected historical content or image reference",
    );
    if (format === "html")
      assert(
        /<img\b/.test(document) && /<figcaption\b/.test(document),
        "HTML ZIP did not render the image and caption",
      );
    else
      assert(
        readableDocument.includes(`![${alt}](${imageName})`),
        "Markdown ZIP did not link its bundled image",
      );
  };
  await verifyMediaBundle("markdown");
  await verifyMediaBundle("html");
  await screenshot(page, runDir, "content-media-history-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "content-media-history-narrow", {
    width: 390,
    height: 844,
  });
  await history
    .getByRole("button", { name: new RegExp(`^v${saved.revision} ·`) })
    .click();
  await verifyDownload(saved, "markdown");
  await verifyDownload(saved, "html");
  console.log(
    "Content: source-backed rich text, authenticated media insertion/reload/history, exact media ZIPs and immutable text exports verified",
  );
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function safeEnvironment(extra = {}) {
  // An explicit allow-list prevents accidental passage of DB, model, proxy,
  // channel, token-center, and other workstation credentials to either child.
  const allowed = [
    "PATH",
    "Path",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
  ];
  const result = Object.fromEntries(
    allowed
      .filter((key) => process.env[key] !== undefined)
      .map((key) => [key, process.env[key]]),
  );
  return {
    ...result,
    TMP: scratch,
    TEMP: scratch,
    ...(browserCache ? { PLAYWRIGHT_BROWSERS_PATH: browserCache } : {}),
    NO_PROXY: "127.0.0.1,localhost",
    no_proxy: "127.0.0.1,localhost",
    ...extra,
  };
}

async function portIsFree(port) {
  const server = createServer();
  try {
    await new Promise((accept, reject) => {
      server.once("error", reject);
      server.listen({ host: "127.0.0.1", port, exclusive: true }, accept);
    });
    return true;
  } catch (error) {
    if (error.code === "EADDRINUSE" || error.code === "EACCES") return false;
    throw error;
  } finally {
    if (server.listening) {
      await new Promise((accept) => server.close(accept));
    }
  }
}

function ownedChild(command, args, cwd, env) {
  const child = spawn(command, args, {
    cwd,
    env,
    windowsHide: true,
    stdio: "ignore",
  });
  child.once("error", (error) => {
    child.launchError = error;
  });
  processes.push(child);
  return child;
}

async function untilReady(url, child, label) {
  const end = Date.now() + 25_000;
  while (Date.now() < end) {
    if (child.launchError) throw new Error(`${label} could not start`);
    assert(child.exitCode === null && !child.killed, `${label} exited early`);
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1500) });
      if (response.ok) return;
    } catch {
      // Expected before the service starts listening.
    }
    await new Promise((accept) => setTimeout(accept, 350));
  }
  throw new Error(`${label} failed its local readiness check within 25s`);
}

async function stopOwnedChildren() {
  for (const child of processes.reverse()) {
    if (child.exitCode !== null || child.signalCode !== null) continue;
    // Both commands are direct child processes, not shell trees. Never kill
    // by executable name or by port: those may belong to another workspace.
    child.kill("SIGTERM");
    await Promise.race([
      new Promise((accept) => child.once("exit", accept)),
      new Promise((accept) => setTimeout(accept, 2500)),
    ]);
    if (child.exitCode === null && child.signalCode === null)
      child.kill("SIGKILL");
  }
}

async function screenshot(page, runDir, name, viewport) {
  await page.setViewportSize(viewport);
  await page.waitForTimeout(350); // allow responsive layout to settle
  const overflow = await page.evaluate(() => ({
    viewport: innerWidth,
    document: document.documentElement.scrollWidth,
    body: document.body.scrollWidth,
    clippedRegions: [
      ".page-content",
      ".knowledge-import-sidebar",
      ".knowledge-import-form",
      ".knowledge-workbench",
      ".source-detail-page",
      ".channel-jobs-page",
      ".channel-jobs-form",
      ".reports-page",
      ".report-preview",
      ".question-sets-page",
      ".question-sets-edit-row",
      ".content-evidence-quotes",
      ".content-evidence-quotes blockquote",
      ".content-media-node",
      ".content-media-picker",
    ].flatMap((selector) =>
      [...document.querySelectorAll(selector)].flatMap((element) => {
        const style = getComputedStyle(element);
        const bounds = element.getBoundingClientRect();
        const clippedByWidth =
          element.scrollWidth > element.clientWidth + 2 &&
          !["auto", "scroll"].includes(style.overflowX);
        const outsideViewport =
          bounds.left < -2 || bounds.right > innerWidth + 2;
        return clippedByWidth || outsideViewport
          ? [
              {
                selector,
                client: element.clientWidth,
                content: element.scrollWidth,
                left: Math.round(bounds.left),
                right: Math.round(bounds.right),
              },
            ]
          : [];
      }),
    ),
  }));
  const path = join(runDir, `${name}.png`);
  await page.screenshot({ path, fullPage: true, animations: "disabled" });
  assert(
    overflow.document <= overflow.viewport + 1 &&
      overflow.body <= overflow.viewport + 1,
    `${name} has horizontal viewport overflow: ${JSON.stringify(overflow)}`,
  );
  if (overflow.clippedRegions.length) {
    visualIssues.push(`${name}: ${JSON.stringify(overflow.clippedRegions)}`);
    console.log(`${name}: screenshot captured; clipped UI regions detected`);
  } else {
    console.log(
      `${name}: screenshot captured; no viewport overflow or clipped UI region`,
    );
  }
}

async function main() {
  assert(
    binary && outputRoot,
    "Set GEO_SMOKE_APP_BINARY and GEO_SMOKE_OUTPUT_DIR",
  );
  assert(
    await portIsFree(apiPort),
    "Smoke API port is occupied; refusing to touch another service",
  );
  assert(
    await portIsFree(webPort),
    "Smoke web port is occupied; refusing to touch another service",
  );
  if (contentMode)
    assert(
      await portIsFree(providerPort),
      "Smoke provider port is occupied; refusing to touch another service",
    );
  const runDir = join(resolve(outputRoot), `run-${randomUUID()}`);
  screenshotsDirectory = runDir;
  await mkdir(scratch, { recursive: true });
  await mkdir(runDir, { recursive: true });
  const password = randomBytes(36).toString("base64url");
  ephemeralPassword = password;
  let modelEnvironment = {};
  if (contentMode) {
    const [agent, content] = await Promise.all([
      approvedBundle("memeloop-agent-loop.bundle.mjs"),
      approvedBundle("memeloop-content-workflow.bundle.mjs"),
    ]);
    ephemeralProviderKey = randomBytes(36).toString("base64url");
    modelEnvironment = {
      ...(await startFixtureProvider(providerPort, ephemeralProviderKey)),
      GEO_AGENT_BUNDLE_PATH: agent.path,
      GEO_AGENT_BUNDLE_SHA256: agent.digest,
      GEO_CONTENT_BUNDLE_PATH: content.path,
      GEO_CONTENT_BUNDLE_SHA256: content.digest,
    };
  }
  const api = ownedChild(
    binary,
    [],
    repository,
    safeEnvironment({
      GEO_DEV_PASSWORD: password,
      GEO_DEV_LOGIN_NAME: "demo@localhost",
      GEO_BIND_ADDR: `127.0.0.1:${apiPort}`,
      GEO_ALLOWED_ORIGINS: `${webUrl},${apiUrl}`,
      ...modelEnvironment,
    }),
  );
  await untilReady(`${apiUrl}/health/ready`, api, "Rust API");
  const viteCli = join(
    dirname(requireWeb.resolve("vite/package.json")),
    "bin",
    "vite.js",
  );
  const vite = ownedChild(
    process.execPath,
    [
      viteCli,
      "--host",
      "127.0.0.1",
      "--port",
      String(webPort),
      "--strictPort",
      "--configLoader",
      "runner",
    ],
    webRoot,
    safeEnvironment({ VITE_DEV_API_PROXY_TARGET: apiUrl }),
  );
  await untilReady(`${webUrl}/`, vite, "Vite");

  if (browserCache) process.env.PLAYWRIGHT_BROWSERS_PATH = browserCache;
  const { chromium } = requireBrowser("playwright");
  browserServer = await chromium.launchServer({
    headless: true,
    timeout: 15_000,
    env: safeEnvironment(),
  });
  browser = await chromium.connect(browserServer.wsEndpoint(), {
    timeout: 15_000,
  });
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    serviceWorkers: "block",
  });
  const page = await context.newPage();
  page.setDefaultTimeout(14_000);
  page.setDefaultNavigationTimeout(18_000);
  const apiResponses = [];
  const writes = [];
  page.on("response", (response) => {
    if (response.url().startsWith(`${webUrl}/api/v1/`)) {
      apiResponses.push({
        path: new URL(response.url()).pathname,
        status: response.status(),
      });
    }
  });
  page.on("request", (request) => {
    const { pathname } = new URL(request.url());
    if (
      request.url().startsWith(`${webUrl}/api/v1/`) &&
      pathname.includes("/reports") &&
      !["GET", "HEAD"].includes(request.method())
    ) {
      writes.push(request.method());
    }
  });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`${webUrl}/login`);
  await page.getByRole("textbox", { name: "用户名" }).fill("demo@localhost");
  await page.getByRole("textbox", { name: "密码" }).fill(password);
  const loginResponse = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/v1/auth/login" &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "登录", exact: true }).click();
  assert(
    (await loginResponse).status() === 200,
    "Synthetic first-party login was rejected",
  );
  await page
    .getByRole("heading", { name: "选择工作区", exact: true })
    .waitFor();
  assert(
    apiResponses.some(
      (response) =>
        response.path.endsWith("/auth/login") && response.status === 200,
    ),
    "First-party browser login did not complete against Rust API",
  );
  console.log("First-party login: real local API accepted");

  // First-use entry must open a real draft conversation without a setup form.
  await page.getByRole("button", { name: "创建项目" }).first().click();
  await page.waitForURL(/\/app\/[^/]+\/[^/]+\/chat$/);
  await page.getByTestId("agent-workbench-page").waitFor();
  assert(
    (await page.getByRole("textbox", { name: "品牌名称" }).count()) === 0,
    "First-use entry must not require a brand form",
  );
  const match = /^\/app\/([^/]+)\/([^/]+)\/chat$/.exec(
    new URL(page.url()).pathname,
  );
  assert(match, "Project creation did not reach its empty composer");
  const [, tenantId, projectId] = match;
  const base = `${webUrl}/app/${tenantId}/${projectId}`;
  await screenshot(page, runDir, "p00-chat-first-desktop", {
    width: 1440,
    height: 900,
  });
  // Optional detail editing supplies synthetic fixtures for later page checks.
  // This is not the default onboarding path or a model-start acceptance test.
  await page.goto(`${base}/setup`);
  await page.getByRole("textbox", { name: "品牌名称" }).fill("Synthetic Acme");
  await page
    .getByRole("textbox", { name: "初始资料（可选）" })
    .fill("Synthetic Acme makes a fictional sample product for local testing.");
  await page.getByRole("button", { name: "下一步" }).click();
  await page.getByRole("heading", { name: "目标与市场" }).waitFor();
  await page.getByRole("textbox", { name: "市场", exact: true }).fill("中国");
  await page.getByRole("textbox", { name: "语言", exact: true }).fill("zh-CN");
  await page.getByRole("button", { name: "下一步" }).click();
  await page.getByRole("heading", { name: "发布资源与预算" }).waitFor();
  await page.getByRole("button", { name: "启动项目" }).click();
  await page.waitForURL(/\/app\/[^/]+\/[^/]+\/chat$/);
  assert(
    page.url() === `${base}/chat`,
    "Optional setup changed the project identity",
  );
  console.log(
    "Chat-first draft entry and optional same-project setup verified",
  );

  await page.goto(`${base}/knowledge`);
  await page
    .getByRole("heading", { name: "企业知识库", exact: true })
    .waitFor();
  await screenshot(page, runDir, "p03-desktop-before-import", {
    width: 1440,
    height: 900,
  });
  await page.getByRole("button", { name: "导入资料", exact: true }).click();
  const sidebar = page.getByRole("complementary", { name: "导入资料" });
  await sidebar.locator('input[type="file"]').setInputFiles({
    name: "synthetic-evidence.csv",
    mimeType: "text/csv",
    buffer: Buffer.from(
      'product,price,note\r\nsample-A,0012,"plain sample"\r\nsample-B,"USD 20","quoted, cell"\r\n',
      "utf8",
    ),
  });
  await sidebar.getByRole("button", { name: "开始导入" }).click();
  await sidebar.getByText("已受理 1 项，失败 0 项").waitFor();
  const detailLink = sidebar.getByRole("link", { name: "查看处理详情" });
  await detailLink.waitFor();
  await screenshot(page, runDir, "p03-desktop-import", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "p03-narrow-import", {
    width: 390,
    height: 844,
  });
  await detailLink.click();
  await page.getByRole("heading", { name: "synthetic-evidence.csv" }).waitFor();
  await page
    .getByText(/第 2–2 条逻辑记录/)
    .first()
    .waitFor();
  await page.getByText("sample-A").first().waitFor();
  await screenshot(page, runDir, "p04-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "p04-narrow", {
    width: 390,
    height: 844,
  });
  assert(
    apiResponses.some(
      (response) =>
        response.path.includes("upload-sessions") && response.status < 300,
    ),
    "CSV upload did not reach the actual Rust API",
  );
  console.log("P04: actual CSV source and logical-record evidence visible");

  await page.goto(`${base}/knowledge`);
  await page.getByRole("button", { name: "导入资料", exact: true }).click();
  const originalMarkdown =
    "# 合成标题\n\n这是 **合成重点**，包含 [示例链接](https://example.invalid/guide)。\n\n1. 第一项\n2. 第二项\n\n| 项目 | 值 |\n| --- | --- |\n| 合成产品 | 中文 |\n";
  await sidebar.locator('input[type="file"]').setInputFiles({
    name: "synthetic-article.md",
    mimeType: "text/markdown",
    buffer: Buffer.from(originalMarkdown, "utf8"),
  });
  await sidebar.getByRole("button", { name: "开始导入" }).click();
  await sidebar.getByText("已受理 1 项，失败 0 项").waitFor();
  await sidebar.getByRole("link", { name: "查看处理详情" }).click();
  await page.getByRole("heading", { name: "synthetic-article.md" }).waitFor();
  const visualEditor = page.locator(
    "[data-testid='knowledge-visual-editor'] [contenteditable='true']",
  );
  await visualEditor.locator("h1").filter({ hasText: "合成标题" }).waitFor();
  assert(
    (await visualEditor.locator("strong").textContent()) === "合成重点",
    "Markdown emphasis was not rendered in the actual knowledge editor",
  );
  assert(
    (await visualEditor.locator("table").textContent()).includes("中文"),
    "Markdown table was not rendered in the actual knowledge editor",
  );
  assert(
    await page.getByRole("button", { name: "保存为新版本" }).isDisabled(),
    "Parsing unchanged Markdown must not create an editable revision",
  );
  const originalVersion = await page
    .getByRole("combobox", { name: "查看证据版本" })
    .inputValue();
  const editorToolbar = page.getByRole("toolbar", { name: "资料编辑" });
  await editorToolbar
    .getByRole("button", { name: "标题", exact: true })
    .focus();
  await page.keyboard.press("ArrowRight");
  assert(
    await editorToolbar
      .getByRole("button", { name: "加粗", exact: true })
      .evaluate((element) => element === document.activeElement),
    "Knowledge editor formatting toolbar must support arrow-key navigation",
  );
  await visualEditor.locator("a").click();
  await visualEditor.locator("a").evaluate((anchor) => {
    const selection = document.getSelection();
    const range = document.createRange();
    range.selectNodeContents(anchor);
    selection?.removeAllRanges();
    selection?.addRange(range);
  });
  assert(
    (await page.evaluate(() => getSelection()?.toString())) === "示例链接",
    "Existing linked text was not selected before revising its URL",
  );
  await editorToolbar.getByRole("button", { name: "添加链接" }).click();
  const linkPopover = page.locator(".source-link-popover");
  const linkInput = linkPopover.getByRole("textbox", { name: "链接地址" });
  assert(
    (await linkInput.inputValue()) === "https://example.invalid/guide",
    "Editing an existing link must prefill its saved URL",
  );
  await linkInput.fill("javascript:alert(1)");
  await linkPopover.getByRole("button", { name: "添加链接" }).click();
  await linkPopover.getByRole("alert").waitFor();
  assert(
    await page.getByRole("button", { name: "保存为新版本" }).isDisabled(),
    "Rejected link schemes must not dirty the saved document",
  );
  await linkInput.fill("/updated");
  await linkPopover.getByRole("button", { name: "添加链接" }).click();
  await page.keyboard.press("Escape");
  assert(
    (await visualEditor.locator("a").getAttribute("href")) === "/updated",
    "Updating a link must update the linked text, not just future typing",
  );
  await visualEditor.locator("h1").click();
  await page.keyboard.press("End");
  await page.keyboard.insertText("（修订）");
  const textRevisionResponse = page.waitForResponse(
    (response) =>
      /\/knowledge\/sources\/[^/]+\/versions$/.test(
        new URL(response.url()).pathname,
      ) && response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "保存为新版本" }).click();
  const savedText = await textRevisionResponse;
  assert(
    savedText.status() === 201,
    "Knowledge text revision was not persisted",
  );
  const textReceipt = await savedText.json();
  assert(
    textReceipt.source_version.source_version_id !== originalVersion,
    "Visual edits must create a distinct immutable source version",
  );
  await page.reload();
  await page
    .locator(".source-detail-page h1")
    .filter({ hasText: "合成标题（修订）" })
    .waitFor();
  assert(
    (await visualEditor.locator("a").getAttribute("href")) === "/updated",
    "The revised link must survive a persisted page reload",
  );
  await screenshot(page, runDir, "p04-markdown-editor-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "p04-markdown-editor-narrow", {
    width: 390,
    height: 844,
  });
  await page
    .getByRole("combobox", { name: "查看证据版本" })
    .selectOption(originalVersion);
  await page
    .getByText("正在查看历史版本的已保存内容。", { exact: true })
    .waitFor();
  assert(
    (await page
      .locator(".source-detail-page [contenteditable='true']")
      .count()) === 0,
    "Historical source versions must remain read-only",
  );
  assert(
    (await page.locator(".source-original-text").textContent()) ===
      originalMarkdown,
    "Saving a visual revision changed the immutable original Markdown",
  );
  console.log(
    "P04: actual Markdown visual edit, persisted reload and immutable history verified",
  );
  if (contentMode) {
    await verifyGeneratedRichContent(page, base, tenantId, projectId, runDir);
  }

  await page.goto(`${base}/measurement`);
  await page
    .getByRole("heading", { name: "测量与洞察", exact: true })
    .waitFor();
  await screenshot(page, runDir, "p13-start-narrow", {
    width: 390,
    height: 844,
  });
  const measureTab = page.getByRole("tab", { name: "开始测量", exact: true });
  await measureTab.focus();
  await page.keyboard.press("ArrowRight");
  assert(
    await page
      .getByRole("tab", { name: "测量记录", exact: true })
      .evaluate((element) => element === document.activeElement),
    "Measurement tabs did not move keyboard focus",
  );
  await page.keyboard.press("Enter");
  await page.waitForURL((url) => url.searchParams.get("tab") === "records");
  await page.getByRole("tab", { name: "问题集", exact: true }).click();
  await page
    .getByRole("heading", { name: "问题集与版本", exact: true })
    .waitFor();
  await page.getByRole("button", { name: "新建问题集", exact: true }).click();
  await page
    .getByRole("textbox", { name: "问题集名称", exact: true })
    .fill("Synthetic questions");
  await page
    .getByRole("textbox", { name: "每行一个问题" })
    .fill(
      Array.from(
        { length: 5 },
        (_, i) => `Synthetic product question ${i + 1}?`,
      ).join("\n"),
    );
  const createdResponse = page.waitForResponse(
    (response) =>
      response.url().includes("/question-sets") &&
      response.request().method() === "POST" &&
      response.status() < 300,
  );
  await page.getByRole("button", { name: "创建并保存问题集" }).click();
  const questionVersion = await (await createdResponse).json();
  assert(
    questionVersion.optimization_count === 4 &&
      questionVersion.evaluation_count === 1,
    "Actual question repository did not preserve the project split",
  );
  await page
    .getByRole("heading", { name: "Synthetic questions · v1" })
    .waitFor();
  await page.getByRole("button", { name: "基于当前版本修订" }).click();
  await page
    .getByRole("textbox", { name: "问题 1", exact: true })
    .fill("Synthetic revised product question?");
  await screenshot(page, runDir, "p13-edit-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "p13-edit-narrow", {
    width: 390,
    height: 844,
  });
  const revisedResponse = page.waitForResponse(
    (response) =>
      response.url().includes("/versions") &&
      response.request().method() === "POST" &&
      response.status() < 300,
  );
  await page.getByRole("button", { name: "保存为新版本" }).click();
  const revisedVersion = await (await revisedResponse).json();
  assert(
    revisedVersion.parent_version_id === questionVersion.id &&
      revisedVersion.questions.every((question) =>
        questionVersion.questions.some(
          (original) =>
            original.question_id === question.question_id &&
            original.purpose === question.purpose,
        ),
      ),
    "Question revision changed identity or fixed purpose",
  );
  await page
    .getByRole("heading", { name: "Synthetic questions · v2" })
    .waitFor();
  await page.reload();
  await page
    .getByLabel("选择问题集", { exact: true })
    .selectOption(questionVersion.question_set_id);
  await page
    .getByRole("heading", { name: "Synthetic questions · v2" })
    .waitFor();
  await page
    .getByLabel("选择不可变版本", { exact: true })
    .selectOption(questionVersion.id);
  await page
    .getByRole("heading", { name: "Synthetic questions · v1" })
    .waitFor();
  await page
    .getByText("Synthetic product question 1?", { exact: true })
    .waitFor();
  console.log(
    "P13: real API create/revise/reload/history; frozen identities preserved",
  );

  await page.goto(`${base}/publications`);
  await page.getByRole("heading", { name: "发布目标与执行记录" }).waitFor();
  await page.getByText("网页账号登录只表示可尝试采样").waitFor();
  await page.getByText("官方联网搜索适配器尚未实测验证").waitFor();
  await page.getByText("还没有测量目标。").waitFor();
  assert(
    apiResponses.some(
      (response) =>
        response.path.endsWith("/cycles/current") && response.status === 200,
    ),
    "P12 did not read the active cycle from the actual Rust API",
  );
  await screenshot(page, runDir, "p12-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "p12-narrow", {
    width: 390,
    height: 844,
  });

  const previewResponse = page.waitForResponse(
    (response) =>
      response.url().includes("/report-preview") && response.status() === 200,
  );
  await page.goto(`${base}/reports`);
  const preview = await (await previewResponse).json();
  assert(
    preview.kind === "preview" &&
      preview.project_id === projectId &&
      !Object.hasOwn(preview, "report_id") &&
      !Object.hasOwn(preview, "revision") &&
      !Object.hasOwn(preview, "correction_of"),
    "P14 response is not a distinct non-persisted preview projection",
  );
  const previewArea = page.getByRole("region", { name: "临时报告预览" });
  await previewArea
    .getByText("临时预览 · 未保存为正式周报", { exact: false })
    .waitFor();
  await page.getByRole("heading", { name: "尚无周报快照" }).waitFor();
  assert(
    (await previewArea.getByText("报告 ID", { exact: true }).count()) === 0 &&
      (await previewArea
        .getByRole("button", { name: "下载覆盖与证据 CSV" })
        .count()) === 0,
    "Temporary preview is being presented as an official saved report",
  );
  await screenshot(page, runDir, "p14-preview-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, runDir, "p14-preview-narrow", {
    width: 390,
    height: 844,
  });
  const refreshedPreview = page.waitForResponse(
    (response) =>
      response.url().includes("/report-preview") && response.status() === 200,
  );
  await previewArea.getByRole("button", { name: "刷新预览" }).click();
  const refreshed = await (await refreshedPreview).json();
  assert(
    refreshed.kind === "preview" && !Object.hasOwn(refreshed, "report_id"),
    "Refreshed preview acquired a saved report identity",
  );
  await page.reload();
  await page.getByRole("heading", { name: "尚无周报快照" }).waitFor();
  assert(
    writes.length === 0,
    `Preview caused unexpected report API write: ${writes.join(", ")}`,
  );
  console.log(
    "P14: distinct temporary preview; no saved report or report write",
  );

  assert(
    errors.length === 0,
    `Browser JavaScript errors: ${errors.join(" | ")}`,
  );
  if (visualIssues.length) {
    for (const issue of visualIssues) console.error(`VISUAL ISSUE: ${issue}`);
    throw new Error(
      `${visualIssues.length} screenshot states contain clipped UI regions`,
    );
  }
  console.log(
    `PASS: synthetic in-memory browser smoke${contentMode ? " + source-backed rich-content" : ""}; screenshots: ${runDir}`,
  );
  console.log(
    `Scope: local Rust API and UI${contentMode ? " with loopback-only synthetic provider" : ""}; no external publishing, AI, or account acceptance.`,
  );
}

try {
  deadline = setTimeout(
    () => {
      console.error(
        `Smoke test exceeded its ${contentMode ? 300 : 180}s deadline`,
      );
      process.exitCode = 1;
      for (const child of processes) {
        if (child.exitCode === null && child.signalCode === null) child.kill();
      }
    },
    contentMode ? 300_000 : 180_000,
  );
  await main();
} catch (error) {
  // Do not include child output, request bodies, tokens, or raw environment.
  const message = error instanceof Error ? error.message : "unknown error";
  console.error(
    `FAIL: ${[ephemeralPassword, ephemeralProviderKey].filter(Boolean).reduce((text, key) => text.replaceAll(key, "[redacted]"), message)}`,
  );
  if (screenshotsDirectory)
    console.error(`Screenshots: ${screenshotsDirectory}`);
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  try {
    if (browser) {
      await Promise.race([
        browser.close().catch(() => undefined),
        new Promise((accept) => setTimeout(accept, 5_000)),
      ]);
    }
    if (browserServer) {
      await Promise.race([
        browserServer.close().catch(() => undefined),
        new Promise((accept) => setTimeout(accept, 5_000)),
      ]);
    }
    if (providerServer) {
      await Promise.race([
        new Promise((accept) => providerServer.close(accept)),
        new Promise((accept) => setTimeout(accept, 2_000)),
      ]);
    }
  } finally {
    // BrowserServer.process() is the Playwright-owned Chromium process. Never
    // terminate other browser instances discovered through a port/name.
    const ownedBrowserProcess = browserServer?.process();
    if (
      ownedBrowserProcess?.exitCode === null &&
      ownedBrowserProcess.signalCode === null
    ) {
      ownedBrowserProcess.kill("SIGTERM");
    }
    await stopOwnedChildren();
  }
}
