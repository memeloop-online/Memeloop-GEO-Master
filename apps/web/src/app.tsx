import { lazy, Suspense } from "react";
import { Navigate, Outlet, Route, Routes } from "react-router-dom";
import { RequireMembership, RequireSession } from "./auth/RequireSession";
import { LoadingState } from "./components/AsyncState";
import { LoginPage } from "./pages/LoginPage";
import { NotFoundPage } from "./pages/NotFoundPage";

const AppShell = lazy(() =>
  import("./components/AppShell").then((module) => ({
    default: module.AppShell,
  })),
);
const OverviewPage = lazy(() =>
  import("./pages/OverviewPage").then((module) => ({
    default: module.OverviewPage,
  })),
);
const KnowledgePage = lazy(() =>
  import("./pages/KnowledgePage").then((module) => ({
    default: module.KnowledgePage,
  })),
);
const AgentWorkbenchPage = lazy(() =>
  import("./pages/AgentWorkbenchPage").then((module) => ({
    default: module.AgentWorkbenchPage,
  })),
);
const SetupEntry = lazy(() =>
  import("./pages/SetupEntry").then((module) => ({
    default: module.SetupEntry,
  })),
);
const SetupPage = lazy(() =>
  import("./pages/SetupPage").then((module) => ({ default: module.SetupPage })),
);
const SourceDetailPage = lazy(() =>
  import("./pages/SourceDetailPage").then((module) => ({
    default: module.SourceDetailPage,
  })),
);
const WorkbenchPage = lazy(() =>
  import("./pages/WorkbenchPage").then((module) => ({
    default: module.WorkbenchPage,
  })),
);
const DocumentManifestPage = lazy(() =>
  import("./pages/DocumentManifestPage").then((module) => ({
    default: module.DocumentManifestPage,
  })),
);
const ChannelAccountsPage = lazy(() =>
  import("./pages/ChannelAccountsPage").then((module) => ({
    default: module.ChannelAccountsPage,
  })),
);
const OperatorAccountsPage = lazy(() =>
  import("./pages/ChannelAccountsPage").then((module) => ({
    default: module.OperatorAccountsPage,
  })),
);
const WorkspacePage = lazy(() =>
  import("./pages/WorkspacePage").then((module) => ({
    default: module.WorkspacePage,
  })),
);
const ReportsPage = lazy(() =>
  import("./pages/ReportsPage").then((module) => ({
    default: module.ReportsPage,
  })),
);
const ChannelJobsPage = lazy(() =>
  import("./pages/ChannelJobsPage").then((module) => ({
    default: module.ChannelJobsPage,
  })),
);
const QuestionSetsPage = lazy(() =>
  import("./pages/QuestionSetsPage").then((module) => ({
    default: module.QuestionSetsPage,
  })),
);
const ContentAssetPage = lazy(() =>
  import("./pages/ContentPages").then((module) => ({
    default: module.ContentAssetPage,
  })),
);
const ContentAssetsPage = lazy(() =>
  import("./pages/ContentPages").then((module) => ({
    default: module.ContentAssetsPage,
  })),
);
const OperatorAppearancePage = lazy(() =>
  import("./pages/OperatorAppearancePage").then((module) => ({
    default: module.OperatorAppearancePage,
  })),
);

function PageOutlet() {
  return (
    <Suspense fallback={<LoadingState />}>
      <Outlet />
    </Suspense>
  );
}

export function AppRoutes() {
  return (
    <Suspense fallback={<LoadingState />}>
      <Routes>
        <Route path="/login" element={<LoginPage />} />
        <Route element={<RequireSession />}>
          <Route path="/" element={<Navigate replace to="/workspaces" />} />
          <Route path="/workspaces" element={<WorkspacePage />} />
          <Route path="/ops/channels" element={<OperatorAccountsPage />} />
          <Route path="/ops/appearance" element={<OperatorAppearancePage />} />
          <Route path="/setup" element={<SetupEntry />} />
          <Route element={<RequireMembership />}>
            <Route path="/app/:tenantId/:projectId" element={<AppShell />}>
              <Route element={<PageOutlet />}>
                <Route index element={<Navigate replace to="chat" />} />
                <Route path="chat" element={<AgentWorkbenchPage />} />
                <Route
                  path="chat/:conversationId"
                  element={<AgentWorkbenchPage />}
                />
                <Route path="setup" element={<SetupPage />} />
                <Route path="overview" element={<OverviewPage />} />
                <Route path="knowledge" element={<KnowledgePage />} />
                <Route
                  path="knowledge/sources/:id"
                  element={<SourceDetailPage />}
                />
                <Route
                  path="campaigns"
                  element={<WorkbenchPage page="campaigns" />}
                />
                <Route
                  path="campaigns/:id"
                  element={<DocumentManifestPage />}
                />
                <Route path="content" element={<ContentAssetsPage />} />
                <Route path="content/:id" element={<ContentAssetPage />} />
                <Route
                  path="channels"
                  element={<ChannelAccountsPage view="channels" />}
                />
                <Route
                  path="channels/connect"
                  element={<ChannelAccountsPage view="connect" />}
                />
                <Route path="publications" element={<ChannelJobsPage />} />
                <Route path="measurement" element={<QuestionSetsPage />} />
                <Route path="reports" element={<ReportsPage />} />
                <Route path="reports/:id" element={<ReportsPage />} />
                <Route
                  path="billing"
                  element={<WorkbenchPage page="billing" />}
                />
                <Route
                  path="settings"
                  element={<ChannelAccountsPage view="settings" />}
                />
              </Route>
            </Route>
          </Route>
        </Route>
        <Route path="*" element={<NotFoundPage />} />
      </Routes>
    </Suspense>
  );
}
