import { useEffect } from "react";
import { Button, Spinner } from "@fluentui/react-components";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Link, useSearchParams } from "react-router-dom";
import {
  getChannelTarget,
  type ChannelAttempt,
  type ChannelTarget,
  type StandaloneMeasurementPlan,
} from "../api/channelJobs";
import {
  classifyBrandRecommendation,
  isMeasurementFocus,
  measurementFocusTargets,
} from "../api/measurementFocus";
import {
  isExtractionActive,
  listObservationAnalyses,
} from "../api/observationAnalysis";
import { useProjectQuery, type MeasurementTarget } from "../api/projects";
import {
  decodeVerifiedRecommendations,
  type RecommendationItem,
} from "../api/recommendations";
import { useAuth } from "../auth/AuthProvider";
import {
  analysisRevisions,
  availableAnalysis,
  observationAnalysisKey,
} from "../pages/observationAnalysisResult";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import { ObservationAnswer } from "./ObservationAnswer";
import "../i18n/measurementOutcome";
import "../pages/measurementMessages";
import "./MeasurementOutcomeSummary.css";

function Recommendation({ item }: { item: RecommendationItem }) {
  const { t } = useTranslation("measurementOutcome");
  return (
    <article className="measurement-outcome-item" aria-label={item.name}>
      <div className="measurement-outcome-item-heading">
        <strong>{item.name}</strong>
        <span>{t(`kind_${item.kind}`)}</span>
        <span>
          {item.rank === null
            ? t("unranked")
            : t("ranked", { rank: item.rank })}
        </span>
      </div>
      <p>{item.reason || t("reasonMissing")}</p>
      <details>
        <summary>{t("evidence")}</summary>
        <blockquote>{item.evidence_quote}</blockquote>
        {item.brand_attribution && (
          <>
            <strong>{t("brandEvidence")}</strong>
            <p>{item.brand_attribution.name}</p>
            <blockquote>{item.brand_attribution.evidence_quote}</blockquote>
          </>
        )}
      </details>
    </article>
  );
}

function Recommendations({ items }: { items: RecommendationItem[] }) {
  const { t } = useTranslation("measurementOutcome");
  const card = (item: RecommendationItem, index: number) => (
    <Recommendation
      key={`${item.kind}-${item.mention_order}-${index}`}
      item={item}
    />
  );
  return (
    <div className="measurement-outcome-items">
      {items.slice(0, 3).map(card)}
      {items.length > 3 && (
        <details>
          <summary>{t("more", { count: items.length - 3 })}</summary>
          <div className="measurement-outcome-items">
            {items.slice(3).map(card)}
          </div>
        </details>
      )}
    </div>
  );
}

function AttemptSummary({
  tenantId,
  projectId,
  targetId,
  attempt,
  focusTargets,
  active,
  evaluationOnly,
}: {
  tenantId: string;
  projectId: string;
  targetId: string;
  attempt: ChannelAttempt;
  focusTargets: readonly MeasurementTarget[];
  active: boolean;
  evaluationOnly: boolean;
}) {
  const { session } = useAuth();
  const { t, i18n } = useTranslation("measurementOutcome");
  const [params] = useSearchParams();
  const requestedRevision =
    params.get("target") === targetId &&
    params.get("attempt") === attempt.attempt_id
      ? params.get("analysisRevision")
      : null;
  const history = useInfiniteQuery({
    queryKey: observationAnalysisKey(
      session,
      tenantId,
      projectId,
      targetId,
      attempt.attempt_id,
    ),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam, signal }) =>
      listObservationAnalyses(
        tenantId,
        projectId,
        targetId,
        attempt.attempt_id,
        pageParam,
        signal,
      ),
    getNextPageParam: (last) => last.next_after ?? undefined,
    enabled:
      active &&
      Boolean(session) &&
      Boolean(attempt.outcome) &&
      !attempt.outcome?.fixture,
    retry: false,
    refetchInterval: (query) =>
      active &&
      (isExtractionActive(query.state.data?.pages[0]?.extraction) ||
        query.state.data?.pages.some((page) =>
          page.items.some((item) => item.state !== "completed"),
        ))
        ? 3000
        : false,
  });
  const revisions = analysisRevisions(history.data);
  const primary = requestedRevision
    ? revisions.find(
        (revision) => revision.request.revision_id === requestedRevision,
      )
    : availableAnalysis(revisions);
  const { fetchNextPage, hasNextPage, isFetching, isError } = history;
  useEffect(() => {
    if (active && !primary && hasNextPage && !isFetching && !isError)
      void fetchNextPage();
  }, [active, primary, hasNextPage, isFetching, isError, fetchNextPage]);
  const savedAnswer =
    primary?.state === "completed" &&
    primary.result?.outcome.status === "grounded"
      ? primary.result.outcome
      : null;
  const grounded =
    savedAnswer ??
    (!requestedRevision &&
    attempt.outcome?.status === "observed" &&
    attempt.outcome.raw_answer?.trim()
      ? {
          raw_answer: attempt.outcome.raw_answer,
          citations: attempt.outcome.citations,
          audit: {} as Record<string, unknown>,
        }
      : null);
  const items = decodeVerifiedRecommendations(grounded?.audit.recommendations);
  const pending =
    !attempt.outcome ||
    isExtractionActive(history.data?.pages[0]?.extraction) ||
    revisions.some((revision) => revision.state !== "completed");
  if (attempt.outcome?.fixture) return <p>{t("fixture")}</p>;
  if (history.isPending && attempt.outcome && !grounded)
    return <Spinner size="tiny" label={t("loading")} />;
  if (!grounded) {
    const status = attempt.outcome?.status;
    return (
      <div className="measurement-outcome-column">
        <p role="status">
          {t(
            requestedRevision
              ? "revisionMissing"
              : pending
                ? "pending"
                : status === "refused"
                  ? "refused"
                  : status &&
                      [
                        "failed",
                        "missing",
                        "login_required",
                        "unsupported",
                      ].includes(status)
                    ? "failed"
                    : "unknown",
          )}
        </p>
        {history.isError && (
          <>
            <p role="alert">{t("loadFailed")}</p>
            <Button onClick={() => void history.refetch()}>{t("retry")}</Button>
          </>
        )}
      </div>
    );
  }
  const ordered = [...(items ?? [])].sort(
    (a, b) => a.mention_order - b.mention_order,
  );
  const alternatives = ordered.filter(
    (item) =>
      !isMeasurementFocus(item, focusTargets) &&
      !focusTargets.some(
        (focus) => classifyBrandRecommendation([item], focus) === "positive",
      ),
  );
  const citations = [
    ...new Set(
      grounded.citations
        .map(safeOriginalPublicUrl)
        .filter((url): url is string => url !== null),
    ),
  ];
  return (
    <div className="measurement-outcome-column">
      {history.isError && (
        <>
          <p role="alert">{t("updateFailed")}</p>
          <Button onClick={() => void history.refetch()}>{t("retry")}</Button>
        </>
      )}
      <p className="measurement-outcome-scope">
        {t("observed", {
          value: new Date(
            primary?.request.observed_at ?? attempt.outcome!.occurred_at,
          ).toLocaleString(i18n.resolvedLanguage),
        })}
      </p>
      <div className="measurement-outcome-grid">
        <section
          className="measurement-outcome-column"
          aria-label={t(
            focusTargets.length ? "focusedTitle" : "recommendedTitle",
          )}
        >
          <h4>
            {t(focusTargets.length ? "focusedTitle" : "recommendedTitle")}
          </h4>
          {items === null ? (
            <p>{t("noConclusion")}</p>
          ) : focusTargets.length ? (
            focusTargets.map((focus) => {
              const matches = ordered.filter((item) =>
                isMeasurementFocus(item, [focus]),
              );
              const affiliates = ordered.filter(
                (item) =>
                  classifyBrandRecommendation([item], focus) === "positive",
              );
              return (
                <section
                  className="measurement-outcome-column"
                  key={JSON.stringify([focus.kind, focus.name])}
                  aria-label={focus.name}
                >
                  <h5>
                    {focus.name} · {t(`kind_${focus.kind}`)}
                  </h5>
                  {focus.kind === "brand" && (
                    <strong>{t("directBrand")}</strong>
                  )}
                  <p>
                    {matches.length
                      ? t("recommended")
                      : t("absent", { name: focus.name })}
                  </p>
                  {matches.length > 0 && <h5>{t("descriptionTitle")}</h5>}
                  <Recommendations items={matches} />
                  {focus.kind === "brand" && (
                    <>
                      <strong>{t("affiliates")}</strong>
                      {affiliates.length ? (
                        <Recommendations items={affiliates} />
                      ) : (
                        <p>
                          {t(
                            classifyBrandRecommendation(ordered, focus) ===
                              "unknown"
                              ? "affiliatesUnknown"
                              : "affiliatesAbsent",
                          )}
                        </p>
                      )}
                    </>
                  )}
                </section>
              );
            })
          ) : ordered.length ? (
            <Recommendations items={ordered} />
          ) : (
            <p>{t("noRecommendations")}</p>
          )}
          <details>
            <summary>{t("answer")}</summary>
            <ObservationAnswer text={grounded.raw_answer} />
          </details>
        </section>
        <div className="measurement-outcome-column">
          {focusTargets.length > 0 && items !== null && (
            <section
              className="measurement-outcome-column"
              aria-label={t("alternativesTitle")}
            >
              <h4>{t("alternativesTitle")}</h4>
              {alternatives.length ? (
                <Recommendations items={alternatives} />
              ) : (
                <p>{t("noAlternatives")}</p>
              )}
            </section>
          )}
          <section
            className="measurement-outcome-column"
            aria-label={t("citationsTitle")}
          >
            <h4>{t("citationsTitle")}</h4>
            {citations.length ? (
              <ul>
                {citations.map((url) => (
                  <li key={url}>
                    <a href={url} target="_blank" rel="noopener noreferrer">
                      {url}
                    </a>
                  </li>
                ))}
              </ul>
            ) : (
              <p>{t("sourceMissing")}</p>
            )}
          </section>
          <section
            className="measurement-outcome-column"
            aria-label={t("nextTitle")}
          >
            <h4>{t("nextTitle")}</h4>
            <p>
              {t(
                evaluationOnly
                  ? "evaluationOnly"
                  : citations.length
                    ? "nextWithSources"
                    : "nextWithoutSources",
              )}
            </p>
          </section>
        </div>
      </div>
    </div>
  );
}

function TargetSummary({
  tenantId,
  projectId,
  target,
  focusTargets,
  active,
}: {
  tenantId: string;
  projectId: string;
  target: ChannelTarget;
  focusTargets: readonly MeasurementTarget[];
  active: boolean;
}) {
  const { session } = useAuth();
  const { t } = useTranslation("measurementOutcome");
  const { t: measurementT } = useTranslation("measurement");
  const [params] = useSearchParams();
  const detail = useQuery({
    queryKey: [
      "channel-target",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      target.target_id,
    ],
    queryFn: () => getChannelTarget(tenantId, projectId, target.target_id),
    enabled: active && Boolean(session),
    retry: false,
    refetchInterval: (query) =>
      !active || query.state.data?.attempts.at(-1)?.outcome ? false : 3000,
  });
  if (target.input.kind !== "measure") return null;
  const input = target.input;
  const requestedAttempt =
    params.get("target") === target.target_id ? params.get("attempt") : null;
  const attempts =
    detail.data?.attempts.filter(
      (attempt) => !requestedAttempt || attempt.attempt_id === requestedAttempt,
    ) ?? [];
  return (
    <section className="measurement-outcome-target" aria-label={input.question}>
      <h3>{input.question}</h3>
      <div className="measurement-outcome-conditions">
        <strong>{input.provider}</strong>
        <span>{t("model", { value: input.model })}</span>
        <span>
          {t("search", {
            value:
              input.search_mode === "web_search"
                ? measurementT("resultWebSearch")
                : input.search_mode,
          })}
        </span>
        <span>
          {t("surface", {
            value:
              input.surface === "consumer_web"
                ? measurementT("resultConsumerWeb")
                : input.surface,
          })}
        </span>
        <span>{t("market", { value: input.market })}</span>
        <span>{t("language", { value: input.language })}</span>
      </div>
      {detail.isError && (
        <>
          <p role="alert">{t(detail.data ? "updateFailed" : "loadFailed")}</p>
          <Button onClick={() => void detail.refetch()}>{t("retry")}</Button>
        </>
      )}
      {detail.isPending ? (
        <Spinner size="tiny" label={t("loading")} />
      ) : !detail.data ? null : !attempts.length ? (
        <p role="status">
          {t(requestedAttempt ? "revisionMissing" : "queued")}
        </p>
      ) : (
        attempts.map((attempt) => (
          <AttemptSummary
            key={attempt.attempt_id}
            {...{ tenantId, projectId, focusTargets, active }}
            targetId={target.target_id}
            attempt={attempt}
            evaluationOnly={
              input.question_binding?.purpose === "frozen_evaluation"
            }
          />
        ))
      )}
    </section>
  );
}

/** Exact-plan evidence only: never infer recommendations from answer text or historical insights. */
export function MeasurementOutcomeSummary({
  tenantId,
  projectId,
  plan,
  active = true,
}: {
  tenantId: string;
  projectId: string;
  plan: StandaloneMeasurementPlan;
  active?: boolean;
}) {
  const { t } = useTranslation("measurementOutcome");
  const project = useProjectQuery(tenantId, projectId);
  const [params] = useSearchParams();
  const focusTargets = measurementFocusTargets(project.data?.settings);
  const selectedTarget = params.get("target");
  return (
    <section className="measurement-outcome" aria-label={t("title")}>
      <div className="measurement-outcome-heading">
        <h3>{t("title")}</h3>
        <Link
          className="measurement-outcome-discuss"
          to={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/chat?${new URLSearchParams({ measurementPlan: plan.plan_id })}`}
        >
          {t("discuss")}
        </Link>
      </div>
      <p className="measurement-outcome-scope">{t("scope")}</p>
      {project.isPending && <p role="status">{t("focusLoading")}</p>}
      {project.isError && <p role="alert">{t("focusFailed")}</p>}
      {plan.targets
        .filter(
          (target) =>
            target.input.kind === "measure" &&
            (!selectedTarget || target.target_id === selectedTarget),
        )
        .map((target) => (
          <TargetSummary
            key={target.target_id}
            {...{ tenantId, projectId, target, focusTargets, active }}
          />
        ))}
    </section>
  );
}
