import type { SourceChunk, SourceLocator } from "../api/knowledge";

type OfficeChunk = SourceChunk & {
  locator: SourceLocator & { kind: "docx" | "xlsx" };
};

function officeChunk(chunk: SourceChunk): chunk is OfficeChunk {
  return chunk.locator?.kind === "docx" || chunk.locator?.kind === "xlsx";
}

/** A cell may span several bounded evidence chunks; concatenate only contiguous slices. */
function cellValue(chunks: OfficeChunk[]): string {
  return [...chunks]
    .sort(
      (a, b) =>
        (a.locator.start_char ?? 0) - (b.locator.start_char ?? 0) ||
        a.ordinal - b.ordinal,
    )
    .map((chunk) => chunk.text)
    .join("");
}

function docxTable(selected: OfficeChunk, chunks: OfficeChunk[]) {
  const matching = chunks.filter(
    (chunk) =>
      chunk.locator.kind === "docx" &&
      chunk.locator.table_index === selected.locator.table_index &&
      chunk.locator.body_element_index ===
        selected.locator.body_element_index &&
      chunk.locator.table_row != null &&
      chunk.locator.table_column != null,
  );
  const rows = new Map<number, Map<number, OfficeChunk[]>>();
  for (const chunk of matching) {
    const rowIndex = chunk.locator.table_row!;
    const columnIndex = chunk.locator.table_column!;
    const row = rows.get(rowIndex) ?? new Map<number, OfficeChunk[]>();
    row.set(columnIndex, [...(row.get(columnIndex) ?? []), chunk]);
    rows.set(rowIndex, row);
  }
  return (
    <div
      className="csv-evidence-scroll"
      role="region"
      tabIndex={0}
      aria-label="DOCX 表格，可横向滚动"
    >
      <table className="csv-evidence-table">
        <caption>
          DOCX 表格 {selected.locator.table_index! + 1} · 已提取的实际单元格
          （空值及失败单元不补造）
        </caption>
        <tbody>
          {[...rows]
            .sort(([a], [b]) => a - b)
            .map(([rowIndex, cells]) => (
              <tr key={rowIndex}>
                {[...cells]
                  .sort(([a], [b]) => a - b)
                  .map(([columnIndex, parts]) => {
                    const locator = parts[0].locator;
                    return (
                      <td
                        key={columnIndex}
                        rowSpan={locator.table_row_span ?? 1}
                        colSpan={locator.table_col_span ?? 1}
                        aria-label={`第 ${rowIndex + 1} 行第 ${columnIndex + 1} 列`}
                      >
                        {cellValue(parts) || "（空值）"}
                        {locator.table_merged && <small> 合并单元格</small>}
                      </td>
                    );
                  })}
              </tr>
            ))}
        </tbody>
      </table>
    </div>
  );
}

function spreadsheetRow(selected: OfficeChunk, chunks: OfficeChunk[]) {
  const coordinate = /^([A-Z]+)([1-9]\d*)$/.exec(selected.locator.range ?? "");
  if (!coordinate) return <p>单元格坐标不完整；请查看原始证据片段。</p>;
  const rowNumber = coordinate[2];
  const cells = new Map<string, OfficeChunk[]>();
  for (const chunk of chunks) {
    if (
      chunk.locator.kind !== "xlsx" ||
      chunk.locator.sheet !== selected.locator.sheet
    )
      continue;
    const match = /^([A-Z]+)([1-9]\d*)$/.exec(chunk.locator.range ?? "");
    if (!match || match[2] !== rowNumber) continue;
    cells.set(match[1], [...(cells.get(match[1]) ?? []), chunk]);
  }
  const columnNumber = (letters: string) =>
    [...letters].reduce(
      (number, letter) => number * 26 + letter.charCodeAt(0) - 64,
      0,
    );
  return (
    <div
      className="csv-evidence-scroll"
      role="region"
      tabIndex={0}
      aria-label="XLSX 工作表当前行，可横向滚动"
    >
      <table className="csv-evidence-table">
        <caption>
          工作表 {selected.locator.sheet} · 第 {rowNumber} 行的已提取单元格
          （仅显示当前行，未出现的格子不补造；公式不执行）
          {selected.locator.header_range &&
            ` · 实际表头范围 ${selected.locator.header_range}`}
        </caption>
        <tbody>
          <tr>
            {[...cells]
              .sort(([a], [b]) => columnNumber(a) - columnNumber(b))
              .map(([column, parts]) => {
                const locator = parts[0].locator;
                // A formula-only chunk carries formula source as searchable
                // evidence; it is not an evaluated or stored cell result.
                const noCachedFormula =
                  locator.cell_kind === "formula_cached" &&
                  locator.cached_value == null;
                const raw = noCachedFormula ? "" : cellValue(parts);
                return (
                  <td key={column}>
                    <div>
                      <b>
                        {column}
                        {rowNumber}
                      </b>
                    </div>
                    <div>
                      原值：
                      {noCachedFormula
                        ? "（无缓存原值）"
                        : raw === ""
                          ? "（空值）"
                          : raw}
                    </div>
                    {locator.display_value != null && (
                      <div>显示值：{locator.display_value}</div>
                    )}
                    {locator.merged_range && (
                      <div>实际合并范围：{locator.merged_range}</div>
                    )}
                    {locator.cell_kind === "formula_cached" && (
                      <>
                        <div>
                          公式原文（不执行）：{locator.formula ?? "未提供"}
                        </div>
                        <div>
                          缓存结果：
                          {locator.cached_value == null
                            ? "缺失（未重新计算）"
                            : `${locator.cached_value}${locator.cached_kind ? `（${locator.cached_kind}）` : ""}`}
                        </div>
                      </>
                    )}
                  </td>
                );
              })}
          </tr>
        </tbody>
      </table>
    </div>
  );
}

export function OfficeEvidenceTable({
  selected,
  chunks,
}: {
  selected: SourceChunk;
  chunks: SourceChunk[];
}) {
  if (!officeChunk(selected)) return null;
  const sameVersion = chunks.filter(
    (chunk): chunk is OfficeChunk =>
      chunk.source_version_id === selected.source_version_id &&
      officeChunk(chunk),
  );
  if (selected.locator.kind === "docx") {
    if (selected.locator.table_index == null) {
      return <p>段落按原始标题路径与段落序号定位；当前不是表格。</p>;
    }
    return docxTable(selected, sameVersion);
  }
  return spreadsheetRow(selected, sameVersion);
}
