import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import * as api from "../api/serp";
import { SerpMeasurementPanel } from "./SerpMeasurementPanel";
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
const protocol: api.SerpProtocol = {
  query: "rain gauge",
  engine: "google",
  source: "search-source",
  surface: "third_party_api",
  country: "US",
  city: null,
  language: "en",
  device: "desktop",
  requested_depth: 10,
};
const measurement: api.SerpMeasurement = {
  measurement_id: "measurement",
  source_key: "source-key",
  protocol,
  target: null,
  scheduled_at: "2026-10-01T00:00:00Z",
  created_at: "2026-10-01T00:00:00Z",
  state: "completed",
};
const observation: api.SerpObservation = {
  observation_id: "observation",
  measurement_id: "measurement",
  attempt_id: "attempt",
  raw_evidence_id: "evidence",
  raw_sha256: "hash",
  parser_version: "parser-v1",
  provider_observed_at: null,
  received_at: "2026-10-01T00:01:00Z",
  analyzed_at: "2026-10-01T00:02:00Z",
  status: "partial",
  actual_conditions: {},
  source_limitations: ["requested_conditions_unverified"],
  coverage: {
    requested_depth: 10,
    observed_organic_depth: 1,
    pages_received: 1,
    completion: "partial",
    truncated: false,
  },
  results: [
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
      locator: "/items/0",
    },
    {
      kind: "organic",
      raw_kind: "organic",
      raw_url: "https://example.org/source",
      normalized_url: "https://example.org/source",
      host: "example.org",
      title: "Organic result",
      page: 1,
      position: 2,
      organic_rank: 1,
      absolute_position: 2,
      locator: "/items/1",
    },
    {
      kind: "organic",
      raw_kind: "organic",
      raw_url: "https://example.org/unranked",
      normalized_url: "https://example.org/unranked",
      host: "example.org",
      title: "Unranked organic result",
      page: 1,
      position: 3,
      organic_rank: null,
      absolute_position: 3,
      locator: "/items/2",
    },
  ],
};
function setup(available = true, items: api.SerpMeasurement[] = [measurement]) {
  vi.spyOn(api, "getSerpCapabilities").mockResolvedValue(
    available
      ? [{ source_key: "source-key", protocol_defaults: protocol }]
      : [],
  );
  const list = vi
    .spyOn(api, "listSerpMeasurements")
    .mockResolvedValue({ items, next_after: null });
  const detail = vi.spyOn(api, "getSerpMeasurement").mockResolvedValue({
    measurement,
    observations: [observation],
    next_after: null,
    execution: null,
  });
  vi.spyOn(api, "listSerpSources").mockResolvedValue({
    items: [],
    next_after: null,
  });
  vi.spyOn(api, "getSerpRaw").mockResolvedValue({
    evidence: {
      evidence_id: "evidence",
      measurement_id: "measurement",
      attempt_id: "attempt",
      operation: "result",
      response_sha256: "hash",
      body_complete: true,
      http_status: 200,
      captured_at: "2026-10-01T00:01:00Z",
      body: [123, 125],
    },
    stored_at: "2026-10-01T00:01:00Z",
  });
  const create = vi
    .spyOn(api, "createSerpMeasurement")
    .mockResolvedValue(measurement);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <SerpMeasurementPanel tenantId="tenant" projectId="project" />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
  return { list, detail, create };
}
afterEach(async () => {
  cleanup();
  vi.restoreAllMocks();
  await i18n.changeLanguage("zh-CN");
});

describe("independent search measurement", () => {
  it("keeps history readable and new paid sampling unavailable without a source", async () => {
    const { create, detail } = setup(false);
    expect(
      await screen.findByText(/当前没有可用的搜索来源/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
    expect(
      await screen.findByRole("link", { name: "设置搜索数据源" }),
    ).toHaveAttribute("href", "/app/tenant/project/settings?tab=search");
    await userEvent.click(
      await screen.findByRole("button", { name: "rain gauge" }),
    );
    await waitFor(() =>
      expect(detail).toHaveBeenCalledWith(
        "tenant",
        "project",
        "measurement",
        undefined,
      ),
    );
    expect(create).not.toHaveBeenCalled();
  });

  it("separates organic ranks from advertisements and retains partial-coverage uncertainty", async () => {
    setup();
    await userEvent.click(
      await screen.findByRole("button", { name: "rain gauge" }),
    );
    expect(await screen.findByText(/自然排名: 1/)).toHaveTextContent(
      "页面位置: 2",
    );
    expect(screen.getByText(/不计自然排名/)).toHaveTextContent("广告");
    expect(screen.getByText(/自然排名未确认/)).toHaveTextContent("自然结果");
    expect(screen.getByText(/仅有部分结果/)).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Organic result" }),
    ).toHaveAttribute("href", "https://example.org/source");
    expect(
      screen.queryByRole("link", { name: "Ad result" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("/items/1")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "查看原始响应" }));
    expect(await screen.findByText("{}")).toBeInTheDocument();
  });

  it("retains the exact keyword, schedule and idempotency key after an uncertain submission", async () => {
    const { create } = setup(true, []);
    create
      .mockRejectedValueOnce(new Error("transport"))
      .mockResolvedValueOnce(measurement);
    await screen.findByText("还没有搜索测量记录。");
    await userEvent.type(
      screen.getByRole("textbox", { name: "关键词" }),
      "  rain gauge  ",
    );
    await userEvent.click(screen.getByRole("button", { name: "开始测量" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("提交尚未确认");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "开始测量" })).toBeEnabled(),
    );
    await userEvent.click(screen.getByRole("button", { name: "开始测量" }));
    await waitFor(() => expect(create).toHaveBeenCalledTimes(2));
    expect(create.mock.calls[0][2]).toEqual(create.mock.calls[1][2]);
    expect(create.mock.calls[0][2].query).toBe("  rain gauge  ");
    expect(create.mock.calls[0][2].idempotency_key).toBeTruthy();
    expect(create.mock.calls[0][2].scheduled_at).toBeTruthy();
  });

  it("accepts an optional domain without enterprise setup and never sends form-owned protocol fields", async () => {
    const { create } = setup(true, []);
    await screen.findByText("还没有搜索测量记录。");
    await userEvent.type(
      screen.getByRole("textbox", { name: "关键词" }),
      "rain gauge",
    );
    await userEvent.selectOptions(
      screen.getByRole("combobox", { name: "关注目标" }),
      "host",
    );
    await userEvent.type(
      screen.getByRole("textbox", { name: "目标域名或网页地址" }),
      "example.org",
    );
    await userEvent.click(screen.getByRole("button", { name: "开始测量" }));
    await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
    expect(create.mock.calls[0][2].target).toEqual({
      kind: "host",
      host: "example.org",
      include_subdomains: true,
    });
    expect(create.mock.calls[0][2]).not.toHaveProperty("protocol");
    expect(create.mock.calls[0][2]).not.toHaveProperty("cycle_id");
  });
});
