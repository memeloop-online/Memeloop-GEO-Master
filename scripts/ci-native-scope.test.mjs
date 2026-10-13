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

test("only explicit browser JavaScript and Markdown paths may skip native checks", () => {
  const paths = [
    "packages/browser-runner/src/index.mjs",
    "packages/browser-runner/src/nested/helper.mjs",
    "packages/browser-runner/test/capture.test.mjs",
    "docs/nested/guide.md",
    "docs/guide with spaces.md",
    "README.md",
    "CONTRIBUTING.md",
  ];
  assert.equal(nativeRequired({ ...pr, paths }), false);
  for (const path of [
    "packages/browser-runner/src/extraction-prompt.txt",
    "packages/browser-runner/test/fixture.json",
    "packages/browser-runner/package.json",
    "packages/browser-runner/Dockerfile",
    "packages/agent-runtime/src/index.mjs",
    "crates/api/src/lib.rs",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "pnpm-lock.yaml",
    "apps/web/src/main.tsx",
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
      nativeRequired({ ...pr, eventName, paths: ["README.md"] }),
      true,
    );
    assert.equal(determineNativeScope({ eventName, event: {} }), true);
  }
  for (const baseRef of ["release", "", undefined]) {
    assert.equal(
      nativeRequired({ ...pr, baseRef, paths: ["README.md"] }),
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
