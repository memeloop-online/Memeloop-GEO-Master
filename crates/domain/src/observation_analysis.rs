//! Saved-evidence interpretation revisions. These never replace a measurement
//! outcome, increase its denominator, or authorize provider cleanup.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    AppError, ChannelJobRepository, ChannelTargetInput, ChannelTargetView, ObservationCapture,
    ObservationCaptureRepository, ObservationCaptureSnapshot, TenantScope, sha256_hex,
};

pub const MAX_ANALYSIS_CANDIDATE_BYTES: usize = 150_000;
pub const MAX_ANALYSIS_ANSWER_BYTES: usize = 100_000;
pub const MAX_ANALYSIS_AUDIT_BYTES: usize = 150_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationAnalysisSource {
    Capture { capture_id: Uuid },
    AttemptEvidence { evidence_index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationAnalysisRequest {
    pub revision_id: Uuid,
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    pub source: ObservationAnalysisSource,
    pub source_sha256: String,
    pub observed_at: DateTime<Utc>,
    pub prompt_version: String,
    pub parser_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationAnalysisState {
    Queued,
    Running,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationAnalysisOutcome {
    Grounded {
        raw_answer: String,
        citations: Vec<String>,
        /// Pointer/quote audit produced by the deterministic grounding check,
        /// not an unrestricted explanation supplied by the model.
        audit: Value,
    },
    Unverified {
        reason: String,
    },
    Failed {
        code: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationAnalysisResult {
    pub actual_model: Option<String>,
    pub candidate_json: Option<String>,
    pub outcome: ObservationAnalysisOutcome,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationAnalysisRevision {
    pub request: ObservationAnalysisRequest,
    pub request_digest: String,
    pub state: ObservationAnalysisState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub analyzed_at: Option<DateTime<Utc>>,
    pub result: Option<ObservationAnalysisResult>,
}

/// Only the successful claimant receives this token. There is deliberately no
/// lease expiry/reclaim: an interrupted external inference is not blindly sent
/// again. The owning claimant may record a terminal interruption failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationAnalysisClaim {
    pub revision: ObservationAnalysisRevision,
    pub claim_token: Uuid,
}

fn bounded_label(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn fixed_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

impl ObservationAnalysisRequest {
    pub fn validate(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        created_at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        if self.revision_id.is_nil()
            || self.target_id.is_nil()
            || self.attempt_id.is_nil()
            || matches!(self.source, ObservationAnalysisSource::Capture { capture_id } if capture_id.is_nil())
            || !digest(&self.source_sha256)
            || !digest(request_digest)
            || !bounded_label(idempotency_key, 256)
            || !bounded_label(&self.prompt_version, 128)
            || !bounded_label(&self.parser_version, 128)
            || self.observed_at > created_at
        {
            return Err(AppError::invalid_request(
                "invalid observation analysis request",
            ));
        }
        Ok(())
    }

    /// Generated revision IDs and acceptance timestamps do not change the
    /// meaning of an idempotent submission.
    pub fn same_input(&self, other: &Self) -> bool {
        let mut normalized = other.clone();
        normalized.revision_id = self.revision_id;
        self == &normalized
    }
}

impl ObservationAnalysisResult {
    pub fn validate(&self) -> Result<(), AppError> {
        let invalid = || AppError::invalid_request("invalid observation analysis result");
        if self
            .actual_model
            .as_ref()
            .is_some_and(|model| !bounded_label(model, 256))
            || self.candidate_json.as_ref().is_some_and(|candidate| {
                candidate.len() > MAX_ANALYSIS_CANDIDATE_BYTES
                    || serde_json::from_str::<Value>(candidate).is_err()
            })
            || (!matches!(self.outcome, ObservationAnalysisOutcome::Failed { .. })
                && self.actual_model.is_none())
        {
            return Err(invalid());
        }
        match &self.outcome {
            ObservationAnalysisOutcome::Grounded {
                raw_answer,
                citations,
                audit,
            } => {
                if raw_answer.trim().is_empty()
                    || raw_answer.len() > MAX_ANALYSIS_ANSWER_BYTES
                    || citations.len() > 50
                    || citations.iter().any(|citation| {
                        citation.len() > 2048
                            || !url::Url::parse(citation).is_ok_and(|url| {
                                matches!(url.scheme(), "https" | "http")
                                    && url.host_str().is_some()
                                    && url.username().is_empty()
                                    && url.password().is_none()
                            })
                    })
                    || !audit.is_object()
                    || !audit["refs"]
                        .as_array()
                        .is_some_and(|refs| !refs.is_empty())
                    || serde_json::to_vec(audit).map_err(|_| invalid())?.len()
                        > MAX_ANALYSIS_AUDIT_BYTES
                    || self.candidate_json.is_none()
                {
                    return Err(invalid());
                }
            }
            ObservationAnalysisOutcome::Unverified { reason } if !fixed_code(reason) => {
                return Err(invalid());
            }
            ObservationAnalysisOutcome::Failed { code } if !fixed_code(code) => {
                return Err(invalid());
            }
            _ => {}
        }
        Ok(())
    }
}

/// Resolve only recognized saved evidence. This checks byte integrity and
/// ownership, not answer semantics. Callers perform interpretation and grounding
/// separately and must not infer cleanup permission from this return value.
pub fn observation_analysis_source_json(
    scope: &TenantScope,
    request: &ObservationAnalysisRequest,
    target: &ChannelTargetView,
    capture: Option<&ObservationCapture>,
) -> Result<String, AppError> {
    if scope.project_id.is_none() {
        return Err(AppError::forbidden("project scope required"));
    }
    if target.target.target_id != request.target_id
        || !matches!(target.target.input, ChannelTargetInput::Measure { .. })
    {
        return Err(AppError::forbidden("analysis measurement target mismatch"));
    }
    let attempt = target
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == request.attempt_id)
        .ok_or_else(|| AppError::not_found("analysis measurement attempt not found"))?;
    let outcome = attempt
        .outcome
        .as_ref()
        .ok_or_else(|| AppError::conflict("measurement attempt has not finished"))?;
    let received_at = attempt
        .received_at
        .ok_or_else(|| AppError::conflict("measurement receipt unavailable"))?;
    if outcome.fixture
        || attempt.claimed_at > request.observed_at
        || request.observed_at > received_at
    {
        return Err(AppError::conflict(
            "analysis source provenance or time mismatch",
        ));
    }
    let (source_json, source_sha256, observed_at) = match request.source {
        ObservationAnalysisSource::Capture { capture_id } => {
            let capture = capture
                .filter(|capture| {
                    capture.input.capture_id == capture_id
                        && capture.input.target_id == request.target_id
                        && capture.input.attempt_id == request.attempt_id
                        && capture.input.account_id == target.target.input.account_id()
                })
                .ok_or_else(|| AppError::not_found("analysis source capture not found"))?;
            if capture.input.validate(scope)? != capture.receipt.digest_sha256 {
                return Err(AppError::conflict(
                    "analysis source receipt digest mismatch",
                ));
            }
            let ObservationCaptureSnapshot::Source {
                source_json,
                source_sha256,
            } = &capture.input.snapshot
            else {
                return Err(AppError::invalid_request(
                    "analysis requires a source capture",
                ));
            };
            (
                source_json.clone(),
                source_sha256.clone(),
                capture.input.observed_at,
            )
        }
        ObservationAnalysisSource::AttemptEvidence { evidence_index } => {
            let evidence = outcome
                .runner_evidence
                .get(evidence_index as usize)
                .ok_or_else(|| AppError::not_found("analysis source evidence not found"))?;
            let source_capture = evidence["kind"] == "observation_capture"
                && evidence["schema_version"] == "geo.observation.capture.v1"
                && evidence["phase"] == "source";
            let extraction_audit = evidence["kind"] == "observation_extraction"
                && evidence["method"] == "llm_grounded";
            if !source_capture && !extraction_audit {
                return Err(AppError::invalid_request(
                    "unsupported saved observation evidence",
                ));
            }
            let source_json = evidence["source_json"]
                .as_str()
                .ok_or_else(|| AppError::invalid_request("saved observation source missing"))?;
            let source_sha256 = evidence["source_sha256"]
                .as_str()
                .ok_or_else(|| AppError::invalid_request("saved observation digest missing"))?;
            // An audit without its own observation timestamp remains bound to
            // the original attempt outcome time, never the replay time.
            let observed_at = match evidence.get("observed_at") {
                Some(value) => serde_json::from_value::<DateTime<Utc>>(value.clone())
                    .map_err(|_| AppError::invalid_request("saved observation time invalid"))?,
                None => outcome.occurred_at,
            };
            (
                source_json.to_owned(),
                source_sha256.to_owned(),
                observed_at,
            )
        }
    };
    if source_json.len() > crate::MAX_OBSERVATION_SOURCE_BYTES
        || sha256_hex(source_json.as_bytes()) != source_sha256
        || source_sha256 != request.source_sha256
        || observed_at != request.observed_at
    {
        return Err(AppError::conflict(
            "analysis source digest or time mismatch",
        ));
    }
    let document: Value = serde_json::from_str(&source_json)
        .map_err(|_| AppError::invalid_request("saved observation document invalid"))?;
    if !document.as_object().is_some_and(|fields| {
        fields
            .keys()
            .all(|key| key == "messages" || key == "rendered_text")
            && document["messages"].is_array()
            && fields.get("rendered_text").is_none_or(Value::is_string)
    }) {
        return Err(AppError::invalid_request(
            "saved observation document invalid",
        ));
    }
    Ok(source_json)
}

#[async_trait]
pub trait ObservationAnalysisRepository: Send + Sync {
    async fn create(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        created_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError>;
    async fn claim(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> Result<Option<ObservationAnalysisClaim>, AppError>;
    async fn finish(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
        claim_token: Uuid,
        result: ObservationAnalysisResult,
        analyzed_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError>;
    async fn get(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Option<ObservationAnalysisRevision>, AppError>;
    async fn list_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        after_revision_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError>;
}

pub type SharedObservationAnalysisRepository = Arc<dyn ObservationAnalysisRepository>;

struct StoredAnalysis {
    scope: TenantScope,
    key_hash: String,
    revision: ObservationAnalysisRevision,
    claim_token: Option<Uuid>,
}

pub struct MemoryObservationAnalysisRepository {
    jobs: Arc<dyn ChannelJobRepository>,
    captures: Arc<dyn ObservationCaptureRepository>,
    records: Mutex<HashMap<Uuid, StoredAnalysis>>,
}

impl MemoryObservationAnalysisRepository {
    pub fn new(
        jobs: Arc<dyn ChannelJobRepository>,
        captures: Arc<dyn ObservationCaptureRepository>,
    ) -> Self {
        Self {
            jobs,
            captures,
            records: Mutex::new(HashMap::new()),
        }
    }
}

fn require_project(scope: &TenantScope) -> Result<(), AppError> {
    if scope.project_id.is_none() {
        Err(AppError::forbidden("project scope required"))
    } else {
        Ok(())
    }
}

#[async_trait]
impl ObservationAnalysisRepository for MemoryObservationAnalysisRepository {
    async fn create(
        &self,
        scope: &TenantScope,
        idempotency_key: &str,
        request_digest: &str,
        request: ObservationAnalysisRequest,
        created_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError> {
        request.validate(scope, idempotency_key, request_digest, created_at)?;
        let target = self.jobs.get_target(scope, request.target_id).await?;
        let capture = match request.source {
            ObservationAnalysisSource::Capture { capture_id } => {
                self.captures.get(scope, capture_id).await?
            }
            ObservationAnalysisSource::AttemptEvidence { .. } => None,
        };
        observation_analysis_source_json(scope, &request, &target, capture.as_ref())?;
        let key_hash = sha256_hex(idempotency_key.as_bytes());
        let mut records = self.records.lock().await;
        if let Some(prior) = records
            .values()
            .find(|stored| stored.scope == *scope && stored.key_hash == key_hash)
        {
            return if prior.revision.request_digest == request_digest
                && prior.revision.request.same_input(&request)
            {
                Ok(prior.revision.clone())
            } else {
                Err(AppError::conflict("analysis idempotency request differs"))
            };
        }
        if records.contains_key(&request.revision_id) {
            return Err(AppError::conflict(
                "analysis revision identity already exists",
            ));
        }
        let revision = ObservationAnalysisRevision {
            request,
            request_digest: request_digest.to_owned(),
            state: ObservationAnalysisState::Queued,
            created_at,
            started_at: None,
            analyzed_at: None,
            result: None,
        };
        records.insert(
            revision.request.revision_id,
            StoredAnalysis {
                scope: scope.clone(),
                key_hash,
                revision: revision.clone(),
                claim_token: None,
            },
        );
        Ok(revision)
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> Result<Option<ObservationAnalysisClaim>, AppError> {
        require_project(scope)?;
        let mut records = self.records.lock().await;
        let stored = records
            .get_mut(&revision_id)
            .filter(|stored| stored.scope == *scope)
            .ok_or_else(|| AppError::not_found("analysis revision not found"))?;
        if stored.revision.state != ObservationAnalysisState::Queued {
            return Ok(None);
        }
        if started_at < stored.revision.created_at {
            return Err(AppError::invalid_request(
                "analysis start precedes acceptance",
            ));
        }
        let claim_token = Uuid::new_v4();
        stored.claim_token = Some(claim_token);
        stored.revision.state = ObservationAnalysisState::Running;
        stored.revision.started_at = Some(started_at);
        Ok(Some(ObservationAnalysisClaim {
            revision: stored.revision.clone(),
            claim_token,
        }))
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
        claim_token: Uuid,
        result: ObservationAnalysisResult,
        analyzed_at: DateTime<Utc>,
    ) -> Result<ObservationAnalysisRevision, AppError> {
        require_project(scope)?;
        result.validate()?;
        let mut records = self.records.lock().await;
        let stored = records
            .get_mut(&revision_id)
            .filter(|stored| stored.scope == *scope)
            .ok_or_else(|| AppError::not_found("analysis revision not found"))?;
        if stored.claim_token != Some(claim_token)
            || stored
                .revision
                .started_at
                .is_none_or(|started| analyzed_at < started)
        {
            return Err(AppError::conflict(
                "analysis claim or completion time differs",
            ));
        }
        if stored.revision.state == ObservationAnalysisState::Completed {
            return if stored.revision.result.as_ref() == Some(&result)
                && stored
                    .revision
                    .analyzed_at
                    .is_some_and(|at| at.timestamp_micros() == analyzed_at.timestamp_micros())
            {
                Ok(stored.revision.clone())
            } else {
                Err(AppError::conflict("analysis result already recorded"))
            };
        }
        if stored.revision.state != ObservationAnalysisState::Running {
            return Err(AppError::conflict("analysis revision not running"));
        }
        stored.revision.result = Some(result);
        stored.revision.analyzed_at = Some(analyzed_at);
        stored.revision.state = ObservationAnalysisState::Completed;
        Ok(stored.revision.clone())
    }

    async fn get(
        &self,
        scope: &TenantScope,
        revision_id: Uuid,
    ) -> Result<Option<ObservationAnalysisRevision>, AppError> {
        require_project(scope)?;
        Ok(self
            .records
            .lock()
            .await
            .get(&revision_id)
            .filter(|stored| stored.scope == *scope)
            .map(|stored| stored.revision.clone()))
    }

    async fn list_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        after_revision_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationAnalysisRevision>, AppError> {
        require_project(scope)?;
        if !(1..=100).contains(&limit) {
            return Err(AppError::invalid_request("invalid analysis page size"));
        }
        let records = self.records.lock().await;
        let matches = |stored: &&StoredAnalysis| {
            stored.scope == *scope
                && stored.revision.request.target_id == target_id
                && stored.revision.request.attempt_id == attempt_id
        };
        if after_revision_id
            .is_some_and(|id| records.get(&id).is_none_or(|stored| !matches(&stored)))
        {
            return Err(AppError::invalid_request("invalid analysis cursor"));
        }
        let mut revisions: Vec<_> = records
            .values()
            .filter(matches)
            .filter(|stored| {
                after_revision_id.is_none_or(|id| stored.revision.request.revision_id > id)
            })
            .map(|stored| stored.revision.clone())
            .collect();
        revisions.sort_by_key(|revision| revision.request.revision_id);
        revisions.truncate(limit);
        Ok(revisions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChannelOutcome, ChannelOutcomeStatus, ChannelTarget, ErrorCode, MemoryChannelJobRepository,
        MemoryObservationCaptureRepository, ObservationCaptureInput, StandaloneMeasurementPlan,
    };
    use chrono::Duration;
    use serde_json::json;

    struct Fixture {
        scope: TenantScope,
        jobs: Arc<MemoryChannelJobRepository>,
        captures: Arc<MemoryObservationCaptureRepository>,
        store: MemoryObservationAnalysisRepository,
        request: ObservationAnalysisRequest,
        source_capture_id: Uuid,
        now: DateTime<Utc>,
    }

    async fn fixture() -> Fixture {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let jobs = Arc::new(MemoryChannelJobRepository::default());
        let captures = Arc::new(MemoryObservationCaptureRepository::new(jobs.clone()));
        let (target_id, attempt_id, account_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let source_json = r#"{"messages":[{"content":"Synthetic saved answer"}]}"#.to_owned();
        let source_sha256 = sha256_hex(source_json.as_bytes());
        let observed_at = now + Duration::seconds(1);
        jobs.create_measurement_plan(
            &scope,
            "synthetic-plan",
            "synthetic-digest",
            StandaloneMeasurementPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                title: "Synthetic".into(),
                input_hash: "synthetic-digest".into(),
                revision: 1,
                created_at: now,
                targets: vec![ChannelTarget {
                    target_id,
                    input: ChannelTargetInput::Measure {
                        account_id,
                        provider: "synthetic".into(),
                        model: "synthetic".into(),
                        surface: "consumer_web".into(),
                        search_mode: "web_search".into(),
                        protocol_version: "v1".into(),
                        question_set_version: "adhoc".into(),
                        question: "Synthetic question?".into(),
                        market: "global".into(),
                        language: "en".into(),
                        scheduled_at: now,
                        sample_ordinal: 0,
                        question_binding: None,
                    },
                }],
            },
        )
        .await
        .unwrap();
        jobs.claim(&scope, target_id, attempt_id, now)
            .await
            .unwrap();
        let source_capture_id = Uuid::new_v4();
        captures
            .save(
                &scope,
                ObservationCaptureInput {
                    capture_id: source_capture_id,
                    target_id,
                    attempt_id,
                    account_id,
                    runner_session_id: Uuid::new_v4(),
                    original_identity: None,
                    ordinal: 0,
                    observed_at,
                    snapshot: ObservationCaptureSnapshot::Source {
                        source_json: source_json.clone(),
                        source_sha256: source_sha256.clone(),
                    },
                    owned_conversation: None,
                    completion: None,
                },
            )
            .await
            .unwrap();
        jobs.finish(&scope, target_id, attempt_id, ChannelOutcome {
            status: ChannelOutcomeStatus::Unknown, detail: Some("interpretation unavailable".into()),
            occurred_at: observed_at, raw_answer: None, citations: vec![], public_url: None,
            screenshot_ref: None, connector_version: Some("synthetic-live".into()), fixture: false,
            runner_evidence: vec![json!({
                "kind":"observation_capture","schema_version":"geo.observation.capture.v1",
                "phase":"source","source_json":source_json,"source_sha256":source_sha256,"observed_at":observed_at,
            })],
        }, now + Duration::seconds(2)).await.unwrap();
        let store = MemoryObservationAnalysisRepository::new(jobs.clone(), captures.clone());
        Fixture {
            scope,
            jobs,
            captures,
            store,
            source_capture_id,
            now: now + Duration::seconds(3),
            request: ObservationAnalysisRequest {
                revision_id: Uuid::new_v4(),
                target_id,
                attempt_id,
                source: ObservationAnalysisSource::AttemptEvidence { evidence_index: 0 },
                source_sha256,
                observed_at,
                prompt_version: "extract.v1".into(),
                parser_version: "ground.v1".into(),
            },
        }
    }

    fn result() -> ObservationAnalysisResult {
        ObservationAnalysisResult {
            actual_model: Some("synthetic".into()),
            candidate_json: Some(r#"{"decision":"unverified"}"#.into()),
            outcome: ObservationAnalysisOutcome::Unverified {
                reason: "model_unverified".into(),
            },
            prompt_tokens: 3,
            completion_tokens: 1,
        }
    }

    #[tokio::test]
    async fn analysis_replay_atomic_claim_fencing_and_terminal_immutability() {
        let f = fixture().await;
        let request_digest = sha256_hex(b"synthetic-request");
        let original = f
            .jobs
            .get_target(&f.scope, f.request.target_id)
            .await
            .unwrap();
        let first = f
            .store
            .create(
                &f.scope,
                "analysis-key",
                &request_digest,
                f.request.clone(),
                f.now,
            )
            .await
            .unwrap();
        assert_eq!(first.state, ObservationAnalysisState::Queued);
        let mut replay = f.request.clone();
        replay.revision_id = Uuid::new_v4();
        assert_eq!(
            first,
            f.store
                .create(
                    &f.scope,
                    "analysis-key",
                    &request_digest,
                    replay.clone(),
                    f.now
                )
                .await
                .unwrap()
        );
        replay.parser_version = "ground.v2".into();
        assert_eq!(
            f.store
                .create(&f.scope, "analysis-key", &request_digest, replay, f.now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        let (left, right) = tokio::join!(
            f.store.claim(&f.scope, f.request.revision_id, f.now),
            f.store.claim(&f.scope, f.request.revision_id, f.now),
        );
        let claims: Vec<_> = [left.unwrap(), right.unwrap()]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(claims.len(), 1);
        let claim = &claims[0];
        assert_eq!(
            f.store
                .finish(
                    &f.scope,
                    f.request.revision_id,
                    Uuid::new_v4(),
                    result(),
                    f.now
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        let completed = f
            .store
            .finish(
                &f.scope,
                f.request.revision_id,
                claim.claim_token,
                result(),
                f.now,
            )
            .await
            .unwrap();
        assert_eq!(completed.state, ObservationAnalysisState::Completed);
        assert_eq!(completed.request.observed_at, f.request.observed_at);
        assert_eq!(completed.analyzed_at, Some(f.now));
        assert_eq!(
            completed,
            f.store
                .finish(
                    &f.scope,
                    f.request.revision_id,
                    claim.claim_token,
                    result(),
                    f.now
                )
                .await
                .unwrap()
        );
        let mut different = result();
        different.completion_tokens += 1;
        assert_eq!(
            f.store
                .finish(
                    &f.scope,
                    f.request.revision_id,
                    claim.claim_token,
                    different,
                    f.now
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        assert!(
            f.store
                .claim(&f.scope, f.request.revision_id, f.now)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            original,
            f.jobs
                .get_target(&f.scope, f.request.target_id)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn analysis_scoped_reads_writes_and_cursor_pages() {
        let f = fixture().await;
        let hash = sha256_hex(b"synthetic-request");
        f.store
            .create(&f.scope, "first", &hash, f.request.clone(), f.now)
            .await
            .unwrap();
        let mut other = f.scope.clone();
        other.tenant_id = Uuid::new_v4().into();
        assert!(
            f.store
                .get(&other, f.request.revision_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            f.store
                .claim(&other, f.request.revision_id, f.now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert!(
            f.store
                .create(&other, "first", &hash, f.request.clone(), f.now)
                .await
                .is_err()
        );
        let mut projectless = f.scope.clone();
        projectless.project_id = None;
        assert_eq!(
            f.store
                .get(&projectless, f.request.revision_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
        let mut next = f.request.clone();
        next.revision_id = Uuid::new_v4();
        f.store
            .create(&f.scope, "second", &hash, next, f.now)
            .await
            .unwrap();
        let first_page = f
            .store
            .list_for_attempt(&f.scope, f.request.target_id, f.request.attempt_id, None, 1)
            .await
            .unwrap();
        let next_page = f
            .store
            .list_for_attempt(
                &f.scope,
                f.request.target_id,
                f.request.attempt_id,
                Some(first_page[0].request.revision_id),
                1,
            )
            .await
            .unwrap();
        assert_eq!(next_page.len(), 1);
        assert_ne!(
            next_page[0].request.revision_id,
            first_page[0].request.revision_id
        );
        assert!(
            f.store
                .list_for_attempt(
                    &other,
                    f.request.target_id,
                    f.request.attempt_id,
                    Some(first_page[0].request.revision_id),
                    1
                )
                .await
                .is_err()
        );
        assert!(
            f.store
                .list_for_attempt(&f.scope, f.request.target_id, f.request.attempt_id, None, 0)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn analysis_capture_and_legacy_sources_reject_missing_tampered_or_rebound_bytes() {
        let f = fixture().await;
        let hash = sha256_hex(b"synthetic-request");
        let mut capture_request = f.request.clone();
        capture_request.source = ObservationAnalysisSource::Capture {
            capture_id: f.source_capture_id,
        };
        f.store
            .create(&f.scope, "capture", &hash, capture_request.clone(), f.now)
            .await
            .unwrap();
        let target = f
            .jobs
            .get_target(&f.scope, f.request.target_id)
            .await
            .unwrap();
        let mut capture = f
            .captures
            .get(&f.scope, f.source_capture_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            observation_analysis_source_json(&f.scope, &capture_request, &target, Some(&capture))
                .is_ok()
        );
        capture.receipt.digest_sha256 = "0".repeat(64);
        assert!(
            observation_analysis_source_json(&f.scope, &capture_request, &target, Some(&capture))
                .is_err()
        );
        let mut wrong_digest = f.request.clone();
        wrong_digest.revision_id = Uuid::new_v4();
        wrong_digest.source_sha256 = "0".repeat(64);
        assert!(
            f.store
                .create(&f.scope, "tamper", &hash, wrong_digest, f.now)
                .await
                .is_err()
        );
        let mut missing = f.request.clone();
        missing.source = ObservationAnalysisSource::AttemptEvidence { evidence_index: 99 };
        assert!(
            f.store
                .create(&f.scope, "missing", &hash, missing, f.now)
                .await
                .is_err()
        );
        let mut tampered = target.clone();
        tampered.attempts[0]
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence[0]["source_json"] = json!(r#"{"messages":[]}"#);
        assert!(observation_analysis_source_json(&f.scope, &f.request, &tampered, None).is_err());
        let mut fixture_target = target.clone();
        fixture_target.attempts[0].outcome.as_mut().unwrap().fixture = true;
        assert!(
            observation_analysis_source_json(&f.scope, &f.request, &fixture_target, None).is_err()
        );
        let mut audit_target = target.clone();
        let evidence = &mut audit_target.attempts[0]
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence[0];
        evidence["kind"] = json!("observation_extraction");
        evidence["method"] = json!("llm_grounded");
        evidence.as_object_mut().unwrap().remove("observed_at");
        assert!(
            observation_analysis_source_json(&f.scope, &f.request, &audit_target, None).is_ok()
        );
    }

    #[test]
    fn analysis_result_bounds_and_model_provenance() {
        let mut valid = result();
        assert!(valid.validate().is_ok());
        valid.actual_model = None;
        assert!(valid.validate().is_err());
        valid.outcome = ObservationAnalysisOutcome::Failed {
            code: "model_unavailable".into(),
        };
        assert!(valid.validate().is_ok());
        valid.candidate_json = Some("x".repeat(MAX_ANALYSIS_CANDIDATE_BYTES + 1));
        assert!(valid.validate().is_err());
        let mut grounded = result();
        grounded.outcome = ObservationAnalysisOutcome::Grounded {
            raw_answer: "Synthetic saved answer".into(),
            citations: vec!["https://example.org/source".into()],
            audit: json!({"refs":[{"path":"/messages/0/content","role":"answer_source"}]}),
        };
        assert!(grounded.validate().is_ok());
        if let ObservationAnalysisOutcome::Grounded { citations, .. } = &mut grounded.outcome {
            citations[0] = "https://secret@example.org/".into();
        }
        assert!(grounded.validate().is_err());
        let mut too_large = result();
        too_large.outcome = ObservationAnalysisOutcome::Grounded {
            raw_answer: "x".repeat(MAX_ANALYSIS_ANSWER_BYTES + 1),
            citations: vec![],
            audit: json!({"refs":[{}]}),
        };
        assert!(too_large.validate().is_err());
    }

    #[tokio::test]
    async fn analysis_time_checks_and_interruption_do_not_allow_reclaim() {
        let f = fixture().await;
        let hash = sha256_hex(b"synthetic-request");
        f.store
            .create(&f.scope, "key", &hash, f.request.clone(), f.now)
            .await
            .unwrap();
        assert!(
            f.store
                .finish(
                    &f.scope,
                    f.request.revision_id,
                    Uuid::new_v4(),
                    result(),
                    f.now
                )
                .await
                .is_err()
        );
        assert!(
            f.store
                .claim(
                    &f.scope,
                    f.request.revision_id,
                    f.now - Duration::seconds(1)
                )
                .await
                .is_err()
        );
        let claim = f
            .store
            .claim(&f.scope, f.request.revision_id, f.now)
            .await
            .unwrap()
            .unwrap();
        assert!(
            f.store
                .claim(&f.scope, f.request.revision_id, f.now + Duration::days(1))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .finish(
                    &f.scope,
                    f.request.revision_id,
                    claim.claim_token,
                    result(),
                    f.now - Duration::seconds(1)
                )
                .await
                .is_err()
        );
        let interrupted = ObservationAnalysisResult {
            actual_model: None,
            candidate_json: None,
            outcome: ObservationAnalysisOutcome::Failed {
                code: "interrupted".into(),
            },
            prompt_tokens: 0,
            completion_tokens: 0,
        };
        f.store
            .finish(
                &f.scope,
                f.request.revision_id,
                claim.claim_token,
                interrupted,
                f.now,
            )
            .await
            .unwrap();
        assert!(
            f.store
                .claim(&f.scope, f.request.revision_id, f.now)
                .await
                .unwrap()
                .is_none()
        );
    }
}
