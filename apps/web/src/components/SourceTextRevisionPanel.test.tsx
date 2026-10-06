import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { AuthProvider } from "../auth/AuthProvider";
import { setCsrfToken, setUnauthorizedHandler } from "../api/client";
import type { SourceSummary } from "../api/knowledge";
import {
  editorContentToText,
  SourceTextRevisionPanel,
  textToEditorContent,
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

afterEach(() => {
  setCsrfToken(undefined);
  setUnauthorizedHandler(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("knowledge source text revision", () => {
  it("preserves Markdown syntax, Chinese text and trailing blank lines as literal text", () => {
    const markdown = "# 标题\n\n- 第一项  \n`literal` **粗体**\n";
    expect(editorContentToText(textToEditorContent(markdown))).toBe(markdown);
    expect(() =>
      editorContentToText({
        type: "doc",
        content: [
          {
            type: "paragraph",
            content: [
              { type: "text", text: "bold", marks: [{ type: "bold" }] },
            ],
          },
        ],
      }),
    ).toThrow(/无法无损保存/);
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
            text: "# 原文",
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
    const editor = await screen.findByRole("textbox", {
      name: "资料正文（原始文本）",
    });
    fireEvent.paste(editor, {
      clipboardData: { getData: () => "<b>添注</b>" },
    });
    const save = screen.getByRole("button", { name: "保存为新版本" });
    await waitFor(() => expect(save).toBeEnabled());
    await user.click(save);
    expect(await screen.findByText(/保存失败：稍后重试/)).toBeInTheDocument();
    expect(editor).toHaveTextContent("<b>添注</b>");
    expect(editor.querySelector("b")).toBeNull();
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
    fireEvent.paste(editor, { clipboardData: { getData: () => "补充" } });
    await userEvent.click(screen.getByRole("button", { name: "保存为新版本" }));
    expect(
      await screen.findByText(/来源已更新，草稿已保留/),
    ).toBeInTheDocument();
    expect(editor).toHaveTextContent("补充");
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
