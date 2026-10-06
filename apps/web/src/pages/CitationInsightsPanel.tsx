import { useState } from "react";
import { Button, Card } from "@fluentui/react-components";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth/AuthProvider";
import {
  getCitationInsights,
  type CitationSample,
  type ObservedCitationSource,
} from "../api/citationInsights";
import { getChannelTarget } from "../api/channelJobs";
import { ErrorState, LoadingState } from "../components/AsyncState";
import "../i18n";
import "./citationInsightsMessages";
import "./CitationInsightsPanel.css";

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
        <Button onClick={() => void insights.refetch()}>{t("refresh")}</Button>
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
