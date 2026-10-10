import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type { SourceSummary } from "../api/knowledge";
import "../i18n";
import {
  preserveSourceLineEndings,
  SourceTextRevisionPanel,
} from "./SourceTextRevisionPanel";

const source: SourceSummary = {
  source_id: "source-a",
  revision: 2,
  kind: "text",
  name: "资料",
  purpose: "internal",
  state: "active",
  current_version_id: "version-a",
  product_ids: [],
};

const session = {
  user: { id: "user-a", login_name: "local", display_name: "Local" },
  operator: { id: "operator-a", slug: "local", display_name: "Local" },
  memberships: [
    {
      tenant_id: "tenant-a",
      tenant_slug: "local",
      tenant_display_name: "Local",
      role: "tenant_admin",
    },
  ],
  expires_at: "2026-12-01T00:00:00Z",
  csrf_token: "csrf-a",
} as const;

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function setup(
  handler: (
    request: RequestInfo | URL,
    init?: RequestInit,
  ) => Promise<Response>,
) {
  const fetchMock = vi.fn((request: RequestInfo | URL, init?: RequestInit) =>
    String(request).includes("/auth/session")
      ? Promise.resolve(response(session))
      : handler(request, init),
  );
  vi.stubGlobal("fetch", fetchMock);
  const rendered = render(
    <FluentProvider theme={webLightTheme}>
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <AuthProvider>
          <SourceTextRevisionPanel
            tenantId="tenant-a"
            projectId="project-a"
            source={source}
            selectedVersionId="version-a"
            canEdit
            onViewLatest={() => undefined}
          />
        </AuthProvider>
      </QueryClientProvider>
    </FluentProvider>,
  );
  return { ...rendered, fetchMock };
}

const rangeRectsDescriptor = Object.getOwnPropertyDescriptor(
  Range.prototype,
  "getClientRects",
);
const rangeBoundsDescriptor = Object.getOwnPropertyDescriptor(
  Range.prototype,
  "getBoundingClientRect",
);

beforeEach(() => {
  // ProseMirror scrolls to the selection; jsdom does not implement Range geometry.
  Object.defineProperty(Range.prototype, "getClientRects", {
    configurable: true,
    value: () => [document.body.getBoundingClientRect()],
  });
  Object.defineProperty(Range.prototype, "getBoundingClientRect", {
    configurable: true,
    value: () => document.body.getBoundingClientRect(),
  });
});

afterEach(() => {
  cleanup();
  if (rangeRectsDescriptor)
    Object.defineProperty(
      Range.prototype,
      "getClientRects",
      rangeRectsDescriptor,
    );
  else delete (Range.prototype as Partial<Range>).getClientRects;
  if (rangeBoundsDescriptor)
    Object.defineProperty(
      Range.prototype,
      "getBoundingClientRect",
      rangeBoundsDescriptor,
    );
  else delete (Range.prototype as Partial<Range>).getBoundingClientRect;
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("knowledge source text revision", () => {
  it("renders supported Markdown without normalizing or saving untouched source bytes", async () => {
    const markdown =
      "# 标题\r\n\r\n- **第一项**\r\n\r\n| 列 | 值 |\r\n| --- | --- |\r\n| 甲 | 中文 |\r\n";
    const { fetchMock } = setup((request) =>
      Promise.resolve(
        new URL(String(request), "http://localhost").pathname.endsWith(
          "/versions/version-a/content",
        )
          ? response({
              source_version_id: "version-a",
              representation: "original",
              media_type: "text/markdown",
              text: markdown,
              text_basis: "exact",
            })
          : response({}),
      ),
    );
    const visual = await screen.findByRole("textbox", {
      name: "资料正文（排版编辑）",
    });
    expect(visual.querySelector("h1")).toHaveTextContent("标题");
    expect(visual.querySelector("strong")).toHaveTextContent("第一项");
    expect(visual.querySelector("table")).toHaveTextContent("中文");
    expect(screen.getByRole("button", { name: "保存为新版本" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "原文编辑" }));
    expect(
      screen.getByRole<HTMLTextAreaElement>("textbox", {
        name: "资料正文（原始文本）",
      }).value,
    ).toBe(markdown.replaceAll("\r\n", "\n"));
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "POST"),
    ).toHaveLength(0);
    expect(preserveSourceLineEndings("甲\n乙\n", "甲\r\n乙\r\n")).toBe(
      "甲\r\n乙\r\n",
    );
  });

  it("uses Fluent's arrow-navigation group and reflects selected formatting", async () => {
    setup((request) =>
      Promise.resolve(
        new URL(String(request), "http://localhost").pathname.endsWith(
          "/versions/version-a/content",
        )
          ? response({
              source_version_id: "version-a",
              representation: "original",
              media_type: "text/markdown",
              text: "# 标题\n\n**粗体** 正文",
              text_basis: "exact",
            })
          : response({}),
      ),
    );
    await screen.findByRole("textbox", { name: "资料正文（排版编辑）" });
    const heading = screen.getByRole("button", { name: "标题" });
    const bold = screen.getByRole("button", { name: "加粗" });
    const toolbar = screen.getByRole("toolbar", { name: "资料编辑" });
    expect(toolbar).toContainElement(heading);
    expect(toolbar.getAttribute("data-tabster")).toContain("mover");
    expect(heading).toHaveAttribute("aria-pressed", "false");
    heading.focus();
    await userEvent.keyboard("{ArrowRight}");
    await userEvent.click(bold);
    await waitFor(() => expect(bold).toHaveAttribute("aria-pressed", "true"));
  });

  it("prefills an existing link and rejects unsafe schemes using the editor's link command", async () => {
    setup((request) =>
      Promise.resolve(
        new URL(String(request), "http://localhost").pathname.endsWith(
          "/versions/version-a/content",
        )
          ? response({
              source_version_id: "version-a",
              representation: "original",
              media_type: "text/markdown",
              text: '[链接](https://example.org "说明")',
              text_basis: "exact",
            })
          : response({}),
      ),
    );
    const visual = await screen.findByRole("textbox", {
      name: "资料正文（排版编辑）",
    });
    expect(visual.querySelector("a")).toHaveAttribute(
      "href",
      "https://example.org",
    );
    const saveButton = screen.getByRole("button", { name: "保存为新版本" });
    expect(saveButton).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "添加链接" }));
    const surface = document.querySelector(
      ".source-link-popover",
    ) as HTMLElement;
    expect(surface).toBeInTheDocument();
    const url = within(surface).getByRole<HTMLInputElement>("textbox", {
      name: "链接地址",
    });
    expect(url.value).toBe("https://example.org");
    const applyLink = within(surface).getByRole("button", { name: "添加链接" });
    fireEvent.change(url, { target: { value: "javascript:alert(1)" } });
    fireEvent.click(applyLink);
    expect(within(surface).getByRole("alert")).toHaveTextContent(
      "链接地址不受支持",
    );
    expect(visual.querySelector("a")).toHaveAttribute(
      "href",
      "https://example.org",
    );
    expect((saveButton as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(url, { target: { value: "/updated" } });
    fireEvent.click(applyLink);
    expect(visual.querySelector("a")).toHaveAttribute("href", "/updated");
    expect(saveButton).toBeEnabled();
    fireEvent.keyDown(surface, { key: "Escape", code: "Escape" });
    await waitFor(() =>
      expect(document.querySelector(".source-link-popover")).toBeNull(),
    );
  });

  it("edits a rich Markdown document and saves its formatting through the revision API", async () => {
    let savedText = "";
    const { fetchMock } = setup((request, init) => {
      const path = new URL(String(request), "http://localhost").pathname;
      if (path.endsWith("/versions/version-a/content"))
        return Promise.resolve(
          response({
            source_version_id: "version-a",
            representation: "original",
            media_type: "text/markdown",
            text: '# 中文\n\n原文 [链接](https://example.org "说明")\n\n| 名称 | 值 |\n| :--- | ---: |\n| 甲 | 2 |\n\n```ts\nconst x = 1\n```\n',
            text_basis: "exact",
          }),
        );
      if (path.endsWith("/versions") && init?.method === "POST") {
        savedText = JSON.parse(String(init.body)).text as string;
        return Promise.resolve(
          response(
            {
              source: {
                ...source,
                revision: 3,
                current_version_id: "version-b",
              },
              source_version: {
                source_version_id: "version-b",
                source_id: "source-a",
                version: 3,
                representation: "authored_text",
              },
              knowledge_release: {
                knowledge_release_id: "release-b",
                sequence: 2,
              },
            },
            201,
          ),
        );
      }
      return Promise.resolve(response({}));
    });
    const visual = await screen.findByRole("textbox", {
      name: "资料正文（排版编辑）",
    });
    expect(visual.querySelector("h1")).toHaveTextContent("中文");
    expect(visual.querySelector("a")).toHaveAttribute(
      "href",
      "https://example.org",
    );
    expect(visual.querySelector("a")).toHaveAttribute("title", "说明");
    expect(visual.querySelector("th")).toHaveStyle({ textAlign: "left" });
    expect(visual.querySelector("pre")).toHaveTextContent("const x = 1");
    await userEvent.click(screen.getByRole("button", { name: "插入表格" }));
    await waitFor(() =>
      expect(visual.querySelector("table")).toBeInTheDocument(),
    );
    await userEvent.click(screen.getByRole("button", { name: "保存为新版本" }));
    await waitFor(() => expect(savedText).toContain("|"));
    expect(savedText).toContain('[链接](https://example.org "说明")');
    expect(savedText).toMatch(/\|\s*:---+\s*\|\s*---+:\s*\|/);
    expect(savedText).toContain("```ts");
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "POST"),
    ).toHaveLength(1);
    expect(
      new Headers(
        fetchMock.mock.calls.find(([, init]) => init?.method === "POST")?.[1]
          ?.headers,
      ).get("If-Match"),
    ).toBe("2");
  });

  it("keeps unsupported clipboard markup out of the visual serializer", async () => {
    const markdown = "# 原文\n\n中文";
    const { fetchMock } = setup((request) =>
      Promise.resolve(
        new URL(String(request), "http://localhost").pathname.endsWith(
          "/versions/version-a/content",
        )
          ? response({
              source_version_id: "version-a",
              representation: "original",
              media_type: "text/markdown",
              text: markdown,
              text_basis: "exact",
            })
          : response({}),
      ),
    );
    const visual = await screen.findByRole("textbox", {
      name: "资料正文（排版编辑）",
    });
    fireEvent.paste(visual, {
      clipboardData: {
        getData: () => "<article>不可丢失</article>",
      },
    });
    expect(await screen.findByText(/剪贴板内容未插入/)).toBeInTheDocument();
    const sourceField = screen.getByRole<HTMLTextAreaElement>("textbox", {
      name: "资料正文（原始文本）",
    });
    expect(sourceField.value).toBe(markdown);
    fireEvent.change(sourceField, {
      target: { value: `${markdown}\n<article>不可丢失</article>` },
    });
    expect(sourceField.value).toContain("<article>不可丢失</article>");
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "POST"),
    ).toHaveLength(0);
  });

  it("sends original UTF-8 content and CRLF line endings from a plain-text edit", async () => {
    let written = "";
    setup((request, init) => {
      const path = new URL(String(request), "http://localhost").pathname;
      if (path.endsWith("/versions/version-a/content"))
        return Promise.resolve(
          response({
            source_version_id: "version-a",
            representation: "original",
            media_type: "text/plain",
            text: "甲\r\n乙🦊\r\n",
            text_basis: "exact",
          }),
        );
      if (path.endsWith("/versions") && init?.method === "POST") {
        written = JSON.parse(String(init.body)).text as string;
        return Promise.resolve(
          response(
            {
              source: {
                ...source,
                revision: 3,
                current_version_id: "version-b",
              },
              source_version: {
                source_version_id: "version-b",
                source_id: "source-a",
                version: 3,
                representation: "authored_text",
              },
              knowledge_release: {
                knowledge_release_id: "release-b",
                sequence: 2,
              },
            },
            201,
          ),
        );
      }
      return Promise.resolve(response({}));
    });
    const editor = await screen.findByRole<HTMLTextAreaElement>("textbox", {
      name: "资料正文（原始文本）",
    });
    expect(editor.value).toBe("甲\n乙🦊\n");
    expect(screen.getByRole("button", { name: "保存为新版本" })).toBeDisabled();
    fireEvent.change(editor, { target: { value: "甲\n乙🦊\n丙\n" } });
    await userEvent.click(screen.getByRole("button", { name: "保存为新版本" }));
    await waitFor(() => expect(written).toBe("甲\r\n乙🦊\r\n丙\r\n"));
  });

  it("keeps the draft and repeats the identical request key after a network error", async () => {
    let saves = 0;
    const { fetchMock } = setup((request, init) => {
      const path = new URL(String(request), "http://localhost").pathname;
      if (path.endsWith("/versions/version-a/content"))
        return Promise.resolve(
          response({
            source_version_id: "version-a",
            representation: "original",
            media_type: "text/markdown",
            text: "# 原文\n\n<b>保留原始 HTML</b>\n",
            text_basis: "extracted",
          }),
        );
      if (path.endsWith("/versions") && init?.method === "POST") {
        saves += 1;
        if (saves === 1)
          return Promise.resolve(
            response({ code: "temporary_error", message: "稍后重试" }, 503),
          );
        return Promise.resolve(
          response(
            {
              source: {
                ...source,
                revision: 3,
                current_version_id: "version-b",
              },
              source_version: {
                source_version_id: "version-b",
                source_id: "source-a",
                version: 3,
                representation: "authored_text",
              },
              knowledge_release: {
                knowledge_release_id: "release-b",
                sequence: 2,
              },
            },
            201,
          ),
        );
      }
      return Promise.resolve(response({}));
    });
    const user = userEvent.setup();
    const editor = await screen.findByRole<HTMLTextAreaElement>("textbox", {
      name: "资料正文（原始文本）",
    });
    expect(editor.value).toContain("<b>保留原始 HTML</b>");
    expect(screen.getByText(/暂不支持可视编辑/)).toBeInTheDocument();
    fireEvent.change(editor, { target: { value: "# 原文\n\n<b>添注</b>\n" } });
    const save = screen.getByRole("button", { name: "保存为新版本" });
    await waitFor(() => expect(save).toBeEnabled());
    await user.click(save);
    expect(await screen.findByText(/保存失败：稍后重试/)).toBeInTheDocument();
    expect(editor.value).toContain("<b>添注</b>");
    await user.click(save);
    await waitFor(() => expect(saves).toBe(2));
    const requests = fetchMock.mock.calls.filter(
      ([request, init]) =>
        new URL(String(request), "http://localhost").pathname.endsWith(
          "/versions",
        ) && init?.method === "POST",
    );
    expect(requests).toHaveLength(2);
    expect(requests[0][1]?.body).toBe(requests[1][1]?.body);
    expect(JSON.parse(String(requests[0][1]?.body)).text).toContain(
      "<b>添注</b>",
    );
    expect(new Headers(requests[0][1]?.headers).get("Idempotency-Key")).toBe(
      new Headers(requests[1][1]?.headers).get("Idempotency-Key"),
    );
    expect(new Headers(requests[0][1]?.headers).get("If-Match")).toBe("2");
  });

  it("preserves a conflict draft without silently overwriting the newer version", async () => {
    const { fetchMock } = setup((request, init) => {
      const path = new URL(String(request), "http://localhost").pathname;
      if (path.endsWith("/versions/version-a/content"))
        return Promise.resolve(
          response({
            source_version_id: "version-a",
            representation: "original",
            media_type: "text/plain",
            text: "初稿",
            text_basis: "exact",
          }),
        );
      if (path.endsWith("/versions") && init?.method === "POST")
        return Promise.resolve(
          response(
            {
              code: "source_revision_conflict",
              message: "来源已更新",
            },
            409,
          ),
        );
      return Promise.resolve(response({}));
    });
    const editor = await screen.findByRole("textbox", {
      name: "资料正文（原始文本）",
    });
    fireEvent.change(editor, { target: { value: "初稿补充" } });
    await userEvent.click(screen.getByRole("button", { name: "保存为新版本" }));
    expect(
      await screen.findByText(/来源已更新，草稿已保留/),
    ).toBeInTheDocument();
    expect((editor as HTMLTextAreaElement).value).toBe("初稿补充");
    expect(screen.getByRole("button", { name: "保存为新版本" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "查看最新版本" })).toBeEnabled();
    expect(
      screen.queryByRole("button", { name: "以最新版本为基线修订" }),
    ).not.toBeInTheDocument();
    expect(
      fetchMock.mock.calls.filter(
        ([request, init]) =>
          new URL(String(request), "http://localhost").pathname.endsWith(
            "/versions",
          ) && init?.method === "POST",
      ),
    ).toHaveLength(1);
  });

  it("leaves historical versions read-only", async () => {
    const fetchMock = vi.fn((request: RequestInfo | URL) =>
      String(request).includes("/auth/session")
        ? Promise.resolve(response(session))
        : Promise.resolve(
            response({
              source_version_id: "version-old",
              representation: "original",
              media_type: "text/plain",
              text: "旧内容",
              text_basis: "exact",
            }),
          ),
    );
    vi.stubGlobal("fetch", fetchMock);
    render(
      <FluentProvider theme={webLightTheme}>
        <QueryClientProvider client={new QueryClient()}>
          <AuthProvider>
            <SourceTextRevisionPanel
              tenantId="tenant-a"
              projectId="project-a"
              source={source}
              selectedVersionId="version-old"
              canEdit
              onViewLatest={() => undefined}
            />
          </AuthProvider>
        </QueryClientProvider>
      </FluentProvider>,
    );
    expect(await screen.findByText("旧内容")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "保存为新版本" }),
    ).not.toBeInTheDocument();
  });
});
