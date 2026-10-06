import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import {
  MemoryRouter,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from "react-router-dom";
import { AppShell } from "./AppShell";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { display_name: "测试用户" },
      memberships: [{ tenant_id: "tenant-a", tenant_display_name: "工作区" }],
    },
    logout: vi.fn(),
  }),
}));

vi.mock("../api/projects", () => ({
  useProjectsQuery: () => ({
    data: {
      items: [
        {
          id: "project-a",
          display_name: "项目",
          settings: { market: "中国", language: "中文" },
        },
      ],
    },
    isPending: false,
    isError: false,
    refetch: vi.fn(),
  }),
}));

function RouteInspector() {
  const location = useLocation();
  const navigate = useNavigate();
  return (
    <>
      <output data-testid="path">{location.pathname}</output>
      <button onClick={() => navigate(-1)}>返回</button>
    </>
  );
}

function renderShell(path: string) {
  return render(
    <FluentProvider theme={webLightTheme}>
      <MemoryRouter initialEntries={[`/app/tenant-a/project-a/${path}`]}>
        <Routes>
          <Route path="/app/:tenantId/:projectId" element={<AppShell />}>
            <Route path="*" element={<RouteInspector />} />
          </Route>
        </Routes>
      </MemoryRouter>
    </FluentProvider>,
  );
}

describe("consolidated project navigation", () => {
  it("has six primary entries and no links to invented resource IDs", () => {
    renderShell("knowledge");
    const nav = screen.getByRole("navigation");
    const groups = [
      "AI 工作台",
      "企业知识",
      "内容与计划",
      "发布与测量",
      "效果报告",
      "项目设置",
    ];
    for (const name of groups) {
      expect(within(nav).getByRole("link", { name })).toBeInTheDocument();
    }
    expect(within(nav).getAllByRole("link")).toHaveLength(6);
    expect(nav.querySelector('a[href*="demo-"]')).toBeNull();
    expect(within(nav).queryByRole("link", { name: "内容编辑器" })).toBeNull();
  });

  it.each([
    ["chat/conversation-a", "AI 工作台"],
    ["overview", "AI 工作台"],
    ["knowledge/sources/source-a", "企业知识"],
    ["knowledge/ask", "企业知识"],
    ["content/asset-a", "内容与计划"],
    ["campaigns/current", "内容与计划"],
    ["measurement", "发布与测量"],
    ["publications", "发布与测量"],
    ["reports/report-a", "效果报告"],
    ["setup", "项目设置"],
    ["channels/connect", "项目设置"],
    ["billing", "项目设置"],
  ])("marks %s under its accessible %s group", (path, label) => {
    renderShell(path);
    const nav = screen.getByRole("navigation");
    expect(within(nav).getByRole("link", { name: label })).toHaveAttribute(
      "aria-current",
      "location",
    );
    expect(nav.querySelectorAll('a[aria-current="location"]')).toHaveLength(1);
  });

  it("reaches real child pages and updates the active group on navigation and browser back", () => {
    renderShell("content/asset-a");
    const nav = screen.getByRole("navigation");
    fireEvent.click(within(nav).getByRole("link", { name: "当前计划与动作" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/campaigns/current",
    );
    fireEvent.click(within(nav).getByRole("link", { name: "发布与测量" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/publications",
    );
    expect(
      within(nav).getByRole("link", { name: "问题集与分析" }),
    ).toHaveAttribute("href", "/app/tenant-a/project-a/measurement");
    fireEvent.click(within(nav).getByRole("link", { name: "问题集与分析" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/measurement",
    );
    expect(
      within(nav).getByRole("link", { name: "发布与测量" }),
    ).toHaveAttribute("aria-current", "location");
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/publications",
    );
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(
      within(nav).getByRole("link", { name: "内容与计划" }),
    ).toHaveAttribute("aria-current", "location");
  });
});
