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
  ArrowExitRegular,
  BookInformationRegular,
  ChatRegular,
  ChevronDownRegular,
  DataUsageRegular,
  DocumentDataRegular,
  PanelLeftContractRegular,
  PanelLeftExpandRegular,
  SettingsRegular,
  TextBulletListSquareRegular,
} from "@fluentui/react-icons";
import {
  Link,
  NavLink,
  Outlet,
  useLocation,
  useNavigate,
  useParams,
} from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { useProjectsQuery } from "../api/projects";
import { ErrorState, LoadingState } from "./AsyncState";

type NavGroup = {
  id: string;
  to: string;
  label: string;
  icon: ReactElement;
  children: { to: string; label: string }[];
};

const navGroups: NavGroup[] = [
  {
    id: "chat",
    to: "chat",
    label: "AI 工作台",
    icon: <ChatRegular />,
    children: [{ to: "overview", label: "项目总览" }],
  },
  {
    id: "knowledge",
    to: "knowledge",
    label: "企业知识",
    icon: <BookInformationRegular />,
    children: [],
  },
  {
    id: "content",
    to: "content",
    label: "内容与计划",
    icon: <TextBulletListSquareRegular />,
    children: [{ to: "campaigns/current", label: "当前计划与动作" }],
  },
  {
    id: "measurement",
    to: "publications",
    label: "发布与测量",
    icon: <DataUsageRegular />,
    children: [{ to: "measurement", label: "问题集与分析" }],
  },
  {
    id: "reports",
    to: "reports",
    label: "效果报告",
    icon: <DocumentDataRegular />,
    children: [],
  },
  {
    id: "settings",
    to: "settings",
    label: "项目设置",
    icon: <SettingsRegular />,
    children: [
      { to: "setup", label: "项目配置" },
      { to: "channels", label: "渠道账号" },
    ],
  },
];

function groupForPath(pathname: string): string | undefined {
  const section = pathname.split("/")[4];
  if (!section || section === "chat" || section === "overview") return "chat";
  if (section === "knowledge" || section === "ask") return "knowledge";
  if (section === "content" || section === "campaigns") return "content";
  if (section === "measurement" || section === "publications")
    return "measurement";
  if (section === "reports") return "reports";
  if (["settings", "setup", "channels", "billing"].includes(section))
    return "settings";
  return undefined;
}

export function AppShell() {
  const [collapsed, setCollapsed] = useState(
    () => window.matchMedia?.("(max-width: 767px)").matches ?? false,
  );
  const [loggingOut, setLoggingOut] = useState(false);
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const activeGroup = groupForPath(pathname);
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
          {navGroups.map((group) => (
            <div className="nav-group" key={group.id}>
              <Tooltip
                content={group.label}
                relationship="label"
                positioning="after"
                visible={collapsed ? undefined : false}
              >
                <Link
                  to={group.to}
                  className={`nav-link${activeGroup === group.id ? " active" : ""}`}
                  aria-current={
                    activeGroup === group.id ? "location" : undefined
                  }
                  aria-label={group.label}
                >
                  <span className="nav-icon">{group.icon}</span>
                  {!collapsed && <span>{group.label}</span>}
                </Link>
              </Tooltip>
              {!collapsed &&
                activeGroup === group.id &&
                group.children.length > 0 && (
                  <div
                    className="nav-children"
                    aria-label={`${group.label}详情`}
                  >
                    {group.children.map((child) => (
                      <NavLink
                        key={child.to}
                        to={child.to}
                        end
                        className={({ isActive }) =>
                          `nav-child-link${isActive ? " active" : ""}`
                        }
                      >
                        {child.label}
                      </NavLink>
                    ))}
                  </div>
                )}
            </div>
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
