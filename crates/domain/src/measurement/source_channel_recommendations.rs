//! Versioned, conservative host-to-channel classification of accepted search citations.
//! A cited host is neither publication proof nor a claim of account readiness.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use url::Url;
use uuid::Uuid;

use crate::{
    CitationCoverage, CitationInsightPage, CitationSample, DistributionScope,
    DistributionScopeMode, QuestionPurpose,
};

pub const SOURCE_CHANNEL_RULE_VERSION: &str = "source-channel-rules.v1";
pub fn mapped_source_channel_keys() -> Vec<(&'static str, &'static str)> {
    RULES
        .iter()
        .map(|rule| (rule.platform_id, rule.placement_slot))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct SourceChannelRule {
    id: &'static str,
    host: &'static str,
    path_prefix: Option<&'static str>,
    platform_id: &'static str,
    placement_slot: &'static str,
    host_relationship: &'static str,
}

// Exact hosts only. A future subdomain rule must check DNS label boundaries.
// These are public URL structures, not external account or publishing observations.
const RULES: &[SourceChannelRule] = &[
    SourceChannelRule {
        id: "zhihu-article",
        host: "zhuanlan.zhihu.com",
        path_prefix: Some("/p/"),
        platform_id: "zhihu",
        placement_slot: "primary",
        host_relationship: "first_party_article",
    },
    SourceChannelRule {
        id: "zhihu-www-article",
        host: "www.zhihu.com",
        path_prefix: Some("/p/"),
        platform_id: "zhihu",
        placement_slot: "primary",
        host_relationship: "first_party_article",
    },
    SourceChannelRule {
        id: "baidu-creator-article",
        host: "baijiahao.baidu.com",
        path_prefix: Some("/s"),
        platform_id: "baidu_creator",
        placement_slot: "primary",
        host_relationship: "first_party_article",
    },
    SourceChannelRule {
        id: "xiaohongshu-post",
        host: "www.xiaohongshu.com",
        path_prefix: Some("/explore/"),
        platform_id: "xiaohongshu",
        placement_slot: "primary",
        host_relationship: "first_party_post",
    },
    SourceChannelRule {
        id: "x-post",
        host: "x.com",
        path_prefix: None,
        platform_id: "x",
        placement_slot: "primary",
        host_relationship: "first_party_post",
    },
    SourceChannelRule {
        id: "x-legacy-post",
        host: "twitter.com",
        path_prefix: None,
        platform_id: "x",
        placement_slot: "primary",
        host_relationship: "first_party_post",
    },
    // An observable source with no supported publishing connector is still shown.
    SourceChannelRule {
        id: "medium-story",
        host: "medium.com",
        path_prefix: None,
        platform_id: "medium",
        placement_slot: "primary",
        host_relationship: "first_party_story",
    },
];

fn rule_for_url(url: &str) -> Option<SourceChannelRule> {
    let parsed = Url::parse(url).ok()?;
    RULES
        .iter()
        .find(|rule| {
            parsed.host_str() == Some(rule.host)
                && rule
                    .path_prefix
                    .is_none_or(|prefix| parsed.path().starts_with(prefix))
        })
        .copied()
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelPublicationStatus {
    pub connector_availability: String,
    pub account_ready: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SourceChannelRecommendation {
    pub source_hosts: Vec<String>,
    pub platform_id: Option<String>,
    pub placement_slot: Option<String>,
    pub rule_ids: Vec<String>,
    pub host_relationships: Vec<String>,
    pub citing_answers: usize,
    pub samples: Vec<CitationSample>,
    pub publication: ChannelPublicationStatus,
    pub targeted: bool,
}

#[derive(Debug, Serialize)]
pub struct SourceChannelRecommendationPage {
    pub scope: &'static str,
    pub rule_version: &'static str,
    pub plan_ids: Vec<Uuid>,
    pub next_after: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CitationCoverage>,
    pub items: Vec<SourceChannelRecommendation>,
}

#[derive(Debug, Clone, Copy)]
pub enum RecommendationAudience {
    Human,
    Optimization,
}

pub fn targeted(scope: &DistributionScope, platform: &str) -> bool {
    match scope.mode {
        DistributionScopeMode::AllEligible => {
            !scope.excluded_platform_ids.iter().any(|id| id == platform)
        }
        DistributionScopeMode::Explicit => {
            scope.included_platform_ids.iter().any(|id| id == platform)
                && !scope.excluded_platform_ids.iter().any(|id| id == platform)
        }
    }
}

/// Builds both human and AI reads from the same authorized, validated citation
/// page. AI counts and even plan IDs are calculated only from explicitly
/// optimization-bound samples: filtering after aggregation alone would leak.
pub fn recommend_sources(
    page: CitationInsightPage,
    distribution: &DistributionScope,
    audience: RecommendationAudience,
    mut publication: impl FnMut(&str, &str) -> ChannelPublicationStatus,
) -> SourceChannelRecommendationPage {
    type Key = (String, String);
    type Group = (
        Option<SourceChannelRule>,
        BTreeSet<String>,
        BTreeSet<String>,
        BTreeMap<(Uuid, Uuid, Uuid), CitationSample>,
    );
    let mut groups: BTreeMap<Key, Group> = BTreeMap::new();
    let mut safe_plans = BTreeSet::new();
    for source in page.observed_sources {
        for url in source.urls {
            let rule = rule_for_url(&url.url);
            let key = rule.map_or_else(
                || ("unmapped".to_owned(), source.host.clone()),
                |rule| (rule.platform_id.to_owned(), rule.placement_slot.to_owned()),
            );
            let group = groups
                .entry(key)
                .or_insert_with(|| (rule, BTreeSet::new(), BTreeSet::new(), BTreeMap::new()));
            for sample in url.samples {
                if matches!(audience, RecommendationAudience::Optimization)
                    && sample.question_purpose != Some(QuestionPurpose::Optimization)
                {
                    continue;
                }
                safe_plans.insert(sample.plan_id);
                group.1.insert(source.host.clone());
                if let Some(rule) = rule {
                    group.2.insert(rule.id.to_owned());
                }
                group
                    .3
                    .entry((sample.plan_id, sample.target_id, sample.attempt_id))
                    .or_insert(sample);
            }
        }
    }
    let items = groups
        .into_values()
        .filter_map(|(rule, source_hosts, rule_ids, samples)| {
            if samples.is_empty() {
                return None;
            }
            let status = rule
                .map(|rule| publication(rule.platform_id, rule.placement_slot))
                .unwrap_or(ChannelPublicationStatus {
                    connector_availability: "unmapped".into(),
                    account_ready: false,
                    reason: Some("source_not_mapped_to_publishing_channel".into()),
                });
            Some(SourceChannelRecommendation {
                source_hosts: source_hosts.into_iter().collect(),
                platform_id: rule.map(|rule| rule.platform_id.to_owned()),
                placement_slot: rule.map(|rule| rule.placement_slot.to_owned()),
                rule_ids: rule_ids.into_iter().collect(),
                host_relationships: rule
                    .map_or_else(Vec::new, |rule| vec![rule.host_relationship.to_owned()]),
                citing_answers: samples.len(),
                samples: if matches!(audience, RecommendationAudience::Human) {
                    samples.into_values().collect()
                } else {
                    Vec::new()
                },
                publication: status,
                targeted: rule.is_some_and(|rule| targeted(distribution, rule.platform_id)),
            })
        })
        .collect();
    let safe_next_after = page.next_after.filter(|id| safe_plans.contains(id));
    SourceChannelRecommendationPage {
        scope: "returned_plans_only",
        rule_version: SOURCE_CHANNEL_RULE_VERSION,
        plan_ids: if matches!(audience, RecommendationAudience::Human) {
            page.plan_ids
        } else {
            safe_plans.into_iter().collect()
        },
        // A page cursor is a plan ID: do not leak an evaluation-only plan's
        // identity into the optimization input just to offer the next page.
        next_after: if matches!(audience, RecommendationAudience::Human) {
            page.next_after
        } else {
            safe_next_after
        },
        coverage: if matches!(audience, RecommendationAudience::Human) {
            Some(page.coverage)
        } else {
            None
        },
        items,
    }
}
