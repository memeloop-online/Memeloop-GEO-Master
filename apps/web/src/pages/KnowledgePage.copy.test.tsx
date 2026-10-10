import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import type { KnowledgeCapabilities } from "../api/knowledge";
import i18n from "../i18n";
import { KnowledgeCapabilitiesNotice, KnowledgePage } from "./KnowledgePage";

const mockData = vi.hoisted(() => ({
  sourceItems: [] as Array<Record<string, unknown>>,
  capabilities: undefined as KnowledgeCapabilities | undefined,
}));

vi.mock("../api/knowledge", () => ({
  useKnowledgeCapabilitiesQuery: () => ({
    data: mockData.capabilities,
    isError: false,
  }),
  useSourcesQuery: () => ({
    data: { items: mockData.sourceItems },
    isPending: false,
    isError: false,
    isFetching: false,
    refetch: vi.fn(),
  }),
  useProductsQuery: () => ({
    data: { items: [] },
    isPending: false,
    isError: false,
    refetch: vi.fn(),
  }),
  useFactsQuery: () => ({
    data: { items: [] },
    isPending: false,
    isError: false,
    isFetching: false,
    refetch: vi.fn(),
  }),
  useKnowledgeReleaseQuery: () => ({ data: undefined }),
  useUploadFilesMutation: () => ({ isPending: false, mutateAsync: vi.fn() }),
  useImportKnowledgeMutation: () => ({
    isPending: false,
    mutateAsync: vi.fn(),
  }),
}));

const capabilities: KnowledgeCapabilities = {
  pdf_parser: { available: false },
  docx_parser: { available: false },
  xlsx_parser: { available: true },
  ocr: { available: false },
  vector: { available: true },
  llm: { available: false },
  url_fetch: { available: false },
};

function renderPage() {
  return render(
    <FluentProvider theme={webLightTheme}>
      <MemoryRouter initialEntries={["/app/tenant-a/project-a/knowledge"]}>
        <Routes>
          <Route
            path="/app/:tenantId/:projectId/knowledge"
            element={<KnowledgePage />}
          />
        </Routes>
      </MemoryRouter>
    </FluentProvider>,
  );
}

beforeEach(async () => {
  mockData.sourceItems = [];
  mockData.capabilities = undefined;
  await act(() => i18n.changeLanguage("zh-CN"));
});

afterEach(async () => {
  await act(() => i18n.changeLanguage("zh-CN"));
});

describe("knowledge page user copy", () => {
  it.each([
    [
      "zh-CN",
      "部分功能暂不可用",
      /PDF 解析.*DOCX 解析.*扫描件识别.*网页导入.*暂不可用/,
    ],
    [
      "en",
      "Some features are unavailable",
      /PDF parsing.*DOCX parsing.*Scanned document recognition.*Web page import.*unavailable/,
    ],
  ])(
    "names unavailable abilities without engineering narration in %s",
    async (language, title, text) => {
      await act(() => i18n.changeLanguage(language));
      render(<KnowledgeCapabilitiesNotice capabilities={capabilities} />);
      expect(screen.getByText(title)).toBeInTheDocument();
      expect(screen.getByText(text)).toBeInTheDocument();
      expect(
        screen.queryByText(/系统不会以空结果|adapter|work package/i),
      ).not.toBeInTheDocument();
    },
  );

  it.each([
    ["zh-CN", "部分完成", "解析失败", "PDF 解析不可用。"],
    [
      "en",
      "Partially complete",
      "Parsing failed",
      "PDF parsing is unavailable.",
    ],
  ])(
    "shows truthful parsing statuses and translated actions in %s",
    async (language, partial, failed, pdfHelp) => {
      await act(() => i18n.changeLanguage(language));
      mockData.capabilities = capabilities;
      mockData.sourceItems = [
        {
          source_id: "partial",
          kind: "file",
          name: "guide.pdf",
          purpose: "internal",
          state: "active",
          import_status: "partial",
        },
        {
          source_id: "failed",
          kind: "file",
          name: "notes.docx",
          purpose: "public",
          state: "active",
          import_status: "failed",
        },
      ];
      const user = userEvent.setup();
      renderPage();
      expect(screen.getByText(partial)).toBeInTheDocument();
      expect(screen.getByText(failed)).toBeInTheDocument();
      await user.click(
        screen.getByRole("button", {
          name: language === "en" ? "Import sources" : "导入资料",
        }),
      );
      expect(
        screen.getByText(new RegExp(pdfHelp.replace(".", "\\."))),
      ).toBeInTheDocument();
      expect(
        screen.queryByText(/P03|系统不会以空结果|当前适配器/),
      ).not.toBeInTheDocument();
    },
  );
});
