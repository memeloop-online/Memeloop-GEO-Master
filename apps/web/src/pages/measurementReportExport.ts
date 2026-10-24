import type {
  MeasurementPeriodPreview,
  MeasurementPeriodReport,
} from "../api/measurementReports";
import type { MeasurementTarget } from "../api/projects";

export type MeasurementReportExportInput = {
  report: MeasurementPeriodReport | MeasurementPeriodPreview;
  view: { question: string | null; target: MeasurementTarget | null };
  locale: "en" | "zh-CN";
};

/** Capture evidence, viewing preferences and language together before downloading. */
export function captureMeasurementReportExport(
  input: MeasurementReportExportInput,
): MeasurementReportExportInput {
  return structuredClone(input);
}
