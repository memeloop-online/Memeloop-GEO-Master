import type { ReportCoverage, ReportSnapshot } from "../api/reports";

type CsvCell = string | number | null;
type CsvRow = [string, string, string, CsvCell, string];

function safeCell(value: CsvCell): string {
  const text = value === null ? "" : String(value);
  // Quoting CSV is not enough: spreadsheets evaluate formula-looking strings.
  const literal =
    /^[\s\u0000-\u001f]*[=+\-@]/u.test(text) || /^[\t\r\n]/u.test(text)
      ? `'${text}`
      : text;
  return `"${literal.replaceAll('"', '""')}"`;
}

function coverageRows(
  area: string,
  group: string,
  coverage: ReportCoverage,
): CsvRow[] {
  const rows: CsvRow[] = [
    [area, group, "availability", coverage.availability, ""],
    [area, group, "expected_count", coverage.expected_count, ""],
    [area, group, "observed_count", coverage.observed_count, ""],
    [area, group, "reason", coverage.reason, ""],
  ];
  for (const [key, count] of Object.entries(coverage.counts)) {
    rows.push([area, group, key, count, ""]);
  }
  return rows;
}

/** Export only values persisted in this immutable report, never inferred metrics. */
export function reportSnapshotCsv(snapshot: ReportSnapshot): string {
  const rows: CsvRow[] = [
    ["snapshot", "", "report_id", snapshot.report_id, ""],
    ["snapshot", "", "project_id", snapshot.project_id, ""],
    ["snapshot", "", "cycle_id", snapshot.cycle_id, ""],
    ["snapshot", "", "revision", snapshot.revision, ""],
    ["snapshot", "", "correction_of", snapshot.correction_of, ""],
    ["snapshot", "", "status", snapshot.status, ""],
    [
      "snapshot",
      "",
      "report_window_start_at",
      snapshot.report_window_start_at,
      "",
    ],
    ["snapshot", "", "report_window_end_at", snapshot.report_window_end_at, ""],
    ["snapshot", "", "report_timezone", snapshot.report_timezone, ""],
    ["snapshot", "", "cutoff_at", snapshot.cutoff_at, ""],
    ["snapshot", "", "evidence_as_of", snapshot.evidence_as_of, ""],
    ["snapshot", "", "generated_at", snapshot.generated_at, ""],
    ["snapshot", "", "reducer_version", snapshot.reducer_version, ""],
    ["snapshot", "", "input_hash", snapshot.input_hash, ""],
  ];
  for (const manifest of snapshot.input_manifest_versions) {
    const key = `${manifest.kind}:${manifest.manifest_id}:${manifest.revision}`;
    rows.push(
      ["manifest", key, "sealed", String(manifest.sealed), ""],
      ["manifest", key, "expected_count", manifest.expected_count, ""],
    );
  }
  rows.push(
    ...coverageRows("document", "", snapshot.documents),
    ...coverageRows("publication", "", snapshot.publications),
    ...coverageRows("measurement", "", snapshot.measurements),
  );
  for (const group of snapshot.publication_groups) {
    rows.push(
      ...coverageRows("publication_group", group.platform_id, group.coverage),
    );
  }
  for (const group of snapshot.measurement_groups) {
    rows.push(
      ...coverageRows(
        "measurement_group",
        group.comparison_key,
        group.coverage,
      ),
    );
  }
  for (const finding of snapshot.findings) {
    rows.push(
      ["finding", finding.finding_id, "kind", finding.kind, ""],
      ["finding", finding.finding_id, "summary", finding.summary, ""],
      [
        "finding",
        finding.finding_id,
        "insufficient_reason",
        finding.insufficient_reason,
        "",
      ],
    );
    for (const id of finding.evidence_ids) {
      rows.push(["finding", finding.finding_id, "evidence_id", id, ""]);
    }
  }
  for (const item of snapshot.evidence) {
    rows.push(
      ["evidence", item.evidence_id, "kind", item.kind, ""],
      ["evidence", item.evidence_id, "resource_id", item.resource_id, ""],
      [
        "evidence",
        item.evidence_id,
        "resource_version",
        item.resource_version,
        "",
      ],
      ["evidence", item.evidence_id, "occurred_at", item.occurred_at, ""],
      ["evidence", item.evidence_id, "received_at", item.received_at, ""],
      ["evidence", item.evidence_id, "summary", item.summary, ""],
    );
  }
  return (
    "\uFEFF" +
    [["section", "group_or_id", "field", "value", "note"], ...rows]
      .map((row) => row.map(safeCell).join(","))
      .join("\r\n") +
    "\r\n"
  );
}

export function downloadReportCsv(snapshot: ReportSnapshot): void {
  const blob = new Blob([reportSnapshotCsv(snapshot)], {
    type: "text/csv;charset=utf-8",
  });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `report-${snapshot.report_id}-revision-${snapshot.revision}.csv`;
  link.hidden = true;
  document.body.append(link);
  try {
    link.click();
  } finally {
    link.remove();
    URL.revokeObjectURL(url);
  }
}
