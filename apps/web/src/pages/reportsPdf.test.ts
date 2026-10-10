import { readFileSync } from "node:fs";
import fontkit from "@pdf-lib/fontkit";
import { PDFDocument, PDFPage } from "pdf-lib";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReportSnapshot } from "../api/reports";
import { buildReportPdf, downloadReportPdf, wrapPdfText } from "./reportsPdf";
import { reportSnapshotCsv } from "./reportsCsv";

const fontBytes = new Uint8Array(
  readFileSync("public/fonts/NotoSansCJKsc-Regular.otf"),
);

function snapshot(): ReportSnapshot {
  const unavailable = {
    availability: "unavailable" as const,
    expected_count: null,
    observed_count: 0,
    counts: {},
    reason: "封存清单未提供",
  };
  return {
    report_id: "report-1",
    project_id: "project-1",
    cycle_id: "cycle-1",
    revision: 2,
    correction_of: "report-original",
    report_window_start_at: "2026-09-21T00:00:00Z",
    report_window_end_at: "2026-09-28T00:00:00Z",
    report_timezone: "Asia/Shanghai",
    cutoff_at: "2026-09-28T12:00:00Z",
    evidence_as_of: "2026-09-28T12:00:00Z",
    generated_at: "2026-09-28T12:01:00Z",
    reducer_version: "reduce-v1",
    input_hash: "frozen-input-hash",
    status: "partial",
    input_manifest_versions: [
      {
        kind: "document",
        manifest_id: "manifest-1",
        revision: 3,
        sealed: true,
        expected_count: 2,
      },
    ],
    documents: {
      availability: "available",
      expected_count: 2,
      observed_count: 1,
      counts: { blocked: 1, planned: 1 },
      reason: null,
    },
    publications: unavailable,
    publication_groups: [{ platform_id: "platform-1", coverage: unavailable }],
    measurements: unavailable,
    measurement_groups: [
      { comparison_key: "opaque-key-1", coverage: unavailable },
    ],
    findings: [
      {
        finding_id: "finding-1",
        kind: "coverage_gap",
        summary: "资料不足，不能推断趋势",
        insufficient_reason: "本周没有完整测量",
        evidence_ids: ["evidence-1"],
      },
    ],
    evidence: [
      {
        evidence_id: "evidence-1",
        kind: "source",
        resource_id: "item-1",
        resource_version: "v1",
        occurred_at: "2026-09-26T00:00:00Z",
        received_at: "2026-09-26T00:01:00Z",
        summary: "来源证据：原始文档缺失。",
      },
    ],
  };
}

describe("report PDF", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("exports per-group purpose without inventing a legacy assignment or mutating the snapshot", () => {
    const report = snapshot();
    report.measurement_groups.push(
      {
        ...report.measurement_groups[0],
        comparison_key: "optimization-key",
        purpose: "optimization",
      },
      {
        ...report.measurement_groups[0],
        comparison_key: "evaluation-key",
        purpose: "frozen_evaluation",
      },
    );
    const before = JSON.stringify(report);
    const csv = reportSnapshotCsv(report);
    expect(csv).toContain(
      '"measurement_group","opaque-key-1","purpose","legacy_unclassified",""',
    );
    expect(csv).toContain(
      '"measurement_group","optimization-key","purpose","optimization",""',
    );
    expect(csv).toContain(
      '"measurement_group","evaluation-key","purpose","frozen_evaluation",""',
    );
    expect(csv).toContain(
      '"measurement_group","opaque-key-1","expected_count","",""',
    );
    expect(JSON.stringify(report)).toBe(before);
  });

  it("wraps opaque identifiers and Chinese without losing code points or crossing the width", () => {
    const text = `资料${"0123456789abcdef-".repeat(18)}终`;
    const lines = wrapPdfText(text, 13, (value) => [...value].length);
    expect(lines.length).toBeGreaterThan(20);
    expect(lines.join("")).toBe(text);
    expect(lines.every((line) => [...line].length <= 13)).toBe(true);
    expect(wrapPdfText("甲\n\n乙", 2, (value) => [...value].length)).toEqual([
      "甲",
      "",
      "乙",
    ]);
  });

  it("avoids private-use localized digits in Latin evidence identifiers", () => {
    const font = fontkit.create(fontBytes);
    const shaped = font.layout("END_OF_LAST_EVIDENCE_4960", {
      locl: false,
    }).glyphs;
    const actual = shaped.slice(-4).map((glyph) => glyph.id);
    const canonical = [..."4960"].map(
      (digit) => font.glyphForCodePoint(digit.codePointAt(0)!).id,
    );
    expect(actual).toEqual(canonical);
  });

  // Real 16 MiB font embedding needs a longer limit when the full suite runs in parallel.
  it("embeds a Chinese-capable font into valid multipage A4 PDF with snapshot metadata", async () => {
    const report = snapshot();
    const drawn = vi.spyOn(PDFPage.prototype, "drawText");
    report.measurement_groups = [
      report.measurement_groups[0],
      {
        ...report.measurement_groups[0],
        comparison_key: "optimization-key",
        purpose: "optimization",
      },
      {
        ...report.measurement_groups[0],
        comparison_key: "evaluation-key",
        purpose: "frozen_evaluation",
      },
    ];
    report.evidence = Array.from({ length: 25 }, (_, index) => ({
      ...report.evidence[0],
      evidence_id: `evidence-${index}`,
      resource_id: `resource-${index}-${"opaque".repeat(18)}`,
      summary: `中文资料 ${index}：${"长文本、".repeat(35)}`,
    }));
    report.supplementary_measurements = [
      {
        target_id: "saved-target",
        attempt_id: "saved-attempt",
        comparison_key: "saved-protocol",
        observation: {
          raw_answer: "SAVED_ANALYSIS_ANSWER",
          citations: ["https://example.org/saved-citation"],
          observed_at: "2026-09-26T00:00:00Z",
          received_at: "2026-09-26T00:01:00Z",
          provenance: {
            revision_id: "saved-revision",
            source: { kind: "capture", capture_id: "saved-capture" },
            source_sha256: "saved-digest",
            observed_at: "2026-09-26T00:00:00Z",
            analyzed_at: "2026-09-27T00:00:00Z",
            actual_model: "saved-model",
            config_revision: null,
            prompt_version: "prompt-1",
            parser_version: "parser-1",
          },
        },
      },
    ];
    const bytes = await buildReportPdf(report, fontBytes);
    const text = drawn.mock.calls.map(([value]) => value).join("\n");
    expect(text).toContain("问题用途：优化");
    expect(text).toContain("问题用途：冻结评估（不进入优化）");
    expect(text).toContain("问题用途：旧未分类（不进入优化）");
    expect(text).toContain("已存原文的补充分析");
    expect(text).toContain("SAVED_ANALYSIS_ANSWER");
    expect(text).toContain("saved-revision");
    expect(text).toContain("saved-model");
    expect(text).toContain("https://example.org/saved-citation");
    expect(new TextDecoder().decode(bytes.slice(0, 8))).toMatch(/^%PDF-1\./);
    const pdf = await PDFDocument.load(bytes);
    expect(pdf.getTitle()).toContain("report-1");
    expect(pdf.getSubject()).toContain("不可变报告快照");
    expect(pdf.getPageCount()).toBeGreaterThan(1);
    expect(
      pdf.getPages().every((page) => {
        const { width, height } = page.getSize();
        return (
          Math.abs(width - 595.28) < 0.1 && Math.abs(height - 841.89) < 0.1
        );
      }),
    ).toBe(true);
    expect(bytes.length).toBeGreaterThan(10_000);
  }, 15_000);

  it("fails safely if font bytes are missing", async () => {
    await expect(buildReportPdf(snapshot(), new Uint8Array())).rejects.toThrow(
      "中文字库不可用",
    );
  });

  it("downloads only the selected snapshot's PDF and revokes its object URL", async () => {
    const objectUrl = "blob:report-test";
    const created = vi.fn((_blob: Blob) => objectUrl);
    const revoked = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(fontBytes, { status: 200 })),
    );
    vi.stubGlobal(
      "URL",
      Object.assign(URL, {
        createObjectURL: created,
        revokeObjectURL: revoked,
      }),
    );
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(function (this: HTMLAnchorElement) {
        expect(this.download).toBe("report-report-1-r2.pdf");
        expect(this.href).toBe(objectUrl);
      });
    await downloadReportPdf(snapshot());
    expect(fetch).toHaveBeenCalledWith("/fonts/NotoSansCJKsc-Regular.otf", {
      credentials: "same-origin",
      signal: undefined,
    });
    expect(created).toHaveBeenCalledOnce();
    expect(created.mock.calls[0][0]).toMatchObject({ type: "application/pdf" });
    expect(click).toHaveBeenCalledOnce();
    expect(revoked).toHaveBeenCalledWith(objectUrl);
  }, 15_000);

  it("does not download if the selected report has been cancelled", async () => {
    const controller = new AbortController();
    controller.abort();
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    await expect(
      downloadReportPdf(snapshot(), { signal: controller.signal }),
    ).rejects.toMatchObject({ name: "AbortError" });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("does not download if selection changes during the font request", async () => {
    const controller = new AbortController();
    let resolveFetch!: (value: Response) => void;
    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          new Promise<Response>((resolve) => {
            resolveFetch = resolve;
          }),
      ),
    );
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click");
    const pending = downloadReportPdf(snapshot(), {
      signal: controller.signal,
    });
    controller.abort();
    resolveFetch(new Response(fontBytes, { status: 200 }));
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    expect(click).not.toHaveBeenCalled();
  });
});
