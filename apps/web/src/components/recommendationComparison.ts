import { isMeasurementFocus } from "../api/measurementFocus";
import type { MeasurementTarget } from "../api/projects";
import type { RecommendationItem } from "../api/recommendations";
import type { RecommendationTrendSample } from "./RecommendationTrend";

type MeasurementInput = RecommendationTrendSample["measurement"];

/** A question's exact text and reference/version form one comparison partition. */
export function questionComparisonKey(input: MeasurementInput) {
  const reference = input.question_binding?.reference;
  return JSON.stringify([
    reference?.question_set_id ?? null,
    reference?.question_set_version_id ?? null,
    reference?.question_id ?? null,
    reference?.question_revision_id ?? null,
    input.question_set_version,
    input.question,
  ]);
}

/** Includes all frozen measurement conditions; never compares unlike observations. */
export function comparisonKey(input: MeasurementInput) {
  return JSON.stringify([
    questionComparisonKey(input),
    input.provider,
    input.model,
    input.surface,
    input.search_mode,
    input.protocol_version,
    input.market,
    input.language,
  ]);
}

export function recommendationFrequency(
  entries: readonly { items: RecommendationItem[] | null }[],
  target: MeasurementTarget,
) {
  let valid = 0;
  let recommended = 0;
  let unranked = 0;
  let ambiguous = 0;
  const ranks = new Map<number, number>();
  for (const { items } of entries) {
    if (items === null) continue;
    valid += 1;
    const matches = items.filter((item) => isMeasurementFocus(item, [target]));
    if (!matches.length) continue;
    recommended += 1;
    // Each answer contributes once. Conflicting ranks have no single rank bucket.
    const sampleRanks = new Set(matches.map((item) => item.rank));
    if (sampleRanks.size > 1) {
      ambiguous += 1;
    } else {
      const rank = matches[0].rank;
      if (rank === null) unranked += 1;
      else ranks.set(rank, (ranks.get(rank) ?? 0) + 1);
    }
  }
  return {
    valid,
    recommended,
    unranked,
    ambiguous,
    ranks,
    ranked: recommended - unranked - ambiguous,
  };
}
