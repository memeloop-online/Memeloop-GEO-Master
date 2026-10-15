import { useRef } from "react";
import { Button, Spinner } from "@fluentui/react-components";
import {
  useInfiniteQuery,
  useMutation,
  useQueryClient,
} from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { createIdempotencyKey } from "../api/client";
import {
  createObservationAnalysis,
  isExtractionActive,
  listObservationAnalyses,
} from "../api/observationAnalysis";
import { useAuth } from "../auth/AuthProvider";
import { ObservationAnswer } from "../components/ObservationAnswer";
import { RecommendationResults } from "../components/RecommendationResults";
import { safeOriginalPublicUrl } from "./PublicationLookupPanel";
import {
  analysisFailureMessageKey,
  analysisUnverifiedMessageKey,
  extractionInterruptionMessageKey,
} from "../i18n/observationAnalysis";
import {
  analysisRevisions,
  availableAnalysis,
  observationAnalysisKey,
} from "./observationAnalysisResult";

export function ObservationAnalysisPanel({
  tenantId,
  projectId,
  targetId,
  attemptId,
  canWrite,
}: {
  tenantId: string;
  projectId: string;
  targetId: string;
  attemptId: string;
  canWrite: boolean;
}) {
  const { session } = useAuth();
  const { t, i18n } = useTranslation("observationAnalysis");
  const client = useQueryClient();
  const pendingKey = useRef<string | null>(null);
  const key = observationAnalysisKey(
    session,
    tenantId,
    projectId,
    targetId,
    attemptId,
  );
  const history = useInfiniteQuery({
    queryKey: key,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam, signal }) =>
      listObservationAnalyses(
        tenantId,
        projectId,
        targetId,
        attemptId,
        pageParam,
        signal,
      ),
    getNextPageParam: (last) => last.next_after ?? undefined,
    enabled: Boolean(session),
    retry: false,
    refetchInterval: (query) =>
      isExtractionActive(query.state.data?.pages[0]?.extraction) ||
      query.state.data?.pages.some((page) =>
        page.items.some((item) => item.state !== "completed"),
      )
        ? 3000
        : false,
  });
  const submission = useMutation({
    mutationFn: () => {
      pendingKey.current ??= createIdempotencyKey();
      return createObservationAnalysis(
        tenantId,
        projectId,
        targetId,
        attemptId,
        pendingKey.current,
      );
    },
    onSuccess: (revision) => {
      pendingKey.current = null;
      // Retain the accepted revision even if the following read temporarily fails.
      client.setQueryData<typeof history.data>(key, (previous) =>
        previous
          ? {
              ...previous,
              pages: previous.pages.map((page, index) =>
                index === 0
                  ? {
                      ...page,
                      items: [
                        revision,
                        ...page.items.filter(
                          (item) =>
                            item.request.revision_id !==
                            revision.request.revision_id,
                        ),
                      ],
                    }
                  : page,
              ),
            }
          : previous,
      );
      void client.invalidateQueries({ queryKey: key });
    },
  });
  const revisions = analysisRevisions(history.data);
  const primary = availableAnalysis(revisions);
  const hasSource = Boolean(history.data?.pages[0]?.sources.length);
  const extraction = history.data?.pages[0]?.extraction;
  const active = revisions.some((item) => item.state !== "completed");
  const date = (value: string) =>
    new Date(value).toLocaleString(i18n.resolvedLanguage);
  const renderRevision = (revision: (typeof revisions)[number]) => {
    const outcome = revision.result?.outcome;
    const grounded =
      revision.state === "completed" && outcome?.status === "grounded"
        ? outcome
        : null;
    const citations = [
      ...new Set(
        (grounded?.citations ?? [])
          .map(safeOriginalPublicUrl)
          .filter((url): url is string => url !== null),
      ),
    ];
    const metadata = (
      <>
        <p>{t("requested", { value: date(revision.created_at) })}</p>
        {revision.analyzed_at && (
          <p>{t("analyzed", { value: date(revision.analyzed_at) })}</p>
        )}
        <p>
          {revision.result?.actual_model
            ? t("model", { value: revision.result.actual_model })
            : t("modelMissing")}
        </p>
        <p>
          {t("source")} ·{" "}
          {t("observed", { value: date(revision.request.observed_at) })}
        </p>
      </>
    );
    return (
      <article key={revision.request.revision_id}>
        <p role="status">
          {t(
            revision.state !== "completed"
              ? revision.state
              : outcome?.status === "failed"
                ? analysisFailureMessageKey(outcome.code)
                : outcome?.status === "unverified"
                  ? analysisUnverifiedMessageKey(outcome.reason)
                  : (outcome?.status ?? "incomplete"),
          )}
        </p>
        {!grounded && metadata}
        {grounded && (
          <>
            <RecommendationResults value={grounded.audit.recommendations} />
            <h5>{t("answer")}</h5>
            <ObservationAnswer text={grounded.raw_answer} />
            <h5>{t("citations")}</h5>
            {citations.length ? (
              <ul>
                {citations.map((url) => (
                  <li key={url} style={{ overflowWrap: "anywhere" }}>
                    <a href={url} target="_blank" rel="noopener noreferrer">
                      {url}
                    </a>
                  </li>
                ))}
              </ul>
            ) : (
              <p>{t("noCitations")}</p>
            )}
            <details>
              <summary>{t("answerDetails")}</summary>
              {metadata}
              <details>
                <summary>{t("evidence")}</summary>
                <pre
                  style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}
                >
                  {JSON.stringify(grounded.audit, null, 2)}
                </pre>
              </details>
            </details>
          </>
        )}
      </article>
    );
  };
  const controls = (
    <>
      <h4>{t("title")}</h4>
      {history.isPending && <Spinner size="tiny" label={t("loading")} />}
      {history.isError && <p role="alert">{t("loadFailed")}</p>}
      {history.data && !hasSource && <p>{t("noSource")}</p>}
      {hasSource && !revisions.length && !extraction && (
        <p>{t("noRevisions")}</p>
      )}
      {extraction && extraction.state !== "completed" && (
        <p role="status">
          {t(
            extraction.state === "interrupted"
              ? extractionInterruptionMessageKey(extraction.interruption)
              : extraction.state,
          )}
        </p>
      )}
      {hasSource && canWrite && (
        <Button
          disabled={submission.isPending || active || history.isError}
          onClick={() => submission.mutate()}
        >
          {t(
            submission.isPending
              ? "submitting"
              : submission.isError
                ? "retrySubmit"
                : "parse",
          )}
        </Button>
      )}
      {!canWrite && <p>{t("readOnly")}</p>}
      {submission.isError && <p role="alert">{t("submitFailed")}</p>}
      <Button
        appearance="subtle"
        disabled={history.isFetching}
        onClick={() => void history.refetch()}
      >
        {t("refresh")}
      </Button>
      <Link
        to={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/settings?tab=ai`}
      >
        {t("settings")}
      </Link>
      {revisions.filter((revision) => revision !== primary).map(renderRevision)}
      {history.hasNextPage && (
        <Button
          disabled={history.isFetching}
          onClick={() => void history.fetchNextPage()}
        >
          {t("more")}
        </Button>
      )}
    </>
  );
  return (
    <section aria-label={t("title")}>
      {primary ? (
        <>
          <h4>{t("availableAnswer")}</h4>
          {renderRevision(primary)}
          <details>
            <summary>{t("history")}</summary>
            {controls}
          </details>
        </>
      ) : (
        controls
      )}
    </section>
  );
}
