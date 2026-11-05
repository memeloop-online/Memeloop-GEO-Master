import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { AgentMessage } from "../api/agent";
import * as plans from "../api/channelJobs";
import * as analyses from "../api/observationAnalysis";
import * as projects from "../api/projects";
import i18n from "../i18n";
import {
  AgentMeasurementResults,
  measurementReferencesByMessage,
} from "./AgentMeasurementResults";

const native = vi.hoisted(() => ({ text: "", setText: vi.fn() }));
vi.mock("@memeloop/react-ui/chat", () => ({
  useAui: () => ({
    thread: {
      composer: () => ({
        getState: () => ({ text: native.text }),
        setText: native.setText,
      }),
    },
  }),
}));
vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: { user: { id: "user-a" }, operator: { id: "operator-a" } },
  }),
}));
const planId = "11111111-1111-4111-8111-111111111111",
  targetId = "22222222-2222-4222-8222-222222222222";
const reference = { plan_id: planId, target_id: targetId };
const target: plans.ChannelTarget = {
  target_id: targetId,
  input: {
    kind: "measure",
    account_id: "account-a",
    provider: "Sample AI",
    model: "Sample",
    surface: "consumer_web",
    search_mode: "web_search",
    protocol_version: "sample",
    question_set_version: "ad_hoc.v1",
    question: "Which sample product?",
    market: "CN",
    language: "zh-CN",
    scheduled_at: "2026-11-01T00:00:00Z",
    sample_ordinal: 1,
  },
};
const plan: plans.StandaloneMeasurementPlan = {
  plan_id: planId,
  project_id: "project-a",
  title: "Sample measurement",
  created_at: "2026-11-01T00:00:00Z",
  targets: [target],
};
function message(
  id: string,
  metadata: unknown,
  role: AgentMessage["role"] = "assistant",
): AgentMessage {
  return {
    id,
    metadata,
    role,
    conversation_id: "conversation-a",
    turn_id: "turn-a",
    content: `measurement_plans: ${JSON.stringify([reference])}`,
    attachments: [],
    sequence: id === "answer-b" ? 2 : 1,
    created_at: "2026-11-01T00:00:00Z",
  };
}
function setup() {
  vi.spyOn(plans, "getMeasurementPlan").mockResolvedValue(plan);
  vi.spyOn(plans, "getChannelTarget").mockResolvedValue({
    target,
    attempts: [],
  });
  vi.spyOn(analyses, "listObservationAnalyses").mockResolvedValue({
    items: [],
    next_after: null,
    sources: [],
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(() =>
      Promise.resolve(
        new Response(
          JSON.stringify({
            id: "project-a",
            settings: { measurement_targets: [] },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      ),
    ),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const focus = vi.fn();
  const renderResult = (projectId = "project-a") => (
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <AgentMeasurementResults
            tenantId="tenant-a"
            projectId={projectId}
            references={[reference, reference]}
            onFocus={focus}
          />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>
  );
  return { client, focus, renderResult };
}
afterEach(async () => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  native.text = "";
  native.setText.mockReset();
  await i18n.changeLanguage("zh-CN");
});

describe("saved assistant measurement references", () => {
  it("uses exact API assistant metadata identities, rejects malformed metadata and prose, and deduplicates each turn", () => {
    const mapped = measurementReferencesByMessage([
      message("prose-only", {}),
      message("user", { measurement_plans: [reference] }, "user"),
      message("tool", { measurement_plans: [reference] }, "tool"),
      message("invalid", {
        measurement_plans: [
          { ...reference, target_id: "not-a-uuid" },
          null,
          { plan_id: "invalid", target_id: targetId },
        ],
      }),
      message("answer-a", { measurement_plans: [reference, reference] }),
      message("answer-b", { measurement_plans: [reference] }),
      {
        ...message("answer-c", { measurement_plans: [reference] }),
        turn_id: "turn-b",
      },
    ]);
    expect([...mapped]).toEqual([
      ["answer-a", [reference]],
      ["answer-c", [reference]],
    ]);
  });

  it("shows loading and retry without changing any business data", async () => {
    const { renderResult } = setup();
    vi.mocked(plans.getMeasurementPlan).mockRejectedValueOnce(
      new Error("unavailable"),
    );
    render(renderResult());
    expect(screen.getByText("正在读取测量结果")).toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button", { name: "重新读取" }));
    expect(
      await screen.findByRole("heading", { name: "Which sample product?" }),
    ).toBeInTheDocument();
    expect(plans.getMeasurementPlan).toHaveBeenCalledTimes(2);
    expect(plans.getMeasurementPlan).toHaveBeenLastCalledWith(
      "tenant-a",
      "project-a",
      planId,
    );
  });

  it.each(["zh-CN", "en"])(
    "reuses the result view and fills only an empty native draft without navigation or sending (%s)",
    async (language) => {
      await i18n.changeLanguage(language);
      const { renderResult, focus } = setup();
      render(renderResult());
      const discuss = await screen.findByRole("button", {
        name: i18n.t("agentMeasurementResults:discuss"),
      });
      expect(
        screen.queryByRole("link", {
          name: i18n.t("measurementOutcome:discuss"),
        }),
      ).not.toBeInTheDocument();
      fireEvent.click(discuss);
      expect(native.setText).toHaveBeenLastCalledWith(
        i18n.t("measurementPlanChat:request", { planId }),
      );
      native.setText.mockClear();
      native.text = "My existing draft";
      fireEvent.click(discuss);
      expect(native.setText).not.toHaveBeenCalled();
      expect(focus).toHaveBeenCalledTimes(2);
      expect(
        screen.getByText(i18n.t("agentMeasurementResults:preserved")),
      ).toHaveAttribute("role", "status");
      expect(plans.getMeasurementPlan).toHaveBeenCalledTimes(1);
    },
  );

  it("isolates plan reads by project and reads back updated project focus", async () => {
    const { renderResult, client } = setup();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const { rerender } = render(renderResult());
    await screen.findByRole("heading", { name: "Which sample product?" });
    expect(invalidate).toHaveBeenCalledWith({
      queryKey: projects.projectQueryKeys.detail({
        userId: "user-a",
        operatorId: "operator-a",
        tenantId: "tenant-a",
        projectId: "project-a",
      }),
    });
    rerender(renderResult("project-b"));
    await waitFor(() =>
      expect(plans.getMeasurementPlan).toHaveBeenLastCalledWith(
        "tenant-a",
        "project-b",
        planId,
      ),
    );
  });

  it("updates the same result card from pending to saved evidence through existing read queries", async () => {
    const { renderResult, client } = setup();
    const date = "2026-11-01T00:00:00Z";
    const attempt: plans.ChannelAttempt = {
      attempt_id: "attempt-a",
      target_id: targetId,
      claimed_at: date,
      received_at: null,
      outcome: null,
    };
    vi.mocked(plans.getChannelTarget).mockResolvedValue({
      target,
      attempts: [attempt],
    });
    render(renderResult());
    expect(await screen.findByText("正在采集或解析回答。")).toBeInTheDocument();
    vi.mocked(plans.getChannelTarget).mockResolvedValue({
      target,
      attempts: [
        {
          ...attempt,
          received_at: date,
          outcome: {
            status: "observed",
            detail: "saved",
            occurred_at: date,
            raw_answer: "The saved answer.",
            citations: ["https://example.org/source"],
            public_url: null,
            screenshot_ref: null,
            connector_version: null,
            runner_evidence: [],
            fixture: false,
          },
        },
      ],
    });
    await client.invalidateQueries({ queryKey: ["channel-target"] });
    expect(
      await screen.findByRole("link", { name: "https://example.org/source" }),
    ).toBeInTheDocument();
    expect(
      screen.getAllByRole("region", { name: "对话中的测量结果" }),
    ).toHaveLength(1);
    expect(plans.getMeasurementPlan).toHaveBeenCalledTimes(1);
  });
});
