import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";
import type { ReportSnapshot } from "../api/reports";
import { reportSnapshotCsv } from "./reportsCsv";

const snapshot: ReportSnapshot = {
  report_id: "synthetic-cycle-report",
  project_id: "synthetic-project",
  cycle_id: "synthetic-cycle",
  revision: 2,
  correction_of: "synthetic-original",
  report_window_start_at: "2026-01-01T00:00:00Z",
  report_window_end_at: "2026-01-08T00:00:00Z",
  report_timezone: "UTC",
  cutoff_at: "2026-01-09T00:00:00Z",
  generated_at: "2026-01-09T00:01:00Z",
  reducer_version: "synthetic-reducer",
  input_hash: "synthetic-hash",
  evidence_as_of: "2026-01-09T00:00:00Z",
  status: "partial",
  input_manifest_versions: [
    {
      kind: "measurement",
      manifest_id: "synthetic-manifest",
      revision: 1,
      sealed: true,
      expected_count: 1,
    },
  ],
  documents: {
    availability: "unavailable",
    expected_count: null,
    observed_count: 0,
    counts: {},
    reason: "No frozen input",
  },
  publications: {
    availability: "unavailable",
    expected_count: null,
    observed_count: 0,
    counts: {},
    reason: null,
  },
  publication_groups: [],
  measurements: {
    availability: "available",
    expected_count: 1,
    observed_count: 1,
    counts: { observed: 1 },
    reason: null,
  },
  measurement_groups: [
    {
      comparison_key: "frozen-key",
      purpose: "optimization",
      coverage: {
        availability: "available",
        expected_count: 1,
        observed_count: 1,
        counts: { observed: 1 },
        reason: null,
      },
    },
  ],
  supplementary_measurements: [
    {
      target_id: "synthetic-target",
      attempt_id: "synthetic-attempt",
      comparison_key: "frozen-key",
      question_binding: { purpose: "optimization" },
      observation: {
        raw_answer: '=formula, "quote"\n原文',
        observed_at: "2026-01-02T00:00:00Z",
        received_at: "2026-01-02T00:01:00Z",
        citations: ["https://example.test/source"],
        provenance: null,
      },
    },
  ],
  findings: [
    {
      finding_id: "synthetic-finding",
      kind: "coverage_gap",
      summary: "Synthetic finding",
      evidence_ids: ["synthetic-evidence"],
      insufficient_reason: null,
    },
  ],
  evidence: [
    {
      evidence_id: "synthetic-evidence",
      kind: "measurement",
      resource_id: "synthetic-target",
      resource_version: null,
      occurred_at: "2026-01-02T00:00:00Z",
      received_at: "2026-01-02T00:01:00Z",
      summary: "Synthetic evidence",
    },
  ],
};

describe("cycle CSV wire compatibility", () => {
  it("retains the complete pre-extraction output bytes", () => {
    // Digest captured from the previous serializer with this same synthetic snapshot.
    expect(
      createHash("sha256").update(reportSnapshotCsv(snapshot)).digest("hex"),
    ).toBe("0073ff87ba04b94a86d192c41ed48f2518d7e3300346f6ef9698aa844332a65d");
  });
});
