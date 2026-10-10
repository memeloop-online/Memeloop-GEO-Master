import { reportAnalysisText } from "../i18n/reportAnalysis";

/** Display labels only; the untouched comparison key remains the grouping identity. */
export function reportMeasurementTitle(comparisonKey: string): string {
  const fields = comparisonKey.split("|");
  if (
    ![8, 10].includes(fields.length) ||
    fields.some((field) => !field.trim())
  ) {
    return reportAnalysisText("measurementRecordTitle");
  }
  return reportAnalysisText("measurementTitle", {
    provider: fields[0] === "kimi" ? "Kimi" : fields[0],
    model: fields[1],
  });
}
