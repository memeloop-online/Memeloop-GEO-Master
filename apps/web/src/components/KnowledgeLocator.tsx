import type { SourceLocator } from "../api/knowledge";

export function formatLocator(locator: SourceLocator | null | undefined) {
  if (!locator) return "未提供精确定位";
  switch (locator.kind) {
    case "pdf":
      return [
        locator.page ? `PDF 第 ${locator.page} 页` : "PDF",
        locator.bbox ? `原文区域坐标 ${locator.bbox.join(", ")}` : null,
        locator.ocr ? "OCR" : null,
      ]
        .filter(Boolean)
        .join(" · ");
    case "docx":
      return [
        "DOCX",
        locator.heading_path?.length ? locator.heading_path.join(" › ") : null,
        locator.table_index != null
          ? `表格 ${locator.table_index + 1}${locator.table_row != null ? ` · 第 ${locator.table_row + 1} 行` : ""}${locator.table_column != null ? ` · 第 ${locator.table_column + 1} 列` : ""}`
          : locator.paragraph_index !== undefined &&
              locator.paragraph_index !== null
            ? `段落 ${locator.paragraph_index + 1}`
            : null,
        locator.table_row_span != null && locator.table_row_span > 1
          ? `跨 ${locator.table_row_span} 行`
          : null,
        locator.table_col_span != null && locator.table_col_span > 1
          ? `跨 ${locator.table_col_span} 列`
          : null,
        locator.table_merged ? "合并单元格" : null,
        locator.start_char != null && locator.end_char != null
          ? `字符 ${locator.start_char}–${locator.end_char}（从 0 开始，不含结束位置）`
          : null,
      ]
        .filter(Boolean)
        .join(" · ");
    case "xlsx":
      return [
        "XLSX",
        locator.sheet ? `工作表 ${locator.sheet}` : null,
        locator.range ? `单元格 ${locator.range}` : null,
        locator.header_range ? `表头 ${locator.header_range}` : null,
        locator.merged_range ? `合并范围 ${locator.merged_range}` : null,
        locator.start_char != null && locator.end_char != null
          ? `字符 ${locator.start_char}–${locator.end_char}（从 0 开始，不含结束位置）`
          : null,
      ]
        .filter(Boolean)
        .join(" · ");
    case "web":
      return [
        "网页快照",
        locator.original_url,
        locator.selector ? `选择器 ${locator.selector}` : null,
      ]
        .filter(Boolean)
        .join(" · ");
    case "text": {
      const lineStart = locator.start_line ?? locator.line_start;
      const lineEnd = locator.end_line ?? locator.line_end ?? lineStart;
      const charStart = locator.start_char ?? locator.char_start;
      const charEnd = locator.end_char ?? locator.char_end ?? charStart;
      return [
        "文本",
        lineStart !== undefined && lineStart !== null
          ? `第 ${lineStart}–${lineEnd} 行`
          : null,
        charStart !== undefined && charStart !== null
          ? `字符 ${charStart}–${charEnd}`
          : null,
      ]
        .filter(Boolean)
        .join(" · ");
    }
    case "csv":
      return [
        "CSV",
        locator.header_row != null ? `表头记录 ${locator.header_row}` : null,
        locator.range ? `范围 ${locator.range}` : null,
        locator.header_range ? `表头 ${locator.header_range}` : null,
        locator.start_row !== undefined && locator.start_row !== null
          ? `第 ${locator.start_row}–${locator.end_row ?? locator.start_row} 条逻辑记录`
          : null,
        locator.start_column !== undefined && locator.start_column !== null
          ? `第 ${locator.start_column}–${locator.end_column ?? locator.start_column} 列`
          : null,
        locator.start_char != null && locator.end_char != null
          ? `单元格字符 ${locator.start_char}–${locator.end_char}（从 0 开始，不含结束位置）`
          : null,
      ]
        .filter(Boolean)
        .join(" · ");
    case "manual":
      return "手工资料的不可变文字版本";
    default:
      return "未提供精确定位";
  }
}

export function KnowledgeLocator({
  locator,
}: {
  locator: SourceLocator | null | undefined;
}) {
  return <span className="knowledge-locator">{formatLocator(locator)}</span>;
}
