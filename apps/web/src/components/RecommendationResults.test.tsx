import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
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

  it("does not mistake legacy results for no recommendations", () => {
    const { container } = render(<RecommendationResults value={undefined} />);
    expect(container).toBeEmptyDOMElement();
  });

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
  });
});
