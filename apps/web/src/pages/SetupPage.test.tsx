import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import type { AuthSession } from "../auth/types";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";

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

const project = {
  id: "project-a",
  slug: "northstar",
  display_name: "Northstar AI",
  status: "active" as const,
  revision: 2,
  settings: {
    brand_name: "Northstar AI",
    product_name: "Northstar Assistant",
    market: "中国大陆",
    language: "简体中文",
    competitors: [],
    resource_mode: "mixed" as const,
    monthly_budget_minor: 6000000,
    budget_currency: "CNY",
    monitoring_reserve_percent: 20,
    target_audience: "企业知识管理团队",
    initial_sources: [
      {
        kind: "url" as const,
        value: "https://example.com",
        visibility: "public" as const,
      },
    ],
  },
  created_at: "2026-09-18T00:00:00Z",
  updated_at: "2026-09-18T00:00:00Z",
};

const estimate = {
  currency: "CNY",
  requested_monthly_budget_minor: 6000000,
  monitoring_reserve_minor: 1200000,
  coverage: {
    source_count: 1,
    document_count: 1,
    document_platform_target_count: 3,
    measurement_sample_count: 6,
  },
  phase_one: { minimum_minor: 1700, maximum_minor: 3400 },
  phase_two: { minimum_minor: 750, maximum_minor: 1500 },
  total: { minimum_minor: 2450, maximum_minor: 4900 },
  basis: ["配置决定了首轮覆盖。"],
  assumptions: ["费用会因实际执行变化。"],
};

const operation = {
  id: "operation-a",
  kind: "project.start",
  status: "queued" as const,
  created_at: "2026-09-18T00:00:00Z",
  updated_at: "2026-09-18T00:00:00Z",
};

const overview = {
  project,
  cycle: { status: "running" as const },
  knowledge: { source_count: 0, fact_count: 0, status: "empty" as const },
  benchmark: {
    question_count: 0,
    planned_samples: 6,
    effective_samples: null,
    status: "not_started" as const,
  },
  content: { published_count: 0, verified_count: 0, blocked_count: 0 },
  cost: { currency: "CNY", reserved_minor: 1200000, settled_minor: 0 },
  next_action: null,
  updated_at: "2026-09-18T00:00:00Z",
};

function response(body: unknown, status = 200) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function renderSetup() {
  return render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider
        client={
          new QueryClient({
            defaultOptions: { queries: { retry: false } },
          })
        }
      >
        <AuthProvider>
          <MemoryRouter initialEntries={["/setup?tenant_id=tenant-a"]}>
            <AppRoutes />
          </MemoryRouter>
        </AuthProvider>
      </QueryClientProvider>
    </FluentProvider>,
  );
}

function pathFor(request: RequestInfo | URL) {
  return new URL(String(request), "http://localhost").pathname;
}

async function completeValidConfiguration(
  user: ReturnType<typeof userEvent.setup>,
) {
  const brandName = await screen.findByRole("textbox", { name: "品牌名称" });
  await user.type(brandName, "Northstar AI");
  await user.type(
    screen.getByRole("textbox", { name: "知识来源" }),
    "https://example.com",
  );
  await user.click(screen.getByRole("button", { name: "下一步" }));
  await user.type(
    screen.getByRole("textbox", { name: "目标产品" }),
    "Northstar Assistant",
  );
  await user.type(
    screen.getByRole("textbox", { name: "目标用户" }),
    "企业知识管理团队",
  );
  await user.click(screen.getByRole("button", { name: "下一步" }));
  await user.type(screen.getByRole("textbox", { name: "月度预算" }), "60000");
  await user.click(screen.getByRole("button", { name: "下一步" }));
  await screen.findByRole("heading", { name: "资源与预算估算" });
}

function requestHandler(startResponses: Response[]) {
  return vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
    const path = pathFor(request);
    if (path.endsWith("/auth/session"))
      return Promise.resolve(response(session));
    if (path.endsWith("/projects/estimate"))
      return Promise.resolve(response(estimate));
    if (path.endsWith("/projects/project-a/start")) {
      return Promise.resolve(
        startResponses.shift() ?? response(operation, 202),
      );
    }
    if (path.endsWith("/projects/project-a/overview")) {
      return Promise.resolve(response(overview));
    }
    if (path.endsWith("/projects") && init?.method === "POST") {
      return Promise.resolve(response(project, 201));
    }
    return Promise.resolve(response({ items: [], next_cursor: null }));
  });
}

function commandCalls(fetchMock: ReturnType<typeof vi.fn>) {
  return fetchMock.mock.calls.filter(([, init]) => init?.method === "POST");
}

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("project setup workflow", () => {
  it("gets a real estimate, then creates a draft and starts its Operation", async () => {
    const fetchMock = requestHandler([response(operation, 202)]);
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await completeValidConfiguration(user);
    expect(screen.getByText("预计总资源成本区间")).toBeInTheDocument();
    expect(screen.getByText(/不是曝光、引用、转化、收入/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "创建草稿并启动" }));

    expect(
      await screen.findByText(/启动 Operation 已受理/),
    ).toBeInTheDocument();

    const commands = commandCalls(fetchMock);
    expect(commands.map(([request]) => pathFor(request))).toEqual([
      "/api/v1/projects/estimate",
      "/api/v1/projects",
      "/api/v1/projects/project-a/start",
    ]);
    const [, estimateInit] = commands[0] as [RequestInfo | URL, RequestInit];
    const [, createInit] = commands[1] as [RequestInfo | URL, RequestInit];
    const [, startInit] = commands[2] as [RequestInfo | URL, RequestInit];
    const estimateHeaders = new Headers(estimateInit.headers);
    const createHeaders = new Headers(createInit.headers);
    const startHeaders = new Headers(startInit.headers);
    expect(estimateHeaders.get("X-CSRF-Token")).toBe("csrf-a");
    expect(createHeaders.get("Idempotency-Key")).toBeTruthy();
    expect(startHeaders.get("Idempotency-Key")).toBeTruthy();
    expect(createHeaders.get("Idempotency-Key")).not.toBe(
      startHeaders.get("Idempotency-Key"),
    );
    for (const headers of [estimateHeaders, createHeaders, startHeaders]) {
      expect(headers.get("x-operator-id")).toBeNull();
      expect(headers.get("x-tenant-id")).toBeNull();
      expect(headers.get("x-project-id")).toBeNull();
    }
    for (const [request] of commands) {
      expect(String(request)).toContain("tenant_id=tenant-a");
    }
  });

  it("retries only start after a created draft could not be started", async () => {
    const fetchMock = requestHandler([
      response({ message: "operation queue unavailable" }, 503),
      response(operation, 202),
    ]);
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await completeValidConfiguration(user);
    await user.click(screen.getByRole("button", { name: "创建草稿并启动" }));
    expect(await screen.findByText(/草稿已创建/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试启动" })).toBeEnabled();

    const firstStart = commandCalls(fetchMock).at(-1);
    const firstStartHeaders = new Headers(
      (firstStart?.[1] as RequestInit | undefined)?.headers,
    );
    await user.click(screen.getByRole("button", { name: "重试启动" }));
    expect(
      await screen.findByText(/启动 Operation 已受理/),
    ).toBeInTheDocument();

    const commands = commandCalls(fetchMock);
    expect(
      commands.filter(([request]) => pathFor(request) === "/api/v1/projects"),
    ).toHaveLength(1);
    const startCalls = commands.filter(
      ([request]) => pathFor(request) === "/api/v1/projects/project-a/start",
    );
    expect(startCalls).toHaveLength(2);
    const retryStartHeaders = new Headers(
      (startCalls[1]?.[1] as RequestInit | undefined)?.headers,
    );
    expect(retryStartHeaders.get("Idempotency-Key")).toBe(
      firstStartHeaders.get("Idempotency-Key"),
    );
    await waitFor(() => expect(commands).toHaveLength(4));
  });
});
