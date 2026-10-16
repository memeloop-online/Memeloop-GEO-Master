import {
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableHeaderCell,
  TableRow,
} from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import type { ChannelTargetInput } from "../api/channelJobs";
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
  observedAt: string;
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

/** Compare verified extracted recommendations; never derive entities or ranks from prose. */
export function RecommendationTrend({
  samples,
}: {
  samples: readonly RecommendationTrendSample[];
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
          <p>{t("description")}</p>
          <p>{t("exactNames")}</p>
          {[...groups].map(([key, group]) => {
            const ordered = [...group].sort((left, right) => {
              const difference =
                Date.parse(left.observedAt) - Date.parse(right.observedAt);
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
            return (
              <section key={key} aria-label={input.question}>
                <h4 style={{ overflowWrap: "anywhere" }}>{input.question}</h4>
                <p>
                  {t("providerModel", {
                    provider: input.provider,
                    model: input.model,
                  })}
                </p>
                <p>
                  {t("conditions", {
                    surface: input.surface,
                    market: input.market,
                    language: input.language,
                    searchMode: input.search_mode,
                  })}
                </p>
                <p>
                  {t("samples", {
                    available: number.format(
                      entries.filter((entry) => entry.items !== null).length,
                    ),
                    total: number.format(entries.length),
                  })}
                </p>
                {entries.length === 1 && <p>{t("singleSample")}</p>}
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
                          const time = Date.parse(sample.observedAt);
                          const timeLabel = Number.isFinite(time)
                            ? date.format(time)
                            : t("timeUnknown");
                          return (
                            <TableHeaderCell key={sample.sampleId} scope="col">
                              <a
                                href={sample.recordHref}
                                aria-label={`${t("viewRecord")} · ${timeLabel}`}
                              >
                                {timeLabel}
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
                        <TableRow key={entityId}>
                          <TableCell style={{ overflowWrap: "anywhere" }}>
                            <div>{entity.name}</div>
                            <small>{t(`kind_${entity.kind}`)}</small>
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
              </section>
            );
          })}
        </>
      )}
    </section>
  );
}
