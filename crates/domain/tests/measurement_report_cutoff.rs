use chrono::{DateTime, Duration};
use geo_domain::*;
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn fixture() -> (
    TenantScope,
    MeasurementPeriodWindow,
    MeasurementPeriodSample,
) {
    let cutoff = DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap();
    let observed_at = cutoff - Duration::hours(1);
    (
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        ),
        MeasurementPeriodWindow {
            start_at: cutoff - Duration::days(7),
            end_at: cutoff,
            report_timezone: "UTC".into(),
        },
        MeasurementPeriodSample {
            plan_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Some(Uuid::new_v4()),
            comparison_key: "synthetic-protocol".into(),
            question_binding: None,
            scheduled_at: observed_at,
            original_status: "unknown".into(),
            observed_live: false,
            observation: Some(EffectiveObservation {
                raw_answer: "Synthetic saved answer".into(),
                citations: vec![],
                observed_at,
                received_at: cutoff,
                provenance: Some(SavedAnalysisProvenance {
                    revision_id: Uuid::new_v4(),
                    source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
                    source_sha256: "a".repeat(64),
                    observed_at,
                    analyzed_at: cutoff,
                    actual_model: "synthetic-parser".into(),
                    config_revision: None,
                    prompt_version: "v1".into(),
                    parser_version: "v1".into(),
                }),
            }),
        },
    )
}

#[test]
fn delayed_generation_uses_cutoff_without_mutating_evidence_or_frozen_reports() {
    let (scope, window, sample) = fixture();
    let cutoff = window.end_at;
    let generated_at = cutoff + Duration::hours(2);
    let preview = preview_measurement_period_at_cutoff(
        &scope,
        &window,
        vec![sample.clone()],
        None,
        cutoff,
        generated_at,
    )
    .unwrap();
    assert_eq!(preview.samples, vec![sample.clone()]);
    assert_eq!(preview.evidence_as_of, cutoff);
    assert_eq!(preview.generated_at, generated_at);
    let first = freeze_measurement_period(&scope, preview, 1, None).unwrap();
    let frozen_bytes = serde_json::to_vec(&first).unwrap();
    for late_receipt in [true, false] {
        let mut late = sample.clone();
        let observation = late.observation.as_mut().unwrap();
        if late_receipt {
            observation.received_at = cutoff + Duration::nanoseconds(1);
        } else {
            observation.provenance.as_mut().unwrap().analyzed_at =
                cutoff + Duration::nanoseconds(1);
        }
        let late_bytes = serde_json::to_vec(&late).unwrap();
        let projection = preview_measurement_period_at_cutoff(
            &scope,
            &window,
            vec![late.clone()],
            None,
            cutoff,
            generated_at,
        )
        .unwrap();
        assert!(projection.samples[0].observation.is_none());
        assert_eq!(projection.samples[0].original_status, "unknown");
        assert_eq!(projection.coverage.planned, 1);
        assert_eq!(projection.coverage.grounded_saved_analysis, 0);
        assert_eq!(serde_json::to_vec(&late).unwrap(), late_bytes);
    }
    assert_eq!(serde_json::to_vec(&first).unwrap(), frozen_bytes);
}

#[test]
fn preview_entry_points_use_current_hash_tuple_and_stable_report_id() {
    let (scope, window, sample) = fixture();
    let cutoff = window.end_at;
    let ai_only =
        preview_measurement_period(&scope, &window, vec![sample.clone()], cutoff).unwrap();
    for search in [None, Some(vec![])] {
        let wrapped = preview_measurement_period_with_search(
            &scope,
            &window,
            vec![sample.clone()],
            search.clone(),
            cutoff,
        )
        .unwrap();
        let explicit = preview_measurement_period_at_cutoff(
            &scope,
            &window,
            vec![sample.clone()],
            search.clone(),
            cutoff,
            cutoff,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&wrapped).unwrap(),
            serde_json::to_vec(&explicit).unwrap()
        );
        assert_eq!(explicit.search.is_some(), search.is_some());
        if explicit.search.is_none() {
            assert_eq!(ai_only, explicit);
            assert!(
                serde_json::to_value(&explicit)
                    .unwrap()
                    .get("search")
                    .is_none()
            );
        }
        let tuple_bytes =
            serde_json::to_vec(&(&window, &explicit.samples, &explicit.search)).unwrap();
        assert_eq!(
            explicit.input_hash,
            hex::encode(Sha256::digest(tuple_bytes))
        );
        let delayed = preview_measurement_period_at_cutoff(
            &scope,
            &window,
            vec![sample.clone()],
            search,
            cutoff,
            cutoff + Duration::days(2),
        )
        .unwrap();
        assert_eq!(delayed.input_hash, explicit.input_hash);
        let first = freeze_measurement_period(&scope, explicit, 1, None).unwrap();
        let later = freeze_measurement_period(&scope, delayed, 1, None).unwrap();
        assert_eq!(first.report_id, later.report_id);
        let digest = Sha256::digest(
            format!(
                "measurement-period-v1:{}:{}:{}:{}:{}:{}:1",
                scope.operator_id,
                scope.tenant_id,
                first.project_id,
                window.start_at,
                window.end_at,
                window.report_timezone,
            )
            .as_bytes(),
        );
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        assert_eq!(first.report_id, Uuid::from_bytes(bytes));
    }
}

#[test]
fn preview_freeze_and_correction_reject_invalid_clocks() {
    let (scope, window, sample) = fixture();
    let cutoff = window.end_at;
    let generated_at = cutoff + Duration::hours(1);
    let preview = preview_measurement_period_at_cutoff(
        &scope,
        &window,
        vec![sample.clone()],
        None,
        cutoff,
        generated_at,
    )
    .unwrap();
    let first = freeze_measurement_period(&scope, preview.clone(), 1, None).unwrap();
    validate_measurement_period_correction(&[], &first).unwrap();
    for invalid_cutoff in [
        cutoff - Duration::nanoseconds(1),
        generated_at + Duration::nanoseconds(1),
    ] {
        assert!(
            preview_measurement_period_at_cutoff(
                &scope,
                &window,
                vec![sample.clone()],
                None,
                invalid_cutoff,
                generated_at,
            )
            .is_err()
        );
        let mut invalid_preview = preview.clone();
        invalid_preview.evidence_as_of = invalid_cutoff;
        assert!(freeze_measurement_period(&scope, invalid_preview, 1, None).is_err());
        let mut invalid_report = first.clone();
        invalid_report.evidence_as_of = invalid_cutoff;
        assert!(validate_measurement_period_correction(&[], &invalid_report).is_err());
    }
}

#[test]
fn corrections_require_monotonic_cutoff_and_frozen_sample_identity() {
    let (scope, window, sample) = fixture();
    let cutoff = window.end_at + Duration::hours(1);
    let generated_at = cutoff + Duration::hours(2);
    let preview = preview_measurement_period_at_cutoff(
        &scope,
        &window,
        vec![sample.clone()],
        None,
        cutoff,
        generated_at,
    )
    .unwrap();
    let first = freeze_measurement_period(&scope, preview, 1, None).unwrap();
    let frozen = serde_json::to_vec(&first).unwrap();
    for (correction_cutoff, accepted) in [
        (cutoff - Duration::nanoseconds(1), false),
        (cutoff, true),
        (cutoff + Duration::nanoseconds(1), true),
    ] {
        let preview = preview_measurement_period_at_cutoff(
            &scope,
            &window,
            vec![sample.clone()],
            None,
            correction_cutoff,
            generated_at,
        )
        .unwrap();
        let correction =
            freeze_measurement_period(&scope, preview, 2, Some(first.report_id)).unwrap();
        assert_eq!(
            validate_measurement_period_correction(std::slice::from_ref(&first), &correction)
                .is_ok(),
            accepted
        );
        let mut changed_identity = correction;
        changed_identity.samples[0].target_id = Uuid::new_v4();
        assert!(
            validate_measurement_period_correction(std::slice::from_ref(&first), &changed_identity)
                .is_err()
        );
    }
    assert_eq!(serde_json::to_vec(&first).unwrap(), frozen);
}
