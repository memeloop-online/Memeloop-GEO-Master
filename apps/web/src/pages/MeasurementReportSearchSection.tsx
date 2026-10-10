import { Card } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import type { MeasurementPeriodSearchSection } from "../api/measurementReports";
import { safeOriginalPublicUrl } from "./PublicationLookupPanel";
import "../i18n/measurementReports";
import "../i18n/serp";

const statuses = new Set([
  "observed",
  "partial",
  "challenge",
  "login_required",
  "missing",
  "failed",
  "unsupported",
]);
const kinds = new Set([
  "organic",
  "advertisement",
  "featured_snippet",
  "maps",
  "ai_overview",
  "other",
]);

export function MeasurementReportSearchSection({
  search,
  tenantId,
  projectId,
  at,
}: {
  search?: MeasurementPeriodSearchSection | null;
  tenantId: string;
  projectId: string;
  at: (value: string) => string;
}) {
  const { t, i18n } = useTranslation("measurementReports");
  const status = (value: string) =>
    value === "no_eligible_observation"
      ? t("searchNoEvidence")
      : statuses.has(value)
        ? t(`statuses.${value}`, { ns: "serp" })
        : t("status_other");
  return (
    <section aria-label={t("searchTitle")}>
      <h3>{t("searchTitle")}</h3>
      {!search ? (
        <p>{t("searchNotIncluded")}</p>
      ) : (
        <>
          <p>{t("searchPlanned", { count: search.coverage.planned })}</p>
          <ul>
            {Object.entries(search.coverage.counts).map(([key, count]) => (
              <li key={key}>
                {status(key)}: {count}
              </li>
            ))}
          </ul>
          {!search.samples.length && <p>{t("searchEmpty")}</p>}
          {search.samples.map(({ cohort, evidence }) => {
            const observation = evidence?.observation;
            const partial =
              observation &&
              (observation.status === "partial" ||
                observation.coverage.truncated ||
                !["requested_depth", "provider_exhausted"].includes(
                  observation.coverage.completion,
                ));
            const match = evidence?.target_match;
            return (
              <Card className="report-panel" key={cohort.measurement_id}>
                <h4
                  style={{ overflowWrap: "anywhere", whiteSpace: "pre-wrap" }}
                >
                  {cohort.query}
                </h4>
                <p>{t("searchSource", { source: cohort.source_key })}</p>
                <p>{t("requested", { ns: "serp", ...cohort.protocol })}</p>
                <p>{t("scheduled", { time: at(cohort.scheduled_at) })}</p>
                {cohort.target && (
                  <p>
                    {t("searchTarget", {
                      target:
                        cohort.target.kind === "host"
                          ? cohort.target.host
                          : cohort.target.url,
                    })}
                  </p>
                )}
                <p>
                  {status(observation?.status ?? "no_eligible_observation")}
                </p>
                <a
                  href={`/app/${encodeURIComponent(tenantId)}/${encodeURIComponent(projectId)}/measurement?tab=search&searchRecord=${encodeURIComponent(cohort.measurement_id)}`}
                >
                  {t("searchRecord")}
                </a>
                {evidence && observation && (
                  <>
                    <p>
                      {t(
                        evidence.evidence_time_basis === "provider_observed_at"
                          ? "searchProviderTime"
                          : "searchReceiptTime",
                        { time: at(evidence.evidence_time) },
                      )}
                    </p>
                    <p>
                      {t("depth", {
                        ns: "serp",
                        observed: observation.coverage.observed_organic_depth,
                        requested: observation.coverage.requested_depth,
                      })}
                    </p>
                    {partial && <p>{t("partial", { ns: "serp" })}</p>}
                    {match?.status === "hit" && (
                      <p>
                        {t("searchTargetHit", {
                          ranks: match.organic_ranks.join(", "),
                        })}
                      </p>
                    )}
                    {match?.status === "not_found_within_depth" && (
                      <p>
                        {t("searchTargetNotFound", {
                          depth: match.covered_depth,
                        })}
                      </p>
                    )}
                    {match?.status === "undetermined" && (
                      <p>{t("searchTargetUndetermined")}</p>
                    )}
                    {!observation.results.length && (
                      <p>{t("noObservation", { ns: "serp" })}</p>
                    )}
                    <ul>
                      {observation.results.map((result, index) => {
                        const href = safeOriginalPublicUrl(
                          result.normalized_url ?? result.raw_url,
                        );
                        return (
                          <li
                            key={`${result.locator}/${index}`}
                            style={{ overflowWrap: "anywhere" }}
                          >
                            {href ? (
                              <a
                                href={href}
                                target="_blank"
                                rel="noopener noreferrer"
                              >
                                {result.title || href}
                              </a>
                            ) : (
                              <span>
                                {result.title ||
                                  result.raw_url ||
                                  t("searchResult")}
                              </span>
                            )}
                            {" · "}
                            {t(
                              `kinds.${kinds.has(result.kind) ? result.kind : "other"}`,
                              { ns: "serp" },
                            )}
                            {" · "}
                            {result.kind === "organic"
                              ? result.organic_rank == null
                                ? t("rankUnknown", { ns: "serp" })
                                : t("searchRank", { rank: result.organic_rank })
                              : t("notOrganic", { ns: "serp" })}
                          </li>
                        );
                      })}
                    </ul>
                    {observation.source_limitations.length > 0 && (
                      <ul>
                        {observation.source_limitations.map((limitation) => (
                          <li key={limitation}>
                            {i18n.exists(`limitations.${limitation}`, {
                              ns: "serp",
                            })
                              ? t(`limitations.${limitation}`, { ns: "serp" })
                              : t("searchLimitation")}
                          </li>
                        ))}
                      </ul>
                    )}
                  </>
                )}
                <details>
                  <summary>{t("searchDetails")}</summary>
                  <pre
                    style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}
                  >
                    {JSON.stringify({ cohort, evidence }, null, 2)}
                  </pre>
                </details>
              </Card>
            );
          })}
        </>
      )}
    </section>
  );
}
