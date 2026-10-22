import type { ProjectSettings, UpdateProjectInput } from "./projects";

/** A product and its brand are not aliases. Prefer the explicitly named product. */
export function measurementFocusNames(
  settings: Pick<ProjectSettings, "brand_name" | "product_name"> | undefined,
): string[] {
  const name = settings?.product_name?.trim() || settings?.brand_name?.trim();
  return name ? [name] : [];
}

export function isMeasurementFocus(
  name: string,
  focusNames: readonly string[],
) {
  return focusNames.some((focus) => focus.trim() === name.trim());
}

/** Partial patch: never resend unrelated project settings from an old form. */
export function measurementFocusPatch(
  revision: number,
  brand: string,
  product: string,
): UpdateProjectInput {
  const settings: Partial<ProjectSettings> = {
    brand_name: brand.trim(),
    product_name: product.trim() || null,
  };
  return { revision, settings };
}
