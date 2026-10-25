import { afterEach, describe, expect, it, vi } from "vitest";
import Papa from "papaparse";
import i18n from "../i18n";
import type {
  MeasurementPeriodPreview,
  MeasurementPeriodReport,
  MeasurementPeriodSample,
} from "../api/measurementReports";
import type { RecommendationItem } from "../api/recommendations";
import { recommendationFrequency } from "../components/recommendationComparison";
import {
  captureMeasurementReportExport,
  type MeasurementReportExportInput,
} from "./measurementReportExport";
import {
  downloadMeasurementReportCsv,
  measurementReportCsv,
} from "./measurementReportsCsv";

const item = (
  name = "Synthetic alias",
  rank: number | null = 1,
  kind: RecommendationItem["kind"] = "product",
): RecommendationItem => ({
  name,
  rank,
  kind,
  mention_order: 1,
  reason: 'Reason, "quoted"\n原文',
  evidence_quote: "Original evidence 原句",
  brand_attribution: null,
  citation_urls: ["https://example.test/source"],
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
  comparison_key: "opaque-frozen-key",
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
    observed_at: "2026-01-02T01:00:00+01:00",
    received_at: "2026-01-02T00:01:00Z",
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
    sample("unavailable", null, {
      observation: null,
      original_status: "missing",
    }),
    sample(
      "conflict",
      verified([item(), { ...item("Synthetic item", 2), mention_order: 2 }]),
    ),
    sample("unranked", verified([item("Synthetic item", null)])),
    sample("wrong-kind", verified([item("Synthetic item", 1, "brand")])),
  ],
});
const input = (report = preview()): MeasurementReportExportInput => ({
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
type Row = Record<string, string>;
const rows = (value: string): Row[] =>
  Papa.parse<Row>(value, { header: true, skipEmptyLines: true }).data.slice(1);
const find = (data: Row[], section: string, field: string) =>
  data.filter((row) => row.section === section && row.field === field);
const value = (data: Row[], section: string, field: string) =>
  find(data, section, field)[0]?.value;

afterEach(async () => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  await i18n.changeLanguage("zh-CN");
});

describe("independent measurement CSV", () => {
  it.each(["en", "zh-CN"] as const)(
    "keeps direct brand metrics separate from affiliated products and unknown attribution in %s",
    (locale) => {
      const attributed = (name: string): RecommendationItem => ({
        ...item("Synthetic product", 1),
        brand_attribution: {
          name,
          evidence_quote: "Original brand relationship 品牌原句",
        },
      });
      const answers = [
        [item("Synthetic brand", 2, "brand")],
        [
          attributed("Synthetic brand alias"),
          { ...attributed("Synthetic brand"), mention_order: 2 },
        ],
        [
          attributed("Synthetic brand"),
          { ...item("Unknown product"), mention_order: 2 },
        ],
        [attributed("Other brand")],
        [item("Unknown product")],
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
      const data = rows(measurementReportCsv(request));
      expect(value(data, "group", "recommended")).toBe(
        String(frequency.recommended),
      );
      expect(value(data, "group", "recommended")).toBe("1");
      expect(value(data, "group", "valid")).toBe("6");
      expect(value(data, "group", "rank_buckets.2")).toBe("1");
      expect(value(data, "group", "rank_buckets.1")).toBeUndefined();
      expect(value(data, "group", "first_place")).toBe("0");
      expect(value(data, "group", "brand_affiliate_positive")).toBe("2");
      expect(value(data, "group", "brand_attribution_known")).toBe("5");
      expect(value(data, "group", "brand_attribution_unknown")).toBe("1");
      expect(value(data, "group", "brand_attribution_valid")).toBe("6");
      expect(
        find(data, "recommendation", "brand_attribution.name").map(
          (row) => row.value,
        ),
      ).toContain("Synthetic brand alias");
      expect(
        find(data, "recommendation", "brand_attribution.evidence_quote").map(
          (row) => row.value,
        ),
      ).toContain("Original brand relationship 品牌原句");
      expect(
        find(data, "recommendation", "brand_attribution").some(
          (row) =>
            row.value_type === "null" &&
            row.value_label ===
              (locale === "en" ? "Unknown brand attribution" : "品牌归属未知"),
        ),
      ).toBe(true);
      expect(
        data
          .filter((row) => row.field.startsWith("brand_"))
          .every((row) => !row.field.endsWith("rate")),
      ).toBe(true);
      report.samples = report.samples.slice(1);
      const affiliateOnly = rows(measurementReportCsv(request));
      expect(value(affiliateOnly, "group", "recommended")).toBe("0");
      expect(value(affiliateOnly, "group", "brand_affiliate_positive")).toBe(
        "2",
      );
      expect(value(affiliateOnly, "group", "ranked")).toBe("0");
      expect(value(affiliateOnly, "group", "first_place_share")).toBe("");
      request.view.target = null;
      const withoutTarget = rows(measurementReportCsv(request));
      expect(
        find(withoutTarget, "group", "brand_affiliate_positive"),
      ).toHaveLength(0);
      expect(value(withoutTarget, "group", "recommended")).toBe("");
      expect(
        find(
          withoutTarget,
          "recommendation",
          "brand_attribution.evidence_quote",
        ),
      ).toHaveLength(4);
    },
  );

  it.each(["en", "zh-CN"] as const)(
    "exports a genuinely unsaved preview and exact saved correction in %s",
    (locale) => {
      const request = { ...input(), locale };
      const data = rows(measurementReportCsv(request));
      expect(value(data, "report", "snapshot_state")).toBe("unsaved_preview");
      expect(value(data, "report", "report_id")).toBe("");
      expect(value(data, "report", "revision")).toBe("");
      expect(value(data, "report", "generated_at")).toBe(
        request.report.generated_at,
      );
      expect(value(data, "report", "input_hash")).toBe("synthetic-hash");
      expect(find(data, "report", "snapshot_state")[0].value_label).toBe(
        locale === "en" ? "Unsaved preview" : "未保存预览",
      );
      const saved: MeasurementPeriodReport = {
        ...request.report,
        kind: "measurement_period",
        report_id: "synthetic-report",
        revision: 3,
        correction_of: "synthetic-original",
      };
      const fixed = rows(measurementReportCsv({ ...request, report: saved }));
      expect(value(fixed, "report", "report_id")).toBe("synthetic-report");
      expect(value(fixed, "report", "revision")).toBe("3");
      expect(value(fixed, "report", "correction_of")).toBe(
        "synthetic-original",
      );
      expect(find(fixed, "report", "evidence_as_of")[0].value_label).not.toBe(
        "",
      );
    },
  );

  it("uses shared denominators, explicit aliases and kinds; counts conflicts and unknown times separately", () => {
    const data = rows(measurementReportCsv(input()));
    expect(value(data, "group", "valid")).toBe("5");
    expect(value(data, "group", "unavailable_count")).toBe("1");
    expect(value(data, "group", "recommended")).toBe("3");
    expect(value(data, "group", "ranked")).toBe("1");
    expect(value(data, "group", "first_place")).toBe("1");
    expect(value(data, "group", "first_place_share")).toBe("1");
    expect(value(data, "group", "recommendation_rate")).toBe("0.6");
    expect(value(data, "group", "unranked")).toBe("1");
    expect(value(data, "group", "conflicting_rank")).toBe("1");
    expect(value(data, "group", "rank_buckets.1")).toBe("1");
    expect(value(data, "group", "actual_observed_unknown_count")).toBe("1");
    expect(value(data, "group", "actual_observed_start_at")).toBe(
      "2026-01-02T00:00:00.000Z",
    );
  });

  it("leaves rates absent for zero denominators and preserves verified-empty versus unavailable", () => {
    for (const recommendations of [null, verified([])]) {
      const report = preview();
      report.samples = [sample("zero", recommendations, { observation: null })];
      const data = rows(measurementReportCsv(input(report)));
      expect(value(data, "group", "first_place_share")).toBe("");
      expect(value(data, "group", "actual_observed_start_at")).toBe("");
      expect(value(data, "group", "actual_observed_unknown_count")).toBe("1");
      expect(value(data, "sample", "recommendation_status")).toBe(
        recommendations ? "verified_empty" : "unavailable",
      );
      expect(value(data, "group", "recommendation_rate")).toBe(
        recommendations ? "0" : "",
      );
    }
  });

  it("uses the selected nonfirst target and exact question text while preserving question references and condition partitions", () => {
    const report = preview();
    const reference = {
      question_set_id: "synthetic-set",
      question_set_version_id: "synthetic-version",
      question_id: "synthetic-question",
      question_revision_id: "revision-a",
    };
    report.samples.push(
      sample("other-question", verified([item("Second item")]), {
        conditions: {
          ...report.samples[0].conditions,
          question: "Other question?",
        },
      }),
    );
    report.samples.push(
      sample("referenced", verified([item("Second alias")]), {
        question_binding: {
          reference,
          purpose: "optimization",
          split_policy_version: "split-1",
        },
      }),
    );
    report.samples.push(
      sample("different-condition", verified([item("Second item")]), {
        conditions: { ...report.samples[0].conditions, model: "other-model" },
      }),
    );
    const request = input(report);
    request.view = {
      question: "Synthetic question?",
      target: {
        name: "Second item",
        kind: "product",
        aliases: ["Second alias"],
      },
    };
    const data = rows(measurementReportCsv(request));
    expect(find(data, "group", "valid")).toHaveLength(3);
    expect(
      find(data, "group", "question_comparison_key")
        .map((row) => row.value)
        .some((key) => key.includes("synthetic-question")),
    ).toBe(true);
    expect(
      find(data, "sample", "conditions.question").every(
        (row) => row.value === "Synthetic question?",
      ),
    ).toBe(true);
    expect(value(data, "view", "target.name")).toBe("Second item");
    expect(value(data, "view", "target.aliases.0")).toBe("Second alias");
    expect(find(data, "group", "recommended").map((row) => row.value)).toEqual([
      "0",
      "1",
      "1",
    ]);
    expect(value(data, "report_coverage", "coverage.planned")).toBe("8");
    expect(value(data, "view", "sample_count")).toBe("8");
    request.view.question = "Synthetic question? ";
    expect(
      value(rows(measurementReportCsv(request)), "view", "sample_count"),
    ).toBe("0");
  });

  it("exports raw recommendation evidence without a tracked target and never rewrites source content", () => {
    const request = input();
    request.view.target = null;
    const original = structuredClone(request.report);
    const data = rows(measurementReportCsv(request));
    expect(value(data, "group", "recommended")).toBe("");
    expect(value(data, "recommendation", "reason")).toBe(
      'Reason, "quoted"\n原文',
    );
    expect(value(data, "recommendation", "evidence_quote")).toBe(
      "Original evidence 原句",
    );
    expect(value(data, "recommendation", "citation_urls.0")).toBe(
      "https://example.test/source",
    );
    expect(value(data, "sample", "observation.observed_at")).toBe(
      "2026-01-02T01:00:00+01:00",
    );
    expect(request.report).toEqual(original);
    expect(value(data, "search", "search_inclusion")).toBe("not_included");
  });

  it("preserves every saved analysis source and original timestamp without translating stored content", () => {
    const request = input();
    request.locale = "zh-CN";
    request.report.samples[0].observation!.provenance = {
      revision_id: "synthetic-analysis-revision",
      source: { kind: "capture", capture_id: "synthetic-capture" },
      source_sha256: "synthetic-source-digest",
      observed_at: "2026-01-02T01:00:00+01:00",
      analyzed_at: "2026-01-02T00:02:00.123Z",
      actual_model: "synthetic-analysis-model",
      config_revision: 7,
      prompt_version: "synthetic-prompt",
      parser_version: "synthetic-parser",
    };
    const before = structuredClone(request.report);
    const data = rows(measurementReportCsv(request));
    expect(
      value(data, "sample", "observation.provenance.source.capture_id"),
    ).toBe("synthetic-capture");
    expect(value(data, "sample", "observation.provenance.source_sha256")).toBe(
      "synthetic-source-digest",
    );
    expect(value(data, "sample", "observation.provenance.analyzed_at")).toBe(
      "2026-01-02T00:02:00.123Z",
    );
    expect(value(data, "sample", "observation.provenance.actual_model")).toBe(
      "synthetic-analysis-model",
    );
    expect(value(data, "sample", "observation.raw_answer")).toBe(
      "Original answer 原文",
    );
    expect(request.report).toEqual(before);
  });

  it("keeps search evidence and report-wide coverage outside the current AI question filter", () => {
    const request = input();
    request.view.question = "Absent question";
    request.report.search = {
      schema_version: "synthetic-search-v1",
      coverage: { planned: 1, counts: { observed: 1 } },
      samples: [
        {
          cohort: {
            measurement_id: "synthetic-search",
            source_key: "synthetic-source",
            query: "Unrelated search query",
            protocol: {
              query: "Unrelated search query",
              engine: "synthetic-engine",
              surface: "web",
              source: "synthetic-source",
              country: "test",
              city: null,
              language: "en",
              device: "desktop",
              requested_depth: 1,
            },
            target: null,
            target_rule_version: "target-1",
            question_binding: null,
            scheduled_at: "2026-01-02T00:00:00Z",
            created_at: "2026-01-01T00:00:00Z",
            stored_at: "2026-01-01T00:00:01Z",
          },
          evidence: {
            observation: {
              observation_id: "synthetic-search-observation",
              measurement_id: "synthetic-search",
              attempt_id: "synthetic-search-attempt",
              raw_evidence_id: "synthetic-search-raw",
              raw_sha256: "synthetic-search-digest",
              parser_version: "synthetic-search-parser",
              provider_observed_at: null,
              received_at: "2026-01-02T00:01:00Z",
              analyzed_at: "2026-01-02T00:02:00Z",
              status: "completed",
              actual_conditions: {
                language: { value: "en", evidence_locator: "$.language" },
                city: null,
              },
              coverage: {
                requested_depth: 1,
                observed_organic_depth: 1,
                pages_received: 1,
                completion: "complete",
                truncated: false,
              },
              results: [
                {
                  kind: "organic",
                  raw_kind: "organic",
                  raw_url: "https://example.test/search",
                  normalized_url: "https://example.test/search",
                  host: "example.test",
                  title: "Original search title 原文",
                  page: 1,
                  position: 1,
                  organic_rank: 1,
                  absolute_position: 1,
                  locator: "$.items[0]",
                },
              ],
              source_limitations: ["Original limitation 原文"],
            },
            raw_stored_at: "2026-01-02T00:01:01Z",
            observation_stored_at: "2026-01-02T00:02:01Z",
            evidence_time: "2026-01-02T00:01:00Z",
            evidence_time_basis: "received_at",
            target_match: { status: "not_requested" },
          },
        },
      ],
    };
    const data = rows(measurementReportCsv(request));
    expect(value(data, "view", "sample_count")).toBe("0");
    expect(value(data, "search", "sample.cohort.query")).toBe(
      "Unrelated search query",
    );
    expect(
      data
        .filter((row) => row.section === "search")
        .every((row) => row.scope === "whole_report_search"),
    ).toBe(true);
    expect(value(data, "search", "coverage.planned")).toBe("1");
    expect(
      value(
        data,
        "search",
        "sample.evidence.observation.results.0.organic_rank",
      ),
    ).toBe("1");
    expect(
      value(data, "search", "sample.evidence.observation.results.0.title"),
    ).toBe("Original search title 原文");
    expect(
      value(data, "search", "sample.evidence.observation.source_limitations.0"),
    ).toBe("Original limitation 原文");
    expect(
      value(data, "search", "sample.evidence.observation.provider_observed_at"),
    ).toBe("");
    expect(
      find(
        data,
        "search",
        "sample.evidence.observation.provider_observed_at",
      )[0].value_type,
    ).toBe("null");
    expect(value(data, "search", "sample.evidence.evidence_time_basis")).toBe(
      "received_at",
    );
    expect(
      value(data, "search", "sample.evidence.observation.raw_sha256"),
    ).toBe("synthetic-search-digest");
    expect(value(data, "report_coverage", "coverage.planned")).toBe("8");
  });

  it("binds captured language and values despite later state or global language changes", async () => {
    const request = input();
    const captured = captureMeasurementReportExport(request);
    const first = measurementReportCsv(captured);
    request.report.samples[0].observation!.raw_answer = "Later answer";
    request.view.target!.aliases.push("Later alias");
    request.view.question = "Other question";
    request.locale = "zh-CN";
    await i18n.changeLanguage("zh-CN");
    expect(measurementReportCsv(captured)).toBe(first);
    expect(value(rows(first), "sample", "observation.raw_answer")).toBe(
      "Original answer 原文",
    );
    expect(find(rows(first), "group", "valid")[0].field_label).toContain(
      "Valid recommendation samples",
    );
  });

  it("uses shared escaping for formula-looking stored values and quoted multiline content", () => {
    const request = input();
    request.report.samples[0].observation!.raw_answer =
      '=HYPERLINK("example")\nsecond,line';
    const csv = measurementReportCsv(request);
    expect(csv.startsWith("\uFEFF")).toBe(true);
    expect(csv.endsWith("\r\n")).toBe(true);
    expect(value(rows(csv), "sample", "observation.raw_answer")).toBe(
      '\'=HYPERLINK("example")\nsecond,line',
    );
    expect(Papa.parse(csv).errors).toEqual([]);
  });

  it("downloads only a local blob and always releases the object URL, including failures", () => {
    const create = vi.fn((_blob: Blob) => "blob:synthetic-export");
    const revoke = vi.fn();
    vi.stubGlobal(
      "URL",
      class extends URL {
        static createObjectURL = create;
        static revokeObjectURL = revoke;
      },
    );
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    downloadMeasurementReportCsv(input());
    expect(create).toHaveBeenCalledTimes(1);
    expect(create.mock.calls[0]?.[0]).toBeInstanceOf(Blob);
    expect(click).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith("blob:synthetic-export");
    expect(document.querySelector("a[download]")).toBeNull();
    expect(fetch).not.toHaveBeenCalled();
    click.mockImplementation(() => {
      throw Error("synthetic download failure");
    });
    expect(() => downloadMeasurementReportCsv(input())).toThrow(
      "synthetic download failure",
    );
    expect(revoke).toHaveBeenCalledTimes(2);
    expect(document.querySelector("a[download]")).toBeNull();
  });
});
