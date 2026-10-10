import { afterEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import * as api from "../api/measurementReports";
import { MeasurementPeriodReportPanel } from "./MeasurementPeriodReportPanel";
import i18n from "../i18n";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: {
      user: { id: "user" },
      operator: { id: "operator" },
      memberships: [{ tenant_id: "tenant", role: "member" }],
    },
  }),
}));
const preview: api.MeasurementPeriodPreview = {
  kind: "measurement_period_preview",
  project_id: "project",
  report_timezone: "UTC",
  report_window_start_at: "2026-10-01T00:00:00Z",
  report_window_end_at: "2026-10-08T00:00:00Z",
  evidence_as_of: "2026-10-08T00:00:00Z",
  generated_at: "2026-10-08T00:00:00Z",
  input_hash: "hash",
  coverage: {
    planned: 1,
    counts: { unknown: 1 },
    observed_live: 0,
    grounded_saved_analysis: 1,
  },
  samples: [
    {
      plan_id: "plan",
      target_id: "target",
      attempt_id: "attempt",
      comparison_key:
        "provider-a|model-a|consumer_web|web_search|v1|ad_hoc.v1|US|en",
      scheduled_at: "2026-10-02T00:00:00Z",
      original_status: "unknown",
      observed_live: false,
      observation: {
        raw_answer: "Saved response answer",
        citations: ["https://example.org/evidence", "javascript:alert(1)"],
        observed_at: "2026-10-02T00:00:00Z",
        received_at: "2026-10-02T00:01:00Z",
        provenance: {
          revision_id: "analysis-revision",
          source: { kind: "capture", capture_id: "capture" },
          source_sha256: "digest",
          observed_at: "2026-10-02T00:00:00Z",
          analyzed_at: "2026-10-03T00:00:00Z",
          actual_model: "analysis-model",
          config_revision: 1,
          parser_version: "parser",
          prompt_version: "prompt",
        },
      },
    },
  ],
};
const saved: api.MeasurementPeriodReport = {
  ...preview,
  kind: "measurement_period",
  report_id: "report",
  revision: 2,
  correction_of: "older-report",
};
function setup(input: api.MeasurementPeriodPreview = preview) {
  const load = vi
    .spyOn(api, "getMeasurementReportPreview")
    .mockResolvedValue(input);
  const list = vi
    .spyOn(api, "listMeasurementReports")
    .mockResolvedValue({ items: [] });
  const detail = vi.spyOn(api, "getMeasurementReport").mockResolvedValue(saved);
  const save = vi.spyOn(api, "saveMeasurementReport").mockResolvedValue(saved);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <MeasurementPeriodReportPanel tenantId="tenant" projectId="project" />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
  return { load, list, detail, save };
}
afterEach(async () => {
  cleanup();
  vi.restoreAllMocks();
  await i18n.changeLanguage("zh-CN");
});
describe("cycle-free measurement report", () => {
  it.each(["hit", "not_found_within_depth"] as const)(
    "shows the frozen %s target match for complete search evidence",
    async (matchStatus) => {
      const sample = searchFixture.samples[0];
      const evidence = sample.evidence!;
      const results = [1, 2, 3].map((rank) => ({
        ...evidence.observation.results[0],
        organic_rank: rank,
        position: rank,
        locator: `/items/${rank}`,
      }));
      setup({
        ...preview,
        search: {
          ...searchFixture,
          coverage: { planned: 1, counts: { observed: 1 } },
          samples: [
            {
              ...sample,
              cohort: {
                ...sample.cohort,
                protocol: { ...sample.cohort.protocol, requested_depth: 3 },
                target: {
                  kind: "host",
                  host:
                    matchStatus === "hit"
                      ? "example.org"
                      : "target.example.org",
                  include_subdomains: true,
                },
              },
              evidence: {
                ...evidence,
                target_match:
                  matchStatus === "hit"
                    ? { status: "hit", organic_ranks: [1, 2, 3] }
                    : { status: "not_found_within_depth", covered_depth: 3 },
                observation: {
                  ...evidence.observation,
                  status: "observed",
                  results,
                  coverage: {
                    ...evidence.observation.coverage,
                    requested_depth: 3,
                    completion: "requested_depth",
                    truncated: false,
                  },
                },
              },
            },
          ],
        },
      });
      const section = await screen.findByRole("region", { name: "传统搜索" });
      expect(
        within(section).getByText(
          matchStatus === "hit"
            ? "目标自然排名：1, 2, 3"
            : "在已覆盖的前 3 项自然结果中未找到目标。",
        ),
      ).toBeVisible();
      expect(
        within(section).queryByText(/仅有部分结果/),
      ).not.toBeInTheDocument();
    },
  );
  it.each([undefined, null])(
    "does not treat an omitted search section (%s) as zero searches",
    async (search) => {
      setup({ ...preview, search });
      const section = await screen.findByRole("region", { name: "传统搜索" });
      expect(
        within(section).getByText("此报告未纳入传统搜索数据。"),
      ).toBeVisible();
      expect(within(section).queryByText(/计划搜索/)).not.toBeInTheDocument();
      expect(
        within(section).queryByText(/暂无搜索样本/),
      ).not.toBeInTheDocument();
    },
  );
  it("distinguishes an included empty search cohort from omitted historical search", async () => {
    const { detail } = setup({
      ...preview,
      search: {
        schema_version: "v1",
        coverage: { planned: 0, counts: {} },
        samples: [],
      },
    });
    expect(await screen.findByText("这个时间范围暂无搜索样本。")).toBeVisible();
    expect(screen.getByText("计划搜索 0 项")).toBeVisible();
    await userEvent.click(
      screen.getByRole("button", { name: "保存此范围的报告" }),
    );
    await screen.findByText("报告版本 2");
    expect(detail).toHaveBeenCalled();
    expect(screen.getByText("此报告未纳入传统搜索数据。")).toBeVisible();
  });
  it("shows separate search counts, partial organic ranks, receipt basis and safe detail links", async () => {
    const { save } = setup({ ...preview, search: searchFixture });
    const section = await screen.findByRole("region", { name: "传统搜索" });
    expect(within(section).getByText("计划搜索 1 项")).toBeVisible();
    expect(screen.getByText(/计划测量 1 项/)).toHaveTextContent(
      "已核验联网回答 0 项",
    );
    expect(
      within(section).getByRole("heading", { name: "rain gauge" }),
    ).toBeVisible();
    expect(within(section).getByText("搜索来源：source-key")).toBeVisible();
    expect(
      within(section).getByText(/请求条件：google · US · en · desktop/),
    ).toBeVisible();
    expect(within(section).getByText(/结果接收时间：/)).toHaveTextContent(
      "供应方观测时间不可用",
    );
    expect(
      within(section).queryByText(/供应方观测时间：/),
    ).not.toBeInTheDocument();
    expect(
      within(section).getByText("仅有部分结果，不能据此判断目标未出现。"),
    ).toBeVisible();
    expect(
      within(section).getByText("自然排名未确认", { exact: false }),
    ).toBeVisible();
    expect(within(section).getByText(/自然排名 3/)).toBeVisible();
    expect(within(section).getByText(/不计自然排名/)).toBeVisible();
    expect(within(section).getByText("尚不能确认目标是否出现。")).toBeVisible();
    expect(within(section).queryByText(/未找到目标/)).not.toBeInTheDocument();
    expect(
      within(section).getByRole("link", { name: "Organic result" }),
    ).toHaveAttribute("href", "https://example.org/source");
    expect(
      within(section).queryByRole("link", { name: "Ad result" }),
    ).not.toBeInTheDocument();
    expect(
      within(section).getByRole("link", { name: "查看搜索记录" }),
    ).toHaveAttribute(
      "href",
      "/app/tenant/project/measurement?tab=search&searchRecord=search-id",
    );
    expect(
      within(section).getByText(/"protocol_version": "search-v1"/),
    ).not.toBeVisible();
    await userEvent.click(within(section).getByText("搜索条件与证据详情"));
    expect(
      within(section).getByText(/"protocol_version": "search-v1"/),
    ).toBeVisible();
    expect(save).not.toHaveBeenCalled();
  });
  it("preserves missing evidence without borrowing live task state or inventing ranks", async () => {
    setup({
      ...preview,
      search: {
        ...searchFixture,
        coverage: { planned: 1, counts: { no_eligible_observation: 1 } },
        samples: [{ ...searchFixture.samples[0], evidence: null }],
      },
    });
    const section = await screen.findByRole("region", { name: "传统搜索" });
    expect(
      within(section).getByText("截至报告时间暂无可用搜索证据", {
        exact: true,
      }),
    ).toBeVisible();
    expect(
      within(section).queryByText(/结果接收时间|供应方观测时间|自然排名/),
    ).not.toBeInTheDocument();
    expect(within(section).queryByText("已完成")).not.toBeInTheDocument();
  });
  it("localizes provider-time evidence without mixing it with receipt time", async () => {
    await i18n.changeLanguage("en");
    const sample = searchFixture.samples[0];
    setup({
      ...preview,
      search: {
        ...searchFixture,
        samples: [
          {
            ...sample,
            evidence: {
              ...sample.evidence!,
              evidence_time_basis: "provider_observed_at",
              evidence_time: "2026-10-02T00:00:00Z",
              observation: {
                ...sample.evidence!.observation,
                provider_observed_at: "2026-10-02T00:00:00Z",
              },
            },
          },
        ],
      },
    });
    const section = await screen.findByRole("region", {
      name: "Traditional search",
    });
    expect(
      within(section).getByText(/Provider observation time:/),
    ).toBeVisible();
    expect(
      within(section).queryByText(/Result received:/),
    ).not.toBeInTheDocument();
    expect(within(section).getByText(/Organic rank 3/)).toBeVisible();
  });
  it("shows a read-only preview with separate original outcome and saved analysis without a cycle", async () => {
    const { save, detail } = setup();
    expect(await screen.findByText("原测量结果：结果未知")).toBeInTheDocument();
    expect(screen.getByText("Saved response answer")).toBeInTheDocument();
    expect(screen.getByText("analysis-model")).toBeInTheDocument();
    expect(screen.getByText("analysis-revision")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "provider-a · model-a" }),
    ).toBeVisible();
    expect(
      screen.queryByRole("heading", {
        name: preview.samples[0].comparison_key,
      }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(preview.samples[0].comparison_key),
    ).not.toBeVisible();
    expect(screen.getByText("analysis-revision")).not.toBeVisible();
    expect(screen.getByText("digest")).not.toBeVisible();
    expect(screen.getByText("Saved response answer")).toBeVisible();
    expect(screen.getByText(/报告范围：/)).toHaveTextContent("UTC");
    await userEvent.click(screen.getByText("记录详情"));
    expect(screen.getByText("analysis-revision")).toBeVisible();
    expect(screen.getByText("digest")).toBeVisible();
    expect(screen.getByText(preview.samples[0].comparison_key)).toBeVisible();
    expect(screen.getByText(/已核验联网回答 0 项/)).toHaveTextContent(
      "已存原文分析 1 项（单独统计）",
    );
    expect(
      screen.getByRole("link", { name: "https://example.org/evidence" }),
    ).toHaveAttribute("href", "https://example.org/evidence");
    expect(
      screen.queryByRole("link", { name: "javascript:alert(1)" }),
    ).not.toBeInTheDocument();
    expect(save).not.toHaveBeenCalled();
    expect(detail).not.toHaveBeenCalled();
  });
  it("saves only on request and reads the immutable saved correction", async () => {
    const { save, detail } = setup();
    await userEvent.click(
      await screen.findByRole("button", { name: "保存此范围的报告" }),
    );
    await waitFor(() =>
      expect(save).toHaveBeenCalledWith("tenant", "project", preview),
    );
    expect(await screen.findByText("报告版本 2")).toBeInTheDocument();
    expect(screen.getByText("更正自报告 older-report")).toBeInTheDocument();
    expect(detail).toHaveBeenCalledWith("tenant", "project", "report");
    expect(
      screen.queryByRole("button", { name: "保存此范围的报告" }),
    ).not.toBeInTheDocument();
  });
  it("retains the attempted window after save uncertainty and a preview refresh", async () => {
    const { save, load } = setup();
    save.mockRejectedValueOnce(new Error("private upstream message"));
    await userEvent.click(
      await screen.findByRole("button", { name: "保存此范围的报告" }),
    );
    expect(await screen.findByText(/保存尚未确认/)).toBeInTheDocument();
    load.mockResolvedValue({
      ...preview,
      report_window_end_at: "2026-10-09T00:00:00Z",
    });
    await userEvent.click(screen.getByRole("button", { name: "刷新测量报告" }));
    await waitFor(() => expect(load).toHaveBeenCalledTimes(2));
    await userEvent.click(
      screen.getByRole("button", { name: "保存此范围的报告" }),
    );
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(save.mock.calls[0]).toEqual(save.mock.calls[1]);
    expect(
      screen.queryByText("private upstream message"),
    ).not.toBeInTheDocument();
  });
  it("localizes the preview without hiding unknown outcomes", async () => {
    await i18n.changeLanguage("en");
    setup();
    expect(
      await screen.findByText("Original measurement: Unknown"),
    ).toBeInTheDocument();
    expect(screen.getByText("Last seven days · preview")).toBeInTheDocument();
  });
});

const searchFixture: api.MeasurementPeriodSearchSection = {
  schema_version: "v1",
  coverage: { planned: 1, counts: { partial: 1 } },
  samples: [
    {
      cohort: {
        measurement_id: "search-id",
        source_key: "source-key",
        query: "rain gauge",
        protocol: {
          query: "rain gauge",
          engine: "google",
          source: "search-source",
          surface: "third_party_api",
          country: "US",
          city: null,
          language: "en",
          device: "desktop",
          requested_depth: 10,
          protocol_version: "search-v1",
        },
        target: {
          kind: "host",
          host: "target.example.org",
          include_subdomains: true,
        },
        target_rule_version: "v1",
        question_binding: null,
        scheduled_at: "2026-10-02T00:00:00Z",
        created_at: "2026-10-01T00:00:00Z",
        stored_at: "2026-10-01T00:00:01Z",
      },
      evidence: {
        raw_stored_at: "2026-10-02T00:01:01Z",
        observation_stored_at: "2026-10-02T00:02:01Z",
        evidence_time: "2026-10-02T00:01:00Z",
        evidence_time_basis: "received_at",
        target_match: { status: "undetermined" },
        observation: {
          observation_id: "observation",
          measurement_id: "search-id",
          attempt_id: "attempt",
          raw_evidence_id: "evidence",
          raw_sha256: "hash",
          parser_version: "parser",
          provider_observed_at: null,
          received_at: "2026-10-02T00:01:00Z",
          analyzed_at: "2026-10-02T00:02:00Z",
          status: "partial",
          actual_conditions: {},
          coverage: {
            requested_depth: 10,
            observed_organic_depth: 3,
            pages_received: 1,
            completion: "partial",
            truncated: true,
          },
          source_limitations: ["requested_conditions_unverified"],
          results: [
            {
              kind: "organic",
              raw_kind: "organic",
              raw_url: "https://example.org/source",
              normalized_url: "https://example.org/source",
              host: "example.org",
              title: "Organic result",
              page: 1,
              position: 4,
              organic_rank: 3,
              absolute_position: 4,
              locator: "/items/0",
            },
            {
              kind: "advertisement",
              raw_kind: "paid",
              raw_url: "javascript:alert(1)",
              normalized_url: null,
              host: null,
              title: "Ad result",
              page: 1,
              position: 1,
              organic_rank: null,
              absolute_position: 1,
              locator: "/items/1",
            },
            {
              kind: "organic",
              raw_kind: "organic",
              raw_url: null,
              normalized_url: null,
              host: null,
              title: "Unranked",
              page: 1,
              position: 5,
              organic_rank: null,
              absolute_position: 5,
              locator: "/items/2",
            },
          ],
        },
      },
    },
  ],
};
