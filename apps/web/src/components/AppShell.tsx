import { useState, type ReactElement } from "react";
import {
  Avatar,
  Button,
  Menu,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  Tooltip,
} from "@fluentui/react-components";
import {
  AddCircleRegular,
  ArrowExitRegular,
  BookInformationRegular,
  CalendarLtrRegular,
  ChatRegular,
  ChevronDownRegular,
  DataUsageRegular,
  DocumentBulletListRegular,
  DocumentDataRegular,
  HomeRegular,
  LightbulbRegular,
  MoneyRegular,
  PanelLeftContractRegular,
  PanelLeftExpandRegular,
  PlugConnectedRegular,
  SearchRegular,
  SendRegular,
  SettingsRegular,
  TextBulletListSquareRegular,
} from "@fluentui/react-icons";
import { NavLink, Outlet, useNavigate, useParams } from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { useProjectsQuery } from "../api/projects";
import { ErrorState, LoadingState } from "./AsyncState";

type NavItem = { to: string; label: string; code: string; icon: ReactElement };

const navItems: NavItem[] = [
  {
    to: "chat",
    label: "AI 工作台",
    code: "P00",
    icon: <ChatRegular />,
  },
  {
    to: "setup",
    label: "项目设置向导",
    code: "P01",
    icon: <SettingsRegular />,
  },
  { to: "overview", label: "项目总览", code: "P02", icon: <HomeRegular /> },
  {
    to: "knowledge",
    label: "企业知识库",
    code: "P03",
    icon: <BookInformationRegular />,
  },
  {
    to: "knowledge/sources/demo-source",
    label: "来源与证据",
    code: "P04",
    icon: <DocumentDataRegular />,
  },
  {
    to: "knowledge/ask",
    label: "企业问答",
    code: "P05",
    icon: <SearchRegular />,
  },
  {
    to: "campaigns",
    label: "优化计划",
    code: "P06",
    icon: <LightbulbRegular />,
  },
  {
    to: "campaigns/demo-plan",
    label: "计划与动作",
    code: "P07",
    icon: <CalendarLtrRegular />,
  },
  {
    to: "content",
    label: "内容资产",
    code: "P08",
    icon: <TextBulletListSquareRegular />,
  },
  {
    to: "content/demo-content",
    label: "内容编辑器",
    code: "P09",
    icon: <DocumentBulletListRegular />,
  },
  {
    to: "channels",
    label: "渠道与资源",
    code: "P10",
    icon: <PlugConnectedRegular />,
  },
  {
    to: "channels/connect",
    label: "批量接入",
    code: "P11",
    icon: <AddCircleRegular />,
  },
  {
    to: "publications",
    label: "发布与验证",
    code: "P12",
    icon: <SendRegular />,
  },
  {
    to: "measurement",
    label: "基线与测量",
    code: "P13",
    icon: <DataUsageRegular />,
  },
  {
    to: "reports",
    label: "效果报告",
    code: "P14",
    icon: <DocumentDataRegular />,
  },
  { to: "billing", label: "预算与账本", code: "P15", icon: <MoneyRegular /> },
  {
    to: "settings",
    label: "项目设置",
    code: "P16",
    icon: <CalendarLtrRegular />,
  },
];

export function AppShell() {
  const [collapsed, setCollapsed] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);
  const navigate = useNavigate();
  const { tenantId, projectId } = useParams();
  const { session, logout } = useAuth();
  const {
    data: projects,
    isPending,
    isError,
    refetch,
  } = useProjectsQuery(tenantId);
  const membership = membershipForTenant(session, tenantId);
  const currentProject = projects?.items.find(
    (project) => project.id === projectId,
  );
  const projectLabel = isPending
    ? "正在加载项目"
    : (currentProject?.display_name ??
      (isError ? "项目列表暂时不可用" : "未找到项目"));
  const projectMeta = currentProject
    ? `${currentProject.settings.market} · ${currentProject.settings.language}`
    : (membership?.tenant_display_name ?? tenantId);

  async function handleLogout() {
    setLoggingOut(true);
    try {
      await logout();
      navigate("/login", { replace: true });
    } finally {
      setLoggingOut(false);
    }
  }

  return (
    <div className={`app-shell ${collapsed ? "sidebar-collapsed" : ""}`}>
      <aside className="sidebar" aria-label="主导航">
        <div className="brand">
          <div className="brand-mark">M</div>
          {!collapsed && (
            <span>
              Memeloop <b>GEO</b>
            </span>
          )}
        </div>
        <nav className="side-nav">
          {navItems.map((item) => (
            <Tooltip
              key={item.to}
              content={`${item.code} · ${item.label}`}
              relationship="label"
              positioning="after"
              visible={collapsed ? undefined : false}
            >
              <NavLink
                to={item.to}
                className={({ isActive }) =>
                  `nav-link${isActive ? " active" : ""}`
                }
                end={item.to === "overview"}
              >
                <span className="nav-icon">{item.icon}</span>
                {!collapsed && (
                  <>
                    <span>{item.label}</span>
                    <small>{item.code}</small>
                  </>
                )}
              </NavLink>
            </Tooltip>
          ))}
        </nav>
        <Button
          className="collapse-button"
          appearance="subtle"
          icon={
            collapsed ? (
              <PanelLeftExpandRegular />
            ) : (
              <PanelLeftContractRegular />
            )
          }
          onClick={() => setCollapsed((value) => !value)}
          aria-label={collapsed ? "展开导航栏" : "折叠导航栏"}
        />
      </aside>
      <header className="topbar">
        <Menu>
          <MenuTrigger disableButtonEnhancement>
            <Button
              appearance="subtle"
              className="project-switcher"
              icon={<Avatar name={projectLabel} color="brand" size={28} />}
              iconPosition="before"
            >
              <span>
                <b>{projectLabel}</b>
                <small>{projectMeta}</small>
              </span>
              <ChevronDownRegular />
            </Button>
          </MenuTrigger>
          <MenuPopover>
            <MenuList>
              {isPending && <MenuItem disabled>正在加载项目…</MenuItem>}
              {projects?.items.map((project) => (
                <MenuItem
                  key={project.id}
                  onClick={() =>
                    navigate(`/app/${tenantId}/${project.id}/chat`)
                  }
                >
                  {project.display_name}
                  {project.id === projectId ? "（当前）" : ""}
                </MenuItem>
              ))}
              {!isPending && !isError && projects?.items.length === 0 && (
                <MenuItem disabled>当前工作区还没有项目</MenuItem>
              )}
              {projects?.next_cursor && (
                <MenuItem disabled>仅显示前 50 个项目</MenuItem>
              )}
              {isError && (
                <MenuItem onClick={() => void refetch()}>重新加载项目</MenuItem>
              )}
              <MenuItem
                onClick={() => navigate(`/setup?tenant_id=${tenantId}`)}
              >
                创建项目
              </MenuItem>
              <MenuItem onClick={() => navigate("/workspaces")}>
                切换工作区
              </MenuItem>
            </MenuList>
          </MenuPopover>
        </Menu>
        <div className="topbar-actions">
          <Button appearance="subtle">帮助</Button>
          <Menu>
            <MenuTrigger disableButtonEnhancement>
              <Button
                appearance="subtle"
                icon={
                  <Avatar
                    name={session?.user.display_name ?? "用户"}
                    color="colorful"
                  />
                }
                aria-label="用户菜单"
              >
                <span className="user-menu-name">
                  {session?.user.display_name}
                </span>
              </Button>
            </MenuTrigger>
            <MenuPopover>
              <MenuList>
                <MenuItem disabled>{session?.user.login_name}</MenuItem>
                <MenuItem onClick={() => navigate("/workspaces")}>
                  切换工作区
                </MenuItem>
                <MenuItem
                  icon={<ArrowExitRegular />}
                  disabled={loggingOut}
                  onClick={() => void handleLogout()}
                >
                  {loggingOut ? "正在退出…" : "退出登录"}
                </MenuItem>
              </MenuList>
            </MenuPopover>
          </Menu>
        </div>
      </header>
      <main className="page-content">
        {isError && (
          <ErrorState
            title="项目切换列表暂时不可用"
            detail="当前页面仍可使用；请重试以切换项目。"
            onRetry={() => void refetch()}
            intent="warning"
          />
        )}
        {isPending && <LoadingState compact label="正在加载项目工作区" />}
        <Outlet />
      </main>
    </div>
  );
}
