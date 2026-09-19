import type { SourceLocator } from "../api/knowledge";

export function formatLocator(locator: SourceLocator | null | undefined) {
  if (!locator) return "未提供精确定位";
  switch (locator.kind) {
    case "pdf":
      return [
        locator.page ? `PDF 第 ${locator.page} 页` : "PDF",
        locator.bbox ? `高亮区域 ${locator.bbox.join(", ")}` : null,
        locator.ocr ? "OCR" : null,
      ]
        .filter(Boolean)
        .join(" · ");
    case "docx":
      return [
        "DOCX",
        locator.heading_path?.join(" › "),
        locator.paragraph_index !== undefined &&
        locator.paragraph_index !== null
          ? `段落 ${locator.paragraph_index + 1}`
          : null,
      ]
        .filter(Boolean)
        .join(" · ");
    case "xlsx":
      return [
        "XLSX",
        locator.sheet ? `工作表 ${locator.sheet}` : null,
        locator.range ? `范围 ${locator.range}` : null,
        locator.header_range ? `表头 ${locator.header_range}` : null,
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
        locator.range ? `范围 ${locator.range}` : null,
        locator.header_range ? `表头 ${locator.header_range}` : null,
        locator.start_row !== undefined && locator.start_row !== null
          ? `第 ${locator.start_row}–${locator.end_row ?? locator.start_row} 行`
          : null,
        locator.start_column !== undefined && locator.start_column !== null
          ? `第 ${locator.start_column}–${locator.end_column ?? locator.start_column} 列`
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
