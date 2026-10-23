import {
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableHeaderCell,
  TableRow,
  tokens,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import type { ChannelTargetInput } from "../api/channelJobs";
import { isMeasurementFocus } from "../api/measurementFocus";
import {
  decodeVerifiedRecommendations,
  type RecommendationItem,
} from "../api/recommendations";
import "../i18n/recommendationTrend";

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

function comparisonKey(input: MeasurementInput) {
  const reference = input.question_binding?.reference;
  return JSON.stringify([
    reference?.question_set_id ?? null,
    reference?.question_set_version_id ?? null,
    reference?.question_id ?? null,
    reference?.question_revision_id ?? null,
    input.question_set_version,
    input.question,
    input.provider,
    input.model,
    input.surface,
    input.search_mode,
    input.protocol_version,
    input.market,
    input.language,
  ]);
}

const entityKey = (item: RecommendationItem) =>
  JSON.stringify([item.kind, item.name]);

const observedTime = (sample: RecommendationTrendSample) =>
  sample.observedAt === null ? NaN : Date.parse(sample.observedAt);

type TrendEntry = {
  sample: RecommendationTrendSample;
  items: RecommendationItem[] | null;
};

function entityState(entry: TrendEntry, entityId: string | undefined) {
  if (entry.items === null)
    return {
      label:
        entry.sample.recommendations == null ? "notAnalyzed" : "unverified",
    };
  const matches = entry.items.filter((item) => entityKey(item) === entityId);
  if (!matches.length) return { label: "absent" };
  if (matches.length > 1) return { label: "ambiguous" };
  return matches[0].rank === null
    ? { label: "unranked" }
    : { label: "rank", rank: matches[0].rank };
}

/** Compare verified extracted recommendations; never derive entities or ranks from prose. */
export function RecommendationTrend({
  samples,
  focusNames = [],
}: {
  samples: readonly RecommendationTrendSample[];
  focusNames?: readonly string[];
}) {
  const { t, i18n } = useTranslation("recommendationTrend");
  const number = new Intl.NumberFormat(i18n.resolvedLanguage || i18n.language);
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
    <section aria-label={t("title")}>
      <h3>{t("title")}</h3>
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
              ...new Set(focusNames.map((name) => name.trim())),
            ].flatMap<{
              id: string | undefined;
              name: string;
              kind: RecommendationItem["kind"] | undefined;
            }>((name) => {
              const matches = [...entities].filter(([, entity]) =>
                isMeasurementFocus(entity.name, [name]),
              );
              return matches.length
                ? matches.map(([id, entity]) => ({
                    id,
                    name: entity.name,
                    kind: entity.kind,
                  }))
                : [{ id: undefined, name, kind: undefined }];
            });
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
              <section key={key} aria-label={input.question}>
                <h4 style={{ overflowWrap: "anywhere" }}>{input.question}</h4>
                <p>
                  {t("providerModel", {
                    provider: input.provider,
                    model: input.model,
                  })}
                </p>
                {focusEntities.length > 0 && (
                  <section aria-label={t("focusChanges")}>
                    <h5>{t("focusChanges")}</h5>
                    {focusEntities.map((entity) => {
                      const states = latestPair.map((entry) =>
                        entityState(entry, entity.id),
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
                          key={entity.id ?? entity.name}
                          aria-label={
                            entity.kind
                              ? `${entity.name} · ${t(`kind_${entity.kind}`)}`
                              : entity.name
                          }
                        >
                          <h5 style={{ overflowWrap: "anywhere" }}>
                            {entity.name}
                            {entity.kind && <> · {t(`kind_${entity.kind}`)}</>}
                          </h5>
                          {entries.length > 1 && !chronologyKnown ? (
                            <p>{t("chronologyUnknown")}</p>
                          ) : (
                            <>
                              <p>
                                {t("latestSample")}{" "}
                                <strong>
                                  {t(latestState.label, {
                                    value:
                                      latestRank === undefined
                                        ? undefined
                                        : number.format(latestRank),
                                  })}
                                </strong>
                                {" · "}
                                <a
                                  href={latest.sample.recordHref}
                                  aria-label={`${t("viewRecord")} · ${timeLabel(latest.sample)}`}
                                >
                                  {timeLabel(latest.sample)}
                                </a>
                              </p>
                              <p>
                                {entries.length === 1
                                  ? t("noPreviousSample")
                                  : change === null
                                    ? t("rankNotComparable")
                                    : t(
                                        change > 0
                                          ? "improved"
                                          : change < 0
                                            ? "declined"
                                            : "unchanged",
                                        {
                                          value: number.format(
                                            Math.abs(change),
                                          ),
                                        },
                                      )}
                              </p>
                              {latestPair.length === 2 && (
                                <p>
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
                                </p>
                              )}
                            </>
                          )}
                        </section>
                      );
                    })}
                  </section>
                )}
                <p>
                  {t("samples", {
                    available: number.format(
                      entries.filter((entry) => entry.items !== null).length,
                    ),
                    total: number.format(entries.length),
                  })}
                </p>
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
                              isMeasurementFocus(entity.name, focusNames)
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
                              {isMeasurementFocus(entity.name, focusNames) && (
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
              </section>
            );
          })}
        </>
      )}
    </section>
  );
}
