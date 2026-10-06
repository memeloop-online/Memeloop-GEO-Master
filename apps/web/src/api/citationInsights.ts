import { apiFetch } from "./client";

export interface CitationSample {
  plan_id: string;
  target_id: string;
  attempt_id: string;
  provider: string;
  model: string;
  surface: string;
  search_mode: string;
  market: string;
  language: string;
  question_set_version: string;
  question_purpose: "optimization" | "frozen_evaluation" | null;
  scheduled_at: string;
  observed_at: string;
  received_at: string;
}

export interface ObservedCitationUrl {
  url: string;
  citing_answers: number;
  samples: CitationSample[];
}

export interface ObservedCitationSource {
  host: string;
  citing_answers: number;
  samples: CitationSample[];
  urls: ObservedCitationUrl[];
}

export interface CitationInsightsPage {
  scope: "returned_plans_only";
  plan_ids: string[];
  next_after: string | null;
  coverage: {
    planned: number;
    pending: number;
    observed_live: number;
    observed_unverified: number;
    observed_without_citations: number;
    refused: number;
    missing: number;
    other_completed: number;
    fixture: number;
  };
  invalid_citation_urls: number;
  observed_sources: ObservedCitationSource[];
}

export function getCitationInsights(
  tenantId: string,
  projectId: string,
  after?: string,
) {
  const query = new URLSearchParams({ limit: "5" });
  if (after) query.set("after", after);
  return apiFetch<CitationInsightsPage>(
    `/projects/${encodeURIComponent(projectId)}/citation-insights?${query}`,
    { tenantId, projectId },
  );
}
