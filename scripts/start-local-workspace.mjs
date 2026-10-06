// Persistent, loopback-only local workspace. Never pass credentials in argv or output.
import { randomBytes, randomUUID } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { lstat, mkdir, open, readFile, realpath } from "node:fs/promises";
import {
  basename,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const webRoot = join(repository, "apps", "web");
const packageRoot = join(repository, "packages", "browser-runner");
const apiUrl = "http://127.0.0.1:8080";
const webUrl = "http://localhost:5173";
const runnerUrl = "http://127.0.0.1:38080";
const label = "org.memeloop-geo.local-workspace";
const image = "postgres:17-alpine";
const markerName = ".geo-local-owned";
const configName = "config.json";
const children = [];
let startupStage = "prerequisites";
let browser;
let browserServer;
let shuttingDown = false;
let shutdownRequested = false;
let resolveStopSignal;
const stopSignal = new Promise((yes) => {
  resolveStopSignal = yes;
});

const startupStages = new Set([
  "prerequisites",
  "private configuration",
  "local PostgreSQL",
  "identity bootstrap",
  "browser runner",
  "Rust API",
  "frontend",
  "first-party browser login",
  "service supervision",
]);

// The verifier accepts only these fixed labels over IPC, never child output,
// exception text, private paths, or arbitrary diagnostic payloads.
export function launcherFailureStage(message) {
  return message?.type === "local-workspace-failure" &&
    startupStages.has(message.stage)
    ? message.stage
    : undefined;
}

function assert(ok, message) {
  if (!ok) throw new Error(message);
}

function inside(root, candidate) {
  const comparable = (value) => {
    let path = resolve(value);
    if (process.platform === "win32") {
      if (path.startsWith("\\\\?\\UNC\\")) path = `\\\\${path.slice(8)}`;
      else if (path.startsWith("\\\\?\\")) path = path.slice(4);
      path = path.toLowerCase();
    }
    return path;
  };
  const path = relative(comparable(root), comparable(candidate));
  return (
    !path ||
    (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path))
  );
}

async function canonicalCandidate(path) {
  let cursor = resolve(path);
  const missing = [];
  for (;;) {
    try {
      return resolve(await realpath(cursor), ...missing.reverse());
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      const parent = dirname(cursor);
      assert(parent !== cursor, "State path cannot be resolved");
      missing.push(basename(cursor));
      cursor = parent;
    }
  }
}

function windowsPowerShell(script, extra = {}) {
  const args = ["-NoProfile", "-NonInteractive", "-Command", script];
  const options = {
    env: cleanEnv(extra),
    encoding: "utf8",
    windowsHide: true,
    timeout: 15000,
    maxBuffer: 8192,
  };
  // Windows PowerShell 5 startup can stall on hosted runners. PowerShell 7
  // requires no profile and is already installed there; retain the built-in
  // Windows PowerShell fallback for local machines without pwsh.
  const result = spawnSync("pwsh.exe", args, options);
  if (result.error?.code === "ENOENT") {
    return spawnSync("powershell.exe", args, options);
  }
  return result;
}

async function checkComponents(path) {
  let cursor = resolve(path);
  const existing = [];
  for (;;) {
    try {
      const stat = await lstat(cursor);
      assert(
        !stat.isSymbolicLink(),
        "State path contains a link or reparse point",
      );
      if (process.platform === "win32") existing.push(cursor);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    const parent = dirname(cursor);
    if (parent === cursor) break;
    cursor = parent;
  }
  if (process.platform === "win32") {
    // Windows path components cannot contain newlines. Probe every existing
    // component in one process with .NET attributes, avoiding slow per-path
    // PowerShell provider resolution while still rejecting all reparse tags.
    const result = windowsPowerShell(
      `try {
  foreach ($candidate in $env:GEO_PRIVATE_COMPONENTS -split [char]10) {
    if (([System.IO.File]::GetAttributes($candidate) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) { exit 2 }
  }
} catch { exit 3 }`,
      { GEO_PRIVATE_COMPONENTS: existing.join("\n") },
    );
    assert(
      result.error?.code !== "ETIMEDOUT",
      "Windows path attribute probe timed out",
    );
    assert(result.status === 0, "State path contains a link or reparse point");
  }
}

// Windows native ACL operations target a single newly created directory/file; never
// recursively applies ACLs to a preexisting shared path.
const aclScript = `
$ErrorActionPreference = 'Stop'
try {
function GetPrivateAcl($item) {
  if ($PSVersionTable.PSEdition -eq 'Core') { return [System.IO.FileSystemAclExtensions]::GetAccessControl($item) }
  return $item.GetAccessControl()
}
function SetPrivateAcl($item, $acl) {
  if ($PSVersionTable.PSEdition -eq 'Core') { [System.IO.FileSystemAclExtensions]::SetAccessControl($item, $acl) }
  else { $item.SetAccessControl($acl) }
}
$path = $env:GEO_PRIVATE_PATH
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$item = if ([System.IO.Directory]::Exists($path)) { [System.IO.DirectoryInfo]::new($path) } else { [System.IO.FileInfo]::new($path) }
$acl = GetPrivateAcl $item
$owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier])
if ($env:GEO_PRIVATE_ACTION -eq 'create') {
  # Elevated Windows processes may give their own freshly created items the
  # Administrators group as owner. Only the just-created path may be repaired.
  $administrators = [System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
  if ($owner.Value -ne $identity.User.Value -and $owner.Value -ne $administrators.Value) { exit 5 }
  if ($owner.Value -eq $administrators.Value) { $acl.SetOwner($identity.User) }
  $acl.SetAccessRuleProtection($true, $false)
  foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) { [void]$acl.RemoveAccessRuleSpecific($rule) }
  $rights = [System.Security.AccessControl.FileSystemRights]::FullControl
  $inherit = if ($item -is [System.IO.DirectoryInfo]) { [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [System.Security.AccessControl.InheritanceFlags]::ObjectInherit } else { [System.Security.AccessControl.InheritanceFlags]::None }
  $rule = [System.Security.AccessControl.FileSystemAccessRule]::new($identity.User, $rights, $inherit, [System.Security.AccessControl.PropagationFlags]::None, [System.Security.AccessControl.AccessControlType]::Allow)
  [void]$acl.SetAccessRule($rule)
  SetPrivateAcl $item $acl
  $acl = GetPrivateAcl $item
} elseif ($owner.Value -ne $identity.User.Value) {
  exit 5
}
if ($acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $identity.User.Value) { exit 5 }
if (-not $acl.AreAccessRulesProtected) { exit 2 }
$rules = $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])
foreach ($rule in $rules) {
  if ($rule.IsInherited -or $rule.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value -ne $identity.User.Value) { exit 3 }
}
if ($rules.Count -eq 0) { exit 4 }
} catch {
  [Console]::Error.WriteLine("$($_.Exception.GetType().Name):$($_.InvocationInfo.ScriptLineNumber)")
  exit 8
}
`;

async function privatePermissions(path, set = false) {
  if (process.platform === "win32") {
    const result = windowsPowerShell(aclScript, {
      GEO_PRIVATE_PATH: path,
      GEO_PRIVATE_ACTION: set ? "create" : "check",
    });
    assert(
      result.status === 0,
      result.error?.code === "ETIMEDOUT"
        ? "Windows private state ACL probe timed out"
        : `Private state ACL could not be verified (${result.status}; ${result.stderr.trim().slice(0, 80)})`,
    );
    return;
  }
  const stat = await lstat(path);
  assert((stat.mode & 0o077) === 0, "Private state permissions are too broad");
}

function newConfig() {
  const id = randomBytes(12).toString("hex");
  return {
    version: 1,
    id,
    operatorId: randomUUID(),
    tenantId: randomUUID(),
    userId: randomUUID(),
    loginName: `local-${id}@localhost`,
    password: randomBytes(36).toString("base64url"),
    runnerToken: randomBytes(36).toString("base64url"),
    cipherKey: randomBytes(32).toString("hex"),
    postgresPassword: randomBytes(36).toString("base64url"),
    container: `geo-local-${id}`,
    volume: `geo-local-${id}-pgdata`,
  };
}

function validConfig(value) {
  assert(
    value?.version === 1 && /^[a-f0-9]{24}$/.test(value.id),
    "Invalid private state",
  );
  assert(
    ["operatorId", "tenantId", "userId"].every((key) =>
      /^[0-9a-f-]{36}$/i.test(value[key]),
    ),
    "Invalid private state IDs",
  );
  assert(
    value.loginName === `local-${value.id}@localhost`,
    "Invalid private login",
  );
  assert(
    ["password", "runnerToken", "postgresPassword"].every(
      (key) =>
        typeof value[key] === "string" &&
        /^[A-Za-z0-9_-]{40,}$/.test(value[key]),
    ),
    "Invalid private secrets",
  );
  assert(
    /^[a-f0-9]{64}$/.test(value.cipherKey),
    "Invalid persistent channel key",
  );
  assert(
    value.container === `geo-local-${value.id}` &&
      value.volume === `geo-local-${value.id}-pgdata`,
    "Invalid resource names",
  );
  return value;
}

async function privateWrite(path, contents) {
  const handle = await open(path, "wx", 0o600);
  try {
    await handle.writeFile(contents);
  } finally {
    await handle.close();
  }
  if (process.platform === "win32") await privatePermissions(path, true);
  await privatePermissions(path);
}

export async function loadState(dir, repo = repository) {
  assert(
    typeof dir === "string" && isAbsolute(dir),
    "GEO_LOCAL_STATE_DIR must be absolute",
  );
  const state = resolve(dir);
  const requestedProject = resolve(repo);
  assert(
    !inside(requestedProject, state) && !inside(state, requestedProject),
    "State directory must be outside the repository",
  );
  const project = await realpath(requestedProject);
  const actualState = await canonicalCandidate(state);
  assert(
    !inside(project, actualState) && !inside(actualState, project),
    "State directory must be outside the repository",
  );
  await checkComponents(state);
  let exists = true;
  try {
    const stat = await lstat(state);
    assert(stat.isDirectory(), "State path is not a directory");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    exists = false;
  }
  if (!exists) {
    await mkdir(state, { mode: 0o700 });
    if (process.platform === "win32") await privatePermissions(state, true);
    await privatePermissions(state);
    const config = newConfig();
    await privateWrite(join(state, markerName), `${config.id}\n`);
    await privateWrite(join(state, configName), JSON.stringify(config));
    return config;
  }
  await privatePermissions(state);
  for (const name of [markerName, configName]) {
    const path = join(state, name);
    await checkComponents(path);
    assert(
      (await lstat(path)).isFile(),
      "Private state contains unexpected file type",
    );
    await privatePermissions(path);
  }
  const config = validConfig(
    JSON.parse(await readFile(join(state, configName), "utf8")),
  );
  assert(
    (await readFile(join(state, markerName), "utf8")) === `${config.id}\n`,
    "Private state ownership marker mismatch",
  );
  return config;
}

function cleanEnv(extra = {}) {
  const base = {};
  for (const name of [
    "HOME",
    "PATH",
    "Path",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "TMP",
    "TEMP",
  ]) {
    if (process.env[name] !== undefined) base[name] = process.env[name];
  }
  return {
    ...base,
    NO_PROXY: "localhost,127.0.0.1",
    no_proxy: "localhost,127.0.0.1",
    ...extra,
  };
}

function docker(args, extra = {}) {
  const result = spawnSync("docker", args, {
    env: cleanEnv(extra),
    encoding: "utf8",
    windowsHide: true,
    timeout: 30000,
    maxBuffer: 1024 * 1024,
  });
  assert(
    result.status === 0,
    "Docker precondition or owned resource operation failed",
  );
  return result.stdout.trim();
}

export function verifyDockerResource(config, resource, kind) {
  const labels = resource?.Labels ?? resource?.Config?.Labels;
  assert(labels?.[label] === config.id, "Refusing foreign Docker resource");
  if (kind === "container") {
    assert(
      resource.Name === `/${config.container}` &&
        resource.Config?.Image === image &&
        Object.keys(resource.HostConfig?.PortBindings ?? {}).length === 1 &&
        resource.HostConfig?.PortBindings?.["5432/tcp"]?.length === 1 &&
        resource.HostConfig?.PortBindings?.["5432/tcp"]?.[0]?.HostIp ===
          "127.0.0.1" &&
        resource.HostConfig.PortBindings["5432/tcp"][0].HostPort === "15432" &&
        resource.Mounts?.length === 1 &&
        resource.Mounts[0].Type === "volume" &&
        resource.Mounts[0].RW === true &&
        resource.Mounts[0].Name === config.volume &&
        resource.Mounts[0].Destination === "/var/lib/postgresql/data",
      "Refusing mismatched Docker container",
    );
  } else {
    assert(
      resource.Name === config.volume,
      "Refusing mismatched Docker volume",
    );
  }
}

async function portFree(port) {
  const server = createServer();
  try {
    await new Promise((yes, no) => {
      server.once("error", no);
      server.listen({ host: "127.0.0.1", port, exclusive: true }, yes);
    });
    return true;
  } catch (error) {
    if (["EADDRINUSE", "EACCES"].includes(error.code)) return false;
    throw error;
  } finally {
    if (server.listening) await new Promise((yes) => server.close(yes));
  }
}

async function postgres(config) {
  assert(
    /^(unix|npipe):\/\//.test(
      docker(["context", "inspect", "--format", "{{.Endpoints.docker.Host}}"]),
    ),
    "Docker must use a local engine",
  );
  docker(["info", "--format", "{{.ServerVersion}}"]);
  const volume = docker([
    "volume",
    "ls",
    "-q",
    "--filter",
    `name=^${config.volume}$`,
  ]);
  if (volume) {
    assert(volume === config.volume, "Ambiguous Docker volume");
    verifyDockerResource(
      config,
      JSON.parse(docker(["volume", "inspect", config.volume]))[0],
      "volume",
    );
  }
  const listed = docker([
    "ps",
    "-a",
    "--format",
    "{{.Names}}",
    "--filter",
    `name=^/${config.container}$`,
  ]);
  let inspected;
  if (listed) {
    assert(listed === config.container, "Ambiguous Docker container");
    inspected = JSON.parse(docker(["inspect", config.container]))[0];
    verifyDockerResource(config, inspected, "container");
  }
  assert(
    inspected?.State?.Running ? true : await portFree(15432),
    "PostgreSQL port is occupied",
  );
  if (!volume)
    docker([
      "volume",
      "create",
      "--label",
      `${label}=${config.id}`,
      config.volume,
    ]);
  if (!inspected) {
    docker(
      [
        "run",
        "-d",
        "--name",
        config.container,
        "--label",
        `${label}=${config.id}`,
        "-p",
        "127.0.0.1:15432:5432",
        "-v",
        `${config.volume}:/var/lib/postgresql/data`,
        "-e",
        "POSTGRES_PASSWORD",
        "-e",
        "POSTGRES_USER=geo_local",
        "-e",
        "POSTGRES_DB=geo_local",
        image,
      ],
      { POSTGRES_PASSWORD: config.postgresPassword },
    );
  } else if (!inspected.State.Running) {
    docker(["start", config.container]);
  }
  await until(async () => postgresTcpReady(config), 45000, "PostgreSQL");
}

export function postgresTcpReady(config, run = spawnSync) {
  // The image's first-run initialization starts a temporary Unix-socket-only
  // server. Its pg_isready success does not mean the application's TCP
  // connection is usable. Wait for the final TCP listener before bootstrap.
  const result = run(
    "docker",
    [
      "exec",
      config.container,
      "pg_isready",
      "-h",
      "127.0.0.1",
      "-p",
      "5432",
      "-U",
      "geo_local",
      "-d",
      "geo_local",
    ],
    {
      env: cleanEnv(),
      encoding: "utf8",
      windowsHide: true,
      timeout: 2000,
    },
  );
  return !result.error && result.status === 0;
}

async function until(check, ms, name, child) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    assert(!shutdownRequested, "Startup interrupted");
    assert(
      !child ||
        (child.exitCode === null &&
          child.signalCode === null &&
          !child.launchError),
      `${name} exited before readiness`,
    );
    if (await check()) return;
    await new Promise((yes) => setTimeout(yes, 350));
  }
  throw new Error(`${name} readiness timeout`);
}

function owned(command, args, cwd, env) {
  assert(!shutdownRequested, "Startup interrupted");
  const child = spawn(command, args, {
    cwd,
    env: cleanEnv(env),
    windowsHide: true,
    stdio: "ignore",
  });
  child.once("error", () => {
    child.launchError = true;
  });
  children.push(child);
  return child;
}

async function ready(url, child, name, headers = {}) {
  await until(
    async () => {
      try {
        return (
          await fetch(url, { headers, signal: AbortSignal.timeout(1200) })
        ).ok;
      } catch {
        return false;
      }
    },
    35000,
    name,
    child,
  );
}

async function stopChildren() {
  if (shuttingDown) return;
  shuttingDown = true;
  if (browser)
    await Promise.race([
      browser.close().catch(() => {}),
      new Promise((yes) => setTimeout(yes, 2500)),
    ]);
  if (browserServer) {
    await Promise.race([
      browserServer.close().catch(() => {}),
      new Promise((yes) => setTimeout(yes, 2500)),
    ]);
    // BrowserServer exposes the exact process we own, never a process-name lookup.
    const child = browserServer.process();
    if (child && child.exitCode === null && child.signalCode === null)
      child.kill("SIGTERM");
  }
  for (const child of [...children].reverse()) {
    if (child.exitCode !== null || child.signalCode !== null) continue;
    child.kill("SIGTERM");
    await Promise.race([
      new Promise((yes) => child.once("exit", yes)),
      new Promise((yes) => setTimeout(yes, 2500)),
    ]);
    if (child.exitCode === null && child.signalCode === null)
      child.kill("SIGKILL");
  }
}

export function requireSecureCookie(cookies) {
  assert(
    cookies.some(
      (cookie) =>
        cookie.name === "__Host-geo_session" &&
        cookie.secure &&
        cookie.httpOnly &&
        cookie.value,
    ),
    "Browser rejected the Secure session cookie; use a local HTTPS ingress, do not disable Secure cookies",
  );
}

async function firstPartyLogin(config, interactive) {
  assert(!shutdownRequested, "Startup interrupted");
  assert(
    process.env.PLAYWRIGHT_BROWSERS_PATH,
    "First-party login needs an installed pinned Chromium cache",
  );
  const require = createRequire(join(packageRoot, "package.json"));
  const { chromium } = require("playwright");
  browserServer = await chromium.launchServer({ headless: !interactive });
  assert(!shutdownRequested, "Startup interrupted");
  browser = await chromium.connect(browserServer.wsEndpoint(), {
    timeout: 10000,
  });
  const context = await browser.newContext({ serviceWorkers: "block" });
  const page = await context.newPage();
  await page.goto(`${webUrl}/login`, { timeout: 20000 });
  await page.getByRole("textbox", { name: "用户名" }).fill(config.loginName);
  await page.getByRole("textbox", { name: "密码" }).fill(config.password);
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await page
    .getByRole("heading", { name: "选择客户工作区" })
    .waitFor({ timeout: 20000 });
  const cookies = await context.cookies(webUrl);
  requireSecureCookie(cookies);
  const authenticated = await page.request.get(
    `${webUrl}/api/v1/auth/session`,
    {
      timeout: 10000,
    },
  );
  assert(authenticated.ok(), "First-party browser session was not accepted");
  // No remote navigation, tracing, screenshots, storageState export, or request logging.
  if (interactive) {
    console.log(
      "Local application login ready. Connect external accounts yourself in the UI.",
    );
  } else {
    await browser.close();
    browser = undefined;
    await browserServer.close();
    browserServer = undefined;
  }
}

async function main() {
  const flags = process.argv.slice(2);
  assert(
    flags.every((flag) => ["--interactive", "--check"].includes(flag)) &&
      new Set(flags).size === flags.length,
    "Usage: node scripts/start-local-workspace.mjs [--interactive] [--check]",
  );
  const binary = process.env.GEO_LOCAL_APP_BINARY;
  assert(
    binary && isAbsolute(binary) && (await lstat(binary)).isFile(),
    "GEO_LOCAL_APP_BINARY must name a built absolute binary",
  );
  assert(await realpath(binary), "Application binary unavailable");
  assert(
    process.env.GEO_LOCAL_STATE_DIR,
    "GEO_LOCAL_STATE_DIR must be specified outside the checkout",
  );
  startupStage = "private configuration";
  const config = await loadState(process.env.GEO_LOCAL_STATE_DIR);
  const scratch = process.env.GEO_LOCAL_TMP_DIR;
  if (scratch) {
    assert(
      isAbsolute(scratch) && !inside(repository, resolve(scratch)),
      "Temporary directory must be outside checkout",
    );
    await checkComponents(scratch);
  }
  for (const port of [8080, 5173, 38080]) {
    assert(await portFree(port), `Local port ${port} must be free`);
  }
  assert(!shutdownRequested, "Startup interrupted");
  startupStage = "local PostgreSQL";
  await postgres(config);
  assert(!shutdownRequested, "Startup interrupted");
  const environment = {
    ...(scratch ? { TMP: scratch, TEMP: scratch } : {}),
    ...(process.env.PLAYWRIGHT_BROWSERS_PATH
      ? { PLAYWRIGHT_BROWSERS_PATH: process.env.PLAYWRIGHT_BROWSERS_PATH }
      : {}),
    DATABASE_URL: `postgres://geo_local:${encodeURIComponent(config.postgresPassword)}@127.0.0.1:15432/geo_local`,
  };
  const apiEnvironment = {
    ...environment,
    GEO_BIND_ADDR: "127.0.0.1:8080",
    GEO_ALLOWED_ORIGINS: "http://localhost:5173,http://127.0.0.1:5173",
    GEO_BROWSER_RUNNER_URL: runnerUrl,
    GEO_BROWSER_RUNNER_TOKEN: config.runnerToken,
    GEO_CHANNEL_SECRET_KEY: config.cipherKey,
  };
  const bootstrapEnvironment = {
    ...environment,
    GEO_BOOTSTRAP_OPERATOR_ID: config.operatorId,
    GEO_BOOTSTRAP_OPERATOR_SLUG: "local-workspace",
    GEO_BOOTSTRAP_OPERATOR_NAME: "Local workspace",
    GEO_BOOTSTRAP_HOST: "localhost:5173",
    GEO_BOOTSTRAP_TENANT_ID: config.tenantId,
    GEO_BOOTSTRAP_TENANT_SLUG: "local-workspace",
    GEO_BOOTSTRAP_TENANT_NAME: "Local workspace",
    GEO_BOOTSTRAP_USER_ID: config.userId,
    GEO_BOOTSTRAP_LOGIN_NAME: config.loginName,
    GEO_BOOTSTRAP_USER_NAME: "Local administrator",
    GEO_BOOTSTRAP_PASSWORD: config.password,
    GEO_BOOTSTRAP_ROLE: "customer_admin",
  };
  startupStage = "identity bootstrap";
  const bootstrap = spawnSync(binary, ["--bootstrap"], {
    cwd: repository,
    env: cleanEnv(bootstrapEnvironment),
    encoding: "utf8",
    windowsHide: true,
    timeout: 60000,
    maxBuffer: 1024 * 1024,
  });
  assert(bootstrap.status === 0, "Local identity bootstrap failed");
  startupStage = "browser runner";
  const runner = owned(
    process.execPath,
    [join(packageRoot, "src", "server.mjs")],
    repository,
    {
      GEO_BROWSER_RUNNER_HOST: "127.0.0.1",
      GEO_BROWSER_RUNNER_PORT: "38080",
      GEO_BROWSER_RUNNER_TOKEN: config.runnerToken,
      ...(process.env.PLAYWRIGHT_BROWSERS_PATH
        ? { PLAYWRIGHT_BROWSERS_PATH: process.env.PLAYWRIGHT_BROWSERS_PATH }
        : {}),
      ...(scratch ? { TMP: scratch, TEMP: scratch } : {}),
    },
  );
  await ready(`${runnerUrl}/v1/capabilities`, runner, "Browser runner", {
    Authorization: `Bearer ${config.runnerToken}`,
  });
  startupStage = "Rust API";
  const api = owned(binary, [], repository, apiEnvironment);
  await ready(`${apiUrl}/health/ready`, api, "Rust API");
  // Vite's Windows .cmd shim requires a command interpreter; use direct Node CLI instead.
  const viteCli = join(repository, "node_modules", "vite", "bin", "vite.js");
  const installedCli = (await lstat(viteCli).catch(() => null))?.isFile()
    ? viteCli
    : join(webRoot, "node_modules", "vite", "bin", "vite.js");
  assert(
    (await lstat(installedCli).catch(() => null))?.isFile(),
    "Install workspace dependencies before starting Vite",
  );
  startupStage = "frontend";
  const viteChild = owned(
    process.execPath,
    [installedCli, "--host", "127.0.0.1", "--port", "5173", "--strictPort"],
    webRoot,
    { VITE_DEV_API_PROXY_TARGET: apiUrl },
  );
  await ready(`${webUrl}/login`, viteChild, "Vite");
  startupStage = "first-party browser login";
  await firstPartyLogin(config, flags.includes("--interactive"));
  startupStage = "service supervision";
  console.log(`Local workspace ready: ${webUrl}`);
  if (flags.includes("--check")) return;
  await Promise.race([
    stopSignal,
    ...children.map(
      (child) =>
        new Promise((_, no) =>
          child.once("exit", () => no(new Error("Owned service exited"))),
        ),
    ),
  ]);
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  process.on("SIGINT", () => {
    shutdownRequested = true;
    resolveStopSignal();
  });
  process.on("SIGTERM", () => {
    shutdownRequested = true;
    resolveStopSignal();
  });
  main()
    .catch((error) => {
      if (process.connected) {
        process.send(
          { type: "local-workspace-failure", stage: startupStage },
          () => {},
        );
      }
      if (
        error?.message?.startsWith("Browser rejected the Secure session cookie")
      ) {
        console.error(
          "Chromium rejected the Secure local cookie; configure HTTPS ingress without weakening cookies.",
        );
      } else {
        console.error(
          `Local workspace failed during ${startupStage}; check prerequisites and private state permissions.`,
        );
      }
      process.exitCode = 1;
    })
    .finally(() => stopChildren());
}
