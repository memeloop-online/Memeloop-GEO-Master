import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import * as jobs from "../api/channelJobs";
import * as analyses from "../api/observationAnalysis";
import type { MeasurementTarget } from "../api/projects";
import type { RecommendationItem } from "../api/recommendations";
import { MeasurementOutcomeSummary } from "./MeasurementOutcomeSummary";
import i18n from "../i18n";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: { user: { id: "user" }, operator: { id: "operator" } },
  }),
}));

let focusTargets: MeasurementTarget[] = [];
vi.mock("../api/projects", () => ({
  useProjectQuery: () => ({
    data: { settings: { measurement_targets: focusTargets } },
    isPending: false,
    isError: false,
  }),
}));

const item: RecommendationItem = {
  name: "Sample purifier",
  kind: "product",
  mention_order: 1,
  rank: null,
  reason: "Quiet operation",
  evidence_quote: "Sample purifier is recommended for quiet operation.",
  citation_urls: ["https://example.test/product"],
  brand_attribution: null,
};
const target: jobs.ChannelTarget = {
  target_id: "target",
  input: {
    kind: "measure",
    account_id: "account",
    provider: "Sample AI",
    model: "Sample model",
    surface: "consumer_web",
    search_mode: "web_search",
    protocol_version: "sample-protocol",
    question_set_version: "sample-question",
    question: "Which purifier is suitable for a small room?",
    market: "CN",
    language: "zh-CN",
    scheduled_at: "2026-01-01T00:00:00Z",
    sample_ordinal: 1,
  },
};
const attempt: jobs.ChannelAttempt = {
  attempt_id: "attempt",
  target_id: target.target_id,
  claimed_at: "2026-01-01T00:00:00Z",
  received_at: "2026-01-01T00:01:00Z",
  outcome: {
    status: "unknown",
    detail: "extraction_pending",
    occurred_at: "2026-01-01T00:01:00Z",
    raw_answer: null,
    citations: ["https://example.test/stale-receipt"],
    public_url: null,
    screenshot_ref: null,
    connector_version: null,
    runner_evidence: [],
    fixture: false,
  },
};
function analysis(
  items: RecommendationItem[] | null = [item],
  citations = ["https://example.test/this-answer"],
  revisionId = "revision",
): analyses.ObservationAnalysisRevision {
  return {
    request: {
      revision_id: revisionId,
      target_id: "target",
      attempt_id: "attempt",
      source: { kind: "capture", capture_id: "capture" },
      source_sha256: "a".repeat(64),
      observed_at: "2026-01-01T00:01:00Z",
      prompt_version: "sample-prompt",
      parser_version: "sample-parser",
    },
    request_digest: "b".repeat(64),
    state: "completed",
    created_at: "2026-01-01T00:02:00Z",
    started_at: "2026-01-01T00:02:00Z",
    analyzed_at: "2026-01-01T00:03:00Z",
    result: {
      actual_model: "Sample parser",
      candidate_json: null,
      outcome: {
        status: "grounded",
        raw_answer: "The complete original answer.",
        citations,
        audit: {
          recommendations:
            items === null
              ? null
              : {
                  version: "geo.recommendations.v2",
                  status: "verified",
                  items,
                },
        },
      },
      prompt_tokens: 0,
      completion_tokens: 0,
    },
  };
}
const page = (
  items: analyses.ObservationAnalysisRevision[],
): analyses.ObservationAnalysisPage => ({
  items,
  next_after: null,
  sources: [],
});
function setup({
  targets = [target],
  attempts = [attempt],
  revisions = [analysis()],
  route = "/",
}: {
  targets?: jobs.ChannelTarget[];
  attempts?: jobs.ChannelAttempt[];
  revisions?: analyses.ObservationAnalysisRevision[];
  route?: string;
} = {}) {
  const getTarget = vi
    .spyOn(jobs, "getChannelTarget")
    .mockImplementation(async (_tenant, _project, id) => ({
      target: targets.find((entry) => entry.target_id === id)!,
      attempts: attempts.map((entry) => ({ ...entry, target_id: id })),
    }));
  const list = vi
    .spyOn(analyses, "listObservationAnalyses")
    .mockResolvedValue(page(revisions));
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const plan: jobs.StandaloneMeasurementPlan = {
    plan_id: "11111111-1111-4111-8111-111111111111",
    project_id: "project",
    title: "Sample measurement",
    created_at: "2026-01-01T00:00:00Z",
    targets,
  };
  render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[route]}>
          <MeasurementOutcomeSummary
            tenantId="tenant"
            projectId="project"
            plan={plan}
          />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
  return { getTarget, list, client };
}

beforeEach(async () => {
  focusTargets = [];
  await i18n.changeLanguage("zh-CN");
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("same-measurement outcome summary", () => {
  it("shows extracted recommendations without claiming a tracked product or inventing a rank", async () => {
    setup();
    expect(
      await screen.findByRole("heading", { name: "AI 推荐了谁" }),
    ).toBeVisible();
    expect(screen.queryByText("AI 是否推荐了关注对象")).not.toBeInTheDocument();
    expect(screen.getByText("未明确排名")).toBeVisible();
    expect(screen.queryByText("推荐第 1 位")).not.toBeInTheDocument();
    expect(screen.getByText("Quiet operation")).toBeVisible();
    fireEvent.click(screen.getByText("查看原文依据"));
    expect(screen.getByText(item.evidence_quote)).toBeVisible();
    expect(
      screen.getByText("查看本次回答中的推荐、描述与引用来源。"),
    ).toBeVisible();
  });

  it("matches exact kind and aliases, shows real rank, and keeps all alternatives", async () => {
    focusTargets = [
      { name: "Sample product", kind: "product", aliases: [item.name] },
    ];
    setup({
      revisions: [
        analysis([
          { ...item, rank: 2 },
          { ...item, name: "Another purifier", mention_order: 2, rank: 1 },
          {
            ...item,
            name: "Sample purifier",
            kind: "brand",
            mention_order: 3,
            rank: null,
          },
        ]),
      ],
    });
    const focus = await screen.findByRole("region", { name: "Sample product" });
    expect(within(focus).getByText("已推荐")).toBeVisible();
    expect(within(focus).getByText("推荐第 2 位")).toBeVisible();
    const alternatives = screen.getByRole("region", { name: "还推荐了谁" });
    expect(within(alternatives).getAllByRole("article")).toHaveLength(2);
    expect(within(alternatives).getByText("Another purifier")).toBeVisible();
  });

  it("does not count missing analysis or unavailable recommendation extraction as absent", async () => {
    focusTargets = [{ name: "Unmeasured item", kind: "product", aliases: [] }];
    setup({ revisions: [analysis(null)] });
    expect(await screen.findByText("暂无可用的推荐结果。")).toBeVisible();
    expect(screen.queryByText(/本次推荐名单中没有/)).not.toBeInTheDocument();
    expect(screen.queryByText("还推荐了谁")).not.toBeInTheDocument();
  });

  it("keeps failed samples unknown and does not use receipt citations as parsed citations", async () => {
    focusTargets = [{ name: "Unmeasured item", kind: "product", aliases: [] }];
    setup({
      attempts: [
        { ...attempt, outcome: { ...attempt.outcome!, status: "failed" } },
      ],
      revisions: [],
    });
    expect(await screen.findByText("本次未取得有效结果。")).toBeVisible();
    expect(screen.queryByText(/本次推荐名单中没有/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: /example\.test/ }),
    ).not.toBeInTheDocument();
  });

  it.each([
    ["refused", "AI 拒绝了本次问题。"],
    ["unknown", "本次结果尚未确认。"],
  ] as const)(
    "preserves the %s result as unknown recommendations",
    async (status, message) => {
      setup({
        attempts: [{ ...attempt, outcome: { ...attempt.outcome!, status } }],
        revisions: [],
      });
      expect(await screen.findByText(message)).toBeVisible();
      expect(screen.queryByText(/本次推荐名单中没有/)).not.toBeInTheDocument();
    },
  );

  it("shows waiting status while an attempt is still being collected", async () => {
    const { list } = setup({
      attempts: [{ ...attempt, outcome: null }],
      revisions: [],
    });
    expect(await screen.findByText("正在采集或解析回答。")).toBeVisible();
    expect(list).not.toHaveBeenCalled();
  });

  it("does not promote a fixture to an actual recommendation result", async () => {
    const { list } = setup({
      attempts: [
        {
          ...attempt,
          outcome: { ...attempt.outcome!, status: "observed", fixture: true },
        },
      ],
    });
    expect(
      await screen.findByText("这是一份测试样本，不作为实际推荐结果。"),
    ).toBeVisible();
    expect(screen.queryByRole("article")).not.toBeInTheDocument();
    expect(list).not.toHaveBeenCalled();
  });

  it("retains an observed answer and its citations without inventing recommendation results", async () => {
    focusTargets = [{ name: item.name, kind: "product", aliases: [] }];
    setup({
      attempts: [
        {
          ...attempt,
          outcome: {
            ...attempt.outcome!,
            status: "observed",
            raw_answer: "A saved answer awaiting recommendation extraction.",
            citations: ["https://example.test/observed"],
          },
        },
      ],
      revisions: [],
    });
    expect(await screen.findByText("暂无可用的推荐结果。")).toBeVisible();
    expect(
      screen.getByRole("link", { name: "https://example.test/observed" }),
    ).toBeVisible();
    expect(screen.queryByText(/本次推荐名单中没有/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("查看完整回答"));
    expect(
      screen.getByText("A saved answer awaiting recommendation extraction."),
    ).toBeVisible();
  });

  it("distinguishes verified empty recommendations from unknown and does not manufacture content advice", async () => {
    setup({ revisions: [analysis([], [])] });
    expect(await screen.findByText("这次回答没有明确推荐对象。")).toBeVisible();
    expect(
      screen.getByText("查看回答中的推荐理由，与 AI 讨论内容方向。"),
    ).toBeVisible();
    expect(
      screen.queryByRole("link", { name: /example\.test/ }),
    ).not.toBeInTheDocument();
  });

  it("separates a directly recommended brand from its affiliated products", async () => {
    focusTargets = [
      { name: "Sample brand", kind: "brand", aliases: ["Brand alias"] },
    ];
    setup({
      revisions: [
        analysis([
          {
            ...item,
            rank: 1,
            brand_attribution: {
              name: "Brand alias",
              evidence_quote: "Sample purifier belongs to Brand alias.",
            },
          },
        ]),
      ],
    });
    const focus = await screen.findByRole("region", { name: "Sample brand" });
    expect(
      within(focus).getByText("本次推荐名单中没有“Sample brand”"),
    ).toBeVisible();
    expect(within(focus).getByText("旗下产品推荐")).toBeVisible();
    expect(
      within(focus).getByRole("article", { name: item.name }),
    ).toBeVisible();
    expect(within(focus).queryByText("已推荐")).not.toBeInTheDocument();
    expect(screen.getByText("这次回答没有其他明确推荐对象。")).toBeVisible();
  });

  it("preserves unknown brand attribution instead of declaring no affiliated products", async () => {
    focusTargets = [{ name: "Sample brand", kind: "brand", aliases: [] }];
    setup();
    expect(await screen.findByText(/部分产品的品牌归属未知/)).toBeVisible();
    expect(
      screen.queryByText("本次没有确认的旗下产品推荐。"),
    ).not.toBeInTheDocument();
  });

  it("uses only safe deduplicated citations from the exact saved analysis", async () => {
    const { list, getTarget } = setup({
      revisions: [
        analysis(
          [item],
          [
            "https://example.test/this-answer",
            "https://example.test/this-answer",
            "javascript:alert(1)",
            "data:text/plain,not-a-source",
          ],
        ),
      ],
    });
    const link = await screen.findByRole("link", {
      name: "https://example.test/this-answer",
    });
    expect(link).toHaveAttribute("href", "https://example.test/this-answer");
    expect(
      within(
        screen.getByRole("region", { name: "本次回答引用的来源" }),
      ).getAllByRole("link"),
    ).toHaveLength(1);
    expect(
      screen.queryByText("https://example.test/stale-receipt"),
    ).not.toBeInTheDocument();
    expect(getTarget).toHaveBeenCalledWith("tenant", "project", "target");
    expect(list).toHaveBeenCalledWith(
      "tenant",
      "project",
      "target",
      "attempt",
      undefined,
      expect.any(AbortSignal),
    );
  });

  it("keeps separate target conditions and respects an exact target and analysis deep link", async () => {
    if (target.input.kind !== "measure")
      throw new Error("Expected measurement fixture");
    const other: jobs.ChannelTarget = {
      ...target,
      target_id: "other-target",
      input: {
        ...target.input,
        model: "Other model",
        provider: "Other AI",
      },
    };
    const { getTarget } = setup({
      targets: [target, other],
      route:
        "/?target=target&attempt=attempt&analysisRevision=selected-revision",
      revisions: [
        analysis([{ ...item, name: "Newer item" }]),
        analysis(
          [item],
          ["https://example.test/selected"],
          "selected-revision",
        ),
      ],
    });
    expect(
      await screen.findByRole("article", { name: item.name }),
    ).toBeVisible();
    expect(screen.queryByText("Newer item")).not.toBeInTheDocument();
    expect(screen.queryByText("Other AI")).not.toBeInTheDocument();
    expect(getTarget).toHaveBeenCalledTimes(1);
    expect(screen.getByText("模型：Sample model")).toBeVisible();
    expect(
      screen.getByRole("link", { name: "https://example.test/selected" }),
    ).toHaveAttribute("href", "https://example.test/selected");
  });

  it("renders different measurement conditions separately instead of combining their conclusions", async () => {
    if (target.input.kind !== "measure")
      throw new Error("Expected measurement fixture");
    setup({
      targets: [
        target,
        {
          ...target,
          target_id: "second-target",
          input: {
            ...target.input,
            provider: "Second AI",
            model: "Second model",
          },
        },
      ],
    });
    await waitFor(() =>
      expect(
        screen.getAllByRole("heading", { name: "AI 推荐了谁" }),
      ).toHaveLength(2),
    );
    expect(screen.getByText("Sample AI")).toBeVisible();
    expect(screen.getByText("Second AI")).toBeVisible();
    expect(screen.getByText("模型：Sample model")).toBeVisible();
    expect(screen.getByText("模型：Second model")).toBeVisible();
  });

  it("never substitutes a different revision when the selected revision is missing", async () => {
    setup({
      route: "/?target=target&attempt=attempt&analysisRevision=missing",
    });
    expect(
      await screen.findByText("指定的结果版本暂不可用，请查看下方原始记录。"),
    ).toBeVisible();
    expect(screen.queryByRole("article")).not.toBeInTheDocument();
  });

  it("keeps frozen evaluation answers out of content optimization guidance", async () => {
    if (target.input.kind !== "measure")
      throw new Error("Expected measurement fixture");
    setup({
      targets: [
        {
          ...target,
          input: {
            ...target.input,
            question_binding: {
              purpose: "frozen_evaluation",
              split_policy_version: "sample-split",
              reference: {
                question_id: "question",
                question_revision_id: "question-revision",
                question_set_id: "set",
                question_set_version_id: "version",
              },
            },
          },
        },
      ],
    });
    expect(
      await screen.findByText("独立评估样本，可查看原文与引用。"),
    ).toBeVisible();
    expect(
      screen.queryByText("从引用来源了解内容机会，和 AI 制定下一步。"),
    ).not.toBeInTheDocument();
  });

  it("localizes summary copy while preserving original answer language", async () => {
    await i18n.changeLanguage("en");
    setup();
    expect(
      await screen.findByRole("heading", { name: "Who did AI recommend?" }),
    ).toBeVisible();
    expect(screen.getByText("Quiet operation")).toBeVisible();
    expect(screen.getByText("No explicit rank")).toBeVisible();
  });

  it("links the selected plan to chat using only its stable business reference", async () => {
    setup();
    const link = screen.getByRole("link", { name: "与 AI 讨论本次测量" });
    expect(link).toHaveAttribute(
      "href",
      "/app/tenant/project/chat?measurementPlan=11111111-1111-4111-8111-111111111111",
    );
    expect(link.getAttribute("href")).not.toContain("question");
  });

  it("keeps saved results visible when a background refresh fails", async () => {
    const { getTarget, client } = setup();
    expect(
      await screen.findByRole("article", { name: item.name }),
    ).toBeVisible();
    getTarget.mockRejectedValue(new Error("Unavailable"));
    await act(() => client.invalidateQueries({ queryKey: ["channel-target"] }));
    expect(
      await screen.findByText("最新状态暂时无法读取，以下保留已有结果。"),
    ).toBeVisible();
    expect(screen.getByRole("article", { name: item.name })).toBeVisible();
    expect(screen.getByRole("button", { name: "重新读取" })).toBeVisible();
  });
});
