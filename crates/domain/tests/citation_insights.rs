use chrono::{Duration, Utc};
use geo_domain::{
    ChannelAttempt, ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ChannelTargetInput,
    ChannelTargetView, OperatorId, ProjectId, StandaloneMeasurementPlan, TenantId, TenantScope,
    sha256_hex, summarize_citations,
};
use serde_json::{Value, json};
use uuid::Uuid;

fn scope() -> TenantScope {
    TenantScope::new(
        OperatorId::new(Uuid::new_v4()),
        TenantId::new(Uuid::new_v4()),
        Some(ProjectId::new(Uuid::new_v4())),
    )
}

// A small caller policy stub: tests here cover domain aggregation, while the
// API calls the real official-search acceptance validator.
fn trusted(view: &ChannelTargetView, attempt: &ChannelAttempt) -> bool {
    let ChannelTargetInput::Measure { provider, .. } = &view.target.input else {
        return false;
    };
    let Some(outcome) = &attempt.outcome else {
        return false;
    };
    outcome.runner_evidence.first().is_some_and(|proof| {
        proof["provider"].as_str() == Some(provider.as_str())
            && proof.get("citations") == serde_json::to_value(&outcome.citations).ok().as_ref()
    })
}

fn target(now: chrono::DateTime<Utc>) -> ChannelTarget {
    ChannelTarget {
        target_id: Uuid::new_v4(),
        input: ChannelTargetInput::Measure {
            account_id: Uuid::new_v4(),
            provider: "provider-a".into(),
            model: "model-a".into(),
            surface: "consumer_web".into(),
            search_mode: "web_search".into(),
            protocol_version: "v1".into(),
            question_set_version: "ad_hoc.v1".into(),
            question: "How does rainfall measurement work?".into(),
            market: "test".into(),
            language: "en".into(),
            scheduled_at: now - Duration::minutes(3),
            sample_ordinal: 0,
            question_binding: None,
        },
    }
}

fn completed(
    target: ChannelTarget,
    at: chrono::DateTime<Utc>,
    status: ChannelOutcomeStatus,
    citations: &[&str],
) -> ChannelTargetView {
    let ChannelTargetInput::Measure {
        account_id,
        provider,
        model,
        surface,
        search_mode,
        protocol_version,
        question_set_version,
        question,
        market,
        language,
        scheduled_at,
        sample_ordinal,
        ..
    } = &target.input
    else {
        unreachable!()
    };
    let attempt_id = Uuid::new_v4();
    let claimed_at = at - Duration::minutes(2);
    let observed_at = at - Duration::minutes(1);
    let citation_values: Vec<String> = citations.iter().map(|url| (*url).into()).collect();
    let proof = json!({
        "kind":"official_search_observation",
        "schema_version":"geo.measure.official_search.v1",
        "target_id":target.target_id,
        "account_id":account_id,
        "provider":provider,
        "model":model,
        "surface":surface,
        "search_mode":search_mode,
        "protocol_version":protocol_version,
        "question_set_version":question_set_version,
        "question_sha256":sha256_hex(question.as_bytes()),
        "market":market,
        "language":language,
        "scheduled_at":scheduled_at,
        "sample_ordinal":sample_ordinal,
        "connector_version":"connector-v1",
        "provenance":"live",
        "disposition":"observed",
        "raw_answer":"Answer.",
        "citations":citation_values,
        "search_event":{
            "kind":"official_search_event",
            "source":"provider_search_event",
            "provenance":"live",
            "occurred_at":observed_at,
            "event_id":"event-1",
            "request_id":"request-1"
        }
    });
    let receipt = json!({
        "kind":"runner_receipt",
        "schema_version":"geo.runner.receipt.v1",
        "provenance":"live",
        "connector_version":"connector-v1",
        "occurred_at":at
    });
    ChannelTargetView {
        target: target.clone(),
        attempts: vec![ChannelAttempt {
            attempt_id,
            target_id: target.target_id,
            claimed_at,
            received_at: Some(at),
            outcome: Some(ChannelOutcome {
                status,
                detail: None,
                occurred_at: at,
                raw_answer: Some("Answer.".into()),
                citations: citation_values,
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("connector-v1".into()),
                runner_evidence: vec![proof, receipt],
                fixture: false,
            }),
        }],
    }
}

fn plan(scope: &TenantScope, targets: &[ChannelTarget]) -> StandaloneMeasurementPlan {
    StandaloneMeasurementPlan {
        plan_id: Uuid::new_v4(),
        project_id: scope.project_id.unwrap(),
        title: "Topic".into(),
        input_hash: "frozen".into(),
        revision: 1,
        created_at: Utc::now(),
        targets: targets.to_vec(),
    }
}

#[test]
fn observed_hosts_urls_deduplicate_per_answer_and_retain_evidence_refs() {
    let scope = scope();
    let now = Utc::now();
    let first = target(now);
    let second = target(now);
    let first_view = completed(
        first.clone(),
        now,
        ChannelOutcomeStatus::Observed,
        &[
            "https://example.org/a",
            "https://example.org/a",
            "https://example.org/b",
            "https://news.example.org/b",
        ],
    );
    let second_view = completed(
        second.clone(),
        now,
        ChannelOutcomeStatus::Observed,
        &["https://example.org/a"],
    );
    let input = vec![(
        plan(&scope, &[first, second]),
        vec![first_view.clone(), second_view],
    )];
    let page = summarize_citations(&scope, &input, Some(Uuid::new_v4()), trusted).unwrap();
    assert_eq!(page.scope, "returned_plans_only");
    assert_eq!(page.coverage.planned, 2);
    assert_eq!(page.coverage.observed_live, 2);
    assert_eq!(page.observed_sources.len(), 2);
    let host = page
        .observed_sources
        .iter()
        .find(|item| item.host == "example.org")
        .unwrap();
    assert_eq!(host.citing_answers, 2);
    assert_eq!(host.urls.len(), 2);
    assert_eq!(host.urls[0].citing_answers, 2);
    assert_eq!(host.urls[1].citing_answers, 1);
    assert_eq!(host.urls[0].url, "https://example.org/a");
    assert_eq!(
        host.samples[0].attempt_id,
        first_view.attempts[0].attempt_id
    );
    assert_eq!(host.samples[0].provider, "provider-a");
    assert_eq!(host.samples[0].model, "model-a");
    assert_eq!(page.observed_sources[1].host, "news.example.org");
    assert_eq!(page.observed_sources[1].citing_answers, 1);
    assert!(page.next_after.is_some());
}

#[test]
fn zero_citations_refusals_missing_pending_fixture_and_bad_proof_remain_distinct() {
    let scope = scope();
    let now = Utc::now();
    let first = target(now);
    let second = target(now);
    let third = target(now);
    let fourth = target(now);
    let fifth = target(now);
    let sixth = target(now);
    let first_view = completed(first.clone(), now, ChannelOutcomeStatus::Observed, &[]);
    let refused = completed(second.clone(), now, ChannelOutcomeStatus::Refused, &[]);
    let missing = completed(third.clone(), now, ChannelOutcomeStatus::Missing, &[]);
    let pending = ChannelTargetView {
        target: fourth.clone(),
        attempts: vec![],
    };
    let mut fixture = completed(
        fifth.clone(),
        now,
        ChannelOutcomeStatus::Observed,
        &["https://example.org/fixture"],
    );
    fixture.attempts[0].outcome.as_mut().unwrap().fixture = true;
    let mut unverified = completed(
        sixth.clone(),
        now,
        ChannelOutcomeStatus::Observed,
        &["https://example.org/fake"],
    );
    unverified.attempts[0]
        .outcome
        .as_mut()
        .unwrap()
        .runner_evidence[0]["provider"] = json!("altered");
    let input = vec![(
        plan(&scope, &[first, second, third, fourth, fifth, sixth]),
        vec![first_view, refused, missing, pending, fixture, unverified],
    )];
    let page = summarize_citations(&scope, &input, None, trusted).unwrap();
    assert_eq!(page.coverage.planned, 6);
    assert_eq!(page.coverage.observed_live, 1);
    assert_eq!(page.coverage.observed_without_citations, 1);
    assert_eq!(page.coverage.observed_unverified, 2);
    assert_eq!(page.coverage.refused, 1);
    assert_eq!(page.coverage.missing, 1);
    assert_eq!(page.coverage.pending, 1);
    assert_eq!(page.coverage.fixture, 1);
    assert!(page.observed_sources.is_empty());
}

#[test]
fn rejects_cross_project_plan_even_when_views_are_present() {
    let original_scope = scope();
    let other_scope = scope();
    let now = Utc::now();
    let target = target(now);
    let input = vec![(
        plan(&original_scope, std::slice::from_ref(&target)),
        vec![completed(target, now, ChannelOutcomeStatus::Observed, &[])],
    )];
    assert!(summarize_citations(&other_scope, &input, None, trusted).is_err());
}

#[test]
fn rejects_hostless_urls_without_fabricating_a_publishing_channel() {
    let scope = scope();
    let now = Utc::now();
    let target = target(now);
    let mut view = completed(
        target.clone(),
        now,
        ChannelOutcomeStatus::Observed,
        &["javascript:alert(1)", "https://example.org/article"],
    );
    // A genuine adapter prevents this; historical malformed storage must not
    // make the source aggregation claim an imaginary host.
    let page = summarize_citations(
        &scope,
        &[(plan(&scope, &[target]), vec![view.clone()])],
        None,
        trusted,
    )
    .unwrap();
    assert_eq!(page.invalid_citation_urls, 1);
    assert_eq!(page.observed_sources.len(), 1);
    view.attempts[0].outcome.as_mut().unwrap().runner_evidence[0]["citations"] = Value::Null;
    let rejected = summarize_citations(
        &scope,
        &[(plan(&scope, &[view.target.clone()]), vec![view])],
        None,
        trusted,
    )
    .unwrap();
    assert_eq!(rejected.coverage.observed_unverified, 1);
}
