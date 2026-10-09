import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useLocation, useSearchParams } from "react-router-dom";
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

const state = vi.hoisted(() => ({
  accounts: [] as unknown[],
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
  return <output data-testid="measurement-url">{location.search}</output>;
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
function renderPanel(canWrite = true, recordsOnly = false, followTabs = false) {
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
        <MemoryRouter>
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
beforeEach(() => {
  vi.clearAllMocks();
  state.accountsError = false;
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
afterEach(cleanup);
describe("standalone arbitrary-topic measurement", () => {
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
        await screen.findByRole("button", { name: /自定义问题测量/ }),
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
    await user.click(screen.getByRole("button", { name: "刷新记录" }));
    expect(await screen.findByText("已取得测量结果")).toBeInTheDocument();
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
      await screen.findByRole("button", { name: /自定义问题测量/ }),
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
          received_at: "2026-10-01T00:00:04Z",
          outcome: {
            status: "unknown",
            detail: "响应采集超时",
            occurred_at: "2026-10-01T00:00:04Z",
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
      await screen.findByRole("button", { name: /自定义问题测量/ }),
    );
    expect(await screen.findByText("测量结果未知")).toBeInTheDocument();
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
      screen.getByRole("option", { name: "Kimi 账号 · 需要重新连接" }),
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
      await screen.findByRole("button", { name: /自定义问题测量/ }),
    );
    await waitFor(() =>
      expect(getMeasurementPlan).toHaveBeenCalledWith(
        "tenant-1",
        "project-1",
        "measurement-1",
      ),
    );
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
      await screen.findByRole("button", { name: /自定义问题测量/ }),
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
      question_count: 1,
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
      screen.getByText(/冻结评估题 · 答案不进入内容优化/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "开始测量" }));
    await waitFor(() => expect(createMeasurementPlan).toHaveBeenCalledOnce());
    const input = vi.mocked(createMeasurementPlan).mock.calls[0][2];
    expect(input.measurements).toEqual([]);
    expect(input.bound_measurements?.[0].question).toEqual({
      question_set_id: "set-1",
      question_set_version_id: "version-2",
      question_id: "question-1",
      question_revision_id: "revision-2",
    });
    expect(input.bound_measurements?.[0]).not.toHaveProperty("purpose");
    expect(input.bound_measurements?.[0]).not.toHaveProperty("market");
    expect(JSON.stringify(input)).not.toContain("如何选择观测时间");
  });
});
