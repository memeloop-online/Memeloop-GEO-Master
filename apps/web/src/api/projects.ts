import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor, type QueryScope } from "../auth/types";
import { apiFetch, type ApiRequestOptions } from "./client";

export type ProjectStatus = "draft" | "active" | "paused" | "archived";
export type ResourceMode = "own" | "platform" | "mixed";
export type SourceKind = "url" | "text";
export type SourceVisibility = "public" | "internal";

export interface InitialSource {
  kind: SourceKind;
  value: string;
  visibility: SourceVisibility;
}

export interface ProjectSettings {
  brand_name: string;
  product_name: string;
  market: string;
  language: string;
  competitors: string[];
  resource_mode: ResourceMode;
  monthly_budget_minor: number;
  budget_currency: string;
  monitoring_reserve_percent: number;
  target_audience?: string;
  initial_sources?: InitialSource[];
}

export interface Project {
  id: string;
  slug: string;
  display_name: string;
  status: ProjectStatus;
  revision: number;
  settings: ProjectSettings;
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

export interface ProjectEstimateRange {
  minimum_minor: number;
  maximum_minor: number;
}

export interface ProjectEstimateCoverage {
  source_count: number;
  document_count: number;
  document_platform_target_count: number;
  measurement_sample_count: number;
}

/**
 * A deterministic planning estimate. It describes planned resource coverage
 * and cost ranges only; it is not an outcome forecast.
 */
export interface ProjectEstimate {
  currency: string;
  requested_monthly_budget_minor: number;
  monitoring_reserve_minor: number;
  coverage: ProjectEstimateCoverage;
  phase_one: ProjectEstimateRange;
  phase_two: ProjectEstimateRange;
  total: ProjectEstimateRange;
  basis: string[];
  assumptions: string[];
}

export type OperationStatus = "queued" | "running" | "succeeded" | "failed";

/** The asynchronous handle returned after a project start is accepted. */
export interface ProjectStartOperation {
  id: string;
  kind: string;
  status: OperationStatus;
  result?: unknown;
  error?: unknown;
  created_at: string;
  updated_at: string;
}

export interface ProjectStartAcceptance {
  projectId: string;
  operation?: ProjectStartOperation;
  acceptedAt: string;
}

export interface UpdateProjectInput {
  revision: number;
  display_name?: string;
  status?: ProjectStatus;
  settings?: Partial<ProjectSettings>;
}

export interface ProjectOverview {
  project: Project;
  cycle: {
    status: "not_started" | "running" | "paused";
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
  startAcceptance: (scope: QueryScope) =>
    [...projectQueryKeys.all, "start-acceptance", ...scopeKey(scope)] as const,
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
  idempotencyKey?: ApiRequestOptions["idempotencyKey"],
): Promise<ProjectStartOperation | undefined> {
  return apiFetch<ProjectStartOperation | undefined>(
    `/projects/${encodeURIComponent(projectId)}/start`,
    {
      method: "POST",
      tenantId,
      idempotencyKey,
    },
  );
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

/**
 * Keeps an accepted start handle scoped to the authenticated tenant/project.
 * The cache is cleared by AuthProvider when the session changes.
 */
export function useProjectStartAcceptance(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const scope = useScope(tenantId, projectId);
  return useQuery({
    queryKey: scope
      ? projectQueryKeys.startAcceptance(scope)
      : [
          ...projectQueryKeys.all,
          "start-acceptance",
          "anonymous",
          "",
          tenantId ?? "",
          projectId ?? null,
        ],
    queryFn: async () => undefined as ProjectStartAcceptance | undefined,
    enabled: false,
    staleTime: Infinity,
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
      idempotencyKey,
    }: {
      projectId: string;
      idempotencyKey: string;
    }) => {
      if (!tenantId) throw new Error("请先选择工作区。");
      return startProject(tenantId, projectId, idempotencyKey);
    },
    onSuccess: async (operation, { projectId }) => {
      if (!tenantId || !session) return;
      const projectScope = queryScopeFor(session, tenantId, projectId);
      const listScope = queryScopeFor(session, tenantId);
      const acceptance: ProjectStartAcceptance = {
        projectId,
        operation,
        acceptedAt: new Date().toISOString(),
      };
      queryClient.setQueryData(
        projectQueryKeys.startAcceptance(projectScope),
        acceptance,
      );
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
