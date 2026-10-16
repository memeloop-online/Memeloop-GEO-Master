export interface RecommendationItem {
  name: string;
  kind: "product" | "restaurant" | "brand" | "service" | "other";
  mention_order: number;
  rank: number | null;
  reason: string | null;
  evidence_quote: string;
  citation_urls: string[];
}

/** Decode the current verified extraction; null is unavailable, [] is verified empty. */
export function decodeVerifiedRecommendations(
  value: unknown,
): RecommendationItem[] | null {
  if (!value || typeof value !== "object") return null;
  const result = value as Record<string, unknown>;
  if (
    result.version !== "geo.recommendations.v1" ||
    result.status !== "verified" ||
    !Array.isArray(result.items)
  )
    return null;
  const valid = result.items.every((value: unknown) => {
    if (!value || typeof value !== "object") return false;
    const item = value as Record<string, unknown>;
    return (
      typeof item.name === "string" &&
      item.name.trim().length > 0 &&
      ["product", "restaurant", "brand", "service", "other"].includes(
        item.kind as string,
      ) &&
      Number.isInteger(item.mention_order) &&
      (item.mention_order as number) > 0 &&
      (item.rank === null ||
        (Number.isInteger(item.rank) && (item.rank as number) > 0)) &&
      (item.reason === null || typeof item.reason === "string") &&
      typeof item.evidence_quote === "string" &&
      Array.isArray(item.citation_urls) &&
      item.citation_urls.every((url) => typeof url === "string")
    );
  });
  return valid ? (result.items as RecommendationItem[]) : null;
}
