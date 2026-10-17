import {
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableHeaderCell,
  TableRow,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { decodeVerifiedRecommendations } from "../api/recommendations";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import "../i18n/observationAnalysis";

/** Render model-extracted entities only; never infer names or rank from prose. */
export function RecommendationResults({ value }: { value: unknown }) {
  const { t } = useTranslation("observationAnalysis");
  const decoded = decodeVerifiedRecommendations(value);
  const items = [...(decoded ?? [])].sort(
    (left, right) => left.mention_order - right.mention_order,
  );
  return (
    <section aria-label={t("recommendationsTitle")}>
      <h5>{t("recommendationsTitle")}</h5>
      {value == null ? (
        <p>{t("recommendationsNotAnalyzed")}</p>
      ) : decoded === null ? (
        <p>{t("recommendationsUnverified")}</p>
      ) : !items.length ? (
        <p>{t("recommendationsEmpty")}</p>
      ) : (
        <div style={{ overflowX: "auto" }}>
          <Table aria-label={t("recommendationsTitle")}>
            <TableHeader>
              <TableRow>
                <TableHeaderCell>{t("recommendationName")}</TableHeaderCell>
                <TableHeaderCell>{t("recommendationRank")}</TableHeaderCell>
                <TableHeaderCell>{t("recommendationOrder")}</TableHeaderCell>
                <TableHeaderCell>{t("recommendationReason")}</TableHeaderCell>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((item, index) => {
                const urls = [
                  ...new Set(
                    item.citation_urls
                      .map(safeOriginalPublicUrl)
                      .filter((url): url is string => url !== null),
                  ),
                ];
                return (
                  <TableRow key={`${item.mention_order}-${index}`}>
                    <TableCell>{item.name}</TableCell>
                    <TableCell>
                      {item.rank ?? t("recommendationRankUnknown")}
                    </TableCell>
                    <TableCell>{item.mention_order}</TableCell>
                    <TableCell>
                      <p>{item.reason || t("recommendationReasonMissing")}</p>
                      <details>
                        <summary>{t("recommendationEvidence")}</summary>
                        <blockquote style={{ whiteSpace: "pre-wrap" }}>
                          {item.evidence_quote}
                        </blockquote>
                        {urls.length > 0 && (
                          <ul>
                            {urls.map((url) => (
                              <li
                                key={url}
                                style={{ overflowWrap: "anywhere" }}
                              >
                                <a
                                  href={url}
                                  target="_blank"
                                  rel="noopener noreferrer"
                                >
                                  {url}
                                </a>
                              </li>
                            ))}
                          </ul>
                        )}
                      </details>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </section>
  );
}
