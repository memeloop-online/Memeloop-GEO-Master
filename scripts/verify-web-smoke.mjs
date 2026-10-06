// Real local-memory API + real Vite + installed Chromium. No route interception,
// external provider requests, account connections, or reusable credentials.
// Set GEO_SMOKE_APP_BINARY and GEO_SMOKE_OUTPUT_DIR (outside the repository).
// Optional: PLAYWRIGHT_BROWSERS_PATH, GEO_SMOKE_TMP_DIR,
// GEO_SMOKE_API_PORT, GEO_SMOKE_WEB_PORT; then run with Node.
import { randomBytes, randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
const processes = [];
const visualIssues = [];
let browser;
let browserServer;
let deadline;
let screenshotsDirectory;
let ephemeralPassword;

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
  const runDir = join(resolve(outputRoot), `run-${randomUUID()}`);
  screenshotsDirectory = runDir;
  await mkdir(scratch, { recursive: true });
  await mkdir(runDir, { recursive: true });
  const password = randomBytes(36).toString("base64url");
  ephemeralPassword = password;
  const api = ownedChild(
    binary,
    [],
    repository,
    safeEnvironment({
      GEO_DEV_PASSWORD: password,
      GEO_DEV_LOGIN_NAME: "demo@localhost",
      GEO_BIND_ADDR: `127.0.0.1:${apiPort}`,
      GEO_ALLOWED_ORIGINS: `${webUrl},${apiUrl}`,
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
  await page.getByRole("heading", { name: "选择客户工作区" }).waitFor();
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
    `PASS: synthetic in-memory browser smoke; screenshots: ${runDir}`,
  );
  console.log(
    "Scope: local Rust API and UI only; no external publishing, AI, or account acceptance.",
  );
}

try {
  deadline = setTimeout(() => {
    console.error("Smoke test exceeded its 180s deadline");
    process.exitCode = 1;
    for (const child of processes) {
      if (child.exitCode === null && child.signalCode === null) child.kill();
    }
  }, 180_000);
  await main();
} catch (error) {
  // Do not include child output, request bodies, tokens, or raw environment.
  const message = error instanceof Error ? error.message : "unknown error";
  console.error(
    `FAIL: ${ephemeralPassword ? message.replaceAll(ephemeralPassword, "[redacted]") : message}`,
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
