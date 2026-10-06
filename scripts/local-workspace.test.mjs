import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
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
  loadState,
  requireSecureCookie,
  verifyDockerResource,
} from "./start-local-workspace.mjs";

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
    const powershell = (path, command) =>
      spawnSync(
        "powershell.exe",
        ["-NoProfile", "-NonInteractive", "-Command", command],
        {
          env: { ...process.env, GEO_PRIVATE_PATH: path },
          encoding: "utf8",
          windowsHide: true,
          timeout: 15000,
          maxBuffer: 8192,
        },
      );
    const ownership = (path) =>
      powershell(
        path,
        '$path=$env:GEO_PRIVATE_PATH; $item=if ([System.IO.Directory]::Exists($path)) { [System.IO.DirectoryInfo]::new($path) } else { [System.IO.FileInfo]::new($path) }; $owner=$item.GetAccessControl().GetOwner([System.Security.Principal.SecurityIdentifier]); $me=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; if ($owner.Value -eq $me.Value) { "owned" } else { "foreign" }',
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
        "$item=[System.IO.FileInfo]::new($env:GEO_PRIVATE_PATH); $acl=$item.GetAccessControl(); $acl.SetOwner([System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')); try { $item.SetAccessControl($acl); exit 0 } catch { exit 8 }",
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
