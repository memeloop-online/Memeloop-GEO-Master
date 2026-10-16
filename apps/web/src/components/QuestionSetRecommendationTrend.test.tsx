import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError } from "../api/client";
import {
  getChannelTarget,
  listMeasurementPlans,
  type ChannelTarget,
  type StandaloneMeasurementPlan,
} from "../api/channelJobs";
import {
  listObservationAnalyses,
  type ObservationAnalysisRevision,
} from "../api/observationAnalysis";
import i18n from "../i18n";
import { QuestionSetRecommendationTrend } from "./QuestionSetRecommendationTrend";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: { user: { id: "user" }, operator: { id: "operator" } },
  }),
}));
vi.mock("../api/channelJobs", () => ({
  getChannelTarget: vi.fn(),
  listMeasurementPlans: vi.fn(),
}));
vi.mock("../api/observationAnalysis", () => ({
  listObservationAnalyses: vi.fn(),
}));

function target(
  id = "target-1",
  setId = "set-1",
  versionId = "version-1",
): ChannelTarget {
  return {
    target_id: id,
    input: {
      kind: "measure",
      account_id: "account",
      provider: "provider",
      model: "model",
      surface: "consumer_web",
      search_mode: "web_search",
      protocol_version: "v1",
      question_set_version: versionId,
      question: "推荐一款露营灯",
      market: "CN",
      language: "zh-CN",
      scheduled_at: "2026-10-01T00:00:00Z",
      sample_ordinal: 0,
      question_binding: {
        reference: {
          question_set_id: setId,
          question_set_version_id: versionId,
          question_id: "question",
          question_revision_id: "question-revision",
        },
        purpose: "optimization",
        split_policy_version: "v1",
      },
    },
  };
}
function plan(id: string, targets: ChannelTarget[]): StandaloneMeasurementPlan {
  return {
    plan_id: id,
    project_id: "project",
    title: "露营灯测量",
    created_at: "2026-10-01T00:00:00Z",
    targets,
  };
}
function revision(targetId: string, rank = 1): ObservationAnalysisRevision {
  return {
    request: {
      revision_id: `analysis-${targetId}`,
      target_id: targetId,
      attempt_id: `attempt-${targetId}`,
      source: { kind: "attempt_evidence", evidence_index: 0 },
      source_sha256: "digest",
      observed_at:
        targetId === "target-1"
          ? "2026-10-01T00:00:00Z"
          : "2026-10-02T00:00:00Z",
      prompt_version: "v1",
      parser_version: "v1",
    },
    request_digest: "digest",
    state: "completed",
    created_at: "2026-10-02T00:00:00Z",
    started_at: null,
    analyzed_at: "2026-10-02T00:00:00Z",
    result: {
      actual_model: "analysis-model",
      candidate_json: null,
      prompt_tokens: 1,
      completion_tokens: 1,
      outcome: {
        status: "grounded",
        raw_answer: "晨光灯适合露营。",
        citations: [],
        audit: {
          recommendations: {
            version: "geo.recommendations.v1",
            status: "verified",
            items: [
              {
                name: "晨光灯",
                kind: "product",
                mention_order: 1,
                rank,
                reason: null,
                evidence_quote: "晨光灯适合露营。",
                citation_urls: [],
              },
            ],
          },
        },
      },
    },
  };
}
function renderTrend() {
  return render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <QuestionSetRecommendationTrend
          tenantId="tenant"
          projectId="project"
          questionSetId="set-1"
          questionSetVersionId="version-1"
        />
      </QueryClientProvider>
    </FluentProvider>,
  );
}
beforeEach(async () => {
  vi.clearAllMocks();
  await i18n.changeLanguage("zh-CN");
  vi.mocked(listMeasurementPlans).mockResolvedValue({
    items: [plan("plan-1", [target()])],
    next_after: null,
  });
  vi.mocked(getChannelTarget).mockImplementation(
    async (_tenant, _project, id) => ({
      target: target(id),
      attempts: [
        {
          attempt_id: `attempt-${id}`,
          target_id: id,
          claimed_at: "2026-10-01T00:00:00Z",
          outcome: null,
          received_at: null,
        },
      ],
    }),
  );
  vi.mocked(listObservationAnalyses).mockImplementation(
    async (_tenant, _project, id) => ({
      items: [revision(id, id === "target-1" ? 1 : 3)],
      next_after: null,
      sources: [],
    }),
  );
});
afterEach(cleanup);

describe("question set recommendation comparison", () => {
  it("loads only on explicit action and includes later pages while excluding other sets and versions", async () => {
    vi.mocked(listMeasurementPlans).mockImplementation(
      async (_tenant, _project, after) =>
        after
          ? { items: [plan("plan-2", [target("target-2")])], next_after: null }
          : {
              items: [
                plan("plan-1", [
                  target(),
                  target("other-set", "set-2"),
                  target("other-version", "set-1", "version-2"),
                ]),
              ],
              next_after: "next-plan",
            },
    );
    renderTrend();
    expect(listMeasurementPlans).not.toHaveBeenCalled();
    expect(getChannelTarget).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    expect(await screen.findByText("晨光灯")).toBeInTheDocument();
    expect(screen.getByText("第 1 位")).toBeInTheDocument();
    expect(screen.getByText("第 3 位")).toBeInTheDocument();
    expect(listMeasurementPlans).toHaveBeenCalledWith(
      "tenant",
      "project",
      "next-plan",
    );
    expect(getChannelTarget).toHaveBeenCalledTimes(2);
    const links = screen.getAllByRole("link", { name: /查看测量记录/ });
    expect(links.map((link) => link.getAttribute("href"))).toEqual([
      "/app/tenant/project/measurement?tab=records&set=set-1&version=version-1&record=plan-1&target=target-1&attempt=attempt-target-1&analysisRevision=analysis-target-1",
      "/app/tenant/project/measurement?tab=records&set=set-1&version=version-1&record=plan-2&target=target-2&attempt=attempt-target-2&analysisRevision=analysis-target-2",
    ]);
  });

  it("links individual targets and analyses within the same plan", async () => {
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [plan("plan-1", [target(), target("target-2")])],
      next_after: null,
    });
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    await screen.findByText("晨光灯");
    const sources = screen
      .getAllByRole("link", { name: /查看测量记录/ })
      .map(
        (link) =>
          new URL(link.getAttribute("href")!, "http://localhost").searchParams,
      );
    expect(sources.map((query) => query.get("record"))).toEqual([
      "plan-1",
      "plan-1",
    ]);
    expect(sources.map((query) => query.get("target"))).toEqual([
      "target-1",
      "target-2",
    ]);
    expect(sources.map((query) => query.get("attempt"))).toEqual([
      "attempt-target-1",
      "attempt-target-2",
    ]);
    expect(sources.map((query) => query.get("analysisRevision"))).toEqual([
      "analysis-target-1",
      "analysis-target-2",
    ]);
  });

  it("keeps the latest attempt unknown instead of reviving an older attempt's result", async () => {
    vi.mocked(getChannelTarget).mockResolvedValue({
      target: target(),
      attempts: [
        {
          attempt_id: "older-attempt",
          target_id: "target-1",
          claimed_at: "2026-10-01T00:00:00Z",
          outcome: null,
          received_at: null,
        },
        {
          attempt_id: "latest-attempt",
          target_id: "target-1",
          claimed_at: "2026-10-02T00:00:00Z",
          outcome: null,
          received_at: null,
        },
      ],
    });
    vi.mocked(listObservationAnalyses).mockImplementation(
      async (_tenant, _project, _target, attempt) => ({
        items: attempt === "older-attempt" ? [revision("target-1")] : [],
        next_after: null,
        sources: [],
      }),
    );
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    expect(await screen.findByText("尚未提取推荐结果")).toBeInTheDocument();
    expect(screen.queryByText("晨光灯")).not.toBeInTheDocument();
    expect(listObservationAnalyses).toHaveBeenCalledTimes(1);
    const link = screen.getByRole("link", { name: /查看测量记录/ });
    expect(link.getAttribute("href")).toContain("attempt=latest-attempt");
    expect(link.getAttribute("href")).not.toContain("analysisRevision=");
  });

  it("reads every analysis page and selects a saved grounded result instead of a later failed revision", async () => {
    const failed = revision("target-1");
    failed.result!.outcome = { status: "failed", code: "model_timeout" };
    vi.mocked(listObservationAnalyses).mockImplementation(
      async (_tenant, _project, _target, _attempt, after) =>
        after
          ? { items: [revision("target-1")], next_after: null, sources: [] }
          : { items: [failed], next_after: "older-analysis", sources: [] },
    );
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    expect(await screen.findByText("晨光灯")).toBeInTheDocument();
    expect(listObservationAnalyses).toHaveBeenCalledWith(
      "tenant",
      "project",
      "target-1",
      "attempt-target-1",
      "older-analysis",
      expect.any(AbortSignal),
    );
    expect(screen.getAllByRole("link", { name: /查看测量记录/ })).toHaveLength(
      1,
    );
  });

  it("keeps missing analyses distinct from an absent recommendation", async () => {
    vi.mocked(listObservationAnalyses).mockResolvedValue({
      items: [],
      next_after: null,
      sources: [],
    });
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    expect(await screen.findByText("尚未提取推荐结果")).toBeInTheDocument();
    expect(screen.queryByText("未出现在推荐结果中")).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: /查看测量记录/ }),
    ).toBeInTheDocument();
  });

  it("does not display an incomplete comparison if a later page fails", async () => {
    vi.mocked(listMeasurementPlans).mockImplementation(
      async (_tenant, _project, after) => {
        if (after) throw new Error("unavailable");
        return { items: [plan("plan-1", [target()])], next_after: "later" };
      },
    );
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    expect(await screen.findByText("排名变化暂时无法读取")).toBeInTheDocument();
    expect(screen.queryByText("晨光灯")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: /查看测量记录/ }),
    ).not.toBeInTheDocument();
  });

  it("shows access errors without converting them into missing recommendations", async () => {
    vi.mocked(listObservationAnalyses).mockRejectedValue(
      new ApiError(403, {}, "Forbidden"),
    );
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    expect(
      await screen.findByText(
        "当前无法访问这些测量记录。请检查登录状态或项目权限。",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText("尚未提取推荐结果")).not.toBeInTheDocument();
    expect(screen.queryByText("未出现在推荐结果中")).not.toBeInTheDocument();
  });

  it("shows loading while all pages are being collected", async () => {
    let resolvePage!: (value: {
      items: StandaloneMeasurementPlan[];
      next_after: null;
    }) => void;
    vi.mocked(listMeasurementPlans).mockReturnValue(
      new Promise((resolve) => {
        resolvePage = resolve;
      }),
    );
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "排名变化" }));
    expect(screen.getByText("正在读取历次测量与推荐结果")).toBeInTheDocument();
    resolvePage({ items: [], next_after: null });
    expect(
      await screen.findByText("该问题集版本还没有测量记录。"),
    ).toBeInTheDocument();
  });

  it("uses English for the entry and empty state", async () => {
    await i18n.changeLanguage("en");
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [],
      next_after: null,
    });
    renderTrend();
    await userEvent.click(screen.getByRole("button", { name: "Rank history" }));
    expect(
      await screen.findByText(
        "This question set version has no measurements yet.",
      ),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(
        screen.queryByText("正在读取历次测量与推荐结果"),
      ).not.toBeInTheDocument(),
    );
  });
});
