import { apiFetch } from "./client";

export interface PartnerConnection {
  connection_id: string;
  knowledge_write: boolean;
  content_export: boolean;
  created_at: string;
  revoked_at: string | null;
}
export interface PartnerKey {
  key_id: string;
  created_at: string;
  expires_at: string | null;
  revoked_at: string | null;
}
export interface PartnerPage<T> {
  items: T[];
  next_cursor: string | null;
}
export interface IssuedPartnerKey {
  key: PartnerKey;
  bearer: string;
}
const path = (projectId: string, connectionId?: string) =>
  `/projects/${encodeURIComponent(projectId)}/partner-connections${connectionId ? `/${encodeURIComponent(connectionId)}` : ""}`;
function pagePath(base: string, after?: string) {
  const query = new URLSearchParams({ limit: "100" });
  if (after) query.set("after", after);
  return `${base}?${query}`;
}
export function listPartnerConnections(
  tenantId: string,
  projectId: string,
  after?: string,
) {
  return apiFetch<PartnerPage<PartnerConnection>>(
    pagePath(path(projectId), after),
    {
      tenantId,
      projectId,
      cache: "no-store",
    },
  );
}
export function listPartnerKeys(
  tenantId: string,
  projectId: string,
  connectionId: string,
  after?: string,
) {
  return apiFetch<PartnerPage<PartnerKey>>(
    pagePath(`${path(projectId, connectionId)}/keys`, after),
    {
      tenantId,
      projectId,
      cache: "no-store",
    },
  );
}
function command<T>(
  tenantId: string,
  projectId: string,
  url: string,
  body: object = {},
) {
  return apiFetch<T>(url, {
    tenantId,
    projectId,
    method: "POST",
    body,
    cache: "no-store",
    idempotency: "omit",
  });
}
export function createPartnerConnection(tenantId: string, projectId: string) {
  return command<PartnerConnection>(tenantId, projectId, path(projectId));
}
export function setPartnerContentExport(
  tenantId: string,
  projectId: string,
  connectionId: string,
  enabled: boolean,
) {
  return command<PartnerConnection>(
    tenantId,
    projectId,
    `${path(projectId, connectionId)}/permissions`,
    { content_export: enabled },
  );
}
export function issuePartnerKey(
  tenantId: string,
  projectId: string,
  connectionId: string,
  expiresAt: string | null = null,
) {
  return command<IssuedPartnerKey>(
    tenantId,
    projectId,
    `${path(projectId, connectionId)}/keys`,
    { expires_at: expiresAt },
  );
}
export function rotatePartnerKey(
  tenantId: string,
  projectId: string,
  connectionId: string,
  keyId: string,
  expiresAt: string | null = null,
) {
  return command<IssuedPartnerKey>(
    tenantId,
    projectId,
    `${path(projectId, connectionId)}/keys/${encodeURIComponent(keyId)}/rotate`,
    { expires_at: expiresAt },
  );
}
export function revokePartnerKey(
  tenantId: string,
  projectId: string,
  connectionId: string,
  keyId: string,
) {
  return command<PartnerKey>(
    tenantId,
    projectId,
    `${path(projectId, connectionId)}/keys/${encodeURIComponent(keyId)}/revoke`,
  );
}
export function revokePartnerConnection(
  tenantId: string,
  projectId: string,
  connectionId: string,
) {
  return command<PartnerConnection>(
    tenantId,
    projectId,
    `${path(projectId, connectionId)}/revoke`,
  );
}
