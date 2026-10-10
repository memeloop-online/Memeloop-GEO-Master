//! Bounded P00 search DTOs. No tenant selector, credential or raw body crosses
//! this boundary; question purpose comes only from persisted server bindings.
use chrono::{DateTime, Utc};
use geo_domain::{
    QuestionPurpose, QuestionReference, SerpCoverage, SerpObservationStatus, SerpResultKind,
    SerpTarget, SerpTargetMatch, SerpTaskState,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpCreateRequest {
    pub query: String,
    pub idempotency_key: String,
    pub scheduled_at: DateTime<Utc>,
    #[serde(default)]
    pub source_key: Option<String>,
    #[serde(default)]
    pub target: Option<SerpTarget>,
    #[serde(default)]
    pub question_reference: Option<QuestionReference>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerpReadMode {
    #[default]
    Capabilities,
    History,
    Detail,
    Sources,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpReadRequest {
    #[serde(default)]
    pub mode: SerpReadMode,
    #[serde(default)]
    pub measurement_id: Option<Uuid>,
    #[serde(default)]
    pub after: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpReparseRequest {
    pub measurement_id: Uuid,
    pub evidence_id: Uuid,
    pub idempotency_key: String,
}

fn label(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
impl SerpCreateRequest {
    pub fn validate(&self) -> Result<(), String> {
        if !label(&self.query, 2800)
            || self.query.chars().count() > 700
            || !label(&self.idempotency_key, 256)
            || self.source_key.as_ref().is_some_and(|key| !label(key, 128))
            || self
                .target
                .as_ref()
                .is_some_and(|target| target.normalized().is_err())
            || self.question_reference.is_some_and(|reference| {
                [
                    reference.question_id,
                    reference.question_revision_id,
                    reference.question_set_id,
                    reference.question_set_version_id,
                ]
                .iter()
                .any(Uuid::is_nil)
            })
        {
            return Err("invalid search creation request".into());
        }
        Ok(())
    }
}
impl SerpReadRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.limit.is_some_and(|limit| limit == 0 || limit > 10)
            || self.after.is_some_and(|id| id.is_nil())
            || self.measurement_id.is_some_and(|id| id.is_nil())
            || match self.mode {
                SerpReadMode::Capabilities => self.measurement_id.is_some() || self.after.is_some(),
                SerpReadMode::History => self.measurement_id.is_some(),
                SerpReadMode::Detail | SerpReadMode::Sources => self.measurement_id.is_none(),
            }
        {
            return Err("invalid search read selection".into());
        }
        Ok(())
    }
}
impl SerpReparseRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.measurement_id.is_nil()
            || self.evidence_id.is_nil()
            || !label(&self.idempotency_key, 256)
        {
            return Err("invalid search reparse request".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpToolCapability {
    pub source_key: String,
    pub engine: String,
    pub country: String,
    pub city: Option<String>,
    pub language: String,
    pub requested_depth: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpToolMeasurement {
    pub measurement_id: Uuid,
    pub state: SerpTaskState,
    pub source_key: String,
    pub scheduled_at: DateTime<Utc>,
    pub question_purpose: Option<QuestionPurpose>,
    pub details_available: bool,
    pub query: Option<String>,
    pub href: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpToolRank {
    pub kind: SerpResultKind,
    pub url: Option<String>,
    pub title: Option<String>,
    pub organic_rank: Option<u32>,
    pub absolute_position: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpToolObservation {
    pub observation_id: Uuid,
    pub evidence_id: Uuid,
    pub status: SerpObservationStatus,
    pub coverage: SerpCoverage,
    pub received_at: DateTime<Utc>,
    pub analyzed_at: DateTime<Utc>,
    /// None means purpose-restricted, never a negative target finding.
    pub target_match: Option<SerpTargetMatch>,
    pub results: Vec<SerpToolRank>,
    pub omitted_results: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpToolEvidence {
    pub evidence_id: Uuid,
    pub captured_at: DateTime<Utc>,
    pub stored_at: DateTime<Utc>,
    pub body_bytes: usize,
    pub body_complete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpCreateReceipt {
    pub measurement: SerpToolMeasurement,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpReadResult {
    pub mode: SerpReadMode,
    /// Copy unchanged into create.scheduled_at and retain it on retries.
    pub server_time: DateTime<Utc>,
    pub capabilities: Vec<SerpToolCapability>,
    pub measurements: Vec<SerpToolMeasurement>,
    pub observations: Vec<SerpToolObservation>,
    pub evidence: Vec<SerpToolEvidence>,
    pub next_after: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerpReparseReceipt {
    pub measurement: SerpToolMeasurement,
    pub observation: SerpToolObservation,
}

impl SerpToolMeasurement {
    pub fn validate_for(&self, scope: &geo_domain::TenantScope) -> Result<(), String> {
        let project = scope.project_id.ok_or("project scope required")?;
        let allowed = self
            .question_purpose
            .is_none_or(|purpose| purpose == QuestionPurpose::Optimization);
        if self.measurement_id.is_nil()
            || !label(&self.source_key, 128)
            || self.details_available != allowed
            || (!allowed && self.query.is_some())
            || (allowed
                && self
                    .query
                    .as_ref()
                    .is_none_or(|query| !label(query, 2800) || query.chars().count() > 700))
            || self.href != format!("/app/{}/{project}/measurement?tab=search", scope.tenant_id)
        {
            return Err("invalid search measurement projection".into());
        }
        Ok(())
    }
}

impl SerpToolObservation {
    pub fn validate_for(&self, measurement: &SerpToolMeasurement) -> Result<(), String> {
        if self.observation_id.is_nil()
            || self.evidence_id.is_nil()
            || self.results.len() > 10
            || self.omitted_results > geo_domain::MAX_SERP_RESULTS
            || (!measurement.details_available
                && (!self.results.is_empty() || self.target_match.is_some()))
            || (measurement.details_available && self.target_match.is_none())
            || self.results.iter().any(|result| {
                result
                    .title
                    .as_ref()
                    .is_some_and(|title| title.chars().count() > 256)
                    || result.url.as_ref().is_some_and(|url| {
                        url.len() > 2048 || geo_domain::normalize_serp_url(url).is_err()
                    })
                    || result.organic_rank == Some(0)
                    || result.absolute_position == Some(0)
                    || (result.kind != SerpResultKind::Organic && result.organic_rank.is_some())
            })
        {
            return Err("invalid search observation projection".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_dtos_reject_scope_raw_credentials_and_invalid_selectors() {
        let create = serde_json::json!({"query":" rain + gauge% ","idempotency_key":"synthetic","scheduled_at":Utc::now()});
        let valid: SerpCreateRequest = serde_json::from_value(create.clone()).unwrap();
        valid.validate().unwrap();
        assert_eq!(valid.query, " rain + gauge% ");
        for field in ["tenant_id", "project_id", "password", "raw_body", "purpose"] {
            let mut forged = create.clone();
            forged[field] = serde_json::json!("not-allowed");
            assert!(serde_json::from_value::<SerpCreateRequest>(forged).is_err());
        }
        assert!(
            SerpReadRequest {
                mode: SerpReadMode::Detail,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SerpReadRequest {
                limit: Some(11),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        SerpReadRequest::default().validate().unwrap();
    }
}
