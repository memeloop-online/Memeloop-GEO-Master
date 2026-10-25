import { describe, expect, it } from "vitest";
import { decodeVerifiedRecommendations } from "./recommendations";

const item = {
  name: "Example model",
  kind: "product",
  mention_order: 1,
  rank: 1,
  reason: null,
  evidence_quote: "Example model is recommended.",
  citation_urls: [],
  brand_attribution: null,
};

const snapshot = (items: unknown[]) => ({
  version: "geo.recommendations.v2",
  status: "verified",
  items,
});

describe("verified recommendation decoder", () => {
  it("decodes current product attribution and explicit unknown attribution", () => {
    const attributed = {
      ...item,
      brand_attribution: {
        name: "Example brand",
        evidence_quote: "Example model is made by Example brand.",
      },
    };
    expect(decodeVerifiedRecommendations(snapshot([attributed, item]))).toEqual(
      [attributed, item],
    );
  });

  it("keeps verified empty distinct from missing, old and unverified snapshots", () => {
    expect(decodeVerifiedRecommendations(snapshot([]))).toEqual([]);
    for (const value of [
      undefined,
      null,
      {},
      { ...snapshot([]), version: "geo.recommendations.v1" },
      { ...snapshot([item]), version: "geo.recommendations.v1" },
      { ...snapshot([]), status: "unverified" },
      { ...snapshot([item]), status: "unverified" },
      { ...snapshot([]), items: undefined },
    ]) {
      expect(decodeVerifiedRecommendations(value)).toBeNull();
    }
  });

  it("rejects missing or malformed attribution instead of inventing unknowns", () => {
    for (const brand_attribution of [
      undefined,
      "Example brand",
      {},
      { name: "Example brand" },
      { name: "", evidence_quote: "An attribution quote." },
      { name: "Example brand", evidence_quote: " " },
      { name: "Example brand", evidence_quote: 7 },
    ]) {
      expect(
        decodeVerifiedRecommendations(
          snapshot([{ ...item, brand_attribution }]),
        ),
      ).toBeNull();
    }
  });

  it.each(["brand", "restaurant", "service", "other"])(
    "permits only null attribution on %s recommendations",
    (kind) => {
      expect(
        decodeVerifiedRecommendations(snapshot([{ ...item, kind }])),
      ).toEqual([{ ...item, kind }]);
      expect(
        decodeVerifiedRecommendations(
          snapshot([
            {
              ...item,
              kind,
              brand_attribution: {
                name: "Example brand",
                evidence_quote: "An attribution quote.",
              },
            },
          ]),
        ),
      ).toBeNull();
    },
  );
});
