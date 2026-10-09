//! Cycle-free, immutable reports over independent project measurement plans.
//! Evaluation evidence stays in reports; this is not an optimizer input.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, EffectiveObservation, FrozenQuestionBinding, ProjectId, TenantScope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodWindow {
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub report_timezone: String,
}

impl MeasurementPeriodWindow {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), AppError> {
        if self.start_at >= self.end_at
            || self.end_at > now
            || self.report_timezone.parse::<chrono_tz::Tz>().is_err()
        {
            return Err(AppError::invalid_request(
                "invalid measurement report window or timezone",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodSample {
    pub plan_id: Uuid,
    pub target_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<Uuid>,
    pub comparison_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_binding: Option<FrozenQuestionBinding>,
    pub scheduled_at: DateTime<Utc>,
    /// Frozen original outcome, never upgraded by a later interpretation.
    pub original_status: String,
    /// Independently validated original live receipt, not saved-analysis success.
    pub observed_live: bool,
    pub observation: Option<EffectiveObservation>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodCoverage {
    pub planned: u64,
    pub counts: BTreeMap<String, u64>,
    pub grounded_saved_analysis: u64,
    pub observed_live: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementPeriodPreviewKind {
    MeasurementPeriodPreview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementPeriodReportKind {
    MeasurementPeriod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodPreview {
    pub kind: MeasurementPeriodPreviewKind,
    pub project_id: ProjectId,
    pub report_window_start_at: DateTime<Utc>,
    pub report_window_end_at: DateTime<Utc>,
    pub report_timezone: String,
    pub evidence_as_of: DateTime<Utc>,
    pub generated_at: DateTime<Utc>,
    pub input_hash: String,
    pub coverage: MeasurementPeriodCoverage,
    pub samples: Vec<MeasurementPeriodSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodReport {
    pub kind: MeasurementPeriodReportKind,
    pub report_id: Uuid,
    pub project_id: ProjectId,
    pub revision: u32,
    pub correction_of: Option<Uuid>,
    pub report_window_start_at: DateTime<Utc>,
    pub report_window_end_at: DateTime<Utc>,
    pub report_timezone: String,
    pub evidence_as_of: DateTime<Utc>,
    pub generated_at: DateTime<Utc>,
    pub input_hash: String,
    pub coverage: MeasurementPeriodCoverage,
    pub samples: Vec<MeasurementPeriodSample>,
}

impl MeasurementPeriodReport {
    pub fn window(&self) -> MeasurementPeriodWindow {
        MeasurementPeriodWindow {
            start_at: self.report_window_start_at,
            end_at: self.report_window_end_at,
            report_timezone: self.report_timezone.clone(),
        }
    }
}

pub fn preview_measurement_period(
    scope: &TenantScope,
    window: &MeasurementPeriodWindow,
    mut samples: Vec<MeasurementPeriodSample>,
    now: DateTime<Utc>,
) -> Result<MeasurementPeriodPreview, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    window.validate(now)?;
    samples.sort_by_key(|sample| sample.target_id);
    let mut seen = BTreeSet::new();
    let mut coverage = MeasurementPeriodCoverage::default();
    for sample in &mut samples {
        if !seen.insert(sample.target_id)
            || sample.scheduled_at < window.start_at
            || sample.scheduled_at >= window.end_at
            || sample.comparison_key.trim().is_empty()
            || ![
                "pending",
                "unknown",
                "observed",
                "refused",
                "missing",
                "failed",
                "login_required",
                "unsupported",
            ]
            .contains(&sample.original_status.as_str())
        {
            return Err(AppError::invalid_request(
                "invalid measurement report sample",
            ));
        }
        if sample.observed_live
            && (sample.original_status != "observed" || sample.attempt_id.is_none())
        {
            return Err(AppError::invalid_request(
                "invalid original observation provenance",
            ));
        }
        if sample.observation.as_ref().is_some_and(|observation| {
            sample.attempt_id.is_none()
                || observation.observed_at < window.start_at
                || observation.observed_at >= window.end_at
                || observation.observed_at > observation.received_at
                || observation.received_at > now
                || observation.provenance.as_ref().is_some_and(|source| {
                    source.observed_at != observation.observed_at
                        || source.analyzed_at > now
                        || source.analyzed_at < observation.observed_at
                        || source.actual_model.trim().is_empty()
                        || source.source_sha256.len() != 64
                        || !source.source_sha256.bytes().all(|b| b.is_ascii_hexdigit())
                })
                || (observation.provenance.is_none() && !sample.observed_live)
        }) {
            sample.observation = None;
        }
        coverage.planned += 1;
        *coverage
            .counts
            .entry(sample.original_status.clone())
            .or_default() += 1;
        coverage.observed_live += u64::from(sample.observed_live);
        coverage.grounded_saved_analysis += u64::from(
            sample
                .observation
                .as_ref()
                .is_some_and(|observation| observation.provenance.is_some()),
        );
    }
    let bytes = serde_json::to_vec(&(window, &samples))
        .map_err(|_| AppError::invalid_request("measurement report cannot be serialized"))?;
    Ok(MeasurementPeriodPreview {
        kind: MeasurementPeriodPreviewKind::MeasurementPeriodPreview,
        project_id,
        report_window_start_at: window.start_at,
        report_window_end_at: window.end_at,
        report_timezone: window.report_timezone.clone(),
        evidence_as_of: now,
        generated_at: now,
        input_hash: hex::encode(Sha256::digest(bytes)),
        coverage,
        samples,
    })
}

pub fn freeze_measurement_period(
    scope: &TenantScope,
    preview: MeasurementPeriodPreview,
    revision: u32,
    correction_of: Option<Uuid>,
) -> Result<MeasurementPeriodReport, AppError> {
    if scope.project_id != Some(preview.project_id)
        || revision == 0
        || (revision == 1) != correction_of.is_none()
    {
        return Err(AppError::invalid_request(
            "invalid measurement report revision",
        ));
    }
    let digest = Sha256::digest(
        format!(
            "measurement-period-v1:{}:{}:{}:{}:{}:{}:{revision}",
            scope.operator_id,
            scope.tenant_id,
            preview.project_id,
            preview.report_window_start_at,
            preview.report_window_end_at,
            preview.report_timezone,
        )
        .as_bytes(),
    );
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(MeasurementPeriodReport {
        kind: MeasurementPeriodReportKind::MeasurementPeriod,
        report_id: Uuid::from_bytes(bytes),
        project_id: preview.project_id,
        revision,
        correction_of,
        report_window_start_at: preview.report_window_start_at,
        report_window_end_at: preview.report_window_end_at,
        report_timezone: preview.report_timezone,
        evidence_as_of: preview.evidence_as_of,
        generated_at: preview.generated_at,
        input_hash: preview.input_hash,
        coverage: preview.coverage,
        samples: preview.samples,
    })
}

pub fn validate_measurement_period_correction(
    existing: &[MeasurementPeriodReport],
    proposed: &MeasurementPeriodReport,
) -> Result<(), AppError> {
    proposed.window().validate(proposed.generated_at)?;
    if proposed.evidence_as_of != proposed.generated_at {
        return Err(AppError::invalid_request(
            "measurement report evidence time differs",
        ));
    }
    if proposed.revision == 1 && proposed.correction_of.is_none() {
        return Ok(());
    }
    let parent = existing
        .iter()
        .find(|row| Some(row.report_id) == proposed.correction_of)
        .ok_or_else(|| AppError::conflict("measurement report correction parent not found"))?;
    if parent.project_id != proposed.project_id
        || parent.window() != proposed.window()
        || parent.revision + 1 != proposed.revision
        || parent.evidence_as_of > proposed.evidence_as_of
        || parent.samples.len() != proposed.samples.len()
        || parent
            .samples
            .iter()
            .zip(&proposed.samples)
            .any(|(before, after)| {
                before.plan_id != after.plan_id
                    || before.target_id != after.target_id
                    || before.comparison_key != after.comparison_key
                    || before.question_binding != after.question_binding
                    || before.scheduled_at != after.scheduled_at
            })
        || existing
            .iter()
            .any(|row| row.window() == proposed.window() && row.revision >= proposed.revision)
    {
        return Err(AppError::conflict(
            "measurement report correction parent is not latest",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObservationAnalysisSource, SavedAnalysisProvenance};
    use chrono::Duration;

    #[test]
    fn period_projection_keeps_original_counts_and_rejects_late_or_unbound_interpretations() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
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
        let observed = now - Duration::hours(1);
        let mut sample = MeasurementPeriodSample {
            plan_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Some(Uuid::new_v4()),
            comparison_key: "protocol".into(),
            question_binding: None,
            scheduled_at: observed,
            original_status: "unknown".into(),
            observed_live: false,
            observation: Some(EffectiveObservation {
                raw_answer: "Saved answer".into(),
                citations: vec![],
                observed_at: observed,
                received_at: observed,
                provenance: Some(SavedAnalysisProvenance {
                    revision_id: Uuid::new_v4(),
                    source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
                    source_sha256: "a".repeat(64),
                    observed_at: observed,
                    analyzed_at: now,
                    actual_model: "parser".into(),
                    config_revision: None,
                    prompt_version: "v1".into(),
                    parser_version: "v1".into(),
                }),
            }),
        };
        let accepted =
            preview_measurement_period(&scope, &window, vec![sample.clone()], now).unwrap();
        assert_eq!(accepted.coverage.planned, 1);
        assert_eq!(accepted.coverage.counts["unknown"], 1);
        assert_eq!(accepted.coverage.grounded_saved_analysis, 1);
        assert_eq!(accepted.coverage.observed_live, 0);
        sample
            .observation
            .as_mut()
            .unwrap()
            .provenance
            .as_mut()
            .unwrap()
            .analyzed_at += Duration::seconds(1);
        let late = preview_measurement_period(&scope, &window, vec![sample.clone()], now).unwrap();
        assert_eq!(late.coverage.grounded_saved_analysis, 0);
        assert!(late.samples[0].observation.is_none());
        sample.observation.as_mut().unwrap().provenance = None;
        assert!(
            preview_measurement_period(&scope, &window, vec![sample.clone()], now)
                .unwrap()
                .samples[0]
                .observation
                .is_none()
        );
        assert!(
            preview_measurement_period(&scope, &window, vec![sample.clone(), sample.clone()], now)
                .is_err()
        );
        let first = freeze_measurement_period(&scope, accepted, 1, None).unwrap();
        sample.target_id = Uuid::new_v4();
        let changed = freeze_measurement_period(
            &scope,
            preview_measurement_period(&scope, &window, vec![sample], now + Duration::seconds(1))
                .unwrap(),
            2,
            Some(first.report_id),
        )
        .unwrap();
        assert!(validate_measurement_period_correction(&[first], &changed).is_err());
    }
}
