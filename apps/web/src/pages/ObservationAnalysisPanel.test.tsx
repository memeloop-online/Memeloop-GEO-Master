import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, useLocation } from "react-router-dom";
import * as api from "../api/observationAnalysis";
import type {
  ObservationAnalysisPage,
  ObservationAnalysisRevision,
  ObservationExtractionStatus,
} from "../api/observationAnalysis";
import { ObservationAnalysisPanel } from "./ObservationAnalysisPanel";
import i18n from "../i18n";
import {
  analysisFailureMessageKey,
  analysisUnverifiedMessageKey,
} from "../i18n/observationAnalysis";

vi.mock("../auth/AuthProvider", () => ({
  useAuth: () => ({
    session: { user: { id: "user" }, operator: { id: "operator" } },
  }),
}));

const source = { kind: "capture" as const, capture_id: "capture" };
const revision: ObservationAnalysisRevision = {
  request: {
    revision_id: "revision",
    target_id: "target",
    attempt_id: "attempt",
    source,
    source_sha256: "a".repeat(64),
    observed_at: "2026-01-01T00:00:00Z",
    parser_version: "parser",
    prompt_version: "prompt",
  },
  request_digest: "b".repeat(64),
  created_at: "2026-01-02T00:00:00Z",
  started_at: null,
  analyzed_at: null,
  state: "queued",
  result: null,
};

function CurrentRoute() {
  const location = useLocation();
  return (
    <output data-testid="current-route">
      {location.pathname}
      {location.search}
    </output>
  );
}

function setup(
  items: ObservationAnalysisRevision[] = [],
  eligible = true,
  canWrite = true,
  extraction: ObservationExtractionStatus | null = null,
  nextAfter: string | null = null,
  initialEntry = "/",
  active = true,
) {
  const page: ObservationAnalysisPage = {
    items,
    extraction,
    next_after: nextAfter,
    sources: eligible
      ? [
          {
            source,
            source_sha256: "a".repeat(64),
            observed_at: revision.request.observed_at,
          },
        ]
      : [],
  };
  const list = vi.spyOn(api, "listObservationAnalyses").mockResolvedValue(page);
  const create = vi
    .spyOn(api, "createObservationAnalysis")
    .mockResolvedValue(revision);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const panel = (isActive: boolean) => (
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[initialEntry]}>
          <CurrentRoute />
          <ObservationAnalysisPanel
            tenantId="tenant"
            projectId="project"
            targetId="target"
            attemptId="attempt"
            canWrite={canWrite}
            active={isActive}
          />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>
  );
  const view = render(panel(active));
  return {
    list,
    create,
    client,
    page,
    setActive: (value: boolean) => view.rerender(panel(value)),
  };
}

afterEach(async () => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
  await i18n.changeLanguage("zh-CN");
});

describe("saved response analysis", () => {
  it("defers hidden analysis reads and linked history pagination until activation", async () => {
    vi.useFakeTimers();
    const { list, create, setActive, client, page } = setup(
      [],
      true,
      false,
      null,
      "older",
      "/?target=target&attempt=attempt&analysisRevision=linked",
      false,
    );
    list.mockImplementation(async () => ({ ...page, next_after: null }));
    await act(async () => {
      client.setQueryData(
        [
          "observation-analyses",
          "user",
          "operator",
          "tenant",
          "project",
          "target",
          "attempt",
        ],
        { pages: [page], pageParams: [undefined] },
      );
    });
    await act(() => vi.advanceTimersByTimeAsync(9000));
    expect(list).not.toHaveBeenCalled();
    setActive(true);
    await act(() => vi.advanceTimersByTimeAsync(1));
    expect(list).toHaveBeenCalled();
    expect(create).not.toHaveBeenCalled();
    client.clear();
  });

  it("pauses pending analysis while hidden and reads persisted completion on return", async () => {
    vi.useFakeTimers();
    const { list, create, page, setActive, client } = setup([revision]);
    await act(() => vi.advanceTimersByTimeAsync(1));
    expect(list).toHaveBeenCalledOnce();
    await act(() => vi.advanceTimersByTimeAsync(3000));
    expect(list).toHaveBeenCalledTimes(2);
    setActive(false);
    const reads = list.mock.calls.length;
    list.mockResolvedValue({
      ...page,
      items: [
        {
          ...revision,
          state: "completed",
          result: {
            actual_model: "saved-model",
            candidate_json: null,
            prompt_tokens: 1,
            completion_tokens: 1,
            outcome: {
              status: "grounded",
              raw_answer: "Completed while this tab was hidden",
              citations: [],
              audit: {},
            },
          },
        },
      ],
    });
    await act(() => vi.advanceTimersByTimeAsync(9000));
    expect(list).toHaveBeenCalledTimes(reads);
    expect(screen.getByText("等待解析")).toBeInTheDocument();
    setActive(true);
    await act(() => vi.advanceTimersByTimeAsync(1));
    expect(list).toHaveBeenCalledTimes(reads + 1);
    expect(
      screen.getByText("Completed while this tab was hidden"),
    ).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(6000));
    expect(list).toHaveBeenCalledTimes(reads + 1);
    expect(create).not.toHaveBeenCalled();
    client.clear();
  });

  it("opens the exact linked revision from an older page instead of the current result", async () => {
    const current: ObservationAnalysisRevision = {
      ...revision,
      state: "completed",
      request: { ...revision.request, revision_id: "current" },
      result: {
        actual_model: "model",
        candidate_json: null,
        prompt_tokens: 1,
        completion_tokens: 1,
        outcome: {
          status: "grounded",
          raw_answer: "Current answer",
          citations: [],
          audit: {},
        },
      },
    };
    const linked: ObservationAnalysisRevision = {
      ...current,
      request: { ...current.request, revision_id: "linked" },
      result: {
        ...current.result!,
        outcome: {
          status: "grounded",
          raw_answer: "Linked historical answer",
          citations: [],
          audit: {},
        },
      },
    };
    const { list, page } = setup(
      [current],
      true,
      false,
      null,
      "older",
      "/?target=target&attempt=attempt&analysisRevision=linked",
    );
    list.mockImplementation(
      async (_tenant, _project, _target, _attempt, after) =>
        after ? { ...page, items: [linked], next_after: null } : page,
    );
    expect(await screen.findByText("Linked historical answer")).toBeVisible();
    expect(screen.getByText("Current answer")).not.toBeVisible();
    expect(list).toHaveBeenCalledWith(
      "tenant",
      "project",
      "target",
      "attempt",
      "older",
      expect.any(AbortSignal),
    );
  });

  it("reports a missing linked revision without substituting another saved answer", async () => {
    setup(
      [
        {
          ...revision,
          state: "completed",
          result: {
            actual_model: "model",
            candidate_json: null,
            prompt_tokens: 1,
            completion_tokens: 1,
            outcome: {
              status: "grounded",
              raw_answer: "Different saved answer",
              citations: [],
              audit: {},
            },
          },
        },
      ],
      true,
      false,
      null,
      null,
      "/?target=target&attempt=attempt&analysisRevision=missing",
    );
    expect(
      await screen.findByText("找不到链接指定的解析版本，请返回测量记录查看。"),
    ).toBeVisible();
    expect(
      screen.queryByText("Different saved answer"),
    ).not.toBeInTheDocument();
  });
  it("shows saved recommendations ahead of the full answer without another analysis request", async () => {
    const { create } = setup([
      {
        ...revision,
        state: "completed",
        result: {
          actual_model: "analysis-model",
          candidate_json: null,
          prompt_tokens: 1,
          completion_tokens: 1,
          outcome: {
            status: "grounded",
            raw_answer: "Try Sample restaurant for lunch.",
            citations: [],
            audit: {
              recommendations: {
                version: "geo.recommendations.v1",
                status: "verified",
                items: [
                  {
                    name: "Sample restaurant",
                    kind: "restaurant",
                    mention_order: 1,
                    rank: null,
                    reason: null,
                    evidence_quote: "Try Sample restaurant for lunch.",
                    citation_urls: [],
                  },
                ],
              },
            },
          },
        },
      },
    ]);
    const table = await screen.findByRole("table", { name: "推荐结果" });
    expect(table).toBeVisible();
    expect(screen.getByText("未明确排名")).toBeVisible();
    const answerHeading = screen.getByRole("heading", { name: "解析出的答案" });
    expect(
      table.compareDocumentPosition(answerHeading) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(create).not.toHaveBeenCalled();
  });

  it.each(["failed", "running"] as const)(
    "keeps an available answer ahead of a newer %s analysis",
    async (state) => {
      const grounded: ObservationAnalysisRevision = {
        ...revision,
        request: { ...revision.request, revision_id: "grounded-revision" },
        state: "completed",
        result: {
          actual_model: "saved-model",
          candidate_json: null,
          prompt_tokens: 1,
          completion_tokens: 1,
          outcome: {
            status: "grounded",
            raw_answer: "Available saved answer",
            citations: [],
            audit: {},
          },
        },
      };
      const newer: ObservationAnalysisRevision =
        state === "running"
          ? { ...revision, state: "running" }
          : {
              ...revision,
              state: "completed",
              result: {
                ...grounded.result!,
                outcome: { status: "failed", code: "model_timeout" },
              },
            };
      const older: ObservationAnalysisRevision = {
        ...grounded,
        request: {
          ...grounded.request,
          revision_id: "older-grounded-revision",
        },
        result: {
          ...grounded.result!,
          outcome: {
            status: "grounded",
            raw_answer: "Older saved answer",
            citations: [],
            audit: {},
          },
        },
      };
      const { create } = setup([newer, grounded, older]);
      expect(await screen.findByText("Available saved answer")).toBeVisible();
      expect(screen.getAllByText("Available saved answer")).toHaveLength(1);
      const history = screen.getByText("解析记录与操作").closest("details")!;
      expect(history.open).toBe(false);
      const newerStatus = screen.getByText(
        state === "running" ? "正在解析" : "解析超时。请稍后重新解析。",
      );
      expect(newerStatus).not.toBeVisible();
      expect(screen.getByText("Older saved answer")).not.toBeVisible();
      await userEvent.click(screen.getByText("解析记录与操作"));
      expect(newerStatus).toBeVisible();
      expect(screen.getByText("Older saved answer")).toBeVisible();
      expect(screen.getByRole("button", { name: "重新解析" })).toHaveProperty(
        "disabled",
        state === "running",
      );
      expect(create).not.toHaveBeenCalled();
    },
  );

  it("promotes an earlier available answer through existing history pagination", async () => {
    const { list, create, page } = setup(
      [
        {
          ...revision,
          state: "completed",
          result: {
            actual_model: "analysis-model",
            candidate_json: null,
            prompt_tokens: 0,
            completion_tokens: 0,
            outcome: { status: "failed", code: "model_timeout" },
          },
        },
      ],
      true,
      true,
      null,
      "earlier-page",
    );
    await screen.findByText("解析超时。请稍后重新解析。");
    list.mockResolvedValueOnce({
      ...page,
      next_after: null,
      items: [
        {
          ...revision,
          request: { ...revision.request, revision_id: "earlier-success" },
          state: "completed",
          result: {
            actual_model: "saved-model",
            candidate_json: null,
            prompt_tokens: 1,
            completion_tokens: 1,
            outcome: {
              status: "grounded",
              raw_answer: "Earlier saved answer",
              citations: [],
              audit: {},
            },
          },
        },
      ],
    });
    await userEvent.click(
      screen.getByRole("button", { name: "更早的解析记录" }),
    );
    expect(await screen.findByText("Earlier saved answer")).toBeVisible();
    expect(screen.getAllByText("Earlier saved answer")).toHaveLength(1);
    expect(list.mock.calls[1].slice(0, 5)).toEqual([
      "tenant",
      "project",
      "target",
      "attempt",
      "earlier-page",
    ]);
    expect(create).not.toHaveBeenCalled();
  });

  it.each(["zh-CN", "en"])(
    "shows interrupted automatic failure guidance and existing settings in %s",
    async (language) => {
      await i18n.changeLanguage(language);
      const { create } = setup([], true, true, {
        state: "interrupted",
        created_at: revision.created_at,
        started_at: revision.created_at,
        finished_at: revision.created_at,
        interruption: "inference_failed",
      });
      expect(
        await screen.findByText(
          i18n.t("extractionFailed", { ns: "observationAnalysis" }),
        ),
      ).toBeInTheDocument();
      expect(
        screen.getByRole("link", {
          name: i18n.t("settings", { ns: "observationAnalysis" }),
        }),
      ).toHaveAttribute("href", "/app/tenant/project/settings?tab=ai");
      expect(
        screen.getByRole("button", {
          name: i18n.t("parse", { ns: "observationAnalysis" }),
        }),
      ).toBeEnabled();
      expect(create).not.toHaveBeenCalled();
    },
  );

  it("keeps completed automatic analysis failure separate from verified answers", async () => {
    setup(
      [
        {
          ...revision,
          state: "completed",
          result: {
            actual_model: "synthetic-model",
            candidate_json: "untrusted candidate",
            prompt_tokens: 1,
            completion_tokens: 1,
            outcome: { status: "failed", code: "grounding_unavailable" },
          },
        },
      ],
      true,
      true,
      {
        state: "completed",
        created_at: revision.created_at,
        started_at: revision.created_at,
        finished_at: revision.created_at,
        interruption: null,
      },
    );
    expect(
      await screen.findByText(
        i18n.t("failureGrounding", { ns: "observationAnalysis" }),
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText("已与原始记录核对")).not.toBeInTheDocument();
    expect(screen.queryByText("untrusted candidate")).not.toBeInTheDocument();
  });

  it.each([
    ["queued", "等待解析"],
    ["running", "正在解析"],
    ["response_received", "正在整理回答"],
  ] as const)(
    "polls automatic %s without a revision and displays its completion",
    async (state, label) => {
      vi.useFakeTimers();
      const { list, create, page } = setup([], true, true, {
        state,
        created_at: revision.created_at,
        started_at: state === "queued" ? null : revision.created_at,
        finished_at: null,
        interruption: null,
      });
      await act(() => vi.advanceTimersByTimeAsync(1));
      expect(screen.getByText(label)).toBeInTheDocument();
      expect(screen.queryByText("尚未解析已存记录。")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "重新解析" })).toBeEnabled();
      list.mockResolvedValue({
        ...page,
        extraction: {
          ...page.extraction!,
          state: "completed",
          finished_at: revision.created_at,
        },
        items: [
          {
            ...revision,
            state: "completed",
            result: {
              actual_model: "synthetic-model",
              candidate_json: null,
              prompt_tokens: 1,
              completion_tokens: 1,
              outcome: {
                status: "grounded",
                raw_answer: "Automatically parsed answer",
                citations: [],
                audit: {},
              },
            },
          },
        ],
      });
      await act(() => vi.advanceTimersByTimeAsync(3000));
      expect(
        screen.getByText("Automatically parsed answer"),
      ).toBeInTheDocument();
      expect(screen.getByText("已与原始记录核对")).toBeInTheDocument();
      const reads = list.mock.calls.length;
      await act(() => vi.advanceTimersByTimeAsync(6000));
      expect(list).toHaveBeenCalledTimes(reads);
      expect(create).not.toHaveBeenCalled();
    },
  );

  it.each([null, "completed", "interrupted"] as const)(
    "stops polling inactive automatic status %s",
    async (state) => {
      vi.useFakeTimers();
      const { list, create } = setup(
        [],
        true,
        true,
        state
          ? {
              state,
              created_at: revision.created_at,
              started_at: revision.created_at,
              finished_at: revision.created_at,
              interruption:
                state === "interrupted" ? "inference_interrupted" : null,
            }
          : null,
      );
      await act(() => vi.advanceTimersByTimeAsync(1));
      expect(list).toHaveBeenCalledOnce();
      if (state === "interrupted") {
        expect(
          screen.getByText("解析中断。你可以重新解析已存回答。"),
        ).toBeInTheDocument();
      }
      await act(() => vi.advanceTimersByTimeAsync(6000));
      expect(list).toHaveBeenCalledOnce();
      expect(create).not.toHaveBeenCalled();
      expect(screen.queryByText("已与原始记录核对")).not.toBeInTheDocument();
    },
  );

  it.each([
    ["zh-CN", "配置解析模型", "重新解析"],
    ["en", "Configure analysis model", "Reanalyze"],
  ])(
    "keeps direct optional AI settings navigation available after analysis failure in %s",
    async (language, linkName, reanalyzeName) => {
      await i18n.changeLanguage(language);
      const { create } = setup([
        {
          ...revision,
          state: "completed",
          result: {
            actual_model: "received-model",
            candidate_json: null,
            prompt_tokens: 0,
            completion_tokens: 0,
            outcome: { status: "failed", code: "model_http_unauthorized" },
          },
        },
      ]);
      const reanalyze = await screen.findByRole("button", {
        name: reanalyzeName,
      });
      expect(reanalyze).toBeEnabled();
      const link = screen.getByRole("link", { name: linkName });
      expect(link).toHaveAttribute(
        "href",
        "/app/tenant/project/settings?tab=ai",
      );
      await userEvent.click(link);
      expect(screen.getByTestId("current-route")).toHaveTextContent(
        "/app/tenant/project/settings?tab=ai",
      );
      expect(create).not.toHaveBeenCalled();
    },
  );

  it.each([
    ["model_unconfigured", "failureConfiguration"],
    ["model_access_denied", "failureAccess"],
    ["model_http_unauthorized", "failureAccess"],
    ["model_http_forbidden", "failureAccess"],
    ["model_budget_exceeded", "failureBudget"],
    ["model_settings_unavailable", "failureUnavailable"],
    ["model_transport_failed", "failureUnavailable"],
    ["model_http_server_error", "failureUnavailable"],
    ["model_http_rate_limited", "failureRateLimit"],
    ["model_timeout", "failureTimeout"],
    ["model_http_timeout", "failureTimeout"],
    ["analysis_timeout", "failureTimeout"],
    ["model_cancelled", "failureInterrupted"],
    ["analysis_interrupted", "failureInterrupted"],
    ["model_request_invalid", "failureRequest"],
    ["model_http_rejected", "failureRequest"],
    ["model_response_invalid", "failureResponse"],
    ["model_response_too_large", "failureResponse"],
    ["analysis_result_invalid", "failureResponse"],
    ["analysis_source_unavailable", "failureSource"],
    ["grounding_unavailable", "failureGrounding"],
    ["model_failed", "failed"],
    ["private-upstream-message", "failed"],
    ["constructor", "failed"],
  ])("maps %s to a fixed translated explanation", (code, key) => {
    expect(analysisFailureMessageKey(code)).toBe(key);
    for (const lng of ["zh-CN", "en"]) {
      const message = i18n.t(key, { lng, ns: "observationAnalysis" });
      expect(message).not.toBe(key);
      expect(message).not.toContain(code);
    }
  });

  it.each(["zh-CN", "en"])(
    "shows specific failure guidance in %s without submitting again or hiding model metadata",
    async (language) => {
      await i18n.changeLanguage(language);
      const codes = [
        "model_unconfigured",
        "model_http_unauthorized",
        "model_http_server_error",
        "model_http_rate_limited",
        "model_timeout",
        "model_response_invalid",
        "analysis_source_unavailable",
        "grounding_unavailable",
        "private-upstream-message",
      ];
      const { create } = setup(
        codes.map((code, index) => ({
          ...revision,
          request: { ...revision.request, revision_id: `revision-${index}` },
          state: "completed",
          result: {
            actual_model: `received-model-${index}`,
            config_revision: 3,
            candidate_json: "private-candidate",
            prompt_tokens: 1,
            completion_tokens: 1,
            outcome: { status: "failed", code },
          },
        })),
      );
      for (const [index, code] of codes.entries()) {
        expect(
          await screen.findByText(
            i18n.t(analysisFailureMessageKey(code), {
              ns: "observationAnalysis",
            }),
          ),
        ).toBeInTheDocument();
        expect(
          screen.getByText(
            i18n.t("model", {
              ns: "observationAnalysis",
              value: `received-model-${index}`,
            }),
          ),
        ).toBeInTheDocument();
        expect(screen.queryByText(code)).not.toBeInTheDocument();
      }
      expect(screen.queryByText("private-candidate")).not.toBeInTheDocument();
      expect(create).not.toHaveBeenCalled();
    },
  );

  it.each([
    "model_output_too_large",
    "model_output_incomplete",
    "model_output_tool_calls",
    "model_output_invalid",
  ])(
    "distinguishes unusable model output %s from source disagreement",
    (reason) => {
      expect(analysisUnverifiedMessageKey(reason)).toBe("failureResponse");
      expect(analysisUnverifiedMessageKey("grounding_failed")).toBe(
        "unverified",
      );
      expect(analysisUnverifiedMessageKey("private-provider-reason")).toBe(
        "unverified",
      );
    },
  );

  it("only offers reanalysis when a saved source exists", async () => {
    const { create } = setup([], false);
    expect(
      await screen.findByText("没有可解析的已存原始记录。"),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "重新解析" }),
    ).not.toBeInTheDocument();
    expect(create).not.toHaveBeenCalled();
    expect(screen.getByRole("link", { name: "配置解析模型" })).toHaveAttribute(
      "href",
      "/app/tenant/project/settings?tab=ai",
    );
  });

  it("preserves the same submission key on transport failure and shows persisted pending state", async () => {
    const { create, list } = setup();
    create.mockRejectedValueOnce(new Error("private-provider-error"));
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "重新解析" }));
    expect(await screen.findByText(/解析提交尚未确认/)).toBeInTheDocument();
    expect(
      screen.queryByText("private-provider-error"),
    ).not.toBeInTheDocument();
    list.mockResolvedValue({
      items: [revision],
      sources: [
        {
          source,
          source_sha256: "a".repeat(64),
          observed_at: revision.request.observed_at,
        },
      ],
      next_after: null,
    });
    await user.click(screen.getByRole("button", { name: "重试提交解析" }));
    expect(await screen.findByText("等待解析")).toBeInTheDocument();
    expect(create).toHaveBeenCalledTimes(2);
    expect(create.mock.calls[0]).toEqual(create.mock.calls[1]);
    expect(create.mock.calls[0].slice(0, 4)).toEqual([
      "tenant",
      "project",
      "target",
      "attempt",
    ]);
    expect(screen.getByRole("button", { name: "重新解析" })).toBeDisabled();
    expect(screen.queryByText(/原测量状态保持不变/)).not.toBeInTheDocument();
  });

  it("renders only verified answer text and safe citations with actual model and source provenance", async () => {
    const { create } = setup(
      [
        {
          ...revision,
          state: "completed",
          analyzed_at: "2026-01-02T00:01:00Z",
          result: {
            actual_model: "example-analysis-model",
            candidate_json: null,
            prompt_tokens: 1,
            completion_tokens: 1,
            outcome: {
              status: "grounded",
              raw_answer: "Line one\n<script>unsafe()</script>",
              citations: [
                "javascript:unsafe()",
                "https://example.test/evidence",
                "https://example.test/evidence",
              ],
              audit: {
                refs: [{ quote: "Original evidence quote", pointer: "/text" }],
              },
            },
          },
        },
      ],
      true,
      false,
    );
    expect(await screen.findByText("已与原始记录核对")).toBeInTheDocument();
    expect(
      screen.getByText("实际解析模型：example-analysis-model"),
    ).not.toBeVisible();
    expect(screen.getByText(/原始记录时间/)).not.toBeVisible();
    expect(screen.getByText(/解析时间/)).not.toBeVisible();
    expect(screen.getByText(/<script>unsafe/)).toBeInTheDocument();
    expect(screen.getAllByText(/<script>unsafe/)).toHaveLength(1);
    expect(screen.getByText("回答信息").closest("details")).not.toHaveAttribute(
      "open",
    );
    expect(document.querySelector("script")).toBeNull();
    expect(
      screen.getAllByRole("link", { name: "https://example.test/evidence" }),
    ).toHaveLength(1);
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(screen.getByText(/Original evidence quote/)).not.toBeVisible();
    await userEvent.click(screen.getByText("回答信息"));
    expect(
      screen.getByText("实际解析模型：example-analysis-model"),
    ).toBeVisible();
    expect(screen.getByText(/原始记录时间/)).toBeVisible();
    expect(screen.getByText(/解析时间/)).toBeVisible();
    await userEvent.click(screen.getByText("查看原文定位依据"));
    expect(screen.getByText(/Original evidence quote/)).toBeVisible();
    await userEvent.click(screen.getByText("解析记录与操作"));
    expect(
      screen.queryByRole("button", { name: "重新解析" }),
    ).not.toBeInTheDocument();
    expect(create).not.toHaveBeenCalled();
  });

  it.each(["unverified", "failed"] as const)(
    "never shows candidate answer for %s",
    async (status) => {
      await i18n.changeLanguage("en");
      setup([
        {
          ...revision,
          state: "completed",
          result: {
            actual_model: "example-model",
            candidate_json: '{"answer":"untrusted candidate"}',
            prompt_tokens: 1,
            completion_tokens: 1,
            outcome:
              status === "unverified"
                ? { status, reason: "grounding_failed" }
                : { status, code: "unavailable" },
          },
        },
      ]);
      expect(
        await screen.findByRole("button", { name: "Reanalyze" }),
      ).toBeEnabled();
      expect(screen.queryByText(/untrusted candidate/)).not.toBeInTheDocument();
      expect(
        screen.queryByText("Checked against the source record"),
      ).not.toBeInTheDocument();
      expect(
        screen.getByText(
          status === "failed"
            ? /This analysis did not finish/
            : /could not be verified/,
        ),
      ).toBeInTheDocument();
    },
  );

  it("keeps viewer access read-only", async () => {
    const { list, create } = setup([], true, false);
    await waitFor(() => expect(list).toHaveBeenCalled());
    expect(screen.getByText(/没有重新解析权限/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "重新解析" }),
    ).not.toBeInTheDocument();
    expect(create).not.toHaveBeenCalled();
  });

  it("refreshes a persisted running analysis to terminal failure without resubmitting", async () => {
    const { list, create } = setup([{ ...revision, state: "running" }]);
    expect(await screen.findByText("正在解析")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重新解析" })).toBeDisabled();
    list.mockResolvedValue({
      items: [
        {
          ...revision,
          state: "completed",
          analyzed_at: "2026-01-02T00:01:00Z",
          result: {
            actual_model: null,
            candidate_json: null,
            prompt_tokens: 0,
            completion_tokens: 0,
            outcome: { status: "failed", code: "interrupted" },
          },
        },
      ],
      next_after: null,
      sources: [
        {
          source,
          source_sha256: "a".repeat(64),
          observed_at: revision.request.observed_at,
        },
      ],
    });
    await userEvent.click(screen.getByRole("button", { name: "刷新解析记录" }));
    expect(await screen.findByText(/这次解析未完成/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重新解析" })).toBeEnabled();
    expect(create).not.toHaveBeenCalled();
  });
});
