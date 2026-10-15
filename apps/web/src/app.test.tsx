import { afterEach, describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, useNavigate } from "react-router-dom";
import { AppRoutes } from "./app";
import { AuthProvider } from "./auth/AuthProvider";
import { useAuth } from "./auth/AuthProvider";
import type { AuthSession } from "./auth/types";
import { apiFetch, setCsrfToken, setUnauthorizedHandler } from "./api/client";
import { projectQueryKeys } from "./api/projects";
const reportsModule = vi.hoisted(() => {
  let resolve!: () => void;
  const ready = new Promise<void>((done) => {
    resolve = done;
  });
  return { requested: vi.fn(), ready, resolve };
});
vi.mock("./pages/ReportsPage", async () => {
  reportsModule.requested();
  await reportsModule.ready;
  return { ReportsPage: () => <h1>报告详情</h1> };
});
function ReportsNavigation() {
  const navigate = useNavigate();
  return (
    <button onClick={() => navigate("/app/tenant-a/project-a/reports")}>
      打开报告详情
    </button>
  );
}

const session: AuthSession = {
  user: {
    id: "user-a",
    login_name: "demo@localhost",
    display_name: "Local Demo",
  },
  operator: { id: "operator-a", slug: "memeloop", display_name: "模因循环" },
  memberships: [
    {
      tenant_id: "tenant-a",
      tenant_slug: "northstar",
      tenant_display_name: "Northstar",
      role: "tenant_admin",
    },
  ],
  expires_at: "2026-09-19T08:00:00Z",
  csrf_token: "csrf-a",
};

function response(body: unknown, status = 200) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function renderApp(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return {
    client,
    ...render(
      <FluentProvider theme={webLightTheme}>
        <QueryClientProvider client={client}>
          <AuthProvider>
            <MemoryRouter initialEntries={[path]}>
              <AppRoutes />
            </MemoryRouter>
          </AuthProvider>
        </QueryClientProvider>
      </FluentProvider>,
    ),
  };
}

function LogoutControl() {
  const { logout } = useAuth();
  return <button onClick={() => void logout()}>测试退出</button>;
}

function SessionStatus() {
  const { status, session: currentSession } = useAuth();
  return <output>{`${status}:${currentSession?.user.id ?? ""}`}</output>;
}

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("authentication and workspace guards", () => {
  it("opens anonymous report deep links without downloading the protected page", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(response({ message: "anonymous" }, 401)),
    );
    renderApp("/app/tenant-a/project-a/reports");
    expect(
      await screen.findByRole("heading", { name: "登录工作区" }),
    ).toBeInTheDocument();
    expect(reportsModule.requested).not.toHaveBeenCalled();
  });
  it("restores a persistent session with no server expiry after remount", async () => {
    const fetchMock = vi
      .fn()
      .mockImplementation(() =>
        Promise.resolve(response({ ...session, expires_at: null })),
      );
    vi.stubGlobal("fetch", fetchMock);
    const mount = () =>
      render(
        <QueryClientProvider client={new QueryClient()}>
          <AuthProvider>
            <SessionStatus />
          </AuthProvider>
        </QueryClientProvider>,
      );
    const first = mount();
    expect(await screen.findByText("authenticated:user-a")).toBeInTheDocument();
    first.unmount();
    mount();
    expect(await screen.findByText("authenticated:user-a")).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(
      screen.queryByRole("heading", { name: "登录工作区" }),
    ).not.toBeInTheDocument();
  });

  it("redirects an anonymous deep link to login without starting a project request", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(response({ message: "anonymous" }, 401));
    vi.stubGlobal("fetch", fetchMock);

    renderApp("/app/tenant-a/project-a/knowledge");

    expect(
      await screen.findByRole("heading", { name: "登录工作区" }),
    ).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(String(fetchMock.mock.calls[0][0])).toContain(
      "/api/v1/auth/session",
    );
  });

  it("keeps a service failure retryable instead of treating it as logout", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response({ message: "maintenance" }, 503))
      .mockResolvedValueOnce(response({ message: "anonymous" }, 401));
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();

    renderApp("/workspaces");

    expect(await screen.findByText("身份服务暂时不可用")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(
      await screen.findByRole("heading", { name: "登录工作区" }),
    ).toBeInTheDocument();
  });

  it("shows permission denied for an unauthorized tenant before any business request", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(session));
    vi.stubGlobal("fetch", fetchMock);

    renderApp("/app/tenant-b/project-b/overview");

    expect(
      await screen.findByRole("heading", { name: "权限不足" }),
    ).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("shows ready knowledge as waiting for coverage planning instead of waiting for processing", async () => {
    const readyOverview = {
      project: {
        id: "project-a",
        slug: "northstar-ai",
        display_name: "Northstar AI",
        status: "active",
        revision: 3,
        settings: {
          brand_name: "Northstar AI",
          product_name: "Northstar Pro",
          market: "中国大陆",
          language: "简体中文",
          target_audience: null,
          objective: "提升产品在购买决策问题中的可见度",
          competitors: [],
          initial_sources: [],
          resource_mode: "mixed",
          monthly_budget_minor: 6000000,
          budget_currency: "CNY",
          monitoring_reserve_percent: 20,
          report_timezone: "Asia/Shanghai",
          report_schedule: {
            report_weekday: "monday",
            report_local_time: "09:00",
            cutoff_weekday: "sunday",
            cutoff_local_time: "23:59",
            period_policy: "previous_calendar_week",
          },
          document_scope: {
            all_active_products: true,
            excluded_product_ids: [],
            markets: ["中国大陆"],
            languages: ["简体中文"],
            content_types: ["product_page"],
            question_clusters: [],
          },
          distribution_scope: {
            mode: "all_eligible",
            included_platform_ids: [],
            excluded_platform_ids: [],
            resource_pool_ids: [],
            replication_policy: "one_account_per_platform",
          },
        },
        created_at: "2026-09-18T00:00:00Z",
        updated_at: "2026-09-19T00:00:00Z",
      },
      cycle: { status: "not_started", awaiting_knowledge: false },
      knowledge: { source_count: 1, fact_count: 3, status: "ready" },
      benchmark: {
        question_count: 0,
        planned_samples: 0,
        effective_samples: null,
        status: "not_started",
      },
      content: { published_count: 0, verified_count: 0, blocked_count: 0 },
      cost: { currency: "CNY", reserved_minor: 0, settled_minor: 0 },
      next_action: {
        code: "import_knowledge",
        label: "导入资料",
        href: "knowledge",
      },
      updated_at: "2026-09-19T00:00:00Z",
    };
    const acceptance = {
      operation_id: "operation-a",
      cycle_id: "cycle-a",
      config_revision_id: "config-a",
      document_manifest: {
        manifest_id: "documents-a",
        revision: 1,
        state: "awaiting_knowledge",
        sealed: false,
        expected_count: null,
      },
      distribution_manifest: {
        manifest_id: "distribution-a",
        revision: 1,
        state: "awaiting_documents",
        sealed: false,
        expected_count: null,
      },
      status: "accepted",
      operation_url: "/operations/operation-a",
    };
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(request), "http://localhost");
        if (url.pathname.endsWith("/auth/session")) {
          return Promise.resolve(response(session));
        }
        if (url.pathname.endsWith("/projects/project-a/overview")) {
          return Promise.resolve(response(readyOverview));
        }
        if (
          url.pathname.endsWith("/projects/project-a/start") &&
          (init?.method ?? "GET") === "GET"
        ) {
          return Promise.resolve(response(acceptance));
        }
        if (url.pathname.endsWith("/projects")) {
          return Promise.resolve(response({ items: [], next_cursor: null }));
        }
        return Promise.resolve(response({}));
      },
    );
    vi.stubGlobal("fetch", fetchMock);

    renderApp("/app/tenant-a/project-a/overview");

    expect(
      await screen.findAllByText("资料已就绪，等待规划内容"),
    ).not.toHaveLength(0);
    expect(screen.queryByText("等待知识处理")).not.toBeInTheDocument();
    expect(
      screen.getByText("项目启动请求已受理。资料已就绪，内容计划待生成。"),
    ).toBeInTheDocument();
    expect(screen.getByText("查看企业知识")).toBeInTheDocument();
    expect(screen.queryByText("导入资料")).not.toBeInTheDocument();
  });

  it("logs out through the user menu and clears the protected route", async () => {
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const url = String(request);
        if (url.endsWith("/auth/session") && init?.method === "DELETE") {
          return Promise.resolve(response(undefined, 204));
        }
        if (url.endsWith("/auth/session"))
          return Promise.resolve(response(session));
        if (url.includes("/projects"))
          return Promise.resolve(response({ items: [], next_cursor: null }));
        return Promise.resolve(response({}));
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    render(
      <FluentProvider theme={webLightTheme}>
        <QueryClientProvider client={new QueryClient()}>
          <AuthProvider>
            <LogoutControl />
            <MemoryRouter
              initialEntries={["/app/tenant-a/project-a/knowledge"]}
            >
              <AppRoutes />
            </MemoryRouter>
          </AuthProvider>
        </QueryClientProvider>
      </FluentProvider>,
    );
    await screen.findByRole("heading", { name: "企业知识库" });
    fireEvent.click(screen.getByRole("button", { name: "测试退出" }));

    expect(
      await screen.findByRole("heading", { name: "登录工作区" }),
    ).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(
        ([request, init]) =>
          String(request).endsWith("/auth/session") &&
          init?.method === "DELETE",
      ),
    ).toBe(true);
  });
});

describe("lazy project navigation", () => {
  it("keeps the shell and its state visible while a child page downloads", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn((request: RequestInfo | URL) =>
        Promise.resolve(
          response(
            String(request).endsWith("/auth/session")
              ? session
              : { items: [], next_cursor: null },
          ),
        ),
      ),
    );
    const user = userEvent.setup();
    render(
      <FluentProvider theme={webLightTheme}>
        <QueryClientProvider
          client={
            new QueryClient({ defaultOptions: { queries: { retry: false } } })
          }
        >
          <AuthProvider>
            <MemoryRouter
              initialEntries={["/app/tenant-a/project-a/knowledge"]}
            >
              <ReportsNavigation />
              <AppRoutes />
            </MemoryRouter>
          </AuthProvider>
        </QueryClientProvider>
      </FluentProvider>,
    );
    await screen.findByRole("heading", { name: "企业知识库" });
    const sidebar = screen.getByRole("complementary", { name: "主导航" });
    await user.click(screen.getByRole("button", { name: "折叠导航栏" }));
    await user.click(screen.getByRole("button", { name: "打开报告详情" }));
    await waitFor(() => expect(reportsModule.requested).toHaveBeenCalledOnce());
    expect(screen.getByRole("complementary", { name: "主导航" })).toBe(sidebar);
    expect(sidebar).toBeVisible();
    expect(screen.getByRole("button", { name: "展开导航栏" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "企业知识库" })).toBeVisible();
    await act(async () => reportsModule.resolve());
    expect(
      await screen.findByRole("heading", { name: "报告详情" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "主导航" })).toBe(sidebar);
    expect(screen.getByRole("button", { name: "展开导航栏" })).toBeVisible();
  });
});

describe("API session boundary", () => {
  it("uses same-origin cookies, a tenant selector and the in-memory CSRF token", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response({ items: [] }));
    vi.stubGlobal("fetch", fetchMock);
    setCsrfToken("csrf-token");

    await apiFetch("/projects?limit=50", {
      method: "POST",
      tenantId: "tenant-a",
      body: { display_name: "Project" },
      idempotencyKey: "stable-create-key",
    });

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = new Headers(init.headers);
    expect(url).toContain("/api/v1/projects?limit=50&tenant_id=tenant-a");
    expect(init.credentials).toBe("same-origin");
    expect(headers.get("X-CSRF-Token")).toBe("csrf-token");
    expect(headers.get("Idempotency-Key")).toBe("stable-create-key");
    expect(headers.get("x-operator-id")).toBeNull();
    expect(headers.get("x-tenant-id")).toBeNull();
    expect(headers.get("x-project-id")).toBeNull();
  });

  it("does not reuse a query cache entry across tenants", async () => {
    const client = new QueryClient();
    const a = {
      userId: "user-a",
      operatorId: "operator-a",
      tenantId: "tenant-a",
    };
    const b = { ...a, tenantId: "tenant-b" };
    client.setQueryData(projectQueryKeys.list(a), { items: ["only-a"] });

    expect(client.getQueryData(projectQueryKeys.list(a))).toEqual({
      items: ["only-a"],
    });
    expect(client.getQueryData(projectQueryKeys.list(b))).toBeUndefined();
    expect(projectQueryKeys.list(a)).toContain("user-a");
    expect(projectQueryKeys.list(a)).toContain("operator-a");
    expect(projectQueryKeys.list(a)).toContain("tenant-a");
    await waitFor(() =>
      expect(client.getQueryCache().getAll()).toHaveLength(1),
    );
  });
});
