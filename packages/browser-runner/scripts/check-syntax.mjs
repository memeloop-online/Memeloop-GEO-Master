// Syntax and module-graph check for the browser runner.
//
// `node --check` proves each file parses, but it does not resolve imports: a
// renamed or missing module only fails when something actually loads it. So
// this also imports the real entry points in a child process, which walks the
// whole graph and surfaces both missing files and import cycles.
//
// Dependency-free on purpose: the desktop image installs this package with
// `npm ci --omit=dev`, so no tooling beyond the dependency list is available.

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const SOURCE_DIRS = ["src", "test", "smoke", "scripts"];
const ENTRY_POINTS = ["src/adapters.mjs", "src/runner.mjs", "src/server.mjs"];

function modulesUnder(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory())
      return entry.name === "node_modules" ? [] : modulesUnder(path);
    return entry.name.endsWith(".mjs") ? [path] : [];
  });
}

const files = SOURCE_DIRS.filter((name) => existsSync(join(root, name)))
  .flatMap((name) => modulesUnder(join(root, name)))
  .sort();

let failed = 0;
for (const file of files) {
  try {
    execFileSync(process.execPath, ["--check", file], { stdio: "pipe" });
  } catch (error) {
    failed += 1;
    process.stderr.write(
      `syntax: ${relative(root, file)}\n${error.stderr ?? ""}\n`,
    );
  }
}
if (failed > 0) {
  process.stderr.write(
    `\n${failed} of ${files.length} modules failed to parse\n`,
  );
  process.exit(1);
}

const graph = `
  const { pathToFileURL } = await import("node:url");
  for (const target of JSON.parse(process.env.GEO_RUNNER_CHECK_ENTRIES))
    await import(pathToFileURL(target).href);
`;
try {
  execFileSync(process.execPath, ["--input-type=module", "-e", graph], {
    cwd: root,
    stdio: "inherit",
    env: {
      ...process.env,
      GEO_RUNNER_CHECK_ENTRIES: JSON.stringify(
        ENTRY_POINTS.map((path) => join(root, path)),
      ),
    },
  });
} catch {
  process.stderr.write("module graph failed to load\n");
  process.exit(1);
}

process.stdout.write(
  `checked ${files.length} modules and ${ENTRY_POINTS.length} entry points\n`,
);
