import { describe, expect, it } from "vitest";
import { uploadMediaType } from "./upload-media";
import { formatLocator } from "../components/KnowledgeLocator";

describe("CSV upload and evidence coordinates", () => {
  it.each(["", "application/vnd.ms-excel", "text/plain", "text/csv"])(
    "uses strict CSV parsing for a CSV filename with browser type %s",
    (type) =>
      expect(uploadMediaType({ name: "Prices.CSV", type })).toBe("text/csv"),
  );
  it("does not label binary spreadsheets as CSV", () => {
    expect(
      uploadMediaType({
        name: "prices.xlsx",
        type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
      }),
    ).toBe("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
    expect(uploadMediaType({ name: "guide.md", type: "" })).toBe(
      "text/markdown",
    );
  });
  it.each([
    [
      ".DOCX",
      "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ],
    [
      ".XLSX",
      "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ],
    [".PDF", "application/pdf"],
  ])(
    "recognizes %s even when browser MIME is absent or incorrect",
    (extension, mediaType) => {
      expect(uploadMediaType({ name: `guide${extension}`, type: "" })).toBe(
        mediaType,
      );
      expect(
        uploadMediaType({
          name: `guide${extension}`,
          type: "application/octet-stream",
        }),
      ).toBe(mediaType);
    },
  );
  it("distinguishes logical CSV records from physical lines", () => {
    expect(
      formatLocator({
        kind: "csv",
        start_row: 2,
        end_row: 2,
        start_column: 1,
        end_column: 3,
        header_row: 1,
      }),
    ).toBe("CSV · 表头记录 1 · 第 2–2 条逻辑记录 · 第 1–3 列");
    expect(formatLocator({ kind: "csv", start_row: 2, start_column: 1 })).toBe(
      "CSV · 第 2–2 条逻辑记录 · 第 1–1 列",
    );
    expect(
      formatLocator({
        kind: "csv",
        header_row: 1,
        start_row: 3,
        end_row: 3,
        start_column: 2,
        end_column: 2,
        start_char: 80,
        end_char: 240,
      }),
    ).toBe(
      "CSV · 表头记录 1 · 第 3–3 条逻辑记录 · 第 2–2 列 · 单元格字符 80–240（从 0 开始，不含结束位置）",
    );
  });
  it("shows Office coordinates and merge/header metadata only when present", () => {
    expect(
      formatLocator({
        kind: "docx",
        heading_path: ["Section"],
        paragraph_index: 3,
        table_index: 0,
        table_row: 2,
        table_column: 1,
        table_row_span: 2,
        table_col_span: 3,
        table_merged: true,
      }),
    ).toBe(
      "DOCX · Section · 表格 1 · 第 3 行 · 第 2 列 · 跨 2 行 · 跨 3 列 · 合并单元格",
    );
    expect(formatLocator({ kind: "xlsx", sheet: "Sheet1", range: "B12" })).toBe(
      "XLSX · 工作表 Sheet1 · 单元格 B12",
    );
    expect(
      formatLocator({
        kind: "xlsx",
        sheet: "Sheet1",
        range: "B12",
        header_range: "A1:D1",
        merged_range: "B12:C12",
      }),
    ).toBe("XLSX · 工作表 Sheet1 · 单元格 B12 · 表头 A1:D1 · 合并范围 B12:C12");
  });
});
