import { apiFetch } from "./client";
import type { ContentRevision, RichNode } from "./content";

export type DistributionFormat = "markdown.v1" | "rich_markdown.v2";
export type MaterializationDeferralReason =
  | "project_paused"
  | "account_unavailable"
  | "connector_unavailable"
  | "content_not_ready"
  | "source_unavailable"
  | "format_unsupported"
  | "temporary_failure"
  | "internal_error";

export interface ContentDistributionInput {
  content_asset_id: string;
  content_revision_id: string;
  account_id: string;
  placement_slot: string;
  format: DistributionFormat;
}

export interface ContentDistributionRequest extends ContentDistributionInput {
  request_id: string;
  scope: {
    operator_id: string;
    tenant_id: string;
    project_id: string | null;
  };
  schema_version: number;
  platform_id: string;
  account_owner_kind: "customer" | "operator_pool";
  idempotency_key_hash: string;
  request_hash: string;
  publication_intent_id: string | null;
  materialization_deferral?: {
    reason: MaterializationDeferralReason | (string & {});
    attempts: number;
    next_retry_at: string;
  } | null;
  created_at: string;
}

/** A projection of the existing channel ledger, not the request's status. */
export interface ContentDistributionPublication {
  request_id: string;
  publication_intent_id: string | null;
  channel_target_id: string | null;
  attempt_id: string | null;
  outcome:
    | "published"
    | "verified"
    | "unknown"
    | "failed"
    | "login_required"
    | "unsupported"
    | "observed"
    | "refused"
    | "missing"
    | null;
  public_url: string | null;
  fixture: boolean | null;
}

function hasRichNode(node: RichNode): boolean {
  return (
    node.type === "media" ||
    node.type === "image" ||
    node.type === "table" ||
    node.type === "tableRow" ||
    node.type === "tableCell" ||
    node.type === "tableHeader" ||
    (node.content?.some(hasRichNode) ?? false)
  );
}

/** A rich or media revision must never be submitted under a plain-text proof. */
export function distributionFormat(
  revision: ContentRevision,
): DistributionFormat {
  return revision.document.schema_version === 2 ||
    revision.document.blocks.some(
      (block) => block.rich && hasRichNode(block.rich.node),
    )
    ? "rich_markdown.v2"
    : "markdown.v1";
}

const path = (projectId: string) =>
  `/projects/${encodeURIComponent(projectId)}/content-distribution-requests`;

export function submitContentDistributionRequest(
  tenantId: string,
  projectId: string,
  input: ContentDistributionInput,
  idempotencyKey: string,
) {
  return apiFetch<ContentDistributionRequest>(path(projectId), {
    tenantId,
    projectId,
    method: "POST",
    body: input,
    idempotencyKey,
  });
}

export function getContentDistributionRequest(
  tenantId: string,
  projectId: string,
  requestId: string,
) {
  return apiFetch<ContentDistributionRequest>(
    `${path(projectId)}/${encodeURIComponent(requestId)}`,
    { tenantId, projectId },
  );
}

export function getContentDistributionPublication(
  tenantId: string,
  projectId: string,
  requestId: string,
) {
  return apiFetch<ContentDistributionPublication>(
    `${path(projectId)}/${encodeURIComponent(requestId)}/publication`,
    { tenantId, projectId },
  );
}
