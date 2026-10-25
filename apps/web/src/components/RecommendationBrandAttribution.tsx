import { useTranslation } from "react-i18next";
import type { BrandRecommendationSummary } from "../api/measurementFocus";
import type { RecommendationItem } from "../api/recommendations";
import "../i18n/measurementFocus";

/** Attribution excerpts stay attached to the product they describe. */
export function RecommendationBrandAttribution({
  item,
}: {
  item: RecommendationItem;
}) {
  const { t } = useTranslation("measurementFocus");
  if (item.kind !== "product") return null;
  return item.brand_attribution ? (
    <div style={{ overflowWrap: "anywhere" }}>
      <p>{t("brandAttribution", { name: item.brand_attribution.name })}</p>
      <details>
        <summary>{t("brandAttributionEvidence")}</summary>
        <blockquote style={{ whiteSpace: "pre-wrap" }}>
          {item.brand_attribution.evidence_quote}
        </blockquote>
      </details>
    </div>
  ) : (
    <p>{t("brandAttributionUnknown")}</p>
  );
}

/** Counts answers once; deliberately presents no affiliate percentage or rank. */
export function RecommendationBrandSummary({
  summary,
}: {
  summary: BrandRecommendationSummary | null;
}) {
  const { t, i18n } = useTranslation("measurementFocus");
  if (!summary) return null;
  const number = new Intl.NumberFormat(i18n.resolvedLanguage || i18n.language);
  return (
    <div role="group" aria-label={t("brandAffiliateTitle")}>
      <strong>{t("brandAffiliateTitle")}</strong>
      <p>
        {t("brandCounts", {
          positive: number.format(summary.positiveAnswers),
          known: number.format(summary.knownAnswers),
          unknown: number.format(summary.unknownAnswers),
          valid: number.format(summary.validAnswers),
        })}
      </p>
      <small>{t("brandCountsBasis")}</small>
    </div>
  );
}
