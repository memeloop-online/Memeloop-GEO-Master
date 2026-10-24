import { Badge, Button, Card, tokens } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import { isMeasurementFocus } from "../api/measurementFocus";
import type { RecommendationItem } from "../api/recommendations";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import "../i18n/recommendationFocusComparison";

function RecommendationCard({
  item,
  focused = false,
  showKind = false,
  onAnalyze,
  analysisPending,
}: {
  item: RecommendationItem;
  focused?: boolean;
  showKind?: boolean;
  onAnalyze?: (item: RecommendationItem) => void;
  analysisPending?: boolean;
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
      </div>
      <p style={{ margin: 0 }}>{item.reason || t("reasonMissing")}</p>
      <details>
        <summary style={{ cursor: "pointer" }}>{t("evidence")}</summary>
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
      </details>
      {onAnalyze && (
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
}: {
  items: RecommendationItem[];
  focusTargets: readonly string[];
  onAnalyze?: (item: RecommendationItem) => void;
  analysisPending?: boolean;
}) {
  const { t } = useTranslation("recommendationFocusComparison");
  const focusGroups = new Map<string, RecommendationItem[]>();
  for (const item of items) {
    if (!isMeasurementFocus(item, focusTargets)) continue;
    const key = JSON.stringify([item.name.trim(), item.kind]);
    const group = focusGroups.get(key) ?? [];
    group.push(item);
    focusGroups.set(key, group);
  }
  if (!focusTargets.length || !items.length) return null;
  if (!focusGroups.size) {
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
      {[...focusGroups].map(([key, matches]) => {
        const focus = matches[0];
        const ambiguous = new Set(matches.map((item) => item.rank)).size > 1;
        const rank = focus.rank;
        const ahead =
          !ambiguous && rank !== null
            ? items
                .filter(
                  (item) =>
                    item.kind === focus.kind &&
                    item.rank !== null &&
                    item.rank < rank &&
                    item.name.trim() !== focus.name.trim(),
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
            {matches.map((item, index) => (
              <RecommendationCard
                key={index}
                item={item}
                focused
                onAnalyze={onAnalyze}
                analysisPending={analysisPending}
              />
            ))}
            {ambiguous ? (
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
          </section>
        );
      })}
    </section>
  );
}
