import { Button } from "@fluentui/react-components";
import { useNavigate, useSearchParams } from "react-router-dom";
import { useAuth } from "../auth/AuthProvider";
import { membershipForTenant } from "../auth/types";
import { UnauthorizedState } from "../components/AsyncState";
import { SetupPage } from "./SetupPage";
import { WorkspacePage } from "./WorkspacePage";

export function SetupEntry() {
  const { session } = useAuth();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const tenantId = searchParams.get("tenant_id") ?? undefined;

  if (!tenantId) return <WorkspacePage />;
  if (!membershipForTenant(session, tenantId)) {
    return (
      <main className="guard-page">
        <UnauthorizedState
          tenantName={tenantId}
          action={
            <Button
              onClick={() => navigate("/workspaces")}
              appearance="primary"
            >
              选择其他工作区
            </Button>
          }
        />
      </main>
    );
  }
  return <SetupPage tenantId={tenantId} />;
}
