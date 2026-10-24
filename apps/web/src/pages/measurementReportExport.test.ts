import { describe, expect, it } from "vitest";
import {
  captureMeasurementReportExport,
  type MeasurementReportExportInput,
} from "./measurementReportExport";

describe("measurement export capture", () => {
  it("synchronously detaches evidence, viewing preferences and language", () => {
    const input: MeasurementReportExportInput = {
      report: {
        kind: "measurement_period_preview",
        project_id: "synthetic-project",
        report_window_start_at: "2026-01-01T00:00:00Z",
        report_window_end_at: "2026-01-08T00:00:00Z",
        report_timezone: "UTC",
        evidence_as_of: "2026-01-09T00:00:00Z",
        generated_at: "2026-01-09T00:01:00Z",
        input_hash: "synthetic-hash",
        coverage: {
          planned: 0,
          counts: { missing: 0 },
          observed_live: 0,
          grounded_saved_analysis: 0,
        },
        samples: [],
      },
      view: {
        question: null,
        target: {
          name: "Synthetic item",
          kind: "product",
          aliases: ["Synthetic alias"],
        },
      },
      locale: "en",
    };
    const captured = captureMeasurementReportExport(input);
    expect(captured).toEqual(input);
    input.report.coverage.counts.missing = 8;
    input.view.target!.aliases.push("Later alias");
    input.view.question = "Changed question";
    input.locale = "zh-CN";
    expect(captured.report.coverage.counts.missing).toBe(0);
    expect(captured.view).toEqual({
      question: null,
      target: {
        name: "Synthetic item",
        kind: "product",
        aliases: ["Synthetic alias"],
      },
    });
    expect(captured.locale).toBe("en");
    expect(captured).not.toBeInstanceOf(Promise);
  });
});
