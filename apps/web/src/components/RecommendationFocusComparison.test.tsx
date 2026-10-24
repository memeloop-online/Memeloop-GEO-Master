import type { MeasurementTarget } from "../api/projects";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import type { RecommendationItem } from "../api/recommendations";
import i18n from "../i18n";
import { RecommendationFocusComparison } from "./RecommendationFocusComparison";

const trackedTargets = (names: string[]): MeasurementTarget[] =>
  names.map((name) => ({ name, kind: "product", aliases: [] }));

afterEach(cleanup);
beforeEach(async () => {
  await i18n.changeLanguage("zh-CN");
});

const item = (
  overrides: Partial<RecommendationItem> = {},
): RecommendationItem => ({
  name: "Example target",
  kind: "product",
  mention_order: 1,
  rank: 3,
  reason: "Target has quiet operation.",
  evidence_quote: "Third choice: Example target for quiet operation.",
  citation_urls: ["https://example.test/review"],
  ...overrides,
});

describe("tracked recommendation comparison", () => {
  it("hands the selected occurrence to its caller, including repeated names", () => {
    const first = item({ mention_order: 1, reason: "First occurrence." });
    const second = item({ mention_order: 3, reason: "Second occurrence." });
    const onAnalyze = vi.fn();
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets([first.name])}
        items={[first, second]}
        onAnalyze={onAnalyze}
      />,
    );
    fireEvent.click(
      within(screen.getAllByRole("article")[1]).getByRole("button", {
        name: "分析改进方向",
      }),
    );
    expect(onAnalyze).toHaveBeenCalledExactlyOnceWith(second);
  });
  it("shows the target first and only same-kind recommendations with strictly smaller ranks", () => {
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["Example target"])}
        items={[
          item({
            name: "Second alternative",
            rank: 2,
            reason: "Second reason.",
          }),
          item({ name: "First alternative", rank: 1, reason: "First reason." }),
          item(),
          item({ name: "Tied alternative", rank: 3 }),
          item({ name: "Later alternative", rank: 4 }),
          item({ name: "Unranked alternative", rank: null }),
          item({ name: "Different kind", rank: 1, kind: "brand" }),
        ]}
      />,
    );
    const cards = screen.getAllByRole("article");
    expect(cards.map((card) => card.getAttribute("aria-label"))).toEqual([
      "Example target",
      "First alternative",
      "Second alternative",
    ]);
    expect(within(cards[0]).getByText("推荐第 3 位")).toBeVisible();
    expect(screen.getByText("Target has quiet operation.")).toBeVisible();
    expect(screen.getByText("First reason.")).toBeVisible();
    expect(screen.getByText("Second reason.")).toBeVisible();
    expect(screen.getByText("排名更靠前的推荐")).toBeVisible();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("does not treat ties or earlier mentions as recommendations ranked ahead", () => {
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["Example target"])}
        items={[
          item({ name: "Earlier mention", rank: null, mention_order: 1 }),
          item({ name: "Tied alternative", rank: 2, mention_order: 2 }),
          item({ rank: 2, mention_order: 3 }),
        ]}
      />,
    );
    expect(screen.getAllByRole("article")).toHaveLength(1);
    expect(
      screen.getByText("本次同类推荐中，没有明确排名更靠前的对象。"),
    ).toBeVisible();
    expect(screen.queryByText("排名更靠前的推荐")).not.toBeInTheDocument();
  });

  it("keeps an unranked target unranked without inferring a comparison", () => {
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["Example target"])}
        items={[
          item({ name: "Ranked alternative", rank: 1 }),
          item({ rank: null, mention_order: 4 }),
        ]}
      />,
    );
    expect(screen.getByText("已推荐，未明确排名")).toBeVisible();
    expect(
      screen.getByText("本次回答未给出该对象的明确排名，暂无法比较先后。"),
    ).toBeVisible();
    expect(screen.queryByText("Ranked alternative")).not.toBeInTheDocument();
  });

  it("shows conflicting target occurrences without choosing a rank for comparison", () => {
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["Example target"])}
        items={[
          item({ rank: 2, reason: "First observed reason." }),
          item({
            rank: 4,
            mention_order: 3,
            reason: "Second observed reason.",
          }),
          item({ name: "Alternative", rank: 1 }),
        ]}
      />,
    );
    expect(
      screen.getAllByRole("article", { name: "Example target" }),
    ).toHaveLength(2);
    expect(screen.getByText("推荐第 2 位")).toBeVisible();
    expect(screen.getByText("推荐第 4 位")).toBeVisible();
    expect(screen.getByText("First observed reason.")).toBeVisible();
    expect(screen.getByText("Second observed reason.")).toBeVisible();
    expect(
      screen.getByText("同一关注对象出现不同排名，暂无法比较先后。"),
    ).toBeVisible();
    expect(screen.queryByText("Alternative")).not.toBeInTheDocument();
    expect(screen.queryByText("排名更靠前的推荐")).not.toBeInTheDocument();
  });

  it("keeps equal names in different kinds in separate comparisons", () => {
    render(
      <RecommendationFocusComparison
        focusTargets={[
          ...trackedTargets(["Example target"]),
          { name: "Example target", kind: "brand", aliases: [] },
        ]}
        items={[
          item(),
          item({ kind: "brand", rank: 1 }),
          item({ name: "Product alternative", rank: 2 }),
          item({ name: "Brand alternative", kind: "brand", rank: 2 }),
        ]}
      />,
    );
    const product = screen.getByRole("region", {
      name: "Example target · 产品",
    });
    const brand = screen.getByRole("region", { name: "Example target · 品牌" });
    expect(within(product).getByText("Product alternative")).toBeVisible();
    expect(
      within(brand).queryByText("Product alternative"),
    ).not.toBeInTheDocument();
    expect(
      within(brand).queryByText("Brand alternative"),
    ).not.toBeInTheDocument();
  });

  it("matches trimmed exact names without matching other models or casing", () => {
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["  Example target  "])}
        items={[
          item({ name: " Example target " }),
          item({ name: "Example target Pro", rank: 4 }),
          item({ name: "example target", rank: 4 }),
        ]}
      />,
    );
    expect(screen.getAllByRole("article")).toHaveLength(1);
    expect(screen.getByText("Example target")).toBeVisible();
    expect(screen.queryByText("Example target Pro")).not.toBeInTheDocument();
    expect(screen.queryByText("example target")).not.toBeInTheDocument();
  });

  it.each([
    { focusTargets: [], items: [item()] },
    { focusTargets: trackedTargets(["Example target"]), items: [] },
  ])(
    "renders nothing without focus names or recommendations ($focusTargets)",
    ({ focusTargets, items }) => {
      const { container } = render(
        <RecommendationFocusComparison
          items={items}
          focusTargets={focusTargets}
        />,
      );
      expect(container).toBeEmptyDOMElement();
    },
  );

  it("shows every occurrence and its actual kind in mention order when the exact model is absent", () => {
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["Example target Pro"])}
        items={[
          item({ kind: "brand", mention_order: 4, rank: 1 }),
          item({ name: "Other item", kind: "other", mention_order: 6 }),
          item({ mention_order: 1, rank: 4 }),
          item({
            name: "Example service",
            kind: "service",
            mention_order: 5,
            rank: null,
          }),
          item({
            mention_order: 3,
            rank: null,
            reason: "A separate occurrence without a rank.",
          }),
          item({
            name: "Example restaurant",
            kind: "restaurant",
            mention_order: 2,
            rank: 2,
          }),
        ]}
      />,
    );
    const section = screen.getByRole("region", { name: "本次回答推荐了什么" });
    expect(section).toBeVisible();
    expect(
      within(section).getByText(
        "以下内容展示本次回答中的推荐理由与原文依据，并不解释关注对象为何未被推荐。",
      ),
    ).toBeVisible();
    const cards = within(section).getAllByRole("article");
    expect(cards.map((card) => card.getAttribute("aria-label"))).toEqual([
      "Example target",
      "Example restaurant",
      "Example target",
      "Example target",
      "Example service",
      "Other item",
    ]);
    expect(within(cards[0]).getByText("产品")).toBeVisible();
    expect(within(cards[0]).getByText("推荐第 4 位")).toBeVisible();
    expect(within(cards[1]).getByText("餐馆")).toBeVisible();
    expect(within(cards[2]).getByText("已推荐，未明确排名")).toBeVisible();
    expect(
      within(cards[2]).getByText("A separate occurrence without a rank."),
    ).toBeVisible();
    expect(within(cards[3]).getByText("品牌")).toBeVisible();
    expect(within(cards[4]).getByText("服务")).toBeVisible();
    expect(within(cards[5]).getByText("其他对象")).toBeVisible();
    expect(screen.queryByText("关注对象")).not.toBeInTheDocument();
    expect(screen.queryByText("排名更靠前的推荐")).not.toBeInTheDocument();
    expect(screen.queryByText("Example target Pro")).not.toBeInTheDocument();
    expect(screen.queryByText(/暂无法比较先后/)).not.toBeInTheDocument();
  });

  it.each(["Example target", "Example target Pro"])(
    "keeps evidence collapsed and renders only safe citations (%s)",
    (focusName) => {
      const { container } = render(
        <RecommendationFocusComparison
          focusTargets={trackedTargets([focusName])}
          items={[
            item({
              evidence_quote: "<script>literal evidence</script>",
              citation_urls: [
                "https://example.test/review",
                "https://example.test/review",
                "http://example.test/other",
                "javascript:alert(1)",
                "data:text/html,unsafe",
                "https://user:secret@example.test/private",
                "/relative",
              ],
            }),
          ]}
        />,
      );
      const details = container.querySelector("details");
      expect(details).not.toHaveAttribute("open");
      expect(
        screen.getByText("<script>literal evidence</script>"),
      ).not.toBeVisible();
      fireEvent.click(screen.getByText("查看原文依据与引用"));
      expect(details).toHaveAttribute("open");
      expect(
        screen.getByText("<script>literal evidence</script>"),
      ).toBeVisible();
      expect(container.querySelector("script")).toBeNull();
      const links = screen.getAllByRole("link");
      expect(links.map((link) => link.getAttribute("href"))).toEqual([
        "https://example.test/review",
        "http://example.test/other",
      ]);
      for (const link of links) {
        expect(link).toHaveAttribute("target", "_blank");
        expect(link).toHaveAttribute("rel", "noopener noreferrer");
      }
    },
  );

  it("uses English labels for the absent-focus recommendations", async () => {
    await i18n.changeLanguage("en");
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["Example target Pro"])}
        items={[item({ reason: null, rank: null })]}
      />,
    );
    expect(
      screen.getByRole("region", { name: "What this answer recommended" }),
    ).toBeVisible();
    expect(screen.getByText("Product")).toBeVisible();
    expect(screen.getByText("No recommendation reason given")).toBeVisible();
    expect(
      screen.getByText("Recommended without an explicit rank"),
    ).toBeVisible();
    expect(
      screen.getByText(
        "These are the recommendation reasons and supporting excerpts from this answer; they do not explain why the tracked item was not recommended.",
      ),
    ).toBeVisible();
  });

  it("uses English translations and preserves missing reasons", async () => {
    await i18n.changeLanguage("en");
    render(
      <RecommendationFocusComparison
        focusTargets={trackedTargets(["Example target"])}
        items={[item({ reason: null }), item({ name: "Alternative", rank: 1 })]}
      />,
    );
    expect(
      screen.getByRole("region", {
        name: "Tracked item and similar recommendations",
      }),
    ).toBeVisible();
    expect(screen.getByText("Recommendations ranked ahead")).toBeVisible();
    expect(screen.getByText("No recommendation reason given")).toBeVisible();
    expect(screen.getByText("Recommended at #3")).toBeVisible();
  });

  it("groups explicit aliases under the canonical target and excludes all tracked aliases from competitors", () => {
    const canonical = {
      name: "Example target",
      kind: "product" as const,
      aliases: ["Target short"],
    };
    const companion = {
      name: "Example companion",
      kind: "product" as const,
      aliases: ["Companion short"],
    };
    const aliasItem = item({ name: "Target short", rank: 3 });
    const onAnalyze = vi.fn();
    render(
      <RecommendationFocusComparison
        focusTargets={[canonical, companion]}
        items={[
          aliasItem,
          item({ name: "Companion short", rank: 1, mention_order: 2 }),
          item({ name: "Alternative", rank: 2, mention_order: 3 }),
        ]}
        onAnalyze={onAnalyze}
      />,
    );
    const target = within(
      screen.getByRole("region", { name: "Example target · 产品" }),
    );
    expect(target.getByText("Target short")).toBeVisible();
    expect(target.getByText("Alternative")).toBeVisible();
    expect(target.queryByText("Companion short")).not.toBeInTheDocument();
    fireEvent.click(
      within(target.getByRole("article", { name: "Target short" })).getByRole(
        "button",
        { name: "分析改进方向" },
      ),
    );
    expect(onAnalyze).toHaveBeenCalledExactlyOnceWith(aliasItem);
  });
  it.each([
    [1, 3],
    [1, null],
  ])("keeps conflicting alias ranks ambiguous: %j", (first, second) => {
    render(
      <RecommendationFocusComparison
        focusTargets={[
          {
            name: "Example target",
            kind: "product",
            aliases: ["Target short"],
          },
        ]}
        items={[
          item({ rank: first }),
          item({ name: "Target short", rank: second, mention_order: 2 }),
          item({ name: "Alternative", rank: 1, mention_order: 3 }),
        ]}
      />,
    );
    expect(
      screen.getByText("同一关注对象出现不同排名，暂无法比较先后。"),
    ).toBeVisible();
    expect(screen.queryByText("Alternative")).not.toBeInTheDocument();
  });
});
