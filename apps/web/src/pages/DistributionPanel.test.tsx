import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type {
  DistributionManifest,
  DistributionTarget,
  DistributionTargetStatus,
} from "../api/distribution";
import { DistributionPanel } from "./DistributionPanel";

const manifest: DistributionManifest = {
  manifest_id: "manifest-1",
  project_id: "project-1",
  cycle_id: "cycle-1",
  revision: 1,
  document_manifest_id: "document-manifest-1",
  document_manifest_revision: 2,
  content_execution_id: "execution-1",
  content_handoff_id: "handoff-1",
  platform_scope: [
    {
      platform_id: "zhihu",
      placement_slot: "primary",
      capability_version: "unverified-v1",
      supported_formats: [],
      unavailable_reason: "connector_unverified",
      fixture: false,
    },
  ],
  document_roster: [
    {
      document_item_id: "document-1",
      document_key: "overview",
      content_type: "article",
      status: "ready",
      reason: null,
      content_revision_id: "revision-1",
    },
  ],
  input_hash: "hash",
  expected_count: 3,
  sealed_at: "2026-10-03T00:00:00Z",
  expansion_cursor: 2,
  complete: false,
};

const target: DistributionTarget = {
  target_id: "target-1",
  manifest_id: manifest.manifest_id,
  ordinal: 0,
  document_item_id: "document-1",
  content_revision_id: "revision-1",
  platform_id: "zhihu",
  placement_slot: "primary",
  variant_id: "variant-1",
  account_id: "account-1",
  publication_intent_id: "intent-1",
  status: "ready",
  reason: null,
  version: 1,
};

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function mockApi({
  initialManifest = manifest,
  role = "member",
  manifestError = false,
  firstStatus = "deferred",
  nextCursor = 0,
  originalTarget = "original-target",
  referenceError = false,
  noIntent = false,
  reusedStatus = "reused_unknown",
}: {
  initialManifest?: DistributionManifest | null;
  role?: string;
  manifestError?: boolean;
  firstStatus?: DistributionTargetStatus;
  nextCursor?: number;
  originalTarget?: string | null;
  referenceError?: boolean;
  noIntent?: boolean;
  reusedStatus?: DistributionTargetStatus;
} = {}) {
  let current = initialManifest;
  const requests: Array<{
    path: string;
    method: string;
    url: URL;
    body: unknown;
  }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(request), "http://localhost");
      const path = url.pathname;
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : undefined;
      requests.push({ path, method, url, body });
      if (path.endsWith("/auth/session"))
        return Promise.resolve(
          response({
            user: {
              id: "user-1",
              login_name: "user@example.test",
              display_name: "User",
            },
            operator: {
              id: "operator-1",
              slug: "operator",
              display_name: "Operator",
            },
            memberships: [
              {
                tenant_id: "tenant-1",
                tenant_slug: "tenant",
                tenant_display_name: "Tenant",
                role,
              },
            ],
            expires_at: "2026-10-03T00:00:00Z",
            csrf_token: "csrf-test",
          }),
        );
      if (path.endsWith("/cycles/cycle-2/distribution-manifest"))
        return Promise.resolve(
          response({ code: "not_found", message: "not frozen" }, 404),
        );
      if (path.endsWith("/cycles/cycle-1/distribution-manifest")) {
        if (method === "POST") {
          current = { ...manifest, expansion_cursor: 1 };
          return Promise.resolve(response(current, 202));
        }
        return Promise.resolve(
          current
            ? response(current)
            : response({ code: "not_found", message: "not frozen" }, 404),
        );
      }
      if (path.endsWith("/distribution-manifests/manifest-1/resume")) {
        current = { ...manifest, expansion_cursor: 3, complete: true };
        return Promise.resolve(response(current, 202));
      }
      if (path.endsWith("/distribution-manifests/manifest-1/targets")) {
        const ordinal = url.searchParams.get("after_ordinal");
        if (ordinal === String(nextCursor))
          return Promise.resolve(
            response({
              manifest_id: manifest.manifest_id,
              rows: [
                {
                  ...target,
                  target_id: "target-2",
                  ordinal: 1,
                  status: reusedStatus,
                  publication_intent_id: "prior-intent",
                },
              ],
              next_ordinal: null,
              expected_count: 3,
            }),
          );
        return Promise.resolve(
          response({
            manifest_id: manifest.manifest_id,
            rows: current?.expansion_cursor
              ? [
                  {
                    ...target,
                    status: firstStatus,
                    publication_intent_id: noIntent
                      ? null
                      : target.publication_intent_id,
                    reason: "account_unassigned",
                  },
                ]
              : [],
            next_ordinal:
              current && current.expansion_cursor > 1 ? nextCursor : null,
            expected_count: 3,
          }),
        );
      }
      if (path.endsWith("/distribution-manifests/manifest-1"))
        return Promise.resolve(
          manifestError
            ? response(
                { code: "unavailable", message: "detail unavailable" },
                503,
              )
            : response(current),
        );
      if (path.endsWith("/publication-target"))
        return Promise.resolve(
          referenceError
            ? response(
                { code: "unavailable", message: "lookup unavailable" },
                503,
              )
            : response(
                originalTarget
                  ? {
                      distribution_target_id: path.includes("/target-2/")
                        ? "target-2"
                        : "target-1",
                      publication_intent_id: path.includes("/target-2/")
                        ? "prior-intent"
                        : "intent-1",
                      channel_target_id: originalTarget,
                    }
                  : null,
              ),
        );
      if (path.endsWith(`/channel-targets/${originalTarget}`))
        return Promise.resolve(
          response({
            target: {
              target_id: originalTarget,
              input: { kind: "publish", title: "generated", platform: "zhihu" },
            },
            attempts: [
              {
                attempt_id: "attempt-1",
                target_id: originalTarget,
                claimed_at: "2026-10-01T00:00:00Z",
                received_at: null,
                outcome: null,
              },
            ],
          }),
        );
      if (
        path.endsWith(`/channel-targets/${originalTarget}/publication-lookup`)
      )
        return Promise.resolve(
          response({
            target_id: originalTarget,
            attempt_id: "attempt-1",
            job: null,
            observations: [],
            next_before: null,
          }),
        );
      return Promise.resolve(
        response({ code: "not_found", message: "not found" }, 404),
      );
    }),
  );
  return requests;
}

function renderPanel(canWrite = true, cycleId = "cycle-1") {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });
  const panel = (currentCycleId: string) => (
    <QueryClientProvider client={queryClient}>
      <FluentProvider theme={webLightTheme}>
        <AuthProvider>
          <DistributionPanel
            tenantId="tenant-1"
            projectId="project-1"
            cycleId={currentCycleId}
            canWrite={canWrite}
          />
        </AuthProvider>
      </FluentProvider>
    </QueryClientProvider>
  );
  const result = render(panel(cycleId));
  return {
    ...result,
    rerenderCycle: (nextCycleId: string) => result.rerender(panel(nextCycleId)),
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
});

describe("formal distribution coverage", () => {
  it("opens generated publication attempts on demand and reads lookup without sending", async () => {
    const requests = mockApi({ firstStatus: "ready" });
    const user = userEvent.setup();
    renderPanel();
    const button = await screen.findByRole("button", {
      name: "查看执行记录与查回",
    });
    expect(
      requests.some((item) => item.path.endsWith("/channel-targets/target-1")),
    ).toBe(false);
    await user.click(button);
    expect(
      await screen.findByText(/原发送尝试 attempt-1 · 结果未知/),
    ).toBeInTheDocument();
    expect(await screen.findByText(/尚未安排自动查回/)).toBeInTheDocument();
    const lookup = requests.filter((item) =>
      item.path.endsWith("/publication-lookup"),
    );
    expect(lookup).toHaveLength(1);
    expect(lookup[0].path).toContain(
      "/channel-targets/original-target/publication-lookup",
    );
    expect(
      requests.some((item) => item.path.endsWith("/channel-targets/target-1")),
    ).toBe(false);
    expect(lookup[0].url.searchParams.get("tenant_id")).toBe("tenant-1");
    expect(requests.every((item) => item.method === "GET")).toBe(true);
  });
  it.each(["reused_unknown", "reused_verified"] as const)(
    "resolves a %s cell through the original intent from an earlier cycle",
    async (reusedStatus) => {
      const requests = mockApi({ reusedStatus });
      const user = userEvent.setup();
      renderPanel();
      await screen.findByText(/尚未展开 1 项/);
      await user.click(screen.getByRole("button", { name: "下一页" }));
      await screen.findByText(
        reusedStatus === "reused_unknown"
          ? "复用既有未知结果，禁止重发"
          : "复用已有验证记录",
      );
      await user.click(
        screen.getByRole("button", { name: "查看执行记录与查回" }),
      );
      expect(
        await screen.findByText(/原发送尝试 attempt-1 · 结果未知/),
      ).toBeInTheDocument();
      expect(
        requests.some((item) =>
          item.path.endsWith("/targets/target-2/publication-target"),
        ),
      ).toBe(true);
      expect(
        requests.some((item) =>
          item.path.endsWith("/channel-targets/target-2"),
        ),
      ).toBe(false);
      expect(
        requests.some((item) =>
          item.path.endsWith(
            "/channel-targets/original-target/publication-lookup",
          ),
        ),
      ).toBe(true);
      expect(requests.every((item) => item.method === "GET")).toBe(true);
    },
  );

  it("shows absent intent and lookup failure without guessing a channel target", async () => {
    const user = userEvent.setup();
    const requests = mockApi({
      firstStatus: "ready",
      noIntent: true,
      originalTarget: null,
    });
    renderPanel();
    await user.click(
      await screen.findByRole("button", { name: "查看执行记录与查回" }),
    );
    expect(
      await screen.findByText(/尚无发布意图或原发送目标/),
    ).toBeInTheDocument();
    expect(
      requests.some((item) => item.path.includes("/channel-targets/")),
    ).toBe(false);
  });

  it("surfaces an unavailable original-target lookup without reading a guessed target", async () => {
    const requests = mockApi({ firstStatus: "ready", referenceError: true });
    const user = userEvent.setup();
    renderPanel();
    await user.click(
      await screen.findByRole("button", { name: "查看执行记录与查回" }),
    );
    expect(await screen.findByText("原发送目标无法读取")).toBeInTheDocument();
    expect(
      requests.some((item) => item.path.includes("/channel-targets/")),
    ).toBe(false);
  });
  it("keeps expanded, unexpanded and deferred cells distinct from publication", async () => {
    const requests = mockApi();
    const user = userEvent.setup();
    renderPanel();
    expect(await screen.findByText(/尚未展开 1 项/)).toBeInTheDocument();
    expect(screen.getByText(/原因：account_unassigned/)).toBeInTheDocument();
    expect(screen.queryByText("已发布（未公开验证）")).not.toBeInTheDocument();
    expect(
      screen.getByText(/登记或排队不等于发送、发布或公开验证/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "下一页" }));
    const second = await screen.findByText(/复用既有未知结果，禁止重发/);
    expect(
      within(second.closest(".channel-job-target") as HTMLElement).getByText(
        /不会另建新意图盲目重发/,
      ),
    ).toBeInTheDocument();
    expect(
      requests
        .find((item) => item.url.searchParams.get("after_ordinal") === "0")
        ?.url.searchParams.get("limit"),
    ).toBe("64");
    expect(
      requests
        .find((item) =>
          item.path.endsWith("/cycles/cycle-1/distribution-manifest"),
        )
        ?.url.searchParams.get("tenant_id"),
    ).toBe("tenant-1");
    expect(
      requests.some((item) =>
        item.path.endsWith("/distribution-manifests/manifest-1"),
      ),
    ).toBe(true);
    await user.click(screen.getByRole("button", { name: "上一页" }));
    expect(
      await screen.findByText(/原因：account_unassigned/),
    ).toBeInTheDocument();
  });

  it("freezes without client-supplied contents and resumes bounded expansion", async () => {
    const requests = mockApi({ initialManifest: null });
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText("尚未冻结正式分发清单");
    await user.click(screen.getByRole("button", { name: "冻结正式分发清单" }));
    expect(await screen.findByText(/尚未展开 2 项/)).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "继续展开／检查延后项" }),
    );
    expect(await screen.findByText(/尚未展开 0 项/)).toBeInTheDocument();
    expect(screen.getByText(/覆盖展开完成/)).toBeInTheDocument();
    const posts = requests.filter((item) => item.method === "POST");
    expect(posts.map((item) => item.path)).toEqual([
      "/api/v1/projects/project-1/cycles/cycle-1/distribution-manifest",
      "/api/v1/projects/project-1/distribution-manifests/manifest-1/resume",
    ]);
    expect(posts.every((item) => item.body === undefined)).toBe(true);
    expect(
      posts.every((item) => !item.url.searchParams.has("after_ordinal")),
    ).toBe(true);
    expect(
      posts.every(
        (item) =>
          item.url.searchParams.get("tenant_id") === "tenant-1" &&
          item.url.searchParams.get("project_id") === "project-1",
      ),
    ).toBe(true);
  });

  it("rescans the visible later page and resets its cursor when the cycle changes", async () => {
    const requests = mockApi({ nextCursor: 256 });
    const user = userEvent.setup();
    const view = renderPanel();
    await screen.findByText(/尚未展开 1 项/);
    await user.click(screen.getByRole("button", { name: "下一页" }));
    await screen.findByText(/复用既有未知结果，禁止重发/);
    await user.click(
      screen.getByRole("button", {
        name: "继续展开／复查当前页起的延后项",
      }),
    );
    await waitFor(() => {
      const post = requests.find(
        (item) =>
          item.method === "POST" &&
          item.path.endsWith("/distribution-manifests/manifest-1/resume"),
      );
      expect(post?.url.searchParams.get("after_ordinal")).toBe("256");
      expect(post?.body).toBeUndefined();
    });
    view.rerenderCycle("cycle-2");
    expect(await screen.findByText("尚未冻结正式分发清单")).toBeInTheDocument();
    view.rerenderCycle("cycle-1");
    await screen.findByText(/尚未展开 0 项/);
    expect(screen.getByText("第 1 页")).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "继续展开／检查延后项" }),
    );
    await waitFor(() => {
      const posts = requests.filter(
        (item) =>
          item.method === "POST" &&
          item.path.endsWith("/distribution-manifests/manifest-1/resume"),
      );
      expect(posts).toHaveLength(2);
      expect(posts[1].url.searchParams.has("after_ordinal")).toBe(false);
    });
  });

  it("shows a missing manifest without offering write controls to a viewer", async () => {
    const requests = mockApi({ initialManifest: null, role: "viewer" });
    renderPanel(false);
    expect(await screen.findByText("尚未冻结正式分发清单")).toBeInTheDocument();
    expect(screen.getByText(/当前角色仅可查看/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "冻结正式分发清单" }),
    ).not.toBeInTheDocument();
    expect(requests.some((item) => item.method === "POST")).toBe(false);
  });

  it("allows detail-load failure to be retried without hiding the frozen denominator", async () => {
    mockApi({ manifestError: true });
    renderPanel(false);
    expect(await screen.findByText(/尚未展开 1 项/)).toBeInTheDocument();
    expect(await screen.findByText("清单详情无法读取")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "继续展开／检查延后项" }),
    ).not.toBeInTheDocument();
  });

  it("does not call POST while just reading coverage", async () => {
    const requests = mockApi();
    renderPanel(false);
    await waitFor(() =>
      expect(screen.getByText(/尚未展开 1 项/)).toBeInTheDocument(),
    );
    expect(requests.every((item) => item.method === "GET")).toBe(true);
  });

  it.each([
    ["blocked", "已阻断"],
    ["not_applicable", "不适用"],
    ["ready", "就绪（非已发布）"],
    ["reused_unknown", "复用既有未知结果，禁止重发"],
  ] as const)(
    "renders %s as %s without claiming publication",
    async (status, label) => {
      mockApi({ firstStatus: status });
      renderPanel(false);
      expect(await screen.findByText(label)).toBeInTheDocument();
      expect(screen.queryByText("已发布")).not.toBeInTheDocument();
    },
  );
});
