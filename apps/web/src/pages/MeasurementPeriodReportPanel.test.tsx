import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import * as api from "../api/measurementReports";
import { MeasurementPeriodReportPanel } from "./MeasurementPeriodReportPanel";
import i18n from "../i18n";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { id: "user" },
      operator: { id: "operator" },
      memberships: [{ tenant_id: "tenant", role: "member" }],
    },
  }),
}));
const preview: api.MeasurementPeriodPreview = {
  kind: "measurement_period_preview",
  project_id: "project",
  report_timezone: "UTC",
  report_window_start_at: "2026-10-01T00:00:00Z",
  report_window_end_at: "2026-10-08T00:00:00Z",
  evidence_as_of: "2026-10-08T00:00:00Z",
  generated_at: "2026-10-08T00:00:00Z",
  input_hash: "hash",
  coverage: {
    planned: 1,
    counts: { unknown: 1 },
    observed_live: 0,
    grounded_saved_analysis: 1,
  },
  samples: [
    {
      plan_id: "plan",
      target_id: "target",
      attempt_id: "attempt",
      comparison_key: "provider/model/web",
      scheduled_at: "2026-10-02T00:00:00Z",
      original_status: "unknown",
      observed_live: false,
      observation: {
        raw_answer: "Saved response answer",
        citations: ["https://example.org/evidence", "javascript:alert(1)"],
        observed_at: "2026-10-02T00:00:00Z",
        received_at: "2026-10-02T00:01:00Z",
        provenance: {
          revision_id: "analysis-revision",
          source: { kind: "capture", capture_id: "capture" },
          source_sha256: "digest",
          observed_at: "2026-10-02T00:00:00Z",
          analyzed_at: "2026-10-03T00:00:00Z",
          actual_model: "analysis-model",
          config_revision: 1,
          parser_version: "parser",
          prompt_version: "prompt",
        },
      },
    },
  ],
};
const saved: api.MeasurementPeriodReport = {
  ...preview,
  kind: "measurement_period",
  report_id: "report",
  revision: 2,
  correction_of: "older-report",
};
function setup() {
  const load = vi
    .spyOn(api, "getMeasurementReportPreview")
    .mockResolvedValue(preview);
  const list = vi
    .spyOn(api, "listMeasurementReports")
    .mockResolvedValue({ items: [] });
  const detail = vi.spyOn(api, "getMeasurementReport").mockResolvedValue(saved);
  const save = vi.spyOn(api, "saveMeasurementReport").mockResolvedValue(saved);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <MeasurementPeriodReportPanel tenantId="tenant" projectId="project" />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
  return { load, list, detail, save };
}
afterEach(async () => {
  cleanup();
  vi.restoreAllMocks();
  await i18n.changeLanguage("zh-CN");
});
describe("cycle-free measurement report", () => {
  it("shows a read-only preview with separate original outcome and saved analysis without a cycle", async () => {
    const { save, detail } = setup();
    expect(await screen.findByText("原测量结果：结果未知")).toBeInTheDocument();
    expect(screen.getByText("Saved response answer")).toBeInTheDocument();
    expect(screen.getByText("analysis-model")).toBeInTheDocument();
    expect(screen.getByText("analysis-revision")).toBeInTheDocument();
    expect(screen.getByText(/已核验联网回答 0 项/)).toHaveTextContent(
      "已存原文分析 1 项（单独统计）",
    );
    expect(
      screen.getByRole("link", { name: "https://example.org/evidence" }),
    ).toHaveAttribute("href", "https://example.org/evidence");
    expect(
      screen.queryByRole("link", { name: "javascript:alert(1)" }),
    ).not.toBeInTheDocument();
    expect(save).not.toHaveBeenCalled();
    expect(detail).not.toHaveBeenCalled();
  });
  it("saves only on request and reads the immutable saved correction", async () => {
    const { save, detail } = setup();
    await userEvent.click(
      await screen.findByRole("button", { name: "保存此范围的报告" }),
    );
    await waitFor(() =>
      expect(save).toHaveBeenCalledWith("tenant", "project", preview),
    );
    expect(await screen.findByText("报告版本 2")).toBeInTheDocument();
    expect(screen.getByText("更正自报告 older-report")).toBeInTheDocument();
    expect(detail).toHaveBeenCalledWith("tenant", "project", "report");
    expect(
      screen.queryByRole("button", { name: "保存此范围的报告" }),
    ).not.toBeInTheDocument();
  });
  it("retains the attempted window after save uncertainty and a preview refresh", async () => {
    const { save, load } = setup();
    save.mockRejectedValueOnce(new Error("private upstream message"));
    await userEvent.click(
      await screen.findByRole("button", { name: "保存此范围的报告" }),
    );
    expect(await screen.findByText(/保存尚未确认/)).toBeInTheDocument();
    load.mockResolvedValue({
      ...preview,
      report_window_end_at: "2026-10-09T00:00:00Z",
    });
    await userEvent.click(screen.getByRole("button", { name: "刷新测量报告" }));
    await waitFor(() => expect(load).toHaveBeenCalledTimes(2));
    await userEvent.click(
      screen.getByRole("button", { name: "保存此范围的报告" }),
    );
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(save.mock.calls[0]).toEqual(save.mock.calls[1]);
    expect(
      screen.queryByText("private upstream message"),
    ).not.toBeInTheDocument();
  });
  it("localizes the preview without hiding unknown outcomes", async () => {
    await i18n.changeLanguage("en");
    setup();
    expect(
      await screen.findByText("Original measurement: Unknown"),
    ).toBeInTheDocument();
    expect(screen.getByText("Last seven days · preview")).toBeInTheDocument();
  });
});
