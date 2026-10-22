import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useLocation } from "react-router-dom";
import * as api from "../api/measurementReports";
import { getChannelTarget, listMeasurementPlans } from "../api/channelJobs";
import { listObservationAnalyses } from "../api/observationAnalysis";
import i18n from "../i18n";
import { QuestionSetRecommendationTrend } from "./QuestionSetRecommendationTrend";

const auth = vi.hoisted(() => ({ role: "member" }));
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { id: "user" },
      operator: { id: "operator" },
      memberships: [{ tenant_id: "tenant", role: auth.role }],
    },
  }),
}));
vi.mock("../api/measurementReports", () => ({
  getMeasurementReportPreview: vi.fn(),
  listMeasurementReports: vi.fn(),
  getMeasurementReport: vi.fn(),
  saveMeasurementReport: vi.fn(),
}));
vi.mock("../api/channelJobs", () => ({
  getChannelTarget: vi.fn(),
  listMeasurementPlans: vi.fn(),
}));
vi.mock("../api/observationAnalysis", () => ({
  listObservationAnalyses: vi.fn(),
}));

const questionSet = {
  question_set_id: "set-1",
  question_set_version_id: "version-1",
};
function sample(
  id: string,
  rank: number | null = 2,
): api.MeasurementPeriodSample {
  return {
    plan_id: "plan-1",
    target_id: id,
    attempt_id: `attempt-${id}`,
    comparison_key: "provider|model|consumer_web|enabled|v1|version-1|US|en",
    conditions: {
      question: "Which lamp suits camping?",
      question_set_version: "version-1",
      provider: "provider",
      model: "model",
      surface: "consumer_web",
      search_mode: "enabled",
      protocol_version: "v1",
      market: "US",
      language: "en",
      sample_ordinal: 1,
    },
    question_binding: {
      reference: {
        ...questionSet,
        question_id: "question-1",
        question_revision_id: "revision-1",
      },
      purpose: "optimization",
      split_policy_version: "v1",
    },
    scheduled_at: "2026-09-01T00:00:00Z",
    original_status: "unknown",
    observed_live: false,
    observation: {
      raw_answer: "Sample lamp is a useful camping choice.",
      citations: [],
      observed_at: "2026-09-01T00:00:00Z",
      received_at: "2026-09-01T00:01:00Z",
      provenance: {
        revision_id: `analysis-${id}`,
        source: { kind: "capture", capture_id: "capture-1" },
        source_sha256: "digest",
        observed_at: "2026-09-01T00:00:00Z",
        analyzed_at: "2026-09-01T00:01:00Z",
        actual_model: "analysis-model",
        config_revision: 1,
        parser_version: "v1",
        prompt_version: "v1",
      },
    },
    recommendations: {
      version: "geo.recommendations.v1",
      status: "verified",
      items: [
        {
          name: "Sample lamp",
          kind: "product",
          mention_order: 1,
          rank,
          reason: null,
          evidence_quote: "Sample lamp is a useful camping choice.",
          citation_urls: [],
        },
      ],
    },
  };
}
function preview(): api.MeasurementPeriodPreview {
  return {
    kind: "measurement_period_preview",
    project_id: "project",
    question_set: questionSet,
    report_window_start_at: "2026-09-01T00:00:00Z",
    report_window_end_at: "2026-10-16T00:00:00Z",
    report_timezone: "UTC",
    evidence_as_of: "2026-10-16T00:00:00Z",
    generated_at: "2026-10-16T00:00:00Z",
    input_hash: "hash",
    coverage: {
      planned: 2,
      counts: { unknown: 2 },
      observed_live: 0,
      grounded_saved_analysis: 2,
    },
    samples: [
      sample("target-1"),
      {
        ...sample("target-2", 3),
        scheduled_at: "2026-10-02T00:00:00Z",
        observation: {
          ...sample("target-2").observation!,
          observed_at: "2026-10-02T00:00:00Z",
        },
      },
    ],
  };
}
function saved(): api.MeasurementPeriodReport {
  return {
    ...preview(),
    kind: "measurement_period",
    report_id: "report-1",
    revision: 1,
    correction_of: null,
    samples: [sample("target-1", 4)],
  };
}
function Location() {
  return <output data-testid="location">{useLocation().search}</output>;
}
function renderTrend(initialEntry = "/?tab=records&record=keep-record") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[initialEntry]}>
          <Location />
          <QuestionSetRecommendationTrend
            tenantId="tenant"
            projectId="project"
            questionSetId="set-1"
            questionSetVersionId="version-1"
          />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
}
async function open() {
  await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
}
beforeEach(async () => {
  vi.clearAllMocks();
  auth.role = "member";
  await i18n.changeLanguage("zh-CN");
  vi.mocked(api.getMeasurementReportPreview).mockResolvedValue(preview());
  vi.mocked(api.listMeasurementReports).mockResolvedValue({ items: [] });
  vi.mocked(api.getMeasurementReport).mockResolvedValue(saved());
  vi.mocked(api.saveMeasurementReport).mockResolvedValue(saved());
});
afterEach(cleanup);

describe("question set rank reports", () => {
  it("loads the scoped report on request and compares frozen ranks with exact source links", async () => {
    renderTrend();
    expect(api.getMeasurementReportPreview).not.toHaveBeenCalled();
    await open();
    expect(await screen.findByText("第 2 位")).toBeVisible();
    expect(screen.getByText("第 3 位")).toBeVisible();
    expect(screen.getAllByText("出现顺序：1")).toHaveLength(2);
    expect(api.getMeasurementReportPreview).toHaveBeenCalledWith(
      "tenant",
      "project",
      questionSet,
      undefined,
    );
    expect(api.listMeasurementReports).toHaveBeenCalledWith(
      "tenant",
      "project",
      questionSet,
    );
    const links = within(
      screen.getByRole("table", { name: "Which lamp suits camping?" }),
    ).getAllByRole("link");
    const query = new URL(links[0].getAttribute("href")!, "http://localhost")
      .searchParams;
    expect(Object.fromEntries(query)).toEqual({
      tab: "records",
      record: "plan-1",
      target: "target-1",
      set: "set-1",
      version: "version-1",
      attempt: "attempt-target-1",
      analysisRevision: "analysis-target-1",
    });
    expect(getChannelTarget).not.toHaveBeenCalled();
    expect(listMeasurementPlans).not.toHaveBeenCalled();
    expect(listObservationAnalyses).not.toHaveBeenCalled();
  });

  it("saves this question set's concrete period and refreshes the immutable report without live analysis", async () => {
    renderTrend();
    await open();
    await userEvent.click(
      await screen.findByRole("button", { name: "保存此范围的报告" }),
    );
    expect(await screen.findByText("第 4 位")).toBeVisible();
    expect(api.saveMeasurementReport).toHaveBeenCalledWith(
      "tenant",
      "project",
      preview(),
    );
    expect(
      screen.getByText("这是已保存的固定版本，后续结果不会改写本报告。"),
    ).toBeVisible();
    vi.mocked(api.getMeasurementReportPreview).mockResolvedValue({
      ...preview(),
      samples: [sample("target-1", 1)],
    });
    await userEvent.click(screen.getByRole("button", { name: "刷新测量报告" }));
    await waitFor(() =>
      expect(api.getMeasurementReport).toHaveBeenCalledTimes(2),
    );
    expect(api.getMeasurementReportPreview).toHaveBeenCalledTimes(1);
    expect(screen.getByText("第 4 位")).toBeVisible();
    expect(screen.queryByText("第 1 位")).not.toBeInTheDocument();
    expect(listObservationAnalyses).not.toHaveBeenCalled();
    const selected = new URLSearchParams(
      screen.getByTestId("location").textContent!,
    );
    expect(selected.get("tab")).toBe("records");
    expect(selected.get("record")).toBe("keep-record");
    expect(selected.get("set")).toBe("set-1");
    expect(selected.get("version")).toBe("version-1");
    expect(selected.get("rankReport")).toBe("report-1");
  });

  it("restores the exact saved report after a reload from its URL without loading a live preview", async () => {
    const address =
      "/?tab=records&set=set-1&version=version-1&rankReport=report-1&record=keep-record";
    const first = renderTrend(address);
    expect(await screen.findByText("第 4 位")).toBeVisible();
    expect(api.getMeasurementReportPreview).not.toHaveBeenCalled();
    expect(api.getMeasurementReport).toHaveBeenCalledWith(
      "tenant",
      "project",
      "report-1",
    );
    first.unmount();
    vi.mocked(api.getMeasurementReportPreview).mockResolvedValue({
      ...preview(),
      samples: [sample("target-1", 1)],
    });
    renderTrend(address);
    expect(await screen.findByText("第 4 位")).toBeVisible();
    expect(api.getMeasurementReportPreview).not.toHaveBeenCalled();
    expect(api.getMeasurementReport).toHaveBeenCalledTimes(2);
    expect(listObservationAnalyses).not.toHaveBeenCalled();
    await userEvent.click(
      screen.getByRole("button", { name: "返回测量结果预览" }),
    );
    expect(await screen.findByText("第 1 位")).toBeVisible();
    expect(screen.getByTestId("location")).toHaveTextContent(
      "set=set-1&version=version-1",
    );
    expect(screen.getByTestId("location")).not.toHaveTextContent("rankReport");
  });

  it("keeps a wrong-scope report URL visible as an error without replacing it or loading live data", async () => {
    vi.mocked(api.getMeasurementReport).mockResolvedValue({
      ...saved(),
      question_set: { ...questionSet, question_set_version_id: "version-2" },
    });
    renderTrend(
      "/?tab=records&set=set-1&version=version-1&rankReport=wrong-report",
    );
    expect(
      await screen.findByText("此报告不属于当前问题集版本，请重新打开。"),
    ).toBeVisible();
    expect(api.getMeasurementReport).toHaveBeenCalledWith(
      "tenant",
      "project",
      "wrong-report",
    );
    expect(api.getMeasurementReportPreview).not.toHaveBeenCalled();
    expect(api.saveMeasurementReport).not.toHaveBeenCalled();
    expect(screen.getByTestId("location")).toHaveTextContent(
      "rankReport=wrong-report",
    );
    expect(
      screen.queryByRole("button", { name: "保存此范围的报告" }),
    ).not.toBeInTheDocument();
  });

  it("reopens saved history after closing the panel without recomputing its recommendations", async () => {
    vi.mocked(api.listMeasurementReports).mockResolvedValue({
      items: [saved()],
    });
    renderTrend();
    await open();
    await userEvent.click(
      await screen.findByRole("button", { name: /查看版本 1/ }),
    );
    expect(await screen.findByText("第 4 位")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "收起排名变化" }));
    await open();
    await userEvent.click(
      await screen.findByRole("button", { name: /查看版本 1/ }),
    );
    expect(await screen.findByText("第 4 位")).toBeVisible();
    expect(api.getMeasurementReport).toHaveBeenLastCalledWith(
      "tenant",
      "project",
      "report-1",
    );
    expect(api.saveMeasurementReport).not.toHaveBeenCalled();
    expect(listObservationAnalyses).not.toHaveBeenCalled();
  });

  it("lets the user choose a concrete time range and return to all measurements", async () => {
    renderTrend();
    await open();
    await screen.findByText("第 2 位");
    await userEvent.click(screen.getByText("选择报告时间范围"));
    fireEvent.change(screen.getByLabelText("开始时间（本地时间）"), {
      target: { value: "2026-09-01T00:00" },
    });
    fireEvent.change(screen.getByLabelText("结束时间（本地时间）"), {
      target: { value: "2026-10-01T00:00" },
    });
    await userEvent.click(screen.getByRole("button", { name: "应用时间范围" }));
    await waitFor(() =>
      expect(api.getMeasurementReportPreview).toHaveBeenLastCalledWith(
        "tenant",
        "project",
        questionSet,
        {
          start_at: new Date("2026-09-01T00:00").toISOString(),
          end_at: new Date("2026-10-01T00:00").toISOString(),
        },
      ),
    );
    await userEvent.click(screen.getByRole("button", { name: "全部测量记录" }));
    await waitFor(() =>
      expect(api.getMeasurementReportPreview).toHaveBeenLastCalledWith(
        "tenant",
        "project",
        questionSet,
        undefined,
      ),
    );
  });

  it.each([
    { ...preview(), project_id: "other-project" },
    { ...preview(), question_set: undefined },
    {
      ...preview(),
      question_set: { ...questionSet, question_set_version_id: "version-2" },
    },
  ])("rejects a preview from another scope", async (value) => {
    vi.mocked(api.getMeasurementReportPreview).mockResolvedValue(value);
    renderTrend();
    await open();
    expect(
      await screen.findByText("此报告不属于当前问题集版本，请重新打开。"),
    ).toBeVisible();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "保存此范围的报告" }),
    ).not.toBeInTheDocument();
  });

  it("rejects a history detail from another version", async () => {
    vi.mocked(api.listMeasurementReports).mockResolvedValue({
      items: [saved()],
    });
    vi.mocked(api.getMeasurementReport).mockResolvedValue({
      ...saved(),
      question_set: { ...questionSet, question_set_version_id: "version-2" },
    });
    renderTrend();
    await open();
    await userEvent.click(
      await screen.findByRole("button", { name: /查看版本 1/ }),
    );
    expect(
      await screen.findByText("此报告不属于当前问题集版本，请重新打开。"),
    ).toBeVisible();
    expect(screen.queryByText("第 4 位")).not.toBeInTheDocument();
  });

  it("shows saved missing recommendations without treating them as absent", async () => {
    vi.mocked(api.getMeasurementReportPreview).mockResolvedValue({
      ...preview(),
      samples: [{ ...sample("target-1"), recommendations: undefined }],
    });
    renderTrend();
    await open();
    expect(await screen.findByText("推荐结果尚未分析")).toBeVisible();
    expect(screen.queryByText("未出现在推荐结果中")).not.toBeInTheDocument();
  });

  it("keeps read-only access and localizes the report", async () => {
    auth.role = "viewer";
    await i18n.changeLanguage("en");
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "Rank history" }));
    expect(
      await screen.findByRole("heading", { name: "Question set rank reports" }),
    ).toBeVisible();
    expect(screen.getByText("Rank 2")).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Save report for this period" }),
    ).not.toBeInTheDocument();
  });
});
