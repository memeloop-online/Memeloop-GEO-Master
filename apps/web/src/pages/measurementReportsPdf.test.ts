import { readFileSync } from "node:fs";
import { PDFDocument, PDFPage } from "pdf-lib";
import Papa from "papaparse";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "../i18n";
import type {
  MeasurementPeriodPreview,
  MeasurementPeriodReport,
  MeasurementPeriodSample,
} from "../api/measurementReports";
import type { RecommendationItem } from "../api/recommendations";
import { recommendationFrequency } from "../components/recommendationComparison";
import { measurementReportExportTranslations } from "../i18n/measurementReportExport";
import type { MeasurementReportExportInput } from "./measurementReportExport";
import { measurementReportCsv } from "./measurementReportsCsv";
import {
  buildMeasurementReportPdf,
  downloadMeasurementReportPdf,
} from "./measurementReportsPdf";

const fontBytes = new Uint8Array(
  readFileSync("src/assets/fonts/NotoSansCJKsc-Regular.otf"),
);
const item = (
  rank: number | null = 1,
  patch: Partial<RecommendationItem> = {},
): RecommendationItem => ({
  name: "Synthetic alias",
  kind: "product",
  mention_order: 1,
  rank,
  reason: "Original reason 原因",
  evidence_quote: "Original quote 原句",
  brand_attribution: null,
  citation_urls: ["https://example.test/recommendation"],
  ...patch,
});
const verified = (items: RecommendationItem[]) => ({
  version: "geo.recommendations.v2",
  status: "verified",
  items,
});
const sample = (
  id: string,
  recommendations: unknown,
  patch: Partial<MeasurementPeriodSample> = {},
): MeasurementPeriodSample => ({
  plan_id: "synthetic-plan",
  target_id: id,
  attempt_id: `attempt-${id}`,
  comparison_key: "frozen-key",
  conditions: {
    provider: "synthetic-platform",
    model: "synthetic-model",
    surface: "web",
    search_mode: "search",
    protocol_version: "protocol-1",
    question_set_version: "version-1",
    question: "Synthetic question?",
    market: "test",
    language: "en",
    sample_ordinal: 1,
  },
  recommendations,
  scheduled_at: "2026-01-02T00:00:00Z",
  original_status: "observed",
  observed_live: true,
  observation: {
    raw_answer: "Original answer 原文",
    citations: ["https://example.test/answer"],
    observed_at: "2026-01-02T00:01:00Z",
    received_at: "2026-01-02T00:02:00Z",
    provenance: null,
  },
  ...patch,
});
const preview = (): MeasurementPeriodPreview => ({
  kind: "measurement_period_preview",
  project_id: "synthetic-project",
  report_window_start_at: "2026-01-01T00:00:00Z",
  report_window_end_at: "2026-01-08T00:00:00Z",
  report_timezone: "Asia/Shanghai",
  evidence_as_of: "2026-01-09T00:00:00Z",
  generated_at: "2026-01-09T00:01:00Z",
  input_hash: "synthetic-hash",
  coverage: {
    planned: 8,
    counts: { observed: 6, missing: 2 },
    observed_live: 6,
    grounded_saved_analysis: 1,
  },
  samples: [
    sample("first", verified([item()])),
    sample("empty", verified([])),
    sample("missing", null, { observation: null, original_status: "missing" }),
    sample(
      "conflict",
      verified([item(), item(2, { name: "Synthetic item", mention_order: 2 })]),
    ),
    sample("unranked", verified([item(null)])),
    sample("wrong-kind", verified([item(1, { kind: "brand" })])),
  ],
});
const input = (
  report: MeasurementReportExportInput["report"] = preview(),
): MeasurementReportExportInput => ({
  report,
  view: {
    question: null,
    target: {
      name: "Synthetic item",
      kind: "product",
      aliases: ["Synthetic alias"],
    },
  },
  locale: "en",
});

beforeEach(() =>
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(fontBytes, { status: 200 })),
  ),
);
afterEach(async () => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  await i18n.changeLanguage("zh-CN");
});

describe("independent measurement PDF", () => {
  it.each(["en", "zh-CN"] as const)(
    "uses the same direct and affiliated brand counts as the UI and CSV in %s",
    async (locale) => {
      const attributed = (name: string): RecommendationItem =>
        item(1, {
          name: "Synthetic product",
          brand_attribution: {
            name,
            evidence_quote: "Original brand relationship 品牌原句",
          },
        });
      const answers = [
        [item(2, { name: "Synthetic brand", kind: "brand" })],
        [
          attributed("Synthetic brand alias"),
          { ...attributed("Synthetic brand"), mention_order: 2 },
        ],
        [
          attributed("Synthetic brand"),
          item(1, { name: "Unknown product", mention_order: 2 }),
        ],
        [attributed("Other brand")],
        [item(1, { name: "Unknown product" })],
        [],
        null,
      ];
      const report = preview();
      report.samples = answers.map((answer, index) =>
        sample(`brand-${index}`, answer === null ? null : verified(answer)),
      );
      const request = { ...input(report), locale };
      request.view.target = {
        name: "Synthetic brand",
        kind: "brand",
        aliases: ["Synthetic brand alias"],
      };
      const frequency = recommendationFrequency(
        answers.map((items) => ({ items })),
        request.view.target,
      );
      expect(frequency.brandSummary).toEqual({
        positiveAnswers: 2,
        knownAnswers: 5,
        unknownAnswers: 1,
        validAnswers: 6,
      });
      expect(frequency.recommended).toBe(1);
      expect([...frequency.ranks]).toEqual([[2, 1]]);
      const csv = Papa.parse<Record<string, string>>(
        measurementReportCsv(request),
        { header: true, skipEmptyLines: true },
      ).data;
      const drawn = vi.spyOn(PDFPage.prototype, "drawText");
      const bytes = await buildMeasurementReportPdf(request);
      const text = drawn.mock.calls.map(([value]) => value).join("\n");
      const labels = measurementReportExportTranslations[locale];
      for (const field of [
        "brand_affiliate_positive",
        "brand_attribution_known",
        "brand_attribution_unknown",
        "brand_attribution_valid",
      ] as const) {
        const count = csv.find(
          (row) => row.section === "group" && row.field === field,
        )?.value;
        expect(text).toContain(`${labels[field]}: ${count}`);
      }
      expect(text).toContain("1 / 6");
      expect(text).toContain("0 / 1");
      expect(text).toContain(
        `${labels.brand_attribution}: Synthetic brand alias`,
      );
      expect(text).toContain("Original brand relationship 品牌原句");
      expect(text).toContain(labels.brand_attribution_not_recorded);
      expect((await PDFDocument.load(bytes)).getPageCount()).toBeGreaterThan(1);
    },
    20_000,
  );

  it.each(["en", "zh-CN"] as const)(
    "renders readable results and exact preview metadata in %s without mutation",
    async (locale) => {
      const request = { ...input(), locale };
      const before = JSON.stringify(request);
      const drawn = vi.spyOn(PDFPage.prototype, "drawText");
      const bytes = await buildMeasurementReportPdf(request);
      const text = drawn.mock.calls.map(([value]) => value).join("\n");
      const pdf = await PDFDocument.load(bytes);
      expect(text).toContain(
        locale === "en" ? "Results at a glance" : "结果概览",
      );
      expect(text).toContain(
        locale === "en" ? "Unsaved preview" : "未保存预览",
      );
      expect(text).toContain("3 / 5");
      expect(text).toContain("60%");
      expect(text).toContain("1 / 1");
      expect(text).toContain("100%");
      expect(text).toContain("Original answer 原文");
      expect(text).toContain("Original reason 原因");
      expect(text).toContain(
        measurementReportExportTranslations[locale].verified_empty,
      );
      expect(text).toContain("https://example.test/recommendation");
      expect(text).toContain("2026-01-02T00:01:00.000Z");
      expect(text).toContain(request.report.generated_at);
      expect(text).toContain("synthetic-hash");
      expect(pdf.getTitle()).toContain(
        locale === "en" ? "Unsaved preview" : "未保存预览",
      );
      expect(pdf.getCreationDate()!.toISOString()).toBe(
        request.report.generated_at.replace("Z", ".000Z"),
      );
      expect(JSON.stringify(request)).toBe(before);
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch).toHaveBeenCalledWith(
        new URL("../assets/fonts/NotoSansCJKsc-Regular.otf", import.meta.url)
          .href,
        {
          credentials: "same-origin",
          signal: undefined,
        },
      );
      expect(bytes.byteLength).toBeGreaterThan(10_000);
    },
    20_000,
  );

  it("keeps exact question/version/condition partitions and whole-report search separate", async () => {
    const report = preview();
    // One base answer is sufficient; the added records exercise each partition.
    report.samples = report.samples.slice(0, 1);
    const other = sample("other-question", verified([item()]), {
      conditions: {
        ...report.samples[0].conditions,
        question: "Other question?",
      },
      observation: {
        ...report.samples[0].observation!,
        raw_answer: "EXCLUDED_ANSWER",
      },
    });
    report.samples.push(
      other,
      sample("other-platform", verified([]), {
        conditions: {
          ...report.samples[0].conditions,
          provider: "second-platform",
        },
      }),
      sample("other-version", verified([item()]), {
        question_binding: {
          purpose: "optimization",
          reference: {
            question_set_id: "synthetic-set",
            question_set_version_id: "second-version",
            question_id: "synthetic-question",
            question_revision_id: "question-revision",
          },
          split_policy_version: "split-1",
        },
      }),
    );
    report.search = {
      schema_version: "search-v1",
      coverage: { planned: 1, counts: { missing: 1 } },
      samples: [
        {
          cohort: {
            measurement_id: "synthetic-search",
            source_key: "synthetic-source",
            query: "SEARCH_OUTSIDE_AI_FILTER",
            protocol: {
              query: "SEARCH_OUTSIDE_AI_FILTER",
              engine: "synthetic-engine",
              surface: "web",
              source: "synthetic-source",
              country: "test",
              city: null,
              language: "en",
              device: "desktop",
              requested_depth: 5,
            },
            target: null,
            target_rule_version: "target-v1",
            question_binding: null,
            scheduled_at: "2026-01-03T00:00:00Z",
            created_at: "2026-01-02T00:00:00Z",
            stored_at: "2026-01-02T00:00:00Z",
          },
          evidence: null,
        },
      ],
    };
    const request = input(report);
    request.view.question = "Synthetic question?";
    const drawn = vi.spyOn(PDFPage.prototype, "drawText");
    await buildMeasurementReportPdf(request);
    const text = drawn.mock.calls.map(([value]) => value).join("\n");
    expect(text).not.toContain("EXCLUDED_ANSWER");
    expect(text).toContain("Comparison 1");
    expect(text).toContain("Comparison 2");
    expect(text).toContain("Comparison 3");
    expect(text).toContain("SEARCH_OUTSIDE_AI_FILTER");
    expect(text).toContain("Planned measurements: 8");
    expect(text).toContain("second-platform");
    expect(text).toContain("second-version");
  }, 20_000);

  it("preserves saved corrections, missing values and zero denominators without invented rates", async () => {
    const report: MeasurementPeriodReport = {
      ...preview(),
      kind: "measurement_period",
      report_id: "synthetic-report",
      revision: 3,
      correction_of: "synthetic-original",
      samples: [sample("unavailable", null, { observation: null })],
      report_timezone: "Invalid/Timezone",
    };
    const drawn = vi.spyOn(PDFPage.prototype, "drawText");
    const bytes = await buildMeasurementReportPdf(input(report));
    const text = drawn.mock.calls.map(([value]) => value).join("\n");
    expect(text).toContain("synthetic-original");
    expect(text).toContain("No valid recommendation results");
    expect(text.replace(/\n/gu, "")).toContain(
      "No recommended samples with a clear, consistent rank",
    );
    expect(text).toContain("Earliest actual observation time: Not recorded");
    expect(text).toContain(
      "Samples without a valid actual observation time: 1",
    );
    expect(text).toContain("Recommendation results unavailable");
    expect(text).not.toContain("0%");
    expect(text).toContain("The stored timezone is invalid");
    const pdf = await PDFDocument.load(bytes);
    expect(pdf.getTitle()).toContain("synthetic-report");
    expect(text).toContain("revision 3");
  }, 20_000);

  it.each(["en", "zh-CN"] as const)(
    "exports attribution evidence without any tracked item and wraps long CJK/opaque content through the final marker in %s",
    async (locale) => {
      const report = preview();
      report.samples = [
        sample(
          "long",
          verified([
            item(1, {
              brand_attribution: {
                name: "Synthetic brand alias",
                evidence_quote: "Original brand relationship 品牌原句",
              },
            }),
            item(2, { name: "Unknown product", mention_order: 2 }),
          ]),
          {
            observation: {
              ...report.samples[0].observation!,
              raw_answer: `${"中文长原文，".repeat(1200)}${"opaque-key".repeat(180)}\nEND_OF_LAST_EVIDENCE_4960\u{1F9EA}`,
              provenance: {
                revision_id: "synthetic-analysis",
                source: { kind: "capture", capture_id: "synthetic-capture" },
                source_sha256: "synthetic-sha",
                observed_at: "2025-12-01T00:00:00Z",
                analyzed_at: "2026-01-02T00:03:00Z",
                actual_model: "synthetic-analysis-model",
                config_revision: null,
                prompt_version: "prompt-1",
                parser_version: "parser-1",
              },
            },
          },
        ),
      ];
      const request = { ...input(report), locale };
      request.view.target = null;
      const drawn = vi.spyOn(PDFPage.prototype, "drawText");
      const bytes = await buildMeasurementReportPdf(request);
      const text = drawn.mock.calls.map(([value]) => value).join("\n");
      const labels = measurementReportExportTranslations[locale];
      expect(text).toContain(
        locale === "en" ? "No tracked item selected" : "未选择关注对象",
      );
      expect(text).not.toContain(labels.brand_affiliate_positive);
      expect(text).toContain(
        `${labels.brand_attribution}: Synthetic brand alias`,
      );
      expect(text).toContain("Original brand relationship 品牌原句");
      expect(text).toContain(labels.brand_attribution_not_recorded);
      expect(text).toContain("END_OF_LAST_EVIDENCE_4960[U+1F9EA]");
      expect(text).toContain("synthetic-analysis-model");
      expect(text).toContain("2025-12-01T00:00:00Z");
      const pdf = await PDFDocument.load(bytes);
      expect(pdf.getPageCount()).toBeGreaterThan(3);
      expect(
        pdf
          .getPages()
          .every(
            (page) => page.getWidth() === 595.28 && page.getHeight() === 841.89,
          ),
      ).toBe(true);
      for (const [value, options] of drawn.mock.calls) {
        expect(
          options!.x! + options!.font!.widthOfTextAtSize(value, options!.size!),
        ).toBeLessThanOrEqual(595.28 - 46 + 0.01);
        expect(options!.y!).toBeGreaterThanOrEqual(27);
      }
    },
    20_000,
  );

  it("captures language and evidence before the font request resolves", async () => {
    let resolve!: (response: Response) => void;
    vi.stubGlobal(
      "fetch",
      vi.fn(
        () =>
          new Promise<Response>((done) => {
            resolve = done;
          }),
      ),
    );
    const request = input();
    request.report.samples = request.report.samples.slice(0, 1);
    const drawn = vi.spyOn(PDFPage.prototype, "drawText");
    const pending = buildMeasurementReportPdf(request);
    request.locale = "zh-CN";
    request.report.samples[0].observation!.raw_answer = "MUTATED_AFTER_CAPTURE";
    await i18n.changeLanguage("zh-CN");
    resolve(new Response(fontBytes, { status: 200 }));
    await pending;
    const text = drawn.mock.calls.map(([value]) => value).join("\n");
    expect(text).toContain("Results at a glance");
    expect(text).not.toContain("MUTATED_AFTER_CAPTURE");
  }, 20_000);

  it("cleans up the object URL and anchor even when the download click fails", async () => {
    const created = vi.fn(() => "blob:measurement-pdf");
    const revoked = vi.fn();
    vi.stubGlobal(
      "URL",
      Object.assign(URL, {
        createObjectURL: created,
        revokeObjectURL: revoked,
      }),
    );
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      expect(this.download).toBe("measurement-report-preview.pdf");
      throw new Error("Download click failed");
    });
    const request = input();
    request.report.samples = request.report.samples.slice(0, 1);
    await expect(downloadMeasurementReportPdf(request)).rejects.toThrow(
      "Download click failed",
    );
    expect(created).toHaveBeenCalledOnce();
    expect(revoked).toHaveBeenCalledWith("blob:measurement-pdf");
    expect(
      document.querySelector('a[download="measurement-report-preview.pdf"]'),
    ).toBeNull();
  }, 20_000);

  it("downloads a saved correction and preserves independent search results and provenance once", async () => {
    const report: MeasurementPeriodReport = {
      ...preview(),
      samples: [],
      kind: "measurement_period",
      report_id: "synthetic-saved",
      revision: 2,
      correction_of: "synthetic-prior",
    };
    report.search = {
      schema_version: "search-v1",
      coverage: { planned: 1, counts: { partial: 1 } },
      samples: [
        {
          cohort: {
            measurement_id: "synthetic-search",
            source_key: "synthetic-source",
            query: "Frozen search query",
            protocol: {
              query: "Frozen search query",
              engine: "synthetic-engine",
              surface: "web",
              source: "synthetic-source",
              country: "test",
              city: null,
              language: "en",
              device: "desktop",
              requested_depth: 5,
            },
            target: {
              kind: "host",
              host: "example.test",
              include_subdomains: false,
            },
            target_rule_version: "target-v1",
            question_binding: null,
            scheduled_at: "2026-01-03T00:00:00Z",
            created_at: "2026-01-02T00:00:00Z",
            stored_at: "2026-01-02T00:00:00Z",
          },
          evidence: {
            observation: {
              observation_id: "synthetic-observation",
              measurement_id: "synthetic-search",
              attempt_id: "synthetic-search-attempt",
              raw_evidence_id: "synthetic-raw-evidence",
              raw_sha256: "synthetic-raw-sha",
              parser_version: "search-parser-v1",
              provider_observed_at: null,
              received_at: "2026-01-03T01:00:00Z",
              analyzed_at: "2026-01-03T01:01:00Z",
              status: "partial",
              actual_conditions: {
                country: {
                  value: "test",
                  evidence_locator: "synthetic-locator",
                },
              },
              coverage: {
                requested_depth: 5,
                observed_organic_depth: 1,
                pages_received: 1,
                completion: "partial",
                truncated: true,
              },
              results: [
                {
                  kind: "organic",
                  raw_kind: "organic",
                  raw_url: "https://example.test/result",
                  normalized_url: "https://example.test/result",
                  host: "example.test",
                  title: "Original search title 原题",
                  page: 1,
                  position: 1,
                  organic_rank: 1,
                  absolute_position: 1,
                  locator: "synthetic-result-locator",
                },
              ],
              source_limitations: ["SYNTHETIC_SOURCE_LIMIT"],
            },
            raw_stored_at: "2026-01-03T01:00:00Z",
            observation_stored_at: "2026-01-03T01:01:00Z",
            evidence_time: "2026-01-03T01:00:00Z",
            evidence_time_basis: "received_at",
            target_match: { status: "hit", organic_ranks: [1] },
          },
        },
      ],
    };
    const created = vi.fn(() => "blob:saved-measurement-pdf");
    const revoked = vi.fn();
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
        expect(this.download).toBe("measurement-report-synthetic-saved-r2.pdf");
      });
    const drawn = vi.spyOn(PDFPage.prototype, "drawText");
    await downloadMeasurementReportPdf(input(report));
    const text = drawn.mock.calls.map(([value]) => value).join("\n");
    expect(text).toContain("Search target found at organic ranks: 1");
    expect(text).toContain("Original search title 原题");
    expect(text).toContain("synthetic-raw-evidence");
    expect(text).toContain("synthetic-raw-sha");
    expect(text).toContain("SYNTHETIC_SOURCE_LIMIT");
    expect(text).toContain("synthetic-prior");
    expect(click).toHaveBeenCalledOnce();
    expect(created).toHaveBeenCalledOnce();
    expect(revoked).toHaveBeenCalledWith("blob:saved-measurement-pdf");
  }, 20_000);

  it("rejects cancellation before a font request or after a cancelled in-flight font response", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      downloadMeasurementReportPdf(input(), { signal: controller.signal }),
    ).rejects.toMatchObject({ name: "AbortError" });
    expect(fetch).not.toHaveBeenCalled();
    const next = new AbortController();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        next.abort();
        return new Response(fontBytes, { status: 200 });
      }),
    );
    const drawn = vi.spyOn(PDFPage.prototype, "drawText");
    await expect(
      buildMeasurementReportPdf(input(), { signal: next.signal }),
    ).rejects.toMatchObject({ name: "AbortError" });
    expect(drawn).not.toHaveBeenCalled();
  });

  it("processes queued cancellation while building a one-sample report", async () => {
    const report = preview();
    report.samples = report.samples.slice(0, 1);
    const controller = new AbortController();
    const created = vi.fn(() => "blob:cancelled-measurement-pdf");
    vi.stubGlobal(
      "URL",
      Object.assign(URL, {
        createObjectURL: created,
        revokeObjectURL: vi.fn(),
      }),
    );
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    const timer = setTimeout(() => controller.abort(), 0);
    try {
      await expect(
        downloadMeasurementReportPdf(input(report), {
          signal: controller.signal,
        }),
      ).rejects.toMatchObject({ name: "AbortError" });
      expect(created).not.toHaveBeenCalled();
      expect(click).not.toHaveBeenCalled();
    } finally {
      clearTimeout(timer);
    }
  });

  it("reports font failures in the captured locale without creating a download", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("unavailable", { status: 404 })),
    );
    await expect(buildMeasurementReportPdf(input())).rejects.toThrow(
      "The report font could not be downloaded",
    );
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(new Uint8Array(), { status: 200 })),
    );
    await expect(buildMeasurementReportPdf(input())).rejects.toThrow(
      "The report font is unavailable",
    );
  });
});
