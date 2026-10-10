import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import * as api from "../api/projectSerp";
import { ChannelAccountsPage } from "./ChannelAccountsPage";
import { ApiError } from "../api/client";
import i18n from "../i18n";

const auth = vi.hoisted(() => ({ role: "tenant_admin" }));
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { id: "user" },
      operator: { id: "operator" },
      memberships: [{ tenant_id: "tenant", role: auth.role }],
    },
  }),
}));
vi.mock("../api/channels", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/channels")>()),
  useChannelData: () => ({
    accounts: { data: { items: [] }, refetch: vi.fn() },
    groups: { data: { items: [] }, refetch: vi.fn() },
    platforms: { data: { items: [] } },
  }),
}));
const initial: api.ProjectSerpSetting = {
  source_key: "primary",
  provider: "dataforseo",
  revision: 3,
  enabled: true,
  credentials_present: true,
  active_credential_revision: 2,
  protocol_defaults: {
    query: "",
    engine: "google",
    surface: "third_party_api",
    source: "dataforseo",
    country: "US",
    city: null,
    language: "en",
    device: "desktop",
    requested_depth: 10,
    source_location_code: "2840",
  },
};
function setup({
  role = "tenant_admin",
  items = [initial],
  encryption = true,
}: {
  role?: string;
  items?: api.ProjectSerpSetting[];
  encryption?: boolean;
} = {}) {
  auth.role = role;
  const get = vi
    .spyOn(api, "getProjectSerpSettings")
    .mockResolvedValue({ items, encryption_available: encryption });
  const save = vi
    .spyOn(api, "saveProjectSerpSetting")
    .mockImplementation(async (_tenant, _project, key, body) => ({
      ...initial,
      source_key: key,
      revision: body.expected_revision + 1,
      enabled: body.enabled,
      protocol_defaults: body.protocol_defaults,
    }));
  const test = vi.spyOn(api, "testProjectSerpSetting").mockResolvedValue({
    source_key: "primary",
    revision: 3,
    status: "connected",
    checked_at: "2026-10-01T00:00:00Z",
  });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <FluentProvider theme={webLightTheme}>
        <MemoryRouter
          initialEntries={["/app/tenant/project/settings?tab=search"]}
        >
          <Routes>
            <Route
              path="/app/:tenantId/:projectId/settings"
              element={<ChannelAccountsPage view="settings" />}
            />
          </Routes>
        </MemoryRouter>
      </FluentProvider>
    </QueryClientProvider>,
  );
  return { get, save, test, client };
}
afterEach(async () => {
  cleanup();
  vi.restoreAllMocks();
  await i18n.changeLanguage("zh-CN");
});
describe("project search source settings", () => {
  it("opens the settings tab, preserves blank credentials, and tests only the saved revision", async () => {
    const { save, test } = setup();
    const user = userEvent.setup();
    await screen.findByText("已保存账号凭据", { exact: false });
    expect(screen.getByRole("tab", { name: "搜索数据源" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      screen.queryByRole("button", { name: "连接账号" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByText("高级搜索设置"));
    await user.clear(screen.getByRole("textbox", { name: /默认语言/ }));
    await user.type(screen.getByRole("textbox", { name: /默认语言/ }), "fr");
    expect(
      screen.getByRole("button", { name: "测试已保存账号" }),
    ).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("设置已保存。")).toBeInTheDocument();
    expect(save.mock.calls[0][3]).toEqual({
      expected_revision: 3,
      enabled: true,
      protocol_defaults: { ...initial.protocol_defaults, language: "fr" },
    });
    test.mockResolvedValue({
      source_key: "primary",
      revision: 4,
      status: "connected",
      checked_at: "2026-10-01T00:00:00Z",
    });
    await user.click(screen.getByRole("button", { name: "测试已保存账号" }));
    expect(test).toHaveBeenCalledWith("tenant", "project", "primary", 4);
    expect(await screen.findByText("账号连接成功。")).toBeInTheDocument();
  });
  it("requires credential pairs, clears both after saving, and keeps them out of the cache", async () => {
    const { save, client } = setup();
    const user = userEvent.setup();
    const login = await screen.findByRole("textbox", { name: "API 账号" });
    const password = screen.getByLabelText("API 密码");
    await user.type(login, "synthetic-login");
    expect(screen.getByRole("button", { name: "保存设置" })).toBeDisabled();
    await user.type(password, "synthetic-password");
    await user.click(screen.getByRole("button", { name: "保存设置" }));
    await screen.findByText("设置已保存。");
    expect(save.mock.calls[0][3]).toMatchObject({
      login: "synthetic-login",
      password: "synthetic-password",
    });
    expect(login).toHaveValue("");
    expect(password).toHaveValue("");
    const cache = JSON.stringify(
      client
        .getQueryCache()
        .getAll()
        .map((entry) => entry.state.data),
    );
    expect(cache).not.toContain("synthetic-login");
    expect(cache).not.toContain("synthetic-password");
    expect(screen.queryByText("账号连接成功。")).not.toBeInTheDocument();
  });
  it("retains inputs on conflict without exposing upstream messages", async () => {
    const { save } = setup();
    save.mockRejectedValue(
      new ApiError(409, { message: "private-provider-text" }, "conflict"),
    );
    const user = userEvent.setup();
    await user.type(
      await screen.findByRole("textbox", { name: "API 账号" }),
      "draft-login",
    );
    await user.type(screen.getByLabelText("API 密码"), "draft-password");
    await user.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText(/设置已被更新/)).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "API 账号" })).toHaveValue(
      "draft-login",
    );
    expect(screen.getByLabelText("API 密码")).toHaveValue("draft-password");
    expect(screen.queryByText("private-provider-text")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存设置" })).toBeDisabled();
  });
  it.each(["member", "viewer"])(
    "shows only redacted read-only settings for %s",
    async (role) => {
      const { save, test } = setup({ role });
      expect(
        await screen.findByText(/修改和测试需要项目管理员权限/),
      ).toBeInTheDocument();
      expect(screen.queryByLabelText("API 账号")).not.toBeInTheDocument();
      expect(screen.queryByLabelText("API 密码")).not.toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: "保存设置" }),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: "测试已保存账号" }),
      ).not.toBeInTheDocument();
      expect(save).not.toHaveBeenCalled();
      expect(test).not.toHaveBeenCalled();
    },
  );
  it("does not offer secret writes when secure account storage is unavailable", async () => {
    setup({ encryption: false });
    expect(
      await screen.findByText("账号保存暂时不可用，请联系管理员。"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("API 密码")).toBeDisabled();
    expect(screen.getByRole("button", { name: "保存设置" })).toBeDisabled();
  });
  it("creates the first source only after explicit input and save", async () => {
    const { save } = setup({ items: [] });
    const user = userEvent.setup();
    expect(await screen.findByText(/尚未配置账号/)).toBeInTheDocument();
    expect(save).not.toHaveBeenCalled();
    expect(
      screen.getByText("默认搜索：Google · 美国 · 英语 · 桌面端"),
    ).toBeInTheDocument();
    expect(
      screen.getByLabelText(/搜索地区编号/).closest("details"),
    ).not.toHaveAttribute("open");
    await user.type(
      screen.getByRole("textbox", { name: "API 账号" }),
      "synthetic-login",
    );
    await user.type(screen.getByLabelText("API 密码"), "synthetic-password");
    await user.click(screen.getByRole("button", { name: "保存设置" }));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0].slice(0, 3)).toEqual([
      "tenant",
      "project",
      "primary",
    ]);
    expect(save.mock.calls[0][3]).toMatchObject({
      expected_revision: 0,
      enabled: false,
      protocol_defaults: {
        query: "",
        source_location_code: "2840",
        country: "US",
        language: "en",
      },
    });
  });
  it("shows failed saved-account tests without promoting the source to connected", async () => {
    await i18n.changeLanguage("en");
    const { test } = setup();
    test.mockResolvedValue({
      source_key: "primary",
      revision: 3,
      status: "authentication_failed",
      checked_at: "2026-10-01T00:00:00Z",
    });
    await userEvent.click(
      await screen.findByRole("button", { name: "Test saved account" }),
    );
    expect(
      await screen.findByText(
        "Connection test failed. Check the account or try again later.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Account connection succeeded."),
    ).not.toBeInTheDocument();
  });
  it("requires reloading after a saved-revision test conflict without retrying automatically", async () => {
    const { test } = setup();
    test.mockRejectedValue(
      new ApiError(409, { message: "private-conflict" }, "conflict"),
    );
    await userEvent.click(
      await screen.findByRole("button", { name: "测试已保存账号" }),
    );
    expect(await screen.findByText(/设置已被更新/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "测试已保存账号" }),
    ).toBeDisabled();
    expect(test).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("private-conflict")).not.toBeInTheDocument();
  });
  it("preserves replacement inputs when saving is uncertain", async () => {
    const { save } = setup();
    save.mockRejectedValue(new Error("private-transport-detail"));
    const user = userEvent.setup();
    await user.type(
      await screen.findByRole("textbox", { name: "API 账号" }),
      "draft-login",
    );
    await user.type(screen.getByLabelText("API 密码"), "draft-password");
    await user.click(screen.getByRole("button", { name: "保存设置" }));
    expect(
      await screen.findByText("保存尚未确认，请重新读取设置后核对。"),
    ).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "API 账号" })).toHaveValue(
      "draft-login",
    );
    expect(screen.getByLabelText("API 密码")).toHaveValue("draft-password");
    expect(save).toHaveBeenCalledTimes(1);
    expect(
      screen.queryByText("private-transport-detail"),
    ).not.toBeInTheDocument();
  });
});
