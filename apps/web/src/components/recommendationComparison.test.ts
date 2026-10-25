import { describe, expect, it } from "vitest";
import {
  classifyBrandRecommendation,
  summarizeBrandRecommendations,
} from "../api/measurementFocus";
import type { MeasurementTarget } from "../api/projects";
import {
  decodeVerifiedRecommendations,
  type RecommendationItem,
} from "../api/recommendations";
import { recommendationFrequency } from "./recommendationComparison";

const brand: MeasurementTarget = {
  name: "Example brand",
  kind: "brand",
  aliases: ["Example alias"],
};

const direct: RecommendationItem = {
  name: brand.name,
  kind: "brand",
  mention_order: 1,
  rank: 2,
  reason: null,
  evidence_quote: "Example brand is recommended.",
  citation_urls: [],
  brand_attribution: null,
};

const product = (
  affiliation: string | null = brand.name,
  rank: number | null = 1,
): RecommendationItem => ({
  ...direct,
  name: "Example model",
  kind: "product",
  rank,
  brand_attribution:
    affiliation === null
      ? null
      : {
          name: affiliation,
          evidence_quote: `Example model is made by ${affiliation}.`,
        },
});

const frequency = (answers: (RecommendationItem[] | null)[]) =>
  recommendationFrequency(
    answers.map((items) => ({ items })),
    brand,
  );

describe("brand product affiliation", () => {
  it("keeps direct-only recommendation metrics independent", () => {
    const result = frequency([[direct]]);
    expect(result.recommended).toBe(1);
    expect(result.ranks).toEqual(new Map([[2, 1]]));
    expect(result.brandSummary).toEqual({
      positiveAnswers: 0,
      knownAnswers: 1,
      unknownAnswers: 0,
      validAnswers: 1,
    });
  });

  it("does not promote an affiliated product's first place to the brand", () => {
    const result = frequency([[product()]]);
    expect(result.recommended).toBe(0);
    expect(result.ranked).toBe(0);
    expect(result.ranks.size).toBe(0);
    expect(result.brandSummary?.positiveAnswers).toBe(1);
  });

  it("counts duplicate models, exact aliases and mixed known/unknown products once", () => {
    const result = frequency([
      [product(), product(" Example alias ", 2), product(null)],
      [direct, product(), product("Other brand")],
    ]);
    expect(result.recommended).toBe(1);
    expect(result.ranks).toEqual(new Map([[2, 1]]));
    expect(result.brandSummary).toEqual({
      positiveAnswers: 2,
      knownAnswers: 2,
      unknownAnswers: 0,
      validAnswers: 2,
    });
  });

  it("does not infer affiliation from model names, case or partial brand matches", () => {
    for (const affiliation of ["example brand", "Example brand Pro"])
      expect(classifyBrandRecommendation([product(affiliation)], brand)).toBe(
        "negative",
      );
    expect(
      classifyBrandRecommendation(
        [{ ...product(null), name: brand.name }],
        brand,
      ),
    ).toBe("unknown");
    expect(classifyBrandRecommendation([direct, product(null)], brand)).toBe(
      "unknown",
    );
  });

  it("partitions valid answers into positive, known-negative and unknown", () => {
    const result = frequency([
      [product()],
      [product("Other brand")],
      [],
      [direct],
      [product(null)],
      [product("Other brand"), product(null)],
      null,
    ]);
    expect(result.valid).toBe(6);
    expect(result.brandSummary).toEqual({
      positiveAnswers: 1,
      knownAnswers: 4,
      unknownAnswers: 2,
      validAnswers: 6,
    });
  });

  it("excludes missing, old and unverified snapshots without treating them as empty", () => {
    const snapshot = {
      version: "geo.recommendations.v2",
      status: "verified",
      items: [],
    };
    const answers = [
      snapshot,
      undefined,
      { ...snapshot, version: "geo.recommendations.v1" },
      { ...snapshot, status: "unverified" },
    ].map(decodeVerifiedRecommendations);
    expect(frequency(answers).brandSummary).toEqual({
      positiveAnswers: 0,
      knownAnswers: 1,
      unknownAnswers: 0,
      validAnswers: 1,
    });
  });

  it("preserves conflicting direct ranks without using product ranks", () => {
    const result = frequency([
      [direct, { ...direct, name: "Example alias", rank: 1 }, product()],
      [{ ...direct, rank: null }, product()],
    ]);
    expect(result).toMatchObject({
      recommended: 2,
      ambiguous: 1,
      unranked: 1,
      ranked: 0,
    });
    expect(result.ranks.size).toBe(0);
    expect(result.brandSummary?.positiveAnswers).toBe(2);
  });

  it("does not apply brand affiliation metrics to another target kind", () => {
    const focus: MeasurementTarget = {
      name: "Example model",
      kind: "product",
      aliases: [],
    };
    expect(classifyBrandRecommendation([product()], focus)).toBeNull();
    expect(summarizeBrandRecommendations([[product()]], focus)).toBeNull();
    expect(
      recommendationFrequency([{ items: [product()] }], focus),
    ).toMatchObject({ recommended: 1, brandSummary: null });
  });
});
