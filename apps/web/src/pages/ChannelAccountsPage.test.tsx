import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { AuthProvider, useAuth } from "../auth/AuthProvider";
import { AppearanceProvider } from "../appearance/AppearanceProvider";
import { OperatorAppearancePage } from "./OperatorAppearancePage";
import i18n from "../i18n";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import {
  assignPoolAccount,
  createChannelAccount,
  createChannelGroup,
  getChannelLoginStatus,
  authorizeChannelDesktop,
  listOperatorConnectorCapabilities,
  listProjectConnectorCapabilities,
  listChannelAccounts,
  listChannelGroups,
  listPoolAccounts,
  unassignPoolAccount,
  updateChannelAccount,
  updateOperatorConnectorCapability,
  type OperatorConnectorCapability,
} from "../api/channels";
import {
  ChannelAccountsPage,
  OperatorAccountsPage,
} from "./ChannelAccountsPage";

const desktopConnections = vi.hoisted(
  () =>
    [] as Array<{
      url: string;
      protocols: string[];
      disconnect: ReturnType<typeof vi.fn>;
      emit: (event: string) => void;
    }>,
);
vi.mock("@novnc/novnc", () => ({
  default: class {
    scaleViewport = false;
    resizeSession = false;
    private handlers = new Map<string, (event: Event) => void>();
    disconnect = vi.fn();
    focus = vi.fn();
    clipboardPasteFrom = vi.fn();
    constructor(
      _target: HTMLElement,
      url: string,
      options: { wsProtocols: string[] },
    ) {
      desktopConnections.push({
        url,
        protocols: options.wsProtocols,
        disconnect: this.disconnect,
        emit: (event: string) => this.handlers.get(event)?.(new Event(event)),
      });
    }
    addEventListener(event: string, handler: (event: Event) => void) {
      this.handlers.set(event, handler);
    }
  },
}));

const session = {
  user: { id: "user-1", login_name: "user@example.test", display_name: "User" },
  operator: { id: "operator-1", slug: "operator", display_name: "Operator" },
  memberships: [
    {
      tenant_id: "tenant-1",
      tenant_slug: "tenant",
      tenant_display_name: "Tenant",
      role: "tenant_admin",
    },
  ],
  expires_at: "2026-10-01T00:00:00Z",
  csrf_token: "csrf-test",
};
const account = {
  account_id: "account-1",
  project_id: "project-1",
  platform: "zhihu",
  group_id: null,
  status: "ready",
  display_name: "已识别账号",
  platform_account_id: "external-1",
  avatar_url: null,
  enabled: true,
  proxy_configured: false,
  proxy_server: null,
  created_at: "2026-10-01T00:00:00Z",
  updated_at: "2026-10-01T00:00:00Z",
};
const platforms = [
  {
    id: "zhihu",
    label: "知乎",
    purpose: "publishing",
    login_supported: true,
    login_entry_available: true,
    measurement_supported: true,
  },
  {
    id: "baidu_creator",
    label: "百家号",
    purpose: "publishing",
    login_supported: true,
    login_entry_available: true,
    measurement_supported: true,
  },
  {
    id: "xiaohongshu",
    label: "小红书",
    purpose: "publishing",
    login_supported: true,
    login_entry_available: true,
    measurement_supported: true,
  },
  {
    id: "kimi",
    label: "Kimi",
    purpose: "measurement",
    login_supported: true,
    login_entry_available: true,
    measurement_supported: true,
  },
];
const snapshot = {
  phase: "login_required",
  identity: null,
};
const unverifiedCapability: OperatorConnectorCapability = {
  platform_id: "zhihu",
  placement_slot: "primary",
  revision: 0,
  enabled: false,
  content_types: [],
  availability: "unavailable",
  deployed_version: null,
  verified_content_types: [],
};
const verifiedCapability = {
  ...unverifiedCapability,
  revision: 3,
  verified_content_types: ["article", "short_post"],
  deployed_version: "live.v1",
};
function response(body: unknown, status = 200) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function mockApi(
  initialAccounts: unknown[] = [],
  operator = false,
  connector = unverifiedCapability,
  operatorRole = "resource_admin",
  catalog = platforms,
) {
  let identityReady = false;
  const requests: { path: string; method: string; body?: unknown; url: URL }[] =
    [];
  const fetchMock = vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(request), "http://localhost");
    const path = url.pathname;
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    requests.push({ path, method, body, url });
    if (
      path.endsWith("/public/appearance") ||
      path.endsWith("/operator/appearance")
    )
      return Promise.resolve(
        response({
          display_name: session.operator.display_name,
          logo_url: null,
          primary_color: "#2563EB",
          default_locale: "zh-CN",
          revision: 1,
        }),
      );
    if (path.endsWith("/auth/session"))
      return Promise.resolve(
        response(
          operator
            ? {
                ...session,
                memberships: [
                  { ...session.memberships[0], role: operatorRole },
                ],
              }
            : session,
        ),
      );
    if (path.endsWith("/operator/connector-capabilities") && method === "GET")
      return Promise.resolve(response({ items: [connector] }));
    if (path.endsWith("/projects/project-1/connector-capabilities"))
      return Promise.resolve(response({ items: [connector] }));
    if (
      path.endsWith("/operator/connector-capabilities/zhihu/primary") &&
      method === "PATCH"
    )
      return Promise.resolve(
        response({ ...connector, ...body, revision: connector.revision + 1 }),
      );
    if (path.endsWith("/operator/channel-groups") && method === "GET")
      return Promise.resolve(
        response({
          items: [{ group_id: "pool-group-1", name: "共享组", created_at: "" }],
        }),
      );
    if (path.endsWith("/operator/channel-accounts") && method === "GET")
      return Promise.resolve(response({ items: initialAccounts }));
    if (
      path.endsWith("/operator/channel-accounts/account-1/assignments") &&
      method === "GET"
    )
      return Promise.resolve(response({ items: [] }));
    if (
      path.endsWith("/operator/channel-accounts/account-1/assignments") &&
      (method === "POST" || method === "DELETE")
    )
      return Promise.resolve(response(undefined, 204));
    if (path.endsWith("/channel-platforms"))
      return Promise.resolve(response({ items: catalog }));
    if (path.endsWith("/channel-accounts") && method === "GET")
      return Promise.resolve(response({ items: initialAccounts }));
    if (path.endsWith("/channel-groups") && method === "GET")
      return Promise.resolve(response({ items: [] }));
    if (path.endsWith("/channel-groups") && method === "POST")
      return Promise.resolve(
        response({
          group_id: "group-1",
          project_id: "project-1",
          name: body.name,
          created_at: "",
        }),
      );
    if (path.endsWith("/channel-accounts") && method === "POST")
      return Promise.resolve(response({ ...account, status: "needs_login" }));
    if (path.endsWith("/channel-accounts/account-1") && method === "PATCH")
      return Promise.resolve(response({ ...account, ...body }));
    if (path.endsWith("/channel-login-sessions") && method === "POST")
      return Promise.resolve(
        response({
          session_id: "login-1",
          account_id: "account-1",
          phase: "login_required",
        }),
      );
    if (
      path.endsWith("/channel-login-sessions/login-1/status") ||
      path.endsWith("/operator/channel-login-sessions/login-1/status")
    )
      return Promise.resolve(
        response(
          identityReady
            ? {
                phase: "ready_to_complete",
                identity: {
                  display_name: "已识别账号",
                  platform_account_id: "external-1",
                },
              }
            : snapshot,
        ),
      );
    if (
      path.endsWith("/channel-login-sessions/login-1/desktop-authorization") ||
      path.endsWith(
        "/operator/channel-login-sessions/login-1/desktop-authorization",
      )
    )
      return Promise.resolve(
        response({
          websocket_path: `${path.replace("/desktop-authorization", "/desktop")}?tenant_id=tenant-1&project_id=project-1`,
          protocol: "geo-desktop.test-grant",
        }),
      );
    if (path.endsWith("/channel-login-sessions/login-1/complete"))
      return Promise.resolve(response({ account }));
    if (path.endsWith("/channel-login-sessions/login-1") && method === "DELETE")
      return Promise.resolve(response(undefined, 204));
    return Promise.resolve(
      response({ code: "not_found", message: "not found" }, 404),
    );
  });
  vi.stubGlobal("fetch", fetchMock);
  return {
    requests,
    fetchMock,
    setIdentityReady: () => {
      identityReady = true;
    },
  };
}
function renderPage(
  view: "channels" | "connect" | "settings" = "channels",
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } }),
) {
  return render(
    <QueryClientProvider client={client}>
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <MemoryRouter initialEntries={["/app/tenant-1/project-1/channels"]}>
            <Routes>
              <Route
                path="/app/:tenantId/:projectId/channels"
                element={<ChannelAccountsPage view={view} />}
              />
            </Routes>
          </MemoryRouter>
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>,
  );
}

function AuthStatusProbe() {
  return <span data-testid="auth-status">{useAuth().status}</span>;
}

function renderOperator() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <AuthStatusProbe />
          <MemoryRouter>
            <OperatorAccountsPage />
          </MemoryRouter>
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  void i18n.changeLanguage("zh-CN");
  desktopConnections.length = 0;
  vi.unstubAllGlobals();
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
});

describe("channel API scope", () => {
  it("keeps project capability reads scoped and passes only revisioned settings to operator PATCH", async () => {
    const { requests } = mockApi();
    await listProjectConnectorCapabilities("tenant-1", "project-1");
    await listOperatorConnectorCapabilities();
    await updateOperatorConnectorCapability("zhihu", "primary", {
      expected_revision: 3,
      enabled: true,
      content_types: ["article"],
    });
    const projectRead = requests.find((entry) =>
      entry.path.endsWith("/projects/project-1/connector-capabilities"),
    )!;
    expect(projectRead.url.searchParams.get("tenant_id")).toBe("tenant-1");
    expect(projectRead.url.searchParams.has("project_id")).toBe(false);
    const update = requests.find((entry) =>
      entry.path.endsWith("/operator/connector-capabilities/zhihu/primary"),
    )!;
    expect(update.body).toEqual({
      expected_revision: 3,
      enabled: true,
      content_types: ["article"],
    });
    expect(update.url.searchParams.has("tenant_id")).toBe(false);
  });

  it("scopes lists, group creation, and account updates to the selected project", async () => {
    const { requests } = mockApi();
    await listChannelAccounts("tenant-1", "project-1");
    await listChannelGroups("tenant-1", "project-1");
    await createChannelGroup("tenant-1", "project-1", "团队资源");
    await updateChannelAccount("tenant-1", "project-1", "account-1", {
      group_id: "group-1",
    });
    expect(
      requests.every(
        (item) => item.url.searchParams.get("project_id") === "project-1",
      ),
    ).toBe(true);
    expect(
      requests.find(
        (item) =>
          item.path.endsWith("/channel-groups") && item.method === "POST",
      )?.body,
    ).toEqual({ project_id: "project-1", name: "团队资源" });
    expect(requests.find((item) => item.method === "PATCH")?.body).toEqual({
      group_id: "group-1",
    });
  });

  it("sends proxy secrets only in write body and authorizes a scoped desktop", async () => {
    const { requests } = mockApi();
    await createChannelAccount("tenant-1", "project-1", "zhihu", undefined, {
      server: "socks5://proxy.example.test:1080",
      username: "user",
      password: "placeholder",
    });
    await authorizeChannelDesktop("tenant-1", "project-1", "login-1");
    await getChannelLoginStatus("tenant-1", "project-1", "login-1");
    const write = requests.find(
      (item) =>
        item.path.endsWith("/channel-accounts") && item.method === "POST",
    )!;
    expect(write.body).toMatchObject({
      project_id: "project-1",
      proxy: { password: "placeholder" },
    });
    expect(write.url.toString()).not.toContain("placeholder");
    expect(
      requests.find((item) => item.path.endsWith("/desktop-authorization"))
        ?.body,
    ).toBeUndefined();
    expect(
      requests
        .find((item) => item.path.endsWith("/status"))
        ?.url.searchParams.get("project_id"),
    ).toBe("project-1");
  });

  it("keeps shared-pool assignment commands outside customer project account mutation", async () => {
    const { requests } = mockApi([], true);
    await listPoolAccounts();
    await assignPoolAccount("account-1", {
      tenant_id: "tenant-2",
      project_id: "project-2",
    });
    await unassignPoolAccount("account-1", {
      tenant_id: "tenant-2",
      project_id: "project-2",
    });
    const commands = requests.filter((item) =>
      item.path.endsWith("/operator/channel-accounts/account-1/assignments"),
    );
    expect(commands.map((item) => item.method)).toEqual(["POST", "DELETE"]);
    expect(commands.map((item) => item.body)).toEqual([
      { tenant_id: "tenant-2", project_id: "project-2" },
      { tenant_id: "tenant-2", project_id: "project-2" },
    ]);
  });
});

describe("account page", () => {
  it.each([
    [false, false],
    [false, true],
    [true, false],
    [true, true],
  ])(
    "ignores a late start response after unmount (operator=%s, rejected=%s)",
    async (operator, rejected) => {
      const { fetchMock, requests } = mockApi([], operator);
      const original = fetchMock.getMockImplementation()!;
      let resolveStart!: (value: Response) => void;
      let rejectStart!: (reason: Error) => void;
      const start = new Promise<Response>((resolve, reject) => {
        resolveStart = resolve;
        rejectStart = reject;
      });
      let startCount = 0;
      fetchMock.mockImplementation((request, init) => {
        if (
          new URL(String(request), "http://localhost").pathname.endsWith(
            "/channel-login-sessions",
          ) &&
          init?.method === "POST"
        ) {
          startCount++;
          return start;
        }
        return original(request, init);
      });
      const view = operator ? renderOperator() : renderPage("connect");
      await userEvent.click(
        await screen.findByRole("button", {
          name: operator ? "创建并登录总部账号" : "启动远程登录",
        }),
      );
      await waitFor(() => expect(startCount).toBe(1));
      expect(screen.getByText("正在启动远程浏览器")).toBeVisible();
      view.unmount();
      await act(async () => {
        if (rejected) rejectStart(new Error("Sign-in temporarily unavailable"));
        else
          resolveStart(
            response({
              session_id: "login-1",
              account_id: "account-1",
              phase: "login_required",
            }),
          );
      });
      expect(desktopConnections).toHaveLength(0);
      expect(
        requests.filter((request) =>
          request.path.includes("/channel-login-sessions/login-1"),
        ),
      ).toHaveLength(0);
      expect(screen.queryByLabelText("远程登录")).toBeNull();
    },
  );

  it.each([
    [false, "close"],
    [false, "unmount"],
    [true, "close"],
    [true, "unmount"],
  ] as const)(
    "mounts the desktop after initial status failure and cleans up on %s/%s",
    async (operator, cleanup) => {
      const { fetchMock, requests } = mockApi([], operator);
      const original = fetchMock.getMockImplementation()!;
      let resolveAuthorization!: (value: Response) => void;
      const authorization = new Promise<Response>((resolve) => {
        resolveAuthorization = resolve;
      });
      let authorizationCount = 0;
      fetchMock.mockImplementation((request, init) => {
        const path = new URL(String(request), "http://localhost").pathname;
        if (path.endsWith("/channel-login-sessions/login-1/status"))
          return Promise.resolve(
            response(
              {
                code: "unavailable",
                message: "Status temporarily unavailable",
              },
              503,
            ),
          );
        if (
          path.endsWith("/channel-login-sessions/login-1/desktop-authorization")
        ) {
          authorizationCount++;
          return authorization;
        }
        return original(request, init);
      });
      const view = operator ? renderOperator() : renderPage("connect");
      await userEvent.click(
        await screen.findByRole("button", {
          name: operator ? "创建并登录总部账号" : "启动远程登录",
        }),
      );
      expect(
        await screen.findByText("Status temporarily unavailable"),
      ).toBeVisible();
      await waitFor(() => expect(authorizationCount).toBe(1));
      expect(desktopConnections).toHaveLength(0);
      await act(async () =>
        resolveAuthorization(
          response({
            websocket_path: `/api/v1/${operator ? "operator/" : ""}channel-login-sessions/login-1/desktop`,
            protocol: "geo-desktop.test-grant",
          }),
        ),
      );
      await waitFor(() => expect(desktopConnections).toHaveLength(1));
      const connection = desktopConnections[0];
      act(() => connection.emit("connect"));
      expect(screen.getByText("Status temporarily unavailable")).toBeVisible();
      expect(screen.getByRole("button", { name: "重试" })).toBeEnabled();
      expect(connection.disconnect).not.toHaveBeenCalled();
      if (cleanup === "close") {
        await userEvent.click(
          screen.getByRole("button", { name: "取消并关闭" }),
        );
        await waitFor(() =>
          expect(screen.queryByLabelText("远程登录")).toBeNull(),
        );
      } else view.unmount();
      expect(connection.disconnect).toHaveBeenCalledTimes(1);
      expect(authorizationCount).toBe(1);
      expect(
        requests.filter(
          (request) =>
            request.path.endsWith("/channel-login-sessions/login-1") &&
            request.method === "DELETE",
        ),
      ).toHaveLength(cleanup === "close" ? 1 : 0);
    },
  );

  it.each([false, true])(
    "does not start login after leaving a pending account creation (operator=%s)",
    async (operator) => {
      const { fetchMock, requests } = mockApi([], operator);
      const original = fetchMock.getMockImplementation()!;
      let resolveCreate!: (value: Response) => void;
      const creation = new Promise<Response>((resolve) => {
        resolveCreate = resolve;
      });
      fetchMock.mockImplementation((request, init) => {
        if (
          new URL(String(request), "http://localhost").pathname.endsWith(
            "/channel-accounts",
          ) &&
          init?.method === "POST"
        )
          return creation;
        return original(request, init);
      });
      const view = operator ? renderOperator() : renderPage("connect");
      await userEvent.click(
        await screen.findByRole("button", {
          name: operator ? "创建并登录总部账号" : "启动远程登录",
        }),
      );
      expect(screen.getByText("正在启动远程浏览器")).toBeVisible();
      view.unmount();
      await act(async () => resolveCreate(response(account)));
      expect(
        requests.some(
          (request) =>
            request.path.endsWith("/channel-login-sessions") &&
            request.method === "POST",
        ),
      ).toBe(false);
    },
  );

  it.each([false, true])(
    "connects the desktop while the first identity status is still pending (operator=%s)",
    async (operator) => {
      const { fetchMock } = mockApi([], operator);
      const original = fetchMock.getMockImplementation()!;
      let resolveStatus!: (value: Response) => void;
      const status = new Promise<Response>((resolve) => {
        resolveStatus = resolve;
      });
      fetchMock.mockImplementation((request, init) => {
        if (String(request).includes("/channel-login-sessions/login-1/status"))
          return status;
        return original(request, init);
      });
      operator ? renderOperator() : renderPage("connect");
      await userEvent.click(
        await screen.findByRole("button", {
          name: operator ? "创建并登录总部账号" : "启动远程登录",
        }),
      );
      await waitFor(() => expect(desktopConnections).toHaveLength(1));
      expect(screen.getByText("正在启动远程浏览器")).toBeVisible();
      await act(async () => resolveStatus(response(snapshot)));
    },
  );

  it.each([false, true])(
    "shows sign-in preparation before create/start resolve without waiting for list refresh (operator=%s)",
    async (operator) => {
      await i18n.changeLanguage("en");
      const { fetchMock } = mockApi([], operator);
      const original = fetchMock.getMockImplementation()!;
      let resolveCreate!: (value: Response) => void;
      let resolveStart!: (value: Response) => void;
      let resolveRefresh!: (value: Response) => void;
      const create = new Promise<Response>((resolve) => {
        resolveCreate = resolve;
      });
      const start = new Promise<Response>((resolve) => {
        resolveStart = resolve;
      });
      const refresh = new Promise<Response>((resolve) => {
        resolveRefresh = resolve;
      });
      let creating = false;
      let createCount = 0;
      let startCount = 0;
      let refreshCount = 0;
      fetchMock.mockImplementation((request, init) => {
        const path = new URL(String(request), "http://localhost").pathname;
        if (path.endsWith("/channel-accounts") && init?.method === "POST") {
          creating = true;
          createCount++;
          return create;
        }
        if (
          path.endsWith("/channel-accounts") &&
          creating &&
          (init?.method ?? "GET") === "GET"
        ) {
          refreshCount++;
          return refresh;
        }
        if (
          path.endsWith("/channel-login-sessions") &&
          init?.method === "POST"
        ) {
          startCount++;
          return start;
        }
        return original(request, init);
      });
      operator ? renderOperator() : renderPage("connect");
      const button = await screen.findByRole("button", {
        name: operator ? "创建并登录总部账号" : "Start remote sign-in",
      });
      await userEvent.click(button);
      const pending = screen.getByLabelText("Remote sign-in");
      expect(
        within(pending).getByText("Starting remote browser"),
      ).toBeVisible();
      expect(
        within(pending).getByText(/Your sign-in page will appear here/),
      ).toBeVisible();
      expect(within(pending).queryByRole("button")).toBeNull();
      expect(button).toBeDisabled();
      await userEvent.click(button);
      expect(createCount).toBe(1);
      expect(startCount).toBe(0);
      await act(async () =>
        resolveCreate(response({ ...account, status: "needs_login" })),
      );
      await waitFor(() => expect(startCount).toBe(1));
      expect(refreshCount).toBeGreaterThan(0);
      expect(screen.getByText("Starting remote browser")).toBeVisible();
      expect(desktopConnections).toHaveLength(0);
      await act(async () =>
        resolveStart(
          response({
            session_id: "login-1",
            account_id: "account-1",
            phase: "login_required",
          }),
        ),
      );
      expect(
        await screen.findByRole("button", { name: "Cancel and close" }),
      ).toBeVisible();
      expect(button).toBeDisabled();
      await act(async () => resolveRefresh(response({ items: [account] })));
    },
  );

  it.each([
    [false, "create"],
    [false, "start"],
    [true, "create"],
    [true, "start"],
  ] as const)(
    "clears preparation and retries a failed %s/%s without recreating saved accounts",
    async (operator, failure) => {
      const { fetchMock } = mockApi([], operator);
      const original = fetchMock.getMockImplementation()!;
      let rejectPending!: (reason: Error) => void;
      const pending = new Promise<Response>((_, reject) => {
        rejectPending = reject;
      });
      let createCount = 0;
      let startCount = 0;
      fetchMock.mockImplementation((request, init) => {
        const path = new URL(String(request), "http://localhost").pathname;
        if (path.endsWith("/channel-accounts") && init?.method === "POST") {
          createCount++;
          if (failure === "create" && createCount === 1) return pending;
        }
        if (
          path.endsWith("/channel-login-sessions") &&
          init?.method === "POST"
        ) {
          startCount++;
          if (failure === "start" && startCount === 1) return pending;
        }
        return original(request, init);
      });
      operator ? renderOperator() : renderPage("connect");
      const button = await screen.findByRole("button", {
        name: operator ? "创建并登录总部账号" : "启动远程登录",
      });
      await userEvent.click(button);
      if (failure === "start") await waitFor(() => expect(startCount).toBe(1));
      expect(screen.getByText("正在启动远程浏览器")).toBeVisible();
      await act(async () =>
        rejectPending(new Error("Sign-in temporarily unavailable")),
      );
      expect(
        await screen.findByText("Sign-in temporarily unavailable"),
      ).toBeVisible();
      expect(screen.queryByLabelText("远程登录")).toBeNull();
      expect(button).toBeEnabled();
      await userEvent.click(screen.getByRole("button", { name: "重试" }));
      expect(
        await screen.findByRole("button", { name: "取消并关闭" }),
      ).toBeVisible();
      expect(createCount).toBe(failure === "create" ? 2 : 1);
      expect(startCount).toBe(failure === "start" ? 2 : 1);
    },
  );

  it("does not label a stored ready account connected when verification is unsupported", async () => {
    mockApi(
      [{ ...account, platform: "deepseek", status: "ready" }],
      false,
      unverifiedCapability,
      "resource_admin",
      [
        {
          id: "deepseek",
          label: "DeepSeek",
          purpose: "measurement",
          login_entry_available: true,
          login_supported: false,
          measurement_supported: false,
        },
      ],
    );
    renderPage();
    expect(await screen.findByText("等待身份验证")).toBeVisible();
    expect(
      screen.queryByText("已连接", { exact: true }),
    ).not.toBeInTheDocument();
  });
  it("opens entry-only platforms without claiming a verified connection", async () => {
    const api = mockApi([], false, unverifiedCapability, "resource_admin", [
      {
        id: "deepseek",
        label: "DeepSeek",
        purpose: "measurement",
        login_entry_available: true,
        login_supported: false,
        measurement_supported: false,
      },
      {
        id: "glm",
        label: "GLM",
        purpose: "measurement",
        login_entry_available: false,
        login_supported: false,
        measurement_supported: false,
      },
    ]);
    const user = userEvent.setup();
    renderPage("connect");
    await user.selectOptions(await screen.findByLabelText("平台"), "deepseek");
    expect(screen.getByRole("option", { name: /GLM/ })).toBeDisabled();
    expect(
      screen.getByText("可打开登录页面，但暂不支持核验账号身份或进行测量。"),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: "启动远程登录" }));
    await waitFor(() =>
      expect(
        api.requests.some(
          (request) =>
            request.path.endsWith("/channel-login-sessions") &&
            request.method === "POST",
        ),
      ).toBe(true),
    );
    expect(screen.queryByText("账号已连接。")).not.toBeInTheDocument();
    expect(
      api.requests.some((request) => request.path.endsWith("/complete")),
    ).toBe(false);
  });
  it("shows account connection separately from read-only project publication verification", async () => {
    const { requests } = mockApi([account]);
    renderPage();
    expect(await screen.findByText("已识别账号")).toBeTruthy();
    const section = await screen.findByLabelText("发布可用性");
    expect(within(section).getByText("暂不可用")).toBeTruthy();
    expect(within(section).getByText("暂不支持发布。")).toBeTruthy();
    expect(within(section).getByText(/账号已连接不代表可以发布/)).toBeTruthy();
    expect(within(section).queryByRole("checkbox")).toBeNull();
    expect(
      requests.some((entry) =>
        entry.path.endsWith("/operator/connector-capabilities/zhihu/primary"),
      ),
    ).toBe(false);
  });

  it("shows project capability permission errors without implying publishing is ready", async () => {
    const { fetchMock } = mockApi([account]);
    const original = fetchMock.getMockImplementation()!;
    fetchMock.mockImplementation((request, init) =>
      String(request).includes("/projects/project-1/connector-capabilities")
        ? Promise.resolve(
            response({ code: "forbidden", message: "forbidden" }, 403),
          )
        : original(request, init),
    );
    renderPage();
    const section = await screen.findByLabelText("发布可用性");
    expect(await within(section).findByText("权限不足")).toBeTruthy();
    expect(within(section).queryByText("可用")).toBeNull();
  });

  it("shows a supported format only when publishing is available", async () => {
    mockApi([account], false, {
      ...verifiedCapability,
      availability: "available",
      content_types: ["plain_text_article.v1"],
    });
    renderPage();
    const section = await screen.findByLabelText("发布可用性");
    expect(await within(section).findByText("可用")).toBeInTheDocument();
    expect(
      within(section).getByText("支持格式：纯文本文章（标题与正文）。"),
    ).toBeInTheDocument();
    expect(within(section).queryByText("暂不支持发布。")).toBeNull();
  });

  it("shows real loaded accounts and reconnects without changing identity locally", async () => {
    const { requests } = mockApi([account]);
    renderPage();
    expect(await screen.findByText("已识别账号")).toBeTruthy();
    expect(screen.getByText("已连接")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "重新连接" }));
    await screen.findByRole("group", { name: "远程浏览器画面" });
    expect(
      requests.some(
        (item) =>
          item.path.endsWith("/channel-login-sessions") &&
          item.method === "POST" &&
          (item.body as { account_id: string }).account_id === "account-1",
      ),
    ).toBe(true);
  });

  it("creates a project-scoped group through the service", async () => {
    const { requests } = mockApi();
    renderPage("settings");
    const groupInput = await screen.findByRole("textbox", {
      name: "新建资源组",
    });
    await userEvent.type(groupInput, "内容团队");
    await userEvent.click(screen.getByRole("button", { name: "创建" }));
    await screen.findByText("资源组已创建。");
    expect(
      requests.find(
        (item) =>
          item.path.endsWith("/channel-groups") && item.method === "POST",
      )?.body,
    ).toEqual({ project_id: "project-1", name: "内容团队" });
  });

  it("reconnects an expired account through noVNC, never screenshot actions", async () => {
    const { requests } = mockApi([{ ...account, status: "expired" }]);
    renderPage();
    expect(await screen.findByText("登录已失效")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "登录并验证" }));
    const remote = await screen.findByLabelText("远程登录");
    await waitFor(() => expect(desktopConnections.length).toBeGreaterThan(0));
    expect(desktopConnections.at(-1)?.url).toContain(
      "/channel-login-sessions/login-1/desktop?",
    );
    expect(desktopConnections.at(-1)?.protocols).toEqual([
      "geo-desktop.test-grant",
    ]);
    expect(within(remote).queryByRole("spinbutton")).toBeNull();
    expect(
      requests.some(
        (request) =>
          request.path.endsWith("/actions") ||
          request.path.endsWith("/snapshot"),
      ),
    ).toBe(false);
    act(() => desktopConnections.at(-1)?.emit("disconnect"));
    await userEvent.click(
      await within(remote).findByRole("button", { name: "重新连接画面" }),
    );
    await waitFor(() =>
      expect(
        requests.filter((request) =>
          request.path.endsWith("/desktop-authorization"),
        ),
      ).toHaveLength(2),
    );
  });

  it("rejects an unexpected desktop target without falling back to screenshots", async () => {
    const { fetchMock, requests } = mockApi([
      { ...account, status: "expired" },
    ]);
    const original = fetchMock.getMockImplementation()!;
    fetchMock.mockImplementation((request, init) =>
      String(request).includes("/desktop-authorization")
        ? Promise.resolve(
            response({
              websocket_path: "//untrusted.example.test/desktop",
              protocol: "geo-desktop.test-grant",
            }),
          )
        : original(request, init),
    );
    renderPage();
    await userEvent.click(
      await screen.findByRole("button", { name: "登录并验证" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "远程浏览器连接失败，请重新连接。",
    );
    expect(desktopConnections).toHaveLength(0);
    expect(
      requests.some(
        (item) =>
          item.path.endsWith("/snapshot") || item.path.endsWith("/actions"),
      ),
    ).toBe(false);
  });

  it("shows assigned shared accounts without customer login or configuration controls", async () => {
    mockApi([
      {
        ...account,
        owner_kind: "operator_pool",
        proxy_configured: true,
        proxy_server: null,
      },
    ]);
    renderPage();
    expect(await screen.findByText("总部共享")).toBeTruthy();
    expect(screen.getByText(/由总部管理登录和网络出口/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "重新连接" })).toBeNull();
    expect(screen.queryByRole("button", { name: "配置" })).toBeNull();
  });

  it("saves verified identity from status and disconnects the desktop", async () => {
    const { requests, setIdentityReady } = mockApi();
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    renderPage("connect", client);
    await screen.findByRole("button", { name: "启动远程登录" });
    await userEvent.click(screen.getByRole("button", { name: "启动远程登录" }));
    await waitFor(() => expect(desktopConnections.length).toBeGreaterThan(0));
    const remote = screen.getByLabelText("远程登录");
    expect(within(remote).queryByRole("img")).toBeNull();
    setIdentityReady();
    await client.invalidateQueries({
      queryKey: ["channel-login", "tenant-1", "project-1", "login-1"],
    });
    await waitFor(() =>
      expect(
        requests.some(
          (item) => item.path.endsWith("/complete") && item.method === "POST",
        ),
      ).toBe(true),
    );
    expect(
      await screen.findByText("账号身份已验证，连接已保存。"),
    ).toBeTruthy();
    expect(desktopConnections.at(-1)?.disconnect).toHaveBeenCalled();
  });

  it("closes an already expired login and discards cached account identity", async () => {
    const { fetchMock } = mockApi();
    const defaultFetch = fetchMock.getMockImplementation()!;
    fetchMock.mockImplementation((request, init) => {
      if (
        String(request).includes("/channel-login-sessions/login-1") &&
        init?.method === "DELETE"
      )
        return Promise.resolve(
          response(
            { code: "not_found", message: "login session expired" },
            404,
          ),
        );
      return defaultFetch(request, init);
    });
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    renderPage("connect", client);
    await userEvent.click(
      await screen.findByRole("button", { name: "启动远程登录" }),
    );
    await waitFor(() => expect(desktopConnections.length).toBeGreaterThan(0));
    expect(
      client.getQueryData([
        "channel-login",
        "tenant-1",
        "project-1",
        "login-1",
      ]),
    ).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "取消并关闭" }));
    await waitFor(() => expect(screen.queryByLabelText("远程登录")).toBeNull());
    await waitFor(() =>
      expect(
        client.getQueryData([
          "channel-login",
          "tenant-1",
          "project-1",
          "login-1",
        ]),
      ).toBeUndefined(),
    );
    expect(screen.getByRole("button", { name: "启动远程登录" })).toBeEnabled();
  });

  it("keeps the Chinese customer settings copy free of work-package labels", async () => {
    mockApi([account]);
    renderPage("settings");
    expect(
      await screen.findByRole("heading", { name: "项目设置" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "项目配置" })).toBeInTheDocument();
    expect(screen.queryByText(/P10|P11|P16/)).toBeNull();
    expect(
      screen.queryByText(/分组不改变|编码助手授权|连接凭据保存在服务端/),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "接入账号" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("已接入账号")).toBeInTheDocument();
  });

  it("localizes customer connection, account status, publishing limits and shared remote sign-in", async () => {
    await i18n.changeLanguage("en");
    mockApi([{ ...account, status: "expired" }]);
    renderPage();
    expect(
      await screen.findByRole("heading", { name: "Channel accounts" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Accounts & resources")).toBeInTheDocument();
    expect(await screen.findByText("Sign-in expired")).toBeInTheDocument();
    const publication = await screen.findByLabelText("Publishing availability");
    expect(
      within(publication).getByText("Currently unavailable"),
    ).toBeInTheDocument();
    expect(
      within(publication).getByText(
        /A connected account does not guarantee publishing/,
      ),
    ).toBeInTheDocument();
    expect(
      within(publication).getByText("Publishing is not currently available."),
    ).toBeInTheDocument();
    expect(screen.queryByText(/P10|P11|P16/)).toBeNull();
    await userEvent.click(
      screen.getByRole("button", { name: "Sign in and verify" }),
    );
    const remote = await screen.findByLabelText("Remote sign-in");
    expect(
      within(remote).getByRole("button", { name: "Cancel and close" }),
    ).toBeInTheDocument();
    expect(
      within(remote).getByText(/Sign in on the remote page below/),
    ).toBeInTheDocument();
    expect(
      await within(remote).findByRole("group", { name: "Remote browser" }),
    ).toBeInTheDocument();
  });

  it("localizes English group creation and the settings page without changing API inputs", async () => {
    await i18n.changeLanguage("en");
    const { requests } = mockApi();
    renderPage("settings");
    expect(
      await screen.findByRole("heading", {
        name: "Project settings",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Project configuration" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(
        /Groups do not change|Coding assistant authorization|Credentials are kept on the server/,
      ),
    ).toBeNull();
    await userEvent.type(
      screen.getByRole("textbox", { name: "New resource group" }),
      "Content team",
    );
    await userEvent.click(screen.getByRole("button", { name: "Create" }));
    expect(
      await screen.findByText("Resource group created."),
    ).toBeInTheDocument();
    expect(
      requests.find(
        (entry) =>
          entry.path.endsWith("/channel-groups") && entry.method === "POST",
      )?.body,
    ).toMatchObject({ name: "Content team" });
  });
  it.each([
    ["zh-CN", "资源组", "启动远程登录"],
    ["en", "Resource group", "Start remote sign-in"],
  ])(
    "connects without creating or selecting a group in %s",
    async (language, groupLabel, loginLabel) => {
      await i18n.changeLanguage(language);
      const { requests } = mockApi();
      renderPage("connect");
      expect(
        await screen.findByRole("combobox", { name: groupLabel }),
      ).toHaveValue("");
      await userEvent.click(
        await screen.findByRole("button", { name: loginLabel }),
      );
      await waitFor(() =>
        expect(
          requests.some(
            (entry) =>
              entry.method === "POST" &&
              entry.path.endsWith("/channel-login-sessions"),
          ),
        ).toBe(true),
      );
      const createAccount = requests.find(
        (entry) =>
          entry.method === "POST" && entry.path.endsWith("/channel-accounts"),
      );
      expect(createAccount?.body).toMatchObject({ platform: "zhihu" });
      expect(createAccount?.body).toHaveProperty("group_id", null);
      expect(
        requests.some(
          (entry) =>
            entry.method === "POST" && entry.path.endsWith("/channel-groups"),
        ),
      ).toBe(false);
    },
  );
});

describe("operator appearance access", () => {
  it.each(["operator_admin", "resource_admin", "tenant_admin"])(
    "uses the canonical wire role for appearance access (%s)",
    async (role) => {
      const { requests } = mockApi([], true, unverifiedCapability, role);
      const client = new QueryClient({
        defaultOptions: { queries: { retry: false } },
      });
      render(
        <FluentProvider theme={webLightTheme}>
          <QueryClientProvider client={client}>
            <AppearanceProvider>
              <AuthProvider>
                <AuthStatusProbe />
                <MemoryRouter>
                  <OperatorAppearancePage />
                </MemoryRouter>
              </AuthProvider>
            </AppearanceProvider>
          </QueryClientProvider>
        </FluentProvider>,
      );
      if (role === "operator_admin") {
        expect(
          await screen.findByDisplayValue(session.operator.display_name),
        ).toBeVisible();
        expect(
          screen.getByRole("button", { name: i18n.t("appearance.save") }),
        ).toBeEnabled();
        expect(
          requests.some((entry) => entry.path.endsWith("/operator/appearance")),
        ).toBe(true);
      } else {
        await waitFor(() =>
          expect(screen.getByTestId("auth-status")).toHaveTextContent(
            "authenticated",
          ),
        );
        expect(
          await screen.findByRole("heading", {
            name: i18n.t("appearance.unavailable"),
          }),
        ).toBeVisible();
        expect(
          screen.queryByRole("button", { name: i18n.t("appearance.save") }),
        ).toBeNull();
        expect(
          requests.some((entry) => entry.path.endsWith("/operator/appearance")),
        ).toBe(false);
      }
    },
  );
});

describe("operator pool", () => {
  it("uses the same noVNC desktop for operator login", async () => {
    const { requests } = mockApi([account], true);
    renderOperator();
    await userEvent.click(
      await screen.findByRole("button", { name: "重新登录" }),
    );
    await waitFor(() => expect(desktopConnections.length).toBeGreaterThan(0));
    expect(desktopConnections.at(-1)?.url).toContain(
      "/operator/channel-login-sessions/login-1/desktop?",
    );
    expect(
      requests.some((item) =>
        item.path.endsWith(
          "/operator/channel-login-sessions/login-1/desktop-authorization",
        ),
      ),
    ).toBe(true);
    expect(requests.some((item) => item.path.endsWith("/actions"))).toBe(false);
  });

  it("offers appearance settings to OEM admins", async () => {
    const { requests } = mockApi(
      [],
      true,
      unverifiedCapability,
      "operator_admin",
    );
    renderOperator();
    expect(
      await screen.findByRole("link", { name: "工作区外观" }),
    ).toHaveAttribute("href", "/ops/appearance");
    expect(
      await screen.findByRole("button", { name: "创建并登录总部账号" }),
    ).toBeEnabled();
    expect(
      requests.some((entry) =>
        entry.path.endsWith("/operator/channel-accounts"),
      ),
    ).toBe(true);
  });
  it("denies customer admins access to the operator pool", async () => {
    const { requests } = mockApi(
      [],
      true,
      unverifiedCapability,
      "tenant_admin",
    );
    renderOperator();
    await waitFor(() =>
      expect(screen.getByTestId("auth-status")).toHaveTextContent(
        "authenticated",
      ),
    );
    expect(
      await screen.findByText("只有总部资源管理员可以管理共享账号资源池。"),
    ).toBeVisible();
    expect(screen.queryByRole("heading", { name: "账号资源池" })).toBeNull();
    expect(requests.some((entry) => entry.path.includes("/operator/"))).toBe(
      false,
    );
  });
  it("does not offer appearance settings to resource admins", async () => {
    mockApi([], true);
    renderOperator();
    await screen.findByRole("heading", { name: "账号资源池" });
    expect(
      screen.queryByRole("link", { name: "工作区外观" }),
    ).not.toBeInTheDocument();
  });
  it("labels article wire format without changing its saved capability key", async () => {
    const { requests } = mockApi([], true, {
      ...verifiedCapability,
      verified_content_types: ["plain_text_article.v1"],
    });
    renderOperator();
    const section = await screen.findByLabelText("运营连接器能力");
    await userEvent.click(
      await within(section).findByRole("checkbox", {
        name: "允许 zhihu 纯文本文章（标题与正文）",
      }),
    );
    await userEvent.click(
      within(section).getByRole("checkbox", {
        name: "启用 zhihu primary 连接器",
      }),
    );
    await userEvent.click(
      within(section).getByRole("button", { name: "保存连接器配置" }),
    );
    await waitFor(() =>
      expect(
        requests.find(
          (entry) =>
            entry.method === "PATCH" &&
            entry.path.endsWith(
              "/operator/connector-capabilities/zhihu/primary",
            ),
        )?.body,
      ).toEqual({
        expected_revision: 3,
        enabled: true,
        content_types: ["plain_text_article.v1"],
      }),
    );
  });

  it("cannot enable an unverified connector from an account login", async () => {
    const { requests } = mockApi([account], true);
    renderOperator();
    const section = await screen.findByLabelText("运营连接器能力");
    const toggle = await within(section).findByRole("checkbox", {
      name: "启用 zhihu primary 连接器",
    });
    expect(toggle).toBeDisabled();
    expect(within(section).getByText("未实测可用")).toBeTruthy();
    expect(
      requests.filter(
        (item) =>
          item.method === "PATCH" &&
          item.path.includes("connector-capabilities"),
      ),
    ).toHaveLength(0);
  });

  it("saves only a verified content subset with an optimistic revision", async () => {
    const { requests } = mockApi([], true, verifiedCapability);
    renderOperator();
    const section = await screen.findByLabelText("运营连接器能力");
    await userEvent.click(
      await within(section).findByRole("checkbox", {
        name: "允许 zhihu article",
      }),
    );
    await userEvent.click(
      within(section).getByRole("checkbox", {
        name: "启用 zhihu primary 连接器",
      }),
    );
    await userEvent.click(
      within(section).getByRole("button", { name: "保存连接器配置" }),
    );
    await waitFor(() =>
      expect(
        requests.find(
          (entry) =>
            entry.path.endsWith(
              "/operator/connector-capabilities/zhihu/primary",
            ) && entry.method === "PATCH",
        )?.body,
      ).toEqual({
        expected_revision: 3,
        enabled: true,
        content_types: ["article"],
      }),
    );
  });

  it("reports a stale revision and reloads instead of claiming a configuration save", async () => {
    const { fetchMock } = mockApi([], true, verifiedCapability);
    const original = fetchMock.getMockImplementation()!;
    fetchMock.mockImplementation((request, init) => {
      if (
        String(request).endsWith(
          "/operator/connector-capabilities/zhihu/primary",
        ) &&
        init?.method === "PATCH"
      )
        return Promise.resolve(
          response({ code: "conflict", message: "revision changed" }, 409),
        );
      return original(request, init);
    });
    renderOperator();
    const section = await screen.findByLabelText("运营连接器能力");
    await userEvent.click(
      await within(section).findByRole("checkbox", {
        name: "允许 zhihu article",
      }),
    );
    await userEvent.click(
      within(section).getByRole("button", { name: "保存连接器配置" }),
    );
    expect(await within(section).findByText(/配置已由其他人更新/)).toBeTruthy();
    expect(within(section).queryByText(/连接器配置已更新/)).toBeNull();
  });

  it("lists the separate shared pool and assigns an account to an explicit project", async () => {
    const { requests } = mockApi(
      [{ ...account, group_id: "pool-group-1" }],
      true,
    );
    renderOperator();
    expect(await screen.findByText("已识别账号")).toBeTruthy();
    expect(screen.getAllByText("共享组").length).toBeGreaterThan(0);
    await userEvent.click(screen.getByRole("button", { name: "项目分配" }));
    await userEvent.type(
      screen.getByRole("textbox", { name: "目标租户 ID" }),
      "tenant-2",
    );
    await userEvent.type(
      screen.getByRole("textbox", { name: "目标项目 ID" }),
      "project-2",
    );
    await userEvent.click(screen.getByRole("button", { name: "分配项目" }));
    await screen.findByText("共享账号已分配给项目。");
    expect(
      requests.find(
        (item) =>
          item.method === "POST" &&
          item.path.endsWith(
            "/operator/channel-accounts/account-1/assignments",
          ),
      )?.body,
    ).toEqual({ tenant_id: "tenant-2", project_id: "project-2" });
  });
});
