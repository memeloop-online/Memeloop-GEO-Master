import { useRef, useState } from "react";
import { Button, Card } from "@fluentui/react-components";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth/AuthProvider";
import {
  getCitationInsights,
  getSourceChannelRecommendations,
  includeRecommendedPlatform,
  type CitationSample,
  type ObservedCitationSource,
  type SourceChannelRecommendation,
} from "../api/citationInsights";
import { getChannelTarget } from "../api/channelJobs";
import { ApiError, createIdempotencyKey } from "../api/client";
import {
  useProjectQuery,
  useUpdateProjectMutation,
  type DistributionScope,
} from "../api/projects";
import { ErrorState, LoadingState } from "../components/AsyncState";
import "../i18n";
import "./citationInsightsMessages";
import "./CitationInsightsPanel.css";

function publicationReasonKey(reason: string | null) {
  switch (reason) {
    case "connector_unavailable":
    case "connector_disabled":
    case "connector_version_mismatch":
    case "connector_unsupported_content_type":
    case "connector_unmapped":
    case "project_account_not_ready":
    case "send_time_eligibility_required":
    case "source_not_mapped_to_publishing_channel":
      return reason;
    default:
      return "publishingNotConfirmed";
  }
}

const platformNameKeys: Record<string, string> = {
  zhihu: "platformZhihu",
  baidu_creator: "platformBaiduCreator",
  xiaohongshu: "platformXiaohongshu",
  x: "platformX",
  medium: "platformMedium",
};

function safeWebUrl(value: string): string | null {
  try {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol) &&
      url.hostname &&
      !url.username &&
      !url.password
      ? url.href
      : null;
  } catch {
    return null;
  }
}

function SourceSample({
  tenantId,
  projectId,
  sample,
}: {
  tenantId: string;
  projectId: string;
  sample: CitationSample;
}) {
  const { session } = useAuth();
  const { t, i18n } = useTranslation("citationInsights");
  const [open, setOpen] = useState(false);
  const detail = useQuery({
    queryKey: [
      "citation-answer",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      sample.target_id,
    ],
    queryFn: () => getChannelTarget(tenantId, projectId, sample.target_id),
    enabled: Boolean(session && open),
    retry: false,
  });
  const attempt = detail.data?.attempts.find(
    (item) => item.attempt_id === sample.attempt_id,
  );
  const date = (value: string) =>
    new Intl.DateTimeFormat(i18n.language, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(value));
  return (
    <li className="citation-insights-sample">
      <Button
        size="small"
        appearance="subtle"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {t("sample")}
      </Button>
      <span>{t("sampleTime", { time: date(sample.observed_at) })}</span>
      {open && (
        <div className="citation-insights-answer">
          <p>
            {t("protocol", {
              provider: sample.provider,
              model: sample.model,
              surface: sample.surface,
            })}{" "}
            · {sample.market} / {sample.language}
          </p>
          <p>
            {t("scheduledTime", { time: date(sample.scheduled_at) })} ·{" "}
            {t("receivedTime", { time: date(sample.received_at) })}
          </p>
          {sample.question_purpose && (
            <p>
              {t(
                sample.question_purpose === "frozen_evaluation"
                  ? "evaluation"
                  : "optimization",
              )}
            </p>
          )}
          {detail.isPending ? (
            <LoadingState label={t("evidenceLoading")} compact />
          ) : detail.isError || !attempt ? (
            <ErrorState
              title={t("evidenceUnavailable")}
              detail={t("evidenceUnavailableDetail")}
              onRetry={() => void detail.refetch()}
            />
          ) : (
            <>
              {detail.data.target.input.kind === "measure" && (
                <div>
                  <strong>{t("question")}</strong>
                  <p>{detail.data.target.input.question}</p>
                </div>
              )}
              <div>
                <strong>{t("answer")}</strong>
                <p className="citation-insights-original-answer">
                  {attempt.outcome?.raw_answer ?? t("answerMissing")}
                </p>
              </div>
            </>
          )}
        </div>
      )}
    </li>
  );
}

function SourceCard({
  tenantId,
  projectId,
  source,
}: {
  tenantId: string;
  projectId: string;
  source: ObservedCitationSource;
}) {
  const { t, i18n } = useTranslation("citationInsights");
  const count = (number: number) =>
    new Intl.NumberFormat(i18n.language).format(number);
  return (
    <Card className="citation-insights-source">
      <h3>{source.host}</h3>
      <p>{t("cited", { count: count(source.citing_answers) })}</p>
      <details>
        <summary>{t("urls")}</summary>
        <ul className="citation-insights-urls">
          {source.urls.map((entry) => {
            const href = safeWebUrl(entry.url);
            return (
              <li key={entry.url}>
                {href ? (
                  <a href={href} target="_blank" rel="noopener noreferrer">
                    {entry.url}
                  </a>
                ) : (
                  <span>{entry.url}</span>
                )}
                <p>{t("urlCited", { count: count(entry.citing_answers) })}</p>
                <ul>
                  {entry.samples.map((sample) => (
                    <SourceSample
                      key={sample.attempt_id}
                      tenantId={tenantId}
                      projectId={projectId}
                      sample={sample}
                    />
                  ))}
                </ul>
              </li>
            );
          })}
        </ul>
      </details>
    </Card>
  );
}

/** Counts and sources always refer only to the five plans in the current page. */
function CitationInsightsContent({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { session } = useAuth();
  const { t, i18n } = useTranslation("citationInsights");
  const [after, setAfter] = useState<string>();
  const [previous, setPrevious] = useState<(string | undefined)[]>([]);
  const [selectionMessage, setSelectionMessage] = useState<
    "saved" | "changed" | "failed" | "unverified" | null
  >(null);
  const [savingPlatform, setSavingPlatform] = useState<string | null>(null);
  const pendingSelection = useRef<{
    platformId: string;
    revision: number;
    scope: DistributionScope;
    idempotencyKey: string;
  } | null>(null);
  const project = useProjectQuery(tenantId, projectId);
  const updateProject = useUpdateProjectMutation(tenantId, projectId);
  const canEdit = session?.memberships.some(
    (membership) =>
      membership.tenant_id === tenantId &&
      (membership.role === "tenant_admin" || membership.role === "member"),
  );
  const insights = useQuery({
    queryKey: [
      "citation-insights",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      after,
    ],
    queryFn: () => getCitationInsights(tenantId, projectId, after),
    enabled: Boolean(session),
    retry: false,
  });
  const recommendations = useQuery({
    queryKey: [
      "source-channel-recommendations",
      session?.user.id,
      session?.operator.id,
      tenantId,
      projectId,
      after,
    ],
    queryFn: () => getSourceChannelRecommendations(tenantId, projectId, after),
    enabled: Boolean(session),
    retry: false,
  });
  async function selectPlatform(platformId: string) {
    if (!canEdit || savingPlatform) return;
    setSavingPlatform(platformId);
    setSelectionMessage(null);
    try {
      // Read before writing or retrying an uncertain response. Never silently
      // rebase a previously attempted change onto somebody else's new scope.
      const latest = await project.refetch();
      if (latest.isError || !latest.data)
        throw latest.error ?? new Error("Project unavailable");
      let intent = pendingSelection.current;
      if (intent && intent.platformId !== platformId) {
        setSelectionMessage("unverified");
        return;
      }
      if (intent) {
        if (
          JSON.stringify(latest.data.settings.distribution_scope) ===
          JSON.stringify(intent.scope)
        ) {
          pendingSelection.current = null;
          await recommendations.refetch();
          setSelectionMessage("saved");
          return;
        }
        if (latest.data.revision !== intent.revision) {
          pendingSelection.current = null;
          setSelectionMessage("changed");
          return;
        }
      } else {
        const scope = includeRecommendedPlatform(
          latest.data.settings.distribution_scope,
          platformId,
        );
        if (
          JSON.stringify(scope) ===
          JSON.stringify(latest.data.settings.distribution_scope)
        ) {
          await recommendations.refetch();
          setSelectionMessage("saved");
          return;
        }
        intent = {
          platformId,
          revision: latest.data.revision,
          scope,
          idempotencyKey: createIdempotencyKey(),
        };
        pendingSelection.current = intent;
      }
      const saved = await updateProject.mutateAsync({
        revision: intent.revision,
        settings: { distribution_scope: intent.scope },
        idempotencyKey: intent.idempotencyKey,
      });
      const readback = await project.refetch();
      if (
        readback.isError ||
        !readback.data ||
        readback.data.revision !== saved.revision ||
        JSON.stringify(readback.data.settings.distribution_scope) !==
          JSON.stringify(intent.scope)
      ) {
        setSelectionMessage("unverified");
        return;
      }
      pendingSelection.current = null;
      await recommendations.refetch();
      setSelectionMessage("saved");
    } catch (error) {
      const changed =
        error instanceof ApiError &&
        (error.status === 409 || error.status === 412);
      if (changed) {
        const attempted = pendingSelection.current;
        if (attempted) {
          const current = await project.refetch();
          if (
            current.data &&
            current.data.revision > attempted.revision &&
            JSON.stringify(current.data.settings.distribution_scope) ===
              JSON.stringify(attempted.scope)
          ) {
            pendingSelection.current = null;
            await recommendations.refetch();
            setSelectionMessage("saved");
            return;
          }
        }
        pendingSelection.current = null;
      }
      setSelectionMessage(changed ? "changed" : "failed");
    } finally {
      setSavingPlatform(null);
    }
  }
  const count = (number: number) =>
    new Intl.NumberFormat(i18n.language).format(number);
  const page = insights.data;
  const coverage = page?.coverage;
  return (
    <section className="citation-insights" aria-label={t("title")}>
      <div className="citation-insights-heading">
        <div>
          <h2>{t("title")}</h2>
          <p>{t("description")}</p>
        </div>
        <Button
          onClick={() =>
            void Promise.all([
              insights.refetch(),
              recommendations.refetch(),
              project.refetch(),
            ])
          }
        >
          {t("refresh")}
        </Button>
      </div>
      {insights.isPending ? (
        <LoadingState label={t("loading")} />
      ) : insights.isError ? (
        <ErrorState
          title={t("error")}
          detail={t("errorDetail")}
          onRetry={() => void insights.refetch()}
        />
      ) : (
        page && (
          <>
            <div className="citation-insights-coverage">
              <strong>{t("batch")}</strong>
              <span>
                {t("batchPlans", { count: count(page.plan_ids.length) })}
              </span>
              <span>{t("planned", { count: count(coverage!.planned) })}</span>
              <span>
                {t("live", { count: count(coverage!.observed_live) })}
              </span>
              <span>
                {t("withoutCitations", {
                  count: count(coverage!.observed_without_citations),
                })}
              </span>
              <span>{t("pending", { count: count(coverage!.pending) })}</span>
              <span>
                {t("unverified", {
                  count: count(coverage!.observed_unverified),
                })}
              </span>
              <span>{t("refused", { count: count(coverage!.refused) })}</span>
              <span>{t("missing", { count: count(coverage!.missing) })}</span>
              <span>
                {t("other", { count: count(coverage!.other_completed) })}
              </span>
              {coverage!.fixture > 0 && (
                <span>{t("fixture", { count: count(coverage!.fixture) })}</span>
              )}
            </div>
            {page.invalid_citation_urls > 0 && (
              <p role="status">
                {t("invalid", { count: count(page.invalid_citation_urls) })}
              </p>
            )}
            <section aria-label={t("recommendationsTitle")}>
              <h3>{t("recommendationsTitle")}</h3>
              <p>{t("recommendationsDescription")}</p>
              {selectionMessage && (
                <p role="status">{t(`selection_${selectionMessage}`)}</p>
              )}
              {recommendations.isPending ? (
                <LoadingState label={t("recommendationsLoading")} compact />
              ) : recommendations.isError ? (
                <ErrorState
                  title={t("recommendationsError")}
                  detail={t("recommendationsErrorDetail")}
                  onRetry={() => void recommendations.refetch()}
                />
              ) : recommendations.data?.items.length ? (
                <>
                  <p>
                    {t("recommendationsScope", {
                      count: count(recommendations.data.plan_ids.length),
                    })}
                  </p>
                  <div className="citation-insights-grid">
                    {recommendations.data.items.map(
                      (item: SourceChannelRecommendation) => {
                        const selectedScope =
                          project.data?.settings.distribution_scope;
                        const selected =
                          item.platform_id && selectedScope
                            ? selectedScope.mode === "all_eligible"
                              ? !selectedScope.excluded_platform_ids.includes(
                                  item.platform_id,
                                )
                              : selectedScope.included_platform_ids.includes(
                                  item.platform_id,
                                ) &&
                                !selectedScope.excluded_platform_ids.includes(
                                  item.platform_id,
                                )
                            : item.targeted;
                        return (
                          <Card
                            className="citation-insights-source"
                            key={`${item.platform_id ?? "unmapped"}/${item.placement_slot ?? ""}/${item.source_hosts.join(",")}`}
                          >
                            <h4>
                              {item.platform_id
                                ? t(
                                    platformNameKeys[item.platform_id] ??
                                      "platformOther",
                                    {
                                      name: item.platform_id,
                                    },
                                  )
                                : t("unmapped")}
                            </h4>
                            <p>{item.source_hosts.join(" · ")}</p>
                            {item.placement_slot && (
                              <p>
                                {t("placement", { value: item.placement_slot })}
                              </p>
                            )}
                            <p>
                              {t("cited", {
                                count: count(item.citing_answers),
                              })}
                            </p>
                            <p>
                              {item.platform_id
                                ? selected
                                  ? t("targetSelected")
                                  : t("targetNotSelected")
                                : t("unmappedDetail")}
                            </p>
                            {item.platform_id && (
                              <p>
                                {item.publication.account_ready
                                  ? t("accountReady")
                                  : t("accountUnavailable")}
                                {" · "}
                                {item.publication.connector_availability ===
                                "available"
                                  ? t("connectorAvailable")
                                  : t("connectorUnavailable")}
                                {" · "}
                                {t(
                                  publicationReasonKey(item.publication.reason),
                                )}
                              </p>
                            )}
                            {item.platform_id && !selected && canEdit && (
                              <Button
                                disabled={Boolean(savingPlatform)}
                                onClick={() =>
                                  void selectPlatform(item.platform_id!)
                                }
                              >
                                {savingPlatform === item.platform_id
                                  ? t("savingTarget")
                                  : t("addTarget")}
                              </Button>
                            )}
                            <details>
                              <summary>{t("evidence")}</summary>
                              <ul>
                                {item.samples.map((sample) => (
                                  <SourceSample
                                    key={`${sample.target_id}/${sample.attempt_id}`}
                                    tenantId={tenantId}
                                    projectId={projectId}
                                    sample={sample}
                                  />
                                ))}
                              </ul>
                            </details>
                          </Card>
                        );
                      },
                    )}
                  </div>
                </>
              ) : (
                <p>{t("recommendationsEmpty")}</p>
              )}
            </section>
            {page.observed_sources.length ? (
              <div className="citation-insights-grid">
                {page.observed_sources.map((source) => (
                  <SourceCard
                    key={source.host}
                    source={source}
                    tenantId={tenantId}
                    projectId={projectId}
                  />
                ))}
              </div>
            ) : (
              <p className="citation-insights-empty">{t("empty")}</p>
            )}
            {(previous.length > 0 || page.next_after) && (
              <nav
                className="citation-insights-pagination"
                aria-label={t("title")}
              >
                {previous.length > 0 && (
                  <Button
                    onClick={() => {
                      setAfter(previous.at(-1));
                      setPrevious((items) => items.slice(0, -1));
                      setSelectionMessage(null);
                    }}
                  >
                    {t("previous")}
                  </Button>
                )}
                {page.next_after && (
                  <Button
                    onClick={() => {
                      setPrevious((items) => [...items, after]);
                      setAfter(page.next_after!);
                      setSelectionMessage(null);
                    }}
                  >
                    {t("next")}
                  </Button>
                )}
              </nav>
            )}
          </>
        )
      )}
    </section>
  );
}

/** A changed scope remounts the cursor so previous batches cannot leak into a new project/session. */
export function CitationInsightsPanel({
  tenantId,
  projectId,
}: {
  tenantId: string;
  projectId: string;
}) {
  const { session } = useAuth();
  return (
    <CitationInsightsContent
      key={`${session?.user.id ?? ""}/${session?.operator.id ?? ""}/${tenantId}/${projectId}`}
      tenantId={tenantId}
      projectId={projectId}
    />
  );
}
