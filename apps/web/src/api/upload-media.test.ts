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
  });
});
