import { apiFetch } from "./client";
import type { ChannelPlatformId } from "./channels";

export interface AccountImportRow {
  row_key: string;
  platform: ChannelPlatformId;
  username: string;
  password: string;
  display_alias?: string;
  group_id?: string;
}

export interface AccountImportRequest {
  import_key: string;
  rows: AccountImportRow[];
}

export interface AccountImportResult {
  import_key: string;
  rows: {
    row_key: string;
    ordinal: number;
    account_id: string | null;
    status: "needs_login" | "failed";
    error_code: string | null;
    masked_username: string | null;
    display_alias: string | null;
  }[];
}

const base = "/operator/channel-account-imports";
export const createAccountImport = (body: AccountImportRequest) =>
  apiFetch<AccountImportResult>(base, {
    method: "POST",
    body,
    idempotencyKey: body.import_key,
  });
export const getAccountImport = (key: string) =>
  apiFetch<AccountImportResult>(`${base}/${encodeURIComponent(key)}`);
export const retryAccountImportRow = (key: string, rowKey: string) =>
  apiFetch<AccountImportResult>(
    `${base}/${encodeURIComponent(key)}/rows/${encodeURIComponent(rowKey)}/retry`,
    { method: "POST", body: {}, idempotency: "omit" },
  );
