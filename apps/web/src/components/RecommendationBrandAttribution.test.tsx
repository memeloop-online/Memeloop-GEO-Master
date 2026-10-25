import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import type { MeasurementTarget } from "../api/projects";
import type { RecommendationItem } from "../api/recommendations";
import i18n from "../i18n";
import { RecommendationResults } from "./RecommendationResults";
import { RecommendationFocusComparison } from "./RecommendationFocusComparison";
import {
  RecommendationTrend,
  type RecommendationTrendSample,
} from "./RecommendationTrend";
import { RecommendationPlatformComparison } from "./RecommendationPlatformComparison";

const target: MeasurementTarget = {
  name: "Example brand",
  kind: "brand",
  aliases: ["Brand alias"],
};
const product: RecommendationItem = {
  name: "Example product",
  kind: "product",
  mention_order: 1,
  rank: 1,
  reason: "Portable",
  evidence_quote: "First: Example product is portable.",
  citation_urls: [],
  brand_attribution: {
    name: "Brand alias",
    evidence_quote: "Example product is made by Brand alias.",
  },
};
const direct: RecommendationItem = {
  ...product,
  name: target.name,
  kind: "brand",
  rank: 3,
  brand_attribution: null,
};
const result = (items: RecommendationItem[]) => ({
  version: "geo.recommendations.v2",
  status: "verified",
  items,
});
const sample = (
  id: string,
  items: RecommendationItem[],
): RecommendationTrendSample => ({
  sampleId: id,
  recordHref: `/records/${id}`,
  observedAt: "2026-10-01T00:00:00Z",
  measurement: {
    provider: "example-provider",
    model: "example-model",
    surface: "consumer_web",
    search_mode: "enabled",
    protocol_version: "v1",
    question_set_version: "temporary.v1",
    question: "Which portable product?",
    market: "US",
    language: "en",
  },
  recommendations: result(items),
});
afterEach(cleanup);
beforeEach(async () => {
  await i18n.changeLanguage("en");
});

describe("brand attribution presentation", () => {
  it.each([
    [
      "affiliate-only",
      [product],
      "Answers recommending affiliated products: 1 · Can be determined: 1 · Uncertain attribution: 0 · Valid answers: 1",
    ],
    [
      "direct-only",
      [direct],
      "Answers recommending affiliated products: 0 · Can be determined: 1 · Uncertain attribution: 0 · Valid answers: 1",
    ],
    [
      "both",
      [product, direct],
      "Answers recommending affiliated products: 1 · Can be determined: 1 · Uncertain attribution: 0 · Valid answers: 1",
    ],
    [
      "unknown",
      [{ ...product, brand_attribution: null }],
      "Answers recommending affiliated products: 0 · Can be determined: 0 · Uncertain attribution: 1 · Valid answers: 1",
    ],
    [
      "empty",
      [],
      "Answers recommending affiliated products: 0 · Can be determined: 1 · Uncertain attribution: 0 · Valid answers: 1",
    ],
  ] as const)(
    "keeps %s counts identical in results, trend and platform views",
    (_, items, counts) => {
      const answer = [...items];
      const samples = [sample("one", answer)];
      const { rerender } = render(
        <RecommendationResults
          value={result(answer)}
          focusTargets={[target]}
        />,
      );
      expect(screen.getByText(counts)).toBeVisible();
      expect(
        within(
          screen.getByRole("group", {
            name: "Affiliated products recommended",
          }),
        ).queryByText(/%/),
      ).not.toBeInTheDocument();
      rerender(
        <RecommendationTrend samples={samples} focusTargets={[target]} />,
      );
      expect(screen.getByText(counts)).toBeVisible();
      rerender(
        <RecommendationPlatformComparison
          samples={samples}
          focusTargets={[target]}
          selectedTarget={target}
          onSelectTarget={() => {}}
          planWindow={{
            start: "2026-10-01T00:00:00Z",
            end: "2026-10-02T00:00:00Z",
          }}
        />,
      );
      expect(screen.getByText(counts)).toBeVisible();
    },
  );

  it("shows affiliate products and original attribution without assigning their rank to the brand", () => {
    render(
      <RecommendationResults
        value={result([product])}
        focusTargets={[target]}
      />,
    );
    expect(
      screen.getByText(
        "Affiliated products recommended; no direct brand recommendation",
      ),
    ).toBeVisible();
    expect(
      screen.queryByText(`“${target.name}” is not in this recommendation list`),
    ).not.toBeInTheDocument();
    const card = within(screen.getByRole("article", { name: product.name }));
    expect(card.getByText("Product")).toBeVisible();
    expect(card.getByText("Recommended at #1")).toBeVisible();
    expect(card.getByText("Brand: Brand alias")).toBeVisible();
    fireEvent.click(card.getByText("View brand attribution excerpt"));
    expect(
      card.getByText(product.brand_attribution!.evidence_quote),
    ).toBeVisible();
    expect(card.queryByText("Tracked item")).not.toBeInTheDocument();
  });

  it("keeps direct brand rank and affiliate product rank in separate groups", () => {
    render(
      <RecommendationFocusComparison
        items={[product, direct]}
        focusTargets={[target]}
      />,
    );
    const brand = within(
      screen.getByRole("region", { name: "Example brand · Brand" }),
    );
    expect(brand.getByText("Direct brand recommendations")).toBeVisible();
    expect(
      within(brand.getByRole("article", { name: target.name })).getByText(
        "Recommended at #3",
      ),
    ).toBeVisible();
    const affiliates = within(
      brand.getByRole("region", { name: "Affiliated products recommended" }),
    );
    expect(
      within(affiliates.getByRole("article", { name: product.name })).getByText(
        "Recommended at #1",
      ),
    ).toBeVisible();
    expect(
      brand.queryByText("Recommendations ranked ahead"),
    ).not.toBeInTheDocument();
  });

  it("counts multiple attributed products in one answer once and excludes unavailable answers", () => {
    const samples = [
      sample("positive", [
        product,
        { ...product, name: "Another product", mention_order: 2 },
      ]),
      sample("unknown", [{ ...product, brand_attribution: null }]),
      sample("empty", []),
      { ...sample("unavailable", []), recommendations: null },
    ];
    render(<RecommendationTrend samples={samples} focusTargets={[target]} />);
    expect(
      screen.getByText(
        "Answers recommending affiliated products: 1 · Can be determined: 2 · Uncertain attribution: 1 · Valid answers: 3",
      ),
    ).toBeVisible();
    expect(screen.getByText("Recommended 0 / 3 times · 0%")).toBeVisible();
    expect(
      screen.getByText("Not applicable · 0 ranked recommendation samples"),
    ).toBeVisible();
  });

  it("uses shared Chinese attribution labels", async () => {
    await i18n.changeLanguage("zh-CN");
    render(
      <RecommendationResults
        value={result([product])}
        focusTargets={[target]}
      />,
    );
    expect(
      screen.getByText(
        "旗下产品获推荐 1 份回答 · 可判定 1 份 · 归属不确定 0 份 · 有效回答 1 份",
      ),
    ).toBeVisible();
    expect(
      screen.getByText("本次推荐了旗下产品，未直接推荐品牌"),
    ).toBeVisible();
  });
});
