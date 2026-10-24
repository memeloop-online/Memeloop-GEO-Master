import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import type { MeasurementTarget } from "../api/projects";
import type { RecommendationItem } from "../api/recommendations";
import type { RecommendationTrendSample } from "./RecommendationTrend";
import { RecommendationPlatformComparison } from "./RecommendationPlatformComparison";
import i18n from "../i18n";

const target: MeasurementTarget = {
  name: "Example lamp",
  kind: "product",
  aliases: ["Lamp alias"],
};
const item: RecommendationItem = {
  name: target.name,
  kind: "product",
  mention_order: 1,
  rank: 1,
  reason: "Portable",
  evidence_quote: "A portable lamp.",
  citation_urls: [],
};
const recommendations = (items: RecommendationItem[], status = "verified") => ({
  version: "geo.recommendations.v1",
  status,
  items,
});
const planWindow = {
  start: "2026-10-01T00:00:00Z",
  end: "2026-10-08T00:00:00Z",
};

function sample(
  id: string,
  items: RecommendationItem[] = [item],
): RecommendationTrendSample {
  return {
    sampleId: id,
    recordHref: `/app/tenant/project/measurement?tab=records&record=plan-${id}&target=target-${id}&attempt=attempt-${id}&analysisRevision=analysis-${id}`,
    observedAt: "2026-10-03T10:00:00Z",
    analysisRevisionId: `analysis-${id}`,
    measurement: {
      provider: "provider-a",
      model: "model-a",
      surface: "consumer_web",
      search_mode: "web_search",
      protocol_version: "v1",
      question_set_version: "temporary.v1",
      question: "Which lamp suits camping?",
      market: "US",
      language: "en",
    },
    recommendations: recommendations(items),
  };
}
function setup(
  samples: RecommendationTrendSample[],
  targets: MeasurementTarget[] = [target],
) {
  return render(
    <FluentProvider theme={webLightTheme}>
      <RecommendationPlatformComparison
        samples={samples}
        focusTargets={targets}
        planWindow={planWindow}
        timezone="UTC"
      />
    </FluentProvider>,
  );
}
function table() {
  return screen.getByRole("table");
}
function row(provider: string) {
  return within(table())
    .getAllByRole("row")
    .find((element) =>
      within(element).queryByRole("rowheader", { name: new RegExp(provider) }),
    )!;
}
beforeEach(async () => {
  await i18n.changeLanguage("en");
});
afterEach(async () => {
  cleanup();
  await i18n.changeLanguage("zh-CN");
});

describe("platform recommendation comparison", () => {
  it.each(["en", "zh-CN"])(
    "keeps independent platform denominators and conditional ranks in %s",
    async (language) => {
      await i18n.changeLanguage(language);
      const first = sample("first", [
        item,
        { ...item, name: " Lamp alias ", mention_order: 2 },
      ]);
      const ranked = sample("ranked", [
        { ...item, name: "Lamp alias", rank: 3 },
      ]);
      const unranked = sample("unranked", [{ ...item, rank: null }]);
      const conflict = sample("conflict", [
        item,
        { ...item, name: "Lamp alias", mention_order: 2, rank: 2 },
      ]);
      const absent = sample("absent", []);
      const otherKind = sample("other-kind", [{ ...item, kind: "brand" }]);
      const missing = { ...sample("missing"), recommendations: undefined };
      const unverified = {
        ...sample("unverified"),
        recommendations: recommendations([item], "unverified"),
      };
      const other = sample("other-platform", []);
      other.measurement = { ...other.measurement, provider: "provider-b" };
      setup([
        first,
        ranked,
        unranked,
        conflict,
        absent,
        otherKind,
        missing,
        unverified,
        other,
      ]);
      expect(within(table()).getAllByRole("row")).toHaveLength(3);
      const a = within(row("provider-a"));
      expect(a.getByText(/4 \/ 6/)).toHaveTextContent("66.7%");
      expect(a.getByText(/1 \/ 2/)).toHaveTextContent("50%");
      expect(
        a.getByText(
          language === "en"
            ? "Valid: 6 · Unavailable: 2"
            : "有效 6 次 · 不可用 2 次",
        ),
      ).toBeVisible();
      expect(
        a.getByText(
          language === "en" ? "Unranked samples: 1" : "未明确排名：1 次",
        ),
      ).toBeVisible();
      expect(
        a.getByText(
          language === "en"
            ? "Conflicting rank samples: 1"
            : "排名不一致：1 次",
        ),
      ).toBeVisible();
      expect(within(row("provider-b")).getByText(/0 \/ 1/)).toHaveTextContent(
        "0%",
      );
      expect(
        within(row("provider-b")).getByText(
          language === "en"
            ? "Not applicable · 0 ranked recommendation samples"
            : "不适用 · 明确排名推荐 0 次",
        ),
      ).toBeVisible();
    },
  );

  it.each([
    "model",
    "surface",
    "search_mode",
    "protocol_version",
    "market",
    "language",
  ] as const)("keeps a different %s in its own row", (condition) => {
    const other = sample("other", []);
    other.measurement = {
      ...other.measurement,
      [condition]: "different-value",
    };
    setup([sample("base"), other]);
    const rows = within(table()).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(2);
    expect(within(rows[0]).getAllByText(/1 \/ 1/)).toHaveLength(2);
    expect(within(rows[1]).getByText(/0 \/ 1/)).toBeVisible();
    expect(within(rows[1]).getByText("different-value")).toBeVisible();
  });

  it("partitions exact question references, revisions, versions and temporary questions", () => {
    const bound = sample("bound");
    const reference = {
      question_set_id: "set",
      question_set_version_id: "set-v1",
      question_id: "question",
      question_revision_id: "question-v1",
    };
    bound.measurement = {
      ...bound.measurement,
      question_binding: {
        reference,
        purpose: "optimization",
        split_policy_version: "v1",
      },
    };
    const variants = (
      [
        "question_set_id",
        "question_set_version_id",
        "question_id",
        "question_revision_id",
      ] as const
    ).map((field) => ({
      ...sample(field),
      measurement: {
        ...bound.measurement,
        question_binding: {
          ...bound.measurement.question_binding!,
          reference: { ...reference, [field]: "different" },
        },
      },
    }));
    const version = {
      ...sample("version"),
      measurement: { ...bound.measurement, question_set_version: "v2" },
    };
    const text = {
      ...sample("text"),
      measurement: {
        ...bound.measurement,
        question: bound.measurement.question + " ",
      },
    };
    setup([bound, ...variants, version, text, sample("temporary")]);
    expect(screen.getAllByRole("table")).toHaveLength(8);
    for (const group of screen.getAllByRole("table"))
      expect(within(group).getAllByRole("row")).toHaveLength(2);
  });

  it("uses only actual observation times, counts unknowns, and keeps every exact evidence href", async () => {
    const a = sample("a");
    a.observedAt = "2026-10-11T10:00:00Z";
    a.measurement.scheduled_at = "2026-10-02T00:00:00Z";
    const b = sample("b");
    b.observedAt = null;
    b.measurement.scheduled_at = "2026-10-04T00:00:00Z";
    const c = { ...sample("c"), observedAt: "invalid-date" };
    setup([a, b, c]);
    const data = within(table());
    const actual = new Intl.DateTimeFormat("en", {
      dateStyle: "medium",
      timeStyle: "short",
      timeZone: "UTC",
    }).format(new Date(a.observedAt));
    expect(data.getByText(`${actual} to ${actual}`)).toBeVisible();
    expect(data.getByText("Samples without a recorded time: 2")).toBeVisible();
    await userEvent.click(data.getByText("View 3 samples"));
    expect(
      data.getAllByRole("link").map((link) => link.getAttribute("href")),
    ).toEqual([a.recordHref, b.recordHref, c.recordHref]);
    expect(
      data
        .getAllByRole("link")
        .filter((link) =>
          link.textContent?.includes("Observation time not recorded"),
        ),
    ).toHaveLength(2);
    expect(table()).not.toHaveTextContent("Oct 2");
    expect(table()).not.toHaveTextContent("Oct 4");
    expect(screen.getByText(/Plan sampling window:/)).toHaveTextContent(
      "start included, end excluded",
    );
  });

  it("switches the current viewing target without modifying evidence or issuing requests", async () => {
    const requests: string[] = [];
    const originalFetch = globalThis.fetch;
    globalThis.fetch = ((input: RequestInfo | URL) => {
      requests.push(String(input));
      throw new Error("Viewing selection must not fetch");
    }) as typeof fetch;
    try {
      const targets: MeasurementTarget[] = [
        target,
        { name: "Example tent", kind: "product", aliases: [] },
      ];
      setup([sample("a")], targets);
      const selector = screen.getByRole("combobox", {
        name: "View tracked item",
      });
      expect(within(table()).getAllByText(/1 \/ 1/)).toHaveLength(2);
      await userEvent.tab();
      expect(selector).toHaveFocus();
      await userEvent.selectOptions(
        selector,
        JSON.stringify(["product", "Example tent"]),
      );
      expect(within(table()).getByText(/0 \/ 1/)).toBeVisible();
      expect(
        within(table()).getByText(
          "Not applicable · 0 ranked recommendation samples",
        ),
      ).toBeVisible();
      expect(requests).toEqual([]);
      expect(screen.getByText(/Matches your current names/)).toBeVisible();
      expect(
        screen.getByRole("region", {
          name: /Platform comparison for question group/,
        }),
      ).toHaveAttribute("tabindex", "0");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("shows unavailable and empty states without zero denominators", () => {
    setup([{ ...sample("unavailable"), recommendations: undefined }]);
    expect(
      within(table()).getByText("No valid recommendation results"),
    ).toBeVisible();
    expect(
      within(table()).getByText("Valid: 0 · Unavailable: 1"),
    ).toBeVisible();
    expect(table()).not.toHaveTextContent(/0\s*\/\s*0|NaN|Infinity/);
    expect(table()).not.toHaveTextContent("0%");
  });

  it("offers a clear next step when no target is configured", () => {
    setup([sample("a")], []);
    expect(
      screen.getByText(
        "Set a tracked item to compare platform recommendations.",
      ),
    ).toBeVisible();
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("shows no fabricated results when the report has no samples", () => {
    setup([]);
    expect(
      screen.getByText("No measurement samples in this range yet."),
    ).toBeVisible();
    expect(screen.queryByRole("table")).toBeNull();
  });
});
