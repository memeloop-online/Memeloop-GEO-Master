import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import {
  getDocumentManifest,
  planDocumentManifest,
} from "../api/documentManifests";

const session = {
  user: { id: "user-1", login_name: "user@example.test", display_name: "User" },
  operator: { id: "operator-1", slug: "operator", display_name: "Operator" },
  memberships: [
    {
      tenant_id: "tenant-1",
      tenant_slug: "tenant",
      tenant_display_name: "Tenant",
      role: "tenant_admin",
    },
  ],
  expires_at: "2026-10-01T00:00:00Z",
  csrf_token: "csrf-test",
};

const start = {
  operation_id: "operation-1",
  cycle_id: "cycle-1",
  config_revision_id: "config-1",
  document_manifest: {
    manifest_id: "manifest-1",
    revision: 1,
    state: "awaiting_knowledge",
    sealed: false,
    expected_count: null,
  },
  distribution_manifest: {
    manifest_id: "distribution-1",
    revision: 1,
    state: "awaiting_documents",
    sealed: false,
    expected_count: null,
  },
  status: "accepted",
  operation_url: "/operations/operation-1",
};

const planned = {
  manifest_id: "manifest-1",
  revision: 1,
  knowledge_release_id: "release-1",
  planner_version: "deterministic-document-v1",
  state: "ready",
  sealed: true,
  expected_count: 2,
  scope_hash: "scope-hash",
  coverage: {
    total: 2,
    planned: 1,
    blocked: 1,
    deferred: 0,
    not_applicable: 0,
  },
  items: [
    {
      document_manifest_item_id: "item-1",
      manifest_id: "manifest-1",
      knowledge_release_id: "release-1",
      document_key: "project:one",
      content_type: "product_page",
      market: "market-one",
      language: "zh",
      state: "planned",
      dependency_hash: "hash-one",
      source_version_refs: ["source-version-1"],
    },
    {
      document_manifest_item_id: "item-2",
      manifest_id: "manifest-1",
      knowledge_release_id: "release-1",
      document_key: "project:two",
      content_type: "faq",
      market: "market-one",
      language: "zh",
      state: "blocked",
      block_reason: "knowledge_release_has_no_public_sources",
      dependency_hash: "hash-two",
      source_version_refs: [],
    },
  ],
};

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function renderPage(path = "/app/tenant-1/project-1/campaigns/current") {
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
        <AuthProvider>
          <MemoryRouter initialEntries={[path]}>
            <AppRoutes />
          </MemoryRouter>
        </AuthProvider>
      </QueryClientProvider>
    </FluentProvider>,
  );
}

function mockInputs(
  planResponse: unknown = planned,
  planStatus = 200,
  {
    persisted,
    startValue = start,
    sessionValue = session,
    releaseId = "release-1",
  }: {
    persisted?: unknown;
    startValue?: typeof start;
    sessionValue?: typeof session;
    releaseId?: string | null;
  } = {},
) {
  let persistedPlan = persisted;
  const fetchMock = vi.fn((request: RequestInfo | URL, _init?: RequestInit) => {
    const url = new URL(String(request), "http://localhost");
    if (url.pathname.endsWith("/auth/session"))
      return Promise.resolve(response(sessionValue));
    if (url.pathname.endsWith("/projects/project-1/start"))
      return Promise.resolve(response(startValue));
    if (url.pathname.endsWith("/knowledge/releases/current")) {
      return Promise.resolve(
        releaseId
          ? response({
              project_id: "project-1",
              knowledge_release_id: releaseId,
              sequence: 2,
            })
          : response({ code: "not_found" }, 404),
      );
    }
    if (url.pathname.endsWith("/knowledge/document-manifests/manifest-1")) {
      return Promise.resolve(
        persistedPlan
          ? response(persistedPlan)
          : response({ code: "not_found", message: "not found" }, 404),
      );
    }
    if (url.pathname.endsWith("/knowledge/document-manifests/plan")) {
      if (planStatus === 200) persistedPlan = planResponse;
      return Promise.resolve(response(planResponse, planStatus));
    }
    if (url.pathname.endsWith("/projects")) {
      return Promise.resolve(response({ items: [], next_cursor: null }));
    }
    return Promise.resolve(response({}, 404));
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("P07 document planning", () => {
  it("plans only on click, shows coverage and dependencies, and explicitly does not claim generated content", async () => {
    const fetchMock = mockInputs();
    const user = userEvent.setup();
    renderPage();

    const button = await screen.findByRole("button", { name: "规划文档清单" });
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("document-manifests/plan"),
      ),
    ).toHaveLength(0);
    await user.click(button);

    expect(await screen.findByText("source-version-1")).toBeInTheDocument();
    expect(screen.getByText("无公开来源版本")).toBeInTheDocument();
    expect(screen.getByText(/待生成 1 · 阻断 1/)).toBeInTheDocument();
    expect(screen.queryByText(/P07|清单句柄|规划器/)).not.toBeInTheDocument();
    expect(
      screen.getByText(/此处显示内容计划，正文与生成进度请查看内容资产/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "查看本轮内容资产与执行" }),
    ).toHaveAttribute(
      "href",
      "/app/tenant-1/project-1/content?cycle_id=cycle-1",
    );
    expect(
      screen.getByText("knowledge_release_has_no_public_sources"),
    ).toBeInTheDocument();
    const calls = fetchMock.mock.calls.filter(([request]) =>
      String(request).includes("document-manifests/plan"),
    );
    expect(calls).toHaveLength(1);
    const [url, init] = calls[0] as [string, RequestInit];
    expect(url).toContain("tenant_id=tenant-1");
    expect(url).toContain("project_id=project-1");
    expect(JSON.parse(String(init.body))).toEqual({
      manifest_id: "manifest-1",
      knowledge_release_id: "release-1",
    });
    expect(new Headers(init.headers).get("X-CSRF-Token")).toBe("csrf-test");

    await user.click(screen.getByRole("button", { name: "刷新文档清单" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(([request]) =>
          String(request).includes("document-manifests/manifest-1"),
        ),
      ).toHaveLength(3),
    );
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("document-manifests/plan"),
      ),
    ).toHaveLength(1);
  });

  it("reads a sealed manifest from its handle despite a stale start acceptance and later knowledge release", async () => {
    const fetchMock = mockInputs(planned, 200, {
      persisted: planned,
      releaseId: "release-2",
    });
    renderPage();
    expect(await screen.findByText("source-version-1")).toBeInTheDocument();
    expect(screen.getByText("知识版本 release-1")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "刷新文档清单" }),
    ).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("document-manifests/plan"),
      ),
    ).toHaveLength(0);
    const [url, init] = fetchMock.mock.calls.find(([request]) =>
      String(request).includes("document-manifests/manifest-1"),
    ) as [string, RequestInit];
    expect(url).toContain("tenant_id=tenant-1&project_id=project-1");
    expect(init.method).toBe("GET");
  });

  it("allows a viewer to read a persisted plan without offering a planning command", async () => {
    const fetchMock = mockInputs(planned, 200, {
      persisted: planned,
      sessionValue: {
        ...session,
        memberships: [{ ...session.memberships[0], role: "viewer" }],
      },
    });
    renderPage();
    expect(await screen.findByText("source-version-1")).toBeInTheDocument();
    expect(screen.getByText(/当前成员仅可查看/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "规划文档清单" }),
    ).not.toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("document-manifests/plan"),
      ),
    ).toHaveLength(0);
  });

  it("retains sealed coverage even when there is no current knowledge release", async () => {
    const fetchMock = mockInputs(planned, 200, {
      persisted: planned,
      releaseId: null,
    });
    renderPage();
    expect(await screen.findByText("source-version-1")).toBeInTheDocument();
    expect(screen.queryByText("尚无可用知识版本")).not.toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("document-manifests/plan"),
      ),
    ).toHaveLength(0);
  });

  it("shows a conflict without replacing the manifest or claiming success", async () => {
    mockInputs({ code: "conflict", message: "release differs" }, 409);
    const user = userEvent.setup();
    renderPage();
    await user.click(
      await screen.findByRole("button", { name: "规划文档清单" }),
    );
    expect(await screen.findByText("规划输入冲突")).toBeInTheDocument();
    expect(screen.getByText(/已有清单不会被覆盖/)).toBeInTheDocument();
    expect(screen.queryByText("内容计划概览")).not.toBeInTheDocument();
  });

  it("does not send a planning request for a different cycle or unauthorized tenant", async () => {
    const fetchMock = mockInputs();
    const first = renderPage("/app/tenant-1/project-1/campaigns/another-cycle");
    expect(await screen.findByText("本轮计划不匹配")).toBeInTheDocument();
    first.unmount();
    renderPage("/app/tenant-other/project-1/campaigns/current");
    expect(await screen.findByText("权限不足")).toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(([request]) =>
        String(request).includes("document-manifests/plan"),
      ),
    ).toHaveLength(0);
  });

  it("scopes direct API calls to the selected project and tenant", async () => {
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(response(planned)));
    vi.stubGlobal("fetch", fetchMock);
    await planDocumentManifest("tenant-other", "project-other", {
      manifest_id: "manifest-other",
      knowledge_release_id: "release-other",
    });
    expect(String(fetchMock.mock.calls[0][0])).toContain(
      "tenant_id=tenant-other&project_id=project-other",
    );
    await getDocumentManifest(
      "tenant-other",
      "project-other",
      "manifest/other",
    );
    expect(String(fetchMock.mock.calls[1][0])).toContain(
      "/knowledge/document-manifests/manifest%2Fother?tenant_id=tenant-other&project_id=project-other",
    );
    expect(fetchMock.mock.calls[1][1]?.method).toBe("GET");
  });
});
