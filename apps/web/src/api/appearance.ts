import { apiFetch } from "./client";

export type UiLocale = "zh-CN" | "en";

export interface Appearance {
  display_name: string;
  logo_url: string | null;
  primary_color: string;
  default_locale: UiLocale;
  revision: number;
}

export type AppearanceInput = Pick<
  Appearance,
  "display_name" | "primary_color" | "default_locale"
>;

export const genericAppearance: Appearance = {
  display_name: "GEO",
  logo_url: null,
  primary_color: "#2563EB",
  default_locale: "zh-CN",
  revision: 0,
};

/** Treat unexpected API shapes as unavailable, never as a host's brand. */
export function parseAppearance(value: unknown): Appearance {
  if (!value || typeof value !== "object")
    throw new Error("Invalid appearance");
  const data = value as Partial<Appearance>;
  if (
    typeof data.display_name !== "string" ||
    !data.display_name.trim() ||
    data.display_name.length > 200 ||
    (data.logo_url !== null && typeof data.logo_url !== "string") ||
    typeof data.primary_color !== "string" ||
    !/^#[0-9a-fA-F]{6}$/.test(data.primary_color) ||
    (data.default_locale !== "zh-CN" && data.default_locale !== "en") ||
    !Number.isSafeInteger(data.revision) ||
    (data.revision ?? -1) < 0
  ) {
    throw new Error("Invalid appearance");
  }
  return data as Appearance;
}

export async function getPublicAppearance(): Promise<Appearance> {
  return parseAppearance(
    await apiFetch<unknown>("/public/appearance", {
      unauthorized: "ignore",
      cache: "no-store",
    }),
  );
}

export async function getOperatorAppearance(): Promise<Appearance> {
  return parseAppearance(await apiFetch<unknown>("/operator/appearance"));
}

export async function updateOperatorAppearance(
  appearance: AppearanceInput,
  expectedRevision: number,
): Promise<Appearance> {
  if (!Number.isSafeInteger(expectedRevision) || expectedRevision < 1) {
    throw new Error("Invalid appearance revision");
  }
  return parseAppearance(
    await apiFetch<unknown>("/operator/appearance", {
      method: "PUT",
      headers: { "If-Match": `"${expectedRevision}"` },
      body: appearance,
    }),
  );
}
