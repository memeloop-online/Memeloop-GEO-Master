import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import type { AuthSession } from "../auth/types";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";

const session: AuthSession = {
  user: {
    id: "user-a",
    login_name: "demo@localhost",
    display_name: "Local Demo",
  },
  operator: { id: "operator-a", slug: "memeloop", display_name: "模因循环" },
  memberships: [
    {
      tenant_id: "tenant-a",
      tenant_slug: "northstar",
      tenant_display_name: "Northstar",
      role: "tenant_admin",
    },
  ],
  expires_at: "2026-09-19T08:00:00Z",
  csrf_token: "csrf-a",
};

function projectForRevision(revision: number) {
  return {
    id: "project-a",
    slug: "northstar",
    display_name: "Northstar AI",
    status: "draft" as const,
    revision,
    settings: {
      brand_name: "Northstar AI",
      product_name: null,
      market: "中国大陆",
      language: "简体中文",
      target_audience: null,
      objective: "提升产品在购买决策问题中的可见度",
      competitors: [],
      initial_sources: [],
      resource_mode: "mixed" as const,
      monthly_budget_minor: 0,
      budget_currency: "CNY",
      monitoring_reserve_percent: 20,
      report_timezone: "Asia/Shanghai",
      report_schedule: {
        report_weekday: "monday",
        report_local_time: "09:00",
        cutoff_weekday: "sunday",
        cutoff_local_time: "23:59",
        period_policy: "previous_calendar_week" as const,
      },
      document_scope: {
        all_active_products: true,
        excluded_product_ids: [],
        markets: ["中国大陆"],
        languages: ["简体中文"],
        content_types: ["product_page", "faq"],
        question_clusters: [],
      },
      distribution_scope: {
        mode: "all_eligible" as const,
        included_platform_ids: [],
        excluded_platform_ids: [],
        resource_pool_ids: [],
        replication_policy: "one_account_per_platform" as const,
      },
    },
    created_at: "2026-09-18T00:00:00Z",
    updated_at: "2026-09-18T00:00:00Z",
  };
}

const acceptance = {
  operation_id: "operation-a",
  cycle_id: "cycle-a",
  config_revision_id: "config-a",
  document_manifest: {
    manifest_id: "documents-a",
    revision: 1,
    state: "awaiting_knowledge",
    sealed: false,
    expected_count: null,
  },
  distribution_manifest: {
    manifest_id: "distribution-a",
    revision: 1,
    state: "awaiting_documents",
    sealed: false,
    expected_count: null,
  },
  status: "accepted" as const,
  operation_url: "/operations/operation-a",
};

// Mirrors the real payload from `estimate_project` in crates/api/src/lib.rs:
// English prose in `reason`, and blockers carrying a structured `code`/`scope`.
// The UI must not parse the prose, so the fixture must not pre-translate it.
const estimate = {
  settings_hash: "settings-hash-a",
  estimator_version: "w02-prerequisites-unknown-v1",
  pricing_snapshot_id: null,
  capability_snapshot_id: null,
  coverage: {
    documents: {
      state: "unknown" as const,
      value: null,
      min: null,
      max: null,
      basis_refs: [],
      reason:
        "KnowledgeRelease is not available; document manifest is not frozen.",
    },
    document_platform_targets: {
      state: "unknown" as const,
      value: null,
      min: null,
      max: null,
      basis_refs: [],
      reason:
        "CapabilitySnapshot is not available; distribution targets are not expanded.",
    },
    measurement_samples: {
      state: "unknown" as const,
      value: null,
      min: null,
      max: null,
      basis_refs: [],
      reason:
        "MeasurementProtocol is not available; measurement samples are not planned.",
    },
  },
  costs: {
    phase_one_documents: {
      state: "unknown" as const,
      value_minor: null,
      min_minor: null,
      max_minor: null,
      basis_refs: [],
      reason:
        "PricingSnapshot and frozen document denominator are unavailable.",
    },
    phase_two_distribution: {
      state: "unknown" as const,
      value_minor: null,
      min_minor: null,
      max_minor: null,
      basis_refs: [],
      reason:
        "PricingSnapshot, CapabilitySnapshot, and distribution denominator are unavailable.",
    },
    measurement: {
      state: "unknown" as const,
      value_minor: null,
      min_minor: null,
      max_minor: null,
      basis_refs: [],
      reason: "PricingSnapshot and MeasurementProtocol are unavailable.",
    },
    total: {
      state: "unknown" as const,
      value_minor: null,
      min_minor: null,
      max_minor: null,
      basis_refs: [],
      reason: "Component costs are not known.",
    },
  },
  budget: {
    monthly_limit_minor: 6000000,
    measurement_reserve_minor: 1200000,
    currency: "CNY",
  },
  blockers: [
    {
      code: "knowledge_release_unavailable",
      scope: "documents",
      reason: "W02 has not resolved immutable knowledge inputs.",
    },
    {
      code: "capability_snapshot_unavailable",
      scope: "document_platform_targets",
      reason: "No eligible platform/account capability snapshot is frozen.",
    },
    {
      code: "measurement_protocol_unavailable",
      scope: "measurement_samples",
      reason: "No measurement protocol or sample plan is frozen.",
    },
    {
      code: "pricing_snapshot_unavailable",
      scope: "costs",
      reason: "No applicable price list snapshot is frozen.",
    },
  ],
  assumptions: [
    "Estimate is side-effect free: it creates no project, reservation, or task.",
    "Zero budget permits later free knowledge work but must block paid actions.",
  ],
};

const overview = {
  project: { ...projectForRevision(3), status: "active" as const },
  cycle: { status: "not_started" as const, awaiting_knowledge: true },
  knowledge: { source_count: 0, fact_count: 0, status: "empty" as const },
  benchmark: {
    question_count: 0,
    planned_samples: 0,
    effective_samples: null,
    status: "not_started" as const,
  },
  content: { published_count: 0, verified_count: 0, blocked_count: 0 },
  cost: { currency: "CNY", reserved_minor: 1200000, settled_minor: 0 },
  next_action: null,
  updated_at: "2026-09-18T00:00:00Z",
};

function response(body: unknown, status = 200) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function renderSetup() {
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
          <MemoryRouter initialEntries={["/setup?tenant_id=tenant-a"]}>
            <AppRoutes />
          </MemoryRouter>
        </AuthProvider>
      </QueryClientProvider>
    </FluentProvider>,
  );
}

function pathFor(request: RequestInfo | URL) {
  return new URL(String(request), "http://localhost").pathname;
}

function overviewForProject(project = overview.project) {
  return { ...overview, project };
}

function requestHandler(
  startResponses: Response[] = [response(acceptance, 202)],
) {
  let nextRevision = 1;
  return vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
    const path = pathFor(request);
    const method = init?.method ?? "GET";
    if (path.endsWith("/auth/session"))
      return Promise.resolve(response(session));
    if (path.endsWith("/projects/estimate")) {
      return Promise.resolve(response(estimate));
    }
    if (path.endsWith("/projects/project-a/start") && method === "GET") {
      return Promise.resolve(response(acceptance));
    }
    if (path.endsWith("/projects/project-a/start")) {
      return Promise.resolve(
        startResponses.shift() ?? response(acceptance, 202),
      );
    }
    if (path.endsWith("/projects/project-a/overview")) {
      return Promise.resolve(response(overviewForProject()));
    }
    if (path.endsWith("/projects/project-a") && method === "PATCH") {
      nextRevision += 1;
      return Promise.resolve(response(projectForRevision(nextRevision)));
    }
    if (path.endsWith("/projects") && method === "POST") {
      return Promise.resolve(response(projectForRevision(nextRevision), 201));
    }
    return Promise.resolve(response({ items: [], next_cursor: null }));
  });
}

function postCalls(fetchMock: ReturnType<typeof vi.fn>) {
  return fetchMock.mock.calls.filter(([, init]) => init?.method === "POST");
}

function patchCalls(fetchMock: ReturnType<typeof vi.fn>) {
  return fetchMock.mock.calls.filter(([, init]) => init?.method === "PATCH");
}

/** The rendered value of the estimate row whose label (or coverage term) is
 * `name`. Reading it per row is what makes the scope-to-label mapping testable;
 * a page-wide `getAllByText` cannot tell one row's label from another's. */
function estimateRowValue(name: string) {
  const row = screen.getByText(name).closest("div");
  return row?.querySelector("strong")?.textContent ?? null;
}

/** The explanation line rendered under the same estimate row. */
function estimateRowExplanation(name: string) {
  const row = screen.getByText(name).closest("div");
  return row?.querySelector("small")?.textContent ?? null;
}

async function advanceToLaunch(
  user: ReturnType<typeof userEvent.setup>,
  { saveFirst = false }: { saveFirst?: boolean } = {},
) {
  await user.type(
    await screen.findByRole("textbox", { name: "品牌名称" }),
    "Northstar AI",
  );
  await user.type(
    screen.getByRole("textbox", { name: "初始资料（可选）" }),
    "https://example.com",
  );
  if (saveFirst) {
    await user.click(screen.getByRole("button", { name: "保存为草稿" }));
    await screen.findByRole("button", { name: "保存草稿" });
  }
  await user.click(screen.getByRole("button", { name: "下一步" }));
  await screen.findByRole("heading", { name: "目标与市场" });
  await user.click(screen.getByRole("button", { name: "下一步" }));
  await screen.findByRole("heading", { name: "发布资源与预算" });
  await screen.findByRole("heading", { name: "资源与预算估算" });
}

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("project setup workflow", () => {
  it("uses three steps and permits optional product and target audience", async () => {
    const fetchMock = requestHandler();
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await advanceToLaunch(user);

    expect(screen.getByText("第 3 步 / 3 步")).toBeInTheDocument();
    expect(screen.queryByText("检查并启动")).not.toBeInTheDocument();
    const createCall = postCalls(fetchMock).find(
      ([request]) => pathFor(request) === "/api/v1/projects",
    );
    const createBody = JSON.parse(
      String((createCall?.[1] as RequestInit).body),
    );
    expect(createBody.settings.product_name).toBeNull();
    expect(createBody.settings.target_audience).toBeNull();
  });

  it("labels each unknown estimate from the blocker that owns its scope", async () => {
    const fetchMock = requestHandler();
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await advanceToLaunch(user);

    // Every cost row is blocked by the `costs`-scoped pricing blocker. A row
    // whose prose also mentions a capability or measurement snapshot must not
    // be relabelled by that incidental mention.
    expect(estimateRowValue("第一阶段文档成本")).toBe("待分项估算");
    expect(estimateRowValue("第二阶段分发成本")).toBe("待分项估算");
    expect(estimateRowValue("测量成本")).toBe("待分项估算");
    expect(estimateRowValue("预计总成本")).toBe("待分项估算");
    // Each coverage row keeps its own scope's label.
    expect(estimateRowValue("文档")).toBe("待知识规划");
    expect(estimateRowValue("文档 × 平台目标")).toBe("待能力快照");
    expect(estimateRowValue("测量样本")).toBe("待测量协议");

    // The reason behind an unknown row is shown, not discarded, and it is the
    // translated blocker text rather than the raw English payload string.
    expect(estimateRowExplanation("预计总成本")).toBe(
      "适用价格表尚未形成快照，因此当前不展示总价。",
    );
    expect(screen.queryByText(/PricingSnapshot/)).not.toBeInTheDocument();

    // Known values are still shown, and the total still spans the grid.
    expect(screen.getByText("¥60,000.00")).toBeInTheDocument();
    expect(screen.getByText("预计总成本").closest("div")).toHaveClass(
      "estimate-total",
    );
  });

  it("creates one draft, serializes revision patches, and starts that revision", async () => {
    const fetchMock = requestHandler();
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await advanceToLaunch(user, { saveFirst: true });
    await user.clear(screen.getByRole("textbox", { name: "月度预算" }));
    await user.type(screen.getByRole("textbox", { name: "月度预算" }), "60000");
    await user.click(screen.getByRole("button", { name: "启动项目" }));

    expect(
      await screen.findByRole("heading", { name: "从一个项目任务开始" }),
    ).toBeInTheDocument();
    expect(
      postCalls(fetchMock).filter(
        ([request]) => pathFor(request) === "/api/v1/projects",
      ),
    ).toHaveLength(1);
    const patches = patchCalls(fetchMock);
    expect(patches).toHaveLength(1);
    const patchHeaders = new Headers((patches[0]?.[1] as RequestInit).headers);
    expect(patchHeaders.get("If-Match")).toBe("1");
    expect(
      JSON.parse(String((patches[0]?.[1] as RequestInit).body)),
    ).toMatchObject({
      revision: 1,
      settings: { monthly_budget_minor: 6000000 },
    });
    const startCall = postCalls(fetchMock).find(
      ([request]) => pathFor(request) === "/api/v1/projects/project-a/start",
    );
    expect(JSON.parse(String((startCall?.[1] as RequestInit).body))).toEqual({
      expected_revision: 2,
    });
  });

  it("retries only the start request with its stable idempotency key", async () => {
    const fetchMock = requestHandler([
      response({ message: "queue unavailable" }, 503),
      response(acceptance, 202),
    ]);
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await advanceToLaunch(user);
    await user.click(screen.getByRole("button", { name: "启动项目" }));
    expect(
      await screen.findByText(/草稿已保存，但启动暂时无法受理/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "重试启动" }));
    expect(
      await screen.findByRole("heading", { name: "从一个项目任务开始" }),
    ).toBeInTheDocument();

    const starts = postCalls(fetchMock).filter(
      ([request]) => pathFor(request) === "/api/v1/projects/project-a/start",
    );
    expect(starts).toHaveLength(2);
    expect(
      new Headers((starts[0]?.[1] as RequestInit).headers).get(
        "Idempotency-Key",
      ),
    ).toBe(
      new Headers((starts[1]?.[1] as RequestInit).headers).get(
        "Idempotency-Key",
      ),
    );
    expect(
      postCalls(fetchMock).filter(
        ([request]) => pathFor(request) === "/api/v1/projects",
      ),
    ).toHaveLength(1);
  });

  it("reads the durable start acceptance after navigation and on refresh without identity headers", async () => {
    const fetchMock = requestHandler();
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await advanceToLaunch(user);
    await user.click(screen.getByRole("button", { name: "启动项目" }));
    await screen.findByRole("heading", { name: "从一个项目任务开始" });
    await user.click(screen.getByRole("link", { name: "P02 · 项目总览" }));
    await screen.findByText(/项目已启动（受理操作 operation-a）/);
    const startReadsBeforeRefresh = fetchMock.mock.calls.filter(
      ([request, init]) =>
        pathFor(request) === "/api/v1/projects/project-a/start" &&
        (init?.method ?? "GET") === "GET",
    ).length;
    expect(startReadsBeforeRefresh).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: "刷新" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(
          ([request, init]) =>
            pathFor(request) === "/api/v1/projects/project-a/start" &&
            (init?.method ?? "GET") === "GET",
        ).length,
      ).toBeGreaterThan(startReadsBeforeRefresh),
    );

    for (const [, init] of [
      ...postCalls(fetchMock),
      ...patchCalls(fetchMock),
    ]) {
      const headers = new Headers((init as RequestInit).headers);
      expect(headers.get("x-operator-id")).toBeNull();
      expect(headers.get("x-tenant-id")).toBeNull();
      expect(headers.get("x-project-id")).toBeNull();
    }
  });

  it("creates the draft before using the real upload-session byte sequence", async () => {
    const fallback = requestHandler();
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = pathFor(request);
        const method = init?.method ?? "GET";
        if (path.endsWith("/knowledge/upload-sessions") && method === "POST") {
          return Promise.resolve(
            response({
              upload_session_id: "upload-a",
              filename: "manual.md",
              expected_size: 4,
              purpose: "public",
              state: "created",
            }),
          );
        }
        if (path.endsWith("/upload-a/content") && method === "PUT") {
          return Promise.resolve(response(undefined, 204));
        }
        if (path.endsWith("/upload-a/complete") && method === "POST") {
          return Promise.resolve(
            response(
              {
                client_item_id: "upload-a",
                status: "queued",
                source: {
                  source_id: "source-a",
                  revision: 1,
                  kind: "file",
                  name: "manual.md",
                  purpose: "public",
                  state: "active",
                  product_ids: [],
                },
                source_version: {
                  source_version_id: "version-a",
                  source_id: "source-a",
                  version: 1,
                  content_sha256: "hash-a",
                },
                import_job: {},
              },
              202,
            ),
          );
        }
        if (
          path.endsWith("/knowledge/materialize-initial-sources") &&
          method === "POST"
        ) {
          return Promise.resolve(response({ items: [] }, 202));
        }
        return fallback(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("crypto", {
      randomUUID: () => "upload-client-id",
      subtle: {
        digest: vi.fn().mockResolvedValue(new Uint8Array(32).buffer),
      },
    });
    const user = userEvent.setup();
    renderSetup();

    await user.type(
      await screen.findByRole("textbox", { name: "品牌名称" }),
      "Northstar AI",
    );
    const file = new File(["demo"], "manual.md", { type: "" });
    Object.defineProperty(file, "arrayBuffer", {
      value: vi.fn().mockResolvedValue(new TextEncoder().encode("demo").buffer),
    });
    await user.upload(screen.getByLabelText("选择文件"), file);
    await user.click(screen.getByRole("button", { name: "保存为草稿" }));

    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(
          ([request, init]) =>
            pathFor(request) ===
              "/api/v1/knowledge/upload-sessions/upload-a/complete" &&
            init?.method === "POST",
        ),
      ).toBe(true),
    );
    const projectCreate = fetchMock.mock.calls.find(
      ([request, init]) =>
        pathFor(request) === "/api/v1/projects" && init?.method === "POST",
    );
    const sessionCreate = fetchMock.mock.calls.find(
      ([request, init]) =>
        pathFor(request) === "/api/v1/knowledge/upload-sessions" &&
        init?.method === "POST",
    );
    expect(projectCreate).toBeDefined();
    expect(sessionCreate).toBeDefined();
    expect(fetchMock.mock.calls.indexOf(projectCreate!)).toBeLessThan(
      fetchMock.mock.calls.indexOf(sessionCreate!),
    );
    expect(
      JSON.parse(String((sessionCreate?.[1] as RequestInit).body)),
    ).toMatchObject({
      filename: "manual.md",
      declared_media_type: "text/markdown",
      expected_sha256: "0".repeat(64),
      purpose: "public",
    });
    for (const [, init] of fetchMock.mock.calls) {
      const headers = new Headers((init as RequestInit | undefined)?.headers);
      expect(headers.get("x-operator-id")).toBeNull();
      expect(headers.get("x-tenant-id")).toBeNull();
      expect(headers.get("x-project-id")).toBeNull();
    }
  });

  it("freezes an accepted file source into the draft before starting a file-only project", async () => {
    const fallback = requestHandler();
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = pathFor(request);
        const method = init?.method ?? "GET";
        if (path.endsWith("/knowledge/upload-sessions") && method === "POST") {
          return Promise.resolve(
            response({
              upload_session_id: "upload-a",
              filename: "manual.txt",
              expected_size: 4,
              purpose: "internal",
              state: "created",
            }),
          );
        }
        if (path.endsWith("/upload-a/content") && method === "PUT") {
          return Promise.resolve(response(undefined, 204));
        }
        if (path.endsWith("/upload-a/complete") && method === "POST") {
          return Promise.resolve(
            response(
              {
                client_item_id: "upload-a",
                status: "queued",
                source: {
                  source_id: "source-file-a",
                  revision: 1,
                  kind: "file",
                  name: "manual.txt",
                  purpose: "internal",
                  state: "active",
                  product_ids: [],
                },
                source_version: {
                  source_version_id: "version-file-a",
                  source_id: "source-file-a",
                  version: 1,
                  content_sha256: "file-sha-a",
                },
              },
              202,
            ),
          );
        }
        return fallback(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("crypto", {
      randomUUID: () => "upload-client-id",
      subtle: {
        digest: vi.fn().mockResolvedValue(new Uint8Array(32).buffer),
      },
    });
    const user = userEvent.setup();
    renderSetup();

    await user.type(
      await screen.findByRole("textbox", { name: "品牌名称" }),
      "Northstar AI",
    );
    await user.selectOptions(
      screen.getByRole("combobox", { name: "文件用途" }),
      "internal",
    );
    const file = new File(["demo"], "manual.txt", { type: "text/plain" });
    Object.defineProperty(file, "arrayBuffer", {
      value: vi.fn().mockResolvedValue(new TextEncoder().encode("demo").buffer),
    });
    await user.upload(screen.getByLabelText("选择文件"), file);
    await user.click(screen.getByRole("button", { name: "保存为草稿" }));
    await screen.findByText(/已受理解析/);

    await user.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: "目标与市场" });
    await user.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: "发布资源与预算" });
    await screen.findByRole("heading", { name: "资源与预算估算" });
    await user.click(screen.getByRole("button", { name: "启动项目" }));
    await screen.findByRole("heading", { name: "从一个项目任务开始" });

    const sourcePatch = fetchMock.mock.calls.find(([request, init]) => {
      if (
        pathFor(request) !== "/api/v1/projects/project-a" ||
        init?.method !== "PATCH"
      ) {
        return false;
      }
      const body = JSON.parse(String(init.body)) as {
        settings?: { initial_sources?: unknown[] };
      };
      return Boolean(
        body.settings?.initial_sources?.some(
          (source) =>
            typeof source === "object" &&
            source !== null &&
            (source as { value?: string }).value === "source-file-a",
        ),
      );
    });
    const start = fetchMock.mock.calls.find(
      ([request, init]) =>
        pathFor(request) === "/api/v1/projects/project-a/start" &&
        init?.method === "POST",
    );
    const materialize = fetchMock.mock.calls.find(
      ([request, init]) =>
        pathFor(request) === "/api/v1/knowledge/materialize-initial-sources" &&
        init?.method === "POST",
    );
    expect(sourcePatch).toBeDefined();
    expect(materialize).toBeDefined();
    expect(start).toBeDefined();
    const materializeUrl = new URL(
      String(materialize?.[0]),
      "http://localhost",
    );
    expect(materializeUrl.searchParams.get("tenant_id")).toBe("tenant-a");
    expect(materializeUrl.searchParams.get("project_id")).toBe("project-a");
    expect(fetchMock.mock.calls.indexOf(sourcePatch!)).toBeLessThan(
      fetchMock.mock.calls.indexOf(materialize!),
    );
    expect(fetchMock.mock.calls.indexOf(materialize!)).toBeLessThan(
      fetchMock.mock.calls.indexOf(start!),
    );
    expect(
      JSON.parse(String((sourcePatch?.[1] as RequestInit).body)),
    ).toMatchObject({
      settings: {
        initial_sources: [
          {
            kind: "object",
            value: "source-file-a",
            visibility: "internal",
            version_ref: "version-file-a",
            content_hash: "file-sha-a",
          },
        ],
      },
    });
  });

  it("does not start when materializing the initial sources fails at HTTP level", async () => {
    const fallback = requestHandler();
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        if (
          pathFor(request) ===
            "/api/v1/knowledge/materialize-initial-sources" &&
          init?.method === "POST"
        ) {
          return Promise.resolve(
            response({ message: "knowledge service unavailable" }, 503),
          );
        }
        return fallback(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await advanceToLaunch(user);
    await user.click(screen.getByRole("button", { name: "启动项目" }));

    expect(
      await screen.findByText(/初始资料暂时无法提交到知识库/),
    ).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(
        ([request, init]) =>
          pathFor(request) === "/api/v1/projects/project-a/start" &&
          init?.method === "POST",
      ),
    ).toBe(false);
  });

  it("starts when materialization returns item failures, preserving awaiting-knowledge handling", async () => {
    const fallback = requestHandler();
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        if (
          pathFor(request) ===
            "/api/v1/knowledge/materialize-initial-sources" &&
          init?.method === "POST"
        ) {
          return Promise.resolve(
            response(
              {
                items: [
                  {
                    client_item_id: "website",
                    status: "failed",
                    error: { message: "URL 抓取能力未配置" },
                  },
                ],
              },
              202,
            ),
          );
        }
        return fallback(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    const user = userEvent.setup();
    renderSetup();

    await advanceToLaunch(user);
    await user.click(screen.getByRole("button", { name: "启动项目" }));

    expect(
      await screen.findByRole("heading", { name: "从一个项目任务开始" }),
    ).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(
        ([request, init]) =>
          pathFor(request) ===
            "/api/v1/knowledge/materialize-initial-sources" &&
          init?.method === "POST",
      ),
    ).toBe(true);
  });

  it("does not enable start while a selected file is still uploading", async () => {
    const fallback = requestHandler();
    let uploadContentStarted: (() => void) | undefined;
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = pathFor(request);
        const method = init?.method ?? "GET";
        if (path.endsWith("/knowledge/upload-sessions") && method === "POST") {
          return Promise.resolve(
            response({
              upload_session_id: "upload-pending",
              filename: "manual.txt",
              expected_size: 4,
              purpose: "public",
              state: "created",
            }),
          );
        }
        if (path.endsWith("/upload-pending/content") && method === "PUT") {
          return new Promise<Response>((resolve) => {
            uploadContentStarted = () => resolve(response(undefined, 204));
          });
        }
        return fallback(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("crypto", {
      randomUUID: () => "upload-pending-id",
      subtle: {
        digest: vi.fn().mockResolvedValue(new Uint8Array(32).buffer),
      },
    });
    const user = userEvent.setup();
    renderSetup();

    await user.type(
      await screen.findByRole("textbox", { name: "品牌名称" }),
      "Northstar AI",
    );
    const file = new File(["demo"], "manual.txt", { type: "text/plain" });
    Object.defineProperty(file, "arrayBuffer", {
      value: vi.fn().mockResolvedValue(new TextEncoder().encode("demo").buffer),
    });
    await user.upload(screen.getByLabelText("选择文件"), file);
    await user.click(screen.getByRole("button", { name: "保存为草稿" }));
    await waitFor(() => expect(uploadContentStarted).toBeDefined());

    await user.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: "目标与市场" });
    await user.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: "发布资源与预算" });
    await screen.findByRole("heading", { name: "资源与预算估算" });

    expect(
      screen.getByRole("button", { name: "等待资料上传完成" }),
    ).toBeDisabled();
    expect(screen.getAllByText(/等待资料上传完成/)).toHaveLength(2);
  });
});
