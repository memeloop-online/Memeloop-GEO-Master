use chrono::Utc;
use geo_domain::{
    ChannelPublicationStatus, CitationCoverage, CitationInsightPage, CitationSample,
    DistributionScope, DistributionScopeMode, ObservedSource, ObservedUrl, QuestionPurpose,
    RecommendationAudience, recommend_sources,
};
use uuid::Uuid;

fn sample(plan_id: Uuid, purpose: Option<QuestionPurpose>) -> CitationSample {
    CitationSample {
        plan_id,
        target_id: Uuid::new_v4(),
        attempt_id: Uuid::new_v4(),
        provider: "provider".into(),
        model: "model".into(),
        surface: "web".into(),
        search_mode: "search".into(),
        market: "generic".into(),
        language: "en".into(),
        question_set_version: "v1".into(),
        question_purpose: purpose,
        scheduled_at: Utc::now(),
        observed_at: Utc::now(),
        received_at: Utc::now(),
        analysis: None,
    }
}

fn page(urls: Vec<(&str, Vec<CitationSample>)>) -> CitationInsightPage {
    CitationInsightPage {
        scope: "returned_plans_only",
        plan_ids: urls
            .iter()
            .flat_map(|(_, samples)| samples.iter().map(|s| s.plan_id))
            .collect(),
        next_after: None,
        coverage: CitationCoverage::default(),
        invalid_citation_urls: 0,
        observed_sources: urls
            .into_iter()
            .map(|(url, samples)| {
                let host = url::Url::parse(url).unwrap().host_str().unwrap().to_owned();
                ObservedSource {
                    host,
                    citing_answers: samples.len(),
                    samples: samples.clone(),
                    urls: vec![ObservedUrl {
                        url: url.into(),
                        citing_answers: samples.len(),
                        samples,
                    }],
                }
            })
            .collect(),
    }
}

fn status(_: &str, _: &str) -> ChannelPublicationStatus {
    ChannelPublicationStatus {
        connector_availability: "unavailable".into(),
        account_ready: false,
        reason: Some("connector_unavailable".into()),
    }
}

#[test]
fn combines_mapped_hosts_without_double_counting_the_same_answer() {
    let first = sample(Uuid::new_v4(), Some(QuestionPurpose::Optimization));
    let observed = page(vec![
        ("https://www.zhihu.com/p/123", vec![first.clone()]),
        ("https://zhuanlan.zhihu.com/p/123", vec![first.clone()]),
    ]);
    let projection = recommend_sources(
        observed,
        &DistributionScope::default(),
        RecommendationAudience::Human,
        status,
    );
    assert_eq!(projection.items.len(), 1);
    assert_eq!(projection.items[0].citing_answers, 1);
    assert_eq!(projection.items[0].source_hosts.len(), 2);
    assert_eq!(projection.items[0].rule_ids.len(), 2);
    assert_eq!(projection.items[0].platform_id.as_deref(), Some("zhihu"));
}

#[test]
fn filters_all_frozen_and_unknown_derived_evidence_before_ai_projection() {
    let eligible = sample(Uuid::new_v4(), Some(QuestionPurpose::Optimization));
    let frozen = sample(Uuid::new_v4(), Some(QuestionPurpose::FrozenEvaluation));
    let unknown = sample(Uuid::new_v4(), None);
    let mut observed = page(vec![
        (
            "https://www.zhihu.com/p/123",
            vec![eligible.clone(), frozen.clone()],
        ),
        (
            "https://private-example.invalid/article",
            vec![unknown.clone()],
        ),
    ]);
    observed.next_after = Some(frozen.plan_id);
    let output = recommend_sources(
        observed,
        &DistributionScope::default(),
        RecommendationAudience::Optimization,
        status,
    );
    let json = serde_json::to_string(&output).unwrap();
    assert_eq!(output.items.len(), 1);
    assert_eq!(output.items[0].citing_answers, 1);
    assert!(output.items[0].samples.is_empty());
    assert!(!json.contains(&frozen.plan_id.to_string()));
    assert!(!json.contains(&unknown.plan_id.to_string()));
    assert!(!json.contains("private-example.invalid"));
    assert!(!json.contains("/p/123"));
    assert!(!json.contains("\"coverage\""));
}

#[test]
fn exact_host_paths_unmapped_sources_and_all_eligible_target_semantics() {
    let observed = page(vec![
        (
            "https://badzhihu.com/p/123",
            vec![sample(Uuid::new_v4(), None)],
        ),
        (
            "https://www.zhihu.com/question/123",
            vec![sample(Uuid::new_v4(), None)],
        ),
        (
            "https://medium.com/story",
            vec![sample(Uuid::new_v4(), None)],
        ),
    ]);
    let mut scope = DistributionScope::default();
    scope.excluded_platform_ids.push("medium".into());
    let projection = recommend_sources(observed, &scope, RecommendationAudience::Human, status);
    assert_eq!(projection.items.len(), 3);
    assert_eq!(
        projection
            .items
            .iter()
            .filter(|item| item.platform_id.is_none())
            .count(),
        2
    );
    assert!(
        !projection
            .items
            .iter()
            .find(|item| item.platform_id.as_deref() == Some("medium"))
            .unwrap()
            .targeted
    );
    scope.mode = DistributionScopeMode::Explicit;
    scope.included_platform_ids.push("medium".into());
    assert!(!geo_domain::targeted(&scope, "medium"));
}
