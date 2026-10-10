/** Render exact string cells, never spreadsheet formulae or inferred numbers. */
export function CsvEvidenceTable({ text }: { text: string }) {
  let headers: string[];
  let values: string[];
  try {
    const record: unknown = JSON.parse(text);
    if (
      !record ||
      typeof record !== "object" ||
      !("headers" in record) ||
      !("values" in record) ||
      !Array.isArray(record.headers) ||
      !Array.isArray(record.values) ||
      record.headers.length === 0 ||
      record.headers.length !== record.values.length ||
      !record.headers.every(
        (cell): cell is string => typeof cell === "string",
      ) ||
      !record.values.every((cell): cell is string => typeof cell === "string")
    ) {
      return <p>表格片段格式不完整，请查看左侧原始片段。</p>;
    }
    headers = record.headers;
    values = record.values;
  } catch {
    return <p>表格片段格式不完整，请查看左侧原始片段。</p>;
  }
  return (
    <div
      className="csv-evidence-scroll"
      tabIndex={0}
      role="region"
      aria-label="CSV 原值表格，可横向滚动"
    >
      <table className="csv-evidence-table">
        <caption>当前记录原值（空表头按列位置标识，公式不执行）</caption>
        <thead>
          <tr>
            {headers.map((header, index) => (
              <th key={index} scope="col">
                <small>第 {index + 1} 列</small>
                <span>{header === "" ? "（空表头）" : header}</span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          <tr>
            {values.map((value, index) => (
              <td key={index}>
                {value === "" ? (
                  <span aria-label="空单元格">（空值）</span>
                ) : (
                  value
                )}
              </td>
            ))}
          </tr>
        </tbody>
      </table>
    </div>
  );
}
