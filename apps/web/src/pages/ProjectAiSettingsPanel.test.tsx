import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import { ChannelAccountsPage } from "./ChannelAccountsPage";
import type { ProjectAiSetting } from "../api/projectAi";
import i18n from "../i18n";

const initial: ProjectAiSetting = {
  usage: "workbench_content",
  revision: 1,
  mode: "custom",
  model: "example-model",
  base_url: "https://api.example.test/v1",
  key_present: true,
  effective: { source: "custom", configured: true, model: "example-model" },
};
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), { status });
}
function setup(
  role = "tenant_admin",
  saveStatus = 200,
  testStatus = 200,
  modelsStatus = 200,
) {
  let setting = { ...initial };
  const requests: {
    method: string;
    path: string;
    body: Record<string, unknown> | undefined;
  }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (request: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(request), "http://localhost");
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : undefined;
      requests.push({ method, path: url.pathname, body });
      if (url.pathname.endsWith("/auth/session"))
        return response({
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
          expires_at: "2030-01-01T00:00:00Z",
          csrf_token: "test-csrf",
        });
      if (url.pathname.endsWith("/ai-settings"))
        return response({
          items: [
            setting,
            {
              ...initial,
              usage: "observation_analysis",
              mode: "inherit",
              model: null,
              base_url: null,
              key_present: false,
              prefer_connected_account: true,
              effective: {
                source: "unconfigured",
                configured: false,
                model: null,
              },
            },
          ],
        });
      if (url.pathname.endsWith("/workbench_content") && method === "PUT") {
        if (saveStatus !== 200)
          return response(
            { message: "do-not-render-provider-secret" },
            saveStatus,
          );
        setting = {
          ...setting,
          model: body.model ?? null,
          base_url: body.base_url ?? null,
          mode: body.mode,
          revision: setting.revision + 1,
        };
        return response(setting);
      }
      if (url.pathname.endsWith("/models"))
        return response(
          modelsStatus === 200
            ? { items: [{ id: "discovered-model" }, { id: "second-model" }] }
            : { message: "do-not-render-provider-secret" },
          modelsStatus,
        );
      if (url.pathname.endsWith("/test"))
        return response(
          testStatus === 200
            ? { success: true }
            : { message: "do-not-render-provider-secret" },
          testStatus,
        );
      return response({ items: [] });
    }),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <MemoryRouter
            initialEntries={["/app/tenant-1/project-1/settings?tab=ai"]}
          >
            <Routes>
              <Route
                path="/app/:tenantId/:projectId/settings"
                element={<ChannelAccountsPage view="settings" />}
              />
            </Routes>
          </MemoryRouter>
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>,
  );
  return { requests, client };
}
afterEach(async () => {
  vi.unstubAllGlobals();
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  await i18n.changeLanguage("zh-CN");
});
describe("project AI settings", () => {
  it("opens the AI deep link, saves a model while preserving a blank key, and tests the persisted revision", async () => {
    const { requests } = setup();
    const section = await screen.findByLabelText("工作台与内容生成");
    expect(screen.getByRole("tab", { name: "模型与 AI" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.queryByLabelText("已接入账号")).not.toBeInTheDocument();
    expect(within(section).getByLabelText("API 密钥")).toHaveValue("");
    expect(
      within(section).getByRole("button", { name: "保存配置" }),
    ).toBeDisabled();
    await userEvent.clear(
      within(section).getByRole("textbox", { name: "模型名称" }),
    );
    await userEvent.type(
      within(section).getByRole("textbox", { name: "模型名称" }),
      "other-model",
    );
    expect(
      within(section).getByRole("button", { name: "测试已保存配置" }),
    ).toBeDisabled();
    await userEvent.click(
      within(section).getByRole("button", { name: "保存配置" }),
    );
    await within(section).findByText("配置已保存。");
    expect(requests.find((item) => item.method === "PUT")?.body).toEqual({
      expected_revision: 1,
      mode: "custom",
      model: "other-model",
      base_url: initial.base_url,
    });
    await userEvent.click(
      within(section).getByRole("button", { name: "测试已保存配置" }),
    );
    await within(section).findByText("模型连接成功。");
    expect(requests.find((item) => item.path.endsWith("/test"))?.body).toEqual({
      expected_revision: 2,
    });
    expect(
      within(section).getByRole("textbox", { name: "模型名称" }),
    ).toHaveValue("other-model");
    expect(within(section).getByLabelText("API 密钥")).toHaveValue("");
  });
  it("clears a newly submitted key and never puts it in the query cache", async () => {
    const { requests, client } = setup();
    const section = await screen.findByLabelText("工作台与内容生成");
    await userEvent.type(
      within(section).getByLabelText("API 密钥"),
      "synthetic-key",
    );
    await userEvent.click(
      within(section).getByRole("button", { name: "保存配置" }),
    );
    await within(section).findByText("配置已保存。");
    expect(requests.find((item) => item.method === "PUT")?.body?.api_key).toBe(
      "synthetic-key",
    );
    expect(within(section).getByLabelText("API 密钥")).toHaveValue("");
    expect(
      JSON.stringify(
        client
          .getQueryCache()
          .getAll()
          .map((query) => query.state.data),
      ),
    ).not.toContain("synthetic-key");
  });
  it("keeps viewer settings read-only", async () => {
    const { requests } = setup("viewer");
    const section = await screen.findByLabelText("工作台与内容生成");
    expect(
      within(section).getByRole("textbox", { name: "模型名称" }),
    ).toBeDisabled();
    expect(
      within(section).getByRole("button", { name: "测试已保存配置" }),
    ).toBeDisabled();
    expect(requests.some((item) => item.method !== "GET")).toBe(false);
  });
  it("discovers with the saved revision and key, then selects a model as an unsaved edit", async () => {
    const { requests } = setup();
    const section = await screen.findByLabelText("工作台与内容生成");
    await userEvent.click(
      within(section).getByRole("button", { name: "获取模型列表" }),
    );
    const choices = await within(section).findByRole("combobox", {
      name: "可用模型",
    });
    expect(
      requests.find((item) => item.path.endsWith("/models"))?.body,
    ).toEqual({ expected_revision: 1 });
    expect(
      within(section).getByRole("textbox", { name: "模型名称" }),
    ).toHaveValue("example-model");
    await userEvent.selectOptions(choices, "discovered-model");
    expect(
      within(section).getByRole("textbox", { name: "模型名称" }),
    ).toHaveValue("discovered-model");
    expect(
      within(section).getByRole("button", { name: "获取模型列表" }),
    ).toBeDisabled();
    expect(
      within(section).getByText("请先保存修改，再测试或获取模型列表。"),
    ).toBeInTheDocument();
    expect(requests.some((item) => item.method === "PUT")).toBe(false);
    await userEvent.click(
      within(section).getByRole("button", { name: "保存配置" }),
    );
    await within(section).findByText("配置已保存。");
    expect(requests.find((item) => item.method === "PUT")?.body?.model).toBe(
      "discovered-model",
    );
    expect(
      requests.find((item) => item.method === "PUT")?.body,
    ).not.toHaveProperty("api_key");
  });
  it("keeps manual model entry available after discovery fails", async () => {
    setup("tenant_admin", 200, 200, 503);
    const section = await screen.findByLabelText("工作台与内容生成");
    await userEvent.click(
      within(section).getByRole("button", { name: "获取模型列表" }),
    );
    await within(section).findByText(
      "无法获取模型列表。你仍可以手动输入模型名称。",
    );
    expect(
      screen.queryByText(/do-not-render-provider-secret/),
    ).not.toBeInTheDocument();
    const input = within(section).getByRole("textbox", { name: "模型名称" });
    await userEvent.clear(input);
    await userEvent.type(input, "manual-model");
    expect(input).toHaveValue("manual-model");
    expect(
      within(section).getByRole("button", { name: "保存配置" }),
    ).toBeEnabled();
  });
  it("does not label an inherited configured route unavailable when its model is resolved at call time", async () => {
    setup();
    const original = vi.mocked(fetch).getMockImplementation()!;
    vi.mocked(fetch).mockImplementation(async (request, init) => {
      const result = await original(request, init);
      if (
        new URL(String(request), "http://localhost").pathname.endsWith(
          "/ai-settings",
        )
      ) {
        const body = await result.json();
        body.items[1].effective = {
          source: "inherit",
          configured: true,
          model: null,
        };
        return response(body);
      }
      return result;
    });
    const section = await screen.findByLabelText("测量结果解析");
    expect(within(section).getByText("默认模型已配置。")).toBeInTheDocument();
    expect(
      within(section).queryByText("当前尚无可用的默认模型。"),
    ).not.toBeInTheDocument();
  });
  it("requires a reload after a successful write whose readback fails", async () => {
    const { requests } = setup();
    const section = await screen.findByLabelText("工作台与内容生成");
    const original = vi.mocked(fetch).getMockImplementation()!;
    vi.mocked(fetch).mockImplementation(async (request, init) => {
      if (
        new URL(String(request), "http://localhost").pathname.endsWith(
          "/ai-settings",
        ) &&
        requests.some((item) => item.method === "PUT")
      )
        return response({}, 503);
      return original(request, init);
    });
    await userEvent.type(
      within(section).getByRole("textbox", { name: "模型名称" }),
      "-updated",
    );
    await userEvent.click(
      within(section).getByRole("button", { name: "保存配置" }),
    );
    await within(section).findByText(
      "配置已保存，但暂时无法重新读取。请重新读取后继续。",
    );
    expect(within(section).queryByText("配置已保存。")).not.toBeInTheDocument();
    expect(
      within(section).getByRole("button", { name: "保存配置" }),
    ).toBeDisabled();
  });
  it("saves inherited routing without resending a custom model or key", async () => {
    const { requests } = setup();
    const section = await screen.findByLabelText("工作台与内容生成");
    await userEvent.selectOptions(
      within(section).getByRole("combobox", { name: "模型来源" }),
      "inherit",
    );
    await userEvent.click(
      within(section).getByRole("button", { name: "保存配置" }),
    );
    await within(section).findByText("配置已保存。");
    expect(requests.find((item) => item.method === "PUT")?.body).toEqual({
      expected_revision: 1,
      mode: "inherit",
    });
    expect(
      within(section).queryByLabelText("API 密钥"),
    ).not.toBeInTheDocument();
  });
  it("reports failed tests without reflecting upstream messages or claiming success", async () => {
    setup("tenant_admin", 200, 502);
    const section = await screen.findByLabelText("工作台与内容生成");
    await userEvent.click(
      within(section).getByRole("button", { name: "测试已保存配置" }),
    );
    await within(section).findByText("模型测试失败，请检查配置后重试。");
    expect(
      screen.queryByText(/do-not-render-provider-secret/),
    ).not.toBeInTheDocument();
    expect(
      within(section).queryByText("模型连接成功。"),
    ).not.toBeInTheDocument();
  });
  it("preserves dirty inputs on conflict without reflecting raw provider errors", async () => {
    setup("tenant_admin", 409);
    const section = await screen.findByLabelText("工作台与内容生成");
    await userEvent.type(
      within(section).getByRole("textbox", { name: "模型名称" }),
      "-edited",
    );
    await userEvent.click(
      within(section).getByRole("button", { name: "保存配置" }),
    );
    await within(section).findByText(
      "配置已被更新。请重新读取最新配置后再修改。",
    );
    expect(
      within(section).getByRole("textbox", { name: "模型名称" }),
    ).toHaveValue("example-model-edited");
    expect(
      screen.queryByText(/do-not-render-provider-secret/),
    ).not.toBeInTheDocument();
    expect(
      within(section).getByRole("button", { name: "保存配置" }),
    ).toBeDisabled();
  });
  it("switches back to accounts without losing the deep-link tab state", async () => {
    setup();
    await screen.findByLabelText("工作台与内容生成");
    await userEvent.click(screen.getByRole("tab", { name: "账号与资源" }));
    await waitFor(() =>
      expect(
        screen.queryByLabelText("工作台与内容生成"),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("tab", { name: "账号与资源" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });
});
