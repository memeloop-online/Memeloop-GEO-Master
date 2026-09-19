import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
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

const source = {
  source_id: "source-a",
  revision: 2,
  kind: "file",
  name: "内部产品手册.pdf",
  purpose: "internal",
  state: "active",
  product_ids: ["product-a"],
  product_names: ["Northstar Pro"],
  import_status: "succeeded",
  chunk_count: 3,
  fact_count: 1,
  last_sync_at: "2026-09-18T10:00:00Z",
};

function response(body: unknown, status = 200) {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function routePath(request: RequestInfo | URL) {
  return new URL(String(request), "http://localhost").pathname;
}

function defaultCapabilities(overrides: Record<string, boolean> = {}) {
  return {
    ocr: true,
    vector_search: true,
    llm_answering: true,
    url_fetch: true,
    ...overrides,
  };
}

function requestHandler({
  capabilities = defaultCapabilities(),
  sourceItems = [source],
  sourceDetail,
  ask,
}: {
  capabilities?: Record<string, boolean>;
  sourceItems?: unknown[];
  sourceDetail?: unknown;
  ask?: unknown;
} = {}) {
  return vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
    const path = routePath(request);
    const method = init?.method ?? "GET";
    if (path.endsWith("/auth/session"))
      return Promise.resolve(response(session));
    if (path.endsWith("/projects")) {
      return Promise.resolve(response({ items: [], next_cursor: null }));
    }
    if (path.endsWith("/knowledge/capabilities")) {
      return Promise.resolve(response(capabilities));
    }
    if (path.endsWith("/knowledge/sources/source-a")) {
      return Promise.resolve(
        response(
          sourceDetail ?? {
            source,
            versions: [
              {
                source_version_id: "version-a",
                source_id: "source-a",
                version: 2,
                content_sha256: "a".repeat(64),
              },
            ],
            chunks: [],
            facts: [],
            import_jobs: [],
            impact: {},
          },
        ),
      );
    }
    if (path.endsWith("/knowledge/sources")) {
      return Promise.resolve(
        response({ items: sourceItems, next_cursor: null }),
      );
    }
    if (path.endsWith("/knowledge/products")) {
      return Promise.resolve(
        response({
          items: [
            {
              product_id: "product-a",
              revision: 1,
              name: "Northstar Pro",
              aliases: [],
              state: "active",
              evidence_refs: [],
            },
          ],
          next_cursor: null,
        }),
      );
    }
    if (path.endsWith("/knowledge/facts")) {
      return Promise.resolve(response({ items: [], next_cursor: null }));
    }
    if (path.endsWith("/knowledge/releases/current")) {
      return Promise.resolve(
        response({ knowledge_release_id: "release-a", sequence: 3 }),
      );
    }
    if (path.endsWith("/knowledge/ask") && method === "POST") {
      return Promise.resolve(
        response(ask ?? { status: "answered", evidence: [] }),
      );
    }
    return Promise.resolve(response({}));
  });
}

function renderPath(path: string) {
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

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("knowledge workbench", () => {
  it("shows the P03 empty state and all missing processing capabilities", async () => {
    vi.stubGlobal(
      "fetch",
      requestHandler({
        sourceItems: [],
        capabilities: defaultCapabilities({
          ocr: false,
          vector_search: false,
          llm_answering: false,
          url_fetch: false,
        }),
      }),
    );

    renderPath("/app/tenant-a/project-a/knowledge");

    expect(
      await screen.findByRole("heading", { name: "企业知识库" }),
    ).toBeInTheDocument();
    expect(await screen.findByText("还没有资料")).toBeInTheDocument();
    expect(screen.getByText(/OCR 扫描识别/)).toBeInTheDocument();
    expect(screen.getByText(/向量检索/)).toBeInTheDocument();
    expect(screen.getByText(/LLM 问答/)).toBeInTheDocument();
    expect(screen.getByText(/网页抓取/)).toBeInTheDocument();
  });

  it("renders source data and marks internal material instead of treating it as public", async () => {
    const fetchMock = requestHandler();
    vi.stubGlobal("fetch", fetchMock);

    renderPath("/app/tenant-a/project-a/knowledge");

    expect(await screen.findByText("内部产品手册.pdf")).toBeInTheDocument();
    expect(screen.getByText("内部资料")).toBeInTheDocument();
    expect(screen.getByText("3 / 1")).toBeInTheDocument();
    for (const path of [
      "/knowledge/sources",
      "/knowledge/products",
      "/knowledge/facts",
    ]) {
      const call = fetchMock.mock.calls.find(
        ([request]) =>
          new URL(String(request), "http://localhost").pathname ===
          `/api/v1${path}`,
      );
      expect(call).toBeDefined();
      const url = new URL(String(call?.[0]), "http://localhost");
      expect([...url.searchParams.keys()]).toEqual(["tenant_id", "project_id"]);
      expect(url.searchParams.get("tenant_id")).toBe("tenant-a");
      expect(url.searchParams.get("project_id")).toBe("project-a");
    }
  });

  it("renders typed P04 PDF locators and leaves unimplemented commands disabled", async () => {
    vi.stubGlobal(
      "fetch",
      requestHandler({
        sourceDetail: {
          source,
          versions: [
            {
              source_version_id: "version-a",
              source_id: "source-a",
              version: 2,
            },
          ],
          chunks: [
            {
              chunk_id: "chunk-a",
              source_version_id: "version-a",
              ordinal: 0,
              kind: "paragraph",
              text: "额定功率为 120W。",
              product_ids: ["product-a"],
              locator: { kind: "pdf", page: 12, bbox: [10, 20, 30, 40] },
            },
          ],
          facts: [],
          import_jobs: [],
          impact: {},
        },
      }),
    );
    const user = userEvent.setup();
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");

    expect(
      await screen.findByText(/PDF 第 12 页 · 高亮区域 10, 20, 30, 40/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "替换文件" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "重试失败部分" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "移除来源" })).toBeDisabled();
    expect(screen.getByText("返回资料中心").closest("a")).toHaveAttribute(
      "href",
      "/app/tenant-a/project-a/knowledge",
    );
    await user.click(screen.getByText("额定功率为 120W。"));
    expect(screen.getAllByText(/PDF 第 12 页/)).toHaveLength(2);
  });

  it("uses evidence-only mode and reports insufficient evidence without invented prose", async () => {
    vi.stubGlobal(
      "fetch",
      requestHandler({
        capabilities: defaultCapabilities({ llm_answering: false }),
        ask: {
          answer_status: "insufficient_evidence",
          mode: "evidence_only",
          answer: null,
          missing: ["当前价格表"],
          evidence: [
            {
              source_id: "source-a",
              source_name: "内部产品手册.pdf",
              chunk_id: "chunk-a",
              locator: { kind: "text", line_start: 5, line_end: 6 },
            },
          ],
        },
      }),
    );
    const user = userEvent.setup();
    renderPath("/app/tenant-a/project-a/knowledge/ask");

    expect(await screen.findByText("证据摘录模式")).toBeInTheDocument();
    await user.type(
      screen.getByRole("textbox", { name: "你的问题" }),
      "当前价格是多少？",
    );
    await user.click(screen.getByRole("button", { name: "查找证据" }));

    expect(await screen.findByText("当前资料没有这项信息")).toBeInTheDocument();
    expect(screen.getByText("当前价格表")).toBeInTheDocument();
    expect(screen.getByText(/第 5–6 行/)).toBeInTheDocument();
    expect(screen.queryByText("基于当前资料的回答")).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: /内部产品手册.pdf/ }),
    ).toHaveAttribute(
      "href",
      "/app/tenant-a/project-a/knowledge/sources/source-a",
    );
  });

  it("labels an evidence-only match as a quoted excerpt rather than an answer", async () => {
    vi.stubGlobal(
      "fetch",
      requestHandler({
        capabilities: defaultCapabilities({ llm_answering: false }),
        ask: {
          answer_status: "answered",
          mode: "evidence_only",
          answer: "额定功率为 120W。",
          evidence: [
            {
              source_id: "source-a",
              source_name: "内部产品手册.pdf",
              chunk_id: "chunk-a",
              locator: { kind: "text", line_start: 5, line_end: 6 },
            },
          ],
        },
      }),
    );
    const user = userEvent.setup();
    renderPath("/app/tenant-a/project-a/knowledge/ask");

    await user.type(
      await screen.findByRole("textbox", { name: "你的问题" }),
      "额定功率是多少？",
    );
    await user.click(screen.getByRole("button", { name: "查找证据" }));

    expect(await screen.findByText("已找到匹配证据")).toBeInTheDocument();
    expect(screen.getByText("证据摘录：")).toBeInTheDocument();
    expect(screen.queryByText("基于当前资料的回答")).not.toBeInTheDocument();
  });

  it("runs the actual file-upload request sequence and reports partial import results", async () => {
    let uploadSession = 0;
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = routePath(request);
        const method = init?.method ?? "GET";
        if (path.endsWith("/knowledge/upload-sessions") && method === "POST") {
          uploadSession += 1;
          return Promise.resolve(
            response({
              upload_session_id: `upload-${uploadSession}`,
              filename: "manual.txt",
              expected_size: 4,
              purpose: "public",
              state: "created",
            }),
          );
        }
        if (path.endsWith("/content") && method === "PUT") {
          return Promise.resolve(response(undefined, 204));
        }
        if (path.endsWith("/complete") && method === "POST") {
          return Promise.resolve(
            response({ status: "queued", import_job: {} }, 202),
          );
        }
        if (path.endsWith("/knowledge/imports") && method === "POST") {
          const body = JSON.parse(String(init?.body)) as {
            items: Array<{ client_item_id: string }>;
          };
          return Promise.resolve(
            response(
              {
                items: body.items.map((item, index) => ({
                  client_item_id: item.client_item_id,
                  status: index === 0 ? "queued" : "failed",
                  error: index === 0 ? null : { message: "URL 抓取能力缺失" },
                })),
              },
              202,
            ),
          );
        }
        return requestHandler({ sourceItems: [] })(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("crypto", {
      randomUUID: () => "client-id-" + Math.random(),
      subtle: {
        digest: vi.fn().mockResolvedValue(new Uint8Array(32).buffer),
      },
    });
    const user = userEvent.setup();
    renderPath("/app/tenant-a/project-a/knowledge");
    await screen.findByRole("heading", { name: "企业知识库" });
    await user.click(screen.getByRole("button", { name: "导入资料" }));

    const file = new File(["demo"], "manual.txt", { type: "text/plain" });
    Object.defineProperty(file, "arrayBuffer", {
      value: vi.fn().mockResolvedValue(new TextEncoder().encode("demo").buffer),
    });
    await user.upload(screen.getByLabelText("选择资料文件"), file);
    await user.type(
      screen.getByRole("textbox", { name: "多个网页 URL" }),
      "https://example.com/a\nhttps://example.com/b",
    );
    await user.click(screen.getByRole("button", { name: "开始导入" }));

    expect(
      await screen.findByText(/已受理 2 项，失败 1 项/),
    ).toBeInTheDocument();
    const fileRequests = fetchMock.mock.calls
      .filter(([request]) =>
        routePath(request).includes("/knowledge/upload-sessions"),
      )
      .map(
        ([request, init]) => `${init?.method ?? "GET"} ${routePath(request)}`,
      );
    expect(fileRequests).toEqual([
      "POST /api/v1/knowledge/upload-sessions",
      "PUT /api/v1/knowledge/upload-sessions/upload-1/content",
      "POST /api/v1/knowledge/upload-sessions/upload-1/complete",
    ]);
    const createRequest = fetchMock.mock.calls.find(
      ([request, init]) =>
        routePath(request).endsWith("/knowledge/upload-sessions") &&
        init?.method === "POST",
    );
    expect(
      JSON.parse(String((createRequest?.[1] as RequestInit).body)),
    ).toMatchObject({
      filename: "manual.txt",
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
});
