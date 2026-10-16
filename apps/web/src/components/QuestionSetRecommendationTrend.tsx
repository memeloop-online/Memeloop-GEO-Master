import { useState } from "react";
import { Button } from "@fluentui/react-components";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { ApiError } from "../api/client";
import { getChannelTarget, listMeasurementPlans } from "../api/channelJobs";
import {
  listObservationAnalyses,
  type ObservationAnalysisRevision,
} from "../api/observationAnalysis";
import { useAuth } from "../auth/AuthProvider";
import { availableAnalysis } from "../pages/observationAnalysisResult";
import { ErrorState, LoadingState } from "./AsyncState";
import {
  RecommendationTrend,
  type RecommendationTrendSample,
} from "./RecommendationTrend";
import "../i18n/questionSetRecommendationTrend";

export function QuestionSetRecommendationTrend({
  tenantId,
  projectId,
  questionSetId,
  questionSetVersionId,
}: {
  tenantId: string;
  projectId: string;
  questionSetId: string;
  questionSetVersionId: string;
}) {
  const { session } = useAuth();
  const { t } = useTranslation("questionSetRecommendationTrend");
  const [open, setOpen] = useState(false);
  const results = useQuery({
    queryKey: [
      "question-set-recommendation-trend",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      questionSetId,
      questionSetVersionId,
    ],
    enabled: Boolean(session && open),
    retry: false,
    queryFn: async ({ signal }) => {
      const samples: RecommendationTrendSample[] = [];
      const seenPlans = new Set<string>();
      const planCursors = new Set<string>();
      let after: string | undefined;
      do {
        signal.throwIfAborted();
        const page = await listMeasurementPlans(tenantId, projectId, after);
        for (const plan of page.items) {
          if (seenPlans.has(plan.plan_id)) continue;
          seenPlans.add(plan.plan_id);
          for (const target of plan.targets) {
            const input = target.input;
            if (input.kind !== "measure") continue;
            const reference = input.question_binding?.reference;
            if (
              reference?.question_set_id !== questionSetId ||
              reference.question_set_version_id !== questionSetVersionId
            )
              continue;
            signal.throwIfAborted();
            const view = await getChannelTarget(
              tenantId,
              projectId,
              target.target_id,
              signal,
            );
            const attempt = view.attempts.at(-1);
            if (attempt?.outcome?.fixture) continue;
            let selected: ObservationAnalysisRevision | undefined;
            // A target is one scheduled sample. Technical attempts and reanalyses do not add samples.
            if (attempt) {
              const revisions: ObservationAnalysisRevision[] = [];
              const analysisCursors = new Set<string>();
              let analysisAfter: string | undefined;
              do {
                const analyses = await listObservationAnalyses(
                  tenantId,
                  projectId,
                  target.target_id,
                  attempt.attempt_id,
                  analysisAfter,
                  signal,
                );
                revisions.push(...analyses.items);
                analysisAfter = analyses.next_after ?? undefined;
                if (analysisAfter && analysisCursors.has(analysisAfter))
                  throw new Error("Repeated analysis cursor");
                if (analysisAfter) analysisCursors.add(analysisAfter);
              } while (analysisAfter);
              selected = availableAnalysis(revisions);
            }
            const outcome = selected?.result?.outcome;
            const query = new URLSearchParams({
              tab: "records",
              set: questionSetId,
              version: questionSetVersionId,
              record: plan.plan_id,
              target: target.target_id,
            });
            if (attempt) query.set("attempt", attempt.attempt_id);
            if (selected)
              query.set("analysisRevision", selected.request.revision_id);
            samples.push({
              sampleId: target.target_id,
              recordHref: `/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/measurement?${query}`,
              observedAt:
                selected?.request.observed_at ??
                attempt?.outcome?.occurred_at ??
                input.scheduled_at,
              measurement: input,
              analysisRevisionId: selected?.request.revision_id,
              recommendations:
                outcome?.status === "grounded"
                  ? outcome.audit.recommendations
                  : undefined,
            });
          }
        }
        after = page.next_after ?? undefined;
        if (after && planCursors.has(after))
          throw new Error("Repeated measurement cursor");
        if (after) planCursors.add(after);
      } while (after);
      return samples;
    },
  });
  const forbidden =
    results.error instanceof ApiError &&
    [401, 403].includes(results.error.status);
  return (
    <div>
      <Button
        appearance="subtle"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {t(open ? "close" : "open")}
      </Button>
      {open && (
        <>
          <p>{t("scope")}</p>
          <p>{t("live")}</p>
          {!session && <p role="alert">{t("forbidden")}</p>}
          {results.isFetching && <LoadingState label={t("loading")} />}
          {results.isError && (
            <ErrorState
              title={t("failed")}
              detail={t(forbidden ? "forbidden" : "retryDetail")}
              onRetry={() => void results.refetch()}
            />
          )}
          {!results.isFetching &&
            !results.isError &&
            results.data &&
            (results.data.length ? (
              <RecommendationTrend samples={results.data} />
            ) : (
              <p>{t("noRecords")}</p>
            ))}
          {results.isSuccess && (
            <Button
              appearance="subtle"
              disabled={results.isFetching}
              onClick={() => void results.refetch()}
            >
              {t("refresh")}
            </Button>
          )}
        </>
      )}
    </div>
  );
}
