import { apiFetch } from "./client";
import type { ProxyInput } from "./channels";

export type NetworkSource =
  | { kind: "direct" }
  | { kind: "standalone"; proxy: ProxyInput }
  | {
      kind: "pool_revision";
      pool_id: string;
      member_id: string;
      revision_id: string;
    };
export interface SavedNetwork {
  binding_id: string | null;
  pool_name?: string | null;
  member_name?: string | null;
  source: {
    kind: "direct" | "standalone" | "legacy_snapshot" | "pool_revision";
    pool_id?: string;
    member_id?: string;
    revision_id?: string;
  };
  server: string | null;
}
export interface AccountNetwork {
  account_id: string;
  version: number;
  active: SavedNetwork;
  pending: SavedNetwork | null;
  busy: boolean;
}
const path = (accountId: string) =>
  `/operator/channel-accounts/${encodeURIComponent(accountId)}/network-binding`;
export const getAccountNetwork = (accountId: string) =>
  apiFetch<AccountNetwork>(path(accountId));
export const saveAccountNetwork = (
  accountId: string,
  body: { expected_version: number; source: NetworkSource },
) =>
  apiFetch<AccountNetwork>(path(accountId), {
    method: "PUT",
    body,
    idempotency: "omit",
  });
