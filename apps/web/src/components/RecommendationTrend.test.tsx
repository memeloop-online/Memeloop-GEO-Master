import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
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
  const datedSample = (
    sampleId: string,
    day: number,
    items: unknown[] = [item],
  ) =>
    sample(sampleId, {
      observedAt: `2026-10-${String(day).padStart(2, "0")}T10:00:00Z`,
      recommendations: recommendations(items),
      recordHref: `/app/tenant/project/measurement?tab=records&record=${sampleId}&analysis=analysis-${sampleId}`,
    });

  it.each([
    [3, 1, "排名上升 2 位"],
    [1, 4, "排名下降 3 位"],
    [2, 2, "排名持平"],
  ])(
    "shows the tracked item's explicit rank change from %s to %s",
    (previous, latest, label) => {
      const first = datedSample("first", 1, [
        { ...item, rank: previous, mention_order: 5 },
      ]);
      const last = datedSample("last", 3, [
        { ...item, rank: latest, mention_order: 1 },
      ]);
      render(
        <RecommendationTrend
          samples={[last, first]}
          focusNames={[item.name]}
        />,
      );
      const summary = within(
        screen.getByRole("region", { name: "关注对象最近变化" }),
      );
      expect(summary.getByText(label)).toBeVisible();
      expect(summary.getAllByText(`第 ${previous} 位`)[0]).toBeVisible();
      expect(
        summary.getAllByRole("link").map((link) => link.getAttribute("href")),
      ).toEqual([first.recordHref, last.recordHref]);
      const disclosure = screen.getByText("完整排名对比");
      expect(disclosure.closest("details")).not.toHaveAttribute("open");
      expect(screen.getByRole("table", { hidden: true })).not.toBeVisible();
      fireEvent.click(disclosure);
      expect(screen.getByRole("table")).toBeVisible();
      const row = screen.getAllByRole("row")[1];
      expect(within(row).getByText("关注对象")).toBeVisible();
      expect(row).toHaveStyle({
        backgroundColor: "var(--colorBrandBackground2)",
      });
    },
  );

  it("uses the immediate last two samples without skipping unavailable extraction", () => {
    render(
      <RecommendationTrend
        samples={[
          datedSample("old", 1, [{ ...item, rank: 4 }]),
          { ...datedSample("missing", 2), recommendations: undefined },
          datedSample("latest", 3, [{ ...item, rank: 1 }]),
        ]}
        focusNames={[item.name]}
      />,
    );
    const summary = within(
      screen.getByRole("region", { name: "关注对象最近变化" }),
    );
    expect(summary.getByText(/推荐结果尚未分析/)).toBeVisible();
    expect(summary.getByText(/第 1 位/)).toBeVisible();
    expect(summary.getByText("这两次结果无法比较明确排名。")).toBeVisible();
    expect(summary.queryByText(/排名上升/)).not.toBeInTheDocument();
    expect(
      summary.getAllByRole("link").map((link) => link.getAttribute("href")),
    ).toEqual([
      datedSample("missing", 2).recordHref,
      datedSample("latest", 3).recordHref,
    ]);
  });

  it.each([
    [
      recommendations([{ ...item, rank: null, mention_order: 1 }]),
      "未明确排名",
    ],
    [recommendations([]), "未出现在推荐结果中"],
    [undefined, "推荐结果尚未分析"],
    [recommendations([item], "unverified"), "推荐结果暂不可用"],
    [
      recommendations([item, { ...item, rank: 1, mention_order: 2 }]),
      "同名同类型有多个结果，排名不唯一",
    ],
  ])(
    "shows a distinct latest state when rank cannot be compared",
    (result, label) => {
      render(
        <RecommendationTrend
          samples={[
            datedSample("first", 1),
            { ...datedSample("last", 2), recommendations: result },
          ]}
          focusNames={[item.name]}
        />,
      );
      const summary = within(
        screen.getByRole("region", { name: "关注对象最近变化" }),
      );
      expect(summary.getByText(new RegExp(label))).toBeVisible();
      expect(summary.getByText("这两次结果无法比较明确排名。")).toBeVisible();
      expect(
        summary.queryByText(/排名上升|排名下降|排名持平/),
      ).not.toBeInTheDocument();
    },
  );

  it("matches tracked entities by exact name and kind without merging aliases", () => {
    render(
      <RecommendationTrend
        samples={[
          datedSample("first", 1, [{ ...item, rank: 3 }]),
          datedSample("last", 2, [
            { ...item, kind: "brand", rank: 1 },
            { ...item, name: `${item.name} plus`, rank: 1, mention_order: 2 },
          ]),
        ]}
        focusNames={[item.name]}
      />,
    );
    const summary = within(
      screen.getByRole("region", { name: "关注对象最近变化" }),
    );
    expect(
      summary.getByRole("region", { name: `${item.name} · 产品` }),
    ).toHaveTextContent("未出现在推荐结果中");
    expect(
      summary.getByRole("region", { name: `${item.name} · 品牌` }),
    ).toHaveTextContent("未出现在推荐结果中");
    expect(
      summary.queryByText(/排名上升|排名下降|排名持平/),
    ).not.toBeInTheDocument();
    expect(summary.queryByText(`${item.name} plus`)).not.toBeInTheDocument();
  });

  it("uses the shared whitespace matching for focus selection and row highlighting", () => {
    render(
      <RecommendationTrend
        samples={[
          datedSample("first", 1, [
            { ...item, name: ` ${item.name} `, rank: 3 },
          ]),
          datedSample("last", 2, [
            { ...item, name: ` ${item.name} `, rank: 1 },
          ]),
        ]}
        focusNames={[item.name, ` ${item.name} `]}
      />,
    );
    const summary = within(
      screen.getByRole("region", { name: "关注对象最近变化" }),
    );
    expect(summary.getByText("排名上升 2 位")).toBeVisible();
    expect(summary.getAllByRole("region")).toHaveLength(1);
    fireEvent.click(screen.getByText("完整排名对比"));
    const row = screen.getAllByRole("row")[1];
    expect(within(row).getByText("关注对象")).toBeVisible();
    expect(row).toHaveStyle({
      backgroundColor: "var(--colorBrandBackground2)",
    });
  });

  it("keeps raw extracted names separate across samples despite focus whitespace matching", () => {
    render(
      <RecommendationTrend
        samples={[
          datedSample("first", 1, [
            { ...item, name: ` ${item.name} `, rank: 3 },
          ]),
          datedSample("last", 2, [{ ...item, rank: 1 }]),
        ]}
        focusNames={[item.name]}
      />,
    );
    const summary = within(
      screen.getByRole("region", { name: "关注对象最近变化" }),
    );
    expect(summary.getAllByRole("region")).toHaveLength(2);
    expect(summary.getAllByText("未出现在推荐结果中")).toHaveLength(2);
    expect(
      summary.queryByText(/排名上升|排名下降|排名持平/),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("完整排名对比"));
    expect(screen.getAllByText("关注对象")).toHaveLength(2);
    expect(screen.getAllByRole("row")).toHaveLength(3);
  });

  it("shows an unseen focus name as absent only for verified samples", () => {
    render(
      <RecommendationTrend
        samples={[
          datedSample("first", 1, []),
          { ...datedSample("last", 2, []), recommendations: undefined },
        ]}
        focusNames={["Tracked sample"]}
      />,
    );
    const summary = within(
      screen.getByRole("region", { name: "关注对象最近变化" }),
    );
    expect(summary.getByText("Tracked sample")).toBeVisible();
    expect(summary.getByText(/未出现在推荐结果中/)).toBeVisible();
    expect(summary.getByText(/推荐结果尚未分析/)).toBeVisible();
  });

  it.each([null, "invalid-date", "2026-10-02T10:00:00Z"])(
    "does not infer chronology for missing, invalid or tied times: %s",
    (observedAt) => {
      render(
        <RecommendationTrend
          samples={[
            { ...datedSample("first", 1, [{ ...item, rank: 3 }]), observedAt },
            datedSample("last", 2, [{ ...item, rank: 1 }]),
          ]}
          focusNames={[item.name]}
        />,
      );
      const summary = within(
        screen.getByRole("region", { name: "关注对象最近变化" }),
      );
      expect(
        summary.getByText("测量时间缺失或重复，暂无法确定最近两次的先后。"),
      ).toBeVisible();
      expect(
        summary.queryByText(/排名上升|排名下降|排名持平/),
      ).not.toBeInTheDocument();
    },
  );

  it("does not pick one of two equally recent previous samples", () => {
    render(
      <RecommendationTrend
        samples={[
          datedSample("first", 1),
          datedSample("tied", 1),
          datedSample("last", 2),
        ]}
        focusNames={[item.name]}
      />,
    );
    expect(
      within(
        screen.getByRole("region", { name: "关注对象最近变化" }),
      ).getByText("测量时间缺失或重复，暂无法确定最近两次的先后。"),
    ).toBeVisible();
  });

  it("does not mutate samples, extracted items or focus names", () => {
    const samples = [
      datedSample("last", 2, [{ ...item, mention_order: 3 }]),
      datedSample("first", 1),
    ];
    const focusNames = [item.name, item.name];
    const original = JSON.stringify({ samples, focusNames });
    render(<RecommendationTrend samples={samples} focusNames={focusNames} />);
    expect(JSON.stringify({ samples, focusNames })).toBe(original);
    expect(
      screen.getAllByRole("region", { name: `${item.name} · 产品` }),
    ).toHaveLength(1);
  });

  it("compares only the last two of three samples and translates the summary", async () => {
    await i18n.changeLanguage("en");
    const previous = datedSample("previous", 2, [
      { ...item, rank: 3, mention_order: 1 },
    ]);
    const latest = datedSample("latest", 3, [
      { ...item, rank: 1, mention_order: 6 },
    ]);
    render(
      <RecommendationTrend
        samples={[
          latest,
          datedSample("old", 1, [{ ...item, rank: 7 }]),
          previous,
        ]}
        focusNames={[item.name]}
      />,
    );
    const summary = within(
      screen.getByRole("region", { name: "Latest tracked item changes" }),
    );
    expect(summary.getByText("Rank improved by 2")).toBeVisible();
    expect(screen.getByText("Full rank history")).toBeVisible();
    expect(summary.getByText("Rank 3")).toBeVisible();
    expect(summary.getByText("Rank 1")).toBeVisible();
    expect(
      summary.getAllByRole("link").map((link) => link.getAttribute("href")),
    ).toEqual([previous.recordHref, latest.recordHref]);
  });

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
    expect(table).toBeVisible();
    expect(table.closest("details")).toHaveAttribute("open");
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
    render(
      <RecommendationTrend
        samples={[first, second]}
        focusNames={[item.name]}
      />,
    );
    screen
      .getAllByText("完整排名对比")
      .forEach((summary) => fireEvent.click(summary));
    expect(screen.getAllByRole("table")).toHaveLength(2);
    expect(screen.getAllByText("推荐结果可用 1 / 1 次")).toHaveLength(2);
    expect(
      screen.getAllByRole("region", { name: "关注对象最近变化" }),
    ).toHaveLength(2);
    expect(
      screen.queryByText(/排名上升|排名下降|排名持平/),
    ).not.toBeInTheDocument();
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
