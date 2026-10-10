//! Search evidence has its own cohort and denominator, never AI citation ranks.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppError, FrozenQuestionBinding, MeasurementPeriodWindow, SerpEvidenceOperation,
    SerpMeasurement, SerpObservation, SerpObservationStatus, SerpProtocol, SerpTarget,
    SerpTargetMatch, SerpTaskState,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodSearchIdentity {
    pub measurement_id: Uuid,
    pub source_key: String,
    /// Exact query bytes, duplicated for presentation and validated against protocol.
    pub query: String,
    pub protocol: SerpProtocol,
    pub target: Option<SerpTarget>,
    pub target_rule_version: String,
    pub question_binding: Option<FrozenQuestionBinding>,
    pub scheduled_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub stored_at: DateTime<Utc>,
}

impl MeasurementPeriodSearchIdentity {
    pub fn from_measurement(measurement: &SerpMeasurement, stored_at: DateTime<Utc>) -> Self {
        Self {
            measurement_id: measurement.measurement_id,
            source_key: measurement.source_key.clone(),
            query: measurement.protocol.query.clone(),
            protocol: measurement.protocol.clone(),
            target: measurement.target.clone(),
            target_rule_version: measurement.target_rule_version.clone(),
            question_binding: measurement.question_binding.clone(),
            scheduled_at: measurement.scheduled_at,
            created_at: measurement.created_at,
            stored_at,
        }
    }

    // Validation/target matching only: never serialized as historical task state.
    fn measurement(&self) -> SerpMeasurement {
        SerpMeasurement {
            measurement_id: self.measurement_id,
            source_key: self.source_key.clone(),
            protocol: self.protocol.clone(),
            target: self.target.clone(),
            target_rule_version: self.target_rule_version.clone(),
            question_binding: self.question_binding.clone(),
            scheduled_at: self.scheduled_at,
            created_at: self.created_at,
            state: SerpTaskState::Queued,
        }
    }

    fn validate(
        &self,
        window: &MeasurementPeriodWindow,
        cutoff: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.protocol.validate()?;
        if let Some(target) = &self.target {
            target.normalized()?;
        }
        if self.measurement_id.is_nil()
            || self.source_key.trim().is_empty()
            || self.source_key.len() > 128
            || self.source_key.chars().any(char::is_control)
            || self.query != self.protocol.query
            || self.target_rule_version != crate::SERP_TARGET_RULE_VERSION
            || self.scheduled_at < window.start_at
            || self.scheduled_at >= window.end_at
            || self.created_at > cutoff
            || self.stored_at > cutoff
            || self.question_binding.as_ref().is_some_and(|binding| {
                let reference = binding.reference;
                reference.question_set_id.is_nil()
                    || reference.question_set_version_id.is_nil()
                    || reference.question_id.is_nil()
                    || reference.question_revision_id.is_nil()
                    || binding.split_policy_version.trim().is_empty()
                    || binding.split_policy_version.len() > 128
                    || binding.split_policy_version.chars().any(char::is_control)
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Narrow immutable provenance; deliberately excludes provider task IDs and bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SerpReportRawMetadata {
    pub evidence_id: Uuid,
    pub measurement_id: Uuid,
    pub attempt_id: Uuid,
    pub operation: SerpEvidenceOperation,
    pub response_sha256: String,
    pub body_complete: bool,
    pub captured_at: DateTime<Utc>,
    pub stored_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SerpReportObservation {
    pub observation: SerpObservation,
    pub observation_stored_at: DateTime<Utc>,
    pub raw: SerpReportRawMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchEvidenceTimeBasis {
    ProviderObservedAt,
    ReceivedAt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodSearchEvidence {
    pub observation: SerpObservation,
    pub raw_stored_at: DateTime<Utc>,
    pub observation_stored_at: DateTime<Utc>,
    pub evidence_time: DateTime<Utc>,
    pub evidence_time_basis: SearchEvidenceTimeBasis,
    pub target_match: SerpTargetMatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodSearchSample {
    pub cohort: MeasurementPeriodSearchIdentity,
    pub evidence: Option<MeasurementPeriodSearchEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementPeriodSearchStatus {
    NoEligibleObservation,
    Observed,
    Partial,
    Challenge,
    LoginRequired,
    Missing,
    Failed,
    Unsupported,
}

impl From<SerpObservationStatus> for MeasurementPeriodSearchStatus {
    fn from(status: SerpObservationStatus) -> Self {
        match status {
            SerpObservationStatus::Observed => Self::Observed,
            SerpObservationStatus::Partial => Self::Partial,
            SerpObservationStatus::Challenge => Self::Challenge,
            SerpObservationStatus::LoginRequired => Self::LoginRequired,
            SerpObservationStatus::Missing => Self::Missing,
            SerpObservationStatus::Failed => Self::Failed,
            SerpObservationStatus::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodSearchCoverage {
    pub planned: u64,
    pub counts: BTreeMap<MeasurementPeriodSearchStatus, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeasurementPeriodSearchSection {
    pub schema_version: String,
    pub coverage: MeasurementPeriodSearchCoverage,
    pub samples: Vec<MeasurementPeriodSearchSample>,
}

/// Filter by immutable provenance and cutoff before choosing the newest useful
/// version. A later unsuccessful interpretation does not erase useful evidence.
/// Application event clocks and repository storage clocks are checked separately
/// against the explicit cutoff, never ordered against each other as causal proof.
pub fn build_measurement_period_search_sample(
    cohort: MeasurementPeriodSearchIdentity,
    candidates: Vec<SerpReportObservation>,
    window: &MeasurementPeriodWindow,
    evidence_as_of: DateTime<Utc>,
) -> Result<MeasurementPeriodSearchSample, AppError> {
    cohort.validate(window, evidence_as_of)?;
    let measurement = cohort.measurement();
    let selected = candidates
        .into_iter()
        .filter(|candidate| {
            let o = &candidate.observation;
            let raw = &candidate.raw;
            let time = o.provider_observed_at.unwrap_or(o.received_at);
            o.validate(&measurement).is_ok()
                && raw.evidence_id == o.raw_evidence_id
                && raw.measurement_id == o.measurement_id
                && raw.attempt_id == o.attempt_id
                && raw.response_sha256 == o.raw_sha256
                && raw.captured_at == o.received_at
                && raw.captured_at <= evidence_as_of
                && raw.stored_at <= evidence_as_of
                && o.received_at <= evidence_as_of
                && o.analyzed_at <= evidence_as_of
                && o.provider_observed_at.is_none_or(|at| at <= evidence_as_of)
                && candidate.observation_stored_at <= evidence_as_of
                && (raw.body_complete || o.status != SerpObservationStatus::Observed)
                && (raw.operation != SerpEvidenceOperation::Submission
                    || (o.status != SerpObservationStatus::Observed && o.results.is_empty()))
                && time >= window.start_at
                && time < window.end_at
        })
        .max_by_key(|candidate| {
            let o = &candidate.observation;
            (
                matches!(
                    o.status,
                    SerpObservationStatus::Observed | SerpObservationStatus::Partial
                ),
                o.analyzed_at,
                o.observation_id,
            )
        });
    let evidence = selected
        .map(|candidate| {
            let observation = candidate.observation;
            let evidence_time = observation
                .provider_observed_at
                .unwrap_or(observation.received_at);
            let evidence_time_basis = if observation.provider_observed_at.is_some() {
                SearchEvidenceTimeBasis::ProviderObservedAt
            } else {
                SearchEvidenceTimeBasis::ReceivedAt
            };
            let target_match = observation.target_match(&measurement)?;
            Ok::<_, AppError>(MeasurementPeriodSearchEvidence {
                observation,
                raw_stored_at: candidate.raw.stored_at,
                observation_stored_at: candidate.observation_stored_at,
                evidence_time,
                evidence_time_basis,
                target_match,
            })
        })
        .transpose()?;
    Ok(MeasurementPeriodSearchSample { cohort, evidence })
}

pub(crate) fn measurement_period_search_section(
    mut samples: Vec<MeasurementPeriodSearchSample>,
    window: &MeasurementPeriodWindow,
    cutoff: DateTime<Utc>,
) -> Result<MeasurementPeriodSearchSection, AppError> {
    samples.sort_by_key(|sample| sample.cohort.measurement_id);
    let mut seen = BTreeSet::new();
    let mut coverage = MeasurementPeriodSearchCoverage::default();
    for sample in &samples {
        sample.cohort.validate(window, cutoff)?;
        if !seen.insert(sample.cohort.measurement_id) {
            return Err(invalid());
        }
        if let Some(evidence) = &sample.evidence {
            let o = &evidence.observation;
            o.validate(&sample.cohort.measurement())?;
            let time = o.provider_observed_at.unwrap_or(o.received_at);
            let basis = if o.provider_observed_at.is_some() {
                SearchEvidenceTimeBasis::ProviderObservedAt
            } else {
                SearchEvidenceTimeBasis::ReceivedAt
            };
            if evidence.evidence_time != time
                || evidence.evidence_time_basis != basis
                || time < window.start_at
                || time >= window.end_at
                || o.received_at > cutoff
                || o.analyzed_at > cutoff
                || o.provider_observed_at.is_some_and(|at| at > cutoff)
                || evidence.raw_stored_at > cutoff
                || evidence.observation_stored_at > cutoff
                || evidence.target_match != o.target_match(&sample.cohort.measurement())?
            {
                return Err(invalid());
            }
        }
        let status = sample.evidence.as_ref().map_or(
            MeasurementPeriodSearchStatus::NoEligibleObservation,
            |evidence| evidence.observation.status.into(),
        );
        coverage.planned += 1;
        *coverage.counts.entry(status).or_default() += 1;
    }
    Ok(MeasurementPeriodSearchSection {
        schema_version: "geo.measurement_period.search.v1".into(),
        coverage,
        samples,
    })
}

fn invalid() -> AppError {
    AppError::invalid_request("invalid search report sample")
}
