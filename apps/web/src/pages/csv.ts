export type CsvCell = string | number | null;

function safeCell(value: CsvCell): string {
  const text = value === null ? "" : String(value);
  // Quoting CSV is not enough: spreadsheets evaluate formula-looking strings.
  const literal =
    /^[\s\u0000-\u001f]*[=+\-@]/u.test(text) || /^[\t\r\n]/u.test(text)
      ? `'${text}`
      : text;
  return `"${literal.replaceAll('"', '""')}"`;
}

/** Shared spreadsheet-safe CSV wire format. */
export function serializeCsv(rows: readonly (readonly CsvCell[])[]): string {
  return (
    "\uFEFF" +
    rows.map((row) => row.map(safeCell).join(",")).join("\r\n") +
    "\r\n"
  );
}
