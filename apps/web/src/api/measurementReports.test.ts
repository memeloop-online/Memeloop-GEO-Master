import { afterEach, describe, expect, it, vi } from "vitest";
import {
  getMeasurementReportPreview,
  listMeasurementReports,
  saveMeasurementReport,
  type MeasurementPeriodProjection,
} from "./measurementReports";

afterEach(() => vi.unstubAllGlobals());

describe("measurement report scope requests", () => {
  it("sends paired question set selectors and explicit bounds, then saves only the report's concrete scope and window", async () => {
    const calls: { url: URL; init: RequestInit }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
        calls.push({ url: new URL(String(input), "http://localhost"), init });
        return new Response(JSON.stringify({ items: [] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }),
    );
    const questionSet = {
      question_set_id: "set/1",
      question_set_version_id: "version 1",
    };
    const window = {
      start_at: "2026-09-01T00:00:00Z",
      end_at: "2026-10-16T00:00:00Z",
    };
    await getMeasurementReportPreview("tenant", "project", questionSet, window);
    await listMeasurementReports("tenant", "project", questionSet);
    const preview: MeasurementPeriodProjection = {
      project_id: "project",
      question_set: questionSet,
      report_window_start_at: window.start_at,
      report_window_end_at: window.end_at,
      report_timezone: "UTC",
      evidence_as_of: window.end_at,
      generated_at: window.end_at,
      input_hash: "hash",
      coverage: {
        planned: 0,
        counts: {},
        grounded_saved_analysis: 0,
        observed_live: 0,
      },
      samples: [],
    };
    await saveMeasurementReport("tenant", "project", preview);
    await listMeasurementReports("tenant", "project");
    await saveMeasurementReport("tenant", "project", {
      ...preview,
      question_set: undefined,
    });
    expect(calls[0].url.pathname).toBe(
      "/api/v1/projects/project/measurement-report-preview",
    );
    expect(calls[1].url.pathname).toBe(
      "/api/v1/projects/project/measurement-reports",
    );
    for (const { url } of calls.slice(0, 2)) {
      expect(url.searchParams.get("question_set_id")).toBe(
        questionSet.question_set_id,
      );
      expect(url.searchParams.get("question_set_version_id")).toBe(
        questionSet.question_set_version_id,
      );
    }
    expect(calls[0].url.searchParams.get("start_at")).toBe(window.start_at);
    expect(calls[0].url.searchParams.get("end_at")).toBe(window.end_at);
    expect(JSON.parse(String(calls[2].init.body))).toEqual({
      ...window,
      report_timezone: "UTC",
      question_set: questionSet,
    });
    expect(calls[3].url.searchParams.has("question_set_id")).toBe(false);
    expect(JSON.parse(String(calls[4].init.body))).toEqual({
      ...window,
      report_timezone: "UTC",
    });
    for (const { url } of calls) {
      expect(url.searchParams.get("tenant_id")).toBe("tenant");
      expect(url.searchParams.get("project_id")).toBe("project");
    }
  });
});
