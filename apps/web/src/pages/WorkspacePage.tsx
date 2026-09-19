import { Button, Card, CardHeader } from "@fluentui/react-components";
import {
  AddRegular,
  ArrowRightRegular,
  BuildingRegular,
} from "@fluentui/react-icons";
import { useLocation, useNavigate } from "react-router-dom";
import { useProjectsQuery } from "../api/projects";
import { useAuth } from "../auth/AuthProvider";
import { EmptyState, ErrorState, LoadingState } from "../components/AsyncState";
import type { TenantMembership } from "../auth/types";

function setupHref(tenantId: string, returnTo: string | null) {
  const query = new URLSearchParams({ tenant_id: tenantId });
  if (returnTo?.startsWith("/") && !returnTo.startsWith("//")) {
    query.set("returnTo", returnTo);
  }
  return `/setup?${query.toString()}`;
}

export function WorkspacePage() {
  const { session } = useAuth();
  const location = useLocation();
  const returnTo = new URLSearchParams(location.search).get("returnTo");
  const memberships = session?.memberships ?? [];

  if (memberships.length === 0) {
    return (
      <main className="workspace-page">
        <EmptyState
          title="还没有可用工作区"
          detail="请联系工作区管理员，为你的账号分配客户组织。"
        />
      </main>
    );
  }

  return (
    <main className="workspace-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">工作区</p>
          <h1>选择客户工作区</h1>
          <p>工作区决定项目、资料、费用和测量数据的租户边界。</p>
        </div>
      </section>
      <section className="workspace-grid" aria-label="已授权工作区">
        {memberships.map((membership) => (
          <WorkspaceCard
            key={membership.tenant_id}
            membership={membership}
            returnTo={returnTo}
          />
        ))}
      </section>
    </main>
  );
}

function WorkspaceCard({
  membership,
  returnTo,
}: {
  membership: TenantMembership;
  returnTo: string | null;
}) {
  const navigate = useNavigate();
  const {
    data: projects,
    isPending,
    isError,
    refetch,
  } = useProjectsQuery(membership.tenant_id);

  return (
    <Card className="workspace-card">
      <CardHeader
        image={<BuildingRegular aria-hidden="true" fontSize={28} />}
        header={
          <div>
            <h2>{membership.tenant_display_name}</h2>
            <p>{membership.tenant_slug}</p>
          </div>
        }
      />
      <p>角色：{membership.role}</p>
      {isPending && <LoadingState compact label="正在加载项目" />}
      {isError && (
        <ErrorState
          title="无法加载项目"
          detail="你仍可创建新项目。"
          onRetry={() => void refetch()}
          intent="warning"
        />
      )}
      {!isPending && !isError && projects?.items.length === 0 && (
        <p className="workspace-project-empty">该工作区还没有项目。</p>
      )}
      {projects && projects.items.length > 0 && (
        <div
          className="workspace-project-list"
          aria-label={`${membership.tenant_display_name} 的项目`}
        >
          {projects.items.map((project) => (
            <Button
              key={project.id}
              onClick={() =>
                navigate(`/app/${membership.tenant_id}/${project.id}/chat`)
              }
              appearance="secondary"
              icon={<ArrowRightRegular />}
            >
              {project.display_name}
            </Button>
          ))}
        </div>
      )}
      <div className="workspace-actions">
        <Button
          onClick={() => navigate(setupHref(membership.tenant_id, returnTo))}
          appearance="primary"
          icon={<AddRegular />}
        >
          创建项目
        </Button>
      </div>
    </Card>
  );
}
