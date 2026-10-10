import { act, fireEvent, render, screen } from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import type { TenantMembership } from "../auth/types";
import i18n from "../i18n";
import { WorkspacePage } from "./WorkspacePage";

const state = vi.hoisted(() => ({
  memberships: [] as TenantMembership[],
  projects: [] as Array<{ id: string; display_name: string }>,
  projectError: false,
  projectPending: false,
}));
const refetch = vi.hoisted(() => vi.fn());

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({ session: { memberships: state.memberships } }),
}));
vi.mock("../api/projects", () => ({
  useProjectsQuery: () => ({
    data:
      state.projectError || state.projectPending
        ? undefined
        : { items: state.projects },
    isError: state.projectError,
    isPending: state.projectPending,
    refetch,
  }),
}));

function LocationDisplay() {
  const location = useLocation();
  return (
    <output data-testid="destination">
      {location.pathname}
      {location.search}
    </output>
  );
}

function renderWorkspace(url = "/workspaces") {
  return render(
    <FluentProvider theme={webLightTheme}>
      <MemoryRouter initialEntries={[url]}>
        <Routes>
          <Route path="/workspaces" element={<WorkspacePage />} />
          <Route path="*" element={<LocationDisplay />} />
        </Routes>
      </MemoryRouter>
    </FluentProvider>,
  );
}

beforeEach(async () => {
  state.memberships = [
    {
      tenant_id: "tenant-a",
      tenant_slug: "team-a",
      tenant_display_name: "Example workspace",
      role: "tenant_admin",
    },
  ];
  state.projects = [{ id: "project-a", display_name: "Example project" }];
  state.projectError = false;
  state.projectPending = false;
  refetch.mockReset();
  await act(() => i18n.changeLanguage("zh-CN"));
});

afterEach(async () => {
  await act(() => i18n.changeLanguage("zh-CN"));
});

describe("workspace localization and navigation", () => {
  it.each([
    [
      "zh-CN",
      "选择工作区",
      "创建项目",
      "管理运营账号池",
      "从对话开始，描述目标或上传资料。",
    ],
    [
      "en",
      "Choose a workspace",
      "Create project",
      "Manage operator account pool",
      "Start a conversation, describe your goal, or upload files.",
    ],
  ])(
    "renders localized text and role for %s",
    async (locale, heading, create, manage, hint) => {
      await act(() => i18n.changeLanguage(locale));
      state.memberships[0].role = "operator_admin";
      renderWorkspace();

      expect(
        screen.getByRole("heading", { level: 1, name: heading }),
      ).toBeInTheDocument();
      expect(screen.getByRole("button", { name: create })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: manage })).toBeInTheDocument();
      expect(screen.getByText(hint)).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Example project" }),
      ).toBeInTheDocument();
      expect(
        screen.getByText(
          locale === "en" ? "Role: Operator administrator" : "角色：运营管理员",
        ),
      ).toBeInTheDocument();
      expect(
        screen.queryByText(/operator_admin|tenant_admin/),
      ).not.toBeInTheDocument();
    },
  );

  it.each([
    ["zh-CN", "还没有可用工作区", "请联系工作区管理员获取访问权限。"],
    [
      "en",
      "No workspaces available",
      "Contact a workspace administrator for access.",
    ],
  ])(
    "shows a localized empty state for %s",
    async (locale, heading, detail) => {
      await act(() => i18n.changeLanguage(locale));
      state.memberships = [];
      renderWorkspace();
      expect(
        screen.getByRole("heading", { name: heading }),
      ).toBeInTheDocument();
      expect(screen.getByText(detail)).toBeInTheDocument();
    },
  );

  it.each([
    [
      "zh-CN",
      "工作区管理员",
      "成员",
      "只读成员",
      "运营人员",
      "运营管理员",
      "品牌管理员",
      "资源管理员",
    ],
    [
      "en",
      "Workspace administrator",
      "Member",
      "Read-only member",
      "Operator",
      "Operator administrator",
      "Brand administrator",
      "Resource administrator",
    ],
  ])(
    "uses customer-facing labels for all membership roles in %s",
    async (locale, ...roles) => {
      await act(() => i18n.changeLanguage(locale));
      state.memberships = (
        [
          "tenant_admin",
          "member",
          "viewer",
          "operator_agent",
          "operator_admin",
          "oem_admin",
          "resource_admin",
        ] as const
      ).map((role, index) => ({
        tenant_id: `tenant-${index}`,
        tenant_slug: `team-${index}`,
        tenant_display_name: `Workspace ${index}`,
        role,
      }));
      renderWorkspace();
      for (const role of roles) {
        expect(
          screen.getByText(locale === "en" ? `Role: ${role}` : `角色：${role}`),
        ).toBeInTheDocument();
      }
      expect(
        screen.queryByText(/operator_agent|resource_admin|tenant_admin/),
      ).not.toBeInTheDocument();
    },
  );

  it("keeps create-project returnTo and existing-project chat routes unchanged", () => {
    const first = renderWorkspace(
      "/workspaces?returnTo=%2Fapp%2Ftenant-a%2Fproject-a%2Fmeasurement",
    );
    fireEvent.click(screen.getByRole("button", { name: "创建项目" }));
    expect(screen.getByTestId("destination")).toHaveTextContent(
      "/setup?tenant_id=tenant-a&returnTo=%2Fapp%2Ftenant-a%2Fproject-a%2Fmeasurement",
    );
    first.unmount();
    renderWorkspace();
    fireEvent.click(screen.getByRole("button", { name: "Example project" }));
    expect(screen.getByTestId("destination")).toHaveTextContent(
      "/app/tenant-a/project-a/chat",
    );
  });

  it("localizes project-load failure and preserves retry", async () => {
    await act(() => i18n.changeLanguage("en"));
    state.projectError = true;
    renderWorkspace();
    expect(screen.getByText("Couldn't load projects")).toBeInTheDocument();
    expect(
      screen.getByText("You can still create a project."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(refetch).toHaveBeenCalledOnce();
    expect(
      screen.getByRole("button", { name: "Create project" }),
    ).toBeInTheDocument();
  });
});
