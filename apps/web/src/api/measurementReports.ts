import { apiFetch } from "./client";
import type { ReportSupplementaryMeasurement } from "./reports";
import type { SerpObservation, SerpProtocol, SerpTarget } from "./serp";

export interface MeasurementPeriodSearchSample {
  cohort: {
    measurement_id: string;
    source_key: string;
    query: string;
    protocol: SerpProtocol;
    target: SerpTarget | null;
    target_rule_version: string;
    question_binding: ReportSupplementaryMeasurement["question_binding"];
    scheduled_at: string;
    created_at: string;
    stored_at: string;
  };
  evidence: {
    observation: SerpObservation;
    raw_stored_at: string;
    observation_stored_at: string;
    evidence_time: string;
    evidence_time_basis: "provider_observed_at" | "received_at";
    target_match:
      | { status: "not_requested" | "undetermined" }
      | { status: "hit"; organic_ranks: number[] }
      | { status: "not_found_within_depth"; covered_depth: number };
  } | null;
}

export interface MeasurementPeriodSearchSection {
  schema_version: string;
  coverage: { planned: number; counts: Record<string, number> };
  samples: MeasurementPeriodSearchSample[];
}

export interface MeasurementPeriodSample {
  plan_id: string;
  target_id: string;
  attempt_id?: string | null;
  comparison_key: string;
  question_binding?: ReportSupplementaryMeasurement["question_binding"];
  scheduled_at: string;
  original_status: string;
  observed_live: boolean;
  observation: ReportSupplementaryMeasurement["observation"] | null;
}

export interface MeasurementPeriodProjection {
  project_id: string;
  report_window_start_at: string;
  report_window_end_at: string;
  report_timezone: string;
  evidence_as_of: string;
  generated_at: string;
  input_hash: string;
  coverage: {
    planned: number;
    counts: Record<string, number>;
    grounded_saved_analysis: number;
    observed_live: number;
  };
  samples: MeasurementPeriodSample[];
  search?: MeasurementPeriodSearchSection | null;
}

export interface MeasurementPeriodReport extends MeasurementPeriodProjection {
  kind: "measurement_period";
  report_id: string;
  revision: number;
  correction_of: string | null;
}
export interface MeasurementPeriodPreview extends MeasurementPeriodProjection {
  kind: "measurement_period_preview";
}

export function getMeasurementReportPreview(
  tenantId: string,
  projectId: string,
) {
  return apiFetch<MeasurementPeriodPreview>(
    `/projects/${encodeURIComponent(projectId)}/measurement-report-preview`,
    { tenantId, projectId },
  );
}
export function listMeasurementReports(tenantId: string, projectId: string) {
  return apiFetch<{ items: MeasurementPeriodReport[] }>(
    `/projects/${encodeURIComponent(projectId)}/measurement-reports`,
    { tenantId, projectId },
  );
}
export function getMeasurementReport(
  tenantId: string,
  projectId: string,
  reportId: string,
) {
  return apiFetch<MeasurementPeriodReport>(
    `/measurement-reports/${encodeURIComponent(reportId)}`,
    { tenantId, projectId },
  );
}
export function saveMeasurementReport(
  tenantId: string,
  projectId: string,
  preview: MeasurementPeriodProjection,
) {
  return apiFetch<MeasurementPeriodReport>(
    `/projects/${encodeURIComponent(projectId)}/measurement-reports`,
    {
      tenantId,
      projectId,
      method: "POST",
      body: {
        start_at: preview.report_window_start_at,
        end_at: preview.report_window_end_at,
        report_timezone: preview.report_timezone,
      },
    },
  );
}
