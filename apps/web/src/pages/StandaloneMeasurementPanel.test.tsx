import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  Link,
  MemoryRouter,
  useLocation,
  useNavigate,
  useSearchParams,
} from "react-router-dom";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { StandaloneMeasurementPanel } from "./StandaloneMeasurementPanel";
import {
  createMeasurementPlan,
  executeChannelTarget,
  getChannelTarget,
  getMeasurementPlan,
  getMeasurementOptions,
  listMeasurementPlans,
} from "../api/channelJobs";
import {
  getQuestionSetVersion,
  listAllQuestionSets,
  listAllQuestionSetVersions,
  type QuestionSetVersion,
} from "../api/questions";
import {
  getMeasurementReport,
  getMeasurementReportPreview,
  listMeasurementReports,
} from "../api/measurementReports";

const state = vi.hoisted(() => ({
  accounts: [] as unknown[],
  platforms: [] as unknown[],
  refetch: vi.fn(),
  accountsError: false,
}));
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: { user: { id: "user" }, operator: { id: "operator" } },
  }),
}));
vi.mock("../api/channels", () => ({
  useChannelData: () => ({
    platforms: {
      data: { items: state.platforms },
      isPending: false,
      isError: false,
      refetch: vi.fn(),
    },
    accounts: {
      data: { items: state.accounts },
      isPending: false,
      isError: state.accountsError,
      error: state.accountsError ? new Error("Account read failed") : null,
      refetch: state.refetch,
    },
  }),
}));
vi.mock("../api/channelJobs", () => ({
  createMeasurementPlan: vi.fn(),
  executeChannelTarget: vi.fn(),
  getChannelTarget: vi.fn(),
  getMeasurementPlan: vi.fn(),
  getMeasurementOptions: vi.fn(),
  listMeasurementPlans: vi.fn(),
}));
vi.mock("../api/questions", () => ({
  listAllQuestionSets: vi.fn(),
  listAllQuestionSetVersions: vi.fn(),
  getQuestionSetVersion: vi.fn(),
}));
vi.mock("../api/measurementReports", () => ({
  getMeasurementReport: vi.fn(),
  getMeasurementReportPreview: vi.fn(),
  listMeasurementReports: vi.fn(),
  saveMeasurementReport: vi.fn(),
}));
const target = {
  target_id: "target-1",
  input: {
    kind: "measure" as const,
    account_id: "account-1",
    provider: "kimi",
    model: "visible-model",
    surface: "consumer_web",
    search_mode: "web_search",
    protocol_version: "v1",
    question_set_version: "ad_hoc.v1",
    question: "如何观察流星雨？",
    market: "CN",
    language: "zh-CN",
    scheduled_at: "2026-10-01T00:00:00Z",
    sample_ordinal: 0,
  },
};
const plan = {
  plan_id: "measurement-1",
  project_id: "project-1",
  title: "自定义问题测量",
  created_at: "2026-10-01T00:00:00Z",
  targets: [],
};
function UrlState() {
  const location = useLocation();
  const navigate = useNavigate();
  return (
    <>
      <output data-testid="measurement-url">{location.search}</output>
      <Link to="?tab=records&record=measurement-2">Open another record</Link>
      <Link to="?tab=records">Clear selection</Link>
      <Link to="?tab=records&set=set-2&version=version-2&rankReport=report-2">
        Open another saved report
      </Link>
      <button onClick={() => navigate(-1)}>Back to previous selection</button>
    </>
  );
}
function Panel({
  canWrite,
  recordsOnly,
  followTabs,
}: {
  canWrite: boolean;
  recordsOnly: boolean;
  followTabs: boolean;
}) {
  const [params] = useSearchParams();
  return (
    <StandaloneMeasurementPanel
      tenantId="tenant-1"
      projectId="project-1"
      canWrite={canWrite}
      recordsOnly={followTabs ? params.get("tab") === "records" : recordsOnly}
    />
  );
}
function renderPanel(
  canWrite = true,
  recordsOnly = false,
  followTabs = false,
  initialEntry = "/",
) {
  return render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider
        client={
          new QueryClient({
            defaultOptions: {
              queries: { retry: false },
              mutations: { retry: false },
            },
          })
        }
      >
        <MemoryRouter initialEntries={[initialEntry]}>
          <Panel
            canWrite={canWrite}
            recordsOnly={recordsOnly}
            followTabs={followTabs}
          />
          <UrlState />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
}
function renderDiscoveryPanel(client: QueryClient, active = true) {
  const element = (visible: boolean) => (
    <StandaloneMeasurementPanel
      tenantId="tenant-1"
      projectId="project-1"
      canWrite
      active={visible}
    />
  );
  const view = render(element(active), {
    wrapper: ({ children }) => (
      <FluentProvider theme={webLightTheme}>
        <QueryClientProvider client={client}>
          <MemoryRouter>{children}</MemoryRouter>
        </QueryClientProvider>
      </FluentProvider>
    ),
  });
  return {
    ...view,
    revisit: (visible = true) => view.rerender(element(visible)),
  };
}
beforeEach(() => {
  vi.clearAllMocks();
  state.accountsError = false;
  state.platforms = [
    {
      id: "kimi",
      label: "Kimi",
      purpose: "measurement",
      login_supported: true,
      login_entry_available: true,
      measurement_supported: true,
    },
  ];
  state.accounts = [
    {
      account_id: "account-1",
      platform: "kimi",
      status: "ready",
      enabled: true,
      display_name: "测量账号",
    },
  ];
  vi.mocked(listMeasurementPlans).mockResolvedValue({
    items: [],
    next_after: null,
  });
  vi.mocked(getMeasurementPlan).mockResolvedValue(plan);
  vi.mocked(getChannelTarget).mockResolvedValue({ target, attempts: [] });
  vi.mocked(createMeasurementPlan).mockResolvedValue(plan);
  vi.mocked(getMeasurementOptions).mockResolvedValue({
    models: [{ id: "visible-model", label: "网页当前模型" }],
    selected_model: "visible-model",
  });
  vi.mocked(listAllQuestionSets).mockResolvedValue({
    items: [],
    next_cursor: null,
  });
  vi.mocked(listAllQuestionSetVersions).mockResolvedValue({
    items: [],
    next_cursor: null,
  });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});
describe("standalone arbitrary-topic measurement", () => {
  it("shows the question, platform, account and timestamp instead of a generic history label", async () => {
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [{ ...plan, title: "联网搜索测量", targets: [target] }],
      next_after: null,
    });
    renderPanel(false, true);
    const record = await screen.findByRole("button", {
      name: /如何观察流星雨.*Kimi.*测量账号/,
    });
    expect(record).toBeVisible();
    expect(record.querySelector("time")).toHaveAttribute(
      "dateTime",
      plan.created_at,
    );
    expect(screen.queryByText("联网搜索测量")).not.toBeInTheDocument();
  });
  it("does not prompt reconnecting unrelated expired accounts when a ready account is selected", async () => {
    state.accounts.push({
      account_id: "expired",
      platform: "kimi",
      status: "expired",
      enabled: true,
      display_name: "另一个账号",
    });
    renderPanel();
    await screen.findByText("模型：网页当前模型");
    expect(
      screen.queryByRole("link", { name: "重新连接已有账号" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "添加测量账号" })).toBeVisible();
  });
  it("uses the user's question as the new record title within the API byte limit", async () => {
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText("模型：网页当前模型");
    const question = "推荐适合出差的轻薄笔记本电脑。".repeat(8);
    await user.type(screen.getByLabelText("要测量的问题"), question);
    await user.click(screen.getByRole("button", { name: "开始测量" }));
    await waitFor(() => expect(createMeasurementPlan).toHaveBeenCalledTimes(1));
    const input = vi.mocked(createMeasurementPlan).mock.calls[0][2];
    expect(input.title).toBe(Array.from(question).slice(0, 50).join(""));
    expect(new TextEncoder().encode(input.title).length).toBeLessThanOrEqual(
      200,
    );
    expect(input.measurements[0].question).toBe(question);
  });
  it("defers model discovery until the measurement form is active", async () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    const panel = render(
      <StandaloneMeasurementPanel
        tenantId="tenant-1"
        projectId="project-1"
        canWrite
        active={false}
      />,
      {
        wrapper: ({ children }) => (
          <FluentProvider theme={webLightTheme}>
            <QueryClientProvider client={client}>
              <MemoryRouter>{children}</MemoryRouter>
            </QueryClientProvider>
          </FluentProvider>
        ),
      },
    );
    await waitFor(() => expect(listAllQuestionSets).toHaveBeenCalledOnce());
    window.dispatchEvent(new Event("focus"));
    expect(getMeasurementOptions).not.toHaveBeenCalled();
    expect(state.refetch).not.toHaveBeenCalled();
    panel.rerender(
      <StandaloneMeasurementPanel
        tenantId="tenant-1"
        projectId="project-1"
        canWrite
        active
      />,
    );
    expect(await screen.findByText("模型：网页当前模型")).toBeVisible();
    expect(getMeasurementOptions).toHaveBeenCalledOnce();
    window.dispatchEvent(new Event("focus"));
    expect(state.refetch).toHaveBeenCalledOnce();
    panel.rerender(
      <StandaloneMeasurementPanel
        tenantId="tenant-1"
        projectId="project-1"
        canWrite
        active={false}
      />,
    );
    window.dispatchEvent(new Event("focus"));
    expect(state.refetch).toHaveBeenCalledOnce();
  });

  it("reuses discovery beyond the ordinary query lifetime across tab and page revisits", async () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 30_000 } },
    });
    const initialTime = Date.now();
    const now = vi.spyOn(Date, "now").mockReturnValue(initialTime);
    const panel = renderDiscoveryPanel(client);
    await screen.findByText("模型：网页当前模型");
    now.mockReturnValue(initialTime + 31_000);
    panel.revisit(false);
    panel.revisit();
    expect(screen.getByText("模型：网页当前模型")).toBeVisible();
    expect(getMeasurementOptions).toHaveBeenCalledOnce();
    panel.unmount();
    renderDiscoveryPanel(client);
    await screen.findByText("模型：网页当前模型");
    expect(getMeasurementOptions).toHaveBeenCalledOnce();
  });

  it("discovers again after five minutes and allows an explicit model refresh", async () => {
    const user = userEvent.setup();
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    const initialTime = Date.now();
    const now = vi.spyOn(Date, "now").mockReturnValue(initialTime);
    const panel = renderDiscoveryPanel(client);
    await screen.findByText("模型：网页当前模型");
    panel.revisit(false);
    now.mockReturnValue(initialTime + 5 * 60_000 + 1);
    panel.revisit();
    await waitFor(() => expect(getMeasurementOptions).toHaveBeenCalledTimes(2));
    await user.click(screen.getByText("高级选项"));
    await user.click(screen.getByRole("button", { name: "刷新可用模型" }));
    await waitFor(() => expect(getMeasurementOptions).toHaveBeenCalledTimes(3));
  });

  it("keeps one pending browser discovery across hide, unmount and return", async () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    let complete!: (
      value: Awaited<ReturnType<typeof getMeasurementOptions>>,
    ) => void;
    vi.mocked(getMeasurementOptions).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          complete = resolve;
        }),
    );
    const panel = renderDiscoveryPanel(client);
    await waitFor(() => expect(getMeasurementOptions).toHaveBeenCalledOnce());
    panel.revisit(false);
    panel.revisit();
    panel.unmount();
    renderDiscoveryPanel(client);
    expect(getMeasurementOptions).toHaveBeenCalledOnce();
    complete({
      models: [{ id: "completed-model", label: "已识别模型" }],
      selected_model: "completed-model",
    });
    expect(await screen.findByText("模型：已识别模型")).toBeVisible();
    expect(getMeasurementOptions).toHaveBeenCalledOnce();
  });

  it.each([
    ["account", { account_id: "account-2" }],
    ["verified login", { updated_at: "2026-10-22T12:00:00Z" }],
    ["identity", { platform_account_id: "synthetic-identity-2" }],
    [
      "network",
      { proxy_configured: true, proxy_server: "http://proxy.invalid" },
    ],
  ])(
    "hides the previous menu and discovers after an %s change",
    async (_, change) => {
      const client = new QueryClient({
        defaultOptions: { queries: { retry: false } },
      });
      const panel = renderDiscoveryPanel(client);
      await screen.findByText("模型：网页当前模型");
      let complete!: (
        value: Awaited<ReturnType<typeof getMeasurementOptions>>,
      ) => void;
      vi.mocked(getMeasurementOptions).mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            complete = resolve;
          }),
      );
      state.accounts = [{ ...(state.accounts[0] as object), ...change }];
      panel.revisit();
      expect(screen.queryByText("模型：网页当前模型")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
      await waitFor(() =>
        expect(getMeasurementOptions).toHaveBeenCalledTimes(2),
      );
      complete({
        models: [{ id: "updated-model", label: "新可用模型" }],
        selected_model: "updated-model",
      });
      expect(await screen.findByText("模型：新可用模型")).toBeVisible();
    },
  );

  it("hides cached models when the selected account expires", async () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    const panel = renderDiscoveryPanel(client);
    await screen.findByText("模型：网页当前模型");
    state.accounts = [{ ...(state.accounts[0] as object), status: "expired" }];
    panel.revisit();
    expect(screen.queryByText("模型：网页当前模型")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
    expect(getMeasurementOptions).toHaveBeenCalledOnce();
  });

  it.each([undefined, "older-version", "discover"])(
    "opens scoped report history without matching measurements on this page (requested version: %s)",
    async (requestedVersion) => {
      vi.mocked(listAllQuestionSets).mockResolvedValue({
        items: [
          {
            id: "set-1",
            name: "灯具问题",
            current_version_id: "current-version",
            current_revision: 2,
            question_count: 1,
            optimization_count: 1,
            evaluation_count: 0,
          },
        ],
        next_cursor: null,
      });
      vi.mocked(listMeasurementPlans).mockResolvedValue({
        items: [plan],
        next_after: "older-page",
      });
      const discover = requestedVersion === "discover";
      const selectedVersion = discover
        ? "older-version"
        : (requestedVersion ?? "current-version");
      if (discover) {
        vi.mocked(listAllQuestionSetVersions).mockResolvedValue({
          items: [2, 1].map((revision) => ({
            id: revision === 2 ? "current-version" : "older-version",
            question_set_id: "set-1",
            revision,
            parent_version_id: revision === 2 ? "older-version" : null,
            name: "灯具问题",
            question_count: 1,
            optimization_count: 1,
            evaluation_count: 0,
            split_policy_version: "v1",
            created_at: "2026-09-01T00:00:00Z",
          })),
          next_cursor: null,
        });
      }
      const snapshot = {
        kind: "measurement_period" as const,
        project_id: "project-1",
        question_set: {
          question_set_id: "set-1",
          question_set_version_id: selectedVersion,
        },
        report_id: "report-1",
        revision: 1,
        correction_of: null,
        report_window_start_at: "2026-09-01T00:00:00Z",
        report_window_end_at: "2026-10-01T00:00:00Z",
        report_timezone: "UTC",
        evidence_as_of: "2026-10-01T00:00:00Z",
        generated_at: "2026-10-01T00:00:00Z",
        input_hash: "hash",
        coverage: {
          planned: 0,
          counts: {},
          observed_live: 0,
          grounded_saved_analysis: 0,
        },
        samples: [],
      };
      vi.mocked(getMeasurementReportPreview).mockResolvedValue({
        ...snapshot,
        kind: "measurement_period_preview",
      });
      vi.mocked(listMeasurementReports).mockResolvedValue({
        items: [snapshot],
      });
      vi.mocked(getMeasurementReport).mockResolvedValue(snapshot);
      renderPanel(
        false,
        true,
        false,
        discover
          ? "/?tab=records"
          : `/?tab=records&set=set-1${requestedVersion ? `&version=${requestedVersion}` : ""}`,
      );
      const folder = (
        await screen.findByText("灯具问题 · 本页 0 条记录")
      ).closest("details")!;
      if (discover) {
        expect(listAllQuestionSetVersions).not.toHaveBeenCalled();
        await userEvent.click(
          within(folder).getByText("灯具问题 · 本页 0 条记录"),
        );
        await within(folder).findByText("历史版本 v1");
        expect(listAllQuestionSetVersions).toHaveBeenCalledWith(
          "tenant-1",
          "project-1",
          "set-1",
        );
        expect(listMeasurementPlans).toHaveBeenCalledOnce();
      }
      expect(
        within(folder).queryByRole("button", {
          name: /自定义问题测量|如何观察流星雨/,
        }),
      ).not.toBeInTheDocument();
      const versionFolder = within(folder)
        .getByText(
          discover
            ? "历史版本 v1"
            : requestedVersion
              ? "历史版本 older-version"
              : "当前版本 v2",
        )
        .closest("details")!;
      await userEvent.click(
        within(versionFolder).getByRole("button", { name: "排名变化" }),
      );
      await userEvent.click(
        await within(versionFolder).findByRole("button", {
          name: /查看版本 1/,
        }),
      );
      expect(
        await within(versionFolder).findByText("报告版本 1"),
      ).toBeVisible();
      expect(listMeasurementReports).toHaveBeenCalledWith(
        "tenant-1",
        "project-1",
        snapshot.question_set,
      );
      expect(getMeasurementReport).toHaveBeenCalledWith(
        "tenant-1",
        "project-1",
        "report-1",
      );
      expect(listMeasurementPlans).toHaveBeenCalledTimes(1);
      expect(getChannelTarget).not.toHaveBeenCalled();
      expect(screen.getByTestId("measurement-url")).toHaveTextContent(
        `version=${selectedVersion}&rankReport=report-1`,
      );
    },
  );

  it.each([true, false])(
    "opens a linked saved rank report on reload independently of the current history page (record listed: %s)",
    async (listed) => {
      const questionSet = {
        question_set_id: "set-1",
        question_set_version_id: "version-1",
      };
      const bound = {
        ...target,
        input: {
          ...target.input,
          question_binding: {
            reference: {
              ...questionSet,
              question_id: "question-1",
              question_revision_id: "revision-1",
            },
            purpose: "optimization" as const,
            split_policy_version: "v1",
          },
        },
      };
      vi.mocked(listMeasurementPlans).mockResolvedValue({
        items: listed ? [{ ...plan, targets: [bound] }] : [],
        next_after: null,
      });
      vi.mocked(listMeasurementReports).mockResolvedValue({ items: [] });
      vi.mocked(getMeasurementReport).mockResolvedValue({
        kind: "measurement_period",
        project_id: "project-1",
        question_set: questionSet,
        report_id: "report-1",
        revision: 1,
        correction_of: null,
        report_window_start_at: "2026-09-01T00:00:00Z",
        report_window_end_at: "2026-10-01T00:00:00Z",
        report_timezone: "UTC",
        evidence_as_of: "2026-10-01T00:00:00Z",
        generated_at: "2026-10-01T00:00:00Z",
        input_hash: "hash",
        coverage: {
          planned: 0,
          counts: {},
          observed_live: 0,
          grounded_saved_analysis: 0,
        },
        samples: [],
      });
      renderPanel(
        false,
        true,
        false,
        "/?tab=records&record=measurement-1&set=set-1&version=version-1&rankReport=report-1",
      );
      expect(await screen.findByText("报告版本 1")).toBeVisible();
      expect(screen.getByRole("button", { name: "测量历史" })).toHaveAttribute(
        "aria-expanded",
        "true",
      );
      expect(getMeasurementReport).toHaveBeenCalledWith(
        "tenant-1",
        "project-1",
        "report-1",
      );
      expect(getMeasurementReportPreview).not.toHaveBeenCalled();
      expect(listAllQuestionSetVersions).not.toHaveBeenCalled();
      expect(screen.getByTestId("measurement-url")).toHaveTextContent(
        "record=measurement-1",
      );
      if (listed) {
        await userEvent.click(
          screen.getByRole("button", { name: /自定义问题测量|如何观察流星雨/ }),
        );
        expect(screen.getByTestId("measurement-url")).not.toHaveTextContent(
          "rankReport=",
        );
        expect(screen.getByTestId("measurement-url")).toHaveTextContent(
          "set=set-1&version=version-1",
        );
        expect(
          screen.getByRole("button", { name: "测量历史" }),
        ).toHaveAttribute("aria-expanded", "false");
      } else {
        const folder = screen
          .getByText("历史版本 version-1")
          .closest("details")!
          .parentElement!.closest("details")!;
        const summary = folder.querySelector("summary")!;
        await userEvent.click(summary);
        expect(folder).not.toHaveAttribute("open");
        await userEvent.click(
          screen.getByRole("link", { name: "Open another saved report" }),
        );
        await userEvent.click(
          screen.getByRole("button", { name: "Back to previous selection" }),
        );
        await waitFor(() =>
          expect(
            screen
              .getByText("历史版本 version-1")
              .closest("details")!
              .parentElement!.closest("details"),
          ).toHaveAttribute("open"),
        );
        expect(await screen.findByText("报告版本 1")).toBeVisible();
        await userEvent.click(
          screen.getByRole("button", { name: "返回测量结果预览" }),
        );
        await userEvent.click(screen.getByRole("button", { name: "测量历史" }));
        await waitFor(() =>
          expect(listAllQuestionSetVersions).toHaveBeenCalledWith(
            "tenant-1",
            "project-1",
            "set-1",
          ),
        );
      }
    },
  );

  it("nests repeated measurements under their actual set and version and separates temporary records", async () => {
    const binding = {
      reference: {
        question_set_id: "set-1",
        question_set_version_id: "version-1",
        question_id: "question-1",
        question_revision_id: "revision-1",
      },
      purpose: "optimization" as const,
      split_policy_version: "v1",
    };
    const bound = {
      ...target,
      input: { ...target.input, question_binding: binding },
    };
    vi.mocked(listAllQuestionSets).mockResolvedValue({
      items: [
        {
          id: "set-1",
          name: "观测问题",
          current_version_id: "version-1",
          current_revision: 1,
          question_count: 1,
          optimization_count: 1,
          evaluation_count: 0,
        },
      ],
      next_cursor: null,
    });
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [
        {
          ...plan,
          plan_id: "bound-1",
          title: "第一次观测",
          targets: [bound, { ...bound, target_id: "another-target" }],
        },
        { ...plan, plan_id: "bound-2", title: "第二次观测", targets: [bound] },
        { ...plan, plan_id: "custom-1", title: "临时观测", targets: [target] },
      ],
      next_after: null,
    });
    vi.mocked(getMeasurementPlan).mockResolvedValue({
      ...plan,
      plan_id: "bound-2",
      targets: [bound],
    });
    const user = userEvent.setup();
    renderPanel(
      false,
      true,
      false,
      "/?tab=records&target=stale&attempt=stale&analysisRevision=stale",
    );
    const folder = (
      await screen.findByText("观测问题 · 本页 2 条记录")
    ).closest("details")!;
    expect(folder).not.toHaveAttribute("open");
    await user.click(within(folder).getByText("观测问题 · 本页 2 条记录"));
    expect(folder).toHaveAttribute("open");
    expect(within(folder).getByText("当前版本 v1")).toBeVisible();
    expect(
      within(folder).getAllByRole("button", { name: /如何观察流星雨/ }),
    ).toHaveLength(2);
    expect(
      within(folder).getByRole("button", { name: "排名变化" }),
    ).toBeVisible();
    expect(within(folder).queryByText(/临时观测/)).not.toBeInTheDocument();
    const temporary = screen
      .getByText("临时测量 · 本页 1 条记录")
      .closest("details")!;
    expect(
      within(temporary).getByRole("button", { name: /如何观察流星雨/ }),
    ).toBeVisible();
    await user.click(
      within(folder).getAllByRole("button", { name: /如何观察流星雨/ })[1],
    );
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "record=bound-2",
    );
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "set=set-1",
    );
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "version=version-1",
    );
    for (const parameter of ["target=", "attempt=", "analysisRevision="]) {
      expect(screen.getByTestId("measurement-url")).not.toHaveTextContent(
        parameter,
      );
    }
  });

  it("opens only the target identified by a ranking source link", async () => {
    const selected = {
      ...target,
      target_id: "selected-target",
      input: { ...target.input, question: "指定问题" },
    };
    const other = {
      ...target,
      target_id: "other-target",
      input: { ...target.input, question: "其他问题" },
    };
    vi.mocked(getMeasurementPlan).mockResolvedValue({
      ...plan,
      targets: [other, selected],
    });
    vi.mocked(getChannelTarget).mockImplementation(
      async (_tenant, _project, id) => ({
        target: id === selected.target_id ? selected : other,
        attempts: [],
      }),
    );
    renderPanel(
      false,
      true,
      false,
      "/?tab=records&record=measurement-1&target=selected-target&attempt=selected-attempt&analysisRevision=selected-analysis",
    );
    expect(
      await screen.findByRole("heading", { name: "指定问题" }),
    ).toBeVisible();
    expect(
      screen.queryByRole("heading", { name: "其他问题" }),
    ).not.toBeInTheDocument();
    expect(getChannelTarget).toHaveBeenCalledTimes(1);
  });
  it.each(["record", "planId"])(
    "opens a %s deep link with history collapsed and the answer visible",
    async (parameter) => {
      const savedPlan = { ...plan, targets: [target] };
      vi.mocked(listMeasurementPlans).mockResolvedValue({
        items: [savedPlan],
        next_after: null,
      });
      vi.mocked(getMeasurementPlan).mockResolvedValue(savedPlan);
      vi.mocked(getChannelTarget).mockResolvedValue({
        target,
        attempts: [
          {
            attempt_id: "attempt-saved",
            target_id: target.target_id,
            claimed_at: "2026-10-01T00:00:01Z",
            received_at: "2026-10-01T00:00:04Z",
            outcome: {
              status: "observed",
              detail: "已收到搜索答案",
              occurred_at: "2026-10-01T00:00:04Z",
              raw_answer: "可在晴朗夜晚观测",
              citations: [],
              public_url: null,
              screenshot_ref: null,
              connector_version: "v1",
              runner_evidence: [],
              fixture: false,
            },
          },
        ],
      });
      renderPanel(
        false,
        true,
        false,
        `/?tab=records&${parameter}=measurement-1`,
      );
      expect(await screen.findByText("可在晴朗夜晚观测")).toBeVisible();
      expect(screen.getByRole("button", { name: "测量历史" })).toHaveAttribute(
        "aria-expanded",
        "false",
      );
      expect(
        screen.queryByRole("button", { name: /自定义问题测量|如何观察流星雨/ }),
      ).not.toBeInTheDocument();
    },
  );

  it("expands history without a selection, collapses on selection and allows reopening", async () => {
    const savedPlan = { ...plan, targets: [target] };
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [savedPlan],
      next_after: null,
    });
    vi.mocked(getMeasurementPlan).mockResolvedValue(savedPlan);
    const user = userEvent.setup();
    renderPanel(false, true);
    const history = screen.getByRole("button", { name: "测量历史" });
    expect(history).toHaveAttribute("aria-expanded", "true");
    await user.click(
      await screen.findByRole("button", {
        name: /自定义问题测量|如何观察流星雨/,
      }),
    );
    expect(await screen.findByText(target.input.question)).toBeVisible();
    expect(history).toHaveAttribute("aria-expanded", "false");
    await user.click(history);
    expect(history).toHaveAttribute("aria-expanded", "true");
    expect(
      screen.getByRole("heading", { name: target.input.question }),
    ).toBeVisible();
    await user.click(
      screen.getByRole("button", { name: /自定义问题测量|如何观察流星雨/ }),
    );
    expect(history).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByText(target.input.question)).toBeVisible();
  });

  it("follows URL selection changes after manually reopening history", async () => {
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [plan, { ...plan, plan_id: "measurement-2", title: "另一条测量" }],
      next_after: null,
    });
    vi.mocked(getMeasurementPlan).mockImplementation(
      async (_tenant, _project, id) => ({
        ...plan,
        plan_id: id,
        title: id === "measurement-2" ? "另一条测量" : plan.title,
      }),
    );
    const user = userEvent.setup();
    renderPanel(false, true, false, "/?tab=records&record=measurement-1");
    const history = screen.getByRole("button", { name: "测量历史" });
    await user.click(history);
    expect(history).toHaveAttribute("aria-expanded", "true");
    await user.click(screen.getByRole("link", { name: "Open another record" }));
    expect(await screen.findByText("另一条测量")).toBeVisible();
    expect(history).toHaveAttribute("aria-expanded", "false");
    expect(getMeasurementPlan).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "measurement-2",
    );
    await user.click(screen.getByRole("link", { name: "Clear selection" }));
    expect(history).toHaveAttribute("aria-expanded", "true");
    expect(
      await screen.findByRole("button", {
        name: /自定义问题测量|如何观察流星雨/,
      }),
    ).toBeVisible();
  });

  it("submits the selected supported provider instead of a fixed provider", async () => {
    state.platforms = [
      {
        id: "deepseek",
        label: "DeepSeek",
        purpose: "measurement",
        login_supported: true,
        login_entry_available: true,
        measurement_supported: true,
      },
    ];
    state.accounts = [
      {
        account_id: "other-account",
        platform: "deepseek",
        status: "ready",
        enabled: true,
      },
    ];
    const user = userEvent.setup();
    renderPanel();
    await screen.findByRole("option", { name: "网页当前模型" });
    await user.type(screen.getByRole("textbox"), "如何观察流星雨？");
    await user.click(screen.getByRole("button", { name: "开始测量" }));
    await waitFor(() =>
      expect(createMeasurementPlan).toHaveBeenCalledWith(
        "tenant-1",
        "project-1",
        expect.objectContaining({
          measurements: [
            expect.objectContaining({
              provider: "deepseek",
              account_id: "other-account",
            }),
          ],
        }),
      ),
    );
  });
  it.each([
    { login_supported: false, measurement_supported: false },
    { login_supported: true, measurement_supported: false },
    { login_supported: false, measurement_supported: true },
  ])(
    "does not measure an account without both verified capabilities: %j",
    async (capabilities) => {
      state.platforms = [
        {
          id: "kimi",
          label: "Kimi",
          purpose: "measurement",
          login_entry_available: true,
          ...capabilities,
        },
      ];
      renderPanel();
      expect(
        await screen.findByRole("button", { name: "开始测量" }),
      ).toBeDisabled();
      expect(getMeasurementOptions).not.toHaveBeenCalled();
      expect(createMeasurementPlan).not.toHaveBeenCalled();
      expect(
        screen.getByRole("option", { name: "还没有可用的测量账号。" }),
      ).toBeInTheDocument();
    },
  );
  it.each([true, false])(
    "keeps record identifiers in collapsed details (account label available: %s)",
    async (hasAccountLabel) => {
      if (!hasAccountLabel) state.accounts = [];
      const savedPlan = { ...plan, targets: [target] };
      vi.mocked(listMeasurementPlans).mockResolvedValue({
        items: [savedPlan],
        next_after: null,
      });
      vi.mocked(getMeasurementPlan).mockResolvedValue(savedPlan);
      const user = userEvent.setup();
      renderPanel(false, true);
      await user.click(
        await screen.findByRole("button", {
          name: /自定义问题测量|如何观察流星雨/,
        }),
      );
      expect(await screen.findByText(target.input.question)).toBeVisible();
      expect(
        screen.getByText(hasAccountLabel ? "kimi · 账号 测量账号" : "kimi"),
      ).toBeVisible();
      expect(screen.queryByText(/account-1/)).not.toBeInTheDocument();
      expect(screen.getByText("计划 ID：measurement-1")).not.toBeVisible();
      await user.click(screen.getByText("测量计划技术详情"));
      expect(screen.getByText("计划 ID：measurement-1")).toBeVisible();
      await user.click(screen.getByText("技术详情 / 原始证据"));
      expect(
        await screen.findByText(/"account_id": "account-1"/),
      ).toBeVisible();
    },
  );

  it("creates one plan and displays queued, in-progress and final results without a second execute request", async () => {
    const savedPlan = { ...plan, targets: [target] };
    vi.mocked(createMeasurementPlan).mockResolvedValue(savedPlan);
    vi.mocked(getMeasurementPlan).mockResolvedValue(savedPlan);
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [savedPlan],
      next_after: null,
    });
    const attempt = {
      attempt_id: "attempt-1",
      target_id: target.target_id,
      claimed_at: "2026-10-01T00:00:01Z",
      received_at: null,
      outcome: null,
    };
    vi.mocked(getChannelTarget)
      .mockResolvedValueOnce({ target, attempts: [] })
      .mockResolvedValueOnce({ target, attempts: [attempt] })
      .mockResolvedValueOnce({
        target,
        attempts: [
          {
            ...attempt,
            received_at: "2026-10-01T00:00:04Z",
            outcome: {
              status: "observed",
              detail: "已收到搜索答案",
              occurred_at: "2026-10-01T00:00:04Z",
              raw_answer: "可在晴朗夜晚观测",
              citations: [],
              public_url: null,
              screenshot_ref: null,
              connector_version: "v1",
              runner_evidence: [],
              fixture: false,
            },
          },
        ],
      });
    const user = userEvent.setup();
    renderPanel(true, false, true);
    await user.type(
      screen.getByLabelText("要测量的问题"),
      target.input.question,
    );
    await screen.findByText("模型：网页当前模型");
    await user.click(screen.getByRole("button", { name: "开始测量" }));
    expect(await screen.findByText("等待自动测量")).toBeInTheDocument();
    expect(screen.getByText("测量已排期。")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "执行此目标" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "刷新记录" }));
    expect(await screen.findByText("执行中或等待结果")).toBeInTheDocument();
    expect(screen.getByText("正在等待测量结果。")).toBeInTheDocument();
    expect(screen.getByText(/开始时间/)).toHaveTextContent(
      new Date(attempt.claimed_at).toLocaleString("zh-CN"),
    );
    expect(screen.queryByText(/结果接收时间|结果时间/)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "刷新记录" }));
    expect(await screen.findByText("已取得测量结果")).toBeInTheDocument();
    expect(screen.getByText(/结果接收时间/)).toHaveTextContent(
      new Date("2026-10-01T00:00:04Z").toLocaleString("zh-CN"),
    );
    expect(screen.queryByText(/开始时间/)).not.toBeInTheDocument();
    expect(screen.getByText("可在晴朗夜晚观测")).toBeVisible();
    expect(screen.getByText("本次回答未提供引用链接。")).toBeVisible();
    expect(screen.queryByText(/已收到搜索答案/)).not.toBeInTheDocument();
    expect(createMeasurementPlan).toHaveBeenCalledOnce();
    expect(executeChannelTarget).not.toHaveBeenCalled();
  });

  it("shows a real record fetch failure without offering a manual execution action", async () => {
    const savedPlan = { ...plan, targets: [target] };
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [savedPlan],
      next_after: null,
    });
    vi.mocked(getMeasurementPlan).mockResolvedValue(savedPlan);
    vi.mocked(getChannelTarget).mockRejectedValue(new Error("状态读取失败"));
    const user = userEvent.setup();
    renderPanel(true, true);
    await user.click(
      await screen.findByRole("button", {
        name: /自定义问题测量|如何观察流星雨/,
      }),
    );
    expect(await screen.findByText("状态读取失败")).toBeInTheDocument();
    expect(screen.getByText("测量状态暂不可用")).toBeInTheDocument();
    expect(screen.queryByText("等待自动测量")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "执行此目标" }),
    ).not.toBeInTheDocument();
    expect(executeChannelTarget).not.toHaveBeenCalled();
  });

  it("describes an unknown measurement without promising reconciliation", async () => {
    const savedPlan = { ...plan, targets: [target] };
    vi.mocked(listMeasurementPlans).mockResolvedValue({
      items: [savedPlan],
      next_after: null,
    });
    vi.mocked(getMeasurementPlan).mockResolvedValue(savedPlan);
    vi.mocked(getChannelTarget).mockResolvedValue({
      target,
      attempts: [
        {
          attempt_id: "attempt-unknown",
          target_id: target.target_id,
          claimed_at: "2026-10-01T00:00:01Z",
          received_at: "2026-10-01T00:01:34Z",
          outcome: {
            status: "unknown",
            detail: "响应采集超时",
            occurred_at: "2026-10-01T00:00:01Z",
            raw_answer: null,
            citations: [],
            public_url: null,
            screenshot_ref: null,
            connector_version: "v1",
            runner_evidence: [],
            fixture: true,
          },
        },
      ],
    });
    const user = userEvent.setup();
    renderPanel(true, true);
    await user.click(
      await screen.findByRole("button", {
        name: /自定义问题测量|如何观察流星雨/,
      }),
    );
    expect(await screen.findByText("测量结果未知")).toBeInTheDocument();
    expect(screen.getByText(/结果接收时间/)).toHaveTextContent(
      new Date("2026-10-01T00:01:34Z").toLocaleString("zh-CN"),
    );
    expect(screen.getByText(/结果接收时间/)).not.toHaveTextContent(
      new Date("2026-10-01T00:00:01Z").toLocaleString("zh-CN"),
    );
    expect(screen.getByText("未能确认本次测量结果。")).toBeInTheDocument();
    expect(screen.queryByText(/响应采集超时/)).not.toBeInTheDocument();
    expect(screen.queryByText("测试数据，非真实测量")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "回答" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/已取得测量结果/)).not.toBeInTheDocument();
    expect(screen.queryByText(/等待对账/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "执行此目标" }),
    ).not.toBeInTheDocument();
    expect(executeChannelTarget).not.toHaveBeenCalled();
    await user.click(screen.getByText("技术详情 / 原始证据"));
    expect(await screen.findByText(/响应采集超时/)).toBeVisible();
  });

  it("keeps model discovery errors visible and does not submit a plan", async () => {
    vi.mocked(getMeasurementOptions).mockRejectedValue(
      new Error("模型发现失败"),
    );
    renderPanel();
    expect(await screen.findByText("模型发现失败")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
    expect(createMeasurementPlan).not.toHaveBeenCalled();
    expect(executeChannelTarget).not.toHaveBeenCalled();
  });

  it("submits an immediate arbitrary question without knowledge, cycle or question-set input", async () => {
    const user = userEvent.setup();
    renderPanel();
    await user.type(screen.getByLabelText("要测量的问题"), "如何观察流星雨？");
    await screen.findByText("模型：网页当前模型");
    expect(screen.queryByLabelText("独立测量协议")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("测量模型标识")).not.toBeInTheDocument();
    expect(screen.getByText("高级选项").closest("details")).not.toHaveAttribute(
      "open",
    );
    await user.click(screen.getByRole("button", { name: "开始测量" }));
    await waitFor(() => expect(createMeasurementPlan).toHaveBeenCalledOnce());
    const input = vi.mocked(createMeasurementPlan).mock.calls[0][2];
    expect(input.measurements[0]).toMatchObject({
      question: "如何观察流星雨？",
      account_id: "account-1",
      question_set_version: "ad_hoc.v1",
      model: "visible-model",
      protocol_version: "v1",
      surface: "consumer_web",
      search_mode: "web_search",
    });
    expect(Date.parse(input.measurements[0].scheduled_at)).toBeLessThanOrEqual(
      Date.now(),
    );
    expect(input).not.toHaveProperty("cycle_id");
    expect(input).not.toHaveProperty("source_id");
    expect(await screen.findByText(/测量计划已受理/)).toBeInTheDocument();
    expect(screen.getByTestId("measurement-url")).toHaveTextContent(
      "tab=records&record=measurement-1",
    );
  });
  it("replays the exact timestamp and idempotency key after an uncertain submission", async () => {
    vi.mocked(createMeasurementPlan)
      .mockRejectedValueOnce(new Error("连接中断"))
      .mockResolvedValueOnce(plan);
    const user = userEvent.setup();
    renderPanel();
    await user.type(screen.getByLabelText("要测量的问题"), "任意问题");
    await screen.findByText("模型：网页当前模型");
    await user.click(screen.getByRole("button", { name: "开始测量" }));
    await user.click(
      await screen.findByRole("button", { name: "重试提交测量" }),
    );
    await waitFor(() => expect(createMeasurementPlan).toHaveBeenCalledTimes(2));
    expect(vi.mocked(createMeasurementPlan).mock.calls[1][2]).toEqual(
      vi.mocked(createMeasurementPlan).mock.calls[0][2],
    );
  });
  it("offers login when no ready account is available and cannot submit", async () => {
    state.accounts = [
      {
        account_id: "expired",
        platform: "kimi",
        enabled: true,
        status: "expired",
      },
    ];
    renderPanel();
    expect(screen.getByRole("link", { name: "添加测量账号" })).toHaveAttribute(
      "href",
      "/app/tenant-1/project-1/channels/connect",
    );
    expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
    expect(
      screen.getByRole("option", { name: "Kimi · 需要重新连接" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("link", { name: "重新连接已有账号" }),
    ).toBeInTheDocument();
    expect(getMeasurementOptions).not.toHaveBeenCalled();
  });
  it("places one accessible add-account link beside the selector without duplicate header controls", async () => {
    renderPanel();
    await screen.findByText("模型：网页当前模型");
    const selector = screen.getByLabelText("独立测量账号");
    const add = screen.getByRole("link", { name: "添加测量账号" });
    expect(selector.parentElement?.parentElement).toContainElement(add);
    expect(add).toHaveAttribute(
      "href",
      "/app/tenant-1/project-1/channels/connect",
    );
    expect(screen.queryByText("登录或连接 Kimi 账号")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "刷新账号" }),
    ).not.toBeInTheDocument();
    await userEvent.click(screen.getByText("高级选项"));
    expect(
      screen.getByText(/自定义问题 · 中国 · 中文 · 仅用于测量/),
    ).toHaveTextContent("通过账号的联网搜索获取回答");
    expect(
      screen.queryByText(/不替换为普通回答|未核验的搜索记录为缺测/),
    ).not.toBeInTheDocument();
  });
  it("retains one connection entry for an empty account list", () => {
    state.accounts = [];
    renderPanel();
    expect(screen.getByRole("link", { name: "连接账号" })).toHaveAttribute(
      "href",
      "/app/tenant-1/project-1/channels/connect",
    );
    expect(
      screen.queryByRole("link", { name: "添加测量账号" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
  });
  it("does not offer account connection controls to read-only users", () => {
    state.accounts = [];
    renderPanel(false);
    expect(
      screen.queryByRole("link", { name: "连接账号" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: "添加测量账号" }),
    ).not.toBeInTheDocument();
  });
  it("keeps explicit retry available when account loading fails", async () => {
    state.accountsError = true;
    renderPanel();
    expect(screen.getByText("测量账号无法读取")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(state.refetch).toHaveBeenCalled();
  });
  it("pages through history and reads the selected plan independently", async () => {
    vi.mocked(listMeasurementPlans).mockImplementation(
      async (_tenant, _project, after) =>
        after
          ? {
              items: [
                { ...plan, plan_id: "measurement-2", title: "更早的测量" },
              ],
              next_after: null,
            }
          : { items: [plan], next_after: "measurement-1" },
    );
    const user = userEvent.setup();
    renderPanel(false, true);
    await user.click(
      await screen.findByRole("button", {
        name: /自定义问题测量|如何观察流星雨/,
      }),
    );
    await waitFor(() =>
      expect(getMeasurementPlan).toHaveBeenCalledWith(
        "tenant-1",
        "project-1",
        "measurement-1",
      ),
    );
    await user.click(screen.getByRole("button", { name: "测量历史" }));
    await user.click(screen.getByRole("button", { name: "下一页测量" }));
    expect(
      await screen.findByRole("button", { name: /更早的测量/ }),
    ).toBeInTheDocument();
    expect(listMeasurementPlans).toHaveBeenCalledWith(
      "tenant-1",
      "project-1",
      "measurement-1",
    );
    await user.click(screen.getByRole("button", { name: "上一页测量" }));
    expect(
      await screen.findByRole("button", {
        name: /自定义问题测量|如何观察流星雨/,
      }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "开始测量" }),
    ).not.toBeInTheDocument();
  });
  it("refreshes saved accounts on returning from login", async () => {
    renderPanel();
    window.dispatchEvent(new Event("focus"));
    expect(state.refetch).toHaveBeenCalled();
  });
  it("never invents a model when discovery returns no models", async () => {
    vi.mocked(getMeasurementOptions).mockResolvedValue({
      models: [],
      selected_model: null,
    });
    const user = userEvent.setup();
    renderPanel();
    await user.type(screen.getByLabelText("要测量的问题"), "任意问题");
    expect(await screen.findByText(/未发现可用网页模型/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
  });
  it("offers existing project questions without a cycle and submits immutable references only", async () => {
    vi.mocked(listAllQuestionSets).mockResolvedValue({
      items: [
        {
          id: "set-1",
          name: "已有问题",
          current_version_id: "version-2",
          current_revision: 2,
          question_count: 1,
          optimization_count: 0,
          evaluation_count: 1,
        },
      ],
      next_cursor: null,
    });
    const frozenVersion: QuestionSetVersion = {
      id: "version-2",
      question_set_id: "set-1",
      revision: 2,
      parent_version_id: "version-1",
      name: "已有问题",
      question_count: 2,
      optimization_count: 0,
      evaluation_count: 1,
      split_policy_version: "v1",
      created_at: "2026-10-01T00:00:00Z",
      content_hash: "hash",
      questions: [
        {
          id: "revision-2",
          question_id: "question-1",
          text: "如何选择观测时间？",
          intent: "selection",
          product_refs: [],
          market: "CN",
          language: "zh-CN",
          source: { kind: "user_provided" },
          weight: 1,
          purpose: "frozen_evaluation",
          split_policy_version: "v1",
        },
        {
          id: "revision-3",
          question_id: "question-2",
          text: "如何选择观测地点？",
          intent: "selection",
          product_refs: [],
          market: "CN",
          language: "zh-CN",
          source: { kind: "user_provided" },
          weight: 1,
          purpose: "optimization",
          split_policy_version: "v1",
        },
      ],
    };
    vi.mocked(getQuestionSetVersion).mockResolvedValue(frozenVersion);
    vi.mocked(listAllQuestionSetVersions).mockResolvedValue({
      items: [frozenVersion],
      next_cursor: null,
    });
    const user = userEvent.setup();
    renderPanel();
    await user.selectOptions(screen.getByLabelText("问题来源"), "bound");
    expect(
      await screen.findByRole("option", { name: "如何选择观测时间？" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: "全部问题（2 题）" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "开始测量" }));
    await waitFor(() => expect(createMeasurementPlan).toHaveBeenCalledOnce());
    const input = vi.mocked(createMeasurementPlan).mock.calls[0][2];
    expect(input.measurements).toEqual([]);
    expect(input.bound_measurements).toHaveLength(2);
    expect(input.bound_measurements?.[0].question).toEqual({
      question_set_id: "set-1",
      question_set_version_id: "version-2",
      question_id: "question-1",
      question_revision_id: "revision-2",
    });
    expect(input.bound_measurements?.[0]).not.toHaveProperty("purpose");
    expect(input.bound_measurements?.[0]).not.toHaveProperty("market");
    expect(input.bound_measurements?.[1].question).toEqual({
      question_set_id: "set-1",
      question_set_version_id: "version-2",
      question_id: "question-2",
      question_revision_id: "revision-3",
    });
    expect(JSON.stringify(input)).not.toContain("如何选择观测时间");
  });
});
