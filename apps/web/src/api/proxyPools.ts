import { apiFetch } from "./client";
import type { ProxyInput } from "./channels";

export interface ProxyPool {
  pool_id: string;
  name: string;
  enabled: boolean;
  version: number;
  created_at: string;
  updated_at: string;
}
export interface ProxyPoolMember {
  member_id: string;
  pool_id: string;
  name: string;
  enabled: boolean;
  version: number;
  revision_id: string;
  proxy_server: string;
  created_at: string;
  updated_at: string;
}
export interface ProxyPage<T> {
  items: T[];
  next_cursor: string | null;
}
const base = "/operator/channel-proxy-pools";
const poolPath = (id: string) => `${base}/${encodeURIComponent(id)}`;
const pageQuery = (cursor?: string) =>
  `?limit=25${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`;
export const listProxyPools = (cursor?: string) =>
  apiFetch<ProxyPage<ProxyPool>>(`${base}${pageQuery(cursor)}`);
export const createProxyPool = (name: string) =>
  apiFetch<ProxyPool>(base, {
    method: "POST",
    body: { name },
    idempotency: "omit",
  });
export const patchProxyPool = (
  id: string,
  body: { expected_version: number; name?: string; enabled?: boolean },
) =>
  apiFetch<ProxyPool>(poolPath(id), {
    method: "PATCH",
    body,
    idempotency: "omit",
  });
export const listProxyMembers = (poolId: string, cursor?: string) =>
  apiFetch<ProxyPage<ProxyPoolMember>>(
    `${poolPath(poolId)}/members${pageQuery(cursor)}`,
  );
export const createProxyMember = (
  poolId: string,
  body: { name: string; proxy: ProxyInput },
) =>
  apiFetch<ProxyPoolMember>(`${poolPath(poolId)}/members`, {
    method: "POST",
    body,
    idempotency: "omit",
  });
export const patchProxyMember = (
  poolId: string,
  memberId: string,
  body: {
    expected_version: number;
    name?: string;
    enabled?: boolean;
    proxy?: ProxyInput;
  },
) =>
  apiFetch<ProxyPoolMember>(
    `${poolPath(poolId)}/members/${encodeURIComponent(memberId)}`,
    { method: "PATCH", body, idempotency: "omit" },
  );
