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
  BookInformationRegular,
  CalendarLtrRegular,
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
import { NavLink, Outlet, useParams } from "react-router-dom";

type NavItem = { to: string; label: string; code: string; icon: ReactElement };

const navItems: NavItem[] = [
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
  { to: "ask", label: "企业问答", code: "P05", icon: <SearchRegular /> },
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
  const { tenantId = "acme", projectId = "northstar" } = useParams();
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
              icon={<Avatar name="N" color="brand" size={28} />}
              iconPosition="before"
            >
              <span>
                <b>Northstar AI 助手</b>
                <small>
                  {tenantId} / {projectId}
                </small>
              </span>
              <ChevronDownRegular />
            </Button>
          </MenuTrigger>
          <MenuPopover>
            <MenuList>
              <MenuItem>Northstar AI 助手（当前）</MenuItem>
              <MenuItem disabled>切换项目（即将提供）</MenuItem>
            </MenuList>
          </MenuPopover>
        </Menu>
        <div className="topbar-actions">
          <Button appearance="subtle">帮助</Button>
          <Avatar name="林" color="colorful" aria-label="当前用户" />
        </div>
      </header>
      <main className="page-content">
        <Outlet />
      </main>
    </div>
  );
}
