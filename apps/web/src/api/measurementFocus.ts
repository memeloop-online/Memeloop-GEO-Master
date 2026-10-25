import type {
  MeasurementTarget,
  ProjectSettings,
  UpdateProjectInput,
} from "./projects";
import type { RecommendationItem } from "./recommendations";

/** Current viewing preferences, independent of enterprise metadata and saved evidence. */
export function measurementFocusTargets(
  settings: Pick<ProjectSettings, "measurement_targets"> | undefined,
): readonly MeasurementTarget[] {
  return settings?.measurement_targets ?? [];
}

export function isMeasurementFocus(
  item: Pick<MeasurementTarget, "name" | "kind">,
  focusTargets: readonly MeasurementTarget[],
) {
  return focusTargets.some(
    (target) =>
      target.kind === item.kind &&
      [target.name, ...target.aliases].some(
        (name) => name.trim() === item.name.trim(),
      ),
  );
}

export type BrandRecommendationClassification =
  "positive" | "negative" | "unknown";

export interface BrandRecommendationSummary {
  positiveAnswers: number;
  knownAnswers: number;
  unknownAnswers: number;
  validAnswers: number;
}

/** Product affiliation is independent of a directly recommended brand and its rank. */
export function classifyBrandRecommendation(
  items: readonly RecommendationItem[],
  focus: MeasurementTarget,
): BrandRecommendationClassification | null {
  if (focus.kind !== "brand") return null;
  const products = items.filter((item) => item.kind === "product");
  if (
    products.some(
      (item) =>
        item.brand_attribution !== null &&
        isMeasurementFocus(
          { name: item.brand_attribution.name, kind: "brand" },
          [focus],
        ),
    )
  )
    return "positive";
  return products.some((item) => item.brand_attribution === null)
    ? "unknown"
    : "negative";
}

/** Each verified answer contributes once; unavailable answers contribute nothing. */
export function summarizeBrandRecommendations(
  answers: readonly (readonly RecommendationItem[] | null)[],
  focus: MeasurementTarget,
): BrandRecommendationSummary | null {
  if (focus.kind !== "brand") return null;
  const summary: BrandRecommendationSummary = {
    positiveAnswers: 0,
    knownAnswers: 0,
    unknownAnswers: 0,
    validAnswers: 0,
  };
  for (const items of answers) {
    if (items === null) continue;
    summary.validAnswers += 1;
    const classification = classifyBrandRecommendation(items, focus);
    if (classification === "unknown") summary.unknownAnswers += 1;
    else {
      summary.knownAnswers += 1;
      if (classification === "positive") summary.positiveAnswers += 1;
    }
  }
  return summary;
}

/** Partial patch: never resend unrelated project settings from an old form. */
export function measurementFocusPatch(
  revision: number,
  targets: readonly MeasurementTarget[],
): UpdateProjectInput {
  const settings: Partial<ProjectSettings> = {
    measurement_targets: targets.map((target) => ({
      name: target.name.trim(),
      kind: target.kind,
      aliases: [
        ...new Set(target.aliases.map((alias) => alias.trim()).filter(Boolean)),
      ],
    })),
  };
  return { revision, settings };
}
