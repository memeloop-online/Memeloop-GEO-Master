import { apiFetch } from "./client";

export interface LoginQueueSelection {
  import_key: string;
  items: { row_key: string; account_id: string }[];
}
export interface LoginQueueView {
  queue_id: string;
  import_key: string;
  status: "running" | "paused" | "completed";
  version: number;
  items: {
    position: number;
    row_key: string;
    account_id: string;
    status: "pending" | "current" | "succeeded" | "skipped";
    attempt: number;
    error_code: string | null;
  }[];
  current: {
    position: number;
    account_id: string;
    session_id: string | null;
    phase: "preparing" | "awaiting_login" | "closing" | "recovery_required";
    requires_session_recovery: boolean;
  } | null;
  can_drive: boolean;
  created_at: string;
  updated_at: string;
}
export type LoginQueueAction =
  "drive" | "pause" | "resume" | "retry" | "skip" | "complete";
const base = "/operator/channel-login-queues";
export const getActiveLoginQueue = () =>
  apiFetch<LoginQueueView | null>(`${base}/active`);
export const getLoginQueue = (id: string) =>
  apiFetch<LoginQueueView>(`${base}/${encodeURIComponent(id)}`);
export const createLoginQueue = (
  body: LoginQueueSelection & { queue_id: string },
) =>
  apiFetch<LoginQueueView>(base, { method: "POST", body, idempotency: "omit" });
export const actOnLoginQueue = (
  id: string,
  action: LoginQueueAction,
  version: number,
) =>
  apiFetch<LoginQueueView>(`${base}/${encodeURIComponent(id)}/${action}`, {
    method: "POST",
    body: { expected_version: version },
    idempotency: "omit",
  });
