import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import * as api from "../api/observationAnalysis";
import type { ObservationAnalysisRevision } from "../api/observationAnalysis";
import { ObservationAnalysisPanel } from "./ObservationAnalysisPanel";
import i18n from "../i18n";

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

function setup(
  items: ObservationAnalysisRevision[] = [],
  eligible = true,
  canWrite = true,
) {
  const list = vi.spyOn(api, "listObservationAnalyses").mockResolvedValue({
    items,
    next_after: null,
    sources: eligible
      ? [
          {
            source,
            source_sha256: "a".repeat(64),
            observed_at: revision.request.observed_at,
          },
        ]
      : [],
  });
  const create = vi
    .spyOn(api, "createObservationAnalysis")
    .mockResolvedValue(revision);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <ObservationAnalysisPanel
            tenantId="tenant"
            projectId="project"
            targetId="target"
            attemptId="attempt"
            canWrite={canWrite}
          />
        </MemoryRouter>
      </QueryClientProvider>
    </FluentProvider>,
  );
  return { list, create, client };
}

afterEach(async () => {
  cleanup();
  vi.restoreAllMocks();
  await i18n.changeLanguage("zh-CN");
});

describe("saved response analysis", () => {
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
    expect(screen.getByText(/原测量状态保持不变/)).toBeInTheDocument();
  });

  it("renders only verified answer text and safe citations with actual model and source provenance", async () => {
    setup([
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
    ]);
    expect(await screen.findByText("已与原始记录核对")).toBeInTheDocument();
    expect(
      screen.getByText("实际解析模型：example-analysis-model"),
    ).toBeInTheDocument();
    expect(screen.getByText(/原始记录时间/)).toBeInTheDocument();
    expect(screen.getByText(/解析时间/)).toBeInTheDocument();
    expect(screen.getByText(/<script>unsafe/)).toBeInTheDocument();
    expect(document.querySelector("script")).toBeNull();
    expect(
      screen.getAllByRole("link", { name: "https://example.test/evidence" }),
    ).toHaveLength(1);
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(screen.getByText(/Original evidence quote/)).toBeInTheDocument();
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
