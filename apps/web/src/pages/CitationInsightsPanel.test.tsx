import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type {
  CitationInsightsPage,
  SourceChannelRecommendationsPage,
} from "../api/citationInsights";
import { CitationInsightsPanel } from "./CitationInsightsPanel";

const reply = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

const sample = {
  plan_id: "plan-1",
  target_id: "target-1",
  attempt_id: "attempt-1",
  provider: "provider-a",
  model: "model-a",
  surface: "consumer_web",
  search_mode: "web_search",
  market: "CN",
  language: "zh-CN",
  question_set_version: "ad_hoc.v1",
  question_purpose: "frozen_evaluation" as const,
  scheduled_at: "2026-10-01T10:00:00Z",
  observed_at: "2026-10-01T10:01:00Z",
  received_at: "2026-10-01T10:02:00Z",
};
const emptyPage: CitationInsightsPage = {
  scope: "returned_plans_only",
  plan_ids: [],
  next_after: null,
  coverage: {
    planned: 0,
    pending: 0,
    observed_live: 0,
    observed_unverified: 0,
    observed_without_citations: 0,
    refused: 0,
    missing: 0,
    other_completed: 0,
    fixture: 0,
  },
  invalid_citation_urls: 0,
  observed_sources: [],
};
const populatedPage: CitationInsightsPage = {
  ...emptyPage,
  plan_ids: ["plan-1", "plan-2"],
  next_after: "plan-2",
  coverage: {
    planned: 9,
    pending: 2,
    observed_live: 3,
    observed_unverified: 1,
    observed_without_citations: 1,
    refused: 1,
    missing: 1,
    other_completed: 1,
    fixture: 1,
  },
  observed_sources: [
    {
      host: "example.org",
      citing_answers: 2,
      samples: [sample, { ...sample, attempt_id: "attempt-2" }],
      urls: [
        {
          url: "https://example.org/article",
          citing_answers: 2,
          samples: [sample, { ...sample, attempt_id: "attempt-2" }],
        },
        {
          url: "javascript:alert(1)",
          citing_answers: 1,
          samples: [sample],
        },
      ],
    },
  ],
};
const recommendedPage: SourceChannelRecommendationsPage = {
  scope: "returned_plans_only",
  plan_ids: ["plan-1", "plan-2"],
  next_after: "plan-2",
  rule_version: "v1",
  coverage: populatedPage.coverage,
  items: [
    {
      source_hosts: ["example.org", "blog.example.org"],
      platform_id: "platform-a",
      placement_slot: "article",
      rule_ids: ["rule-1", "rule-2"],
      host_relationships: ["first_party_article"],
      citing_answers: 2,
      samples: [sample],
      publication: {
        connector_availability: "unavailable",
        account_ready: false,
        reason: "connector_unavailable",
      },
      targeted: false,
    },
    {
      source_hosts: ["unmapped.example"],
      platform_id: null,
      placement_slot: null,
      rule_ids: [],
      host_relationships: [],
      citing_answers: 1,
      samples: [sample],
      publication: {
        connector_availability: "unmapped",
        account_ready: false,
        reason: "source_not_mapped_to_publishing_channel",
      },
      targeted: false,
    },
  ],
};

function projectForRecommendations(mode: "all_eligible" | "explicit") {
  return {
    id: "project-1",
    revision: 7,
    settings: {
      distribution_scope: {
        mode,
        included_platform_ids: mode === "explicit" ? ["platform-b"] : [],
        excluded_platform_ids: ["platform-a", "platform-c"],
        resource_pool_ids: ["pool-a"],
        replication_policy: "one_account_per_platform",
      },
    },
  };
}

function mockApi(
  pages: CitationInsightsPage[],
  failFirst = false,
  options?: {
    recommendations: SourceChannelRecommendationsPage;
    mode: "all_eligible" | "explicit";
    conflict?: boolean;
    conflictAfterSave?: boolean;
    failFirstUpdate?: boolean;
  },
) {
  const calls: string[] = [];
  const patchKeys: Array<string | null> = [];
  const patches: Array<{
    revision: number;
    settings: Record<string, unknown>;
  }> = [];
  let project = options ? projectForRecommendations(options.mode) : null;
  let failed = false;
  let failedUpdate = false;
  vi.stubGlobal(
    "fetch",
    vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(request), "http://localhost");
      calls.push(url.toString());
      if (url.pathname.endsWith("/auth/session"))
        return Promise.resolve(
          reply({
            user: {
              id: "user-1",
              login_name: "user@example.test",
              display_name: "User",
            },
            operator: {
              id: "operator-1",
              slug: "operator",
              display_name: "Operator",
            },
            memberships: [
              {
                tenant_id: "tenant-1",
                tenant_slug: "tenant",
                tenant_display_name: "Tenant",
                role: options ? "member" : "viewer",
              },
            ],
            expires_at: "2026-10-01T00:00:00Z",
            csrf_token: "csrf-test",
          }),
        );
      if (url.pathname.endsWith("/citation-insights")) {
        if (failFirst && !failed) {
          failed = true;
          return Promise.resolve(
            reply({ code: "internal", message: "Unavailable" }, 503),
          );
        }
        return Promise.resolve(
          reply(url.searchParams.has("after") ? pages[1] : pages[0]),
        );
      }
      if (url.pathname.endsWith("/source-channel-recommendations"))
        return Promise.resolve(
          options
            ? reply(options.recommendations)
            : reply({ code: "not_found" }, 404),
        );
      if (url.pathname.endsWith("/projects/project-1") && project) {
        if (init?.method === "PATCH") {
          patches.push(JSON.parse(String(init.body)));
          patchKeys.push(new Headers(init.headers).get("Idempotency-Key"));
          if (options?.failFirstUpdate && !failedUpdate) {
            failedUpdate = true;
            return Promise.reject(new TypeError("Uncertain connection"));
          }
          if (options?.conflict)
            return Promise.resolve(reply({ code: "conflict" }, 409));
          project = {
            ...project,
            revision: project.revision + 1,
            settings: {
              ...project.settings,
              ...patches.at(-1)!.settings,
            },
          } as typeof project;
          if (options?.conflictAfterSave)
            return Promise.resolve(reply({ code: "conflict" }, 409));
        }
        return Promise.resolve(reply(project));
      }
      if (url.pathname.endsWith("/channel-targets/target-1"))
        return Promise.resolve(
          reply({
            target: {
              target_id: "target-1",
              input: { kind: "measure", question: "What was cited?" },
            },
            attempts: [
              {
                attempt_id: "attempt-1",
                outcome: { raw_answer: "Answer with evidence." },
              },
            ],
          }),
        );
      return Promise.resolve(
        reply({ code: "not_found", message: "Not found" }, 404),
      );
    }),
  );
  return { calls, patches, patchKeys };
}

function panel(projectId: string) {
  return (
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <CitationInsightsPanel tenantId="tenant-1" projectId={projectId} />
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>
  );
}

function renderPanel() {
  return render(panel("project-1"));
}

afterEach(() => {
  vi.unstubAllGlobals();
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
});

describe("citation insights", () => {
  it.each(["all_eligible", "explicit"] as const)(
    "adds a mapped target in %s mode without losing exclusions, pools or other targets",
    async (mode) => {
      const { calls, patches } = mockApi([populatedPage], false, {
        recommendations: recommendedPage,
        mode,
      });
      const user = userEvent.setup();
      renderPanel();
      expect(
        await screen.findByText("example.org · blog.example.org"),
      ).toBeInTheDocument();
      expect(screen.getByText("尚未归类的信源")).toBeInTheDocument();
      expect(screen.getByText(/发布连接方式尚不可用/)).toBeInTheDocument();
      expect(
        screen.getByText("依据当前 2 个测量计划的回答"),
      ).toBeInTheDocument();
      expect(screen.getByText(/尚无可用发布连接方式/)).toBeInTheDocument();
      expect(
        screen.queryAllByRole("button", { name: "列入后续发布目标" }),
      ).toHaveLength(1);
      await user.click(
        screen.getByRole("button", { name: "列入后续发布目标" }),
      );
      expect(
        await screen.findByText("已保存并核对项目目标；后续周期会采用新设置。"),
      ).toBeInTheDocument();
      expect(patches).toHaveLength(1);
      expect(patches[0].revision).toBe(7);
      expect(patches[0].settings).toEqual({
        distribution_scope: {
          mode,
          included_platform_ids:
            mode === "explicit" ? ["platform-b", "platform-a"] : [],
          excluded_platform_ids: ["platform-c"],
          resource_pool_ids: ["pool-a"],
          replication_policy: "one_account_per_platform",
        },
      });
      expect(
        calls.filter((call) => call.includes("/projects/project-1?")),
      ).toHaveLength(4);
    },
  );

  it("preserves the user's selection on a project revision conflict", async () => {
    const { patches } = mockApi([populatedPage], false, {
      recommendations: recommendedPage,
      mode: "explicit",
      conflict: true,
    });
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText("example.org · blog.example.org");
    await user.click(screen.getByRole("button", { name: "列入后续发布目标" }));
    expect(
      await screen.findByText("项目设置已在其他位置更新；请重新查看后再选择。"),
    ).toBeInTheDocument();
    expect(patches).toHaveLength(1);
    expect(
      screen.getByRole("button", { name: "列入后续发布目标" }),
    ).toBeEnabled();
    expect(
      screen.queryByText("已保存并核对项目目标；后续周期会采用新设置。"),
    ).toBeNull();
  });

  it("retries an uncertain update with the same revision, scope and key", async () => {
    const { patches, patchKeys } = mockApi([populatedPage], false, {
      recommendations: recommendedPage,
      mode: "explicit",
      failFirstUpdate: true,
    });
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText("example.org · blog.example.org");
    await user.click(screen.getByRole("button", { name: "列入后续发布目标" }));
    expect(
      await screen.findByText("未能保存目标；请重试。"),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "列入后续发布目标" }));
    expect(
      await screen.findByText("已保存并核对项目目标；后续周期会采用新设置。"),
    ).toBeInTheDocument();
    expect(patches).toHaveLength(2);
    expect(patches[1]).toEqual(patches[0]);
    expect(patchKeys[0]).toBeTruthy();
    expect(patchKeys[1]).toBe(patchKeys[0]);
  });

  it("reconciles an ambiguous conflict with the persisted project scope", async () => {
    const { patches } = mockApi([populatedPage], false, {
      recommendations: recommendedPage,
      mode: "all_eligible",
      conflictAfterSave: true,
    });
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText("example.org · blog.example.org");
    await user.click(screen.getByRole("button", { name: "列入后续发布目标" }));
    expect(
      await screen.findByText("已保存并核对项目目标；后续周期会采用新设置。"),
    ).toBeInTheDocument();
    expect(patches).toHaveLength(1);
  });

  it("explains an empty batch without implying missing measurements had no citations", async () => {
    const { calls } = mockApi([emptyPage]);
    renderPanel();
    expect(
      await screen.findByText(
        "还没有可分析的引用，完成一次联网测量后在这里查看来源。",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("计划测量 0 项")).toBeInTheDocument();
    expect(
      calls.some((call) => call.includes("citation-insights?limit=5")),
    ).toBe(true);
  });

  it("keeps page-local denominators and evidence distinct from missing, unverified, and fixtures", async () => {
    mockApi([populatedPage, emptyPage]);
    const user = userEvent.setup();
    renderPanel();
    expect(await screen.findByText("计划测量 9 项")).toBeInTheDocument();
    expect(screen.getByText("测量计划 2 个")).toBeInTheDocument();
    expect(
      screen.getByText("未核验回答 1 项（不计入引用）"),
    ).toBeInTheDocument();
    expect(screen.getByText("其中无引用 1 项")).toBeInTheDocument();
    expect(screen.getByText("缺测 1 项")).toBeInTheDocument();
    expect(screen.getByText("模拟记录 1 项（不计入引用）")).toBeInTheDocument();
    expect(screen.getByText("2 条回答引用")).toBeInTheDocument();
    await user.click(screen.getByText("具体网页"));
    const link = screen.getByRole("link", {
      name: "https://example.org/article",
    });
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    expect(link).toHaveAttribute("target", "_blank");
    expect(
      screen.queryByRole("link", { name: "javascript:alert(1)" }),
    ).toBeNull();
    await user.click(
      screen.getAllByRole("button", { name: "查看原始问答" })[0],
    );
    expect(await screen.findByText("What was cited?")).toBeInTheDocument();
    expect(screen.getByText("Answer with evidence.")).toBeInTheDocument();
    expect(
      screen.getByText("冻结评估问题 · 不进入内容优化"),
    ).toBeInTheDocument();
  });

  it("moves between batches without presenting the first page's totals as global", async () => {
    const { calls } = mockApi([populatedPage, emptyPage]);
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText("计划测量 9 项");
    await user.click(screen.getByRole("button", { name: "下一批" }));
    expect(await screen.findByText("计划测量 0 项")).toBeInTheDocument();
    expect(screen.queryByText("计划测量 9 项")).toBeNull();
    expect(calls.some((call) => call.includes("after=plan-2"))).toBe(true);
    await user.click(screen.getByRole("button", { name: "上一批" }));
    expect(await screen.findByText("计划测量 9 项")).toBeInTheDocument();
  });

  it("starts at the first batch when the project scope changes", async () => {
    const { calls } = mockApi([populatedPage, emptyPage]);
    const user = userEvent.setup();
    const view = renderPanel();
    await screen.findByText("计划测量 9 项");
    await user.click(screen.getByRole("button", { name: "下一批" }));
    await screen.findByText("计划测量 0 项");
    view.rerender(panel("project-2"));
    expect(await screen.findByText("计划测量 9 项")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "上一批" })).toBeNull();
    expect(
      calls.find((call) =>
        call.includes("/projects/project-2/citation-insights"),
      ),
    ).not.toContain("after=");
  });

  it("shows a recoverable error without inventing source records", async () => {
    const { calls } = mockApi([emptyPage], true);
    const user = userEvent.setup();
    renderPanel();
    const heading = await screen.findByText("引用信源暂时无法读取");
    expect(
      within(heading.closest(".fui-MessageBar")!).getByRole("button", {
        name: /重试/,
      }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(
        "还没有可分析的引用，完成一次联网测量后在这里查看来源。",
      ),
    ).toBeNull();
    await user.click(
      within(heading.closest(".fui-MessageBar")!).getByRole("button", {
        name: /重试/,
      }),
    );
    expect(await screen.findByText("计划测量 0 项")).toBeInTheDocument();
    expect(
      calls.filter((call) => call.includes("citation-insights")),
    ).toHaveLength(2);
  });
});
