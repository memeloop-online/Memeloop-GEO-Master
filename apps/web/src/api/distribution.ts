import { ApiError, apiFetch } from "./client";

export type DistributionTargetStatus =
  | "pending"
  | "blocked"
  | "deferred"
  | "not_applicable"
  | "cancelled"
  | "ready"
  | "reused_verified"
  | "reused_unknown";

export interface PlatformPlacement {
  platform_id: string;
  placement_slot: string;
  capability_version: string;
  supported_formats: string[];
  unavailable_reason: string | null;
  fixture: boolean;
}

export interface DistributionDocument {
  document_item_id: string;
  document_key: string;
  content_type: string;
  status: string;
  reason: string | null;
  content_revision_id: string | null;
}

export interface DistributionManifest {
  manifest_id: string;
  project_id: string;
  cycle_id: string;
  revision: number;
  document_manifest_id: string;
  document_manifest_revision: number;
  content_execution_id: string;
  content_handoff_id: string;
  platform_scope: PlatformPlacement[];
  document_roster: DistributionDocument[];
  input_hash: string;
  expected_count: number;
  sealed_at: string;
  expansion_cursor: number;
  complete: boolean;
}

export interface DistributionTarget {
  target_id: string;
  manifest_id: string;
  ordinal: number;
  document_item_id: string;
  content_revision_id: string | null;
  platform_id: string;
  placement_slot: string;
  variant_id: string | null;
  account_id: string | null;
  publication_intent_id: string | null;
  status: DistributionTargetStatus;
  reason: string | null;
  version: number;
}

export interface DistributionTargetPage {
  manifest_id: string;
  rows: DistributionTarget[];
  next_ordinal: number | null;
  expected_count: number;
}

export interface PublicationTargetReference {
  distribution_target_id: string;
  publication_intent_id: string;
  channel_target_id: string;
}

const encoded = encodeURIComponent;
const scope = (tenantId: string, projectId: string) => ({
  tenantId,
  projectId,
});
const base = (projectId: string) => `/projects/${encoded(projectId)}`;

/** An absent formal manifest is distinct from an API or permission failure. */
export async function getCycleDistributionManifest(
  tenantId: string,
  projectId: string,
  cycleId: string,
): Promise<DistributionManifest | null> {
  try {
    return await apiFetch<DistributionManifest>(
      `${base(projectId)}/cycles/${encoded(cycleId)}/distribution-manifest`,
      scope(tenantId, projectId),
    );
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

export function freezeDistributionManifest(
  tenantId: string,
  projectId: string,
  cycleId: string,
) {
  return apiFetch<DistributionManifest>(
    `${base(projectId)}/cycles/${encoded(cycleId)}/distribution-manifest`,
    { ...scope(tenantId, projectId), method: "POST" },
  );
}

export function getDistributionManifest(
  tenantId: string,
  projectId: string,
  manifestId: string,
) {
  return apiFetch<DistributionManifest>(
    `${base(projectId)}/distribution-manifests/${encoded(manifestId)}`,
    scope(tenantId, projectId),
  );
}

export function getDistributionTargets(
  tenantId: string,
  projectId: string,
  manifestId: string,
  afterOrdinal?: number,
  limit = 64,
) {
  const query = new URLSearchParams({ limit: String(limit) });
  if (afterOrdinal !== undefined)
    query.set("after_ordinal", String(afterOrdinal));
  return apiFetch<DistributionTargetPage>(
    `${base(projectId)}/distribution-manifests/${encoded(manifestId)}/targets?${query}`,
    scope(tenantId, projectId),
  );
}

export function getDistributionPublicationTarget(
  tenantId: string,
  projectId: string,
  manifestId: string,
  targetId: string,
  signal?: AbortSignal,
) {
  return apiFetch<PublicationTargetReference | null>(
    `${base(projectId)}/distribution-manifests/${encoded(manifestId)}/targets/${encoded(targetId)}/publication-target`,
    { ...scope(tenantId, projectId), signal },
  );
}

export function resumeDistributionManifest(
  tenantId: string,
  projectId: string,
  manifestId: string,
  afterOrdinal?: number,
) {
  const query =
    afterOrdinal === undefined
      ? ""
      : `?${new URLSearchParams({ after_ordinal: String(afterOrdinal) })}`;
  return apiFetch<DistributionManifest>(
    `${base(projectId)}/distribution-manifests/${encoded(manifestId)}/resume${query}`,
    { ...scope(tenantId, projectId), method: "POST" },
  );
}
