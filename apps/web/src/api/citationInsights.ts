import { apiFetch } from "./client";
import type { DistributionScope } from "./projects";
import type { SavedAnalysisProvenance } from "./observationAnalysis";

export interface CitationSample {
  analysis?: SavedAnalysisProvenance | null;
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
    grounded_saved_analysis?: number;
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

export interface SourceChannelRecommendation {
  source_hosts: string[];
  platform_id: string | null;
  placement_slot: string | null;
  rule_ids: string[];
  host_relationships: string[];
  citing_answers: number;
  samples: CitationSample[];
  publication: {
    connector_availability: string;
    account_ready: boolean;
    reason: string | null;
  };
  targeted: boolean;
}

export interface SourceChannelRecommendationsPage {
  scope: "returned_plans_only";
  plan_ids: string[];
  next_after: string | null;
  rule_version: string;
  coverage?: CitationInsightsPage["coverage"];
  items: SourceChannelRecommendation[];
}

export function getSourceChannelRecommendations(
  tenantId: string,
  projectId: string,
  after?: string,
) {
  const query = new URLSearchParams({ limit: "5" });
  if (after) query.set("after", after);
  return apiFetch<SourceChannelRecommendationsPage>(
    `/projects/${encodeURIComponent(projectId)}/source-channel-recommendations?${query}`,
    { tenantId, projectId },
  );
}

/** A target change must leave every unrelated distribution choice untouched. */
export function includeRecommendedPlatform(
  scope: DistributionScope,
  platformId: string,
): DistributionScope {
  if (scope.mode === "all_eligible") {
    return {
      ...scope,
      excluded_platform_ids: scope.excluded_platform_ids.filter(
        (id) => id !== platformId,
      ),
    };
  }
  return {
    ...scope,
    included_platform_ids: scope.included_platform_ids.includes(platformId)
      ? [...scope.included_platform_ids]
      : [...scope.included_platform_ids, platformId],
    excluded_platform_ids: scope.excluded_platform_ids.filter(
      (id) => id !== platformId,
    ),
  };
}
