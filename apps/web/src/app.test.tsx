import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "./app";
import { AuthProvider } from "./auth/AuthProvider";
import { useAuth } from "./auth/AuthProvider";
import type { AuthSession } from "./auth/types";
import { apiFetch, setCsrfToken, setUnauthorizedHandler } from "./api/client";
import { projectQueryKeys } from "./api/projects";

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

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("authentication and workspace guards", () => {
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
