import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type { CitationInsightsPage } from "../api/citationInsights";
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

function mockApi(pages: CitationInsightsPage[], failFirst = false) {
  const calls: string[] = [];
  let failed = false;
  vi.stubGlobal(
    "fetch",
    vi.fn((request: RequestInfo | URL) => {
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
                role: "viewer",
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
  return calls;
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
  it("explains an empty batch without implying missing measurements had no citations", async () => {
    const calls = mockApi([emptyPage]);
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
    const calls = mockApi([populatedPage, emptyPage]);
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
    const calls = mockApi([populatedPage, emptyPage]);
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
    const calls = mockApi([emptyPage], true);
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
