import { Card, Field, Select } from "@fluentui/react-components";
import { useTranslation } from "react-i18next";
import type { MeasurementTarget } from "../api/projects";
import { decodeVerifiedRecommendations } from "../api/recommendations";
import type { RecommendationTrendSample } from "./RecommendationTrend";
import {
  comparisonKey,
  questionComparisonKey,
  recommendationFrequency,
} from "./recommendationComparison";
import "../i18n/recommendationPlatformComparison";
import "./RecommendationPlatformComparison.css";
import { RecommendationBrandSummary } from "./RecommendationBrandAttribution";
import "../i18n/measurementFocus";

/** Read-only comparison of the report's selected evidence and current viewing target. */
export function RecommendationPlatformComparison({
  samples,
  focusTargets,
  selectedTarget: target,
  onSelectTarget,
  planWindow,
  timezone = "UTC",
}: {
  samples: readonly RecommendationTrendSample[];
  focusTargets: readonly MeasurementTarget[];
  selectedTarget: MeasurementTarget | null;
  onSelectTarget: (target: MeasurementTarget) => void;
  planWindow: { start: string; end: string };
  timezone?: string;
}) {
  const { t, i18n } = useTranslation("recommendationPlatformComparison");
  const { t: focusT } = useTranslation("measurementFocus");
  const targetKey = (target: MeasurementTarget) =>
    JSON.stringify([target.kind, target.name.trim()]);
  const locale = i18n.resolvedLanguage || i18n.language;
  const number = new Intl.NumberFormat(locale);
  const percent = new Intl.NumberFormat(locale, {
    style: "percent",
    maximumFractionDigits: 1,
  });
  const date = new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
    timeZone: timezone,
  });
  const time = (value: string | null) =>
    value === null ? NaN : Date.parse(value);
  const at = (value: string | null) => {
    const timestamp = time(value);
    return Number.isFinite(timestamp)
      ? date.format(timestamp)
      : t("unknownTime");
  };
  const questions = new Map<string, Map<string, RecommendationTrendSample[]>>();
  for (const sample of samples) {
    const questionKey = questionComparisonKey(sample.measurement);
    const groups =
      questions.get(questionKey) ??
      new Map<string, RecommendationTrendSample[]>();
    const conditionKey = comparisonKey(sample.measurement);
    const group = groups.get(conditionKey) ?? [];
    group.push(sample);
    groups.set(conditionKey, group);
    questions.set(questionKey, groups);
  }

  return (
    <section
      className="recommendation-platform-comparison"
      aria-label={t("title")}
    >
      <h3>{t("title")}</h3>
      <p>{t("scope")}</p>
      <p>
        {t("planWindow", {
          start: at(planWindow.start),
          end: at(planWindow.end),
          timezone,
        })}
      </p>
      {target ? (
        <Field label={t("target")}>
          <Select
            aria-label={t("target")}
            value={targetKey(target)}
            onChange={(_, data) => {
              const selected = focusTargets.find(
                (item) => targetKey(item) === data.value,
              );
              if (selected) onSelectTarget(selected);
            }}
          >
            {focusTargets.map((item) => (
              <option key={targetKey(item)} value={targetKey(item)}>
                {item.name} · {t(`kind_${item.kind}`)}
              </option>
            ))}
          </Select>
        </Field>
      ) : (
        <p>{t("noTarget")}</p>
      )}
      {target && !samples.length && <p>{t("empty")}</p>}
      {target && samples.length > 0 && (
        <>
          <p>{t("currentTarget")}</p>
          <p>{t("firstPlaceBasis")}</p>
          {[...questions].map(([questionKey, groups], questionIndex) => {
            const first = groups.values().next().value?.[0];
            if (!first) return null;
            return (
              <Card
                key={questionKey}
                className="recommendation-platform-question"
              >
                <h4>{first.measurement.question}</h4>
                <p>
                  {t("questionGroup", {
                    number: number.format(questionIndex + 1),
                    version: first.measurement.question_set_version,
                  })}
                </p>
                <div
                  className="recommendation-platform-scroll"
                  role="region"
                  aria-label={t("tableRegion", {
                    question: first.measurement.question,
                    number: number.format(questionIndex + 1),
                  })}
                  tabIndex={0}
                >
                  <table className="recommendation-platform-table">
                    <caption>
                      {t("tableCaption", { name: target.name })}
                    </caption>
                    <thead>
                      <tr>
                        <th scope="col">{t("conditions")}</th>
                        <th scope="col">
                          {target.kind === "brand"
                            ? focusT("brandDirect")
                            : t("recommendationRate")}
                        </th>
                        {target.kind === "brand" && (
                          <th scope="col">{focusT("brandAffiliateTitle")}</th>
                        )}
                        <th scope="col">{t("firstPlace")}</th>
                        <th scope="col">{t("distribution")}</th>
                        <th scope="col">{t("observedTimes")}</th>
                        <th scope="col">{t("evidence")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {[...groups].map(([key, group]) => {
                        const input = group[0].measurement;
                        const frequency = recommendationFrequency(
                          group.map((sample) => ({
                            items: decodeVerifiedRecommendations(
                              sample.recommendations,
                            ),
                          })),
                          target,
                        );
                        const actualTimes = group
                          .map((sample) => time(sample.observedAt))
                          .filter(Number.isFinite);
                        const firstTime = actualTimes.length
                          ? Math.min(...actualTimes)
                          : NaN;
                        const lastTime = actualTimes.length
                          ? Math.max(...actualTimes)
                          : NaN;
                        const rate = (count: number, total: number) =>
                          t("rate", {
                            value: number.format(count),
                            total: number.format(total),
                            percentage: percent.format(count / total),
                          });
                        return (
                          <tr key={key}>
                            <th scope="row">
                              <strong>{input.provider}</strong>
                              <dl>
                                {(
                                  [
                                    "model",
                                    "surface",
                                    "search_mode",
                                    "protocol_version",
                                    "market",
                                    "language",
                                  ] as const
                                ).map((field) => (
                                  <div key={field}>
                                    <dt>{t(field)}</dt>
                                    <dd>{input[field]}</dd>
                                  </div>
                                ))}
                              </dl>
                            </th>
                            <td>
                              <p>
                                {frequency.valid
                                  ? rate(frequency.recommended, frequency.valid)
                                  : t("noValid")}
                              </p>
                              <p>
                                {t("sampleCounts", {
                                  valid: number.format(frequency.valid),
                                  unavailable: number.format(
                                    group.length - frequency.valid,
                                  ),
                                })}
                              </p>
                            </td>
                            {target.kind === "brand" && (
                              <td>
                                <RecommendationBrandSummary
                                  summary={frequency.brandSummary}
                                />
                              </td>
                            )}
                            <td>
                              {frequency.ranked
                                ? rate(
                                    frequency.ranks.get(1) ?? 0,
                                    frequency.ranked,
                                  )
                                : t("noRanked")}
                            </td>
                            <td>
                              {[...frequency.ranks]
                                .sort(([a], [b]) => a - b)
                                .map(([rank, count]) => (
                                  <p key={rank}>
                                    {t("rankCount", {
                                      rank: number.format(rank),
                                      value: number.format(count),
                                    })}
                                  </p>
                                ))}
                              <p>
                                {t("unrankedCount", {
                                  value: number.format(frequency.unranked),
                                })}
                              </p>
                              <p>
                                {t("conflictCount", {
                                  value: number.format(frequency.ambiguous),
                                })}
                              </p>
                            </td>
                            <td>
                              <p>
                                {actualTimes.length
                                  ? t("dateRange", {
                                      start: date.format(firstTime),
                                      end: date.format(lastTime),
                                    })
                                  : t("unknownTime")}
                              </p>
                              <p>
                                {t("unknownCount", {
                                  value: number.format(
                                    group.length - actualTimes.length,
                                  ),
                                })}
                              </p>
                            </td>
                            <td>
                              <details>
                                <summary>
                                  {t("sampleEvidence", {
                                    value: number.format(group.length),
                                  })}
                                </summary>
                                <ul>
                                  {group.map((sample, index) => (
                                    <li
                                      key={JSON.stringify([
                                        sample.sampleId,
                                        sample.recordHref,
                                        index,
                                      ])}
                                    >
                                      <a href={sample.recordHref}>
                                        {t("record", {
                                          number: number.format(index + 1),
                                        })}{" "}
                                        · {at(sample.observedAt)}
                                      </a>
                                    </li>
                                  ))}
                                </ul>
                              </details>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </Card>
            );
          })}
        </>
      )}
    </section>
  );
}
