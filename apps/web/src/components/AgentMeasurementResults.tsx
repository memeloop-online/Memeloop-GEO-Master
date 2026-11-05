import { useEffect, useState } from "react";
import { Button, Spinner } from "@fluentui/react-components";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useAui } from "@memeloop/react-ui/chat";
import { useTranslation } from "react-i18next";
import type { AgentMessage } from "../api/agent";
import { getMeasurementPlan } from "../api/channelJobs";
import { projectQueryKeys } from "../api/projects";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor } from "../auth/types";
import { MeasurementOutcomeSummary } from "./MeasurementOutcomeSummary";
import "../i18n/agentMeasurementResults";
import "../i18n/measurementPlanChat";
import "./AgentMeasurementResults.css";

interface MeasurementReference {
  plan_id: string;
  target_id: string;
}

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Resource identity comes only from saved API assistant metadata, never prose. */
export function measurementReferencesByMessage(
  messages: readonly AgentMessage[],
) {
  const result = new Map<string, MeasurementReference[]>();
  const seen = new Set<string>();
  for (const message of [...messages].sort((a, b) => a.sequence - b.sequence)) {
    if (
      message.role !== "assistant" ||
      !message.metadata ||
      typeof message.metadata !== "object" ||
      Array.isArray(message.metadata)
    )
      continue;
    const references = (message.metadata as Record<string, unknown>)
      .measurement_plans;
    if (!Array.isArray(references)) continue;
    for (const value of references) {
      if (!value || typeof value !== "object" || Array.isArray(value)) continue;
      const { plan_id, target_id } = value as Record<string, unknown>;
      if (
        typeof plan_id !== "string" ||
        !uuid.test(plan_id) ||
        typeof target_id !== "string" ||
        !uuid.test(target_id)
      )
        continue;
      const key = JSON.stringify([
        message.conversation_id,
        message.turn_id ?? message.id,
        plan_id,
        target_id,
      ]);
      if (seen.has(key)) continue;
      seen.add(key);
      const list = result.get(message.id) ?? [];
      list.push({ plan_id, target_id });
      result.set(message.id, list);
    }
  }
  return result;
}

function MeasurementResult({
  tenantId,
  projectId,
  planId,
  targetIds,
  onFocus,
}: {
  tenantId: string;
  projectId: string;
  planId: string;
  targetIds: readonly string[];
  onFocus: () => void;
}) {
  const { session } = useAuth();
  const { t } = useTranslation("agentMeasurementResults");
  const { t: planT } = useTranslation("measurementPlanChat");
  const aui = useAui();
  const [preserved, setPreserved] = useState(false);
  const plan = useQuery({
    queryKey: [
      "agent-measurement-plan",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      planId,
    ],
    queryFn: () => getMeasurementPlan(tenantId, projectId, planId),
    enabled: Boolean(session),
    retry: false,
    refetchOnMount: "always",
  });
  const targets = plan.data?.targets.filter((target) =>
    targetIds.includes(target.target_id),
  );
  return (
    <section className="agent-measurement-result" aria-label={t("title")}>
      {plan.isPending ? (
        <Spinner size="tiny" label={t("loading")} />
      ) : !plan.data || !targets?.length ? (
        <div role="alert">
          <p>{t("unavailable")}</p>
          <Button onClick={() => void plan.refetch()}>{t("retry")}</Button>
        </div>
      ) : (
        <>
          {plan.isError && (
            <div role="alert">
              <p>{t("updateFailed")}</p>
              <Button onClick={() => void plan.refetch()}>{t("retry")}</Button>
            </div>
          )}
          <MeasurementOutcomeSummary
            tenantId={tenantId}
            projectId={projectId}
            plan={{ ...plan.data, targets }}
            discussionAction={
              <Button
                appearance="primary"
                onClick={() => {
                  // Message slots scope `aui.composer` to the message editor.
                  // Explicitly address the existing thread's native composer.
                  const composer = aui.thread.composer();
                  const hasDraft = composer.getState().text !== "";
                  if (!hasDraft) composer.setText(planT("request", { planId }));
                  setPreserved(hasDraft);
                  onFocus();
                }}
              >
                {t("discuss")}
              </Button>
            }
          />
          {preserved && <p role="status">{t("preserved")}</p>}
        </>
      )}
    </section>
  );
}

export function AgentMeasurementResults({
  tenantId,
  projectId,
  references,
  onFocus,
}: {
  tenantId: string;
  projectId: string;
  references: readonly MeasurementReference[];
  onFocus: () => void;
}) {
  const { session } = useAuth();
  const client = useQueryClient();
  const referenceKey = JSON.stringify(references);
  useEffect(() => {
    if (!session || !references.length) return;
    void client.invalidateQueries({
      queryKey: projectQueryKeys.detail(
        queryScopeFor(session, tenantId, projectId),
      ),
    });
  }, [client, session, tenantId, projectId, referenceKey]);
  const plans = new Map<string, string[]>();
  for (const reference of references) {
    const targets = plans.get(reference.plan_id) ?? [];
    if (!targets.includes(reference.target_id))
      targets.push(reference.target_id);
    plans.set(reference.plan_id, targets);
  }
  return (
    <div className="agent-measurement-results">
      {[...plans].map(([planId, targetIds]) => (
        <MeasurementResult
          key={JSON.stringify([
            session?.user.id,
            session?.operator.id,
            tenantId,
            projectId,
            planId,
          ])}
          {...{ tenantId, projectId, planId, targetIds, onFocus }}
        />
      ))}
    </div>
  );
}
