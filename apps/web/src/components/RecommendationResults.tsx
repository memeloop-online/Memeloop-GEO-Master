import {
  Badge,
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableHeaderCell,
  TableRow,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import type { ReactNode } from "react";
import { decodeVerifiedRecommendations } from "../api/recommendations";
import { isMeasurementFocus } from "../api/measurementFocus";
import type { MeasurementTarget } from "../api/projects";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import "../i18n/observationAnalysis";
import "../i18n/measurementFocus";
import { RecommendationFocusComparison } from "./RecommendationFocusComparison";

function FullRecommendationList({
  collapsed,
  children,
}: {
  collapsed: boolean;
  children: ReactNode;
}) {
  const { t } = useTranslation("recommendationFocusComparison");
  const list = <div style={{ overflowX: "auto" }}>{children}</div>;
  return collapsed ? (
    <details style={{ marginTop: 12 }}>
      <summary style={{ cursor: "pointer" }}>{t("fullList")}</summary>
      {list}
    </details>
  ) : (
    list
  );
}

/** Render model-extracted entities only; never infer names or rank from prose. */
export function RecommendationResults({
  value,
  focusTargets = [],
}: {
  value: unknown;
  focusTargets?: readonly MeasurementTarget[];
}) {
  const { t } = useTranslation("observationAnalysis");
  const { t: focusT } = useTranslation("measurementFocus");
  const decoded = decodeVerifiedRecommendations(value);
  const items = [...(decoded ?? [])].sort(
    (left, right) => left.mention_order - right.mention_order,
  );
  const noFocusMatch =
    focusTargets.length > 0 &&
    items.length > 0 &&
    !items.some((item) => isMeasurementFocus(item, focusTargets));
  return (
    <section aria-label={t("recommendationsTitle")}>
      <h5>{t("recommendationsTitle")}</h5>
      {decoded !== null &&
        focusTargets
          .filter(
            (target) =>
              !items.some((item) => isMeasurementFocus(item, [target])),
          )
          .map((target) => (
            <div
              key={JSON.stringify([target.kind, target.name])}
              style={{ marginBottom: 12 }}
            >
              <strong>
                {focusT("resultTitle", { name: target.name })} ·{" "}
                {focusT(`kind_${target.kind}`)}
              </strong>
              <p>{focusT("notFound", { name: target.name })}</p>
            </div>
          ))}
      {decoded !== null && (
        <RecommendationFocusComparison
          items={items}
          focusTargets={focusTargets}
        />
      )}
      {value == null ? (
        <p>{t("recommendationsNotAnalyzed")}</p>
      ) : decoded === null ? (
        <p>{t("recommendationsUnverified")}</p>
      ) : !items.length ? (
        <p>{t("recommendationsEmpty")}</p>
      ) : (
        <FullRecommendationList collapsed={noFocusMatch}>
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
                    <TableCell>
                      {item.name}{" "}
                      {isMeasurementFocus(item, focusTargets) && (
                        <Badge appearance="tint">{focusT("focused")}</Badge>
                      )}
                    </TableCell>
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
        </FullRecommendationList>
      )}
    </section>
  );
}
