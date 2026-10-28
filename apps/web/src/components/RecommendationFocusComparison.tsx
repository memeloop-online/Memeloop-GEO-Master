import { Badge, Button, Card, tokens } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import {
  classifyBrandRecommendation,
  isMeasurementFocus,
} from "../api/measurementFocus";
import type { MeasurementTarget } from "../api/projects";
import type { RecommendationItem } from "../api/recommendations";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import "../i18n/recommendationFocusComparison";
import { RecommendationBrandAttribution } from "./RecommendationBrandAttribution";

function RecommendationCard({
  item,
  focused = false,
  showKind = false,
  onAnalyze,
  analysisPending,
  compact = false,
}: {
  item: RecommendationItem;
  focused?: boolean;
  showKind?: boolean;
  onAnalyze?: (item: RecommendationItem) => void;
  analysisPending?: boolean;
  compact?: boolean;
}) {
  const { t } = useTranslation("recommendationFocusComparison");
  const urls = [
    ...new Set(
      item.citation_urls
        .map(safeOriginalPublicUrl)
        .filter((url): url is string => url !== null),
    ),
  ];
  return (
    <Card
      role="article"
      aria-label={item.name}
      style={{ minWidth: 0, overflowWrap: "anywhere" }}
    >
      <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
        <strong>{item.name}</strong>
        {focused && <Badge appearance="tint">{t("focused")}</Badge>}
        {showKind && (
          <Badge appearance="outline">{t(`kind_${item.kind}`)}</Badge>
        )}
        <span>
          {item.rank === null
            ? t("unranked")
            : t("ranked", { rank: item.rank })}
        </span>
        {compact && <span>{t("citationCount", { count: urls.length })}</span>}
      </div>
      <p style={{ margin: 0 }}>{item.reason || t("reasonMissing")}</p>
      {!compact && <RecommendationBrandAttribution item={item} />}
      <details>
        <summary style={{ cursor: "pointer" }}>{t("evidence")}</summary>
        {compact && <RecommendationBrandAttribution item={item} />}
        <blockquote
          style={{
            margin: "8px 0",
            paddingLeft: 12,
            borderLeft: `2px solid ${tokens.colorNeutralStroke2}`,
            whiteSpace: "pre-wrap",
          }}
        >
          {item.evidence_quote}
        </blockquote>
        {urls.length > 0 && (
          <ul style={{ margin: "8px 0", paddingLeft: 20 }}>
            {urls.map((url) => (
              <li key={url}>
                <a href={url} target="_blank" rel="noopener noreferrer">
                  {url}
                </a>
              </li>
            ))}
          </ul>
        )}
        {compact && onAnalyze && (
          <Button disabled={analysisPending} onClick={() => onAnalyze(item)}>
            {t("analyzeImprovements")}
          </Button>
        )}
      </details>
      {!compact && onAnalyze && (
        <Button disabled={analysisPending} onClick={() => onAnalyze(item)}>
          {t("analyzeImprovements")}
        </Button>
      )}
    </Card>
  );
}

/** Show verified recommendations; compare explicit ranks only for exact focus matches. */
export function RecommendationFocusComparison({
  items,
  focusTargets,
  onAnalyze,
  analysisPending,
  compact = false,
}: {
  items: RecommendationItem[];
  focusTargets: readonly MeasurementTarget[];
  onAnalyze?: (item: RecommendationItem) => void;
  analysisPending?: boolean;
  compact?: boolean;
}) {
  const { t } = useTranslation("recommendationFocusComparison");
  const { t: focusT } = useTranslation("measurementFocus");
  if (compact) {
    const ordered = [...items].sort(
      (a, b) => a.mention_order - b.mention_order,
    );
    const card = (item: RecommendationItem, index: number) => (
      <RecommendationCard
        key={`${item.kind}/${item.mention_order}/${index}`}
        item={item}
        focused={isMeasurementFocus(item, focusTargets)}
        showKind
        compact
        onAnalyze={onAnalyze}
        analysisPending={analysisPending}
      />
    );
    return (
      <section
        aria-label={t("recommendedTitle")}
        style={{ display: "grid", gap: 8, minWidth: 0 }}
      >
        {focusTargets.map((target) => {
          const matches = items.filter((item) =>
            isMeasurementFocus(item, [target]),
          );
          const ranks = new Set(matches.map((item) => item.rank));
          const rank = matches[0]?.rank ?? null;
          return (
            <p
              key={JSON.stringify([target.kind, target.name])}
              style={{ margin: 0 }}
            >
              <strong>{target.name}</strong>
              {" · "}
              {target.kind === "brand" && (
                <>
                  {focusT("brandDirect")}
                  {" · "}
                </>
              )}
              {!matches.length
                ? focusT("notFound", { name: target.name })
                : ranks.size > 1
                  ? t("ambiguous")
                  : rank === null
                    ? t("unranked")
                    : t("ranked", { rank })}
            </p>
          );
        })}
        {ordered.slice(0, 3).map(card)}
        {ordered.length > 3 && (
          <details>
            <summary style={{ cursor: "pointer" }}>
              {t("showAll", { count: ordered.length })}
            </summary>
            <div style={{ display: "grid", gap: 8, marginTop: 8 }}>
              {ordered.slice(3).map(card)}
            </div>
          </details>
        )}
      </section>
    );
  }
  const focusGroups = focusTargets
    .map((target) => ({
      target,
      matches: items.filter((item) => isMeasurementFocus(item, [target])),
      affiliates: items.filter(
        (item) => classifyBrandRecommendation([item], target) === "positive",
      ),
    }))
    .filter(
      ({ matches, affiliates }) => matches.length > 0 || affiliates.length > 0,
    );
  if (!focusTargets.length || !items.length) return null;
  if (!focusGroups.length) {
    return (
      <section
        aria-label={t("recommendedTitle")}
        style={{ display: "grid", gap: 12, minWidth: 0 }}
      >
        <h5 style={{ margin: 0 }}>{t("recommendedTitle")}</h5>
        <p style={{ margin: 0 }}>{t("recommendedScope")}</p>
        {[...items]
          .sort((left, right) => left.mention_order - right.mention_order)
          .map((item, index) => (
            <RecommendationCard
              key={index}
              item={item}
              showKind
              onAnalyze={onAnalyze}
              analysisPending={analysisPending}
            />
          ))}
      </section>
    );
  }

  return (
    <section
      aria-label={t("title")}
      style={{ display: "grid", gap: 12, minWidth: 0 }}
    >
      <h5 style={{ margin: 0 }}>{t("title")}</h5>
      <p style={{ margin: 0 }}>{t("scope")}</p>
      {focusGroups.map(({ target: focus, matches, affiliates }) => {
        const key = JSON.stringify([focus.kind, focus.name]);
        const ambiguous = new Set(matches.map((item) => item.rank)).size > 1;
        const rank = matches[0]?.rank ?? null;
        const ahead =
          !ambiguous && rank !== null
            ? items
                .filter(
                  (item) =>
                    item.kind === focus.kind &&
                    item.rank !== null &&
                    item.rank < rank &&
                    !isMeasurementFocus(item, [focus]),
                )
                .sort((left, right) => left.rank! - right.rank!)
            : [];
        return (
          <section
            key={key}
            aria-label={t("group", {
              name: focus.name.trim(),
              kind: t(`kind_${focus.kind}`),
            })}
            style={{ display: "grid", gap: 8, minWidth: 0 }}
          >
            <h6 style={{ margin: 0, fontSize: "inherit" }}>
              {t("group", {
                name: focus.name.trim(),
                kind: t(`kind_${focus.kind}`),
              })}
            </h6>
            {focus.kind === "brand" && <strong>{focusT("brandDirect")}</strong>}
            {!matches.length && <p>{focusT("brandDirectAbsent")}</p>}
            {matches.map((item, index) => (
              <RecommendationCard
                key={index}
                item={item}
                focused
                onAnalyze={onAnalyze}
                analysisPending={analysisPending}
              />
            ))}
            {matches.length === 0 ? null : ambiguous ? (
              <p style={{ margin: 0 }}>{t("ambiguous")}</p>
            ) : rank === null ? (
              <p style={{ margin: 0 }}>{t("cannotCompare")}</p>
            ) : ahead.length ? (
              <>
                <h6 style={{ margin: "4px 0 0", fontSize: "inherit" }}>
                  {t("ahead")}
                </h6>
                {ahead.map((item, index) => (
                  <RecommendationCard
                    key={index}
                    item={item}
                    onAnalyze={onAnalyze}
                    analysisPending={analysisPending}
                  />
                ))}
              </>
            ) : (
              <p style={{ margin: 0 }}>{t("noneAhead")}</p>
            )}
            {affiliates.length > 0 && (
              <section
                aria-label={focusT("brandAffiliateTitle")}
                style={{ display: "grid", gap: 8 }}
              >
                <h6 style={{ margin: 0, fontSize: "inherit" }}>
                  {focusT("brandAffiliateTitle")}
                </h6>
                {affiliates.map((item, index) => (
                  <RecommendationCard
                    key={index}
                    item={item}
                    showKind
                    onAnalyze={onAnalyze}
                    analysisPending={analysisPending}
                  />
                ))}
              </section>
            )}
          </section>
        );
      })}
    </section>
  );
}
