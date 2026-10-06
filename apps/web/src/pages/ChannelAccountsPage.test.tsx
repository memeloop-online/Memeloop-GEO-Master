import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import {
  assignPoolAccount,
  createChannelAccount,
  createChannelGroup,
  getChannelLoginSnapshot,
  listOperatorConnectorCapabilities,
  listProjectConnectorCapabilities,
  listChannelAccounts,
  listChannelGroups,
  listPoolAccounts,
  sendChannelLoginAction,
  unassignPoolAccount,
  updateChannelAccount,
  updateOperatorConnectorCapability,
  type OperatorConnectorCapability,
} from "../api/channels";
import {
  ChannelAccountsPage,
  OperatorAccountsPage,
} from "./ChannelAccountsPage";

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
  { id: "zhihu", label: "知乎", purpose: "publishing", login_supported: true },
  {
    id: "baidu_creator",
    label: "百家号",
    purpose: "publishing",
    login_supported: true,
  },
  {
    id: "xiaohongshu",
    label: "小红书",
    purpose: "publishing",
    login_supported: true,
  },
  { id: "kimi", label: "Kimi", purpose: "measurement", login_supported: true },
];
const snapshot = {
  phase: "login_required",
  url: "https://example.test/login",
  width: 800,
  height: 600,
  screenshot_base64: "AA==",
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
) {
  const requests: { path: string; method: string; body?: unknown; url: URL }[] =
    [];
  const fetchMock = vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(request), "http://localhost");
    const path = url.pathname;
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    requests.push({ path, method, body, url });
    if (path.endsWith("/auth/session"))
      return Promise.resolve(
        response(
          operator
            ? {
                ...session,
                memberships: [
                  { ...session.memberships[0], role: "resource_admin" },
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
      return Promise.resolve(response({ items: platforms }));
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
    if (path.endsWith("/channel-login-sessions/login-1/snapshot"))
      return Promise.resolve(response(snapshot));
    if (path.endsWith("/channel-login-sessions/login-1/actions"))
      return Promise.resolve(
        response({
          ...snapshot,
          phase: "ready_to_complete",
          identity: {
            display_name: "已识别账号",
            platform_account_id: "external-1",
          },
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
  return { requests, fetchMock };
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

function renderOperator() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <MemoryRouter>
            <OperatorAccountsPage />
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

  it("sends proxy secrets only in write body and preserves exact remote action", async () => {
    const { requests } = mockApi();
    await createChannelAccount("tenant-1", "project-1", "zhihu", undefined, {
      server: "socks5://proxy.example.test:1080",
      username: "user",
      password: "placeholder",
    });
    await sendChannelLoginAction("tenant-1", "project-1", "login-1", {
      kind: "click",
      x: 33,
      y: 44,
    });
    await getChannelLoginSnapshot("tenant-1", "project-1", "login-1");
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
      requests.find((item) => item.path.endsWith("/actions"))?.body,
    ).toEqual({ kind: "click", x: 33, y: 44 });
    expect(
      requests
        .find((item) => item.path.endsWith("/snapshot"))
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
  it("shows account connection separately from read-only project publication verification", async () => {
    const { requests } = mockApi([account]);
    renderPage();
    expect(await screen.findByText("已识别账号")).toBeTruthy();
    const section = await screen.findByLabelText("项目发布连接器能力");
    expect(within(section).getByText("未实测可用")).toBeTruthy();
    expect(
      within(section).getByText(/账号已连接只代表登录身份有效/),
    ).toBeTruthy();
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
    const section = await screen.findByLabelText("项目发布连接器能力");
    expect(await within(section).findByText("权限不足")).toBeTruthy();
    expect(within(section).queryByText("已验证可用")).toBeNull();
  });

  it("shows real loaded accounts and reconnects without changing identity locally", async () => {
    const { requests } = mockApi([account]);
    renderPage();
    expect(await screen.findByText("已识别账号")).toBeTruthy();
    expect(screen.getByText("已连接")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "重新连接" }));
    await screen.findByRole("img", { name: /远程登录页面截图/ });
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

  it("offers reconnect for expired accounts and keyboard-accessible pixel actions", async () => {
    const { requests } = mockApi([{ ...account, status: "expired" }]);
    renderPage();
    expect(await screen.findByText("登录已失效")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "登录并验证" }));
    const remote = await screen.findByLabelText("远程登录");
    await userEvent.type(
      within(remote).getByRole("spinbutton", { name: "点击横坐标" }),
      "20",
    );
    await userEvent.type(
      within(remote).getByRole("spinbutton", { name: "点击纵坐标" }),
      "40",
    );
    await userEvent.click(
      within(remote).getByRole("button", { name: "点击坐标" }),
    );
    await waitFor(() =>
      expect(
        requests.find(
          (item) =>
            item.path.endsWith("/actions") &&
            (item.body as { kind: string }).kind === "click",
        )?.body,
      ).toEqual({ kind: "click", x: 20, y: 40 }),
    );
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

  it("saves verified identity automatically after a remote action and clears typed text", async () => {
    const { requests } = mockApi();
    renderPage("connect");
    await screen.findByRole("button", { name: "启动远程登录" });
    await userEvent.click(screen.getByRole("button", { name: "启动远程登录" }));
    await screen.findByRole("img", { name: /远程登录页面截图/ });
    const remote = screen.getByLabelText("远程登录");
    const input = within(remote).getByLabelText(/向当前焦点输入文字/);
    expect(input).toHaveAttribute("type", "text");
    await userEvent.type(input, "ordinary text");
    await userEvent.click(
      within(remote).getByRole("button", { name: "发送文字" }),
    );
    await waitFor(() =>
      expect(
        requests.some(
          (item) => item.path.endsWith("/complete") && item.method === "POST",
        ),
      ).toBe(true),
    );
    expect((input as HTMLInputElement).value).toBe("");
    expect(await screen.findByText("已验证账号身份并保存连接。")).toBeTruthy();
  });

  it("closes an already expired login and discards cached account screenshots", async () => {
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
    await screen.findByRole("img", { name: /远程登录页面截图/ });
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
});

describe("operator pool", () => {
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
