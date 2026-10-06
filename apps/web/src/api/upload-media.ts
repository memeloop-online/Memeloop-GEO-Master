/** Declared type only: backend parsing still validates the uploaded bytes. */
export function uploadMediaType(file: Pick<File, "name" | "type">): string {
  const filename = file.name.toLowerCase();
  // Some browsers label CSV as a spreadsheet MIME. Do not send it to the
  // binary spreadsheet parser; extension selects the strict CSV parser.
  if (filename.endsWith(".csv")) return "text/csv";
  if (filename.endsWith(".docx"))
    return "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
  if (filename.endsWith(".xlsx"))
    return "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
  if (filename.endsWith(".pdf")) return "application/pdf";
  if (file.type) return file.type;
  if (filename.endsWith(".txt")) return "text/plain";
  if (filename.endsWith(".md") || filename.endsWith(".markdown"))
    return "text/markdown";
  return "application/octet-stream";
}
