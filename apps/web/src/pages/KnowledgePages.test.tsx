import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter } from "react-router-dom";
import { AppRoutes } from "../app";
import { AuthProvider } from "../auth/AuthProvider";
import type { AuthSession } from "../auth/types";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import i18n from "../i18n";

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
  capabilities?: Record<string, unknown>;
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

afterEach(async () => {
  await act(() => i18n.changeLanguage("zh-CN"));
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
    expect(screen.getByText("部分功能暂不可用")).toBeInTheDocument();
    expect(screen.getByText(/扫描件识别/)).toBeInTheDocument();
    expect(screen.getByText(/相似内容检索/)).toBeInTheDocument();
    expect(screen.getAllByText(/知识问答/).length).toBeGreaterThan(0);
    expect(screen.getByText(/网页导入/)).toBeInTheDocument();
    expect(screen.getByText(/PDF 解析/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "导入资料" }));
    expect(screen.getByText(/PDF 解析不可用/)).toBeInTheDocument();
    expect(screen.getByText(/当前无法确认可解析格式/)).toBeInTheDocument();
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

  it("renders typed source PDF locators without dead-end commands", async () => {
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
      await screen.findByText(/PDF 第 12 页 · 原文区域坐标 10, 20, 30, 40/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试失败部分" })).toBeDisabled();
    expect(
      screen.queryByRole("button", { name: "替换文件" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "移除来源" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("返回资料中心").closest("a")).toHaveAttribute(
      "href",
      "/app/tenant-a/project-a/knowledge",
    );
    await user.click(screen.getByText("额定功率为 120W。"));
    expect(screen.getAllByText(/PDF 第 12 页/)).toHaveLength(2);
  });

  it("opens a CSV record as exact cells with its header and logical row locator", async () => {
    const text = JSON.stringify({
      headers: ["型号", "价格"],
      values: ["001", "1.00 元"],
    });
    vi.stubGlobal(
      "fetch",
      requestHandler({
        sourceDetail: {
          source,
          versions: [],
          chunks: [
            {
              chunk_id: "csv-row",
              source_version_id: "version-a",
              ordinal: 0,
              kind: "table",
              text,
              product_ids: [],
              locator: {
                kind: "csv",
                start_row: 2,
                end_row: 2,
                start_column: 1,
                end_column: 2,
                header_row: 1,
              },
            },
          ],
          facts: [],
          import_jobs: [],
          impact: {},
        },
      }),
    );
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");
    await userEvent.click(await screen.findByText(text));
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "001" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "1.00 元" })).toBeInTheDocument();
    expect(screen.getAllByText(/表头记录 1.*第 2–2 条逻辑记录/)).toHaveLength(
      2,
    );
  });

  it("labels a generated CSV cell slice without rendering it as a complete row", async () => {
    const text = JSON.stringify({ values: ["连续片段"] });
    vi.stubGlobal(
      "fetch",
      requestHandler({
        sourceDetail: {
          source,
          versions: [],
          chunks: [
            {
              chunk_id: "csv-slice",
              source_version_id: "version-a",
              ordinal: 1,
              kind: "table",
              text,
              product_ids: [],
              extraction_method: "deterministic_csv_evidence_v1",
              locator: {
                kind: "csv",
                start_row: 2,
                end_row: 2,
                start_column: 2,
                end_column: 2,
                header_row: 1,
                start_char: 4,
                end_char: 8,
              },
            },
          ],
          facts: [],
          import_jobs: [],
          impact: {},
        },
      }),
    );
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");
    await userEvent.click(await screen.findByText(text));
    expect(screen.getByText(/完整记录可在原始片段中查看/)).toBeInTheDocument();
    expect(screen.getAllByText(/单元格字符 4–8/)).toHaveLength(2);
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
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
      // Fluent's focus manager also uses WebCrypto, even when hashing is mocked.
      getRandomValues: crypto.getRandomValues.bind(crypto),
      randomUUID: () => "client-id-" + Math.random(),
      subtle: {
        digest: vi.fn().mockResolvedValue(new Uint8Array(32).buffer),
      },
    });
    const user = userEvent.setup();
    renderPath("/app/tenant-a/project-a/knowledge");
    const heading = await screen.findByRole("heading", { name: "企业知识库" });
    // Keep accessible queries local instead of recomputing the whole app shell.
    await user.click(
      within(heading.closest("section")!).getByRole("button", {
        name: "导入资料",
      }),
    );
    const importPanel = within(
      screen.getByRole("complementary", { name: "导入资料" }),
    );

    const file = new File(["demo"], "manual.txt", { type: "text/plain" });
    Object.defineProperty(file, "arrayBuffer", {
      value: vi.fn().mockResolvedValue(new TextEncoder().encode("demo").buffer),
    });
    await user.upload(importPanel.getByLabelText("选择资料文件"), file);
    // Bulk URL import is a paste interaction; avoid a render per character.
    await user.click(importPanel.getByLabelText("网页地址（每行一个）"));
    await user.paste("https://example.com/a\nhttps://example.com/b");
    await user.click(importPanel.getByRole("button", { name: "开始导入" }));

    expect(
      await importPanel.findByText(/已受理 2 项，失败 1 项/),
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

  it("keeps an accepted PDF pending until polling returns parsed page evidence", async () => {
    let detailReads = 0;
    const queued = {
      ...source,
      name: "sample.pdf",
      purpose: "public",
      current_version_id: null,
      import_status: "queued",
      chunk_count: 0,
      fact_count: 0,
    };
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = routePath(request);
        const method = init?.method ?? "GET";
        if (path.endsWith("/knowledge/upload-sessions") && method === "POST") {
          return Promise.resolve(
            response({
              upload_session_id: "upload-pdf",
              filename: "sample.pdf",
              state: "created",
            }),
          );
        }
        if (path.endsWith("/upload-pdf/content") && method === "PUT")
          return Promise.resolve(response(undefined, 204));
        if (path.endsWith("/upload-pdf/complete") && method === "POST")
          return Promise.resolve(
            response(
              {
                status: "queued",
                source: queued,
                import_job: {
                  import_job_id: "job-pdf",
                  source_id: queued.source_id,
                  status: "queued",
                  stage: "parse",
                  completed_units: 0,
                  failed_units: 0,
                },
                source_version: null,
                release: null,
              },
              202,
            ),
          );
        if (path.endsWith("/knowledge/sources/source-a")) {
          detailReads++;
          const ready = detailReads > 1;
          return Promise.resolve(
            response({
              source: ready
                ? {
                    ...queued,
                    current_version_id: "version-pdf",
                    import_status: "succeeded",
                    chunk_count: 1,
                  }
                : queued,
              versions: ready
                ? [
                    {
                      source_version_id: "version-pdf",
                      source_id: queued.source_id,
                      version: 1,
                    },
                  ]
                : [],
              chunks: ready
                ? [
                    {
                      chunk_id: "page-one",
                      source_version_id: "version-pdf",
                      ordinal: 0,
                      kind: "paragraph",
                      text: "已提取的 PDF 正文",
                      locator: { kind: "pdf", page: 1 },
                      product_ids: [],
                    },
                  ]
                : [],
              facts: [],
              import_jobs: [
                {
                  import_job_id: "job-pdf",
                  source_id: queued.source_id,
                  status: ready ? "succeeded" : "queued",
                  stage: "parse",
                  completed_units: ready ? 1 : 0,
                  failed_units: 0,
                  errors: [],
                },
              ],
              impact: {},
            }),
          );
        }
        return requestHandler({
          sourceItems: [queued],
          capabilities: {
            ...defaultCapabilities({ ocr: false }),
            pdf_parser: true,
            supported_media_types: ["application/pdf"],
          },
        })(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("crypto", {
      getRandomValues: crypto.getRandomValues.bind(crypto),
      randomUUID: () => "pdf-request",
      subtle: {
        digest: vi.fn().mockResolvedValue(new Uint8Array(32).buffer),
      },
    });
    const user = userEvent.setup();
    renderPath("/app/tenant-a/project-a/knowledge");
    await user.click(await screen.findByRole("button", { name: "导入资料" }));
    const file = new File(["%PDF"], "sample.pdf", { type: "application/pdf" });
    Object.defineProperty(file, "arrayBuffer", {
      value: vi.fn().mockResolvedValue(new TextEncoder().encode("%PDF").buffer),
    });
    await user.upload(screen.getByLabelText("选择资料文件"), file);
    await user.click(screen.getByRole("button", { name: "开始导入" }));
    expect((await screen.findAllByText("等待解析")).length).toBeGreaterThan(0);
    expect(screen.queryByText("解析完成")).not.toBeInTheDocument();
    await user.click(await screen.findByRole("link", { name: "查看处理详情" }));
    expect(
      await screen.findByText(/等待解析。已完成 0 页，失败 0 页/),
    ).toBeInTheDocument();
    expect(screen.queryByText("已提取的 PDF 正文")).not.toBeInTheDocument();
    expect(
      await screen.findByText("已提取的 PDF 正文", {}, { timeout: 5000 }),
    ).toBeInTheDocument();
    expect(screen.getByText("PDF 第 1 页")).toBeInTheDocument();
    expect(detailReads).toBeGreaterThanOrEqual(2);
  }, 8000);

  it("retries only a partial job once and keeps old page evidence visible", async () => {
    let retried = false;
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = routePath(request);
        if (path.endsWith("/knowledge/import-jobs/job-partial/retry")) {
          retried = true;
          return Promise.resolve(
            response(
              {
                import_job_id: "job-partial",
                source_id: "source-a",
                status: "queued",
                stage: "parse",
                completed_units: 1,
                failed_units: 1,
                errors: [],
              },
              202,
            ),
          );
        }
        return requestHandler({
          sourceDetail: {
            source: {
              ...source,
              current_version_id: "version-a",
              import_status: retried ? "queued" : "partial",
            },
            versions: [
              {
                source_version_id: "version-a",
                source_id: "source-a",
                version: 2,
              },
            ],
            chunks: [
              {
                chunk_id: "page-one",
                source_version_id: "version-a",
                ordinal: 0,
                kind: "paragraph",
                text: "上一轮保存的第 1 页证据",
                locator: { kind: "pdf", page: 1 },
                product_ids: [],
              },
            ],
            facts: [],
            import_jobs: [
              {
                import_job_id: "job-partial",
                source_id: "source-a",
                stage: "parse",
                status: retried ? "queued" : "partial",
                completed_units: 1,
                failed_units: 1,
                errors: retried
                  ? []
                  : [
                      {
                        page: 2,
                        code: "ocr_required",
                        message: "private raw provider error",
                      },
                    ],
              },
            ],
            impact: {},
          },
        })(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");
    expect(
      await screen.findByText(
        "第 2 页：该页没有可提取的文字；扫描内容需要 OCR",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("上一轮保存的第 1 页证据")).toBeInTheDocument();
    expect(
      screen.queryByText("private raw provider error"),
    ).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "重试失败部分" }));
    await waitFor(() =>
      expect(screen.getByText(/重试请求已受理/)).toBeInTheDocument(),
    );
    await waitFor(() =>
      expect(screen.getByText("上一轮保存的第 1 页证据")).toBeInTheDocument(),
    );
    const retries = fetchMock.mock.calls.filter(([request]) =>
      routePath(request).endsWith("/knowledge/import-jobs/job-partial/retry"),
    );
    expect(retries).toHaveLength(1);
    const retryUrl = new URL(String(retries[0][0]), "http://localhost");
    expect(retryUrl.searchParams.get("tenant_id")).toBe("tenant-a");
    expect(retryUrl.searchParams.get("project_id")).toBe("project-a");
    expect(retries[0][1]?.method).toBe("POST");
    const headers = new Headers(retries[0][1]?.headers);
    expect(headers.get("X-CSRF-Token")).toBe("csrf-a");
    expect(headers.has("Idempotency-Key")).toBe(true);
  });

  it("disables retry for a viewer even if the latest PDF job failed", async () => {
    const viewerSession = {
      ...session,
      memberships: [{ ...session.memberships[0], role: "viewer" }],
    };
    vi.stubGlobal(
      "fetch",
      vi.fn((request: RequestInfo | URL, init?: RequestInit) => {
        if (routePath(request).endsWith("/auth/session"))
          return Promise.resolve(response(viewerSession));
        return requestHandler({
          sourceDetail: {
            source: { ...source, import_status: "failed" },
            versions: [],
            chunks: [],
            facts: [],
            import_jobs: [
              {
                import_job_id: "job-failed",
                source_id: "source-a",
                stage: "parse",
                status: "failed",
                completed_units: 0,
                failed_units: 1,
                errors: [
                  { code: "invalid_pdf", message: "private stack trace" },
                ],
              },
            ],
            impact: {},
          },
        })(request, init);
      }),
    );
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");
    expect(
      await screen.findByText(/PDF 文件无效或无法读取/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重试失败部分" })).toBeDisabled();
    expect(screen.queryByText("private stack trace")).not.toBeInTheDocument();
  });

  it("shows DOCX heading, paragraph and actual table spans without creating missing cells", async () => {
    vi.stubGlobal(
      "fetch",
      requestHandler({
        sourceDetail: {
          source: {
            ...source,
            name: "manual.docx",
            current_version_id: "docx-v2",
            import_status: "partial",
          },
          versions: [
            { source_version_id: "docx-v1", source_id: "source-a", version: 1 },
            { source_version_id: "docx-v2", source_id: "source-a", version: 2 },
          ],
          chunks: [
            {
              chunk_id: "old",
              source_version_id: "docx-v1",
              ordinal: 0,
              kind: "paragraph",
              text: "旧证据仍可查看",
              product_ids: [],
              locator: { kind: "docx", heading_path: [], paragraph_index: 0 },
            },
            {
              chunk_id: "heading",
              source_version_id: "docx-v2",
              ordinal: 0,
              kind: "paragraph",
              text: "规格说明",
              product_ids: [],
              locator: {
                kind: "docx",
                heading_path: ["第二章", "规格"],
                paragraph_index: 2,
              },
            },
            {
              chunk_id: "cell-a",
              source_version_id: "docx-v2",
              ordinal: 1,
              kind: "table",
              text: "型号",
              product_ids: [],
              locator: {
                kind: "docx",
                heading_path: ["第二章"],
                paragraph_index: 4,
                body_element_index: 4,
                table_index: 0,
                table_row: 0,
                table_column: 0,
                table_row_span: 2,
                table_col_span: 1,
                table_merged: true,
              },
            },
            {
              chunk_id: "cell-b",
              source_version_id: "docx-v2",
              ordinal: 2,
              kind: "table",
              text: "A-100",
              product_ids: [],
              locator: {
                kind: "docx",
                heading_path: ["第二章"],
                paragraph_index: 4,
                body_element_index: 4,
                table_index: 0,
                table_row: 0,
                table_column: 1,
                table_row_span: 1,
                table_col_span: 1,
                table_merged: false,
              },
            },
          ],
          facts: [],
          import_jobs: [
            {
              import_job_id: "docx-job",
              source_id: "source-a",
              status: "partial",
              stage: "release",
              completed_units: 2,
              failed_units: 1,
              errors: [
                {
                  unit_id: 2,
                  format: "docx",
                  code: "parse_failed",
                  message: "private parser details",
                },
              ],
            },
          ],
          impact: {},
        },
      }),
    );
    const user = userEvent.setup();
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");
    expect(
      (await screen.findAllByText(/已完成 2 个正文区块，失败 1 个正文区块/))
        .length,
    ).toBeGreaterThan(0);
    expect(screen.getByText(/解析单元 3：该解析单元失败/)).toBeInTheDocument();
    expect(
      screen.queryByText("private parser details"),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/第二章 › 规格 · 段落 3/)).toBeInTheDocument();
    expect(screen.queryByText("旧证据仍可查看")).not.toBeInTheDocument();
    await user.click(screen.getByText("型号"));
    const cell = screen.getByRole("cell", { name: "第 1 行第 1 列" });
    expect(cell).toHaveAttribute("rowspan", "2");
    expect(
      screen.getByRole("cell", { name: "第 1 行第 2 列" }),
    ).toHaveTextContent("A-100");
    expect(
      screen.queryByRole("cell", { name: "第 2 行第 1 列" }),
    ).not.toBeInTheDocument();
    await user.selectOptions(
      screen.getByRole("combobox", { name: "查看证据版本" }),
      "docx-v1",
    );
    expect(screen.getByText("旧证据仍可查看")).toBeInTheDocument();
    expect(screen.queryByText("规格说明")).not.toBeInTheDocument();
    expect(
      screen.getByText(/正在查看历史版本的已保存内容/),
    ).toBeInTheDocument();
  });

  it("shows XLSX worksheet A1 values, cached formula missing, and real metadata only", async () => {
    const formula = '=HYPERLINK("https://example.invalid","open")';
    vi.stubGlobal(
      "fetch",
      requestHandler({
        sourceDetail: {
          source: {
            ...source,
            name: "data.xlsx",
            current_version_id: "xlsx-v1",
            import_status: "succeeded",
          },
          versions: [
            { source_version_id: "xlsx-v1", source_id: "source-a", version: 1 },
          ],
          chunks: [
            {
              chunk_id: "xlsx-a",
              source_version_id: "xlsx-v1",
              ordinal: 0,
              kind: "table",
              text: "001",
              product_ids: [],
              locator: {
                kind: "xlsx",
                sheet: "Prices",
                range: "A12",
                cell_kind: "string",
              },
            },
            {
              chunk_id: "xlsx-b",
              source_version_id: "xlsx-v1",
              ordinal: 1,
              kind: "table",
              text: formula,
              product_ids: [],
              locator: {
                kind: "xlsx",
                sheet: "Prices",
                range: "B12",
                cell_kind: "formula_cached",
                formula,
              },
            },
            {
              chunk_id: "xlsx-c",
              source_version_id: "xlsx-v1",
              ordinal: 2,
              kind: "table",
              text: "12.50",
              product_ids: [],
              locator: {
                kind: "xlsx",
                sheet: "Prices",
                range: "C12",
                cell_kind: "number",
                display_value: "$12.50",
              },
            },
          ],
          facts: [],
          import_jobs: [
            {
              import_job_id: "xlsx-job",
              source_id: "source-a",
              stage: "release",
              status: "succeeded",
              completed_units: 1,
              failed_units: 0,
              errors: [],
            },
          ],
          impact: {},
        },
      }),
    );
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");
    expect(
      await screen.findByText(/已解析 1 个工作表行区块/),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByText("12.50"));
    expect(screen.getByRole("table")).toHaveTextContent("A12");
    expect(screen.getByRole("table")).toHaveTextContent("原值：001");
    expect(screen.getByRole("table")).toHaveTextContent("显示值：$12.50");
    expect(screen.getByRole("table")).toHaveTextContent(
      "缓存结果：缺失（未重新计算）",
    );
    expect(screen.getByRole("table")).toHaveTextContent("原值：（无缓存原值）");
    expect(screen.getByRole("table")).not.toHaveTextContent(`原值：${formula}`);
    expect(screen.getByRole("table")).toHaveTextContent(formula);
    expect(screen.getByRole("table")).not.toHaveTextContent("实际表头范围");
    expect(
      screen.queryByRole("link", { name: "open" }),
    ).not.toBeInTheDocument();
  });

  it("accepts Office upload as queued evidence, not an already released document", async () => {
    const pending = {
      ...source,
      name: "manual.docx",
      import_status: "queued",
      current_version_id: null,
      chunk_count: 0,
    };
    const fetchMock = vi.fn(
      (request: RequestInfo | URL, init?: RequestInit) => {
        const path = routePath(request);
        if (
          path.endsWith("/knowledge/upload-sessions") &&
          init?.method === "POST"
        ) {
          expect(JSON.parse(String(init.body)).declared_media_type).toBe(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
          );
          return Promise.resolve(
            response({ upload_session_id: "office-upload", state: "created" }),
          );
        }
        if (path.endsWith("/office-upload/content") && init?.method === "PUT")
          return Promise.resolve(response(undefined, 204));
        if (path.endsWith("/office-upload/complete") && init?.method === "POST")
          return Promise.resolve(
            response(
              {
                status: "queued",
                source: pending,
                source_version: null,
                release: null,
                import_job: {
                  import_job_id: "office-job",
                  source_id: "source-a",
                  stage: "parse",
                  status: "queued",
                  completed_units: 0,
                  failed_units: 0,
                },
              },
              202,
            ),
          );
        return requestHandler({
          sourceItems: [pending],
          capabilities: {
            ...defaultCapabilities({ ocr: false }),
            docx_parser: true,
            xlsx_parser: false,
            supported_media_types: [
              "text/plain",
              "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ],
          },
        })(request, init);
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("crypto", {
      getRandomValues: crypto.getRandomValues.bind(crypto),
      randomUUID: () => "office-request",
      subtle: { digest: vi.fn().mockResolvedValue(new Uint8Array(32).buffer) },
    });
    renderPath("/app/tenant-a/project-a/knowledge");
    await userEvent.click(
      await screen.findByRole("button", { name: "导入资料" }),
    );
    expect(screen.getByText(/文件上传后仍需解析/)).toBeInTheDocument();
    expect(screen.getByText(/XLSX 解析不可用/)).toBeInTheDocument();
    const file = new File(["synthetic"], "manual.docx", { type: "" });
    Object.defineProperty(file, "arrayBuffer", {
      value: vi
        .fn()
        .mockResolvedValue(new TextEncoder().encode("synthetic").buffer),
    });
    await userEvent.upload(screen.getByLabelText("选择资料文件"), file);
    await userEvent.click(screen.getByRole("button", { name: "开始导入" }));
    expect((await screen.findAllByText("等待解析")).length).toBeGreaterThan(0);
    expect(screen.queryByText("解析完成")).not.toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(([request]) =>
        routePath(request).endsWith("/office-upload/complete"),
      ),
    ).toBe(true);
  });

  it("shows English source progress, failed-page details, and available saved versions", async () => {
    await act(() => i18n.changeLanguage("en"));
    vi.stubGlobal(
      "fetch",
      requestHandler({
        sourceDetail: {
          source: {
            ...source,
            current_version_id: "version-a",
            import_status: "partial",
          },
          versions: [
            {
              source_version_id: "version-old",
              source_id: "source-a",
              version: 1,
            },
            {
              source_version_id: "version-a",
              source_id: "source-a",
              version: 2,
            },
          ],
          chunks: [
            {
              chunk_id: "saved-page",
              source_version_id: "version-a",
              ordinal: 0,
              kind: "paragraph",
              text: "Saved text from the first page",
              locator: { kind: "pdf", page: 1 },
              product_ids: [],
            },
          ],
          facts: [],
          import_jobs: [
            {
              import_job_id: "job-partial",
              source_id: "source-a",
              stage: "parse",
              status: "partial",
              completed_units: 1,
              failed_units: 1,
              errors: [
                {
                  page: 2,
                  code: "ocr_required",
                  message: "private parser details",
                },
              ],
            },
          ],
          impact: {},
        },
      }),
    );
    renderPath("/app/tenant-a/project-a/knowledge/sources/source-a");

    expect(await screen.findByText("Partially parsed")).toBeInTheDocument();
    expect(
      screen.getByText(/Completed 1 page; failed 1 page/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Page 2: No extractable text on this page; scanned content needs OCR",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Saved text from the first page"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Retry failed parts" }),
    ).toBeEnabled();
    expect(
      screen.getByRole("combobox", { name: "View evidence version" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("private parser details"),
    ).not.toBeInTheDocument();
  });
});
