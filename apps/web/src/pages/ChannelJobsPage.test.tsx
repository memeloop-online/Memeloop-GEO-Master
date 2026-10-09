import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type {
  ChannelOutcome,
  ChannelPlan,
  ChannelTarget,
  ChannelTargetView,
} from "../api/channelJobs";
import { ChannelJobsPage, TargetCard } from "./ChannelJobsPage";
import i18n from "../i18n";

const automaticTarget: ChannelTarget = {
  target_id: "measurement-target",
  input: {
    kind: "measure",
    question: "如何选择办公工具？",
    provider: "example",
    account_id: "measurement-account",
    model: "example-model",
    surface: "consumer_web",
    search_mode: "web_search",
    protocol_version: "v1",
    question_set_version: "v1",
    market: "CN",
    language: "zh-CN",
    scheduled_at: "2026-10-01T00:00:00Z",
    sample_ordinal: 0,
  },
};

async function renderAutomaticResult(overrides: Partial<ChannelOutcome> = {}) {
  mockApi();
  const result = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <MemoryRouter>
            <TargetCard
              target={automaticTarget}
              tenantId="tenant-1"
              projectId="project-1"
              automatic
              canWrite
              loading={false}
              loadError={null}
              executing={false}
              executeError={null}
              onExecute={vi.fn()}
              onRefresh={vi.fn()}
              view={{
                target: automaticTarget,
                attempts: [
                  {
                    attempt_id: "private-attempt-id",
                    target_id: automaticTarget.target_id,
                    claimed_at: "2026-10-01T00:00:00Z",
                    received_at: "2026-10-01T00:01:00Z",
                    outcome: {
                      status: "observed",
                      detail: "internal_reason_code",
                      occurred_at: "2026-10-01T00:01:00Z",
                      raw_answer: "第一行\n第二行 <script>alert(1)</script>",
                      citations: ["https://example.com/source"],
                      public_url: null,
                      screenshot_ref: null,
                      connector_version: "internal-connector-v3",
                      runner_evidence: [{ source_json: "large-audit-payload" }],
                      fixture: false,
                      ...overrides,
                    },
                  },
                ],
              }}
            />
          </MemoryRouter>
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>,
  );
  await screen.findByText(i18n.t("noSource", { ns: "observationAnalysis" }));
  return result;
}

describe("automatic measurement results", () => {
  it("shows the answer as plain text and citations, with evidence only on demand", async () => {
    const { container } = await renderAutomaticResult();
    expect(screen.getByRole("heading", { name: "回答" })).toBeVisible();
    const answer = screen.getByText(/第一行/);
    expect(answer.textContent).toBe("第一行\n第二行 <script>alert(1)</script>");
    expect(answer).toHaveStyle({ whiteSpace: "pre-wrap" });
    expect(container.querySelector("script")).toBeNull();
    expect(
      screen.getByRole("link", { name: "https://example.com/source" }),
    ).toHaveAttribute("rel", "noopener noreferrer");
    expect(screen.queryByText(/large-audit-payload/)).not.toBeInTheDocument();
    expect(screen.queryByText(/private-attempt-id/)).not.toBeInTheDocument();
    const details = container.querySelector("details")!;
    expect(details.open).toBe(false);
    await userEvent.click(screen.getByText("技术详情 / 原始证据"));
    await waitFor(() =>
      expect(screen.getByText(/large-audit-payload/)).toBeVisible(),
    );
  });

  it("only links safe http and https citations", async () => {
    await renderAutomaticResult({
      citations: [
        "https://example.com/source",
        "http://example.com/other",
        "javascript:alert(1)",
        "data:text/html,test",
        "/relative",
        "https://user:password@example.com/private",
        "https://example.com/source",
      ],
    });
    expect(screen.getAllByRole("link", { name: /^https?:/ })).toHaveLength(2);
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(
      screen.getByRole("link", { name: "http://example.com/other" }),
    ).toHaveAttribute("href", "http://example.com/other");
  });

  it("does not infer a search failure from an answer without citations", async () => {
    await renderAutomaticResult({ citations: [] });
    expect(screen.getByText("本次回答未提供引用链接。")).toBeVisible();
    expect(screen.queryByText(/未联网/)).not.toBeInTheDocument();
  });

  it.each(["unknown", "missing"] as const)(
    "does not present an unconfirmed %s answer as a result",
    async (status) => {
      await renderAutomaticResult({ status });
      expect(
        screen.queryByRole("heading", { name: "回答" }),
      ).not.toBeInTheDocument();
      expect(screen.queryByText(/第一行/)).not.toBeInTheDocument();
      expect(
        screen.queryByRole("link", { name: /^https?:/ }),
      ).not.toBeInTheDocument();
    },
  );

  it("labels fixtures and does not present them as real answers", async () => {
    await renderAutomaticResult({ fixture: true });
    expect(screen.getAllByText("测试数据，非真实测量").length).toBeGreaterThan(
      0,
    );
    expect(screen.queryByText("已取得测量结果")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "回答" }),
    ).not.toBeInTheDocument();
  });

  it("localizes the result in English", async () => {
    await i18n.changeLanguage("en");
    try {
      await renderAutomaticResult({ citations: [] });
      expect(screen.getByRole("heading", { name: "Answer" })).toBeVisible();
      expect(
        screen.getByText("This answer did not provide citation links."),
      ).toBeVisible();
      expect(
        screen.getByText("Technical details / original evidence"),
      ).toBeVisible();
    } finally {
      await act(async () => {
        await i18n.changeLanguage("zh-CN");
      });
    }
  });
});

const cycleId = "cycle-1";
const source = {
  source_id: "source-1",
  revision: 1,
  kind: "file",
  name: "公开说明.md",
  purpose: "public",
  state: "active",
  current_version_id: "version-1",
  product_ids: [],
};
const account = {
  account_id: "account-1",
  project_id: "project-1",
  platform: "zhihu",
  group_id: null,
  status: "ready",
  display_name: "项目账号",
  platform_account_id: "external-1",
  avatar_url: null,
  enabled: true,
  proxy_configured: false,
  proxy_server: null,
  created_at: "2026-10-01T00:00:00Z",
  updated_at: "2026-10-01T00:00:00Z",
};
const measurementAccount = {
  ...account,
  account_id: "measure-account-1",
  platform: "kimi",
  display_name: "测量账号",
};
const target = {
  target_id: "target-1",
  input: {
    kind: "publish" as const,
    source_id: "source-1",
    source_version_id: "version-1",
    platform: "zhihu",
    account_id: "account-1",
    title: "公开说明",
    body: "版本正文",
    body_sha256: "hash",
  },
};
const frozenPlan: ChannelPlan = {
  plan_id: "plan-1",
  project_id: "project-1",
  cycle_id: cycleId,
  input_hash: "hash",
  revision: 1,
  created_at: "2026-10-01T00:00:00Z",
  targets: [target],
};
const boundVersion = {
  id: "set-version-1",
  question_set_id: "set-1",
  revision: 1,
  parent_version_id: null,
  name: "计划问题",
  optimization_count: 0,
  evaluation_count: 1,
  split_policy_version: "project_registry_nfkc_v1",
  content_hash: "hash",
  created_at: "2026-10-01T00:00:00Z",
  questions: [
    {
      id: "revision-1",
      question_id: "question-1",
      text: "如何选择？",
      intent: "selection",
      product_refs: [],
      market: "CN",
      language: "zh-CN",
      source: { kind: "user_provided" },
      weight: 1,
      purpose: "frozen_evaluation",
      split_policy_version: "project_registry_nfkc_v1",
    },
  ],
};

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function mockApi({
  accounts = [account],
  sources = [source],
  plan: initialPlan,
  outcome,
  executeError = false,
  role = "tenant_admin",
  currentCycleId = cycleId,
  cycleError = false,
  measurementSupported = true,
  planConflict = false,
  questionSets = [],
  questionVersion = null,
}: {
  accounts?: unknown[];
  sources?: unknown[];
  plan?: ChannelPlan;
  outcome?: "unknown" | "unsupported" | "login_required";
  executeError?: boolean;
  role?: string;
  currentCycleId?: string | null;
  cycleError?: boolean;
  measurementSupported?: boolean;
  planConflict?: boolean;
  questionSets?: unknown[];
  questionVersion?: unknown;
} = {}) {
  let plan = initialPlan;
  let detail: ChannelTargetView = { target, attempts: [] };
  const requests: Array<{
    path: string;
    method: string;
    body: unknown;
    url: URL;
  }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(request), "http://localhost");
      const path = url.pathname;
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : null;
      requests.push({ path, method, body, url });
      if (path.endsWith("/analyses"))
        return Promise.resolve(
          response({ items: [], sources: [], next_after: null }),
        );
      if (path.endsWith("/auth/session"))
        return Promise.resolve(
          response({
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
                role,
              },
            ],
            expires_at: "2026-10-01T00:00:00Z",
            csrf_token: "csrf-test",
          }),
        );
      if (path.endsWith("/projects/project-1/question-sets"))
        return Promise.resolve(
          response({ items: questionSets, next_cursor: null }),
        );
      if (path.endsWith("/measurement-plans"))
        return Promise.resolve(response({ items: [], next_after: null }));
      if (path.endsWith("/measurement-options"))
        return Promise.resolve(
          response({
            models: [{ id: "visible-model", label: "网页模型" }],
            selected_model: "visible-model",
          }),
        );
      if (path.endsWith("/question-sets/set-1/versions"))
        return Promise.resolve(
          response({
            items: questionVersion
              ? [
                  {
                    id: "set-version-1",
                    question_set_id: "set-1",
                    revision: 1,
                    name: "计划问题",
                    question_count: 1,
                    optimization_count: 0,
                    evaluation_count: 1,
                  },
                ]
              : [],
            next_cursor: null,
          }),
        );
      if (path.endsWith("/question-sets/set-1/versions/set-version-1"))
        return Promise.resolve(response(questionVersion));
      if (path.endsWith("/projects/project-1/cycles/current"))
        return Promise.resolve(
          cycleError
            ? response({ code: "internal", message: "cycle unavailable" }, 503)
            : currentCycleId
              ? response({
                  project_id: "project-1",
                  cycle_id: currentCycleId,
                  report_timezone: "Asia/Shanghai",
                  report_window_start_at: "2026-10-01T00:00:00Z",
                  report_window_end_at: "2026-10-08T00:00:00Z",
                  cutoff_at: "2026-10-09T00:00:00Z",
                  document_manifest: null,
                  distribution_manifest: null,
                })
              : response({ code: "not_found", message: "no cycle" }, 404),
        );
      if (path.endsWith(`/cycles/${currentCycleId}/channel-plan`)) {
        if (method === "POST") {
          if (planConflict)
            return Promise.resolve(
              response(
                { code: "conflict", message: "plan already sealed" },
                409,
              ),
            );
          plan = frozenPlan;
          return Promise.resolve(response(plan));
        }
        return Promise.resolve(
          plan
            ? response(plan)
            : response({ code: "not_found", message: "no plan" }, 404),
        );
      }
      if (path.endsWith("/knowledge/sources/source-1"))
        return Promise.resolve(
          response({
            source,
            versions: [
              {
                source_version_id: "version-1",
                source_id: "source-1",
                version: 1,
              },
            ],
            chunks: [],
            facts: [],
            import_jobs: [],
            impact: {},
          }),
        );
      if (path.endsWith("/knowledge/sources"))
        return Promise.resolve(response({ items: sources, next_cursor: null }));
      if (path.endsWith("/channel-accounts"))
        return Promise.resolve(response({ items: accounts }));
      if (path.endsWith("/channel-groups"))
        return Promise.resolve(response({ items: [] }));
      if (path.endsWith("/channel-platforms"))
        return Promise.resolve(
          response({
            items: [
              {
                id: "zhihu",
                label: "知乎",
                purpose: "publishing",
                login_supported: true,
              },
              ...(measurementSupported
                ? [
                    {
                      id: "kimi",
                      label: "Kimi 网页",
                      purpose: "measurement",
                      login_supported: true,
                    },
                  ]
                : []),
            ],
          }),
        );
      if (path.endsWith("/channel-targets/target-1/execute")) {
        if (executeError)
          return Promise.resolve(
            response(
              {
                code: "internal",
                message: "network ambiguous",
              },
              503,
            ),
          );
        detail = {
          target,
          attempts: [
            {
              attempt_id: "attempt-1",
              target_id: "target-1",
              claimed_at: "2026-10-01T00:00:00Z",
              received_at: "2026-10-01T00:00:01Z",
              outcome: {
                status: outcome ?? "unknown",
                detail: "runner did not provide verified external evidence",
                occurred_at: "2026-10-01T00:00:01Z",
                raw_answer: null,
                citations: [],
                public_url: null,
                screenshot_ref: null,
                connector_version: "unverified.v1",
                runner_evidence: [
                  { kind: "runner_status", value: outcome ?? "unknown" },
                ],
                fixture: false,
              },
            },
          ],
        };
        return Promise.resolve(response(detail));
      }
      if (path.endsWith("/channel-targets/target-1"))
        return Promise.resolve(response(detail));
      if (path.endsWith("/channel-targets/target-1/publication-lookup"))
        return Promise.resolve(
          response({
            target_id: "target-1",
            attempt_id: "attempt-1",
            job: null,
            observations: [],
            next_before: null,
          }),
        );
      return Promise.resolve(
        response({ code: "not_found", message: "not found" }, 404),
      );
    }),
  );
  return requests;
}

function renderPage() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <MemoryRouter
            initialEntries={["/app/tenant-1/project-1/publications"]}
          >
            <Routes>
              <Route
                path="/app/:tenantId/:projectId/publications"
                element={<ChannelJobsPage />}
              />
            </Routes>
          </MemoryRouter>
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
});

describe("P12 channel jobs", () => {
  it("labels frozen bound samples by purpose and absent historical bindings as unclassified", async () => {
    const input = {
      kind: "measure" as const,
      account_id: "measure-account-1",
      provider: "kimi",
      model: "visible-model",
      surface: "consumer_web",
      search_mode: "web_search",
      protocol_version: "v1",
      question_set_version: "set-version-1",
      question: "如何选择？",
      market: "CN",
      language: "zh-CN",
      scheduled_at: "2026-10-06T04:30:00Z",
      sample_ordinal: 0,
    };
    const bound: ChannelPlan = {
      ...frozenPlan,
      targets: [
        {
          target_id: "target-1",
          input: {
            ...input,
            question_binding: {
              reference: {
                question_set_id: "set-1",
                question_set_version_id: "set-version-1",
                question_id: "question-1",
                question_revision_id: "revision-1",
              },
              purpose: "frozen_evaluation",
              split_policy_version: "project_registry_nfkc_v1",
            },
          },
        },
      ],
    };
    mockApi({ plan: bound });
    const { unmount } = renderPage();
    expect(
      await screen.findByText(/用途 冻结评估（不进入优化）/),
    ).toBeInTheDocument();
    unmount();
    mockApi({
      plan: {
        ...frozenPlan,
        targets: [{ target_id: "target-1", input }],
      },
    });
    renderPage();
    expect(
      await screen.findByText(/用途 自定义问题（不进入优化）/),
    ).toBeInTheDocument();
  });

  it("freezes a bound version member by reference only, without text or caller-controlled purpose", async () => {
    const requests = mockApi({
      accounts: [measurementAccount],
      sources: [],
      questionSets: [
        {
          id: "set-1",
          name: "计划问题",
          current_version_id: "set-version-1",
          current_revision: 1,
          question_count: 1,
          optimization_count: 0,
          evaluation_count: 1,
        },
      ],
      questionVersion: boundVersion,
    });
    const user = userEvent.setup();
    renderPage();
    await user.selectOptions(
      await screen.findByLabelText("绑定问题集"),
      "set-1",
    );
    await user.selectOptions(
      await screen.findByLabelText("绑定不可变版本"),
      "set-version-1",
    );
    await user.selectOptions(
      await screen.findByLabelText("绑定问题"),
      "question-1",
    );
    expect(screen.getByText(/已选用途：冻结评估/)).toBeInTheDocument();
    await user.selectOptions(
      screen.getByLabelText("项目 Kimi 测量账号"),
      "measure-account-1",
    );
    await user.type(screen.getByLabelText("可见模型标识"), "visible-model");
    await user.type(screen.getByLabelText("采样协议版本"), "web-v1");
    await user.type(
      screen.getByLabelText("计划采样时间（本地时间）"),
      "2026-10-06T12:30",
    );
    await user.click(screen.getByRole("button", { name: "加入测量目标" }));
    await user.click(screen.getByRole("button", { name: "封存本轮计划" }));
    const body = requests.find(
      (item) =>
        item.method === "POST" &&
        item.path.endsWith("/cycles/cycle-1/channel-plan"),
    )?.body;
    expect(body).toEqual({
      publications: [],
      measurements: [],
      bound_measurements: [
        {
          account_id: "measure-account-1",
          provider: "kimi",
          model: "visible-model",
          surface: "consumer_web",
          search_mode: "web_search",
          protocol_version: "web-v1",
          question: {
            question_set_id: "set-1",
            question_set_version_id: "set-version-1",
            question_id: "question-1",
            question_revision_id: "revision-1",
          },
          scheduled_at: new Date("2026-10-06T12:30").toISOString(),
          sample_ordinal: 0,
        },
      ],
    });
  });

  it("builds a cycle-scoped sealed plan from a public source version and assigned account", async () => {
    const requests = mockApi();
    const user = userEvent.setup();
    renderPage();
    await user.selectOptions(
      await screen.findByLabelText("公开 TXT/Markdown 来源"),
      "source-1",
    );
    await user.selectOptions(
      await screen.findByLabelText("项目发布账号"),
      "account-1",
    );
    await waitFor(() =>
      expect(screen.getByLabelText("来源版本")).toHaveValue("version-1"),
    );
    await user.click(screen.getByRole("button", { name: "加入目标" }));
    await user.click(screen.getByRole("button", { name: "封存本轮计划" }));
    await screen.findByText(/版本 1 ·/);
    const request = requests.find(
      (item) =>
        item.method === "POST" &&
        item.path.endsWith("/cycles/cycle-1/channel-plan"),
    );
    expect(request?.body).toEqual({
      publications: [
        {
          source_id: "source-1",
          source_version_id: "version-1",
          platform: "zhihu",
          account_id: "account-1",
        },
      ],
      measurements: [],
      bound_measurements: [],
    });
    expect(request?.url.searchParams.get("tenant_id")).toBe("tenant-1");
    expect(request?.url.searchParams.get("project_id")).toBe("project-1");
    const current = requests.find((item) =>
      item.path.endsWith("/projects/project-1/cycles/current"),
    );
    expect(current?.url.searchParams.get("tenant_id")).toBe("tenant-1");
    expect(current?.url.searchParams.get("project_id")).toBe("project-1");
    expect(
      screen.queryByRole("button", { name: "加入目标" }),
    ).not.toBeInTheDocument();
    expect(
      requests.some((item) => item.path.endsWith("/projects/project-1/start")),
    ).toBe(false);
  });

  it("follows an advanced current cycle instead of the original start cycle", async () => {
    const requests = mockApi({ currentCycleId: "cycle-2" });
    renderPage();
    expect(await screen.findByText("当前周期：cycle-2")).toBeInTheDocument();
    await screen.findByRole("button", { name: "封存本轮计划" });
    expect(
      requests.some((item) =>
        item.path.endsWith("/cycles/cycle-2/channel-plan"),
      ),
    ).toBe(true);
    expect(
      requests.some((item) =>
        item.path.endsWith("/cycles/cycle-1/channel-plan"),
      ),
    ).toBe(false);
    expect(
      requests.some((item) => item.path.endsWith("/projects/project-1/start")),
    ).toBe(false);
  });

  it("shows the unstarted state when no current cycle exists", async () => {
    const requests = mockApi({ currentCycleId: null });
    renderPage();
    expect(await screen.findByText("尚无周期发布计划")).toBeInTheDocument();
    expect(screen.queryByLabelText("要测量的问题")).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "管理问题集" })).toHaveAttribute(
      "href",
      "/app/tenant-1/project-1/measurement",
    );
    expect(requests.some((item) => item.path.includes("/channel-plan"))).toBe(
      false,
    );
  });

  it("keeps the independent measurement deep link when current-cycle loading fails", async () => {
    mockApi({ cycleError: true, sources: [], accounts: [measurementAccount] });
    renderPage();
    expect(await screen.findByText("无法读取当前项目周期")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "管理问题集" })).toHaveAttribute(
      "href",
      "/app/tenant-1/project-1/measurement",
    );
    expect(screen.queryByLabelText("要测量的问题")).not.toBeInTheDocument();
  });

  it("does not seal without a source or account and explains the missing resource", async () => {
    const requests = mockApi({ accounts: [], sources: [] });
    renderPage();
    expect(
      await screen.findByText(/没有可选的公开 TXT\/Markdown/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/尚无分配给项目的可用发布账号/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "封存本轮计划" })).toBeDisabled();
    expect(requests.filter((item) => item.method === "POST")).toHaveLength(0);
  });

  it("freezes a typed independent Kimi measurement without any publication", async () => {
    const requests = mockApi({
      sources: [],
      accounts: [account, measurementAccount],
    });
    const user = userEvent.setup();
    renderPage();
    await user.selectOptions(
      await screen.findByLabelText("测量问题模式"),
      "legacy",
    );
    await user.selectOptions(
      await screen.findByLabelText("项目 Kimi 测量账号"),
      "measure-account-1",
    );
    await user.type(screen.getByLabelText("可见模型标识"), "observed-model");
    await user.type(screen.getByLabelText("采样协议版本"), "protocol-v1");
    await user.type(screen.getByLabelText("临时问题集标签"), "evaluation-v2");
    await user.type(screen.getByLabelText("市场"), "CN");
    await user.type(screen.getByLabelText("语言"), "zh-CN");
    await user.type(
      screen.getByLabelText("临时问题（未分类）"),
      "这是什么产品？",
    );
    await user.clear(screen.getByLabelText("样本序号（0–10000）"));
    await user.type(screen.getByLabelText("样本序号（0–10000）"), "3");
    await user.type(
      screen.getByLabelText("计划采样时间（本地时间）"),
      "2026-10-06T12:30",
    );
    expect(
      screen.getByText(/官方联网搜索适配器尚未实测验证/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "加入测量目标" }));
    await user.click(screen.getByRole("button", { name: "封存本轮计划" }));
    const request = requests.find(
      (item) =>
        item.method === "POST" &&
        item.path.endsWith("/cycles/cycle-1/channel-plan"),
    );
    expect(request?.body).toEqual({
      publications: [],
      measurements: [
        {
          account_id: "measure-account-1",
          provider: "kimi",
          model: "observed-model",
          surface: "consumer_web",
          search_mode: "web_search",
          protocol_version: "protocol-v1",
          question_set_version: "evaluation-v2",
          question: "这是什么产品？",
          market: "CN",
          language: "zh-CN",
          scheduled_at: new Date("2026-10-06T12:30").toISOString(),
          sample_ordinal: 3,
        },
      ],
      bound_measurements: [],
    });
    expect(screen.queryByText(/已观察/)).not.toBeInTheDocument();
  });

  it("rejects empty or malformed measurement inputs, publishing accounts and unavailable Kimi accounts", async () => {
    const requests = mockApi({
      sources: [],
      accounts: [account, { ...measurementAccount, status: "needs_login" }],
    });
    const user = userEvent.setup();
    renderPage();
    expect(
      await screen.findByText(/没有已连接且就绪的项目 Kimi 测量账号/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("项目 Kimi 测量账号")).toBeDisabled();
    expect(
      within(screen.getByLabelText("项目 Kimi 测量账号")).queryByRole(
        "option",
        { name: /项目账号/ },
      ),
    ).not.toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("测量问题模式"), "legacy");
    await user.type(screen.getByLabelText("临时问题（未分类）"), "问题");
    expect(screen.getByRole("button", { name: "加入测量目标" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "封存本轮计划" })).toBeDisabled();
    expect(requests.filter((item) => item.method === "POST")).toHaveLength(0);
  });

  it("keeps a ready measurement disabled for malformed date or sample ordinal", async () => {
    const requests = mockApi({ sources: [], accounts: [measurementAccount] });
    const user = userEvent.setup();
    renderPage();
    await user.selectOptions(
      await screen.findByLabelText("测量问题模式"),
      "legacy",
    );
    await user.selectOptions(
      await screen.findByLabelText("项目 Kimi 测量账号"),
      "measure-account-1",
    );
    await user.type(screen.getByLabelText("可见模型标识"), "observed-model");
    await user.type(screen.getByLabelText("采样协议版本"), "v1");
    await user.type(screen.getByLabelText("临时问题集标签"), "q1");
    await user.type(screen.getByLabelText("市场"), "CN");
    await user.type(screen.getByLabelText("语言"), "zh-CN");
    await user.type(screen.getByLabelText("临时问题（未分类）"), "问题");
    await user.clear(screen.getByLabelText("样本序号（0–10000）"));
    await user.type(screen.getByLabelText("样本序号（0–10000）"), "10001");
    expect(screen.getByRole("button", { name: "加入测量目标" })).toBeDisabled();
    await user.clear(screen.getByLabelText("样本序号（0–10000）"));
    await user.type(screen.getByLabelText("样本序号（0–10000）"), "0");
    expect(screen.getByRole("button", { name: "加入测量目标" })).toBeDisabled();
    expect(requests.filter((item) => item.method === "POST")).toHaveLength(0);
  });

  it("does not offer measurement accounts when the platform lacks measurement login support", async () => {
    mockApi({
      sources: [],
      accounts: [measurementAccount],
      measurementSupported: false,
    });
    renderPage();
    expect(
      await screen.findByText(/没有已连接且就绪的项目 Kimi 测量账号/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("项目 Kimi 测量账号")).toBeDisabled();
  });

  it("keeps the plan editable after a 409 conflict and offers a refresh", async () => {
    const requests = mockApi({
      sources: [],
      accounts: [measurementAccount],
      planConflict: true,
    });
    const user = userEvent.setup();
    renderPage();
    await user.selectOptions(
      await screen.findByLabelText("测量问题模式"),
      "legacy",
    );
    await user.selectOptions(
      await screen.findByLabelText("项目 Kimi 测量账号"),
      "measure-account-1",
    );
    await user.type(screen.getByLabelText("可见模型标识"), "observed-model");
    await user.type(screen.getByLabelText("采样协议版本"), "v1");
    await user.type(screen.getByLabelText("临时问题集标签"), "q1");
    await user.type(screen.getByLabelText("市场"), "CN");
    await user.type(screen.getByLabelText("语言"), "zh-CN");
    await user.type(screen.getByLabelText("临时问题（未分类）"), "问题");
    await user.type(
      screen.getByLabelText("计划采样时间（本地时间）"),
      "2026-10-06T12:30",
    );
    await user.click(screen.getByRole("button", { name: "加入测量目标" }));
    await user.click(screen.getByRole("button", { name: "封存本轮计划" }));
    expect(
      await screen.findByText(/本轮可能已由其他操作封存，请刷新计划/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "封存本轮计划" })).toBeEnabled();
    expect(requests.filter((item) => item.method === "POST")).toHaveLength(1);
  });

  it.each(["unsupported", "login_required", "unknown"] as const)(
    "shows %s as non-verified with raw detail and no resend",
    async (outcome) => {
      const requests = mockApi({ plan: frozenPlan, outcome });
      const user = userEvent.setup();
      renderPage();
      await user.click(
        await screen.findByRole("button", { name: "执行此目标" }),
      );
      await screen.findByText(
        /runner did not provide verified external evidence/,
      );
      expect(
        screen.queryByRole("button", { name: "执行此目标" }),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByText("已发布（未公开验证）"),
      ).not.toBeInTheDocument();
      const card = screen.getByText("公开说明").closest(".channel-job-target")!;
      await user.click(within(card as HTMLElement).getByText("原始结果与证据"));
      expect(
        within(card as HTMLElement).getByText(/runner_status/),
      ).toBeInTheDocument();
      expect(
        requests.filter((item) => item.path.endsWith("/execute")),
      ).toHaveLength(1);
      const lookupRequests = requests.filter((item) =>
        item.path.endsWith("/publication-lookup"),
      );
      expect(lookupRequests).toHaveLength(outcome === "unknown" ? 1 : 0);
      expect(lookupRequests.every((item) => item.method === "GET")).toBe(true);
    },
  );

  it("does not offer another execute after an ambiguous request error", async () => {
    const requests = mockApi({ plan: frozenPlan, executeError: true });
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole("button", { name: "执行此目标" }));
    await screen.findByText(/请求可能已被服务端领取/);
    expect(
      screen.queryByRole("button", { name: "执行此目标" }),
    ).not.toBeInTheDocument();
    expect(
      requests.filter((item) => item.path.endsWith("/execute")),
    ).toHaveLength(1);
  });

  it("does not offer write actions to a viewer", async () => {
    mockApi({ plan: frozenPlan, role: "viewer" });
    renderPage();
    await screen.findByText("公开说明");
    expect(
      screen.queryByRole("button", { name: "执行此目标" }),
    ).not.toBeInTheDocument();
  });
});
