import type { InfiniteData } from "@tanstack/react-query";
import type {
  ObservationAnalysisPage,
  ObservationAnalysisRevision,
} from "../api/observationAnalysis";
import type { AuthSession } from "../auth/types";

export function observationAnalysisKey(
  session: AuthSession | null | undefined,
  tenantId: string,
  projectId: string,
  targetId: string,
  attemptId: string,
) {
  return [
    "observation-analyses",
    session?.user.id,
    session?.operator.id,
    tenantId,
    projectId,
    targetId,
    attemptId,
  ];
}

export function analysisRevisions(
  data: InfiniteData<ObservationAnalysisPage> | undefined,
) {
  return [
    ...new Map(
      (data?.pages.flatMap((page) => page.items) ?? []).map((item) => [
        item.request.revision_id,
        item,
      ]),
    ).values(),
  ];
}

export function availableAnalysis(revisions: ObservationAnalysisRevision[]) {
  return revisions.find(
    (revision) =>
      revision.state === "completed" &&
      revision.result?.outcome.status === "grounded" &&
      Boolean(revision.result.outcome.raw_answer.trim()),
  );
}
