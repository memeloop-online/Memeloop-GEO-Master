import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
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
  it("has exactly five primary entries in the requested order and no invented resource IDs", () => {
    renderShell("knowledge");
    const nav = screen.getByRole("navigation");
    const groups = [
      "AI 工作台",
      "测量与洞察",
      "内容与发布",
      "企业知识",
      "项目设置",
    ];
    expect(
      within(nav)
        .getAllByRole("link")
        .map((link) => link.getAttribute("aria-label")),
    ).toEqual(groups);
    expect(nav.querySelector('a[href*="demo-"]')).toBeNull();
    expect(within(nav).queryByRole("link", { name: "内容编辑器" })).toBeNull();
  });

  it.each([
    ["chat/conversation-a", "AI 工作台"],
    ["overview", "AI 工作台"],
    ["knowledge/sources/source-a", "企业知识"],
    ["knowledge/ask", "企业知识"],
    ["content/asset-a", "内容与发布"],
    ["campaigns/current", "内容与发布"],
    ["measurement", "测量与洞察"],
    ["publications", "内容与发布"],
    ["reports/report-a", "测量与洞察"],
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
    fireEvent.click(within(nav).getByRole("link", { name: "发布目标与执行" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/publications",
    );
    expect(
      within(nav).getByRole("link", { name: "内容与发布" }),
    ).toHaveAttribute("aria-current", "location");
    fireEvent.click(within(nav).getByRole("link", { name: "测量与洞察" }));
    expect(
      within(nav).getByRole("link", { name: "独立测量与问题集" }),
    ).toHaveAttribute("href", "/app/tenant-a/project-a/measurement");
    expect(within(nav).getByRole("link", { name: "效果报告" })).toHaveAttribute(
      "href",
      "/app/tenant-a/project-a/reports",
    );
    fireEvent.click(within(nav).getByRole("link", { name: "效果报告" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/reports",
    );
    fireEvent.click(
      within(nav).getByRole("link", { name: "独立测量与问题集" }),
    );
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/measurement",
    );
    expect(
      within(nav).getByRole("link", { name: "测量与洞察" }),
    ).toHaveAttribute("aria-current", "location");
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/reports",
    );
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/measurement",
    );
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(screen.getByTestId("path")).toHaveTextContent(
      "/app/tenant-a/project-a/publications",
    );
    fireEvent.click(screen.getByRole("button", { name: "返回" }));
    expect(
      within(nav).getByRole("link", { name: "内容与发布" }),
    ).toHaveAttribute("aria-current", "location");
  });

  it("starts narrow layouts collapsed, closes after navigation, and can dismiss expanded navigation", () => {
    const consoleError = vi.spyOn(console, "error");
    const listeners = new Set<(event: MediaQueryListEvent) => void>();
    let narrow = true;
    vi.stubGlobal(
      "matchMedia",
      vi.fn(() => ({
        get matches() {
          return narrow;
        },
        addEventListener: (
          _: string,
          listener: (event: MediaQueryListEvent) => void,
        ) => listeners.add(listener),
        removeEventListener: (
          _: string,
          listener: (event: MediaQueryListEvent) => void,
        ) => listeners.delete(listener),
      })),
    );
    try {
      renderShell("content/asset-a");
      const nav = screen.getByRole("navigation");
      expect(
        screen.getByRole("button", { name: "展开导航栏" }),
      ).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "展开导航栏" }));
      expect(
        screen.getByRole("button", { name: "关闭导航栏" }),
      ).toBeInTheDocument();
      fireEvent.click(screen.getByRole("link", { name: "发布目标与执行" }));
      expect(screen.getByTestId("path")).toHaveTextContent(
        "/app/tenant-a/project-a/publications",
      );
      expect(
        screen.getByRole("button", { name: "展开导航栏" }),
      ).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "展开导航栏" }));
      fireEvent.click(screen.getByRole("button", { name: "关闭导航栏" }));
      expect(
        screen.getByRole("button", { name: "展开导航栏" }),
      ).toBeInTheDocument();
      narrow = false;
      fireEvent.click(screen.getByRole("button", { name: "展开导航栏" }));
      narrow = true;
      act(() => {
        for (const listener of listeners)
          listener({ matches: true } as MediaQueryListEvent);
      });
      expect(
        screen.getByRole("button", { name: "展开导航栏" }),
      ).toBeInTheDocument();
      expect(
        within(nav).getByRole("link", { name: "内容与发布" }),
      ).toHaveAttribute("aria-current", "location");
      expect(
        consoleError.mock.calls.some(([message]) =>
          String(message).includes(
            "@fluentui/react-utilities [useControllableState]",
          ),
        ),
      ).toBe(false);
    } finally {
      consoleError.mockRestore();
      vi.unstubAllGlobals();
    }
  });
});
