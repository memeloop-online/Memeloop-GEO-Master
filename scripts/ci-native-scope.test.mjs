import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { determineNativeScope, nativeRequired } from "./ci-native-scope.mjs";

const pr = { eventName: "pull_request", baseRef: "main" };

test("only explicit web/browser source and Markdown paths may skip native checks", () => {
  const paths = [
    "apps/web/src/main.tsx",
    "apps/web/src/i18n/messages.ts",
    "apps/web/src/components/example.test.tsx",
    "apps/web/src/nested/helper.test.ts",
    "apps/web/src/styles.css",
    "apps/web/src/components/example.module.css",
    "packages/browser-runner/src/index.mjs",
    "packages/browser-runner/src/nested/helper.mjs",
    "packages/browser-runner/test/capture.test.mjs",
    "docs/nested/guide.md",
    "docs/guide with spaces.md",
    "README.md",
    "CONTRIBUTING.md",
  ];
  assert.equal(nativeRequired({ ...pr, paths }), false);
  for (const path of paths) {
    assert.equal(nativeRequired({ ...pr, paths: [path] }), false, path);
  }
  for (const path of [
    "packages/browser-runner/src/extraction-prompt.txt",
    "packages/browser-runner/test/fixture.json",
    "packages/browser-runner/package.json",
    "packages/browser-runner/Dockerfile",
    "packages/agent-runtime/src/index.mjs",
    "crates/api/src/lib.rs",
    "crates/worker/src/runtime.rs",
    "crates/worker/src/timers.js",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    "package.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "apps/web/package.json",
    "apps/web/vite.config.ts",
    "apps/web/tsconfig.app.json",
    "apps/web/styles.css",
    "apps/web/public/styles.css",
    "apps/web/src/main.tsx.bak",
    "apps/web/src/main.ts.txt",
    "apps/web/src/main.TSX",
    "apps/web/src/main.js",
    "apps/web/src/styles.css.bak",
    "apps/web/src/styles.css.scss",
    "apps/web/src/styles.CSS",
    "apps/web/src/styles.scss",
    "apps/web/src/prompt.txt",
    "apps/web/src2/main.tsx",
    "apps/web/src2/styles.css",
    "apps/web/src/.tsx",
    "apps/web/src/.css",
    "apps/web/src/main.tsx\nCargo.lock",
    "apps/web/src/styles.css\nCargo.lock",
    "apps/web/src/main.tsx/other",
    "apps/web/src/styles.css/other",
    "scripts/bundle-agent.mjs",
    ".github/workflows/ci.yml",
    "scripts/ci-native-scope.mjs",
    "scripts/ci-native-scope.test.mjs",
    "docs/image.png",
    "docs/.md",
    "README.md.bak",
    "README.md\nCargo.lock",
    "unknown",
    "",
    null,
  ]) {
    assert.equal(nativeRequired({ ...pr, paths: [...paths, path] }), true);
  }
});

test("empty and unsupported event contexts require native checks", () => {
  assert.equal(nativeRequired(), true);
  assert.equal(nativeRequired({ ...pr, paths: [] }), true);
  assert.equal(nativeRequired(pr), true);
  for (const eventName of [
    "push",
    "workflow_dispatch",
    "pull_request_target",
    "",
  ]) {
    assert.equal(
      nativeRequired({
        ...pr,
        eventName,
        paths: ["README.md", "apps/web/src/styles.css"],
      }),
      true,
    );
    assert.equal(determineNativeScope({ eventName, event: {} }), true);
  }
  for (const baseRef of ["release", "", undefined]) {
    assert.equal(
      nativeRequired({
        ...pr,
        baseRef,
        paths: ["README.md", "apps/web/src/styles.css"],
      }),
      true,
    );
    assert.equal(
      determineNativeScope({
        eventName: "pull_request",
        event: { pull_request: { base: { ref: baseRef } } },
      }),
      true,
    );
  }
});

function repository(t) {
  const root = resolve(process.env.GEO_CI_SCOPE_TEST_TMP_DIR || tmpdir());
  const cwd = mkdtempSync(join(root, "geo-ci-native-scope-"));
  t.after(() => {
    // Delete only the exact temporary child created by this test.
    assert.equal(dirname(resolve(cwd)), root);
    assert.ok(cwd.startsWith(join(root, "geo-ci-native-scope-")));
    rmSync(cwd, { recursive: true, force: true });
  });
  const git = (...args) =>
    execFileSync("git", args, {
      cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  git("init", "--quiet");
  const write = (path, text) => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), text);
  };
  const commit = () => {
    git("add", "--all");
    git(
      "-c",
      "user.name=CI fixture",
      "-c",
      "user.email=ci@example.invalid",
      "-c",
      "commit.gpgsign=false",
      "commit",
      "--quiet",
      "-m",
      "fixture",
    );
    return git("rev-parse", "HEAD");
  };
  const scope = (base, head) =>
    determineNativeScope({
      eventName: "pull_request",
      event: {
        pull_request: { base: { ref: "main", sha: base }, head: { sha: head } },
      },
      cwd,
    });
  return { cwd, git, write, commit, scope };
}

test("whole PR retains an earlier native change followed by a safe commit", (t) => {
  const repo = repository(t);
  repo.write("README.md", "base");
  const base = repo.commit();
  repo.write("crates/example/src/lib.rs", "native");
  const native = repo.commit();
  repo.write("docs/guide with spaces.md", "safe");
  const head = repo.commit();
  assert.equal(repo.scope(native, head), false);
  assert.equal(repo.scope(base, head), true);
  assert.equal(repo.scope(head, head), true);
});

test("native-to-docs rename cannot hide the removed native input", (t) => {
  const repo = repository(t);
  repo.write("crates/example/src/lib.rs", "identical content");
  const base = repo.commit();
  mkdirSync(join(repo.cwd, "docs"));
  renameSync(
    join(repo.cwd, "crates/example/src/lib.rs"),
    join(repo.cwd, "docs/moved.md"),
  );
  const head = repo.commit();
  assert.match(
    repo.git("diff", "--name-status", "--find-renames", base, head),
    /^R100/,
  );
  assert.equal(repo.scope(base, head), true);
});

test("frontend TS/TSX/CSS additions, edits, and deletions may skip native checks", (t) => {
  const repo = repository(t);
  repo.write("apps/web/src/old.tsx", "base");
  repo.write("apps/web/src/styles.css", "base");
  const base = repo.commit();
  repo.write("apps/web/src/old.tsx", "edited");
  repo.write("apps/web/src/styles.css", "edited");
  repo.write("apps/web/src/nested/helper.ts", "added");
  repo.write("apps/web/src/nested/component.css", "added");
  const edited = repo.commit();
  assert.equal(repo.scope(base, edited), false);
  repo.git("rm", "apps/web/src/old.tsx");
  repo.git("rm", "apps/web/src/styles.css");
  const deleted = repo.commit();
  assert.equal(repo.scope(edited, deleted), false);
  assert.equal(repo.scope(base, deleted), false);
});

test("CSS cannot hide native or dependency inputs earlier in the PR", (t) => {
  const repo = repository(t);
  repo.write("apps/web/src/styles.css", "base");
  const base = repo.commit();
  repo.write("Cargo.lock", "changed");
  const native = repo.commit();
  repo.write("apps/web/src/styles.css", "edited");
  const head = repo.commit();
  assert.equal(repo.scope(native, head), false);
  assert.equal(repo.scope(base, head), true);
});

for (const [from, to] of [
  ["crates/example/src/lib.rs", "apps/web/src/moved.css"],
  ["apps/web/src/styles.css", "apps/web/styles.css"],
]) {
  test(`rename across the CSS allowlist boundary requires native checks: ${from} -> ${to}`, (t) => {
    const repo = repository(t);
    repo.write(from, "identical content");
    const base = repo.commit();
    mkdirSync(dirname(join(repo.cwd, to)), { recursive: true });
    renameSync(join(repo.cwd, from), join(repo.cwd, to));
    const head = repo.commit();
    assert.match(
      repo.git("diff", "--name-status", "--find-renames", base, head),
      /^R100/,
    );
    assert.equal(repo.scope(base, head), true);
  });
}

test("native-to-frontend rename cannot hide the removed native input", (t) => {
  const repo = repository(t);
  repo.write("crates/example/src/lib.rs", "identical content");
  const base = repo.commit();
  mkdirSync(join(repo.cwd, "apps/web/src"), { recursive: true });
  renameSync(
    join(repo.cwd, "crates/example/src/lib.rs"),
    join(repo.cwd, "apps/web/src/moved.tsx"),
  );
  const head = repo.commit();
  assert.match(
    repo.git("diff", "--name-status", "--find-renames", base, head),
    /^R100/,
  );
  assert.equal(repo.scope(base, head), true);
});

test("missing or unavailable SHAs fail instead of producing a successful skip", (t) => {
  const repo = repository(t);
  repo.write("README.md", "base");
  const base = repo.commit();
  assert.throws(
    () => repo.scope(undefined, base),
    /complete pull request commit SHAs/,
  );
  assert.throws(() => repo.scope(base, "f".repeat(40)));
});
