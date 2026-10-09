import { Card } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { useParams } from "react-router-dom";
import type { ReportSupplementaryMeasurement } from "../api/reports";
import "../i18n/reportAnalysis";

export function ReportSupplementaryMeasurements({
  items,
  timezone,
}: {
  items: ReportSupplementaryMeasurement[];
  timezone: string;
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
      <h2>{t("title")}</h2>
      <p>{t("count", { count: items.length })}</p>
      <p>{t("note")}</p>
      {items.map((item) => {
        const { observation } = item;
        const analysis = observation.provenance;
        return (
          <Card
            key={`${item.target_id}/${item.attempt_id}/${analysis?.revision_id ?? ""}`}
            className="report-panel"
          >
            <h3>{item.comparison_key}</h3>
            <p>
              {t(
                item.question_binding?.purpose === "frozen_evaluation"
                  ? "evaluation"
                  : item.question_binding?.purpose === "optimization"
                    ? "optimization"
                    : "unclassified",
              )}
            </p>
            <dl className="report-metadata">
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
                    /* Preserve non-link evidence as text. */
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
            {item.plan_id && tenantId && projectId && (
              <a
                href={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/measurement?tab=records&record=${encodeURIComponent(item.plan_id)}`}
              >
                {t("record")}
              </a>
            )}
          </Card>
        );
      })}
    </section>
  );
}
