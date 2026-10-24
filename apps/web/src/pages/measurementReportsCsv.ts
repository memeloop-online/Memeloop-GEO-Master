import type { MeasurementPeriodSample } from "../api/measurementReports";
import { isMeasurementFocus } from "../api/measurementFocus";
import { decodeVerifiedRecommendations } from "../api/recommendations";
import {
  comparisonKey,
  questionComparisonKey,
  recommendationFrequency,
} from "../components/recommendationComparison";
import { measurementReportExportTranslations } from "../i18n/measurementReportExport";
import { serializeCsv, type CsvCell } from "./csv";
import type { MeasurementReportExportInput } from "./measurementReportExport";

const columns = [
  "section",
  "scope",
  "group_or_id",
  "field",
  "value",
  "value_type",
  "section_label",
  "scope_label",
  "field_label",
  "value_label",
] as const;

/** Export the captured view using frozen evidence and shared comparison semantics. */
export function measurementReportCsv(
  input: MeasurementReportExportInput,
): string {
  const { report, view, locale } = input;
  const labels: Record<string, string> =
    measurementReportExportTranslations[locale];
  const label = (key: string) => labels[key] ?? labels.stored_field;
  const number = new Intl.NumberFormat(locale, { maximumFractionDigits: 6 });
  const percent = new Intl.NumberFormat(locale, {
    style: "percent",
    maximumFractionDigits: 6,
  });
  const rows: CsvCell[][] = [
    [...columns],
    columns.map((field) => label(field)),
  ];
  const date = (value: string) => {
    const timestamp = Date.parse(value);
    if (!Number.isFinite(timestamp)) return labels.unknown;
    try {
      return new Intl.DateTimeFormat(locale, {
        dateStyle: "medium",
        timeStyle: "short",
        timeZone: report.report_timezone || "UTC",
      }).format(timestamp);
    } catch {
      // A historical invalid timezone must not erase or reinterpret the raw value.
      return value;
    }
  };
  const add = (
    section: string,
    scope: string,
    group: string,
    field: string,
    value: unknown,
    description?: string,
  ) => {
    const tail =
      field
        .split(".")
        .filter((part) => !/^\d+$/u.test(part))
        .at(-1) ?? field;
    const type =
      value === null ? "null" : Array.isArray(value) ? "array" : typeof value;
    const raw: CsvCell =
      value == null
        ? null
        : typeof value === "number"
          ? value
          : typeof value === "object"
            ? JSON.stringify(value)
            : String(value);
    const human =
      description ??
      (value == null
        ? labels.unknown
        : typeof value === "boolean"
          ? label(value ? "yes" : "no")
          : typeof value === "number"
            ? (["recommendation_rate", "first_place_share"].includes(field)
                ? percent
                : number
              ).format(value)
            : (tail.endsWith("_at") ||
                  ["evidence_time", "evidence_as_of"].includes(tail)) &&
                typeof value === "string"
              ? date(value)
              : tail === "kind" && typeof value === "string"
                ? label(value)
                : ["status", "original_status"].includes(tail) &&
                    typeof value === "string"
                  ? label(`status_${value}`)
                  : "");
    rows.push([
      section,
      scope,
      group,
      field,
      raw,
      type,
      label(section),
      label(scope),
      label(tail),
      human,
    ]);
  };
  // Keep each frozen field, including unknown future nested evidence, without translating content.
  const stored = (
    section: string,
    scope: string,
    group: string,
    field: string,
    value: unknown,
  ) => {
    if (
      value !== null &&
      typeof value === "object" &&
      Object.keys(value).length
    ) {
      for (const [key, nested] of Object.entries(value))
        stored(section, scope, group, `${field}.${key}`, nested);
    } else add(section, scope, group, field, value);
  };
  const saved = report.kind === "measurement_period";
  const reportFields = {
    snapshot_state: saved ? "saved" : "unsaved_preview",
    report_id: saved ? report.report_id : null,
    revision: saved ? report.revision : null,
    correction_of: saved ? report.correction_of : null,
    kind: report.kind,
    project_id: report.project_id,
    question_set: report.question_set ?? null,
    report_window_start_at: report.report_window_start_at,
    report_window_end_at: report.report_window_end_at,
    report_timezone: report.report_timezone,
    evidence_as_of: report.evidence_as_of,
    generated_at: report.generated_at,
    input_hash: report.input_hash,
  };
  for (const [field, value] of Object.entries(reportFields)) {
    if (field === "snapshot_state")
      add("report", "whole_report", "", field, value, label(value as string));
    else stored("report", "whole_report", "", field, value);
  }
  stored("report_coverage", "whole_report", "", "coverage", report.coverage);
  const samples = report.samples.filter(
    (sample) =>
      view.question === null || sample.conditions.question === view.question,
  );
  add(
    "view",
    "filtered_ai_view",
    "",
    "question_scope",
    view.question === null ? "all_questions" : "exact_question",
    label(view.question === null ? "all_questions" : "exact_question"),
  );
  add("view", "filtered_ai_view", "", "question", view.question);
  add(
    "view",
    "filtered_ai_view",
    "",
    "target_scope",
    view.target ? "selected_target" : "no_target",
    label(view.target ? "selected_target" : "no_target"),
  );
  stored("view", "filtered_ai_view", "", "target", view.target);
  add("view", "filtered_ai_view", "", "sample_count", samples.length);
  const answers = samples.filter((sample) =>
    Boolean(sample.observation?.raw_answer.trim()),
  ).length;
  add("view", "filtered_ai_view", "", "answer_available_count", answers);
  add(
    "view",
    "filtered_ai_view",
    "",
    "answer_unavailable_count",
    samples.length - answers,
  );
  const measurement = (sample: MeasurementPeriodSample) => ({
    ...sample.conditions,
    question_binding: sample.question_binding ?? undefined,
  });
  const groups = new Map<string, MeasurementPeriodSample[]>();
  for (const sample of samples) {
    const key = comparisonKey(measurement(sample));
    const group = groups.get(key) ?? [];
    group.push(sample);
    groups.set(key, group);
  }
  for (const [key, group] of groups) {
    const first = group[0];
    const entries = group.map((sample) => ({
      items: decodeVerifiedRecommendations(sample.recommendations),
    }));
    add(
      "group",
      "filtered_ai_view",
      key,
      "question_comparison_key",
      questionComparisonKey(measurement(first)),
    );
    add("group", "filtered_ai_view", key, "comparison_key", key);
    stored("group", "filtered_ai_view", key, "conditions", first.conditions);
    stored(
      "group",
      "filtered_ai_view",
      key,
      "question_binding",
      first.question_binding ?? null,
    );
    add("group", "filtered_ai_view", key, "sample_count", group.length);
    const frequency = view.target
      ? recommendationFrequency(entries, view.target)
      : null;
    const valid =
      frequency?.valid ??
      entries.filter((entry) => entry.items !== null).length;
    add("group", "filtered_ai_view", key, "valid", valid);
    add(
      "group",
      "filtered_ai_view",
      key,
      "unavailable_count",
      group.length - valid,
    );
    for (const [field, value] of Object.entries({
      recommended: frequency?.recommended ?? null,
      ranked: frequency?.ranked ?? null,
      first_place: frequency ? (frequency.ranks.get(1) ?? 0) : null,
      unranked: frequency?.unranked ?? null,
      conflicting_rank: frequency?.ambiguous ?? null,
    }))
      add(
        "group",
        "filtered_ai_view",
        key,
        field,
        value,
        !view.target ? labels.not_applicable : undefined,
      );
    add(
      "group",
      "filtered_ai_view",
      key,
      "recommendation_rate",
      frequency && valid ? frequency.recommended / valid : null,
      !view.target
        ? labels.not_applicable
        : !valid
          ? labels.no_valid
          : undefined,
    );
    add(
      "group",
      "filtered_ai_view",
      key,
      "first_place_share",
      frequency?.ranked
        ? (frequency.ranks.get(1) ?? 0) / frequency.ranked
        : null,
      !view.target
        ? labels.not_applicable
        : !frequency?.ranked
          ? labels.no_ranked
          : undefined,
    );
    stored(
      "group",
      "filtered_ai_view",
      key,
      "rank_buckets",
      frequency
        ? Object.fromEntries([...frequency.ranks].sort(([a], [b]) => a - b))
        : null,
    );
    const times = group
      .map((sample) =>
        sample.observation?.observed_at
          ? Date.parse(sample.observation.observed_at)
          : NaN,
      )
      .filter(Number.isFinite);
    add(
      "group",
      "filtered_ai_view",
      key,
      "actual_observed_start_at",
      times.length
        ? new Date(
            times.reduce((first, time) => Math.min(first, time)),
          ).toISOString()
        : null,
    );
    add(
      "group",
      "filtered_ai_view",
      key,
      "actual_observed_end_at",
      times.length
        ? new Date(
            times.reduce((last, time) => Math.max(last, time)),
          ).toISOString()
        : null,
    );
    add(
      "group",
      "filtered_ai_view",
      key,
      "actual_observed_unknown_count",
      group.length - times.length,
    );
  }
  samples.forEach((sample, index) => {
    const id = JSON.stringify([
      sample.plan_id,
      sample.target_id,
      sample.attempt_id ?? null,
      index,
    ]);
    for (const [field, value] of Object.entries(sample))
      stored(
        "sample",
        "filtered_ai_view",
        id,
        field === "comparison_key" ? "frozen_comparison_key" : field,
        value,
      );
    const items = decodeVerifiedRecommendations(sample.recommendations);
    const state =
      items === null
        ? "unavailable"
        : items.length
          ? "verified"
          : "verified_empty";
    add(
      "sample",
      "filtered_ai_view",
      id,
      "recommendation_status",
      state,
      label(state),
    );
    items?.forEach((item, itemIndex) => {
      const occurrence = JSON.stringify([id, itemIndex]);
      for (const [field, value] of Object.entries(item))
        stored("recommendation", "filtered_ai_view", occurrence, field, value);
      add(
        "recommendation",
        "filtered_ai_view",
        occurrence,
        "selected_target_match",
        view.target ? isMeasurementFocus(item, [view.target]) : null,
        view.target ? undefined : labels.not_applicable,
      );
    });
  });
  const inclusion = report.search == null ? "not_included" : "included";
  add(
    "search",
    "whole_report_search",
    "",
    "search_inclusion",
    inclusion,
    label(inclusion),
  );
  if (report.search) {
    stored(
      "search",
      "whole_report_search",
      "",
      "schema_version",
      report.search.schema_version,
    );
    stored(
      "search",
      "whole_report_search",
      "",
      "coverage",
      report.search.coverage,
    );
    report.search.samples.forEach((sample, index) =>
      stored(
        "search",
        "whole_report_search",
        JSON.stringify([sample.cohort.measurement_id, index]),
        "sample",
        sample,
      ),
    );
  }
  return serializeCsv(rows);
}

/** Browser-only download: no report save, refresh or external request. */
export function downloadMeasurementReportCsv(
  input: MeasurementReportExportInput,
): void {
  const blob = new Blob([measurementReportCsv(input)], {
    type: "text/csv;charset=utf-8",
  });
  const url = URL.createObjectURL(blob);
  let link: HTMLAnchorElement | undefined;
  try {
    link = document.createElement("a");
    link.href = url;
    const { report } = input;
    const identity =
      report.kind === "measurement_period"
        ? `${report.report_id}-revision-${report.revision}`
        : `preview-${report.report_window_start_at}-${report.report_window_end_at}-${report.generated_at}-${report.input_hash}`;
    link.download = `measurement-${identity.replace(/[^a-zA-Z0-9._-]/gu, "_")}.csv`;
    link.hidden = true;
    document.body.append(link);
    link.click();
  } finally {
    link?.remove();
    URL.revokeObjectURL(url);
  }
}
