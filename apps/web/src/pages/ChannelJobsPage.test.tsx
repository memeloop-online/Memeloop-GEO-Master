import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type { ChannelPlan, ChannelTargetView } from "../api/channelJobs";
import { ChannelJobsPage } from "./ChannelJobsPage";

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
}: {
  accounts?: unknown[];
  sources?: unknown[];
  plan?: ChannelPlan;
  outcome?: "unknown" | "unsupported" | "login_required";
  executeError?: boolean;
  role?: string;
  currentCycleId?: string | null;
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
      if (path.endsWith("/projects/project-1/cycles/current"))
        return Promise.resolve(
          currentCycleId
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
    expect(await screen.findByText("项目尚未启动")).toBeInTheDocument();
    expect(requests.some((item) => item.path.includes("/channel-plan"))).toBe(
      false,
    );
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
