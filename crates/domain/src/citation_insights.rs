//! Human-readable, project-scoped source observations from completed independent measurements.
//! This projection is NOT an input to content generation or the optimization projection:
//! frozen evaluation questions and their per-answer citations must never feed an optimizer.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use url::Url;
use uuid::Uuid;

use crate::{
    AppError, ChannelAttempt, ChannelOutcomeStatus, ChannelTargetInput, ChannelTargetView,
    QuestionPurpose, StandaloneMeasurementPlan, TenantScope,
};

#[derive(Debug, Default, Serialize)]
pub struct CitationCoverage {
    /// Every target in the returned page of plans, including unfinished and failed targets.
    pub planned: usize,
    pub pending: usize,
    pub observed_live: usize,
    pub observed_unverified: usize,
    pub observed_without_citations: usize,
    pub refused: usize,
    pub missing: usize,
    pub other_completed: usize,
    /// Overlaps the outcome counters above; a fixture never contributes a source.
    pub fixture: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CitationSample {
    pub plan_id: Uuid,
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    pub provider: String,
    pub model: String,
    pub surface: String,
    pub search_mode: String,
    pub market: String,
    pub language: String,
    pub question_set_version: String,
    pub question_purpose: Option<QuestionPurpose>,
    pub scheduled_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ObservedUrl {
    /// Original, complete URL as recorded by the trusted measurement adapter.
    pub url: String,
    /// Number of distinct completed answers citing this exact URL.
    pub citing_answers: usize,
    pub samples: Vec<CitationSample>,
}

#[derive(Debug, Serialize)]
pub struct ObservedSource {
    /// URL host, not an inferred registrable domain or publishing channel.
    pub host: String,
    /// Number of distinct completed answers citing any URL on this host.
    pub citing_answers: usize,
    pub samples: Vec<CitationSample>,
    pub urls: Vec<ObservedUrl>,
}

#[derive(Debug, Serialize)]
pub struct CitationInsightPage {
    /// Aggregates and denominators cover ONLY plans returned in this UUID keyset page.
    pub scope: &'static str,
    pub plan_ids: Vec<Uuid>,
    pub next_after: Option<Uuid>,
    pub coverage: CitationCoverage,
    /// Invalid URLs are not assigned an invented host or counted as an observed source.
    pub invalid_citation_urls: usize,
    pub observed_sources: Vec<ObservedSource>,
}

#[derive(Default)]
struct UrlAccumulator {
    samples: Vec<CitationSample>,
}

#[derive(Default)]
struct SourceAccumulator {
    samples: Vec<CitationSample>,
    urls: BTreeMap<String, UrlAccumulator>,
}

fn citation_host(raw: &str) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    parsed.host_str().map(str::to_owned)
}

/// `views` must contain exactly one scoped persisted target view per plan target.
/// No global totals or inferred publishing capability are produced.
pub fn summarize_citations(
    scope: &TenantScope,
    plans: &[(StandaloneMeasurementPlan, Vec<ChannelTargetView>)],
    next_after: Option<Uuid>,
    // Authoritative acceptance/provenance check provided by the API's
    // existing official-search validator, not a second protocol parser.
    is_trusted_search: impl Fn(&ChannelTargetView, &ChannelAttempt) -> bool,
) -> Result<CitationInsightPage, AppError> {
    let mut coverage = CitationCoverage::default();
    let mut hosts: BTreeMap<String, SourceAccumulator> = BTreeMap::new();
    let mut invalid_citation_urls = 0;
    let mut plan_ids = Vec::with_capacity(plans.len());
    for (plan, views) in plans {
        plan.validate(scope)?;
        if views.len() != plan.targets.len() {
            return Err(AppError::invalid_request(
                "incomplete measurement target page",
            ));
        }
        plan_ids.push(plan.plan_id);
        for (target, view) in plan.targets.iter().zip(views) {
            if target != &view.target {
                return Err(AppError::invalid_request("measurement target mismatch"));
            }
            coverage.planned += 1;
            let Some(attempt) = view.attempts.last() else {
                coverage.pending += 1;
                continue;
            };
            let Some(outcome) = &attempt.outcome else {
                coverage.pending += 1;
                continue;
            };
            if outcome.fixture {
                coverage.fixture += 1;
            }
            match outcome.status {
                ChannelOutcomeStatus::Observed => {
                    if outcome.fixture || !is_trusted_search(view, attempt) {
                        coverage.observed_unverified += 1;
                        continue;
                    }
                    coverage.observed_live += 1;
                    if outcome.citations.is_empty() {
                        coverage.observed_without_citations += 1;
                    }
                    let ChannelTargetInput::Measure {
                        provider,
                        model,
                        surface,
                        search_mode,
                        market,
                        language,
                        question_set_version,
                        question_binding,
                        scheduled_at,
                        ..
                    } = &target.input
                    else {
                        unreachable!("plan validated")
                    };
                    let sample = CitationSample {
                        plan_id: plan.plan_id,
                        target_id: target.target_id,
                        attempt_id: attempt.attempt_id,
                        provider: provider.clone(),
                        model: model.clone(),
                        surface: surface.clone(),
                        search_mode: search_mode.clone(),
                        market: market.clone(),
                        language: language.clone(),
                        question_set_version: question_set_version.clone(),
                        question_purpose: question_binding.as_ref().map(|binding| binding.purpose),
                        scheduled_at: *scheduled_at,
                        observed_at: outcome.occurred_at,
                        received_at: attempt.received_at.expect("verified search has receipt"),
                    };
                    let mut seen_hosts = HashSet::new();
                    let mut seen_urls = HashSet::new();
                    for url in &outcome.citations {
                        if !seen_urls.insert(url.as_str()) {
                            continue;
                        }
                        let Some(host) = citation_host(url) else {
                            invalid_citation_urls += 1;
                            continue;
                        };
                        let source = hosts.entry(host.clone()).or_default();
                        if seen_hosts.insert(host) {
                            source.samples.push(sample.clone());
                        }
                        source
                            .urls
                            .entry(url.clone())
                            .or_default()
                            .samples
                            .push(sample.clone());
                    }
                }
                ChannelOutcomeStatus::Refused => coverage.refused += 1,
                ChannelOutcomeStatus::Missing => coverage.missing += 1,
                _ => coverage.other_completed += 1,
            }
        }
    }
    let observed_sources = hosts
        .into_iter()
        .map(|(host, source)| ObservedSource {
            host,
            citing_answers: source.samples.len(),
            samples: source.samples,
            urls: source
                .urls
                .into_iter()
                .map(|(url, observed)| ObservedUrl {
                    url,
                    citing_answers: observed.samples.len(),
                    samples: observed.samples,
                })
                .collect(),
        })
        .collect();
    Ok(CitationInsightPage {
        scope: "returned_plans_only",
        plan_ids,
        next_after,
        coverage,
        invalid_citation_urls,
        observed_sources,
    })
}
