import { Navigate, Route, Routes } from "react-router-dom";
import { RequireMembership, RequireSession } from "./auth/RequireSession";
import { AppShell } from "./components/AppShell";
import { LoginPage } from "./pages/LoginPage";
import { NotFoundPage } from "./pages/NotFoundPage";
import { OverviewPage } from "./pages/OverviewPage";
import { KnowledgeAskPage } from "./pages/KnowledgeAskPage";
import { KnowledgePage } from "./pages/KnowledgePage";
import { AgentWorkbenchPage } from "./pages/AgentWorkbenchPage";
import { SetupEntry } from "./pages/SetupEntry";
import { SetupPage } from "./pages/SetupPage";
import { SourceDetailPage } from "./pages/SourceDetailPage";
import { WorkbenchPage } from "./pages/WorkbenchPage";
import { DocumentManifestPage } from "./pages/DocumentManifestPage";
import {
  ChannelAccountsPage,
  OperatorAccountsPage,
} from "./pages/ChannelAccountsPage";
import { WorkspacePage } from "./pages/WorkspacePage";
import { ReportsPage } from "./pages/ReportsPage";
import { ChannelJobsPage } from "./pages/ChannelJobsPage";

export function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
      <Route element={<RequireSession />}>
        <Route path="/" element={<Navigate replace to="/workspaces" />} />
        <Route path="/workspaces" element={<WorkspacePage />} />
        <Route path="/ops/channels" element={<OperatorAccountsPage />} />
        <Route path="/setup" element={<SetupEntry />} />
        <Route element={<RequireMembership />}>
          <Route path="/app/:tenantId/:projectId" element={<AppShell />}>
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
            <Route path="knowledge/ask" element={<KnowledgeAskPage />} />
            <Route
              path="ask"
              element={<Navigate replace to="../knowledge/ask" />}
            />
            <Route
              path="campaigns"
              element={<WorkbenchPage page="campaigns" />}
            />
            <Route path="campaigns/:id" element={<DocumentManifestPage />} />
            <Route path="content" element={<WorkbenchPage page="content" />} />
            <Route
              path="content/:id"
              element={<WorkbenchPage page="contentDetail" />}
            />
            <Route
              path="channels"
              element={<ChannelAccountsPage view="channels" />}
            />
            <Route
              path="channels/connect"
              element={<ChannelAccountsPage view="connect" />}
            />
            <Route path="publications" element={<ChannelJobsPage />} />
            <Route
              path="measurement"
              element={<WorkbenchPage page="measurement" />}
            />
            <Route path="reports" element={<ReportsPage />} />
            <Route path="reports/:id" element={<ReportsPage />} />
            <Route path="billing" element={<WorkbenchPage page="billing" />} />
            <Route
              path="settings"
              element={<ChannelAccountsPage view="settings" />}
            />
          </Route>
        </Route>
      </Route>
      <Route path="*" element={<NotFoundPage />} />
    </Routes>
  );
}
