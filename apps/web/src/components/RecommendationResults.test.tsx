import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { RecommendationResults } from "./RecommendationResults";
import i18n from "../i18n";

afterEach(cleanup);
beforeEach(async () => {
  await i18n.changeLanguage("zh-CN");
});

const item = {
  name: "Sample purifier",
  kind: "product",
  mention_order: 1,
  rank: 2,
  reason: "Quiet operation",
  evidence_quote: "Second choice: Sample purifier for quiet operation.",
  citation_urls: ["https://example.test/review"],
};
const result = (items: unknown[], status = "verified") => ({
  version: "geo.recommendations.v1",
  status,
  items,
});

describe("recommendation results", () => {
  it("highlights only the explicit target and summarizes its actual rank", () => {
    render(
      <RecommendationResults
        value={result([
          item,
          {
            ...item,
            name: "Sample purifier Pro",
            mention_order: 2,
            rank: null,
          },
        ])}
        focusNames={["Sample purifier"]}
      />,
    );
    expect(screen.getByText("推荐第 2 位")).toBeVisible();
    expect(screen.getAllByText("关注对象")).toHaveLength(2);
  });

  it("does not call missing analysis an absent recommendation", () => {
    const { rerender } = render(
      <RecommendationResults value={null} focusNames={["Example"]} />,
    );
    expect(screen.queryByText(/本次推荐名单/)).not.toBeInTheDocument();
    rerender(
      <RecommendationResults value={result([item])} focusNames={["Example"]} />,
    );
    expect(screen.getByText("本次推荐名单中没有“Example”")).toBeVisible();
  });

  it("shows alternatives first and keeps the complete list collapsed when every focus is absent", () => {
    render(
      <RecommendationResults
        value={result([
          { ...item, mention_order: 2, rank: null, reason: null },
          { ...item, name: "Sample restaurant", kind: "restaurant", rank: 1 },
        ])}
        focusNames={["Sample purifier Pro", "Another focus"]}
      />,
    );
    expect(
      screen.getByText("本次推荐名单中没有“Sample purifier Pro”"),
    ).toBeVisible();
    expect(screen.getByText("本次推荐名单中没有“Another focus”")).toBeVisible();
    const section = screen.getByRole("region", { name: "本次回答推荐了什么" });
    expect(
      within(section)
        .getAllByRole("article")
        .map((card) => card.getAttribute("aria-label")),
    ).toEqual(["Sample restaurant", "Sample purifier"]);
    expect(within(section).getByText("Quiet operation")).toBeVisible();
    expect(within(section).getByText("已推荐，未明确排名")).toBeVisible();
    expect(within(section).getByText("未说明推荐理由")).toBeVisible();
    const summary = screen.getByText("查看完整推荐列表");
    const details = summary.closest("details");
    expect(details).not.toHaveAttribute("open");
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    fireEvent.click(summary);
    expect(details).toHaveAttribute("open");
    const table = screen.getByRole("table");
    expect(table).toBeVisible();
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(within(table).getByText("未明确排名")).toBeVisible();
  });

  it("retains the matched comparison and open list when only one tracked name is absent", () => {
    render(
      <RecommendationResults
        value={result([item])}
        focusNames={["Sample purifier", "Sample purifier Pro"]}
      />,
    );
    expect(
      screen.getByText("本次推荐名单中没有“Sample purifier Pro”"),
    ).toBeVisible();
    expect(screen.getByRole("table")).toBeVisible();
    expect(
      screen.getByRole("article", { name: "Sample purifier" }),
    ).toBeVisible();
    expect(screen.queryByText("查看完整推荐列表")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("region", { name: "本次回答推荐了什么" }),
    ).not.toBeInTheDocument();
  });

  it.each([
    undefined,
    null,
    result([item], "unverified"),
    result([{ ...item, rank: "first" }]),
    result([]),
  ])(
    "does not show fallback cards for unavailable or empty extraction (%s)",
    (value) => {
      render(<RecommendationResults value={value} focusNames={["Example"]} />);
      expect(screen.queryByRole("article")).not.toBeInTheDocument();
      expect(screen.queryByText("查看完整推荐列表")).not.toBeInTheDocument();
      expect(
        screen.queryByRole("region", { name: "本次回答推荐了什么" }),
      ).not.toBeInTheDocument();
    },
  );

  it("keeps the existing open list without a tracked name", () => {
    render(<RecommendationResults value={result([item])} />);
    expect(screen.getByRole("table")).toBeVisible();
    expect(screen.queryByRole("article")).not.toBeInTheDocument();
    expect(screen.queryByText("查看完整推荐列表")).not.toBeInTheDocument();
  });
  it("shows extracted entity order separately from explicit ranking", () => {
    render(
      <RecommendationResults
        value={result([
          {
            ...item,
            name: "Sample restaurant",
            kind: "restaurant",
            mention_order: 2,
            rank: null,
            reason: null,
          },
          item,
        ])}
      />,
    );
    const rows = screen.getAllByRole("row");
    const first = within(rows[1]).getAllByRole("cell");
    expect(first[0]).toHaveTextContent("Sample purifier");
    expect(first[1]).toHaveTextContent("2");
    expect(first[2]).toHaveTextContent("1");
    const second = within(rows[2]).getAllByRole("cell");
    expect(second[0]).toHaveTextContent("Sample restaurant");
    expect(second[1]).toHaveTextContent("未明确排名");
    expect(second[2]).toHaveTextContent("2");
    expect(second[3]).toHaveTextContent("未说明理由");
  });

  it.each([undefined, null])(
    "shows missing analysis without treating it as no recommendations (%s)",
    (value) => {
      render(<RecommendationResults value={value} />);
      expect(screen.getByText("推荐结果尚未分析")).toBeVisible();
      expect(screen.queryByText("未发现具体推荐对象")).not.toBeInTheDocument();
      expect(screen.queryByRole("table")).not.toBeInTheDocument();
    },
  );

  it("distinguishes verified empty extraction from unverified extraction", () => {
    const { rerender } = render(<RecommendationResults value={result([])} />);
    expect(screen.getByText("未发现具体推荐对象")).toBeVisible();
    rerender(<RecommendationResults value={result([item], "unverified")} />);
    expect(screen.getByText("未能识别推荐对象")).toBeVisible();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    expect(screen.queryByText("Sample purifier")).not.toBeInTheDocument();
  });

  it("treats malformed or unsupported extraction as unverified", () => {
    const { rerender } = render(
      <RecommendationResults value={result([{ ...item, rank: "first" }])} />,
    );
    expect(screen.getByText("未能识别推荐对象")).toBeVisible();
    rerender(
      <RecommendationResults
        value={{ ...result([item]), version: "unsupported" }}
      />,
    );
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("uses only safe source links and renders evidence as text", () => {
    const { container } = render(
      <RecommendationResults
        value={result([
          {
            ...item,
            evidence_quote: "<script>not executable</script>",
            citation_urls: [
              ...item.citation_urls,
              ...item.citation_urls,
              "javascript:alert(1)",
              "https://user:secret@example.test/private",
              "/settings",
            ],
          },
        ])}
      />,
    );
    const links = container.querySelectorAll("a");
    expect(links).toHaveLength(1);
    expect(links[0]).toHaveAttribute("href", "https://example.test/review");
    expect(links[0]).toHaveAttribute("rel", "noopener noreferrer");
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("blockquote")).toHaveTextContent(
      "<script>not executable</script>",
    );
  });

  it("renders English labels from shared translations", async () => {
    await i18n.changeLanguage("en");
    render(<RecommendationResults value={result([{ ...item, rank: null }])} />);
    expect(
      screen.getByRole("table", { name: "Recommendations" }),
    ).toBeVisible();
    expect(screen.getByText("No explicit rank")).toBeVisible();
    cleanup();
    render(<RecommendationResults value={undefined} />);
    expect(screen.getByText("Recommendations not analyzed yet")).toBeVisible();
  });

  it("uses the English full-list disclosure with absent-focus cards", async () => {
    await i18n.changeLanguage("en");
    render(
      <RecommendationResults
        value={result([item])}
        focusNames={["Sample purifier Pro"]}
      />,
    );
    expect(
      screen.getByRole("region", { name: "What this answer recommended" }),
    ).toBeVisible();
    expect(screen.getByText("View the full recommendation list")).toBeVisible();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });
});
