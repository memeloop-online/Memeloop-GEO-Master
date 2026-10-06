import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import {
  getReport,
  getReportEvidence,
  getReportPreview,
  listReports,
  type ReportPreview,
  type ReportSnapshot,
} from "../api/reports";
import { reportSnapshotCsv } from "./reportsCsv";

const session = {
  user: { id: "user-1", login_name: "user@example.test", display_name: "User" },
  operator: { id: "operator-1", slug: "operator", display_name: "Operator" },
  memberships: [
    {
      tenant_id: "tenant-1",
      tenant_slug: "tenant",
      tenant_display_name: "Tenant",
      role: "viewer",
    },
  ],
  expires_at: "2026-10-01T00:00:00Z",
  csrf_token: "csrf-test",
};

const unavailable = {
  availability: "unavailable" as const,
  expected_count: null,
  observed_count: 0,
  counts: {},
  reason: "尚无封存输入",
};

const snapshot: ReportSnapshot = {
  report_id: "report-1",
  project_id: "project-1",
  cycle_id: "cycle-1",
  revision: 2,
  correction_of: "report-original",
  report_window_start_at: "2026-09-21T00:00:00Z",
  report_window_end_at: "2026-09-28T00:00:00Z",
  report_timezone: "Asia/Shanghai",
  cutoff_at: "2026-09-28T12:00:00Z",
  generated_at: "2026-09-28T12:01:00Z",
  reducer_version: "reduce-v1",
  input_hash: "input-hash-1",
  evidence_as_of: "2026-09-28T12:00:00Z",
  status: "partial",
  input_manifest_versions: [
    {
      kind: "document",
      manifest_id: "manifest-1",
      revision: 1,
      sealed: true,
      expected_count: 2,
    },
  ],
  documents: {
    availability: "available",
    expected_count: 2,
    observed_count: 2,
    counts: { planned: 1, blocked: 1 },
    reason: null,
  },
  publications: unavailable,
  publication_groups: [],
  measurements: unavailable,
  measurement_groups: [],
  findings: [
    {
      finding_id: "finding-1",
      kind: "coverage_gap",
      summary: "一个分支仍需资料",
      evidence_ids: ["evidence-1"],
      insufficient_reason: null,
    },
  ],
  evidence: [
    {
      evidence_id: "evidence-1",
      kind: "document_manifest_item",
      resource_id: "item-1",
      resource_version: "1",
      occurred_at: "2026-09-26T08:00:00Z",
      received_at: "2026-09-26T08:01:00Z",
      summary: "资料不足的文档分支",
    },
  ],
};

const {
  report_id: _reportId,
  revision: _revision,
  correction_of: _correctionOf,
  ...projection
} = snapshot;
const reportPreview: ReportPreview = {
  ...projection,
  kind: "preview",
  generated_at: "2026-09-27T12:01:00Z",
  evidence_as_of: "2026-09-27T12:00:00Z",
};

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function mockApi({
  reports = [snapshot],
  detail = snapshot,
  listStatus = 200,
  detailStatus = 200,
  evidenceStatus = 200,
  cycleStatus = 200,
  previewStatus = 200,
  preview = reportPreview,
  previewWait,
}: {
  reports?: ReportSnapshot[];
  detail?: ReportSnapshot;
  listStatus?: number;
  detailStatus?: number;
  evidenceStatus?: number;
  cycleStatus?: number;
  previewStatus?: number;
  preview?: ReportPreview;
  previewWait?: Promise<void>;
} = {}) {
  const fetchMock = vi.fn((request: RequestInfo | URL) => {
    const url = new URL(String(request), "http://localhost");
    if (url.pathname.endsWith("/auth/session"))
      return Promise.resolve(response(session));
    if (url.pathname.endsWith("/projects"))
      return Promise.resolve(response({ items: [], next_cursor: null }));
    if (url.pathname.endsWith("/projects/project-1/cycles/current"))
      return Promise.resolve(
        response(
          cycleStatus === 200
            ? {
                project_id: "project-1",
                cycle_id: "cycle-1",
                report_timezone: "Asia/Shanghai",
                report_window_start_at: snapshot.report_window_start_at,
                report_window_end_at: snapshot.report_window_end_at,
                cutoff_at: snapshot.cutoff_at,
              }
            : { message: "当前没有周期" },
          cycleStatus,
        ),
      );
    if (url.pathname.endsWith("/cycles/cycle-1/report-preview"))
      return (previewWait ?? Promise.resolve()).then(() =>
        response(
          previewStatus === 200 ? preview : { message: "预览暂不可用" },
          previewStatus,
        ),
      );
    if (url.pathname.endsWith("/projects/project-1/reports"))
      return Promise.resolve(
        response(
          listStatus === 200 ? { items: reports } : { message: "无权读取列表" },
          listStatus,
        ),
      );
    if (url.pathname.endsWith("/reports/report-1/evidence"))
      return Promise.resolve(
        response(
          evidenceStatus === 200
            ? { items: detail.evidence }
            : { message: "证据暂不可用" },
          evidenceStatus,
        ),
      );
    if (url.pathname.endsWith("/reports/report-1"))
      return Promise.resolve(
        response(
          detailStatus === 200 ? detail : { message: "无权读取报告" },
          detailStatus,
        ),
      );
    return Promise.resolve(response({ message: "not found" }, 404));
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function renderPage(path = "/app/tenant-1/project-1/reports") {
  return render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider
        client={
          new QueryClient({
            defaultOptions: { queries: { retry: false } },
          })
        }
      >
        <AuthProvider>
          <MemoryRouter initialEntries={[path]}>
            <AppRoutes />
          </MemoryRouter>
        </AuthProvider>
      </QueryClientProvider>
    </FluentProvider>,
  );
}

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe("P14 immutable reports", () => {
  it("renders a read-only preview alongside an empty saved list with distinct times, full denominator and inline evidence", async () => {
    const fetchMock = mockApi({
      reports: [],
      preview: {
        ...reportPreview,
        measurements: {
          availability: "available",
          expected_count: 4,
          observed_count: 2,
          counts: { missing: 1, not_mentioned: 1, pending: 2 },
          reason: null,
        },
        measurement_groups: [
          {
            comparison_key: "frozen-protocol",
            coverage: {
              availability: "available",
              expected_count: 4,
              observed_count: 2,
              counts: { missing: 1, not_mentioned: 1, pending: 2 },
              reason: null,
            },
          },
        ],
      },
    });
    const user = userEvent.setup();
    renderPage();
    const previewSection = await screen.findByRole("region", {
      name: "临时报告预览",
    });
    expect(
      within(previewSection).getByText(/临时预览 · 未保存为正式周报/),
    ).toBeInTheDocument();
    expect(
      within(previewSection).getByText("资料不足的文档分支"),
    ).toBeInTheDocument();
    expect(within(previewSection).getByText("生成时间")).toBeInTheDocument();
    expect(within(previewSection).getByText("证据水位")).toBeInTheDocument();
    expect(within(previewSection).getByText("冻结截止")).toBeInTheDocument();
    expect(
      within(previewSection).getByText("2026/09/27 20:01"),
    ).toBeInTheDocument();
    expect(
      within(previewSection).getByText("2026/09/27 20:00"),
    ).toBeInTheDocument();
    expect(
      within(previewSection).getByText("2026/09/28 20:00"),
    ).toBeInTheDocument();
    const measurement = within(previewSection)
      .getByRole("heading", { name: "AI 渠道测量覆盖" })
      .closest(".report-panel")!;
    expect(
      within(measurement as HTMLElement).getByText(/计划分母 4/),
    ).toBeInTheDocument();
    expect(
      within(measurement as HTMLElement).getByText("缺测"),
    ).toBeInTheDocument();
    expect(
      within(measurement as HTMLElement).getByText("未提及"),
    ).toBeInTheDocument();
    expect(
      within(previewSection).getByRole("link", { name: /查看预览证据/ }),
    ).toHaveAttribute("href", "#evidence-evidence-1");
    expect(
      within(previewSection).queryByText("报告 ID"),
    ).not.toBeInTheDocument();
    expect(within(previewSection).queryByText("修订")).not.toBeInTheDocument();
    expect(
      within(previewSection).queryByRole("button", { name: /CSV/ }),
    ).not.toBeInTheDocument();
    expect(await screen.findByText("尚无周报快照")).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("/cycles/cycle-1/report-preview"),
      ),
    ).toHaveLength(1);
    expect(
      fetchMock.mock.calls.some(([request]) =>
        String(request).includes("/reports/report-1/evidence"),
      ),
    ).toBe(false);
    await user.click(
      within(previewSection).getByRole("button", { name: "刷新预览" }),
    );
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("/cycles/cycle-1/report-preview"),
      ),
    ).toHaveLength(2);
  });

  it("reports preview loading/errors/no active cycle without hiding saved snapshots", async () => {
    let finishPreview!: () => void;
    mockApi({
      previewWait: new Promise<void>((resolve) => {
        finishPreview = resolve;
      }),
    });
    const loadingPage = renderPage();
    expect(
      await screen.findByLabelText("正在生成临时报告预览"),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("link", { name: /2026.*2026/ }),
    ).toBeInTheDocument();
    finishPreview();
    loadingPage.unmount();
    vi.unstubAllGlobals();
    mockApi({ cycleStatus: 404 });
    const page = renderPage();
    expect(await screen.findByText("暂无活动周期")).toBeInTheDocument();
    expect(
      screen.queryByRole("region", { name: "临时报告预览" }),
    ).not.toBeInTheDocument();
    expect(
      await screen.findByRole("link", { name: /2026.*2026/ }),
    ).toBeInTheDocument();
    page.unmount();
    vi.unstubAllGlobals();
    mockApi({ previewStatus: 503 });
    renderPage();
    expect(await screen.findByText("无法加载临时预览")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
    expect(
      await screen.findByRole("link", { name: /2026.*2026/ }),
    ).toBeInTheDocument();
  });

  it("lists saved snapshots, follows detail, and preserves correction lineage", async () => {
    const fetchMock = mockApi();
    const user = userEvent.setup();
    renderPage();
    expect(await screen.findByText(/显式更正版本，原报告/)).toBeInTheDocument();
    await user.click(screen.getByRole("link", { name: /2026.*2026/ }));
    expect(await screen.findByText("report-original")).toBeInTheDocument();
    expect(screen.getByText(/后续证据不会静默改写本版本/)).toBeInTheDocument();
    const reportCalls = fetchMock.mock.calls.filter(([request]) =>
      String(request).includes("/reports"),
    );
    expect(reportCalls.length).toBeGreaterThanOrEqual(3);
    reportCalls.forEach(([request]) => {
      expect(String(request)).toContain("tenant_id=tenant-1");
      expect(String(request)).toContain("project_id=project-1");
    });
    expect(
      reportCalls.some(([request]) =>
        String(request).includes("/reports/report-1/evidence"),
      ),
    ).toBe(true);
  });

  it("shows partial missing inputs explicitly, never fabricating a metric or pooled trend", async () => {
    mockApi();
    renderPage("/app/tenant-1/project-1/reports/report-1");
    expect(await screen.findByText("一个分支仍需资料")).toBeInTheDocument();
    const coverage = screen.getByRole("region", { name: "三类独立覆盖" });
    expect(within(coverage).getAllByText(/尚无封存输入/)).toHaveLength(2);
    expect(within(coverage).getAllByText(/计划分母 未知/)).toHaveLength(2);
    expect(screen.getByText(/不能推断提及、引用或趋势/)).toBeInTheDocument();
    expect(screen.queryByText(/GEO 总分/)).not.toBeInTheDocument();
    const link = screen.getByRole("link", { name: /查看快照证据/ });
    expect(link).toHaveAttribute("href", "#evidence-evidence-1");
    expect(await screen.findByText("资料不足的文档分支")).toBeInTheDocument();
    expect(screen.getByText("item-1")).toBeInTheDocument();
  });

  it("shows an honest empty state and scoped permission errors", async () => {
    mockApi({ reports: [] });
    const page = renderPage();
    expect(await screen.findByText("尚无周报快照")).toBeInTheDocument();
    page.unmount();
    vi.unstubAllGlobals();
    mockApi({ listStatus: 403 });
    renderPage();
    expect(await screen.findByText("权限不足")).toBeInTheDocument();
    expect(screen.queryByText("尚无周报快照")).not.toBeInTheDocument();
  });

  it("keeps report coverage readable when the separate evidence request fails", async () => {
    mockApi({ evidenceStatus: 503 });
    renderPage("/app/tenant-1/project-1/reports/report-1");
    expect(await screen.findByText("无法读取证据明细")).toBeInTheDocument();
    expect(screen.getByText("文档覆盖与资料缺口")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试" })).toBeInTheDocument();
  });

  it("keeps platform and AI comparison groups separate, including unknown and missing states", async () => {
    mockApi({
      detail: {
        ...snapshot,
        publication_groups: [
          {
            platform_id: "platform-one",
            coverage: {
              availability: "available",
              expected_count: null,
              observed_count: 1,
              counts: { unknown: 1 },
              reason: "逐平台计划分母未提供",
            },
          },
        ],
        measurement_groups: [
          {
            comparison_key: "protocol-one",
            coverage: {
              availability: "available",
              expected_count: null,
              observed_count: 1,
              counts: { not_mentioned: 1 },
              reason: "逐口径计划分母未提供",
            },
          },
          {
            comparison_key: "protocol-two",
            coverage: {
              availability: "available",
              expected_count: null,
              observed_count: 1,
              counts: { missing: 1 },
              reason: "逐口径计划分母未提供",
            },
          },
        ],
      },
    });
    renderPage("/app/tenant-1/project-1/reports/report-1");
    expect(await screen.findByText("platform-one")).toBeInTheDocument();
    const groups = screen.getByRole("region", { name: "平台与测量比较组" });
    expect(within(groups).getByText("protocol-one")).toBeInTheDocument();
    expect(within(groups).getByText("protocol-two")).toBeInTheDocument();
    expect(within(groups).getByText("结果未知 1")).toBeInTheDocument();
    expect(within(groups).getByText("未提及 1")).toBeInTheDocument();
    expect(within(groups).getByText("缺测 1")).toBeInTheDocument();
    expect(within(groups).getAllByText("逐口径计划分母未提供")).toHaveLength(2);
    expect(screen.queryByText(/%/)).not.toBeInTheDocument();
  });

  it("renders independent lookup findings without relabeling unknown publication or leaking private evidence to CSV", async () => {
    const asset = {
      evidence_id: "asset-evidence-1",
      kind: "publication_lookup_asset_observed",
      resource_id: "frozen-target-1",
      resource_version: null,
      occurred_at: "2026-09-28T08:00:00Z",
      received_at: "2026-09-28T08:01:00Z",
      summary:
        "Independent lookup observed a public asset; original send remains unproven",
    };
    const report: ReportSnapshot = {
      ...snapshot,
      publications: {
        availability: "available",
        expected_count: 1,
        observed_count: 1,
        counts: { unknown: 1 },
        reason: null,
      },
      findings: [
        {
          finding_id: "lookup-finding",
          kind: "publication_asset_observed",
          summary:
            "A public asset was observed independently; this does not prove the original publication send succeeded.",
          evidence_ids: [asset.evidence_id],
          insufficient_reason:
            "No trusted causal link between the original send and the observed asset is available.",
        },
      ],
      evidence: [asset],
    };
    mockApi({ detail: report });
    renderPage("/app/tenant-1/project-1/reports/report-1");
    expect(await screen.findByText(asset.summary)).toBeInTheDocument();
    expect(
      screen.getByText(
        /this does not prove the original publication send succeeded/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/查回发现资产不证明原发送成功/),
    ).toBeInTheDocument();
    expect(screen.getByText("结果未知")).toBeInTheDocument();
    const csv = reportSnapshotCsv(report);
    expect(csv).toContain('"publication_asset_observed"');
    expect(csv).toContain('"publication_lookup_asset_observed"');
    expect(csv).toContain('"publication","","unknown","1",""');
    expect(csv).not.toContain("public_url");
    expect(csv).not.toContain("account_id");
    expect(csv).not.toContain("raw_evidence");
  });

  it("formats report and evidence timestamps in the frozen project timezone, not browser UTC", async () => {
    vi.stubEnv("TZ", "UTC");
    mockApi();
    renderPage("/app/tenant-1/project-1/reports/report-1");
    expect(
      await screen.findByRole("heading", {
        name: "周报 · 2026/09/21 08:00",
      }),
    ).toBeInTheDocument();
    expect(screen.getByText(/截止 2026\/09\/28 20:00/)).toBeInTheDocument();
    expect(await screen.findByText("2026/09/26 16:00")).toBeInTheDocument();
    expect(screen.getByText("2026/09/26 16:01")).toBeInTheDocument();
  });

  it("labels UTC fallback when a stored timezone is invalid", async () => {
    mockApi({ detail: { ...snapshot, report_timezone: "Unknown/Invalid" } });
    renderPage("/app/tenant-1/project-1/reports/report-1");
    expect(
      await screen.findByRole("heading", {
        name: /2026\/09\/21 00:00 UTC（报告时区无效）/,
      }),
    ).toBeInTheDocument();
  });

  it("exports only persisted snapshot coverage and evidence with formula-safe CSV cells", async () => {
    const csv = reportSnapshotCsv({
      ...snapshot,
      publication_groups: [
        {
          platform_id: "=1+1",
          coverage: {
            availability: "available",
            expected_count: null,
            observed_count: 1,
            counts: { unknown: 1 },
            reason: null,
          },
        },
      ],
      evidence: [
        {
          ...snapshot.evidence[0],
          summary: ' \t=HYPERLINK("example","click")',
        },
      ],
    });
    expect(csv.startsWith("\uFEFF")).toBe(true);
    expect(csv).toContain('"publication_group","\'=1+1","unknown","1",""');
    expect(csv).toContain('"\u0027 \t=HYPERLINK(""example"",""click"")"');
    expect(csv).toContain('"publication","","expected_count","",""');
    expect(csv).not.toContain("GEO score");

    mockApi();
    const createObjectURL = vi.fn(() => "blob:report");
    const revokeObjectURL = vi.fn();
    class DownloadURL extends URL {
      static createObjectURL = createObjectURL;
      static revokeObjectURL = revokeObjectURL;
    }
    vi.stubGlobal("URL", DownloadURL);
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    const user = userEvent.setup();
    renderPage("/app/tenant-1/project-1/reports/report-1");
    await user.click(
      await screen.findByRole("button", { name: "下载覆盖与证据 CSV" }),
    );
    expect(click).toHaveBeenCalledTimes(1);
    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:report");
    expect(document.querySelector('a[download^="report-"]')).toBeNull();
  });

  it("scopes direct list/detail/evidence reads and encodes resource IDs", async () => {
    const fetchMock = vi.fn((_request: RequestInfo | URL) =>
      Promise.resolve(response({ items: [] })),
    );
    vi.stubGlobal("fetch", fetchMock);
    await listReports("tenant-other", "project-other");
    await getReport("tenant-other", "project-other", "report/other");
    await getReportEvidence("tenant-other", "project-other", "report/other");
    await getReportPreview("tenant-other", "project-other", "cycle/other");
    expect(String(fetchMock.mock.calls[0][0])).toContain(
      "/projects/project-other/reports?tenant_id=tenant-other&project_id=project-other",
    );
    expect(String(fetchMock.mock.calls[1][0])).toContain(
      "/reports/report%2Fother?tenant_id=tenant-other&project_id=project-other",
    );
    expect(String(fetchMock.mock.calls[2][0])).toContain(
      "/reports/report%2Fother/evidence?tenant_id=tenant-other&project_id=project-other",
    );
    expect(String(fetchMock.mock.calls[3][0])).toContain(
      "/cycles/cycle%2Fother/report-preview?tenant_id=tenant-other&project_id=project-other",
    );
  });
});
