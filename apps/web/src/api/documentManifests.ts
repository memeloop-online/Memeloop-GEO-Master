import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../auth/AuthProvider";
import { queryScopeFor, type QueryScope } from "../auth/types";
import { ApiError, apiFetch } from "./client";

export interface DocumentManifestPlanInput {
  manifest_id: string;
  knowledge_release_id: string;
}

export interface DocumentManifestItem {
  document_manifest_item_id: string;
  manifest_id: string;
  knowledge_release_id: string;
  document_key: string;
  content_type: string;
  product_id?: string | null;
  market: string;
  language: string;
  state: "planned" | "blocked" | "deferred" | "not_applicable";
  block_reason?: string | null;
  dependency_hash: string;
  source_version_refs: string[];
}

export interface DocumentManifest {
  manifest_id: string;
  revision: number;
  knowledge_release_id: string;
  planner_version: string;
  state: "awaiting_knowledge" | "planning" | "ready" | "closed";
  sealed: boolean;
  expected_count: number | null;
  scope_hash: string;
  items: DocumentManifestItem[];
  coverage: {
    total: number;
    planned: number;
    blocked: number;
    deferred: number;
    not_applicable: number;
  };
}

export const documentManifestQueryKey = (
  scope: QueryScope,
  manifestId: string,
) =>
  [
    "document-manifests",
    "detail",
    scope.userId,
    scope.operatorId,
    scope.tenantId,
    scope.projectId ?? "",
    manifestId,
  ] as const;

/** A newly created draft handle has no persisted plan until the first POST. */
export async function getDocumentManifest(
  tenantId: string,
  projectId: string,
  manifestId: string,
): Promise<DocumentManifest | null> {
  try {
    return await apiFetch<DocumentManifest>(
      `/knowledge/document-manifests/${encodeURIComponent(manifestId)}`,
      { tenantId, projectId },
    );
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

export function useDocumentManifestQuery(
  tenantId: string | undefined,
  projectId: string | undefined,
  manifestId: string | undefined,
) {
  const { session } = useAuth();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useQuery({
    queryKey: scope
      ? documentManifestQueryKey(scope, manifestId ?? "")
      : [
          "document-manifests",
          "detail",
          "anonymous",
          "",
          tenantId,
          projectId,
          manifestId,
        ],
    queryFn: () => getDocumentManifest(tenantId!, projectId!, manifestId!),
    enabled: Boolean(scope && manifestId),
  });
}

export function planDocumentManifest(
  tenantId: string,
  projectId: string,
  input: DocumentManifestPlanInput,
): Promise<DocumentManifest> {
  return apiFetch<DocumentManifest>("/knowledge/document-manifests/plan", {
    method: "POST",
    tenantId,
    projectId,
    body: input,
  });
}

export function usePlanDocumentManifestMutation(
  tenantId: string | undefined,
  projectId: string | undefined,
) {
  const { session } = useAuth();
  const queryClient = useQueryClient();
  const scope =
    session && tenantId && projectId
      ? queryScopeFor(session, tenantId, projectId)
      : undefined;
  return useMutation({
    mutationKey: [
      "document-manifests",
      "plan",
      scope?.userId ?? "",
      scope?.operatorId ?? "",
      scope?.tenantId ?? "",
      scope?.projectId ?? "",
    ],
    mutationFn: (input: DocumentManifestPlanInput) => {
      if (!scope || !tenantId || !projectId) {
        throw new Error("请先选择有权限的项目。");
      }
      return planDocumentManifest(tenantId, projectId, input);
    },
    onSuccess: async (result, input) => {
      if (!scope) return;
      const queryKey = documentManifestQueryKey(scope, input.manifest_id);
      queryClient.setQueryData(queryKey, result);
      await queryClient.invalidateQueries({ queryKey });
    },
  });
}
