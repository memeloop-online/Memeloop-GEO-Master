import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  mkdtemp,
  mkdir,
  readFile,
  rm,
  symlink,
  unlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  launcherFailureStage,
  loadState,
  postgresTcpReady,
  probeBrowserRunner,
  requireSecureCookie,
  selectBrowserRunner,
  startBrowserRunner,
  verifyDockerResource,
} from "./start-local-workspace.mjs";

test("runner selection requires a paired credential-free backend-compatible origin", () => {
  const config = { runnerToken: "synthetic-persistent-local-token" };
  assert.deepEqual(selectBrowserRunner(config, {}), {
    external: false,
    url: "http://127.0.0.1:38080",
    token: config.runnerToken,
  });
  assert.deepEqual(
    selectBrowserRunner(config, {
      GEO_BROWSER_RUNNER_URL: "https://runner.example.invalid:8443/",
      GEO_BROWSER_RUNNER_TOKEN: "synthetic-external-token",
    }),
    {
      external: true,
      url: "https://runner.example.invalid:8443",
      token: "synthetic-external-token",
    },
  );
  for (const overrides of [
    { GEO_BROWSER_RUNNER_URL: "https://runner.example.invalid" },
    { GEO_BROWSER_RUNNER_TOKEN: "synthetic-external-token" },
    {
      GEO_BROWSER_RUNNER_URL: "https://runner.example.invalid",
      GEO_BROWSER_RUNNER_TOKEN: "",
    },
    {
      GEO_BROWSER_RUNNER_URL: "",
      GEO_BROWSER_RUNNER_TOKEN: "synthetic-external-token",
    },
    ...[
      "ftp://runner.example.invalid",
      "https://user:private-password@runner.example.invalid",
      "https://runner.example.invalid/v1",
      "https://runner.example.invalid?private-query",
      "https://runner.example.invalid#private-fragment",
      "https://runner.example.invalid/ path",
      "not-an-origin",
    ].map((url) => ({
      GEO_BROWSER_RUNNER_URL: url,
      GEO_BROWSER_RUNNER_TOKEN: "synthetic-external-token",
    })),
  ]) {
    assert.throws(
      () => selectBrowserRunner(config, overrides),
      (error) =>
        !/private-password|private-query|private-fragment|synthetic-external-token/.test(
          error.message,
        ),
    );
  }
});

test("runner probe authenticates capabilities and never follows redirects", async () => {
  const requests = [];
  const request = async (url, options) => {
    requests.push({ url, options });
    return { status: 200, json: async () => ({ connectors: [] }) };
  };
  assert.equal(
    await probeBrowserRunner(
      "https://runner.example.invalid",
      "synthetic-external-token",
      request,
    ),
    true,
  );
  assert.equal(
    requests[0].url,
    "https://runner.example.invalid/v1/capabilities",
  );
  assert.equal(requests[0].options.redirect, "manual");
  assert.equal(
    requests[0].options.headers.Authorization,
    "Bearer synthetic-external-token",
  );
  for (const response of [
    { status: 401 },
    { status: 302 },
    { status: 200, json: async () => ({}) },
    { status: 200, json: async () => "unrelated page" },
  ]) {
    assert.equal(
      await probeBrowserRunner(
        "https://runner.example.invalid",
        "token",
        async () => response,
      ),
      false,
    );
  }
  assert.equal(
    await probeBrowserRunner(
      "https://runner.example.invalid",
      "token",
      async () => {
        throw new Error("synthetic network outage");
      },
    ),
    false,
  );
});

test("external runner remains unowned; local runner is launched and checked", async () => {
  const launched = [];
  const checked = [];
  const ownedChild = { exitCode: null, signalCode: null };
  const launch = (...args) => {
    launched.push(args);
    return ownedChild;
  };
  const probe = async (url, token) => {
    checked.push([url, token]);
    return true;
  };
  const external = selectBrowserRunner(
    { runnerToken: "persistent-local-token" },
    {
      GEO_BROWSER_RUNNER_URL: "http://runner.example.invalid:38080",
      GEO_BROWSER_RUNNER_TOKEN: "external-token",
    },
  );
  assert.equal(
    await startBrowserRunner(external, { launch, probe }),
    undefined,
  );
  assert.equal(launched.length, 0);
  assert.deepEqual(checked, [[external.url, external.token]]);
  await assert.rejects(
    startBrowserRunner(external, {
      launch,
      probe: async () => false,
    }),
    /capabilities unavailable/,
  );
  assert.equal(launched.length, 0);
  const local = selectBrowserRunner(
    { runnerToken: "persistent-local-token" },
    {},
  );
  const child = await startBrowserRunner(local, { launch, probe });
  assert.equal(child, ownedChild);
  assert.equal(launched.length, 1);
  assert.equal(launched[0][3].GEO_BROWSER_RUNNER_TOKEN, local.token);
  assert.deepEqual(checked.at(-1), [local.url, local.token]);
});

test("PostgreSQL readiness rejects the temporary socket-only initialization server", () => {
  let tcpReady = false;
  const probe = (command, args, options) => {
    assert.equal(command, "docker");
    assert.equal(options.timeout, 2000);
    assert.deepEqual(args, [
      "exec",
      "synthetic-owned-container",
      "pg_isready",
      "-h",
      "127.0.0.1",
      "-p",
      "5432",
      "-U",
      "geo_local",
      "-d",
      "geo_local",
    ]);
    return { status: args.includes("-h") && !tcpReady ? 2 : 0 };
  };
  const config = { container: "synthetic-owned-container" };
  assert.equal(postgresTcpReady(config, probe), false);
  tcpReady = true;
  assert.equal(postgresTcpReady(config, probe), true);
  assert.equal(
    postgresTcpReady(config, () => ({ status: null })),
    false,
  );
  assert.equal(
    postgresTcpReady(config, () => ({
      status: 0,
      error: new Error("timeout"),
    })),
    false,
  );
});

test("launcher diagnostics accept fixed failure stages only", () => {
  assert.equal(
    launcherFailureStage({
      type: "local-workspace-failure",
      stage: "identity bootstrap",
      details: "synthetic-private-data",
    }),
    "identity bootstrap",
  );
  for (const message of [
    null,
    "synthetic-private-data",
    { type: "other", stage: "identity bootstrap" },
    { type: "local-workspace-failure", stage: "synthetic-private-data" },
    { type: "local-workspace-failure", stage: "identity bootstrap\nprivate" },
    { type: "local-workspace-failure", stage: {} },
  ]) {
    assert.equal(launcherFailureStage(message), undefined);
  }
});

test("failed launcher delivers a safe stage over IPC with private output suppressed", async () => {
  const child = spawn(
    process.execPath,
    [join(import.meta.dirname, "start-local-workspace.mjs"), "--check"],
    {
      env: {
        ...process.env,
        GEO_LOCAL_APP_BINARY: "deliberately-invalid-private-path",
        DATABASE_URL: "postgres://private-password@private-host/db",
      },
      windowsHide: true,
      stdio: ["ignore", "ignore", "ignore", "ipc"],
    },
  );
  const messages = [];
  child.on("message", (message) => messages.push(message));
  const timeout = setTimeout(() => child.kill(), 5000);
  try {
    const code = await new Promise((done, fail) => {
      child.once("error", fail);
      child.once("close", done);
    });
    assert.equal(code, 1);
    assert.deepEqual(messages, [
      { type: "local-workspace-failure", stage: "prerequisites" },
    ]);
    assert.equal(launcherFailureStage(messages[0]), "prerequisites");
  } finally {
    clearTimeout(timeout);
  }
});

test("new private state persists identity, database password and encryption key", async () => {
  const parent = await mkdtemp(join(tmpdir(), "geo-private-check-"));
  try {
    const state = join(parent, "owned");
    const first = await loadState(state);
    assert.equal(first.cipherKey.length, 64);
    assert.equal(first.password.length > 40, true);
    assert.deepEqual(await loadState(state), first);
    assert.deepEqual(
      JSON.parse(await readFile(join(state, "config.json"), "utf8")),
      first,
    );
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
});

test("refuses checkout and foreign/preexisting state without rewriting it", async () => {
  const parent = await mkdtemp(join(tmpdir(), "geo-private-check-"));
  try {
    const repo = join(parent, "repository");
    await mkdir(repo);
    await assert.rejects(loadState(join(repo, "secrets"), repo), /outside/);
    if (process.platform === "win32") {
      await assert.rejects(
        loadState(join(`\\\\?\\${repo}`, "secrets"), repo),
        /outside/,
      );
    }
    const foreign = join(parent, "foreign");
    await mkdir(foreign);
    await writeFile(join(foreign, "other-data"), "untouched");
    await assert.rejects(loadState(foreign, repo));
    assert.equal(
      await readFile(join(foreign, "other-data"), "utf8"),
      "untouched",
    );
    const owned = join(parent, "owned");
    const config = await loadState(owned, repo);
    await writeFile(join(owned, ".geo-local-owned"), `${randomUUID()}\n`);
    await assert.rejects(loadState(owned, repo), /marker mismatch/);
    assert.equal(
      (await readFile(join(owned, "config.json"), "utf8")).includes(
        config.cipherKey,
      ),
      true,
    );
    if (process.platform !== "win32") {
      const link = join(parent, "linked");
      await symlink(owned, link);
      await assert.rejects(loadState(link, repo), /link/);
      await writeFile(join(parent, "foreign-config"), "not a trusted config");
      await unlink(join(owned, "config.json"));
      await symlink(join(parent, "foreign-config"), join(owned, "config.json"));
      await assert.rejects(loadState(owned, repo), /link/);
    }
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
});

test(
  "Windows creation owns private state; existing foreign owner is never repaired",
  { skip: process.platform !== "win32" },
  async (t) => {
    const parent = await mkdtemp(join(tmpdir(), "geo-private-check-"));
    const powershell = (path, command) => {
      const args = ["-NoProfile", "-NonInteractive", "-Command", command];
      const options = {
        env: { ...process.env, GEO_PRIVATE_PATH: path },
        encoding: "utf8",
        windowsHide: true,
        timeout: 15000,
        maxBuffer: 8192,
      };
      const result = spawnSync("pwsh.exe", args, options);
      return result.error?.code === "ENOENT"
        ? spawnSync("powershell.exe", args, options)
        : result;
    };
    const ownership = (path) =>
      powershell(
        path,
        '$path=$env:GEO_PRIVATE_PATH; $item=if ([System.IO.Directory]::Exists($path)) { [System.IO.DirectoryInfo]::new($path) } else { [System.IO.FileInfo]::new($path) }; $acl=if ($PSVersionTable.PSEdition -eq "Core") { [System.IO.FileSystemAclExtensions]::GetAccessControl($item) } else { $item.GetAccessControl() }; $owner=$acl.GetOwner([System.Security.Principal.SecurityIdentifier]); $me=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; if ($owner.Value -eq $me.Value) { "owned" } else { "foreign" }',
      ).stdout.trim();
    try {
      const state = join(parent, "owned");
      await loadState(state);
      for (const path of [
        state,
        join(state, ".geo-local-owned"),
        join(state, "config.json"),
      ]) {
        assert.equal(ownership(path), "owned");
      }
      const marker = join(state, ".geo-local-owned");
      const changed = powershell(
        marker,
        "$item=[System.IO.FileInfo]::new($env:GEO_PRIVATE_PATH); $acl=if ($PSVersionTable.PSEdition -eq 'Core') { [System.IO.FileSystemAclExtensions]::GetAccessControl($item) } else { $item.GetAccessControl() }; $acl.SetOwner([System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')); try { if ($PSVersionTable.PSEdition -eq 'Core') { [System.IO.FileSystemAclExtensions]::SetAccessControl($item, $acl) } else { $item.SetAccessControl($acl) }; exit 0 } catch { exit 8 }",
      );
      if (changed.status !== 0) {
        t.skip("Changing a synthetic fixture owner requires elevated Windows");
        return;
      }
      assert.equal(ownership(marker), "foreign");
      await assert.rejects(loadState(state), /Private state ACL/);
      assert.equal(ownership(marker), "foreign");
    } finally {
      await rm(parent, { recursive: true, force: true });
    }
  },
);

test("foreign Docker volume/container and changed binding never pass inspection", () => {
  const config = {
    id: "a".repeat(24),
    container: `geo-local-${"a".repeat(24)}`,
    volume: `geo-local-${"a".repeat(24)}-pgdata`,
  };
  const volume = {
    Name: config.volume,
    Labels: { "org.memeloop-geo.local-workspace": config.id },
  };
  verifyDockerResource(config, volume, "volume");
  assert.throws(
    () => verifyDockerResource(config, { ...volume, Labels: {} }, "volume"),
    /foreign/,
  );
  const container = {
    Name: `/${config.container}`,
    Config: { Image: "postgres:17-alpine", Labels: volume.Labels },
    HostConfig: {
      PortBindings: {
        "5432/tcp": [{ HostIp: "127.0.0.1", HostPort: "15432" }],
      },
    },
    Mounts: [
      {
        Type: "volume",
        RW: true,
        Name: config.volume,
        Destination: "/var/lib/postgresql/data",
      },
    ],
  };
  verifyDockerResource(config, container, "container");
  assert.throws(
    () =>
      verifyDockerResource(
        config,
        {
          ...container,
          HostConfig: {
            PortBindings: {
              "5432/tcp": [{ HostIp: "0.0.0.0", HostPort: "15432" }],
            },
          },
        },
        "container",
      ),
    /mismatched/,
  );
  assert.throws(
    () =>
      verifyDockerResource(
        config,
        {
          ...container,
          HostConfig: {
            PortBindings: {
              "5432/tcp": [
                { HostIp: "127.0.0.1", HostPort: "15432" },
                { HostIp: "0.0.0.0", HostPort: "15432" },
              ],
            },
          },
        },
        "container",
      ),
    /mismatched/,
  );
  assert.throws(
    () =>
      verifyDockerResource(
        config,
        { ...container, Mounts: [...container.Mounts, { Name: "other" }] },
        "container",
      ),
    /mismatched/,
  );
});

test("startup error output never includes a supplied secret or machine path", async () => {
  const { spawnSync } = await import("node:child_process");
  const result = spawnSync(
    process.execPath,
    [join(import.meta.dirname, "start-local-workspace.mjs")],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        GEO_LOCAL_APP_BINARY: "deliberately-invalid-private-path",
        DATABASE_URL: "postgres://private-password@private-host/db",
      },
    },
  );
  assert.notEqual(result.status, 0);
  assert.doesNotMatch(
    result.stderr,
    /private-password|private-host|deliberately-invalid-private-path/,
  );
});

test("invalid external runner configuration never logs its URL or bearer token", async () => {
  const parent = await mkdtemp(join(tmpdir(), "geo-private-check-"));
  try {
    const result = spawnSync(
      process.execPath,
      [join(import.meta.dirname, "start-local-workspace.mjs"), "--check"],
      {
        encoding: "utf8",
        timeout: 20000,
        env: {
          ...process.env,
          GEO_LOCAL_APP_BINARY: process.execPath,
          GEO_LOCAL_STATE_DIR: join(parent, "state"),
          GEO_BROWSER_RUNNER_URL:
            "https://private-user:private-password@runner.example.invalid",
          GEO_BROWSER_RUNNER_TOKEN: "synthetic-private-bearer",
        },
      },
    );
    assert.equal(result.status, 1);
    assert.doesNotMatch(
      `${result.stdout}${result.stderr}`,
      /private-user|private-password|runner\.example\.invalid|synthetic-private-bearer/,
    );
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
});

test("browser authentication requires the persistent Secure HttpOnly session cookie", () => {
  const cookie = {
    name: "__Host-geo_session",
    value: "synthetic-cookie",
    secure: true,
    httpOnly: true,
  };
  requireSecureCookie([cookie]);
  assert.throws(
    () => requireSecureCookie([{ ...cookie, secure: false }]),
    /HTTPS/,
  );
  assert.throws(
    () => requireSecureCookie([{ ...cookie, httpOnly: false }]),
    /HTTPS/,
  );
  assert.throws(() => requireSecureCookie([]), /HTTPS/);
});
