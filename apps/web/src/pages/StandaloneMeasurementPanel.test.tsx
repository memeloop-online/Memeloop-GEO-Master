import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { StandaloneMeasurementPanel } from "./StandaloneMeasurementPanel";
import {
  createMeasurementPlan,
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
      isError: false,
      refetch: state.refetch,
    },
  }),
}));
vi.mock("../api/channelJobs", () => ({
  createMeasurementPlan: vi.fn(),
  getMeasurementPlan: vi.fn(),
  getMeasurementOptions: vi.fn(),
  listMeasurementPlans: vi.fn(),
}));
vi.mock("../api/questions", () => ({
  listAllQuestionSets: vi.fn(),
  listAllQuestionSetVersions: vi.fn(),
  getQuestionSetVersion: vi.fn(),
}));
vi.mock("./ChannelJobsPage", () => ({
  PlannedTarget: ({ target }: { target: { target_id: string } }) => (
    <div>执行证据 {target.target_id}</div>
  ),
}));
const plan = {
  plan_id: "measurement-1",
  project_id: "project-1",
  title: "自定义问题测量",
  created_at: "2026-10-01T00:00:00Z",
  targets: [],
};
function renderPanel(canWrite = true) {
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
          <StandaloneMeasurementPanel
            tenantId="tenant-1"
            projectId="project-1"
            canWrite={canWrite}
          />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
}
beforeEach(() => {
  vi.clearAllMocks();
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
    expect(
      screen.getByRole("link", { name: "登录或连接 Kimi 账号" }),
    ).toHaveAttribute("href", "/app/tenant-1/project-1/channels/connect");
    expect(screen.getByRole("button", { name: "开始测量" })).toBeDisabled();
    expect(
      screen.getByRole("option", { name: "Kimi 账号 · 需要重新连接" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("link", { name: "重新连接已有账号" }),
    ).toBeInTheDocument();
    expect(getMeasurementOptions).not.toHaveBeenCalled();
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
    renderPanel(false);
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
