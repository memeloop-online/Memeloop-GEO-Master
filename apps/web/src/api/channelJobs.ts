import { ApiError, apiFetch } from "./client";

export type ChannelOutcomeStatus =
  | "published"
  | "verified"
  | "unknown"
  | "failed"
  | "login_required"
  | "unsupported"
  | "observed"
  | "refused"
  | "missing";

export type ChannelTargetInput =
  | {
      kind: "publish";
      source_id: string;
      source_version_id: string;
      platform: string;
      account_id: string;
      title: string;
      body: string;
      body_sha256: string;
    }
  | {
      kind: "measure";
      account_id: string;
      provider: string;
      model: string;
      surface: string;
      search_mode: string;
      protocol_version: string;
      question_set_version: string;
      question: string;
      market: string;
      language: string;
      scheduled_at: string;
      sample_ordinal: number;
      question_binding?: {
        reference: QuestionReference;
        purpose: "optimization" | "frozen_evaluation";
        split_policy_version: string;
      };
    };

export interface QuestionReference {
  question_set_id: string;
  question_set_version_id: string;
  question_id: string;
  question_revision_id: string;
}

export interface ChannelTarget {
  target_id: string;
  input: ChannelTargetInput;
}

export interface ChannelPlan {
  plan_id: string;
  project_id: string;
  cycle_id: string;
  input_hash: string;
  revision: number;
  created_at: string;
  targets: ChannelTarget[];
}

export interface CurrentCycle {
  project_id: string;
  cycle_id: string;
  report_timezone: string;
  report_window_start_at: string;
  report_window_end_at: string;
  cutoff_at: string;
  document_manifest?: unknown | null;
  distribution_manifest?: unknown | null;
}

export interface ChannelOutcome {
  status: ChannelOutcomeStatus;
  detail: string | null;
  occurred_at: string;
  raw_answer: string | null;
  citations: string[];
  public_url: string | null;
  screenshot_ref: string | null;
  connector_version: string | null;
  runner_evidence: unknown[];
  fixture: boolean;
}

export interface ChannelAttempt {
  attempt_id: string;
  target_id: string;
  claimed_at: string;
  outcome: ChannelOutcome | null;
  received_at: string | null;
}

export interface ChannelTargetView {
  target: ChannelTarget;
  attempts: ChannelAttempt[];
}

export interface PublicationLookupPage {
  target_id: string;
  attempt_id: string | null;
  job: {
    query_count: number;
    next_due_at: string | null;
    last_error_code: string | null;
    in_progress: boolean;
  } | null;
  observations: {
    execution_id: string;
    finding: "unknown" | "asset_observed";
    observed_at: string;
    received_at: string;
    error_code: string | null;
    public_url: string | null;
  }[];
  next_before: string | null;
}

export function getPublicationLookup(
  tenantId: string,
  projectId: string,
  targetId: string,
  before?: string,
  signal?: AbortSignal,
) {
  const path = `/projects/${encoded(projectId)}/channel-targets/${encoded(targetId)}/publication-lookup`;
  return apiFetch<PublicationLookupPage>(
    `${path}${before ? `?before=${encoded(before)}` : ""}`,
    { ...scope(tenantId, projectId), signal },
  );
}

export interface PublicationRequest {
  source_id: string;
  source_version_id: string;
  platform: string;
  account_id: string;
}

export interface MeasurementRequest {
  account_id: string;
  provider: string;
  model: string;
  surface: string;
  search_mode: string;
  protocol_version: string;
  question_set_version: string;
  question: string;
  market: string;
  language: string;
  scheduled_at: string;
  sample_ordinal: number;
}

export interface BoundMeasurementRequest {
  account_id: string;
  provider: string;
  model: string;
  surface: string;
  search_mode: string;
  protocol_version: string;
  question: QuestionReference;
  scheduled_at: string;
  sample_ordinal: number;
}

export interface ChannelPlanRequest {
  publications: PublicationRequest[];
  measurements: MeasurementRequest[];
  bound_measurements?: BoundMeasurementRequest[];
}

export interface StandaloneMeasurementPlan {
  plan_id: string;
  project_id: string;
  title: string;
  created_at: string;
  targets: ChannelTarget[];
}

export interface StandaloneMeasurementRequest {
  idempotency_key: string;
  title: string;
  measurements: MeasurementRequest[];
  bound_measurements?: BoundMeasurementRequest[];
}

export interface MeasurementOptions {
  models: { id: string; label: string }[];
  selected_model: string | null;
}

export function getMeasurementOptions(
  tenantId: string,
  projectId: string,
  accountId: string,
) {
  return apiFetch<MeasurementOptions>(
    `/projects/${encoded(projectId)}/channel-accounts/${encoded(accountId)}/measurement-options`,
    scope(tenantId, projectId),
  );
}

export function createMeasurementPlan(
  tenantId: string,
  projectId: string,
  input: StandaloneMeasurementRequest,
) {
  return apiFetch<StandaloneMeasurementPlan>(
    `/projects/${encoded(projectId)}/measurement-plans`,
    { ...scope(tenantId, projectId), method: "POST", body: input },
  );
}

export function listMeasurementPlans(
  tenantId: string,
  projectId: string,
  after?: string,
) {
  return apiFetch<{
    items: StandaloneMeasurementPlan[];
    next_after: string | null;
  }>(
    `/projects/${encoded(projectId)}/measurement-plans?limit=20${after ? `&after=${encoded(after)}` : ""}`,
    scope(tenantId, projectId),
  );
}

export function getMeasurementPlan(
  tenantId: string,
  projectId: string,
  planId: string,
) {
  return apiFetch<StandaloneMeasurementPlan>(
    `/projects/${encoded(projectId)}/measurement-plans/${encoded(planId)}`,
    scope(tenantId, projectId),
  );
}

const encoded = (value: string) => encodeURIComponent(value);
const scope = (tenantId: string, projectId: string) => ({
  tenantId,
  projectId,
});

/** A project may not yet have a cycle; this is not a loading failure. */
export async function getCurrentCycle(
  tenantId: string,
  projectId: string,
): Promise<CurrentCycle | null> {
  try {
    return await apiFetch<CurrentCycle>(
      `/projects/${encoded(projectId)}/cycles/current`,
      scope(tenantId, projectId),
    );
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

/** A missing plan is a normal state before the cycle is sealed. */
export async function getChannelPlan(
  tenantId: string,
  projectId: string,
  cycleId: string,
): Promise<ChannelPlan | null> {
  try {
    return await apiFetch<ChannelPlan>(
      `/projects/${encoded(projectId)}/cycles/${encoded(cycleId)}/channel-plan`,
      scope(tenantId, projectId),
    );
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) return null;
    throw error;
  }
}

export function submitChannelPlan(
  tenantId: string,
  projectId: string,
  cycleId: string,
  input: ChannelPlanRequest,
) {
  return apiFetch<ChannelPlan>(
    `/projects/${encoded(projectId)}/cycles/${encoded(cycleId)}/channel-plan`,
    { ...scope(tenantId, projectId), method: "POST", body: input },
  );
}

export function getChannelTarget(
  tenantId: string,
  projectId: string,
  targetId: string,
  signal?: AbortSignal,
) {
  return apiFetch<ChannelTargetView>(
    `/projects/${encoded(projectId)}/channel-targets/${encoded(targetId)}`,
    { ...scope(tenantId, projectId), signal },
  );
}

export function executeChannelTarget(
  tenantId: string,
  projectId: string,
  targetId: string,
) {
  return apiFetch<ChannelTargetView>(
    `/projects/${encoded(projectId)}/channel-targets/${encoded(targetId)}/execute`,
    { ...scope(tenantId, projectId), method: "POST", body: {} },
  );
}
