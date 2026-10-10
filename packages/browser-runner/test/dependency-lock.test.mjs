import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

test("the standalone image lock matches the runner runtime dependencies", async () => {
  const manifest = JSON.parse(
    await readFile(new URL("../package.json", import.meta.url), "utf8"),
  );
  const lock = JSON.parse(
    await readFile(new URL("../package-lock.json", import.meta.url), "utf8"),
  );
  assert.deepEqual(
    lock.packages[""].dependencies,
    manifest.dependencies,
    "Update the runner npm lock as well as the workspace lock when adding dependencies",
  );
  for (const [name, version] of Object.entries(manifest.dependencies)) {
    assert.equal(
      lock.packages[`node_modules/${name}`]?.version,
      version,
      `The image must install the pinned ${name} version`,
    );
  }
});
