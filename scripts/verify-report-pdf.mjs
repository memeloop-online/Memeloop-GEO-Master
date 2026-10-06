// End-to-end synthetic PDF acceptance: the shipped exporter/font and real PDFBox text extraction.
// GEO_REPORT_PDF_OUTPUT_DIR and GEO_REPORT_PDF_PARSER_JAR must be absolute paths.
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const webRoot = join(repository, "apps", "web");
const output = process.env.GEO_REPORT_PDF_OUTPUT_DIR;
const parserJar = process.env.GEO_REPORT_PDF_PARSER_JAR;
const java = process.env.GEO_REPORT_PDF_JAVA ?? "java";
const secret = "INJECTED_SECRET_SHOULD_NEVER_APPEAR_5932";
const check = (condition, message) => {
  if (!condition) throw new Error(message);
};
const isInside = (parent, child) => {
  const rel = relative(parent, child);
  return (
    !rel || (rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel))
  );
};

async function childProcess(command, args) {
  const safeEnv = Object.fromEntries(
    ["PATH", "Path", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT", "JAVA_HOME"]
      .filter((key) => process.env[key] !== undefined)
      .map((key) => [key, process.env[key]]),
  );
  await new Promise((accept, reject) => {
    const child = spawn(command, args, {
      cwd: repository,
      env: safeEnv,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let diagnostics = "";
    for (const stream of [child.stdout, child.stderr]) {
      stream.on("data", (chunk) => {
        diagnostics = (diagnostics + chunk.toString()).slice(-4000);
      });
    }
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error("PDF parser exceeded its 60-second deadline"));
    }, 60_000);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("close", (code) => {
      clearTimeout(timer);
      if (code === 0) accept();
      else reject(new Error(`PDF parser failed (${code}): ${diagnostics}`));
    });
  });
}

function snapshot(large) {
  const coverage = (
    availability,
    expected,
    observed,
    counts,
    reason = null,
  ) => ({
    availability,
    expected_count: expected,
    observed_count: observed,
    counts,
    reason,
  });
  const evidence = (index) => ({
    evidence_id: `evidence-${String(index).padStart(3, "0")}`,
    kind: index === 0 ? "source" : "measurement_answer",
    resource_id: `resource-${index}-${"abcdef0123456789".repeat(large ? 5 : 1)}`,
    resource_version: `version-${index}`,
    occurred_at: "2026-09-24T02:30:00Z",
    received_at: "2026-09-24T03:00:00Z",
    summary:
      index === (large ? 54 : 0)
        ? `末条证据跨页仍可查：${"中文原始证据需完整保留，".repeat(large ? 25 : 1)}终止标记 END_OF_LAST_EVIDENCE_4960`
        : `中文原始证据 ${index}：${"答案缺测不等于未提及。".repeat(large ? 4 : 1)}`,
  });
  return {
    report_id: large ? "report-large-immutable" : "report-small-immutable",
    project_id: "project-synthetic",
    cycle_id: "cycle-synthetic",
    revision: 3,
    correction_of: "report-prior-immutable",
    report_window_start_at: "2026-09-21T00:00:00Z",
    report_window_end_at: "2026-09-28T00:00:00Z",
    report_timezone: "Asia/Shanghai",
    cutoff_at: "2026-09-28T12:00:00Z",
    generated_at: "2026-09-28T12:01:00Z",
    reducer_version: "synthetic-reducer-v1",
    input_hash: "FROZEN_INPUT_HASH_3579",
    evidence_as_of: "2026-09-28T11:45:00Z",
    status: "partial",
    input_manifest_versions: [
      {
        kind: "document",
        manifest_id: "manifest-document",
        revision: 4,
        sealed: true,
        expected_count: 0,
      },
      {
        kind: "distribution",
        manifest_id: "manifest-distribution",
        revision: 5,
        sealed: false,
        expected_count: null,
      },
      {
        kind: "measurement",
        manifest_id: "manifest-measurement",
        revision: 8,
        sealed: true,
        expected_count: 3,
      },
    ],
    documents: coverage("available", 0, 0, { ready: 0 }),
    publications: coverage(
      "unsealed",
      null,
      0,
      { unknown: 1 },
      "冻结计划未封存",
    ),
    publication_groups: [
      {
        platform_id: "platform-synthetic",
        coverage: coverage("available", 2, 1, { unknown: 1, verified: 0 }),
      },
    ],
    measurements: coverage("available", 3, 2, {
      missing: 1,
      not_mentioned: 1,
      mentioned: 0,
    }),
    measurement_groups: [
      {
        comparison_key: "comparison-frozen-synthetic",
        coverage: coverage("available", 3, 2, {
          missing: 1,
          not_mentioned: 1,
          mentioned: 0,
        }),
      },
    ],
    findings: [
      {
        finding_id: "finding-coverage-gap",
        kind: "insufficient_evidence",
        summary: "中文结论：未知发布与技术缺测不可作为失败或未提及。",
        evidence_ids: ["evidence-000", "evidence-999"],
        insufficient_reason: "部分采样尚未完成",
      },
    ],
    evidence: Array.from({ length: large ? 55 : 1 }, (_, index) =>
      evidence(index),
    ),
    // These properties are deliberately not part of ReportSnapshot.
    private_token: secret,
    customer_secret: { credentials: secret },
  };
}

async function main() {
  check(
    output && isAbsolute(output),
    "GEO_REPORT_PDF_OUTPUT_DIR must be absolute",
  );
  check(
    parserJar && isAbsolute(parserJar),
    "GEO_REPORT_PDF_PARSER_JAR must be absolute",
  );
  const outputDir = resolve(output);
  check(
    !isInside(repository, outputDir),
    "PDF artifacts must be outside the repository",
  );
  const requireWeb = createRequire(join(webRoot, "package.json"));
  const requireVite = createRequire(requireWeb.resolve("vite"));
  const { build } = requireVite("esbuild");
  const scratch = await mkdtemp(join(tmpdir(), "geo-report-pdf-"));
  try {
    const modulePath = join(scratch, "report-export.mjs");
    await build({
      entryPoints: [join(webRoot, "src", "pages", "reportsPdf.ts")],
      outfile: modulePath,
      bundle: true,
      platform: "node",
      format: "esm",
      target: "node22",
      logLevel: "silent",
    });
    const { buildReportPdf } = await import(pathToFileURL(modulePath).href);
    const fontBytes = new Uint8Array(
      await readFile(
        join(webRoot, "public", "fonts", "NotoSansCJKsc-Regular.otf"),
      ),
    );
    await mkdir(outputDir, { recursive: true });
    for (const kind of ["small", "large"]) {
      const bytes = await buildReportPdf(snapshot(kind === "large"), fontBytes);
      check(
        Buffer.from(bytes.subarray(0, 5)).toString("ascii") === "%PDF-",
        "Exporter did not generate a PDF",
      );
      const pdf = join(outputDir, `report-${kind}-synthetic.pdf`);
      await writeFile(pdf, bytes);
      await childProcess(java, [
        "--source",
        "21",
        "-cp",
        parserJar,
        join(repository, "scripts", "VerifyReportPdf.java"),
        pdf,
        outputDir,
        kind,
      ]);
      console.log(`Verified ${kind} PDF (${bytes.length} bytes).`);
    }
    console.log("Synthetic report PDF acceptance passed.");
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(`Report PDF acceptance failed: ${error.message}`);
  process.exitCode = 1;
});
