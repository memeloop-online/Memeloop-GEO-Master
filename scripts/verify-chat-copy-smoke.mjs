// Isolated, synthetic browser check for the chat-first zh/en copy.
// Set GEO_SMOKE_APP_BINARY to a copied development binary and
// GEO_SMOKE_OUTPUT_DIR outside the repository; optionally set isolated
// GEO_SMOKE_API_PORT/GEO_SMOKE_WEB_PORT and PLAYWRIGHT_BROWSERS_PATH.
// GEO_TEST_CHROMIUM_PATH optionally selects an existing Chromium executable.
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const webRoot = join(repository, "apps", "web");
const requireWeb = createRequire(join(webRoot, "package.json"));
const requireBrowser = createRequire(
  join(repository, "packages", "browser-runner", "package.json"),
);
const apiPort = Number(process.env.GEO_SMOKE_API_PORT ?? 18090);
const webPort = Number(process.env.GEO_SMOKE_WEB_PORT ?? 15179);
const apiUrl = `http://127.0.0.1:${apiPort}`;
const webUrl = `http://127.0.0.1:${webPort}`;
const outputDir = process.env.GEO_SMOKE_OUTPUT_DIR;
const binary = process.env.GEO_SMOKE_APP_BINARY;
const scratch = resolve(process.env.GEO_SMOKE_TMP_DIR ?? tmpdir());
const children = [];
let browserServer;
let browser;
let deadline;

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function safeEnvironment(extra = {}) {
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
  return {
    ...Object.fromEntries(
      allowed
        .filter((name) => process.env[name] !== undefined)
        .map((name) => [name, process.env[name]]),
    ),
    TMP: scratch,
    TEMP: scratch,
    ...(process.env.PLAYWRIGHT_BROWSERS_PATH
      ? { PLAYWRIGHT_BROWSERS_PATH: process.env.PLAYWRIGHT_BROWSERS_PATH }
      : {}),
    NO_PROXY: "127.0.0.1,localhost",
    no_proxy: "127.0.0.1,localhost",
    ...extra,
  };
}

async function portFree(port) {
  const server = createServer();
  try {
    await new Promise((accept, reject) => {
      server.once("error", reject);
      server.listen({ host: "127.0.0.1", port, exclusive: true }, accept);
    });
    return true;
  } catch (error) {
    if (["EADDRINUSE", "EACCES"].includes(error.code)) return false;
    throw error;
  } finally {
    if (server.listening) await new Promise((accept) => server.close(accept));
  }
}

function launch(command, args, cwd, env) {
  const child = spawn(command, args, {
    cwd,
    env,
    windowsHide: true,
    stdio: "ignore",
  });
  child.once("error", (error) => {
    child.launchError = error;
  });
  children.push(child);
  return child;
}

async function ready(url, child) {
  const end = Date.now() + 25_000;
  while (Date.now() < end) {
    assert(!child.launchError && child.exitCode === null, "Child exited early");
    try {
      if ((await fetch(url, { signal: AbortSignal.timeout(1200) })).ok) return;
    } catch {
      // Service has not bound its private port yet.
    }
    await new Promise((accept) => setTimeout(accept, 250));
  }
  throw new Error("Isolated service readiness timed out");
}

async function screenshot(page, name, viewport) {
  await page.setViewportSize(viewport);
  await page.waitForTimeout(300);
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth > innerWidth + 1 ||
      document.body.scrollWidth > innerWidth + 1,
  );
  assert(!overflow, `${name} has horizontal overflow`);
  await page.screenshot({
    path: join(outputDir, `${name}.png`),
    fullPage: true,
    animations: "disabled",
  });
}

async function main() {
  assert(binary && outputDir, "Set copied binary and private output directory");
  assert(
    [apiPort, webPort].every(
      (port) => Number.isInteger(port) && port >= 1 && port <= 65535,
    ) && apiPort !== webPort,
    "Use distinct valid isolated ports",
  );
  assert(
    (await portFree(apiPort)) && (await portFree(webPort)),
    "Isolated smoke port occupied; refusing to touch another process",
  );
  await mkdir(outputDir, { recursive: true });
  await mkdir(scratch, { recursive: true });
  const password = randomBytes(32).toString("base64url");
  const api = launch(
    binary,
    [],
    repository,
    safeEnvironment({
      GEO_DEV_LOGIN_NAME: "demo@localhost",
      GEO_DEV_PASSWORD: password,
      GEO_BIND_ADDR: `127.0.0.1:${apiPort}`,
      GEO_ALLOWED_ORIGINS: `${webUrl},${apiUrl}`,
    }),
  );
  await ready(`${apiUrl}/health/ready`, api);
  const viteCli = join(
    dirname(requireWeb.resolve("vite/package.json")),
    "bin",
    "vite.js",
  );
  const web = launch(
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
  await ready(`${webUrl}/`, web);
  const { chromium } = requireBrowser("playwright");
  browserServer = await chromium.launchServer({
    headless: true,
    ...(process.env.GEO_TEST_CHROMIUM_PATH
      ? { executablePath: process.env.GEO_TEST_CHROMIUM_PATH }
      : {}),
    env: safeEnvironment(),
    timeout: 15_000,
  });
  browser = await chromium.connect(browserServer.wsEndpoint());
  const context = await browser.newContext({ acceptDownloads: true });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`${webUrl}/login`);
  await page.getByRole("textbox", { name: "用户名" }).fill("demo@localhost");
  await page.getByRole("textbox", { name: "密码" }).fill(password);
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await page
    .getByRole("heading", { name: "选择工作区", exact: true })
    .waitFor();
  await page.getByRole("button", { name: "创建项目" }).first().click();
  await page.waitForURL(/\/app\/[^/]+\/[^/]+\/chat$/);
  await page.getByTestId("agent-workbench-page").waitFor();
  const freshUrl = page.url();
  const list = page.getByRole("complementary", { name: "AI 对话列表" });
  await page.getByRole("heading", { name: "从你的资料或想法开始" }).waitFor();
  await page.getByPlaceholder("提出问题、描述任务，或添加文件").waitFor();
  assert(
    (await list.getByRole("heading", { name: "AI 工作台" }).count()) === 1 &&
      (await list.getByText("还没有对话。").count()) === 1 &&
      (await page.getByRole("textbox", { name: "品牌名称" }).count()) === 0,
    "First Chinese composer showed duplicate titles or setup gate",
  );
  await screenshot(page, "chat-zh-first-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, "chat-zh-first-narrow", { width: 390, height: 844 });

  const chineseInput = page.getByTestId("agent-multi-file-input");
  await chineseInput.setInputFiles({
    name: "synthetic-note.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("Synthetic browser copy verification only.\n"),
  });
  const attachmentGroup = page.getByLabel("待发送附件");
  await attachmentGroup
    .getByText("上传的文件可在对话中使用。需要加入企业知识时，请告诉 AI。")
    .waitFor();
  const completed = page.waitForResponse(
    (response) =>
      /\/agent\/attachments\/upload-sessions\/[^/]+\/complete$/.test(
        new URL(response.url()).pathname,
      ) && response.request().method() === "POST",
  );
  const created = page
    .waitForResponse(
      (response) =>
        new URL(response.url()).pathname.endsWith("/agent/conversations") &&
        response.request().method() === "POST",
    )
    .then(async (response) => {
      assert(response.status() < 300, "First send did not create conversation");
      return response.json();
    });
  await attachmentGroup.getByRole("button", { name: "仅发送附件" }).click();
  assert(
    (await completed).status() === 200,
    "Synthetic attachment did not upload",
  );
  const conversation = await created;
  await page.waitForURL(/\/chat\/[0-9a-f-]+$/);
  const runtimeNotice = page.locator(".agent-runtime-notice");
  // The persisted notice includes a reload action, not just the limitation copy.
  const chineseLimitation = runtimeNotice.getByText(
    /^AI 服务未启用，请联系管理员完成配置。\s*重新加载对话$/,
  );
  await chineseLimitation.waitFor({ state: "visible" });
  await runtimeNotice
    .getByRole("button", { name: "重新加载对话", exact: true })
    .waitFor({ state: "visible" });
  // The send flow can still have a detail GET in flight. Only accept a request
  // issued after the reload commits, not an old document's late response whose
  // body Chromium discards on navigation.
  let reloadCommitted = false;
  const onReload = (frame) => {
    if (frame === page.mainFrame()) reloadCommitted = true;
  };
  page.on("framenavigated", onReload);
  const restored = page
    .waitForRequest(
      (request) =>
        reloadCommitted &&
        new URL(request.url()).pathname.endsWith(
          `/agent/conversations/${conversation.id}`,
        ) &&
        request.method() === "GET",
    )
    .then(async (request) => {
      const response = await request.response();
      assert(response?.ok(), "Could not restore the no-model conversation");
      return response.json();
    });
  let restoredConversation;
  try {
    [restoredConversation] = await Promise.all([restored, page.reload()]);
  } finally {
    page.off("framenavigated", onReload);
  }
  assert(
    restoredConversation.runs.length === 1 &&
      restoredConversation.runs[0].status === "failed" &&
      restoredConversation.runs[0].capability.status === "missing" &&
      restoredConversation.turns.length === 1 &&
      restoredConversation.turns[0].status === "failed",
    "No-model attachment send did not persist its actual AI limitation",
  );
  assert(
    restoredConversation.messages.length === 1 &&
      restoredConversation.messages[0].role === "user" &&
      restoredConversation.messages[0].attachments.length === 1,
    "No-model conversation lost its attachment or fabricated an assistant reply",
  );
  await chineseLimitation.waitFor({ state: "visible" });
  await runtimeNotice
    .getByRole("button", { name: "重新加载对话", exact: true })
    .waitFor({ state: "visible" });
  assert(
    freshUrl !== page.url() &&
      (await page.getByRole("heading", { name: "AI 工作台" }).count()) >= 1,
    "First attachment send did not preserve the chat-first flow",
  );
  const chineseDate = new Intl.DateTimeFormat("zh-CN", {
    dateStyle: "medium",
  }).format(new Date(conversation.created_at));
  await list.getByText(`新对话 · ${chineseDate}`).waitFor();
  assert(
    (await list.locator(".agent-conversation-item").count()) === 1,
    "First attachment send created duplicate conversations",
  );
  await screenshot(page, "chat-zh-upload-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, "chat-zh-upload-narrow", { width: 390, height: 844 });

  await page.getByRole("combobox", { name: "语言" }).selectOption("en");
  await page.getByRole("heading", { name: "AI workspace" }).first().waitFor();
  const englishList = page.getByRole("complementary", {
    name: "AI conversations",
  });
  await runtimeNotice
    .getByText(
      /^AI service is not enabled\. Contact your administrator to set it up\.\s*Reload conversations$/,
    )
    .waitFor({ state: "visible" });
  await runtimeNotice
    .getByRole("button", { name: "Reload conversations", exact: true })
    .waitFor({ state: "visible" });
  const englishDate = new Intl.DateTimeFormat("en", {
    dateStyle: "medium",
  }).format(new Date(conversation.created_at));
  await englishList.getByText(`New conversation · ${englishDate}`).waitFor();
  await screenshot(page, "chat-en-upload-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, "chat-en-upload-narrow", { width: 390, height: 844 });
  await englishList.getByRole("button", { name: "New", exact: true }).click();
  await page.waitForURL(/\/app\/[^/]+\/[^/]+\/chat$/);
  await page
    .getByRole("heading", { name: "Start with your files or ideas" })
    .waitFor();
  await page
    .getByPlaceholder("Ask a question, describe a task, or add a file")
    .waitFor();
  await screenshot(page, "chat-en-first-desktop", {
    width: 1440,
    height: 900,
  });
  await screenshot(page, "chat-en-first-narrow", { width: 390, height: 844 });
  assert(!errors.length, `Browser errors: ${errors.join(" | ")}`);
  console.log(
    `PASS: actual zh/en chat-first, upload scope, and locale dates; screenshots: ${outputDir}`,
  );
}

try {
  await Promise.race([
    main(),
    new Promise((_, reject) => {
      deadline = setTimeout(
        () => reject(new Error("Smoke deadline exceeded")),
        100_000,
      );
    }),
  ]);
} catch (error) {
  console.error(`FAIL: ${error instanceof Error ? error.message : "unknown"}`);
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  if (browser) await browser.close().catch(() => undefined);
  if (browserServer) await browserServer.close().catch(() => undefined);
  for (const child of children.reverse()) {
    if (child.exitCode !== null || child.signalCode !== null) continue;
    child.kill("SIGTERM");
    await Promise.race([
      new Promise((accept) => child.once("exit", accept)),
      new Promise((accept) => setTimeout(accept, 2500)),
    ]);
    if (child.exitCode === null && child.signalCode === null)
      child.kill("SIGKILL");
  }
}
