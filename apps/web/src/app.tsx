import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "./components/AppShell";
import { SetupPage } from "./pages/SetupPage";
import { OverviewPage } from "./pages/OverviewPage";
import { WorkbenchPage } from "./pages/WorkbenchPage";
import { NotFoundPage } from "./pages/NotFoundPage";

export function AppRoutes() {
  return (
    <Routes>
      <Route
        path="/"
        element={<Navigate replace to="/app/acme/northstar/overview" />}
      />
      <Route path="/app/:tenantId/:projectId" element={<AppShell />}>
        <Route index element={<Navigate replace to="overview" />} />
        <Route path="setup" element={<SetupPage />} />
        <Route path="overview" element={<OverviewPage />} />
        <Route path="knowledge" element={<WorkbenchPage page="knowledge" />} />
        <Route
          path="knowledge/sources/:id"
          element={<WorkbenchPage page="source" />}
        />
        <Route path="ask" element={<WorkbenchPage page="ask" />} />
        <Route path="campaigns" element={<WorkbenchPage page="campaigns" />} />
        <Route
          path="campaigns/:id"
          element={<WorkbenchPage page="campaign" />}
        />
        <Route path="content" element={<WorkbenchPage page="content" />} />
        <Route
          path="content/:id"
          element={<WorkbenchPage page="contentDetail" />}
        />
        <Route path="channels" element={<WorkbenchPage page="channels" />} />
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
        <Route path="settings" element={<WorkbenchPage page="settings" />} />
      </Route>
      <Route path="*" element={<NotFoundPage />} />
    </Routes>
  );
}
