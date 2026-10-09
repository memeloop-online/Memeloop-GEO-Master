import { apiFetch } from "./client";

export type AnalysisSource =
  | { kind: "capture"; capture_id: string }
  | { kind: "attempt_evidence"; evidence_index: number };

export interface ObservationAnalysisRevision {
  request: {
    revision_id: string;
    target_id: string;
    attempt_id: string;
    source: AnalysisSource;
    source_sha256: string;
    observed_at: string;
    prompt_version: string;
    parser_version: string;
  };
  request_digest: string;
  state: "queued" | "running" | "completed";
  created_at: string;
  started_at: string | null;
  analyzed_at: string | null;
  result: {
    config_revision?: number | null;
    actual_model: string | null;
    candidate_json: string | null;
    outcome:
      | {
          status: "grounded";
          raw_answer: string;
          citations: string[];
          audit: Record<string, unknown>;
        }
      | { status: "unverified"; reason: string }
      | { status: "failed"; code: string };
    prompt_tokens: number;
    completion_tokens: number;
  } | null;
}

export interface ObservationAnalysisPage {
  items: ObservationAnalysisRevision[];
  next_after: string | null;
  sources: {
    source: AnalysisSource;
    source_sha256: string;
    observed_at: string;
  }[];
}

function path(projectId: string, targetId: string, attemptId: string) {
  return `/projects/${encodeURIComponent(projectId)}/channel-targets/${encodeURIComponent(targetId)}/attempts/${encodeURIComponent(attemptId)}/analyses`;
}

export function listObservationAnalyses(
  tenantId: string,
  projectId: string,
  targetId: string,
  attemptId: string,
  after?: string,
  signal?: AbortSignal,
) {
  return apiFetch<ObservationAnalysisPage>(
    `${path(projectId, targetId, attemptId)}?limit=20${after ? `&after=${encodeURIComponent(after)}` : ""}`,
    { tenantId, projectId, signal },
  );
}

export function createObservationAnalysis(
  tenantId: string,
  projectId: string,
  targetId: string,
  attemptId: string,
  idempotencyKey: string,
) {
  return apiFetch<ObservationAnalysisRevision>(
    path(projectId, targetId, attemptId),
    {
      tenantId,
      projectId,
      method: "POST",
      idempotencyKey,
      body: { idempotency_key: idempotencyKey },
    },
  );
}
