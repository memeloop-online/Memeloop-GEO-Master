import type { MeasurementTarget, ProjectSettings, UpdateProjectInput } from "./projects";

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
  return focusTargets.some((target) =>
    target.kind === item.kind &&
    [target.name, ...target.aliases].some((name) => name.trim() === item.name.trim()),
  );
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
      aliases: [...new Set(target.aliases.map((alias) => alias.trim()).filter(Boolean))],
    })),
  };
  return { revision, settings };
}
