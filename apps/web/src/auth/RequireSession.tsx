import { Button } from "@fluentui/react-components";
import {
  Navigate,
  Outlet,
  useLocation,
  useNavigate,
  useParams,
} from "react-router-dom";
import {
  ErrorState,
  LoadingState,
  UnauthorizedState,
} from "../components/AsyncState";
import { useAuth } from "./AuthProvider";
import { membershipForTenant } from "./types";

function safeReturnTo(pathname: string, search: string, hash: string) {
  const value = `${pathname}${search}${hash}`;
  return value.startsWith("/") && !value.startsWith("//")
    ? value
    : "/workspaces";
}

export function RequireSession() {
  const { status, error, refresh } = useAuth();
  const location = useLocation();

  if (status === "checking") return <LoadingState label="正在验证登录状态" />;
  if (status === "unavailable") {
    return (
      <main className="guard-page">
        <ErrorState
          title="身份服务暂时不可用"
          detail={error?.message ?? "无法确认登录状态，请稍后重试。"}
          onRetry={() => void refresh()}
        />
      </main>
    );
  }
  if (status === "anonymous") {
    return (
      <Navigate
        replace
        to={`/login?returnTo=${encodeURIComponent(safeReturnTo(location.pathname, location.search, location.hash))}`}
      />
    );
  }
  return <Outlet />;
}

export function RequireMembership() {
  const { session } = useAuth();
  const { tenantId } = useParams();
  const navigate = useNavigate();
  const membership = membershipForTenant(session, tenantId);

  if (!membership) {
    return (
      <main className="guard-page">
        <UnauthorizedState
          tenantName={tenantId}
          action={
            <Button
              onClick={() => navigate("/workspaces")}
              appearance="primary"
            >
              返回工作区
            </Button>
          }
        />
      </main>
    );
  }
  return <Outlet />;
}
