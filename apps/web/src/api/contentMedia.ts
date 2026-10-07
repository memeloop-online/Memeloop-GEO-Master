import type { AgentAttachmentReference } from "./agent";
import { apiFetch, apiFetchBlob } from "./client";

export interface MediaObjectKey {
  object_id: string;
  object_version: number;
  sha256: string;
}

export interface MediaReference extends MediaObjectKey {
  alt: string;
  caption: string;
}

export interface ContentMediaBinding {
  binding_id: string;
  operator_id: string;
  tenant_id: string;
  project_id: string;
  image: {
    key: MediaObjectKey;
    media_type: string;
    byte_len: number;
    width: number;
    height: number;
  };
  state: "active" | "withdrawn";
  created_at: string;
  withdrawn_at: string | null;
}

export interface MediaBindingPage {
  items: ContentMediaBinding[];
  next_cursor: string | null;
}

const base = (projectId: string) =>
  `/projects/${encodeURIComponent(projectId)}/content-media/bindings`;
const bindingPath = (projectId: string, bindingId: string) =>
  `${base(projectId)}/${encodeURIComponent(bindingId)}`;
const scope = (tenantId: string, projectId: string) => ({
  tenantId,
  projectId,
});

export function validMediaObjectKey(value: unknown): value is MediaObjectKey {
  if (typeof value !== "object" || value === null) return false;
  const key = value as Record<string, unknown>;
  return (
    typeof key.object_id === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(
      key.object_id,
    ) &&
    key.object_id !== "00000000-0000-0000-0000-000000000000" &&
    Number.isSafeInteger(key.object_version) &&
    Number(key.object_version) > 0 &&
    typeof key.sha256 === "string" &&
    /^[0-9a-f]{64}$/.test(key.sha256)
  );
}

export function validMediaReference(value: unknown): value is MediaReference {
  if (!validMediaObjectKey(value)) return false;
  const media = value as MediaReference;
  return (
    typeof media.alt === "string" &&
    Boolean(media.alt.trim()) &&
    typeof media.caption === "string" &&
    new TextEncoder().encode(media.alt).length <= 1_000_000 &&
    new TextEncoder().encode(media.caption).length <= 1_000_000
  );
}

export function mediaKeyFromAttachment(
  attachment: AgentAttachmentReference,
): MediaObjectKey {
  const candidate = {
    object_id: attachment.object_id,
    object_version: Number(attachment.object_version),
    sha256: attachment.sha256,
  };
  if (!validMediaObjectKey(candidate))
    throw new Error("The uploaded image has no verified object identity.");
  return candidate;
}

export function listContentMedia(
  tenantId: string,
  projectId: string,
  after?: string,
): Promise<MediaBindingPage> {
  const query = new URLSearchParams({ limit: "50" });
  if (after) query.set("after", after);
  return apiFetch<MediaBindingPage>(
    `${base(projectId)}?${query}`,
    scope(tenantId, projectId),
  );
}

export function bindContentMedia(
  tenantId: string,
  projectId: string,
  key: MediaObjectKey,
): Promise<ContentMediaBinding> {
  if (!validMediaObjectKey(key)) throw new Error("Invalid image identity.");
  return apiFetch<ContentMediaBinding>(base(projectId), {
    ...scope(tenantId, projectId),
    method: "POST",
    body: key,
  });
}

export function readContentMediaBytes(
  tenantId: string,
  projectId: string,
  bindingId: string,
  signal?: AbortSignal,
): Promise<Blob> {
  return apiFetchBlob(`${bindingPath(projectId, bindingId)}/bytes`, {
    ...scope(tenantId, projectId),
    signal,
  });
}

/** Read the bounded PNG derivative used by authenticated media-list previews. */
export function readContentMediaThumbnail(
  tenantId: string,
  projectId: string,
  bindingId: string,
  signal?: AbortSignal,
): Promise<Blob> {
  return apiFetchBlob(`${bindingPath(projectId, bindingId)}/thumbnail`, {
    ...scope(tenantId, projectId),
    signal,
    accept: "image/png",
  });
}

export function withdrawContentMedia(
  tenantId: string,
  projectId: string,
  bindingId: string,
): Promise<ContentMediaBinding> {
  return apiFetch<ContentMediaBinding>(bindingPath(projectId, bindingId), {
    ...scope(tenantId, projectId),
    method: "DELETE",
  });
}

/** Listing uses current active bindings; historical previews can resolve them by key. */
export async function findContentMediaBinding(
  tenantId: string,
  projectId: string,
  key: MediaObjectKey,
  signal?: AbortSignal,
): Promise<ContentMediaBinding | null> {
  let after: string | null = null;
  do {
    if (signal?.aborted) return null;
    const page = await listContentMedia(
      tenantId,
      projectId,
      after ?? undefined,
    );
    const match = page.items.find(
      (item) =>
        item.state === "active" &&
        item.image.key.object_id === key.object_id &&
        item.image.key.object_version === key.object_version &&
        item.image.key.sha256 === key.sha256,
    );
    if (match) return match;
    after = page.next_cursor;
  } while (after);
  return null;
}
