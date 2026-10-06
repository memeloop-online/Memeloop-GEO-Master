import { useQuery } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor, type QueryScope } from "../auth/types";
import { getCurrentCycle } from "./channelJobs";
import { apiFetch } from "./client";

export interface ReportCoverage {
  availability: "available" | "unavailable" | "unsealed";
  expected_count: number | null;
  observed_count: number;
  counts: Record<string, number>;
  reason: string | null;
}

export interface ReportManifestRef {
  kind: "document" | "distribution" | "measurement";
  manifest_id: string;
  revision: number;
  sealed: boolean;
  expected_count: number | null;
}

export interface ReportEvidenceReference {
  evidence_id: string;
  kind: string;
  resource_id: string;
  resource_version: string | null;
  occurred_at: string | null;
  received_at: string | null;
  summary: string;
}

export interface ReportFinding {
  finding_id: string;
  kind: string;
  summary: string;
  evidence_ids: string[];
  insufficient_reason: string | null;
}

export interface PublicationGroup {
  platform_id: string;
  coverage: ReportCoverage;
}

export interface MeasurementGroup {
  /** Opaque frozen comparison key; never combine unrelated groups. */
  comparison_key: string;
  coverage: ReportCoverage;
}

export interface ReportSnapshot {
  report_id: string;
  project_id: string;
  cycle_id: string;
  revision: number;
  correction_of: string | null;
  report_window_start_at: string;
  report_window_end_at: string;
  report_timezone: string;
  cutoff_at: string;
  generated_at: string;
  reducer_version: string;
  input_hash: string;
  evidence_as_of: string;
  status: "complete" | "partial";
  input_manifest_versions: ReportManifestRef[];
  documents: ReportCoverage;
  publications: ReportCoverage;
  publication_groups: PublicationGroup[];
  measurements: ReportCoverage;
  measurement_groups: MeasurementGroup[];
  findings: ReportFinding[];
  evidence: ReportEvidenceReference[];
}

export type ReportProjection = Omit<
  ReportSnapshot,
  "report_id" | "revision" | "correction_of"
>;

export type ReportPreview = ReportProjection & { kind: "preview" };

export interface ReportList {
  items: ReportSnapshot[];
}

export const currentReportCycleQueryKey = (scope: QueryScope) =>
  [
    "reports",
    "current-cycle",
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? "",
  ] as const;

export const reportPreviewQueryKey = (scope: QueryScope, cycleId: string) =>
  [
    "reports",
    "preview",
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? "",
    cycleId,
  ] as const;

export const reportsQueryKey = (scope: QueryScope) =>
  [
    "reports",
    "list",
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? "",
  ] as const;

export const reportDetailQueryKey = (scope: QueryScope, reportId: string) =>
  [
    "reports",
    "detail",
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? "",
    reportId,
  ] as const;

export function listReports(tenantId: string, projectId: string) {
  return apiFetch<ReportList>(
    `/projects/${encodeURIComponent(projectId)}/reports`,
    { tenantId, projectId },
  );
}

export function getReport(
  tenantId: string,
  projectId: string,
  reportId: string,
) {
  return apiFetch<ReportSnapshot>(`/reports/${encodeURIComponent(reportId)}`, {
    tenantId,
    projectId,
  });
}

export function getReportEvidence(
  tenantId: string,
  projectId: string,
  reportId: string,
) {
  return apiFetch<{ items: ReportEvidenceReference[] }>(
    `/reports/${encodeURIComponent(reportId)}/evidence`,
    { tenantId, projectId },
  );
}

export function getReportPreview(
  tenantId: string,
  projectId: string,
  cycleId: string,
) {
  return apiFetch<ReportPreview>(
    `/cycles/${encodeURIComponent(cycleId)}/report-preview`,
    { tenantId, projectId },
  );
}

export function useCurrentReportCycleQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? currentReportCycleQueryKey(scope)
      : ["reports", "current-cycle", "anonymous", tenantId, projectId],
    queryFn: () => getCurrentCycle(tenantId!, projectId!),
    enabled: Boolean(scope),
  });
}

export function useReportPreviewQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  cycleId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? reportPreviewQueryKey(scope, cycleId ?? "")
      : ["reports", "preview", "anonymous", tenantId, projectId, cycleId],
    queryFn: () => getReportPreview(tenantId!, projectId!, cycleId!),
    enabled: Boolean(scope && cycleId),
  });
}

export function useReportsQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? reportsQueryKey(scope)
      : ["reports", "list", "anonymous", tenantId, projectId],
    queryFn: () => listReports(tenantId!, projectId!),
    enabled: Boolean(scope),
  });
}

export function useReportQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  reportId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? reportDetailQueryKey(scope, reportId ?? "")
      : ["reports", "detail", "anonymous", tenantId, projectId, reportId],
    queryFn: () => getReport(tenantId!, projectId!, reportId!),
    enabled: Boolean(scope && reportId),
  });
}

export function useReportEvidenceQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  reportId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? [...reportDetailQueryKey(scope, reportId ?? ""), "evidence"]
      : [
          "reports",
          "detail",
          "anonymous",
          tenantId,
          projectId,
          reportId,
          "evidence",
        ],
    queryFn: () => getReportEvidence(tenantId!, projectId!, reportId!),
    enabled: Boolean(scope && reportId),
  });
}
