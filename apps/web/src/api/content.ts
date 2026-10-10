import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor, type QueryScope } from "../auth/types";
import { apiFetch, apiFetchBlob } from "./client";
import { getCurrentCycle } from "./channelJobs";

export interface ContentCoverage {
  total: number;
  ready: number;
  blocked: number;
  deferred: number;
  not_applicable: number;
  cancelled: number;
  incomplete: number;
}

export interface ContentExecution {
  execution_id: string;
  project_id: string;
  cycle_id: string;
  manifest_id: string;
  manifest_revision: number;
  policy_version: string;
  input_hash: string;
  status: "running" | "closed" | "cancelled";
  expected_count: number;
  coverage: ContentCoverage;
  handoff_id: string | null;
}

export interface ContentEvidence {
  reference: {
    source_version_id: string;
    chunk_id?: string | null;
    locator: { kind: string; [key: string]: unknown };
  };
  exact_quote: string;
}
export type ContentEvidenceRef = ContentEvidence["reference"];

export interface ContentBrief {
  brief_id: string;
  title: string;
  objective: string;
  evidence: ContentEvidenceRef[];
  quotes: ContentEvidence[];
  created_at: string;
}

export interface ContentItem {
  item_id: string;
  execution_id: string;
  document_key: string;
  branch_key: string;
  input_hash: string;
  planning_state: "planned" | "blocked" | "deferred" | "not_applicable";
  planning_reason: string | null;
  status:
    | "pending"
    | "prepared"
    | "drafted"
    | "needs_repair"
    | "ready"
    | "blocked"
    | "deferred"
    | "not_applicable"
    | "cancelled";
  reason: string | null;
  source_version_refs: string[];
  brief: ContentBrief | null;
  asset_id: string | null;
  current_revision_id: string | null;
  ready_revision_id: string | null;
  reuse_binding?: {
    origin_execution_id: string;
    origin_item_id: string;
    asset_id: string;
    revision_id: string;
    check_id: string;
    fingerprint: string;
    reused_at: string;
  } | null;
  automatic_repair_count?: number;
  steps: Array<{
    step: "prepare" | "generate" | "check" | "repair";
    expires_at: string;
  }>;
}

export interface ContentAsset {
  asset_id: string;
  execution_id: string;
  item_id: string;
  current_revision_id: string;
  created_at: string;
}

export interface ContentBlock {
  block_id: string;
  kind: "heading" | "paragraph" | "list" | "rich";
  text: string;
  citation_ids: string[];
  items: string[];
  rich?: { version: 1; node: RichNode } | null;
}

export interface RichNode {
  type: string;
  attrs?: Record<string, unknown>;
  content?: RichNode[];
  text?: string;
  marks?: Array<{ type: string; attrs?: Record<string, unknown> }>;
}

export interface StructuredDocument {
  title: string;
  blocks: ContentBlock[];
  schema_version?: 2;
}

export interface ContentRevisionExport {
  revision_id: string;
  format: "markdown" | "html";
  media_type: string;
  filename: string;
  content: string;
}

export interface ContentFinding {
  finding_id: string;
  code: string;
  block_id: string | null;
  evidence: ContentEvidenceRef[];
  detail: string;
  blocking: boolean;
}

export interface ContentRevision {
  revision_id: string;
  asset_id: string;
  revision: number;
  base_revision_id: string | null;
  derived_from_revision_id?: string | null;
  document: StructuredDocument;
  markdown: string;
  evidence: ContentEvidenceRef[];
  quotes: ContentEvidence[];
  findings: ContentFinding[];
  created_at: string;
}

const encoded = encodeURIComponent;
const projectPath = (projectId: string) => `/projects/${encoded(projectId)}`;
const scopeOptions = (tenantId: string, projectId: string) => ({
  tenantId,
  projectId,
});
const key = (scope: QueryScope, ...parts: string[]) =>
  [
    "content",
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? "",
    ...parts,
  ] as const;

export function listContentExecutions(
  tenantId: string,
  projectId: string,
  cycleId: string,
) {
  return apiFetch<ContentExecution[]>(
    `${projectPath(projectId)}/cycles/${encoded(cycleId)}/document-executions`,
    scopeOptions(tenantId, projectId),
  );
}

export function startContentExecution(
  tenantId: string,
  projectId: string,
  cycleId: string,
) {
  return apiFetch<ContentExecution>(
    `${projectPath(projectId)}/cycles/${encoded(cycleId)}/document-executions`,
    { ...scopeOptions(tenantId, projectId), method: "POST", body: {} },
  );
}

export function resumeContentExecution(
  tenantId: string,
  projectId: string,
  executionId: string,
) {
  return apiFetch<ContentExecution>(
    `${projectPath(projectId)}/document-executions/${encoded(executionId)}/resume`,
    { ...scopeOptions(tenantId, projectId), method: "POST" },
  );
}

export function cancelContentExecution(
  tenantId: string,
  projectId: string,
  executionId: string,
) {
  return apiFetch<ContentExecution>(
    `${projectPath(projectId)}/document-executions/${encoded(executionId)}/cancel`,
    { ...scopeOptions(tenantId, projectId), method: "POST" },
  );
}

export function getContentExecution(
  tenantId: string,
  projectId: string,
  executionId: string,
) {
  return apiFetch<ContentExecution>(
    `${projectPath(projectId)}/document-executions/${encoded(executionId)}`,
    scopeOptions(tenantId, projectId),
  );
}

export function listContentItems(
  tenantId: string,
  projectId: string,
  executionId: string,
) {
  return apiFetch<ContentItem[]>(
    `${projectPath(projectId)}/document-executions/${encoded(executionId)}/items`,
    scopeOptions(tenantId, projectId),
  );
}

export function getContentAsset(
  tenantId: string,
  projectId: string,
  assetId: string,
) {
  return apiFetch<ContentAsset>(
    `${projectPath(projectId)}/contents/${encoded(assetId)}`,
    scopeOptions(tenantId, projectId),
  );
}

export function listContentRevisions(
  tenantId: string,
  projectId: string,
  assetId: string,
) {
  return apiFetch<ContentRevision[]>(
    `${projectPath(projectId)}/contents/${encoded(assetId)}/revisions`,
    scopeOptions(tenantId, projectId),
  );
}

export function exportContentRevision(
  tenantId: string,
  projectId: string,
  assetId: string,
  revisionId: string,
  format: "markdown" | "html",
) {
  return apiFetch<ContentRevisionExport>(
    `${projectPath(projectId)}/contents/${encoded(assetId)}/revisions/${encoded(revisionId)}/export?format=${format}`,
    scopeOptions(tenantId, projectId),
  );
}

export function exportContentRevisionBundle(
  tenantId: string,
  projectId: string,
  assetId: string,
  revisionId: string,
  format: "markdown" | "html",
): Promise<Blob> {
  return apiFetchBlob(
    `${projectPath(projectId)}/contents/${encoded(assetId)}/revisions/${encoded(revisionId)}/export-bundle?format=${format}`,
    { ...scopeOptions(tenantId, projectId), accept: "application/zip" },
  );
}

export function appendContentRevision(
  tenantId: string,
  projectId: string,
  assetId: string,
  baseRevisionId: string,
  document: StructuredDocument,
) {
  return apiFetch<ContentRevision>(
    `${projectPath(projectId)}/contents/${encoded(assetId)}/revisions`,
    {
      ...scopeOptions(tenantId, projectId),
      method: "POST",
      body: { base_revision_id: baseRevisionId, document },
    },
  );
}

export function forkReusedContentItem(
  tenantId: string,
  projectId: string,
  executionId: string,
  itemId: string,
  baseRevisionId: string,
  document: StructuredDocument,
) {
  return apiFetch<ContentRevision>(
    `${projectPath(projectId)}/document-executions/${encoded(executionId)}/items/${encoded(itemId)}/fork`,
    {
      ...scopeOptions(tenantId, projectId),
      method: "POST",
      body: { base_revision_id: baseRevisionId, document },
    },
  );
}

export function useContentCycleQuery(
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
      ? key(scope, "current-cycle")
      : ["content", "anonymous", tenantId, projectId, "current-cycle"],
    queryFn: () => getCurrentCycle(tenantId!, projectId!),
    enabled: Boolean(scope),
  });
}

export function useContentExecutionsQuery(
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
      ? key(scope, "executions", cycleId ?? "")
      : ["content", "anonymous", tenantId, projectId, "executions", cycleId],
    queryFn: () => listContentExecutions(tenantId!, projectId!, cycleId!),
    enabled: Boolean(scope && cycleId),
    refetchInterval: (query) =>
      query.state.data?.some((entry) => entry.status === "running")
        ? 5000
        : false,
  });
}

export function useContentItemsQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  executionId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? key(scope, "items", executionId ?? "")
      : ["content", "anonymous", tenantId, projectId, "items", executionId],
    queryFn: () => listContentItems(tenantId!, projectId!, executionId!),
    enabled: Boolean(scope && executionId),
    refetchInterval: (query) =>
      query.state.data?.some((item) =>
        ["pending", "prepared", "drafted", "needs_repair"].includes(
          item.status,
        ),
      )
        ? 5000
        : false,
  });
}

export function useContentAssetQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  assetId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? key(scope, "asset", assetId ?? "")
      : ["content", "anonymous", tenantId, projectId, "asset", assetId],
    queryFn: () => getContentAsset(tenantId!, projectId!, assetId!),
    enabled: Boolean(scope && assetId),
  });
}

export function useContentRevisionsQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  assetId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? key(scope, "revisions", assetId ?? "")
      : ["content", "anonymous", tenantId, projectId, "revisions", assetId],
    queryFn: () => listContentRevisions(tenantId!, projectId!, assetId!),
    enabled: Boolean(scope && assetId),
  });
}

export function useStartContentExecutionMutation(
  tenantId: string,
  projectId: string,
  cycleId: string,
) {
  const { session } = useAuth();
  const client = useQueryClient();
  const scope = session && queryScopeFor(session, tenantId, projectId);
  return useMutation({
    mutationFn: () => startContentExecution(tenantId, projectId, cycleId),
    onSuccess: async () => {
      if (scope)
        await client.invalidateQueries({
          queryKey: key(scope, "executions", cycleId),
        });
    },
  });
}

export function useContentExecutionActionMutation(
  tenantId: string,
  projectId: string,
  cycleId: string,
  action: "resume" | "cancel",
) {
  const { session } = useAuth();
  const client = useQueryClient();
  const scope = session && queryScopeFor(session, tenantId, projectId);
  return useMutation({
    mutationFn: (executionId: string) =>
      action === "resume"
        ? resumeContentExecution(tenantId, projectId, executionId)
        : cancelContentExecution(tenantId, projectId, executionId),
    onSuccess: async (result) => {
      if (!scope) return;
      const listKey = key(scope, "executions", cycleId);
      client.setQueryData<ContentExecution[]>(listKey, (current) =>
        current?.map((entry) =>
          entry.execution_id === result.execution_id ? result : entry,
        ),
      );
      await Promise.all([
        client.invalidateQueries({ queryKey: listKey }),
        client.invalidateQueries({
          queryKey: key(scope, "items", result.execution_id),
        }),
      ]);
    },
  });
}

export function useAppendContentRevisionMutation(
  tenantId: string,
  projectId: string,
  assetId: string,
) {
  const { session } = useAuth();
  const client = useQueryClient();
  const scope = session && queryScopeFor(session, tenantId, projectId);
  return useMutation({
    mutationFn: ({
      baseRevisionId,
      document,
    }: {
      baseRevisionId: string;
      document: StructuredDocument;
    }) =>
      appendContentRevision(
        tenantId,
        projectId,
        assetId,
        baseRevisionId,
        document,
      ),
    onSuccess: async () => {
      if (scope) {
        await client.invalidateQueries({
          queryKey: key(scope, "asset", assetId),
        });
        await client.invalidateQueries({
          queryKey: key(scope, "revisions", assetId),
        });
      }
    },
  });
}

export function useForkReusedContentItemMutation(
  tenantId: string,
  projectId: string,
  executionId: string,
  itemId: string,
) {
  const { session } = useAuth();
  const client = useQueryClient();
  const scope = session && queryScopeFor(session, tenantId, projectId);
  return useMutation({
    mutationFn: ({
      baseRevisionId,
      document,
    }: {
      baseRevisionId: string;
      document: StructuredDocument;
    }) =>
      forkReusedContentItem(
        tenantId,
        projectId,
        executionId,
        itemId,
        baseRevisionId,
        document,
      ),
    onSuccess: async (result) => {
      if (!scope) return;
      await Promise.all([
        client.invalidateQueries({
          queryKey: key(scope, "items", executionId),
        }),
        client.invalidateQueries({
          queryKey: key(scope, "asset", result.asset_id),
        }),
        client.invalidateQueries({
          queryKey: key(scope, "revisions", result.asset_id),
        }),
      ]);
    },
  });
}
