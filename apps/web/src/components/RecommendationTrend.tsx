import {
  Badge,
  Card,
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableHeaderCell,
  TableRow,
  Text,
  tokens,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import type { ChannelTargetInput } from "../api/channelJobs";
import {
  classifyBrandRecommendation,
  isMeasurementFocus,
} from "../api/measurementFocus";
import type { MeasurementTarget } from "../api/projects";
import {
  decodeVerifiedRecommendations,
  type RecommendationItem,
} from "../api/recommendations";
import {
  comparisonKey,
  recommendationFrequency,
} from "./recommendationComparison";
import "../i18n/recommendationTrend";
import "./RecommendationTrend.css";
import {
  RecommendationBrandAttribution,
  RecommendationBrandSummary,
} from "./RecommendationBrandAttribution";
import "../i18n/measurementFocus";

type FullMeasurementInput = Extract<ChannelTargetInput, { kind: "measure" }>;
type MeasurementInput = Omit<
  FullMeasurementInput,
  "kind" | "account_id" | "scheduled_at" | "sample_ordinal"
> &
  Partial<FullMeasurementInput>;

/** One persisted measurement sample, using one selected analysis revision. */
export interface RecommendationTrendSample {
  sampleId: string;
  recordHref: string;
  observedAt: string | null;
  measurement: MeasurementInput;
  analysisRevisionId?: string;
  /** The selected grounded analysis's outcome.audit.recommendations. */
  recommendations: unknown;
}

const entityKey = (item: RecommendationItem) =>
  JSON.stringify([item.kind, item.name]);

const observedTime = (sample: RecommendationTrendSample) =>
  sample.observedAt === null ? NaN : Date.parse(sample.observedAt);

type TrendEntry = {
  sample: RecommendationTrendSample;
  items: RecommendationItem[] | null;
};

function entityState(entry: TrendEntry, target: MeasurementTarget) {
  if (entry.items === null)
    return {
      label:
        entry.sample.recommendations == null ? "notAnalyzed" : "unverified",
    };
  const matches = entry.items.filter((item) =>
    isMeasurementFocus(item, [target]),
  );
  if (!matches.length)
    return {
      label:
        classifyBrandRecommendation(entry.items, target) === "positive"
          ? "affiliateOnly"
          : target.kind === "brand"
            ? "directBrandAbsent"
            : "absent",
    };
  if (new Set(matches.map((item) => item.rank)).size > 1)
    return { label: "ambiguous" };
  return matches[0].rank === null
    ? { label: "unranked" }
    : { label: "rank", rank: matches[0].rank };
}

/** Compare verified extracted recommendations; never derive entities or ranks from prose. */
export function RecommendationTrend({
  samples,
  focusTargets = [],
  embedded = false,
}: {
  samples: readonly RecommendationTrendSample[];
  focusTargets?: readonly MeasurementTarget[];
  embedded?: boolean;
}) {
  const { t, i18n } = useTranslation("recommendationTrend");
  const { t: focusT } = useTranslation("measurementFocus");
  const number = new Intl.NumberFormat(i18n.resolvedLanguage || i18n.language);
  const percent = new Intl.NumberFormat(
    i18n.resolvedLanguage || i18n.language,
    {
      style: "percent",
      maximumFractionDigits: 1,
    },
  );
  const date = new Intl.DateTimeFormat(i18n.resolvedLanguage || i18n.language, {
    dateStyle: "medium",
    timeStyle: "short",
  });
  const groups = new Map<string, RecommendationTrendSample[]>();
  for (const sample of samples) {
    const key = comparisonKey(sample.measurement);
    const group = groups.get(key) ?? [];
    group.push(sample);
    groups.set(key, group);
  }

  return (
    <section className="recommendation-trend" aria-label={t("title")}>
      {!embedded && <h3>{t("title")}</h3>}
      {!samples.length ? (
        <p>{t("empty")}</p>
      ) : (
        <>
          {[...groups].map(([key, group]) => {
            const ordered = [...group].sort((left, right) => {
              const difference = observedTime(left) - observedTime(right);
              return Number.isFinite(difference) ? difference : 0;
            });
            const entries = ordered.map((sample) => ({
              sample,
              items: decodeVerifiedRecommendations(sample.recommendations),
            }));
            const entities = new Map<string, RecommendationItem>();
            for (const { items } of entries) {
              for (const item of [...(items ?? [])].sort(
                (left, right) => left.mention_order - right.mention_order,
              )) {
                if (!entities.has(entityKey(item)))
                  entities.set(entityKey(item), item);
              }
            }
            const input = ordered[0].measurement;
            const focusEntities = [
              ...new Map(
                focusTargets.map((target) => [
                  JSON.stringify([target.kind, target.name.trim()]),
                  target,
                ]),
              ).values(),
            ].map((target) => ({
              ...target,
              name: target.name.trim(),
              id: JSON.stringify([target.kind, target.name.trim()]),
            }));
            // Unknown times or ties at the latest-pair boundary cannot establish chronology.
            const latestPair = entries.slice(-2);
            const chronologyKnown =
              entries.every(({ sample }) =>
                Number.isFinite(observedTime(sample)),
              ) &&
              latestPair.length === 2 &&
              observedTime(latestPair[0].sample) <
                observedTime(latestPair[1].sample) &&
              (entries.length < 3 ||
                observedTime(entries[entries.length - 3].sample) <
                  observedTime(latestPair[0].sample));
            const timeLabel = (sample: RecommendationTrendSample) => {
              const time = observedTime(sample);
              return Number.isFinite(time)
                ? date.format(time)
                : t("timeUnknown");
            };
            return (
              <Card
                key={key}
                className="recommendation-trend-group"
                role="region"
                aria-label={input.question}
              >
                <h4 className="recommendation-trend-question">
                  {input.question}
                </h4>
                <Text size={200} className="recommendation-trend-secondary">
                  {t("providerModel", {
                    provider: input.provider,
                    model: input.model,
                  })}
                </Text>
                {focusEntities.length > 0 && (
                  <section aria-label={t("frequency")}>
                    <Text weight="semibold">{t("frequency")}</Text>
                    {focusEntities.map((entity) => {
                      const frequency = recommendationFrequency(
                        entries,
                        entity,
                      );
                      return (
                        <div
                          role="group"
                          aria-label={
                            entity.kind
                              ? `${entity.name} · ${t(`kind_${entity.kind}`)}`
                              : entity.name
                          }
                          className="recommendation-trend-focus"
                          key={entity.id ?? entity.name}
                        >
                          <Text weight="semibold">
                            {entity.name}
                            {entity.kind && ` · ${t(`kind_${entity.kind}`)}`}
                          </Text>
                          {entity.kind === "brand" && (
                            <Text>{focusT("brandDirect")}</Text>
                          )}
                          <Text>
                            {frequency.valid === 0
                              ? t("noValidSamples")
                              : t("frequencyRate", {
                                  recommended: number.format(
                                    frequency.recommended,
                                  ),
                                  valid: number.format(frequency.valid),
                                  percentage: percent.format(
                                    frequency.recommended / frequency.valid,
                                  ),
                                })}
                          </Text>
                          <RecommendationBrandSummary
                            summary={frequency.brandSummary}
                          />
                          <Text
                            size={200}
                            className="recommendation-trend-secondary"
                          >
                            {t("frequencySamples", {
                              valid: number.format(frequency.valid),
                              unavailable: number.format(
                                entries.length - frequency.valid,
                              ),
                            })}
                          </Text>
                          <div role="group" aria-label={t("firstPlaceShare")}>
                            <Text size={200}>{t("firstPlaceShare")}</Text>
                            <div>
                              {frequency.ranked === 0
                                ? t("noRankedSamples")
                                : t("firstPlaceRate", {
                                    first: number.format(
                                      frequency.ranks.get(1) ?? 0,
                                    ),
                                    ranked: number.format(frequency.ranked),
                                    percentage: percent.format(
                                      (frequency.ranks.get(1) ?? 0) /
                                        frequency.ranked,
                                    ),
                                  })}
                            </div>
                            <Text
                              size={200}
                              className="recommendation-trend-secondary"
                            >
                              {t("firstPlaceBasis")}
                            </Text>
                          </div>
                          {frequency.valid > 0 && (
                            <div
                              role="group"
                              aria-label={t("rankDistribution")}
                            >
                              <Text size={200}>{t("rankDistribution")}</Text>
                              {[...frequency.ranks]
                                .sort(([left], [right]) => left - right)
                                .map(([rank, count]) => (
                                  <div key={rank}>
                                    {t("rankCount", {
                                      rank: number.format(rank),
                                      value: number.format(count),
                                    })}
                                  </div>
                                ))}
                              <div>
                                {t("unrankedCount", {
                                  value: number.format(frequency.unranked),
                                })}
                              </div>
                              {frequency.ambiguous > 0 && (
                                <div>
                                  {t("ambiguousCount", {
                                    value: number.format(frequency.ambiguous),
                                  })}
                                </div>
                              )}
                            </div>
                          )}
                        </div>
                      );
                    })}
                  </section>
                )}
                {focusEntities.length > 0 && (
                  <section aria-label={t("focusChanges")}>
                    {focusEntities.map((entity) => {
                      const states = latestPair.map((entry) =>
                        entityState(entry, entity),
                      );
                      const latest = latestPair[latestPair.length - 1];
                      const latestState = states[states.length - 1];
                      const previousRank =
                        states.length === 2 ? states[0].rank : undefined;
                      const latestRank = latestState.rank;
                      const change =
                        previousRank !== undefined && latestRank !== undefined
                          ? previousRank - latestRank
                          : null;
                      return (
                        <section
                          className="recommendation-trend-focus"
                          key={entity.id ?? entity.name}
                          aria-label={
                            entity.kind
                              ? `${entity.name} · ${t(`kind_${entity.kind}`)}`
                              : entity.name
                          }
                        >
                          <Text
                            weight="semibold"
                            className="recommendation-trend-entity"
                          >
                            {entity.name}
                            {entity.kind && (
                              <Text
                                size={200}
                                className="recommendation-trend-secondary"
                              >
                                {" "}
                                · {t(`kind_${entity.kind}`)}
                              </Text>
                            )}
                          </Text>
                          {entries.length > 1 && !chronologyKnown ? (
                            <p>{t("chronologyUnknown")}</p>
                          ) : (
                            <>
                              <Text
                                size={200}
                                className="recommendation-trend-secondary"
                              >
                                {t("latestSample")}
                              </Text>
                              <div className="recommendation-trend-rank-line">
                                <Text
                                  as="strong"
                                  size={latestRank === undefined ? 500 : 800}
                                  weight="semibold"
                                  className="recommendation-trend-rank"
                                >
                                  {t(latestState.label, {
                                    value:
                                      latestRank === undefined
                                        ? undefined
                                        : number.format(latestRank),
                                  })}
                                </Text>
                                {entries.length > 1 && change !== null && (
                                  <Badge
                                    appearance="tint"
                                    color={
                                      change > 0
                                        ? "success"
                                        : change < 0
                                          ? "danger"
                                          : "informative"
                                    }
                                    className="recommendation-trend-delta"
                                  >
                                    {t(
                                      change > 0
                                        ? "improved"
                                        : change < 0
                                          ? "declined"
                                          : "unchanged",
                                      {
                                        value: number.format(Math.abs(change)),
                                      },
                                    )}
                                  </Badge>
                                )}
                              </div>
                              <div className="recommendation-trend-date">
                                <a
                                  href={latest.sample.recordHref}
                                  aria-label={`${t("viewRecord")} · ${timeLabel(latest.sample)}`}
                                >
                                  {timeLabel(latest.sample)}
                                </a>
                              </div>
                              {(entries.length === 1 || change === null) && (
                                <Text
                                  size={200}
                                  className="recommendation-trend-secondary"
                                >
                                  {t(
                                    entries.length === 1
                                      ? "noPreviousSample"
                                      : "rankNotComparable",
                                  )}
                                </Text>
                              )}
                              {latestPair.length === 2 && (
                                <div className="recommendation-trend-previous">
                                  {t("previousSample")}{" "}
                                  <span>
                                    {t(states[0].label, {
                                      value:
                                        previousRank === undefined
                                          ? undefined
                                          : number.format(previousRank),
                                    })}
                                  </span>
                                  {" · "}
                                  <a
                                    href={latestPair[0].sample.recordHref}
                                    aria-label={`${t("viewRecord")} · ${timeLabel(latestPair[0].sample)}`}
                                  >
                                    {timeLabel(latestPair[0].sample)}
                                  </a>
                                </div>
                              )}
                            </>
                          )}
                        </section>
                      );
                    })}
                  </section>
                )}
                <Text size={200} className="recommendation-trend-secondary">
                  {t("samples", {
                    available: number.format(
                      entries.filter((entry) => entry.items !== null).length,
                    ),
                    total: number.format(entries.length),
                  })}
                </Text>
                {entries.length === 1 && focusEntities.length === 0 && (
                  <p>{t("singleSample")}</p>
                )}
                <details>
                  <summary>{t("measurementConditions")}</summary>
                  <p>
                    {t("conditions", {
                      surface: input.surface,
                      market: input.market,
                      language: input.language,
                      searchMode: input.search_mode,
                    })}
                  </p>
                  <p>{t("description")}</p>
                  <p>{t("exactNames")}</p>
                </details>
                <details open={focusEntities.length === 0}>
                  <summary>{t("fullHistory")}</summary>
                  <div style={{ overflowX: "auto" }}>
                    <Table
                      aria-label={input.question}
                      style={{
                        minWidth: Math.max(460, 180 * (entries.length + 1)),
                      }}
                    >
                      <TableHeader>
                        <TableRow>
                          <TableHeaderCell scope="col">
                            {t("entity")}
                          </TableHeaderCell>
                          {entries.map(({ sample, items }) => {
                            const label = timeLabel(sample);
                            return (
                              <TableHeaderCell
                                key={sample.sampleId}
                                scope="col"
                              >
                                <a
                                  href={sample.recordHref}
                                  aria-label={`${t("viewRecord")} · ${label}`}
                                >
                                  {label}
                                </a>
                                {items === null && (
                                  <p>
                                    {t(
                                      sample.recommendations == null
                                        ? "notAnalyzed"
                                        : "unverified",
                                    )}
                                  </p>
                                )}
                              </TableHeaderCell>
                            );
                          })}
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {[...entities].map(([entityId, entity]) => (
                          <TableRow
                            key={entityId}
                            style={
                              isMeasurementFocus(entity, focusTargets)
                                ? {
                                    backgroundColor:
                                      tokens.colorBrandBackground2,
                                  }
                                : undefined
                            }
                          >
                            <TableCell style={{ overflowWrap: "anywhere" }}>
                              <div>{entity.name}</div>
                              <small>{t(`kind_${entity.kind}`)}</small>
                              {isMeasurementFocus(entity, focusTargets) && (
                                <div>{t("tracked")}</div>
                              )}
                            </TableCell>
                            {entries.map(({ sample, items }) => {
                              const matches = items?.filter(
                                (item) => entityKey(item) === entityId,
                              );
                              return (
                                <TableCell key={sample.sampleId}>
                                  {items === null
                                    ? t(
                                        sample.recommendations == null
                                          ? "notAnalyzed"
                                          : "unverified",
                                      )
                                    : !matches?.length
                                      ? t("absent")
                                      : matches.map((item, index) => (
                                          <div key={index}>
                                            <div>
                                              {item.rank === null
                                                ? t("unranked")
                                                : t("rank", {
                                                    value: number.format(
                                                      item.rank,
                                                    ),
                                                  })}
                                            </div>
                                            <div>
                                              {t("order", {
                                                value: number.format(
                                                  item.mention_order,
                                                ),
                                              })}
                                            </div>
                                            <details>
                                              <summary>{t("evidence")}</summary>
                                              <blockquote
                                                style={{
                                                  whiteSpace: "pre-wrap",
                                                  overflowWrap: "anywhere",
                                                }}
                                              >
                                                {item.evidence_quote}
                                              </blockquote>
                                            </details>
                                            <RecommendationBrandAttribution
                                              item={item}
                                            />
                                          </div>
                                        ))}
                                </TableCell>
                              );
                            })}
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </div>
                  {entities.size === 0 &&
                    entries.some((entry) => entry.items !== null) && (
                      <p>{t("noEntities")}</p>
                    )}
                </details>
              </Card>
            );
          })}
        </>
      )}
    </section>
  );
}
