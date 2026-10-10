use chrono::{DateTime, Duration};
use geo_domain::*;
use uuid::Uuid;

fn fixture() -> (
    TenantScope,
    MeasurementPeriodWindow,
    MeasurementPeriodSearchIdentity,
    SerpReportObservation,
) {
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let at = now - Duration::hours(1);
    let scope = TenantScope::new(
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Some(Uuid::new_v4().into()),
    );
    let window = MeasurementPeriodWindow {
        start_at: now - Duration::days(7),
        end_at: now,
        report_timezone: "UTC".into(),
    };
    let measurement = SerpMeasurement {
        measurement_id: Uuid::new_v4(),
        source_key: "synthetic-primary".into(),
        protocol: SerpProtocol {
            query: "  synthetic query + exact%  ".into(),
            engine: SerpEngine::Google,
            surface: SerpSurface::ThirdPartyApi,
            source: "synthetic".into(),
            source_location_code: "2840".into(),
            country: "US".into(),
            city: None,
            language: "en".into(),
            device: SerpDevice::Desktop,
            operating_system: "windows".into(),
            requested_depth: 10,
            max_pages: 1,
            priority: 1,
            login: "unspecified".into(),
            personalization: "unspecified".into(),
            protocol_version: SERP_PROTOCOL_VERSION.into(),
            connector_version: "synthetic.v1".into(),
        },
        target: Some(SerpTarget::Host {
            host: "absent.example.org".into(),
            include_subdomains: false,
        }),
        target_rule_version: SERP_TARGET_RULE_VERSION.into(),
        question_binding: None,
        scheduled_at: at,
        created_at: at,
        state: SerpTaskState::Completed,
    };
    let observation = SerpObservation {
        observation_id: Uuid::new_v4(),
        measurement_id: measurement.measurement_id,
        attempt_id: Uuid::new_v4(),
        raw_evidence_id: Uuid::new_v4(),
        raw_sha256: "a".repeat(64),
        parser_version: "synthetic.v1".into(),
        provider_observed_at: None,
        received_at: at,
        analyzed_at: at,
        status: SerpObservationStatus::Observed,
        actual_conditions: SerpActualConditions::default(),
        coverage: SerpCoverage {
            requested_depth: 10,
            observed_organic_depth: 1,
            pages_received: 1,
            completion: SerpCoverageCompletion::ProviderExhausted,
            truncated: false,
            exhaustion_evidence_locator: Some("/exhausted".into()),
        },
        results: vec![SerpResult {
            kind: SerpResultKind::Organic,
            raw_kind: "organic".into(),
            raw_url: Some("https://example.org/".into()),
            normalized_url: Some("https://example.org/".into()),
            host: Some("example.org".into()),
            normalization_version: SERP_URL_RULE_VERSION.into(),
            title: Some("Synthetic result".into()),
            page: Some(1),
            position: 1,
            organic_rank: Some(1),
            absolute_position: Some(2),
            locator: "/results/0".into(),
        }],
        source_limitations: vec![SerpLimitationCode::RequestedConditionsUnverified],
    };
    observation.validate(&measurement).unwrap();
    let raw = SerpReportRawMetadata {
        evidence_id: observation.raw_evidence_id,
        measurement_id: observation.measurement_id,
        attempt_id: observation.attempt_id,
        operation: SerpEvidenceOperation::ResultRead,
        response_sha256: observation.raw_sha256.clone(),
        body_complete: true,
        captured_at: at,
        stored_at: at,
    };
    (
        scope,
        window,
        MeasurementPeriodSearchIdentity::from_measurement(&measurement, at),
        SerpReportObservation {
            observation,
            observation_stored_at: at,
            raw,
        },
    )
}

#[test]
fn receipt_time_fallback_is_explicit_and_ai_denominator_is_independent() {
    let (scope, window, cohort, candidate) = fixture();
    let sample =
        build_measurement_period_search_sample(cohort, vec![candidate], &window, window.end_at)
            .unwrap();
    let evidence = sample.evidence.as_ref().unwrap();
    assert_eq!(
        evidence.evidence_time_basis,
        SearchEvidenceTimeBasis::ReceivedAt
    );
    assert_eq!(evidence.observation.provider_observed_at, None);
    assert_eq!(
        evidence.target_match,
        SerpTargetMatch::NotFoundWithinDepth { covered_depth: 1 }
    );
    let preview = preview_measurement_period_with_search(
        &scope,
        &window,
        vec![],
        Some(vec![sample]),
        window.end_at,
    )
    .unwrap();
    assert_eq!(preview.coverage.planned, 0);
    let search = preview.search.unwrap();
    assert_eq!(search.coverage.planned, 1);
    assert_eq!(
        search.coverage.counts[&MeasurementPeriodSearchStatus::Observed],
        1
    );
}

#[test]
fn application_and_database_clock_skew_does_not_permanently_exclude_saved_evidence() {
    let (scope, window, original_cohort, original) = fixture();
    for skew_seconds in [-20, 20] {
        let mut cohort = original_cohort.clone();
        let mut candidate = original.clone();
        cohort.stored_at += Duration::seconds(skew_seconds);
        candidate.raw.stored_at += Duration::seconds(skew_seconds);
        candidate.observation_stored_at += Duration::seconds(skew_seconds + 1);
        let sample = build_measurement_period_search_sample(
            cohort.clone(),
            vec![candidate.clone()],
            &window,
            window.end_at,
        )
        .unwrap();
        let evidence = sample.evidence.as_ref().unwrap();
        // Preserve every recorded clock value; do not normalize with max/min.
        assert_eq!(sample.cohort, cohort);
        assert_eq!(evidence.observation, original.observation);
        assert_eq!(evidence.raw_stored_at, candidate.raw.stored_at);
        assert_eq!(
            evidence.observation_stored_at,
            candidate.observation_stored_at
        );
        let preview = preview_measurement_period_with_search(
            &scope,
            &window,
            vec![],
            Some(vec![sample.clone()]),
            window.end_at,
        )
        .unwrap();
        assert_eq!(preview.search.unwrap().samples, vec![sample]);
    }
}

#[test]
fn each_event_and_storage_timestamp_must_independently_pass_fixed_cutoff() {
    let (scope, window, cohort, original) = fixture();
    let cutoff = window.end_at;
    let late = cutoff + Duration::microseconds(1);
    for field in 0..5 {
        let mut candidate = original.clone();
        match field {
            0 => candidate.observation.provider_observed_at = Some(late),
            1 => {
                // Keep receipt identity bound even when its event clock is late.
                candidate.observation.provider_observed_at = Some(original.observation.received_at);
                candidate.observation.received_at = late;
                candidate.raw.captured_at = late;
            }
            2 => candidate.observation.analyzed_at = late,
            3 => candidate.raw.stored_at = late,
            4 => candidate.observation_stored_at = late,
            _ => unreachable!(),
        }
        assert!(
            build_measurement_period_search_sample(
                cohort.clone(),
                vec![candidate],
                &window,
                cutoff,
            )
            .unwrap()
            .evidence
            .is_none(),
            "late candidate timestamp {field}"
        );
    }
    let sample =
        build_measurement_period_search_sample(cohort.clone(), vec![original], &window, cutoff)
            .unwrap();
    for field in 0..7 {
        let mut late_sample = sample.clone();
        let evidence = late_sample.evidence.as_mut().unwrap();
        match field {
            0 => evidence.observation.provider_observed_at = Some(late),
            1 => evidence.observation.received_at = late,
            2 => evidence.observation.analyzed_at = late,
            3 => evidence.raw_stored_at = late,
            4 => evidence.observation_stored_at = late,
            5 => late_sample.cohort.created_at = late,
            6 => late_sample.cohort.stored_at = late,
            _ => unreachable!(),
        }
        assert!(
            preview_measurement_period_with_search(
                &scope,
                &window,
                vec![],
                Some(vec![late_sample]),
                cutoff,
            )
            .is_err(),
            "late snapshot timestamp {field}"
        );
    }
    for late_created in [true, false] {
        let mut late_cohort = cohort.clone();
        if late_created {
            late_cohort.created_at = late;
        } else {
            late_cohort.stored_at = late;
        }
        assert!(
            build_measurement_period_search_sample(late_cohort, vec![], &window, cutoff).is_err()
        );
    }
}

#[test]
fn every_storage_boundary_and_raw_binding_is_enforced() {
    let (_, window, cohort, valid) = fixture();
    let cutoff = window.end_at;
    let mut variants = Vec::new();
    let mut value = valid.clone();
    value.observation_stored_at = cutoff + Duration::microseconds(1);
    variants.push(value);
    let mut value = valid.clone();
    value.raw.stored_at = cutoff + Duration::microseconds(1);
    variants.push(value);
    let mut value = valid.clone();
    value.observation.analyzed_at = cutoff + Duration::microseconds(1);
    variants.push(value);
    let mut value = valid.clone();
    value.raw.response_sha256 = "b".repeat(64);
    variants.push(value);
    let mut value = valid.clone();
    value.raw.attempt_id = Uuid::new_v4();
    variants.push(value);
    let mut value = valid.clone();
    value.raw.body_complete = false;
    variants.push(value);
    let mut value = valid.clone();
    value.raw.operation = SerpEvidenceOperation::Submission;
    variants.push(value);
    for candidate in variants {
        assert!(
            build_measurement_period_search_sample(
                cohort.clone(),
                vec![candidate],
                &window,
                cutoff,
            )
            .unwrap()
            .evidence
            .is_none()
        );
    }
    let mut inclusive = valid;
    inclusive.observation.analyzed_at = cutoff;
    inclusive.observation_stored_at = cutoff;
    assert!(
        build_measurement_period_search_sample(cohort.clone(), vec![inclusive], &window, cutoff,)
            .unwrap()
            .evidence
            .is_some()
    );
    let mut late_cohort = cohort;
    late_cohort.stored_at = cutoff + Duration::microseconds(1);
    assert!(build_measurement_period_search_sample(late_cohort, vec![], &window, cutoff,).is_err());
}

#[test]
fn provider_time_is_not_replaced_by_receipt_to_fit_window() {
    let (_, window, cohort, mut candidate) = fixture();
    candidate.observation.provider_observed_at = Some(window.start_at - Duration::seconds(1));
    assert!(
        build_measurement_period_search_sample(
            cohort.clone(),
            vec![candidate.clone()],
            &window,
            window.end_at,
        )
        .unwrap()
        .evidence
        .is_none()
    );
    candidate.observation.provider_observed_at = Some(window.start_at);
    let evidence =
        build_measurement_period_search_sample(cohort, vec![candidate], &window, window.end_at)
            .unwrap()
            .evidence
            .unwrap();
    assert_eq!(evidence.evidence_time, window.start_at);
    assert_eq!(
        evidence.evidence_time_basis,
        SearchEvidenceTimeBasis::ProviderObservedAt
    );
}

#[test]
fn later_failed_parse_never_hides_useful_evidence_and_partial_is_not_not_found() {
    let (_, window, cohort, valid) = fixture();
    let mut failed = valid.clone();
    failed.observation.observation_id = Uuid::new_v4();
    failed.observation.analyzed_at += Duration::seconds(1);
    failed.observation_stored_at = failed.observation.analyzed_at;
    failed.observation.status = SerpObservationStatus::Failed;
    failed.observation.results.clear();
    failed.observation.coverage.observed_organic_depth = 0;
    failed.observation.coverage.completion = SerpCoverageCompletion::Partial;
    failed.observation.coverage.truncated = true;
    let sample = build_measurement_period_search_sample(
        cohort.clone(),
        vec![valid.clone(), failed.clone()],
        &window,
        window.end_at,
    )
    .unwrap();
    assert_eq!(
        sample.evidence.unwrap().observation.observation_id,
        valid.observation.observation_id
    );
    failed.observation.status = SerpObservationStatus::Partial;
    // A newer successful partial interpretation can correct earlier completeness.
    // Only unsuccessful interpretations cannot hide useful evidence.
    let sample =
        build_measurement_period_search_sample(cohort, vec![valid, failed], &window, window.end_at)
            .unwrap();
    assert_eq!(
        sample.evidence.unwrap().target_match,
        SerpTargetMatch::Undetermined
    );
}

#[test]
fn legacy_snapshot_none_is_not_successful_empty_search_and_correction_cannot_add_it() {
    let (scope, window, _, _) = fixture();
    let preview = preview_measurement_period(&scope, &window, vec![], window.end_at).unwrap();
    let first = freeze_measurement_period(&scope, preview, 1, None).unwrap();
    let json = serde_json::to_value(&first).unwrap();
    assert!(json.get("search").is_none());
    assert!(
        serde_json::from_value::<MeasurementPeriodReport>(json)
            .unwrap()
            .search
            .is_none()
    );
    let added = preview_measurement_period_with_search(
        &scope,
        &window,
        vec![],
        Some(vec![]),
        window.end_at,
    )
    .unwrap();
    assert_ne!(first.input_hash, added.input_hash);
    let correction = freeze_measurement_period(&scope, added, 2, Some(first.report_id)).unwrap();
    assert!(validate_measurement_period_correction(&[first], &correction).is_err());
}

#[test]
fn correction_preserves_cohort_and_source_protocol_are_not_collapsed() {
    let (scope, window, cohort, _) = fixture();
    let mut other = cohort.clone();
    other.measurement_id = Uuid::new_v4();
    other.source_key = "synthetic-secondary".into();
    let samples = vec![
        MeasurementPeriodSearchSample {
            cohort: cohort.clone(),
            evidence: None,
        },
        MeasurementPeriodSearchSample {
            cohort: other.clone(),
            evidence: None,
        },
    ];
    let first_preview = preview_measurement_period_with_search(
        &scope,
        &window,
        vec![],
        Some(samples.clone()),
        window.end_at,
    )
    .unwrap();
    assert_eq!(first_preview.search.as_ref().unwrap().coverage.planned, 2);
    assert_eq!(
        first_preview.search.as_ref().unwrap().coverage.counts
            [&MeasurementPeriodSearchStatus::NoEligibleObservation],
        2
    );
    let mut reversed = samples.clone();
    reversed.reverse();
    let reordered = preview_measurement_period_with_search(
        &scope,
        &window,
        vec![],
        Some(reversed),
        window.end_at,
    )
    .unwrap();
    assert_eq!(first_preview.input_hash, reordered.input_hash);
    let first = freeze_measurement_period(&scope, first_preview, 1, None).unwrap();
    let unchanged = freeze_measurement_period(&scope, reordered, 2, Some(first.report_id)).unwrap();
    validate_measurement_period_correction(std::slice::from_ref(&first), &unchanged).unwrap();
    let mut changed = unchanged;
    changed.search.as_mut().unwrap().samples[0]
        .cohort
        .protocol
        .connector_version = "synthetic.v2".into();
    assert!(
        validate_measurement_period_correction(std::slice::from_ref(&first), &changed).is_err()
    );
    let duplicate = vec![samples[0].clone(), samples[0].clone()];
    assert!(
        preview_measurement_period_with_search(
            &scope,
            &window,
            vec![],
            Some(duplicate),
            window.end_at,
        )
        .is_err()
    );
}
