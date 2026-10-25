import type { MeasurementPeriodSample } from "../api/measurementReports";
import { isMeasurementFocus } from "../api/measurementFocus";
import { decodeVerifiedRecommendations } from "../api/recommendations";
import {
  comparisonKey,
  recommendationFrequency,
} from "../components/recommendationComparison";
import { measurementReportExportTranslations } from "../i18n/measurementReportExport";
import { measurementReportPdfTranslations } from "../i18n/measurementReportPdf";
import {
  captureMeasurementReportExport,
  type MeasurementReportExportInput,
} from "./measurementReportExport";
import {
  PdfLayout,
  createPdfDocument,
  loadPdfFont,
  downloadPdfBytes,
  yieldToBrowser,
} from "./pdfLayout";

/** Build only from captured report evidence, viewing preferences and language. */
export async function buildMeasurementReportPdf(
  input: MeasurementReportExportInput,
  options: { signal?: AbortSignal } = {},
): Promise<Uint8Array> {
  const { signal } = options;
  signal?.throwIfAborted();
  const { report, view, locale } = captureMeasurementReportExport(input);
  const labels: Record<string, string> = {
    ...measurementReportExportTranslations[locale],
    ...measurementReportPdfTranslations[locale],
  };
  const t = (key: string, values: Record<string, string | number> = {}) =>
    (labels[key] ?? labels.stored_field).replace(
      /\{\{(\w+)\}\}/gu,
      (_, name: string) => String(values[name] ?? ""),
    );
  const number = new Intl.NumberFormat(locale);
  const percent = new Intl.NumberFormat(locale, {
    style: "percent",
    maximumFractionDigits: 1,
  });
  const errors = {
    missing: t("fontMissing"),
    invalid: t("fontInvalid"),
    download: t("fontDownload"),
    incomplete: t("fontIncomplete"),
  };
  const fontBytes = await loadPdfFont(errors, signal);
  signal?.throwIfAborted();
  const { document, font } = await createPdfDocument(fontBytes, errors);
  signal?.throwIfAborted();
  const saved = report.kind === "measurement_period";
  const identity = saved
    ? `${report.report_id} · ${number.format(report.revision)}`
    : t("unsaved_preview");
  document.setTitle(`${t("title")} · ${identity}`);
  document.setSubject(t("scope"));
  document.setCreator("GEO");
  document.setProducer("GEO PDF export");
  const generated = new Date(report.generated_at);
  if (Number.isFinite(generated.getTime())) {
    document.setCreationDate(generated);
    document.setModificationDate(generated);
  }
  const layout = new PdfLayout(
    document,
    font,
    (page, total) =>
      t(saved ? "footerSaved" : "footerPreview", {
        page: number.format(page),
        total: number.format(total),
        revision: saved ? number.format(report.revision) : "",
      }),
    signal,
    true,
  );
  const show = (value: unknown): string =>
    value == null
      ? t("unknown")
      : typeof value === "number"
        ? number.format(value)
        : typeof value === "boolean"
          ? t(value ? "yes" : "no")
          : String(value);
  const field = (key: string, value: unknown) =>
    layout.text(`${t(key)}: ${show(value)}`, { indent: 10 });
  let date: Intl.DateTimeFormat | null = null;
  try {
    date = new Intl.DateTimeFormat(locale, {
      dateStyle: "medium",
      timeStyle: "long",
      timeZone: report.report_timezone || "UTC",
    });
  } catch {
    /* Historical timezone values must not erase original timestamps. */
  }
  const at = (value: string | null | undefined) => {
    if (!value) return t("unknown");
    const timestamp = Date.parse(value);
    return Number.isFinite(timestamp) && date ? date.format(timestamp) : value;
  };
  const timestamp = (key: string, value: string | null | undefined) => {
    field(key, at(value));
    if (value)
      layout.text(`${t("rawTime")}: ${value}`, {
        indent: 20,
        size: 8,
        muted: true,
      });
  };
  // Detailed frozen structures belong in the appendix, under readable field labels.
  const stored = (key: string, value: unknown, indent = 10) => {
    signal?.throwIfAborted();
    const title = labels[key] ?? `${t("stored_field")} (${key})`;
    if (
      value !== null &&
      typeof value === "object" &&
      Object.keys(value).length
    ) {
      layout.text(title, { indent, size: 8, gap: 1, muted: true });
      for (const [nested, item] of Object.entries(value))
        stored(nested, item, Math.min(indent + 8, 42));
    } else {
      layout.text(`${title}: ${Array.isArray(value) ? "[]" : show(value)}`, {
        indent,
        size: 8,
        gap: 1,
      });
    }
  };
  const samples = report.samples.filter(
    (sample) =>
      view.question === null || sample.conditions.question === view.question,
  );
  const answers = samples.filter((sample) =>
    Boolean(sample.observation?.raw_answer.trim()),
  ).length;
  const groups = new Map<string, MeasurementPeriodSample[]>();
  for (const sample of samples) {
    const key = comparisonKey({
      ...sample.conditions,
      question_binding: sample.question_binding ?? undefined,
    });
    const group = groups.get(key) ?? [];
    group.push(sample);
    groups.set(key, group);
  }
  const comparisonIndices = new Map<MeasurementPeriodSample, number>();
  const comparisons = [...groups.values()].map((group, index) => {
    for (const sample of group) comparisonIndices.set(sample, index + 1);
    const entries = group.map((sample) => ({
      items: decodeVerifiedRecommendations(sample.recommendations),
    }));
    const frequency = view.target
      ? recommendationFrequency(entries, view.target)
      : null;
    return {
      group,
      frequency,
      valid:
        frequency?.valid ??
        entries.filter((entry) => entry.items !== null).length,
    };
  });
  const rate = (count: number, total: number, empty: string) =>
    total
      ? t("rate", {
          count: number.format(count),
          total: number.format(total),
          percentage: percent.format(count / total),
        })
      : t(empty);
  const rates = (
    frequency: NonNullable<(typeof comparisons)[number]["frequency"]>,
  ) => {
    field(
      "recommendationRate",
      rate(frequency.recommended, frequency.valid, "no_valid"),
    );
    field(
      "firstPlaceShare",
      rate(frequency.ranks.get(1) ?? 0, frequency.ranked, "no_ranked"),
    );
  };
  layout.text(t("title"), { size: 21, gap: 10 });
  layout.text(
    saved
      ? t("savedHeading", { revision: number.format(report.revision) })
      : t("unsaved_preview"),
    { size: 11, gap: 8 },
  );
  layout.heading(t("summary"));
  layout.text(
    t("sampleSummary", {
      samples: number.format(samples.length),
      answers: number.format(answers),
      missing: number.format(samples.length - answers),
    }),
    { size: 11, gap: 8 },
  );
  field("question_scope", view.question ?? t("all_questions"));
  if (view.target) {
    field("target", `${view.target.name} · ${t(view.target.kind)}`);
    field("aliases", view.target.aliases.join(", ") || t("unknown"));
  }
  for (const [index, comparison] of comparisons.entries()) {
    await yieldToBrowser(index, signal);
    const first = comparison.group[0];
    layout.text(
      `${t("group", { number: number.format(index + 1) })} · ${first.conditions.provider} · ${first.conditions.model}`,
      { size: 10, gap: 6 },
    );
    layout.text(first.conditions.question, { indent: 10 });
    if (comparison.frequency) rates(comparison.frequency);
    else field("validSamples", comparison.valid);
  }
  layout.heading(t("reading"));
  layout.text(t(view.target ? "currentTarget" : "noTarget"), { muted: true });
  layout.text(t("scope"), { muted: true });
  layout.text(t("limits"), { muted: true });
  layout.text(t("basis"), { muted: true });
  field("report_timezone", report.report_timezone || "UTC");
  if (!report.report_timezone)
    layout.text(t("timezoneFallback"), { muted: true });
  if (!date) layout.text(t("timezoneInvalid"), { muted: true });
  field("report_window_start_at", at(report.report_window_start_at));
  field("report_window_end_at", at(report.report_window_end_at));
  layout.heading(t("report_coverage"));
  field("planned", report.coverage.planned);
  field("observed_live", report.coverage.observed_live);
  field("grounded_saved_analysis", report.coverage.grounded_saved_analysis);
  for (const [status, count] of Object.entries(report.coverage.counts))
    field(`status_${status}`, count);

  layout.heading(t("groups"));
  layout.text(t("groupScope"), { muted: true });
  if (!samples.length) layout.text(t("noSamples"));
  for (const [index, { group, frequency, valid }] of comparisons.entries()) {
    await yieldToBrowser(index, signal);
    const first = group[0];
    layout.heading(
      `${t("group", { number: number.format(index + 1) })} · ${first.conditions.provider}`,
    );
    layout.text(first.conditions.question, { size: 11, gap: 7 });
    for (const key of [
      "model",
      "surface",
      "search_mode",
      "market",
      "language",
      "question_set_version",
    ] as const)
      field(key, first.conditions[key]);
    field("sample_count", group.length);
    field("validSamples", valid);
    field("unavailableSamples", group.length - valid);
    if (frequency) {
      rates(frequency);
      field("unrankedSamples", frequency.unranked);
      field("conflictingSamples", frequency.ambiguous);
      if (!frequency.ranks.size)
        layout.text(t("noRanks"), { indent: 10, muted: true });
      for (const [rank, count] of [...frequency.ranks].sort(
        ([a], [b]) => a - b,
      ))
        layout.text(
          t("rankBucket", {
            rank: number.format(rank),
            count: number.format(count),
          }),
          { indent: 10 },
        );
    }
    const times = group
      .map((sample) =>
        sample.observation?.observed_at
          ? Date.parse(sample.observation.observed_at)
          : NaN,
      )
      .filter(Number.isFinite);
    layout.text(t("actualTimes"), { indent: 10, muted: true });
    timestamp(
      "actual_observed_start_at",
      times.length ? new Date(Math.min(...times)).toISOString() : null,
    );
    timestamp(
      "actual_observed_end_at",
      times.length ? new Date(Math.max(...times)).toISOString() : null,
    );
    field("actual_observed_unknown_count", group.length - times.length);
  }

  layout.heading(t("searchTitle"));
  layout.text(t("searchSeparate"), { muted: true });
  if (!report.search) layout.text(t("not_included"));
  else {
    field("planned", report.search.coverage.planned);
    for (const [status, count] of Object.entries(report.search.coverage.counts))
      layout.text(
        `${labels[`status_${status}`] ?? status}: ${number.format(count)}`,
        { indent: 10 },
      );
    if (!report.search.samples.length) layout.text(t("noSearchSamples"));
    for (const [index, sample] of report.search.samples.entries()) {
      await yieldToBrowser(index, signal);
      layout.text(sample.cohort.query, { size: 11, gap: 7 });
      field("source_key", sample.cohort.source_key);
      if (!sample.evidence) layout.text(t("noSearchEvidence"));
      else {
        const match = sample.evidence.target_match;
        layout.text(
          match.status === "hit"
            ? t("searchHit", {
                ranks: match.organic_ranks
                  .map((rank) => number.format(rank))
                  .join(", "),
              })
            : match.status === "not_found_within_depth"
              ? t("searchNotFound", {
                  depth: number.format(match.covered_depth),
                })
              : t(
                  match.status === "undetermined"
                    ? "searchUnknown"
                    : "searchNotRequested",
                ),
        );
        field(
          "status",
          labels[`status_${sample.evidence.observation.status}`] ??
            sample.evidence.observation.status,
        );
        timestamp("evidence_time", sample.evidence.evidence_time);
        for (const result of sample.evidence.observation.results) {
          layout.text(result.title ?? t("searchResult"), { indent: 10 });
          field("organic_rank", result.organic_rank);
          field("raw_url", result.raw_url);
        }
      }
    }
  }

  layout.heading(t("appendix"));
  layout.text(t("unsupported"), { size: 8, muted: true });
  for (const [index, sample] of samples.entries()) {
    await yieldToBrowser(index, signal);
    layout.heading(
      `${t("record", { number: number.format(index + 1) })} · ${t("group", { number: number.format(comparisonIndices.get(sample)!) })} · ${sample.conditions.provider}`,
    );
    layout.text(sample.conditions.question, { size: 11 });
    layout.text(
      `${t("original_status")}: ${labels[`status_${sample.original_status}`] ?? sample.original_status} · ${t("observed_live")}: ${show(sample.observed_live)}`,
      { size: 8, muted: true, gap: 1 },
    );
    for (const key of ["observed_at", "received_at"] as const) {
      const value = sample.observation?.[key];
      layout.text(`${t(key)}: ${at(value)}${value ? ` · ${value}` : ""}`, {
        size: 8,
        muted: true,
        gap: 1,
      });
    }
    layout.text(t("raw_answer"), { size: 10, gap: 6 });
    layout.text(sample.observation?.raw_answer ?? t("missing"), { gap: 8 });
    for (const citation of sample.observation?.citations ?? [])
      field("citations", citation);
    if (!sample.observation?.citations.length)
      layout.text(t("noCitations"), { muted: true });
    const items = decodeVerifiedRecommendations(sample.recommendations);
    layout.text(
      t(
        items === null
          ? "unavailable"
          : items.length
            ? "verified"
            : "verified_empty",
      ),
      { size: 10, gap: 7 },
    );
    for (const item of items ?? []) {
      layout.text(`${item.name} · ${t(item.kind)}`, { size: 10, gap: 3 });
      layout.text(
        `${t("rank")}: ${show(item.rank)} · ${t("mention_order")}: ${show(item.mention_order)}${view.target ? ` · ${t("selected")}: ${show(isMeasurementFocus(item, [view.target]))}` : ""}`,
        { indent: 10, size: 8, gap: 2 },
      );
      field("reason", item.reason);
      field("evidence_quote", item.evidence_quote);
      for (const url of item.citation_urls) field("citation_urls", url);
    }
    layout.text(
      ["plan_id", "target_id", "attempt_id"]
        .map(
          (key) =>
            `${t(key)}: ${show(sample[key as "plan_id" | "target_id" | "attempt_id"])}`,
        )
        .join(" · "),
      { size: 8, gap: 1, muted: true },
    );
    layout.text(
      `${t("scheduled_at")}: ${at(sample.scheduled_at)} · ${sample.scheduled_at}`,
      { size: 8, gap: 1, muted: true },
    );
    stored("frozen_comparison_key", sample.comparison_key);
    stored("sample_ordinal", sample.conditions.sample_ordinal);
    stored("protocol_version", sample.conditions.protocol_version);
    stored("question_binding", sample.question_binding ?? null);
    stored("provenance", sample.observation?.provenance ?? null);
    // Verified recommendations are presented once above; unavailable extraction
    // details remain evidence and cannot be replaced with an empty result.
    if (items === null)
      stored("recommendations", sample.recommendations ?? null);
  }
  if (report.search) {
    layout.heading(t("searchAppendix"));
    stored("schema_version", report.search.schema_version);
    for (const [index, sample] of report.search.samples.entries()) {
      await yieldToBrowser(index, signal);
      layout.heading(
        `${t("record", { number: number.format(index + 1) })} · ${sample.cohort.query}`,
      );
      stored("cohort", sample.cohort);
      stored("evidence", sample.evidence);
    }
  }
  layout.heading(t("metadata"));
  layout.text(t(saved ? "savedNote" : "previewNote"));
  field("snapshot_state", t(saved ? "saved" : "unsaved_preview"));
  if (saved) {
    field("report_id", report.report_id);
    field("revision", report.revision);
    field("correction_of", report.correction_of);
  }
  field("project_id", report.project_id);
  field("kind", t(report.kind));
  field("report_timezone", report.report_timezone);
  stored("question_set", report.question_set ?? null);
  timestamp("report_window_start_at", report.report_window_start_at);
  timestamp("report_window_end_at", report.report_window_end_at);
  timestamp("evidence_as_of", report.evidence_as_of);
  timestamp("generated_at", report.generated_at);
  field("input_hash", report.input_hash);
  stored("coverage", report.coverage);
  stored("question", view.question);
  stored("target", view.target);
  layout.finish();
  try {
    signal?.throwIfAborted();
    const bytes = await document.save();
    signal?.throwIfAborted();
    return bytes;
  } catch {
    signal?.throwIfAborted();
    throw new Error(t("buildFailed"));
  }
}

export async function downloadMeasurementReportPdf(
  input: MeasurementReportExportInput,
  options: { signal?: AbortSignal } = {},
): Promise<void> {
  options.signal?.throwIfAborted();
  const captured = captureMeasurementReportExport(input);
  const bytes = await buildMeasurementReportPdf(captured, options);
  const report = captured.report;
  const filename =
    report.kind === "measurement_period"
      ? `measurement-report-${report.report_id}-r${report.revision}.pdf`
      : "measurement-report-preview.pdf";
  downloadPdfBytes(bytes, filename, options.signal);
}
