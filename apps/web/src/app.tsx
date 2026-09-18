import { Navigate, Route, Routes } from "react-router-dom";
import { RequireMembership, RequireSession } from "./auth/RequireSession";
import { AppShell } from "./components/AppShell";
import { LoginPage } from "./pages/LoginPage";
import { NotFoundPage } from "./pages/NotFoundPage";
import { OverviewPage } from "./pages/OverviewPage";
import { SetupEntry } from "./pages/SetupEntry";
import { SetupPage } from "./pages/SetupPage";
import { WorkbenchPage } from "./pages/WorkbenchPage";
import { WorkspacePage } from "./pages/WorkspacePage";

export function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
      <Route element={<RequireSession />}>
        <Route path="/" element={<Navigate replace to="/workspaces" />} />
        <Route path="/workspaces" element={<WorkspacePage />} />
        <Route path="/setup" element={<SetupEntry />} />
        <Route element={<RequireMembership />}>
          <Route path="/app/:tenantId/:projectId" element={<AppShell />}>
            <Route index element={<Navigate replace to="overview" />} />
            <Route path="setup" element={<SetupPage />} />
            <Route path="overview" element={<OverviewPage />} />
            <Route
              path="knowledge"
              element={<WorkbenchPage page="knowledge" />}
            />
            <Route
              path="knowledge/sources/:id"
              element={<WorkbenchPage page="source" />}
            />
            <Route
              path="knowledge/ask"
              element={<WorkbenchPage page="ask" />}
            />
            <Route
              path="ask"
              element={<Navigate replace to="../knowledge/ask" />}
            />
            <Route
              path="campaigns"
              element={<WorkbenchPage page="campaigns" />}
            />
            <Route
              path="campaigns/:id"
              element={<WorkbenchPage page="campaign" />}
            />
            <Route path="content" element={<WorkbenchPage page="content" />} />
            <Route
              path="content/:id"
              element={<WorkbenchPage page="contentDetail" />}
            />
            <Route
              path="channels"
              element={<WorkbenchPage page="channels" />}
            />
            <Route
              path="channels/connect"
              element={<WorkbenchPage page="connect" />}
            />
            <Route
              path="publications"
              element={<WorkbenchPage page="publications" />}
            />
            <Route
              path="measurement"
              element={<WorkbenchPage page="measurement" />}
            />
            <Route path="reports" element={<WorkbenchPage page="reports" />} />
            <Route path="billing" element={<WorkbenchPage page="billing" />} />
            <Route
              path="settings"
              element={<WorkbenchPage page="settings" />}
            />
          </Route>
        </Route>
      </Route>
      <Route path="*" element={<NotFoundPage />} />
    </Routes>
  );
}
