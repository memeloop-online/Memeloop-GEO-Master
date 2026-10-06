import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type {
  ContentExecution,
  ContentItem,
  ContentRevision,
  StructuredDocument,
} from "../api/content";
import {
  documentToEditor,
  editorToDocument,
} from "../components/StructuredContentEditor";

const session = {
  user: { id: "user-1", login_name: "user@example.test", display_name: "User" },
  operator: { id: "operator-1", slug: "operator", display_name: "Operator" },
  memberships: [
    {
      tenant_id: "tenant-1",
      tenant_slug: "tenant",
      tenant_display_name: "Tenant",
      role: "member",
    },
  ],
  expires_at: "2026-10-01T00:00:00Z",
  csrf_token: "csrf-test",
};

const manifest = {
  manifest_id: "manifest-1",
  revision: 1,
  knowledge_release_id: "release-1",
  planner_version: "planner-v1",
  state: "ready",
  sealed: true,
  expected_count: 2,
  scope_hash: "scope",
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
      document_key: "guide",
      content_type: "guide",
      product_id: null,
      market: "CN",
      language: "zh",
      state: "planned",
      block_reason: null,
      dependency_hash: "hash-1",
      source_version_refs: ["version-1"],
    },
    {
      document_manifest_item_id: "item-2",
      manifest_id: "manifest-1",
      knowledge_release_id: "release-1",
      document_key: "faq",
      content_type: "faq",
      product_id: null,
      market: "CN",
      language: "zh",
      state: "blocked",
      block_reason: "资料不足",
      dependency_hash: "hash-2",
      source_version_refs: [],
    },
  ],
};

const coverage = {
  total: 2,
  ready: 1,
  blocked: 1,
  deferred: 0,
  not_applicable: 0,
  cancelled: 0,
  incomplete: 0,
};
const execution: ContentExecution = {
  execution_id: "execution-1",
  project_id: "project-1",
  cycle_id: "cycle-1",
  manifest_id: "manifest-1",
  manifest_revision: 1,
  policy_version: "policy-1",
  input_hash: "input",
  status: "closed",
  expected_count: 2,
  coverage,
  handoff_id: "handoff-1",
};
const items: ContentItem[] = [
  {
    item_id: "item-1",
    execution_id: "execution-1",
    document_key: "guide",
    branch_key: "branch-1",
    input_hash: "hash-1",
    planning_state: "planned",
    planning_reason: null,
    status: "ready",
    reason: null,
    source_version_refs: ["version-1"],
    brief: null,
    asset_id: "asset-1",
    current_revision_id: "revision-1",
    ready_revision_id: "revision-1",
    steps: [],
  },
  {
    item_id: "item-2",
    execution_id: "execution-1",
    document_key: "faq",
    branch_key: "branch-2",
    input_hash: "hash-2",
    planning_state: "blocked",
    planning_reason: "资料不足",
    status: "blocked",
    reason: "资料不足",
    source_version_refs: [],
    brief: null,
    asset_id: null,
    current_revision_id: null,
    ready_revision_id: null,
    steps: [],
  },
];
const revision: ContentRevision = {
  revision_id: "revision-1",
  asset_id: "asset-1",
  revision: 1,
  base_revision_id: null,
  document: {
    title: "有来源的指南",
    blocks: [
      {
        block_id: "block-1",
        kind: "paragraph",
        text: "原始正文",
        citation_ids: ["chunk-1"],
        items: [],
      },
    ],
  },
  markdown: "# 有来源的指南\n\n原始正文",
  evidence: [
    {
      source_version_id: "version-1",
      chunk_id: "chunk-1",
      locator: {
        kind: "text",
        start_line: 2,
        end_line: 2,
        start_char: 0,
        end_char: 6,
      },
    },
  ],
  quotes: [
    {
      reference: {
        source_version_id: "version-1",
        chunk_id: "chunk-1",
        locator: {
          kind: "text",
          start_line: 2,
          end_line: 2,
          start_char: 0,
          end_char: 6,
        },
      },
      exact_quote: "证据原文片段",
    },
  ],
  findings: [
    {
      finding_id: "finding-1",
      code: "claim_uncertain",
      block_id: "block-1",
      evidence: [],
      detail: "此处依据不足",
      blocking: true,
    },
  ],
  created_at: "2026-10-01T08:00:00Z",
};
const reusedItem: ContentItem = {
  ...items[0],
  reuse_binding: {
    origin_execution_id: "execution-previous",
    origin_item_id: "item-previous",
    asset_id: "asset-1",
    revision_id: "revision-1",
    check_id: "check-previous",
    fingerprint: "opaque-fingerprint",
    reused_at: "2026-10-02T08:00:00Z",
  },
};

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function mockApi({
  executionList = [execution],
  itemList = items,
  editStatus = 201,
  executionsStatus = 200,
  role = "member",
  manifestStatus = 200,
  resumeStatus = 202,
  cancelStatus = 200,
  forkStatus = 201,
  sourceAdvanced = false,
  documentOverride,
}: {
  executionList?: ContentExecution[];
  itemList?: ContentItem[];
  editStatus?: number;
  executionsStatus?: number;
  role?: "member" | "viewer";
  manifestStatus?: number;
  resumeStatus?: number;
  cancelStatus?: number;
  forkStatus?: number;
  sourceAdvanced?: boolean;
  documentOverride?: StructuredDocument;
} = {}) {
  let persistedExecutions = executionList;
  let persistedItems = itemList;
  let forkedRevision: ContentRevision | undefined;
  const editingRevision = documentOverride
    ? { ...revision, document: documentOverride }
    : revision;
  const requests = vi.fn((url: RequestInfo | URL, init?: RequestInit) => {
    const path = new URL(String(url), "http://localhost").pathname;
    if (path.endsWith("/auth/session"))
      return Promise.resolve(
        json({
          ...session,
          memberships: [{ ...session.memberships[0], role }],
        }),
      );
    if (path.endsWith("/projects"))
      return Promise.resolve(json({ items: [], next_cursor: null }));
    if (path.endsWith("/projects/project-1/cycles/current"))
      return Promise.resolve(
        json({
          project_id: "project-1",
          cycle_id: "cycle-1",
          report_timezone: "UTC",
          report_window_start_at: "2026-10-01T00:00:00Z",
          report_window_end_at: "2026-10-08T00:00:00Z",
          cutoff_at: "2026-10-08T00:00:00Z",
          document_manifest: {
            manifest_id: "manifest-1",
            revision: 1,
            state: "ready",
            sealed: true,
            expected_count: 2,
          },
          distribution_manifest: null,
        }),
      );
    if (path.endsWith("/knowledge/document-manifests/manifest-1"))
      return Promise.resolve(
        json(
          manifestStatus === 200 ? manifest : { message: "not found" },
          manifestStatus,
        ),
      );
    if (path.endsWith("/cycles/cycle-1/document-executions")) {
      if (init?.method === "POST")
        return Promise.resolve(
          editStatus === 201
            ? json(execution, 202)
            : json(
                { code: "capability_missing", message: "未配置生成能力" },
                503,
              ),
        );
      return Promise.resolve(
        json(
          executionsStatus === 200
            ? persistedExecutions
            : { message: "执行账本不可用" },
          executionsStatus,
        ),
      );
    }
    if (path.endsWith("/document-executions/execution-1/resume"))
      return Promise.resolve(
        resumeStatus === 202
          ? json(persistedExecutions[0], 202)
          : json(
              { code: "capability_missing", message: "生成能力尚未配置" },
              resumeStatus,
            ),
      );
    if (path.endsWith("/document-executions/execution-1/cancel")) {
      if (cancelStatus === 200)
        persistedExecutions = [
          { ...persistedExecutions[0], status: "cancelled" },
        ];
      return Promise.resolve(
        cancelStatus === 200
          ? json(persistedExecutions[0])
          : json({ message: "取消失败" }, cancelStatus),
      );
    }
    if (path.endsWith("/document-executions/execution-1/items"))
      return Promise.resolve(json(persistedItems));
    if (path.endsWith("/document-executions/execution-1/items/item-1/fork")) {
      if (forkStatus !== 201)
        return Promise.resolve(
          json({ code: "conflict", message: "基线版本已改变" }, forkStatus),
        );
      forkedRevision = {
        ...revision,
        asset_id: "asset-forked",
        revision_id: "revision-forked",
        base_revision_id: null,
        derived_from_revision_id: "revision-1",
        document: JSON.parse(String(init?.body)).document,
        findings: [],
      };
      persistedItems = persistedItems.map((item) =>
        item.item_id === "item-1"
          ? {
              ...item,
              asset_id: "asset-forked",
              current_revision_id: "revision-forked",
              ready_revision_id: null,
              status: "drafted",
              reuse_binding: null,
            }
          : item,
      );
      return Promise.resolve(json(forkedRevision, 201));
    }
    if (path.endsWith("/contents/asset-1"))
      return Promise.resolve(
        json({
          asset_id: "asset-1",
          execution_id: "execution-1",
          item_id: "item-1",
          current_revision_id: sourceAdvanced ? "revision-2" : "revision-1",
          created_at: "2026-10-01T08:00:00Z",
        }),
      );
    if (path.endsWith("/contents/asset-1/revisions")) {
      if (init?.method === "POST")
        return Promise.resolve(
          editStatus === 201
            ? json(
                {
                  ...editingRevision,
                  revision_id: "revision-2",
                  revision: 2,
                  base_revision_id: "revision-1",
                  findings: [],
                  document: JSON.parse(String(init.body)).document,
                },
                201,
              )
            : json({ code: "conflict", message: "基线版本已改变" }, editStatus),
        );
      return Promise.resolve(
        json(
          sourceAdvanced
            ? [
                editingRevision,
                {
                  ...editingRevision,
                  revision_id: "revision-2",
                  revision: 2,
                  base_revision_id: "revision-1",
                  document: { ...revision.document, title: "原资产后续修订" },
                },
              ]
            : [editingRevision],
        ),
      );
    }
    if (path.endsWith("/contents/asset-forked"))
      return Promise.resolve(
        json({
          asset_id: "asset-forked",
          execution_id: "execution-1",
          item_id: "item-1",
          current_revision_id: "revision-forked",
          created_at: "2026-10-02T08:00:00Z",
        }),
      );
    if (path.endsWith("/contents/asset-forked/revisions"))
      return Promise.resolve(json(forkedRevision ? [forkedRevision] : []));
    return Promise.resolve(json({ message: "not found" }, 404));
  });
  vi.stubGlobal("fetch", requests);
  return requests;
}

function renderPage(path: string) {
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

function putCaretAtEnd(element: Element) {
  const selection = window.getSelection();
  const range = document.createRange();
  range.selectNodeContents(element);
  range.collapse(false);
  selection?.removeAllRanges();
  selection?.addRange(range);
  document.dispatchEvent(new Event("selectionchange"));
}

beforeEach(() => {
  // jsdom omits layout methods that ProseMirror uses to place the caret.
  Object.defineProperty(document, "elementFromPoint", {
    configurable: true,
    value: () => document.activeElement ?? document.body,
  });
  Object.defineProperty(Range.prototype, "getClientRects", {
    configurable: true,
    value: () => [],
  });
  Object.defineProperty(Range.prototype, "getBoundingClientRect", {
    configurable: true,
    value: () => new DOMRect(0, 0, 0, 0),
  });
});

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("P08 content assets", () => {
  it("separates current-cycle coverage from a reused checked source", async () => {
    mockApi({ itemList: [reusedItem, items[1]] });
    renderPage("/app/tenant-1/project-1/content");
    const list = await screen.findByRole("region", { name: "全部文档分支" });
    expect(
      await within(list).findByText(
        /复用已检查版本；本轮仍有独立清单项和覆盖记录/,
      ),
    ).toHaveTextContent("check-previous");
    expect(
      within(list).getByRole("link", { name: "查看复用正文与原检查证据" }),
    ).toHaveAttribute(
      "href",
      "/app/tenant-1/project-1/content/asset-1?reuse_execution_id=execution-1&reuse_item_id=item-1",
    );
    expect(
      within(list).getByRole("link", { name: /查看原资产/ }),
    ).toHaveAttribute("href", "/app/tenant-1/project-1/content/asset-1");
    expect(
      within(list).queryByText("opaque-fingerprint"),
    ).not.toBeInTheDocument();
  });
  it("shows automatic repair as unfinished without a manual approval action", async () => {
    mockApi({
      executionList: [
        {
          ...execution,
          status: "running",
          handoff_id: null,
          coverage: { ...coverage, ready: 0, incomplete: 1 },
        },
      ],
      itemList: [
        {
          ...items[0],
          status: "needs_repair",
          ready_revision_id: null,
          automatic_repair_count: 1,
          reason: "当前版本仍有依据不足的表述",
        },
        items[1],
      ],
    });
    renderPage("/app/tenant-1/project-1/content");
    expect(await screen.findByText(/执行：running/)).toHaveTextContent(
      "未完成 1",
    );
    const list = screen.getByRole("region", { name: "全部文档分支" });
    expect(
      await within(list).findByText(/已完成 1 \/ 2 轮/),
    ).toBeInTheDocument();
    expect(within(list).getAllByText(/待自动修正/)).toHaveLength(2);
    expect(within(list).queryByRole("button")).not.toBeInTheDocument();
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
  });

  it("keeps the frozen denominator and missing-asset branch visible", async () => {
    mockApi();
    renderPage("/app/tenant-1/project-1/content");
    expect(await screen.findByText(/规划分母 2/)).toBeInTheDocument();
    expect(screen.getByText(/执行：closed/)).toHaveTextContent("分母 2");
    const list = screen.getByRole("region", { name: "全部文档分支" });
    expect(
      await within(list).findByRole("link", { name: "查看正文与版本" }),
    ).toHaveAttribute("href", "/app/tenant-1/project-1/content/asset-1");
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(
      within(list).getAllByText("资料不足", { exact: false }),
    ).toHaveLength(2);
    expect(within(list).getByText(/尚无持久正文资产/)).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("checkbox", { name: "仅看未就绪项" }),
    );
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    expect(
      within(list).getByRole("heading", { name: "faq" }),
    ).toBeInTheDocument();
  });

  it("reports no execution separately and shows capability failure on start", async () => {
    const requests = mockApi({ executionList: [], editStatus: 503 });
    renderPage("/app/tenant-1/project-1/content");
    expect(await screen.findByText(/尚无正文执行/)).toBeInTheDocument();
    expect(screen.getAllByText(/尚无执行记录/)).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: "启动正文生成" }));
    expect(await screen.findByText("内容生成能力尚未配置")).toBeInTheDocument();
    expect(
      requests.mock.calls.some(
        ([url, init]) =>
          String(url).includes("/cycles/cycle-1/document-executions") &&
          init?.method === "POST",
      ),
    ).toBe(true);
  });

  it("does not mistake a failed execution read for an empty execution", async () => {
    mockApi({ executionsStatus: 503 });
    renderPage("/app/tenant-1/project-1/content");
    expect(await screen.findByText("无法读取执行进度")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "启动正文生成" }),
    ).not.toBeInTheDocument();
  });

  it("renders an unplanned manifest handle as empty without starting generation", async () => {
    mockApi({ manifestStatus: 404 });
    renderPage("/app/tenant-1/project-1/content");
    expect(await screen.findByText("清单尚未规划")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "启动正文生成" }),
    ).not.toBeInTheDocument();
  });

  it("marks a partial item response as a coverage gap", async () => {
    mockApi({ itemList: [items[0]] });
    renderPage("/app/tenant-1/project-1/content");
    expect(await screen.findByText(/执行账本返回 1 项/)).toBeInTheDocument();
    expect(screen.getByText(/执行记录缺失，覆盖待核对/)).toBeInTheDocument();
  });

  it("keeps a viewer read-only", async () => {
    mockApi({ executionList: [], role: "viewer" });
    renderPage("/app/tenant-1/project-1/content");
    expect(await screen.findByText(/当前成员仅可查看/)).toBeInTheDocument();
    expect(
      await screen.findByRole("button", { name: "启动正文生成" }),
    ).toBeDisabled();
  });

  it("resumes a persisted running execution without posting fabricated body", async () => {
    const requests = mockApi({
      executionList: [{ ...execution, status: "running" }],
    });
    renderPage("/app/tenant-1/project-1/content");
    await userEvent.click(
      await screen.findByRole("button", { name: "恢复未完成分支" }),
    );
    expect(await screen.findByText(/恢复请求已受理/)).toBeInTheDocument();
    expect(
      requests.mock.calls.some(
        ([url, init]) =>
          String(url).includes(
            "/projects/project-1/document-executions/execution-1/resume",
          ) &&
          init?.method === "POST" &&
          init.body === undefined,
      ),
    ).toBe(true);
    await waitFor(() =>
      expect(
        requests.mock.calls.filter(([url]) =>
          String(url).includes("/cycles/cycle-1/document-executions"),
        ).length,
      ).toBeGreaterThan(1),
    );
  });

  it("cancels uncompleted branches and refreshes the persisted state", async () => {
    const requests = mockApi({
      executionList: [{ ...execution, status: "running" }],
    });
    renderPage("/app/tenant-1/project-1/content");
    await userEvent.click(
      await screen.findByRole("button", { name: "取消未完成分支" }),
    );
    expect(await screen.findByText(/取消结果已持久化/)).toBeInTheDocument();
    expect(
      requests.mock.calls.some(
        ([url, init]) =>
          String(url).includes(
            "/projects/project-1/document-executions/execution-1/cancel",
          ) &&
          init?.method === "POST" &&
          init.body === undefined,
      ),
    ).toBe(true);
    expect(
      screen.queryByRole("button", { name: "取消未完成分支" }),
    ).not.toBeInTheDocument();
  });

  it("makes resume capability failure explicit, without inventing progress", async () => {
    mockApi({
      executionList: [{ ...execution, status: "running" }],
      resumeStatus: 503,
    });
    renderPage("/app/tenant-1/project-1/content");
    await userEvent.click(
      await screen.findByRole("button", { name: "恢复未完成分支" }),
    );
    expect(await screen.findByText("内容生成能力尚未配置")).toBeInTheDocument();
    expect(screen.queryByText(/恢复请求已受理/)).not.toBeInTheDocument();
  });

  it("disables resume and cancel for a viewer", async () => {
    const requests = mockApi({
      executionList: [{ ...execution, status: "running" }],
      role: "viewer",
    });
    renderPage("/app/tenant-1/project-1/content");
    expect(
      await screen.findByRole("button", { name: "恢复未完成分支" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "取消未完成分支" }),
    ).toBeDisabled();
    expect(
      requests.mock.calls.some(([, init]) => init?.method === "POST"),
    ).toBe(false);
  });
});

describe("P09 content revision", () => {
  it("round-trips list captions and citations without touching the source version", () => {
    const original: StructuredDocument = {
      title: "有来源的指南",
      blocks: [
        revision.document.blocks[0],
        {
          block_id: "block-list",
          kind: "list",
          text: "有来源的项目",
          citation_ids: ["chunk-1"],
          items: ["第一项", "第二项"],
        },
      ],
    };
    expect(editorToDocument(documentToEditor(original), original)).toEqual(
      original,
    );
    const duplicated = documentToEditor(original);
    duplicated.content?.splice(1, 0, { ...duplicated.content[0] });
    const split = editorToDocument(duplicated, original);
    expect(split.blocks[1].block_id).not.toBe("block-1");
    expect(split.blocks[1].citation_ids).toEqual(["chunk-1"]);
    expect(split.blocks[2]).toEqual(original.blocks[1]);
  });

  it("merges separately cited paragraphs into one list with both source refs", () => {
    const original: StructuredDocument = {
      title: "有来源的指南",
      blocks: [
        revision.document.blocks[0],
        {
          block_id: "block-2",
          kind: "paragraph",
          text: "第二个有不同引用的段落",
          citation_ids: ["chunk-2"],
          items: [],
        },
      ],
    };
    const source = documentToEditor(original);
    const merged = editorToDocument(
      {
        type: "doc",
        content: [
          {
            type: "bulletList",
            attrs: { geoBlockId: null, geoCitations: [] },
            content: source.content?.map((paragraph) => ({
              type: "listItem",
              content: [paragraph],
            })),
          },
        ],
      },
      original,
    );
    expect(merged.blocks).toEqual([
      {
        block_id: "block-1",
        kind: "list",
        text: "",
        citation_ids: ["chunk-1", "chunk-2"],
        items: ["原始正文", "第二个有不同引用的段落"],
      },
    ]);
  });

  it("shows source evidence and forks a reused item with an optimistic base", async () => {
    const requests = mockApi({ itemList: [reusedItem, items[1]] });
    renderPage(
      "/app/tenant-1/project-1/content/asset-1?reuse_execution_id=execution-1&reuse_item_id=item-1",
    );
    expect(await screen.findByText("证据原文片段")).toBeInTheDocument();
    expect(screen.getByText("此处依据不足")).toBeInTheDocument();
    expect(screen.getByText(/本轮覆盖独立记录/)).toBeInTheDocument();
    expect(
      requests.mock.calls.some(([url]) =>
        String(url).endsWith(
          "/contents/asset-1?tenant_id=tenant-1&project_id=project-1",
        ),
      ),
    ).toBe(true);
    expect(
      requests.mock.calls.some(([url]) =>
        String(url).endsWith(
          "/document-executions/execution-1/items?tenant_id=tenant-1&project_id=project-1",
        ),
      ),
    ).toBe(true);
    const title = screen.getByRole("textbox", { name: "标题" });
    await userEvent.type(title, "新");
    await userEvent.click(screen.getByRole("button", { name: "创建本轮修订" }));
    await waitFor(() =>
      expect(
        requests.mock.calls.some(
          ([url, init]) =>
            String(url).endsWith(
              "/projects/project-1/document-executions/execution-1/items/item-1/fork?tenant_id=tenant-1&project_id=project-1",
            ) &&
            init?.method === "POST" &&
            JSON.parse(String(init.body)).base_revision_id === "revision-1" &&
            JSON.parse(String(init.body)).document.title === "有来源的指南新",
        ),
      ).toBe(true),
    );
    expect(
      requests.mock.calls.some(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      ),
    ).toBe(false);
    expect(
      await screen.findByText(/此版本从其他资产的版本 revision-1 派生/),
    ).toBeInTheDocument();
  });

  it("keeps editing pinned to the checked source revision if the original asset advances", async () => {
    const requests = mockApi({
      itemList: [reusedItem, items[1]],
      sourceAdvanced: true,
    });
    renderPage(
      "/app/tenant-1/project-1/content/asset-1?reuse_execution_id=execution-1&reuse_item_id=item-1",
    );
    expect(await screen.findByRole("textbox", { name: "标题" })).toHaveValue(
      "有来源的指南",
    );
    await userEvent.type(screen.getByRole("textbox", { name: "标题" }), "本轮");
    await userEvent.click(screen.getByRole("button", { name: "创建本轮修订" }));
    await waitFor(() =>
      expect(
        requests.mock.calls.some(
          ([url, init]) =>
            String(url).includes("/items/item-1/fork") &&
            JSON.parse(String(init?.body)).base_revision_id === "revision-1",
        ),
      ).toBe(true),
    );
    expect(
      requests.mock.calls.some(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      ),
    ).toBe(false);
  });

  it("retains the local draft on a reuse fork conflict", async () => {
    mockApi({ itemList: [reusedItem, items[1]], forkStatus: 409 });
    renderPage(
      "/app/tenant-1/project-1/content/asset-1?reuse_execution_id=execution-1&reuse_item_id=item-1",
    );
    const title = await screen.findByRole("textbox", { name: "标题" });
    await userEvent.type(title, "本地改动");
    await userEvent.click(screen.getByRole("button", { name: "创建本轮修订" }));
    expect(await screen.findByText(/本地输入仍保留/)).toBeInTheDocument();
    expect(title).toHaveValue("有来源的指南本地改动");
  });

  it("does not permit a viewer to fork a reused asset", async () => {
    const requests = mockApi({
      itemList: [reusedItem, items[1]],
      role: "viewer",
    });
    renderPage(
      "/app/tenant-1/project-1/content/asset-1?reuse_execution_id=execution-1&reuse_item_id=item-1",
    );
    expect(await screen.findByText("证据原文片段")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "标题" })).toBeDisabled();
    expect(
      screen.queryByRole("button", { name: "创建本轮修订" }),
    ).not.toBeInTheDocument();
    expect(
      requests.mock.calls.some(([, init]) => init?.method === "POST"),
    ).toBe(false);
  });

  it("refuses to edit an asset through a mismatched current-cycle context", async () => {
    mockApi({ itemList: [items[0], items[1]] });
    renderPage(
      "/app/tenant-1/project-1/content/asset-1?reuse_execution_id=execution-1&reuse_item_id=item-1",
    );
    expect(await screen.findByText("无法核对本轮复用项")).toBeInTheDocument();
    expect(
      screen.queryByRole("textbox", { name: "标题" }),
    ).not.toBeInTheDocument();
  });
  it("renders persisted exact quotes, findings and keyboard-accessible edit", async () => {
    const requests = mockApi();
    renderPage("/app/tenant-1/project-1/content/asset-1");
    expect(
      await screen.findByRole("heading", { name: "有来源的指南" }),
    ).toBeInTheDocument();
    expect(screen.getByText("证据原文片段")).toBeInTheDocument();
    expect(screen.getByText("此处依据不足")).toBeInTheDocument();
    const user = userEvent.setup();
    const title = screen.getByRole("textbox", { name: "标题" });
    await user.click(title);
    await user.keyboard("新");
    await user.click(screen.getByRole("button", { name: "保存新版本" }));
    await waitFor(() =>
      expect(
        requests.mock.calls.some(
          ([url, init]) =>
            String(url).endsWith(
              "/contents/asset-1/revisions?tenant_id=tenant-1&project_id=project-1",
            ) &&
            init?.method === "POST" &&
            JSON.parse(String(init.body)).base_revision_id === "revision-1",
        ),
      ).toBe(true),
    );
  });

  it("saves Tiptap paragraph edits without changing block or citation identities", async () => {
    const requests = mockApi();
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const paragraph = await screen.findByRole("textbox", {
      name: "结构化正文",
    });
    await userEvent.click(paragraph);
    putCaretAtEnd(within(paragraph).getByText("原始正文"));
    await userEvent.keyboard("延展");
    await userEvent.click(screen.getByRole("button", { name: "保存新版本" }));
    await waitFor(() => {
      const save = requests.mock.calls.find(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      );
      expect(save).toBeDefined();
      const payload = JSON.parse(String(save?.[1]?.body));
      expect(payload.base_revision_id).toBe("revision-1");
      expect(payload.document.blocks).toEqual([
        {
          block_id: "block-1",
          kind: "paragraph",
          text: "原始正文延展",
          citation_ids: ["chunk-1"],
          items: [],
        },
      ]);
    });
  });

  it("edits actual list items while preserving caption, citations and list structure", async () => {
    const requests = mockApi({
      documentOverride: {
        title: revision.document.title,
        blocks: [
          ...revision.document.blocks,
          {
            block_id: "block-list",
            kind: "list",
            text: "有来源的项目",
            citation_ids: ["chunk-1"],
            items: ["第一项", "第二项"],
          },
        ],
      },
    });
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const body = await screen.findByRole("textbox", {
      name: "结构化正文",
    });
    const item = within(body).getByText("第一项");
    await userEvent.click(item);
    putCaretAtEnd(item);
    await userEvent.keyboard("{Enter}补充");
    expect(
      Array.from(body.querySelectorAll("li")).map((entry) => entry.textContent),
    ).toEqual(["第一项", "补充", "第二项"]);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "保存新版本" })).toBeEnabled(),
    );
    await userEvent.click(screen.getByRole("button", { name: "保存新版本" }));
    await waitFor(() => {
      const save = requests.mock.calls.find(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      );
      expect(save).toBeDefined();
      expect(JSON.parse(String(save?.[1]?.body)).document.blocks[1]).toEqual({
        block_id: "block-list",
        kind: "list",
        text: "有来源的项目",
        citation_ids: ["chunk-1"],
        items: ["第一项", "补充", "第二项"],
      });
    });
  });

  it("uses native list conversion while retaining the paragraph citation and identity", async () => {
    const requests = mockApi();
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const body = await screen.findByRole("textbox", { name: "结构化正文" });
    await userEvent.click(within(body).getByText("原始正文"));
    await userEvent.click(screen.getByRole("button", { name: "列表" }));
    expect(body.querySelectorAll("li")).toHaveLength(1);
    await userEvent.click(screen.getByRole("button", { name: "保存新版本" }));
    await waitFor(() => {
      const save = requests.mock.calls.find(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      );
      expect(save).toBeDefined();
      expect(JSON.parse(String(save?.[1]?.body)).document.blocks[0]).toEqual({
        block_id: "block-1",
        kind: "list",
        text: "",
        citation_ids: ["chunk-1"],
        items: ["原始正文"],
      });
    });
  });

  it("wraps multiple cited paragraphs with native list commands and restores them on undo", async () => {
    const requests = mockApi({
      documentOverride: {
        title: revision.document.title,
        blocks: [
          revision.document.blocks[0],
          {
            block_id: "block-2",
            kind: "paragraph",
            text: "第二段",
            citation_ids: ["chunk-1"],
            items: [],
          },
        ],
      },
    });
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const body = await screen.findByRole("textbox", { name: "结构化正文" });
    const first = within(body).getByText("原始正文");
    const second = within(body).getByText("第二段");
    await userEvent.click(first);
    const range = document.createRange();
    range.setStart(first.firstChild!, 0);
    range.setEnd(second.firstChild!, second.textContent!.length);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
    await userEvent.click(screen.getByRole("button", { name: "列表" }));
    expect(body.querySelectorAll("li")).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: "撤销" }));
    expect(body.querySelectorAll("li")).toHaveLength(0);
    expect(
      Array.from(body.querySelectorAll("p[data-geo-block-id]")).map(
        (paragraph) => paragraph.getAttribute("data-geo-block-id"),
      ),
    ).toEqual(["block-1", "block-2"]);
    await userEvent.click(screen.getByRole("button", { name: "重做" }));
    expect(body.querySelectorAll("li")).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: "保存新版本" }));
    await waitFor(() => {
      const save = requests.mock.calls.find(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      );
      expect(save).toBeDefined();
      expect(JSON.parse(String(save?.[1]?.body)).document.blocks).toEqual([
        {
          block_id: "block-1",
          kind: "list",
          text: "",
          citation_ids: ["chunk-1"],
          items: ["原始正文", "第二段"],
        },
      ]);
    });
  });

  it("unwraps a native list into paragraphs without discarding its citation", async () => {
    const requests = mockApi({
      documentOverride: {
        title: revision.document.title,
        blocks: [
          revision.document.blocks[0],
          {
            block_id: "block-list",
            kind: "list",
            text: "",
            citation_ids: ["chunk-1"],
            items: ["第一项", "第二项"],
          },
        ],
      },
    });
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const body = await screen.findByRole("textbox", { name: "结构化正文" });
    const firstItem = within(body).getByText("第一项");
    await userEvent.click(firstItem);
    putCaretAtEnd(firstItem);
    await userEvent.click(screen.getByRole("button", { name: "段落" }));
    expect(body.querySelectorAll("li")).toHaveLength(1);
    await userEvent.click(screen.getByRole("button", { name: "保存新版本" }));
    await waitFor(() => {
      const save = requests.mock.calls.find(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      );
      expect(save).toBeDefined();
      const blocks = JSON.parse(String(save?.[1]?.body)).document.blocks;
      expect(blocks[1]).toEqual({
        block_id: "block-list",
        kind: "paragraph",
        text: "第一项",
        citation_ids: ["chunk-1"],
        items: [],
      });
      expect(blocks[2]).toMatchObject({
        kind: "list",
        text: "",
        citation_ids: ["chunk-1"],
        items: ["第二项"],
      });
      expect(blocks[2].block_id).not.toBe("block-list");
    });
  });

  it("pastes unsupported markup as literal text instead of silently storing HTML", async () => {
    const requests = mockApi();
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const paragraph = await screen.findByRole("textbox", {
      name: "结构化正文",
    });
    await userEvent.click(paragraph);
    putCaretAtEnd(within(paragraph).getByText("原始正文"));
    await userEvent.paste("<b>仅纯文本</b>");
    await userEvent.click(screen.getByRole("button", { name: "保存新版本" }));
    await waitFor(() => {
      const save = requests.mock.calls.find(
        ([url, init]) =>
          String(url).includes("/contents/asset-1/revisions") &&
          init?.method === "POST",
      );
      expect(save).toBeDefined();
      expect(JSON.parse(String(save?.[1]?.body)).document.blocks[0].text).toBe(
        "原始正文<b>仅纯文本</b>",
      );
    });
  });

  it("uses the editor's undo and redo history for body changes", async () => {
    mockApi();
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const body = await screen.findByRole("textbox", {
      name: "结构化正文",
    });
    await userEvent.click(body);
    putCaretAtEnd(within(body).getByText("原始正文"));
    await userEvent.keyboard("新");
    expect(within(body).getByText("原始正文新")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "撤销" }));
    expect(within(body).getByText("原始正文")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "重做" }));
    expect(within(body).getByText("原始正文新")).toBeInTheDocument();
  });

  it("preserves unsaved draft after an optimistic conflict", async () => {
    mockApi({ editStatus: 409 });
    renderPage("/app/tenant-1/project-1/content/asset-1");
    const title = await screen.findByRole("textbox", { name: "标题" });
    await userEvent.type(title, "本地改动");
    await userEvent.click(screen.getByRole("button", { name: "保存新版本" }));
    expect(await screen.findByText(/本地输入仍保留/)).toBeInTheDocument();
    expect(title).toHaveValue("有来源的指南本地改动");
    expect(screen.getByRole("button", { name: "保存新版本" })).toBeDisabled();
  });

  it("disables edit controls for a viewer while retaining evidence", async () => {
    const requests = mockApi({ role: "viewer" });
    renderPage("/app/tenant-1/project-1/content/asset-1");
    expect(await screen.findByText("证据原文片段")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "标题" })).toBeDisabled();
    expect(
      screen.queryByRole("button", { name: "保存新版本" }),
    ).not.toBeInTheDocument();
    expect(
      requests.mock.calls.some(([, init]) => init?.method === "POST"),
    ).toBe(false);
  });
});
