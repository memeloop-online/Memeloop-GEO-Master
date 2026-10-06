import { useEffect, useState, type ReactElement } from "react";
import { useTranslation } from "react-i18next";
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
import { Brand } from "./Brand";
import { LanguageSelect } from "./LanguageSelect";

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
    id: "measurement",
    to: "measurement",
    label: "测量与洞察",
    icon: <DataUsageRegular />,
    children: [
      { to: "measurement", label: "独立测量与问题集" },
      { to: "reports", label: "效果报告" },
    ],
  },
  {
    id: "content",
    to: "content",
    label: "内容与发布",
    icon: <TextBulletListSquareRegular />,
    children: [
      { to: "campaigns/current", label: "当前计划与动作" },
      { to: "publications", label: "发布目标与执行" },
    ],
  },
  {
    id: "knowledge",
    to: "knowledge",
    label: "企业知识",
    icon: <BookInformationRegular />,
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
const childLabelKeys: Record<string, string> = {
  overview: "overview",
  measurement: "standalone",
  reports: "reports",
  "campaigns/current": "campaigns",
  publications: "publications",
  setup: "setup",
  channels: "channels",
};

function groupForPath(pathname: string): string | undefined {
  const section = pathname.split("/")[4];
  if (!section || section === "chat" || section === "overview") return "chat";
  if (section === "knowledge" || section === "ask") return "knowledge";
  if (section === "content" || section === "campaigns") return "content";
  if (section === "publications") return "content";
  if (section === "measurement" || section === "reports") return "measurement";
  if (["settings", "setup", "channels", "billing"].includes(section))
    return "settings";
  return undefined;
}

export function AppShell() {
  const { t } = useTranslation();
  const [collapsed, setCollapsed] = useState(
    () => window.matchMedia?.("(max-width: 767px)")?.matches ?? false,
  );
  const [loggingOut, setLoggingOut] = useState(false);
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const activeGroup = groupForPath(pathname);
  useEffect(() => {
    const narrow = window.matchMedia?.("(max-width: 767px)");
    if (!narrow) return;
    const onChange = (event: MediaQueryListEvent) => {
      if (event.matches) setCollapsed(true);
    };
    narrow.addEventListener?.("change", onChange);
    return () => narrow.removeEventListener?.("change", onChange);
  }, []);
  useEffect(() => {
    if (window.matchMedia?.("(max-width: 767px)")?.matches) setCollapsed(true);
  }, [pathname]);
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
    ? t("shell.projectLoading")
    : (currentProject?.display_name ??
      (isError ? t("shell.projectUnavailable") : t("shell.projectMissing")));
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
      {!collapsed && (
        <button
          type="button"
          className="sidebar-dismiss"
          aria-label={t("shell.closeNavigation")}
          onClick={() => setCollapsed(true)}
        />
      )}
      <aside className="sidebar" aria-label={t("shell.navigation")}>
        <Brand collapsed={collapsed} />
        <nav className="side-nav">
          {navGroups.map((group) => {
            const groupLabel = t(`navigation.${group.id}`, {
              defaultValue: group.label,
            });
            const groupLink = (
              <Link
                to={group.to}
                className={`nav-link${activeGroup === group.id ? " active" : ""}`}
                aria-current={activeGroup === group.id ? "location" : undefined}
                aria-label={groupLabel}
              >
                <span className="nav-icon">{group.icon}</span>
                {!collapsed && <span>{groupLabel}</span>}
              </Link>
            );
            return (
              <div className="nav-group" key={group.id}>
                {collapsed ? (
                  <Tooltip
                    content={groupLabel}
                    relationship="label"
                    positioning="after"
                  >
                    {groupLink}
                  </Tooltip>
                ) : (
                  groupLink
                )}
                {!collapsed &&
                  activeGroup === group.id &&
                  group.children.length > 0 && (
                    <div
                      className="nav-children"
                      aria-label={t("shell.groupDetails", {
                        group: groupLabel,
                      })}
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
                          {t(
                            `navigation.${childLabelKeys[child.to] ?? child.to}`,
                            {
                              defaultValue: child.label,
                            },
                          )}
                        </NavLink>
                      ))}
                    </div>
                  )}
              </div>
            );
          })}
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
          aria-label={
            collapsed
              ? t("shell.expandNavigation")
              : t("shell.collapseNavigation")
          }
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
              {isPending && (
                <MenuItem disabled>{t("shell.loadingProjects")}</MenuItem>
              )}
              {projects?.items.map((project) => (
                <MenuItem
                  key={project.id}
                  onClick={() =>
                    navigate(`/app/${tenantId}/${project.id}/chat`)
                  }
                >
                  {project.display_name}
                  {project.id === projectId ? t("shell.current") : ""}
                </MenuItem>
              ))}
              {!isPending && !isError && projects?.items.length === 0 && (
                <MenuItem disabled>{t("shell.noProjects")}</MenuItem>
              )}
              {projects?.next_cursor && (
                <MenuItem disabled>{t("shell.firstFifty")}</MenuItem>
              )}
              {isError && (
                <MenuItem onClick={() => void refetch()}>
                  {t("shell.reloadProjects")}
                </MenuItem>
              )}
              <MenuItem
                onClick={() => navigate(`/setup?tenant_id=${tenantId}`)}
              >
                {t("shell.createProject")}
              </MenuItem>
              <MenuItem onClick={() => navigate("/workspaces")}>
                {t("shell.switchWorkspace")}
              </MenuItem>
            </MenuList>
          </MenuPopover>
        </Menu>
        <div className="topbar-actions">
          <LanguageSelect />
          <Button appearance="subtle">{t("shell.help")}</Button>
          <Menu>
            <MenuTrigger disableButtonEnhancement>
              <Button
                appearance="subtle"
                icon={
                  <Avatar
                    name={session?.user.display_name ?? t("shell.user")}
                    color="colorful"
                  />
                }
                aria-label={t("shell.userMenu")}
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
                  {t("shell.switchWorkspace")}
                </MenuItem>
                <MenuItem
                  icon={<ArrowExitRegular />}
                  disabled={loggingOut}
                  onClick={() => void handleLogout()}
                >
                  {loggingOut ? t("shell.loggingOut") : t("shell.logout")}
                </MenuItem>
              </MenuList>
            </MenuPopover>
          </Menu>
        </div>
      </header>
      <main className="page-content">
        {isError && (
          <ErrorState
            title={t("shell.projectSwitchUnavailable")}
            detail={t("shell.projectSwitchDetail")}
            onRetry={() => void refetch()}
            intent="warning"
          />
        )}
        {isPending && (
          <LoadingState compact label={t("shell.loadingWorkspace")} />
        )}
        <Outlet />
      </main>
    </div>
  );
}
