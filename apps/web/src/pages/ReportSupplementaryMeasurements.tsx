import { Card } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { useParams } from "react-router-dom";
import type { ReportSupplementaryMeasurement } from "../api/reports";
import "../i18n/reportAnalysis";
import { reportMeasurementTitle } from "./reportMeasurementTitle";

export function ReportSupplementaryMeasurements({
  items,
  timezone,
  embedded = false,
}: {
  items: ReportSupplementaryMeasurement[];
  timezone: string;
  embedded?: boolean;
}) {
  const { t, i18n } = useTranslation("reportAnalysis");
  const { tenantId, projectId } = useParams();
  const at = (value: string) => {
    try {
      return new Intl.DateTimeFormat(i18n.language, {
        dateStyle: "medium",
        timeStyle: "short",
        timeZone: timezone,
      }).format(new Date(value));
    } catch {
      return value;
    }
  };
  return (
    <section className="report-section" aria-label={t("title")}>
      {!embedded && (
        <>
          <h2>{t("title")}</h2>
          <p>{t("count", { count: items.length })}</p>
          <p>{t("note")}</p>
        </>
      )}
      {items.map((item) => {
        const { observation } = item;
        const analysis = observation.provenance;
        const Container = embedded ? "div" : Card;
        return (
          <Container
            key={`${item.target_id}/${item.attempt_id}/${analysis?.revision_id ?? ""}`}
            className="report-panel"
          >
            {!embedded && (
              <h3>{reportMeasurementTitle(item.comparison_key)}</h3>
            )}
            <p>
              {t("observed")}：{at(observation.observed_at)} · {timezone}
            </p>
            <h4>{t("answer")}</h4>
            <p style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>
              {observation.raw_answer}
            </p>
            <h4>{t("citations")}</h4>
            {observation.citations.length ? (
              <ul>
                {observation.citations.map((url, index) => {
                  let href: string | undefined;
                  try {
                    const parsed = new URL(url);
                    if (
                      ["https:", "http:"].includes(parsed.protocol) &&
                      !parsed.username &&
                      !parsed.password
                    )
                      href = parsed.href;
                  } catch {
                    /* Retain non-link evidence as text. */
                  }
                  return (
                    <li key={`${index}/${url}`}>
                      {href ? (
                        <a
                          href={href}
                          target="_blank"
                          rel="noopener noreferrer"
                        >
                          {url}
                        </a>
                      ) : (
                        url
                      )}
                    </li>
                  );
                })}
              </ul>
            ) : (
              <p>{t("noCitations")}</p>
            )}
            <details>
              <summary>{t("recordDetails")}</summary>
              <p>
                {t(
                  item.question_binding?.purpose === "frozen_evaluation"
                    ? "evaluation"
                    : item.question_binding?.purpose === "optimization"
                      ? "optimization"
                      : "unclassified",
                )}
              </p>
              <p>{timezone}</p>
              <dl className="report-metadata">
                <dt>{t("comparison")}</dt>
                <dd>{item.comparison_key}</dd>
                <dt>{t("target")}</dt>
                <dd>{item.target_id}</dd>
                <dt>{t("attempt")}</dt>
                <dd>{item.attempt_id}</dd>
                {item.plan_id && (
                  <>
                    <dt>{t("plan")}</dt>
                    <dd>{item.plan_id}</dd>
                  </>
                )}
                <dt>{t("observed")}</dt>
                <dd>{at(observation.observed_at)}</dd>
                <dt>{t("received")}</dt>
                <dd>{at(observation.received_at)}</dd>
                {analysis && (
                  <>
                    <dt>{t("analyzed")}</dt>
                    <dd>{at(analysis.analyzed_at)}</dd>
                    <dt>{t("model")}</dt>
                    <dd>{analysis.actual_model}</dd>
                    {analysis.config_revision !== null && (
                      <>
                        <dt>{t("config")}</dt>
                        <dd>{analysis.config_revision}</dd>
                      </>
                    )}
                    <dt>{t("revision")}</dt>
                    <dd>{analysis.revision_id}</dd>
                    <dt>{t("source")}</dt>
                    <dd>
                      {analysis.source.kind === "capture"
                        ? analysis.source.capture_id
                        : analysis.source.evidence_index}
                    </dd>
                    <dt>{t("digest")}</dt>
                    <dd>{analysis.source_sha256}</dd>
                    <dt>{t("parser")}</dt>
                    <dd>{analysis.parser_version}</dd>
                    <dt>{t("prompt")}</dt>
                    <dd>{analysis.prompt_version}</dd>
                  </>
                )}
              </dl>
            </details>
            {!embedded && item.plan_id && tenantId && projectId && (
              <a
                href={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/measurement?tab=records&record=${encodeURIComponent(item.plan_id)}`}
              >
                {t("record")}
              </a>
            )}
          </Container>
        );
      })}
    </section>
  );
}
