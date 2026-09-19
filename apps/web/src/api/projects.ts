import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor, type QueryScope } from "../auth/types";
import { ApiError, apiFetch, type ApiRequestOptions } from "./client";

export type ProjectStatus = "draft" | "active" | "paused" | "archived";
export type ResourceMode = "own" | "platform" | "mixed";
export type SourceKind = "url" | "text" | "object" | "knowledge_collection";
export type SourceVisibility = "public" | "internal";

export interface InitialSource {
  kind: SourceKind;
  value: string;
  visibility: SourceVisibility;
  version_ref?: string | null;
  content_hash?: string | null;
}

export interface ReportSchedule {
  report_weekday: string;
  report_local_time: string;
  cutoff_weekday: string;
  cutoff_local_time: string;
  period_policy: "previous_calendar_week";
}

export interface DocumentScope {
  all_active_products: boolean;
  excluded_product_ids: string[];
  markets: string[];
  languages: string[];
  content_types: string[];
  question_clusters: Array<{
    key: string;
    state: "pending_resolution" | "resolved";
  }>;
}

export interface DistributionScope {
  mode: "all_eligible" | "explicit";
  included_platform_ids: string[];
  excluded_platform_ids: string[];
  resource_pool_ids: string[];
  replication_policy: "one_account_per_platform";
}

export interface ProjectSettings {
  brand_name: string;
  product_name?: string | null;
  market: string;
  language: string;
  target_audience?: string | null;
  objective?: string | null;
  competitors: string[];
  initial_sources: InitialSource[];
  resource_mode: ResourceMode;
  budget_currency: string;
  monthly_budget_minor: number;
  monitoring_reserve_percent: number;
  report_timezone: string;
  report_schedule: ReportSchedule;
  document_scope: DocumentScope;
  distribution_scope: DistributionScope;
}

export interface Project {
  id: string;
  slug: string;
  display_name: string;
  status: ProjectStatus;
  revision: number;
  settings: ProjectSettings;
  current_config_revision_id?: string | null;
  current_cycle_id?: string | null;
  start_operation_id?: string | null;
  created_at: string;
  updated_at: string;
}

export interface ProjectListResponse {
  items: Project[];
  next_cursor: string | null;
}

export interface CreateProjectInput {
  slug?: string;
  display_name: string;
  settings: ProjectSettings;
}

export interface CountEstimate {
  state: "unknown" | "estimated" | "frozen";
  value: number | null;
  min: number | null;
  max: number | null;
  basis_refs: string[];
  reason: string | null;
}

export interface MoneyEstimate {
  state: "unknown" | "estimated" | "frozen";
  value_minor: number | null;
  min_minor: number | null;
  max_minor: number | null;
  basis_refs: string[];
  reason: string | null;
}

export interface ProjectEstimate {
  settings_hash: string;
  estimator_version: string;
  pricing_snapshot_id: string | null;
  capability_snapshot_id: string | null;
  coverage: {
    documents: CountEstimate;
    document_platform_targets: CountEstimate;
    measurement_samples: CountEstimate;
  };
  costs: {
    phase_one_documents: MoneyEstimate;
    phase_two_distribution: MoneyEstimate;
    measurement: MoneyEstimate;
    total: MoneyEstimate;
  };
  budget: {
    monthly_limit_minor: number;
    measurement_reserve_minor: number;
    currency: string;
  };
  blockers: Array<{
    code: string;
    scope: string;
    reason: string;
  }>;
  assumptions: string[];
}

export interface ProjectManifestAcceptance {
  manifest_id: string;
  revision: number;
  state: string;
  sealed: boolean;
  expected_count: number | null;
}

/** A durable server-side acceptance, retrievable after navigation or refresh. */
export interface ProjectStartAcceptance {
  operation_id: string;
  cycle_id: string;
  config_revision_id: string;
  document_manifest: ProjectManifestAcceptance;
  distribution_manifest: ProjectManifestAcceptance;
  status: "accepted";
  operation_url: string;
}

export interface StartProjectInput {
  expected_revision: number;
}

export interface UpdateProjectInput {
  revision: number;
  display_name?: string;
  settings?: Partial<ProjectSettings>;
}

export interface ProjectOverview {
  project: Project;
  cycle: {
    status: "not_started" | "running" | "paused";
    awaiting_knowledge: boolean;
  };
  knowledge: {
    source_count: number;
    fact_count: number;
    status: "empty" | "importing" | "ready";
  };
  benchmark: {
    question_count: number;
    planned_samples: number;
    effective_samples: number | null;
    status: "not_started" | "running" | "ready";
  };
  content: {
    published_count: number;
    verified_count: number;
    blocked_count: number;
  };
  cost: {
    currency: string;
    reserved_minor: number;
    settled_minor: number;
  };
  next_action: {
    code: string;
    label: string;
    href: string;
  } | null;
  updated_at: string;
}

function scopeKey(scope: QueryScope) {
  return [
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? null,
  ] as const;
}

export const projectQueryKeys = {
  all: ["projects"] as const,
  list: (scope: QueryScope) =>
    [...projectQueryKeys.all, "list", ...scopeKey(scope)] as const,
  detail: (scope: QueryScope) =>
    [...projectQueryKeys.all, "detail", ...scopeKey(scope)] as const,
  overview: (scope: QueryScope) =>
    [...projectQueryKeys.all, "overview", ...scopeKey(scope)] as const,
  estimate: (scope: QueryScope, inputKey: string) =>
    [
      ...projectQueryKeys.all,
      "estimate",
      ...scopeKey(scope),
      inputKey,
    ] as const,
  start: (scope: QueryScope) =>
    [...projectQueryKeys.all, "start", ...scopeKey(scope)] as const,
};

export function listProjects(tenantId: string): Promise<ProjectListResponse> {
  return apiFetch<ProjectListResponse>("/projects?limit=50", { tenantId });
}

export function getProject(
  projectId: string,
  tenantId: string,
): Promise<Project> {
  return apiFetch<Project>(`/projects/${encodeURIComponent(projectId)}`, {
    tenantId,
  });
}

export function createProject(
  tenantId: string,
  input: CreateProjectInput,
  idempotencyKey?: ApiRequestOptions["idempotencyKey"],
): Promise<Project> {
  return apiFetch<Project>("/projects", {
    method: "POST",
    body: input,
    tenantId,
    idempotencyKey,
  });
}

export function estimateProject(
  tenantId: string,
  input: CreateProjectInput,
): Promise<ProjectEstimate> {
  return apiFetch<ProjectEstimate>("/projects/estimate", {
    method: "POST",
    body: input,
    tenantId,
  });
}

export function startProject(
  tenantId: string,
  projectId: string,
  input: StartProjectInput,
  idempotencyKey?: ApiRequestOptions["idempotencyKey"],
): Promise<ProjectStartAcceptance> {
  return apiFetch<ProjectStartAcceptance>(
    `/projects/${encodeURIComponent(projectId)}/start`,
    {
      method: "POST",
      body: input,
      tenantId,
      idempotencyKey,
    },
  );
}

/**
 * A project without an accepted start is normal: the server returns 404 until
 * the first acceptance exists.
 */
export async function getProjectStart(
  projectId: string,
  tenantId: string,
): Promise<ProjectStartAcceptance | undefined> {
  try {
    return await apiFetch<ProjectStartAcceptance>(
      `/projects/${encodeURIComponent(projectId)}/start`,
      { tenantId },
    );
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return undefined;
    throw error;
  }
}

export function updateProject(
  tenantId: string,
  projectId: string,
  input: UpdateProjectInput,
): Promise<Project> {
  return apiFetch<Project>(`/projects/${encodeURIComponent(projectId)}`, {
    method: "PATCH",
    body: input,
    headers: { "If-Match": String(input.revision) },
    tenantId,
  });
}

export function getProjectOverview(
  projectId: string,
  tenantId: string,
): Promise<ProjectOverview> {
  return apiFetch<ProjectOverview>(
    `/projects/${encodeURIComponent(projectId)}/overview`,
    { tenantId },
  );
}

function useScope(tenantId: string | undefined, projectId?: string) {
  const { session } = useAuth();
  return session && tenantId
    ? queryScopeFor(session, tenantId, projectId)
    : undefined;
}

function estimateInputKey(input: CreateProjectInput) {
  return JSON.stringify(input);
}

export function useProjectsQuery(tenantId: string | undefined) {
  const scope = useScope(tenantId);
  return useQuery({
    queryKey: scope
      ? projectQueryKeys.list(scope)
      : [
          ...projectQueryKeys.all,
          "list",
          "anonymous",
          "",
          tenantId ?? "",
          null,
        ],
    queryFn: () => listProjects(tenantId!),
    enabled: Boolean(scope),
  });
}

export function useProjectQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? projectQueryKeys.detail(scope)
      : [
          ...projectQueryKeys.all,
          "detail",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: () => getProject(projectId!, tenantId!),
    enabled: Boolean(scope && projectId),
  });
}

export function useProjectOverviewQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? projectQueryKeys.overview(scope)
      : [
          ...projectQueryKeys.all,
          "overview",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: () => getProjectOverview(projectId!, tenantId!),
    enabled: Boolean(scope && projectId),
  });
}

export function useProjectEstimateQuery(
  tenantId: string | undefined,
  input: CreateProjectInput | undefined,
) {
  const scope = useScope(tenantId);
  const inputKey = input ? estimateInputKey(input) : "invalid-input";
  return useQuery({
    queryKey: scope
      ? projectQueryKeys.estimate(scope, inputKey)
      : [
          ...projectQueryKeys.all,
          "estimate",
          "anonymous",
          "",
          tenantId ?? "",
          null,
          inputKey,
        ],
    queryFn: () => estimateProject(tenantId!, input!),
    enabled: Boolean(scope && input),
  });
}

export function useProjectStartQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? projectQueryKeys.start(scope)
      : [
          ...projectQueryKeys.all,
          "start",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: () => getProjectStart(projectId!, tenantId!),
    enabled: Boolean(scope && projectId),
  });
}

export function useCreateProjectMutation(tenantId: string | undefined) {
  const queryClient = useQueryClient();
  const scope = useScope(tenantId);
  return useMutation({
    mutationFn: ({
      input,
      idempotencyKey,
    }: {
      input: CreateProjectInput;
      idempotencyKey: string;
    }) => {
      if (!tenantId) throw new Error("请先选择工作区。");
      return createProject(tenantId, input, idempotencyKey);
    },
    onSuccess: async () => {
      if (!scope) return;
      await queryClient.invalidateQueries({
        queryKey: projectQueryKeys.list(scope),
      });
    },
  });
}

export function useStartProjectMutation(tenantId: string | undefined) {
  const queryClient = useQueryClient();
  const { session } = useAuth();
  return useMutation({
    mutationFn: ({
      projectId,
      expectedRevision,
      idempotencyKey,
    }: {
      projectId: string;
      expectedRevision: number;
      idempotencyKey: string;
    }) => {
      if (!tenantId) throw new Error("请先选择工作区。");
      return startProject(
        tenantId,
        projectId,
        { expected_revision: expectedRevision },
        idempotencyKey,
      );
    },
    onSuccess: async (_acceptance, { projectId }) => {
      if (!tenantId || !session) return;
      const projectScope = queryScopeFor(session, tenantId, projectId);
      const listScope = queryScopeFor(session, tenantId);
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: projectQueryKeys.list(listScope),
        }),
        queryClient.invalidateQueries({
          queryKey: projectQueryKeys.detail(projectScope),
        }),
        queryClient.invalidateQueries({
          queryKey: projectQueryKeys.overview(projectScope),
        }),
        queryClient.invalidateQueries({
          queryKey: projectQueryKeys.start(projectScope),
        }),
      ]);
    },
  });
}

export function useUpdateProjectMutation(
  tenantId: string | undefined,
  projectId: string,
) {
  const queryClient = useQueryClient();
  const scope = useScope(tenantId, projectId);
  return useMutation({
    mutationFn: (input: UpdateProjectInput) => {
      if (!tenantId) throw new Error("请先选择工作区。");
      return updateProject(tenantId, projectId, input);
    },
    onSuccess: async (project) => {
      if (!scope) return;
      await Promise.all([
        queryClient.setQueryData(projectQueryKeys.detail(scope), project),
        queryClient.invalidateQueries({
          queryKey: projectQueryKeys.list({ ...scope, projectId: undefined }),
        }),
        queryClient.invalidateQueries({
          queryKey: projectQueryKeys.overview(scope),
        }),
      ]);
    },
  });
}
