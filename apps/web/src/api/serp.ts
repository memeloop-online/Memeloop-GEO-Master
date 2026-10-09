import { apiFetch } from "./client";

export type SerpState =
  | "queued"
  | "claimed"
  | "sending"
  | "awaiting_result"
  | "completed"
  | "unknown"
  | "failed"
  | "cancelled";
export type SerpTarget =
  | { kind: "url"; url: string }
  | { kind: "host"; host: string; include_subdomains: boolean };
export interface SerpProtocol {
  query: string;
  engine: string;
  surface: string;
  source: string;
  country: string;
  city: string | null;
  language: string;
  device: string;
  requested_depth: number;
  [key: string]: unknown;
}
export interface SerpCapability {
  source_key: string;
  protocol_defaults: SerpProtocol;
}
export interface SerpMeasurement {
  measurement_id: string;
  protocol: SerpProtocol;
  target: SerpTarget | null;
  scheduled_at: string;
  created_at: string;
  state: SerpState;
}
export interface SerpResult {
  kind:
    | "organic"
    | "advertisement"
    | "featured_snippet"
    | "maps"
    | "ai_overview"
    | "other";
  raw_kind: string;
  raw_url: string | null;
  normalized_url: string | null;
  host: string | null;
  title: string | null;
  page: number | null;
  position: number;
  organic_rank: number | null;
  absolute_position: number | null;
  locator: string;
}
export interface SerpObservation {
  observation_id: string;
  measurement_id: string;
  attempt_id: string;
  raw_evidence_id: string;
  raw_sha256: string;
  parser_version: string;
  provider_observed_at: string | null;
  received_at: string;
  analyzed_at: string;
  status: string;
  actual_conditions: Record<
    string,
    { value: string; evidence_locator: string } | null
  >;
  coverage: {
    requested_depth: number;
    observed_organic_depth: number;
    pages_received: number;
    completion: string;
    truncated: boolean;
  };
  results: SerpResult[];
  source_limitations: string[];
}
export interface SerpDetail {
  measurement: SerpMeasurement;
  observations: SerpObservation[];
  next_after: string | null;
  execution: {
    attempt_id: string | null;
    provider_task_id: string | null;
    next_poll_at: string | null;
  } | null;
}
export interface SerpRawReceipt {
  evidence_id: string;
  measurement_id: string;
  attempt_id: string;
  operation: string;
  response_sha256: string;
  body_bytes: number;
  body_complete: boolean;
  http_status: number | null;
  captured_at: string;
  stored_at: string;
}
export interface SerpStoredRaw {
  evidence: Omit<SerpRawReceipt, "body_bytes" | "stored_at"> & {
    body: number[];
  };
  stored_at: string;
}
export interface CreateSerpMeasurement {
  idempotency_key: string;
  source_key: string;
  query: string;
  target?: SerpTarget;
  scheduled_at: string;
}
function base(projectId: string) {
  return `/projects/${encodeURIComponent(projectId)}/serp-measurements`;
}
const page = (after?: string) =>
  `?limit=20${after ? `&after=${encodeURIComponent(after)}` : ""}`;
export function getSerpCapabilities(tenantId: string, projectId: string) {
  return apiFetch<SerpCapability[]>(
    `/projects/${encodeURIComponent(projectId)}/serp-capabilities`,
    { tenantId, projectId },
  );
}
export function listSerpMeasurements(
  tenantId: string,
  projectId: string,
  after?: string,
) {
  return apiFetch<{ items: SerpMeasurement[]; next_after: string | null }>(
    base(projectId) + page(after),
    { tenantId, projectId },
  );
}
export function getSerpMeasurement(
  tenantId: string,
  projectId: string,
  id: string,
  after?: string,
) {
  return apiFetch<SerpDetail>(
    `${base(projectId)}/${encodeURIComponent(id)}${page(after)}`,
    { tenantId, projectId },
  );
}
export function createSerpMeasurement(
  tenantId: string,
  projectId: string,
  request: CreateSerpMeasurement,
) {
  return apiFetch<SerpMeasurement>(base(projectId), {
    tenantId,
    projectId,
    method: "POST",
    body: request,
    idempotency: "omit",
  });
}
export function cancelSerpMeasurement(
  tenantId: string,
  projectId: string,
  id: string,
) {
  return apiFetch<SerpMeasurement>(
    `${base(projectId)}/${encodeURIComponent(id)}/cancel`,
    { tenantId, projectId, method: "POST", body: {}, idempotency: "omit" },
  );
}
export function listSerpSources(
  tenantId: string,
  projectId: string,
  id: string,
  after?: string,
) {
  return apiFetch<{ items: SerpRawReceipt[]; next_after: string | null }>(
    `${base(projectId)}/${encodeURIComponent(id)}/sources${page(after)}`,
    { tenantId, projectId },
  );
}
export function getSerpRaw(
  tenantId: string,
  projectId: string,
  id: string,
  evidenceId: string,
) {
  return apiFetch<SerpStoredRaw>(
    `${base(projectId)}/${encodeURIComponent(id)}/raw/${encodeURIComponent(evidenceId)}`,
    { tenantId, projectId },
  );
}
