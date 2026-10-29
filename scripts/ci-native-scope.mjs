import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

// Web TS/TSX/CSS is compiled by Vite; the agent bundler reads its package manifest.
// Keep manifests and shared inputs native: extraction-prompt.txt is embedded.
const safePath =
  /^(?:apps\/web\/src\/.+\.(?:tsx?|css)|packages\/browser-runner\/(?:src|test)\/.+\.mjs|docs\/.+\.md|README\.md|CONTRIBUTING\.md)$/;

export function nativeRequired({ eventName, baseRef, paths } = {}) {
  return (
    eventName !== "pull_request" ||
    baseRef !== "main" ||
    !Array.isArray(paths) ||
    paths.length === 0 ||
    paths.some((path) => typeof path !== "string" || !safePath.test(path))
  );
}

export function determineNativeScope({
  eventName,
  event,
  cwd = process.cwd(),
}) {
  const baseRef = event?.pull_request?.base?.ref;
  if (eventName !== "pull_request" || baseRef !== "main") return true;
  const base = event.pull_request.base.sha;
  const head = event.pull_request.head?.sha;
  if (![base, head].every((sha) => /^[0-9a-f]{40}$/i.test(sha ?? ""))) {
    throw new Error("Native scope requires complete pull request commit SHAs.");
  }
  // Include the entire PR and both sides of renames. Git failures must fail CI.
  const diff = execFileSync(
    "git",
    ["diff", "--name-only", "--no-renames", "-z", `${base}...${head}`, "--"],
    { cwd, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 },
  );
  if (diff && !diff.endsWith("\0")) {
    throw new Error("Native scope received an incomplete Git path list.");
  }
  const paths = diff ? diff.slice(0, -1).split("\0") : [];
  return nativeRequired({ eventName, baseRef, paths });
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const eventName = process.env.GITHUB_EVENT_NAME;
  const event =
    eventName === "pull_request"
      ? JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"))
      : {};
  const required = determineNativeScope({ eventName, event });
  appendFileSync(process.env.GITHUB_OUTPUT, `required=${required}\n`);
  console.log(`Windows native checks required: ${required}`);
}
