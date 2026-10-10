import { Button, Card, CardHeader } from "@fluentui/react-components";
import {
  AddRegular,
  ArrowRightRegular,
  BuildingRegular,
} from "@fluentui/react-icons";
import { useTranslation } from "react-i18next";
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
  const { t } = useTranslation();
  const { session } = useAuth();
  const navigate = useNavigate();
  const location = useLocation();
  const returnTo = new URLSearchParams(location.search).get("returnTo");
  const memberships = session?.memberships ?? [];

  if (memberships.length === 0) {
    return (
      <main className="workspace-page">
        <EmptyState
          title={t("workspace.emptyTitle")}
          detail={t("workspace.emptyDetail")}
        />
      </main>
    );
  }

  return (
    <main className="workspace-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">{t("workspace.eyebrow")}</p>
          <h1>{t("workspace.title")}</h1>
          <p>{t("workspace.description")}</p>
        </div>
        {memberships.some((membership) =>
          ["operator_admin", "resource_admin"].includes(membership.role),
        ) && (
          <Button onClick={() => navigate("/ops/channels")}>
            {t("workspace.manageAccounts")}
          </Button>
        )}
      </section>
      <section className="workspace-grid" aria-label={t("workspace.listLabel")}>
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
  const { t } = useTranslation();
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
      <p>
        {t("workspace.roleLabel", {
          role: t(`workspace.roles.${membership.role}`),
        })}
      </p>
      {isPending && (
        <LoadingState compact label={t("workspace.loadingProjects")} />
      )}
      {isError && (
        <ErrorState
          title={t("workspace.projectsUnavailable")}
          detail={t("workspace.projectsUnavailableDetail")}
          onRetry={() => void refetch()}
          intent="warning"
        />
      )}
      {!isPending && !isError && projects?.items.length === 0 && (
        <p className="workspace-project-empty">{t("workspace.noProjects")}</p>
      )}
      {projects && projects.items.length > 0 && (
        <div
          className="workspace-project-list"
          aria-label={t("workspace.projectsFor", {
            name: membership.tenant_display_name,
          })}
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
          {t("workspace.createProject")}
        </Button>
      </div>
      <p className="workspace-project-empty">{t("workspace.createHint")}</p>
    </Card>
  );
}
