import {
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableHeaderCell,
  TableRow,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import "../i18n/observationAnalysis";

interface Recommendation {
  name: string;
  kind: "product" | "restaurant" | "brand" | "service" | "other";
  mention_order: number;
  rank: number | null;
  reason: string | null;
  evidence_quote: string;
  citation_urls: string[];
}

function isRecommendation(value: unknown): value is Recommendation {
  if (!value || typeof value !== "object") return false;
  const item = value as Record<string, unknown>;
  return (
    typeof item.name === "string" &&
    item.name.trim().length > 0 &&
    ["product", "restaurant", "brand", "service", "other"].includes(
      item.kind as string,
    ) &&
    Number.isInteger(item.mention_order) &&
    (item.mention_order as number) > 0 &&
    (item.rank === null ||
      (Number.isInteger(item.rank) && (item.rank as number) > 0)) &&
    (item.reason === null || typeof item.reason === "string") &&
    typeof item.evidence_quote === "string" &&
    Array.isArray(item.citation_urls) &&
    item.citation_urls.every((url) => typeof url === "string")
  );
}

/** Render model-extracted entities only; never infer names or rank from prose. */
export function RecommendationResults({ value }: { value: unknown }) {
  const { t } = useTranslation("observationAnalysis");
  // Older saved analyses have no entity extraction, not an empty recommendation.
  if (value == null) return null;
  const result = value as Record<string, unknown>;
  const verified =
    result.version === "geo.recommendations.v1" &&
    result.status === "verified" &&
    Array.isArray(result.items) &&
    result.items.every(isRecommendation);
  const items = verified
    ? [...(result.items as Recommendation[])].sort(
        (left, right) => left.mention_order - right.mention_order,
      )
    : [];
  return (
    <section aria-label={t("recommendationsTitle")}>
      <h5>{t("recommendationsTitle")}</h5>
      {!verified ? (
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
