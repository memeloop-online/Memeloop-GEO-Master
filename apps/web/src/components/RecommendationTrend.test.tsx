import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import {
  RecommendationTrend,
  type RecommendationTrendSample,
} from "./RecommendationTrend";
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
  citation_urls: [],
};

const recommendations = (items: unknown[], status = "verified") => ({
  version: "geo.recommendations.v1",
  status,
  items,
});

function sample(
  sampleId: string,
  overrides: Partial<RecommendationTrendSample> = {},
): RecommendationTrendSample {
  return {
    sampleId,
    recordHref: `/app/tenant/project/measurement?tab=records&record=${sampleId}`,
    observedAt: "2026-10-01T10:00:00Z",
    analysisRevisionId: `analysis-${sampleId}`,
    measurement: {
      kind: "measure",
      account_id: "sample-account",
      provider: "sample-provider",
      model: "sample-model",
      surface: "consumer_web",
      search_mode: "enabled",
      protocol_version: "sample-protocol-v1",
      question_set_version: "sample-version",
      question: "Which purifier suits a small room?",
      market: "CN",
      language: "zh-CN",
      scheduled_at: "2026-10-01T10:00:00Z",
      sample_ordinal: 1,
    },
    recommendations: recommendations([item]),
    ...overrides,
  };
}

describe("recommendation rank history", () => {
  it("compares each explicit rank chronologically while keeping mention order separate", () => {
    const first = sample("first");
    const later = sample("later", {
      observedAt: "2026-10-03T10:00:00Z",
      recommendations: recommendations([
        { ...item, rank: 1, mention_order: 3 },
      ]),
    });
    render(<RecommendationTrend samples={[later, first]} />);
    const table = screen.getByRole("table");
    const headers = within(table).getAllByRole("columnheader");
    expect(within(headers[1]).getByRole("link")).toHaveAttribute(
      "href",
      first.recordHref,
    );
    expect(within(headers[2]).getByRole("link")).toHaveAttribute(
      "href",
      later.recordHref,
    );
    const cells = within(within(table).getAllByRole("row")[1]).getAllByRole(
      "cell",
    );
    expect(cells[1]).toHaveTextContent("第 2 位");
    expect(cells[1]).toHaveTextContent("出现顺序：1");
    expect(cells[2]).toHaveTextContent("第 1 位");
    expect(cells[2]).toHaveTextContent("出现顺序：3");
    expect(screen.getByText("推荐结果可用 2 / 2 次")).toBeVisible();
  });

  it("does not assign a bottom rank to unranked or absent items", () => {
    render(
      <RecommendationTrend
        samples={[
          sample("ranked"),
          sample("unranked", {
            recommendations: recommendations([{ ...item, rank: null }]),
          }),
          sample("empty", { recommendations: recommendations([]) }),
        ]}
      />,
    );
    const cells = within(screen.getAllByRole("row")[1]).getAllByRole("cell");
    expect(cells[1]).toHaveTextContent("第 2 位");
    expect(cells[2]).toHaveTextContent("未明确排名");
    expect(cells[2]).toHaveTextContent("出现顺序：1");
    expect(cells[3]).toHaveTextContent("未出现在推荐结果中");
    expect(cells[3]).not.toHaveTextContent("位");
    expect(screen.getByText("推荐结果可用 3 / 3 次")).toBeVisible();
  });

  it("distinguishes unavailable extraction from verified empty extraction", () => {
    render(
      <RecommendationTrend
        samples={[
          sample("verified"),
          sample("missing", { recommendations: undefined }),
          sample("unverified", {
            recommendations: recommendations([item], "unverified"),
          }),
          sample("empty", { recommendations: recommendations([]) }),
        ]}
      />,
    );
    const cells = within(screen.getAllByRole("row")[1]).getAllByRole("cell");
    expect(cells[2]).toHaveTextContent("推荐结果尚未分析");
    expect(cells[3]).toHaveTextContent("推荐结果暂不可用");
    expect(cells[4]).toHaveTextContent("未出现在推荐结果中");
    expect(screen.getByText("推荐结果可用 2 / 4 次")).toBeVisible();
  });

  it.each([
    ["question", "Which purifier suits a large room?"],
    ["provider", "another-provider"],
    ["model", "another-model"],
    ["surface", "api"],
    ["search_mode", "disabled"],
    ["protocol_version", "sample-protocol-v2"],
    ["question_set_version", "another-version"],
    ["market", "US"],
    ["language", "en"],
  ])("keeps different %s values in separate comparisons", (field, value) => {
    const first = sample("first");
    const second = sample("second", {
      measurement: { ...first.measurement, [field]: value },
    });
    render(<RecommendationTrend samples={[first, second]} />);
    expect(screen.getAllByRole("table")).toHaveLength(2);
    expect(screen.getAllByText("推荐结果可用 1 / 1 次")).toHaveLength(2);
  });

  it("separates bound question revisions even when their question text is identical", () => {
    const first = sample("first");
    first.measurement.question_binding = {
      reference: {
        question_set_id: "sample-set",
        question_set_version_id: "sample-set-version",
        question_id: "sample-question",
        question_revision_id: "first-revision",
      },
      purpose: "optimization",
      split_policy_version: "sample-split-v1",
    };
    const second = sample("second", {
      measurement: {
        ...first.measurement,
        question_binding: {
          ...first.measurement.question_binding,
          reference: {
            ...first.measurement.question_binding.reference,
            question_revision_id: "second-revision",
          },
        },
      },
    });
    render(<RecommendationTrend samples={[first, second]} />);
    expect(screen.getAllByRole("table")).toHaveLength(2);
  });

  it("does not merge aliases or object types by guessing entity identity", () => {
    render(
      <RecommendationTrend
        samples={[
          sample("first"),
          sample("second", {
            recommendations: recommendations([
              { ...item, name: "Sample purifier plus" },
              { ...item, kind: "brand", mention_order: 2 },
            ]),
          }),
        ]}
      />,
    );
    expect(screen.getAllByRole("row")).toHaveLength(4);
    expect(screen.getAllByText("未出现在推荐结果中")).toHaveLength(3);
  });

  it.each([
    recommendations([{ ...item, rank: "first" }]),
    recommendations([{ ...item, mention_order: 0 }]),
    { ...recommendations([item]), version: "unsupported" },
  ])(
    "rejects malformed or unsupported extraction without inventing rows",
    (value) => {
      render(
        <RecommendationTrend
          samples={[sample("invalid", { recommendations: value })]}
        />,
      );
      expect(screen.getByText("推荐结果暂不可用")).toBeVisible();
      expect(screen.queryByText(item.name)).not.toBeInTheDocument();
      expect(
        screen.queryByText("可用结果中未发现具体推荐对象。"),
      ).not.toBeInTheDocument();
    },
  );

  it("handles verified empty extraction, no samples, and a single measurement honestly", () => {
    const { rerender } = render(<RecommendationTrend samples={[]} />);
    expect(screen.getByText("还没有可比较的推荐测量记录。")).toBeVisible();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    rerender(
      <RecommendationTrend
        samples={[sample("empty", { recommendations: recommendations([]) })]}
      />,
    );
    expect(screen.getByText("可用结果中未发现具体推荐对象。")).toBeVisible();
    expect(
      screen.getByText("当前只有一次测量，后续同条件测量可用于对比变化。"),
    ).toBeVisible();
  });

  it("renders archived evidence as text and handles a missing observation date", () => {
    const { container } = render(
      <RecommendationTrend
        samples={[
          sample("first", {
            observedAt: "invalid-date",
            recommendations: recommendations([
              { ...item, evidence_quote: "<script>example</script>" },
            ]),
          }),
        ]}
      />,
    );
    expect(screen.getByRole("link")).toHaveAttribute(
      "href",
      sample("first").recordHref,
    );
    expect(screen.getByText("时间未记录")).toBeVisible();
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByText("<script>example</script>")).toBeInTheDocument();
  });

  it("renders English labels from shared resources", async () => {
    await i18n.changeLanguage("en");
    render(
      <RecommendationTrend
        samples={[
          sample("first", {
            recommendations: recommendations([{ ...item, rank: null }]),
          }),
        ]}
      />,
    );
    expect(
      screen.getByRole("region", { name: "Recommendation rank history" }),
    ).toBeVisible();
    expect(screen.getByText("No explicit rank")).toBeVisible();
    expect(screen.getByText("Mention order: 1")).toBeVisible();
  });
});
