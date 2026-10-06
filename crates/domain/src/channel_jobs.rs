//! Frozen external channel targets and server-owned execution evidence.
//! Browser observations are not content-generation or customer-supplied receipts.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    AppError, ChannelSecret, EvidenceRef, FrozenQuestionBinding, ProjectId, QuestionPurpose,
    QuestionReference, ReportEvidenceReference, ReportManifestKind, ReportManifestRef,
    ReportMeasurementStatus, ReportMeasurementTarget, ReportPublicationStatus,
    ReportPublicationTarget, TenantScope, sha256_hex,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelTargetInput {
    Publish {
        source_id: Uuid,
        source_version_id: Uuid,
        platform: String,
        account_id: Uuid,
        title: String,
        body: String,
        body_sha256: String,
    },
    /// Database-owned immutable distribution input; never accepted by public plan DTOs.
    GeneratedPublish {
        content_revision_id: Uuid,
        variant_id: Uuid,
        publication_intent_id: Uuid,
        distribution_target_id: Uuid,
        platform: String,
        account_id: Uuid,
        title: String,
        body: String,
        body_sha256: String,
        payload_hash: String,
        evidence: Vec<EvidenceRef>,
    },
    Measure {
        account_id: Uuid,
        provider: String,
        model: String,
        surface: String,
        search_mode: String,
        protocol_version: String,
        question_set_version: String,
        question: String,
        market: String,
        language: String,
        scheduled_at: DateTime<Utc>,
        sample_ordinal: u32,
        /// Absent on all historical ad-hoc samples. Never infer classification
        /// from the legacy free-text question-set version.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question_binding: Option<FrozenQuestionBinding>,
    },
}

impl ChannelTargetInput {
    pub fn account_id(&self) -> Uuid {
        match self {
            Self::Publish { account_id, .. }
            | Self::GeneratedPublish { account_id, .. }
            | Self::Measure { account_id, .. } => *account_id,
        }
    }

    pub fn is_publication(&self) -> bool {
        matches!(self, Self::Publish { .. } | Self::GeneratedPublish { .. })
    }

    pub fn comparison_key(&self) -> Option<String> {
        match self {
            Self::Measure {
                provider,
                model,
                surface,
                search_mode,
                protocol_version,
                question_set_version,
                market,
                language,
                question_binding,
                ..
            } => {
                let legacy = format!(
                    "{provider}|{model}|{surface}|{search_mode}|{protocol_version}|{question_set_version}|{market}|{language}"
                );
                Some(match question_binding {
                    None => legacy,
                    Some(binding) => format!(
                        "{legacy}|{}|{}",
                        match binding.purpose {
                            QuestionPurpose::Optimization => "optimization",
                            QuestionPurpose::FrozenEvaluation => "frozen_evaluation",
                        },
                        binding.split_policy_version
                    ),
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelTarget {
    pub target_id: Uuid,
    pub input: ChannelTargetInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPlan {
    pub plan_id: Uuid,
    pub project_id: ProjectId,
    pub cycle_id: Uuid,
    pub input_hash: String,
    pub revision: i32,
    pub created_at: DateTime<Utc>,
    /// Complete and sealed, including unsupported or presently unauthenticated targets.
    pub targets: Vec<ChannelTarget>,
}

/// Project-owned measurement, independent of knowledge or optimization cycles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandaloneMeasurementPlan {
    pub plan_id: Uuid,
    pub project_id: ProjectId,
    pub title: String,
    pub input_hash: String,
    pub revision: i32,
    pub created_at: DateTime<Utc>,
    pub targets: Vec<ChannelTarget>,
}

impl StandaloneMeasurementPlan {
    pub fn validate(&self, scope: &TenantScope) -> Result<(), AppError> {
        if scope.project_id != Some(self.project_id) {
            return Err(AppError::forbidden("measurement plan outside project"));
        }
        if self.title.trim().is_empty()
            || self.revision <= 0
            || self.targets.is_empty()
            || self
                .targets
                .iter()
                .any(|target| !matches!(target.input, ChannelTargetInput::Measure { .. }))
        {
            return Err(AppError::invalid_request(
                "invalid standalone measurement plan",
            ));
        }
        let mut ids = std::collections::HashSet::new();
        if self
            .targets
            .iter()
            .any(|target| !ids.insert(target.target_id))
        {
            return Err(AppError::invalid_request("duplicate measurement target"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelOutcomeStatus {
    Published,
    Verified,
    Unknown,
    Failed,
    LoginRequired,
    Unsupported,
    Observed,
    Refused,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelOutcome {
    pub status: ChannelOutcomeStatus,
    pub detail: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub raw_answer: Option<String>,
    pub citations: Vec<String>,
    pub public_url: Option<String>,
    pub screenshot_ref: Option<String>,
    pub connector_version: Option<String>,
    /// Original structured runner readback/search evidence; never client-supplied.
    pub runner_evidence: Vec<serde_json::Value>,
    /// Explicit provenance. Fixture responses must never claim a real observation.
    pub fixture: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelAttempt {
    pub attempt_id: Uuid,
    pub target_id: Uuid,
    pub claimed_at: DateTime<Utc>,
    pub outcome: Option<ChannelOutcome>,
    pub received_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelTargetView {
    pub target: ChannelTarget,
    pub attempts: Vec<ChannelAttempt>,
}

/// A repository-discovered, tenant-scoped target that has never been attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDispatchCandidate {
    pub scope: TenantScope,
    pub target_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCycleInputs {
    pub manifests: Vec<ReportManifestRef>,
    pub publications: Option<Vec<ReportPublicationTarget>>,
    pub measurements: Option<Vec<ReportMeasurementTarget>>,
}

/// Deliberately not a report, answer, evidence payload, or optimizer action.
/// Only these identities can be passed to a future optimization consumer;
/// that consumer must fetch any further facts through its own scoped gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationEligibleProjection {
    pub cycle_id: Uuid,
    pub observations: Vec<OptimizationEligibleObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationEligibleObservation {
    pub target_id: Uuid,
    pub observation_id: Uuid,
    pub question: QuestionReference,
    pub protocol_comparison_key: String,
}

/// Never projects held-out text, answers, citations, free-text details,
/// report findings, or mixed-purpose counts, including nested evidence.
/// Evidence must be a timely live official search observation whose frozen
/// target fields match the runner's original proof.
pub fn optimization_eligible_projection(
    scope: &TenantScope,
    plan: &ChannelPlan,
    attempts: &HashMap<Uuid, Vec<ChannelAttempt>>,
    as_of: DateTime<Utc>,
) -> Result<OptimizationEligibleProjection, AppError> {
    if scope.project_id != Some(plan.project_id) {
        return Err(AppError::forbidden(
            "optimization projection outside project scope",
        ));
    }
    let mut observations = Vec::new();
    for target in &plan.targets {
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
            question_binding: Some(binding),
        } = &target.input
        else {
            continue;
        };
        if binding.purpose != QuestionPurpose::Optimization
            || question_set_version != &binding.reference.question_set_version_id.to_string()
        {
            continue;
        }
        let Some(attempt) = attempts
            .get(&target.target_id)
            .and_then(|items| items.last())
        else {
            continue;
        };
        let Some(outcome) = &attempt.outcome else {
            continue;
        };
        if outcome.fixture
            || outcome.status != ChannelOutcomeStatus::Observed
            || outcome
                .raw_answer
                .as_ref()
                .is_none_or(|answer| answer.trim().is_empty())
            || outcome
                .connector_version
                .as_ref()
                .is_none_or(|version| version.is_empty() || version.starts_with("fixture"))
            || !attempt.received_at.is_some_and(|received| {
                attempt.claimed_at <= outcome.occurred_at
                    && *scheduled_at <= outcome.occurred_at
                    && outcome.occurred_at <= received
                    && received <= as_of
                    && *scheduled_at <= received
            })
        {
            continue;
        }
        let expected_question_hash = sha256_hex(question.as_bytes());
        let valid_proofs: Vec<_> = outcome
            .runner_evidence
            .iter()
            .filter(|proof| {
                proof.get("kind").and_then(serde_json::Value::as_str)
                    == Some("official_search_observation")
                    && proof.get("target_id").and_then(serde_json::Value::as_str)
                        == Some(target.target_id.to_string().as_str())
                    && proof.get("account_id").and_then(serde_json::Value::as_str)
                        == Some(account_id.to_string().as_str())
                    && proof
                        .get("question_sha256")
                        .and_then(serde_json::Value::as_str)
                        == Some(expected_question_hash.as_str())
                    && proof.get("provider").and_then(serde_json::Value::as_str)
                        == Some(provider.as_str())
                    && proof.get("model").and_then(serde_json::Value::as_str)
                        == Some(model.as_str())
                    && proof.get("surface").and_then(serde_json::Value::as_str)
                        == Some(surface.as_str())
                    && proof.get("search_mode").and_then(serde_json::Value::as_str)
                        == Some(search_mode.as_str())
                    && proof
                        .get("protocol_version")
                        .and_then(serde_json::Value::as_str)
                        == Some(protocol_version.as_str())
                    && proof
                        .get("question_set_version")
                        .and_then(serde_json::Value::as_str)
                        == Some(question_set_version.as_str())
                    && proof.get("market").and_then(serde_json::Value::as_str)
                        == Some(market.as_str())
                    && proof.get("language").and_then(serde_json::Value::as_str)
                        == Some(language.as_str())
                    && proof.get("scheduled_at").and_then(|value| {
                        serde_json::from_value::<DateTime<Utc>>(value.clone()).ok()
                    }) == Some(*scheduled_at)
                    && proof
                        .get("sample_ordinal")
                        .and_then(serde_json::Value::as_u64)
                        == Some(*sample_ordinal as u64)
                    && proof.get("provenance").and_then(serde_json::Value::as_str) == Some("live")
                    && proof.get("disposition").and_then(serde_json::Value::as_str)
                        == Some("observed")
                    && matches!(
                        proof
                            .get("schema_version")
                            .and_then(serde_json::Value::as_str),
                        Some("geo.measure.official_search.v1" | "geo.measure.official_search.v2")
                    )
                    && proof
                        .get("search_event")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(|event| {
                            event.get("kind").and_then(serde_json::Value::as_str)
                                == Some("official_search_event")
                                && event.get("provenance").and_then(serde_json::Value::as_str)
                                    == Some("live")
                                && matches!(
                                    event.get("source").and_then(serde_json::Value::as_str),
                                    Some("provider_search_event" | "provider_connect_stream")
                                )
                                && event
                                    .get("occurred_at")
                                    .or_else(|| event.get("observed_at"))
                                    .and_then(|value| {
                                        serde_json::from_value::<DateTime<Utc>>(value.clone()).ok()
                                    })
                                    .is_some_and(|at| {
                                        at >= attempt.claimed_at && at <= outcome.occurred_at
                                    })
                        })
                    && proof.get("raw_answer").and_then(serde_json::Value::as_str)
                        == outcome.raw_answer.as_deref()
                    && proof
                        .get("connector_version")
                        .and_then(serde_json::Value::as_str)
                        == outcome.connector_version.as_deref()
            })
            .collect();
        if valid_proofs.len() != 1 {
            continue;
        }
        observations.push(OptimizationEligibleObservation {
            target_id: target.target_id,
            observation_id: attempt.attempt_id,
            question: binding.reference,
            protocol_comparison_key: target.input.comparison_key().unwrap_or_default(),
        });
    }
    Ok(OptimizationEligibleProjection {
        cycle_id: plan.cycle_id,
        observations,
    })
}

#[async_trait]
pub trait ChannelJobRepository: Send + Sync {
    async fn replay_measurement_plan(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<StandaloneMeasurementPlan>, AppError>;
    async fn create_measurement_plan(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
        plan: StandaloneMeasurementPlan,
    ) -> Result<StandaloneMeasurementPlan, AppError>;
    async fn get_measurement_plan(
        &self,
        scope: &TenantScope,
        plan_id: Uuid,
    ) -> Result<Option<StandaloneMeasurementPlan>, AppError>;
    /// Ascending UUID keyset page, with an exclusive cursor.
    async fn list_measurement_plans(
        &self,
        scope: &TenantScope,
        after_plan_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<StandaloneMeasurementPlan>, AppError>;
    /// Ascending keyset page of plans containing an immutable target bound to
    /// optimization. Excluded-only plan IDs must never become AI cursors.
    async fn list_optimization_measurement_plans(
        &self,
        scope: &TenantScope,
        after_plan_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<StandaloneMeasurementPlan>, AppError>;
    /// Atomically insert durable targets and mark their outbox commands materialized.
    /// This does not reserve an account, claim an attempt, or send externally.
    async fn materialize_pending_commands(
        &self,
        after_command_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ChannelDispatchCandidate>, AppError>;
    /// Trusted in-memory equivalent; no cross-repository atomicity is implied.
    async fn insert_generated_target(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        command_id: Uuid,
        target: ChannelTarget,
    ) -> Result<ChannelTarget, AppError>;
    /// A reversible, operator-wide account reservation before browser startup.
    /// Expiry must be bounded; a stale holder cannot claim after its expiry.
    async fn reserve_account(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        reservation_id: Uuid,
        at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), AppError>;
    async fn release_account(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        reservation_id: Uuid,
    ) -> Result<(), AppError>;
    /// Atomically validates a live owned reservation and makes the irreversible
    /// one-shot claim. Once claimed, timeout/crash is unknown, not retryable.
    async fn claim_reserved(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        reservation_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<(ChannelTarget, ChannelAttempt), AppError>;
    /// Keyset page of due, never-attempted targets. `after_target_id` is an
    /// exclusive UUID cursor; callers must continue after the last returned ID
    /// even when dispatch defers that target.
    async fn scan_pending(
        &self,
        after_target_id: Option<Uuid>,
        as_of: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ChannelDispatchCandidate>, AppError>;
    async fn create_plan(
        &self,
        scope: &TenantScope,
        plan: ChannelPlan,
    ) -> Result<ChannelPlan, AppError>;
    async fn get_plan(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<Option<ChannelPlan>, AppError>;
    /// Atomic one-shot claim. A publication with an unresolved attempt cannot
    /// be claimed again; lookup must reconcile it first.
    async fn claim(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<(ChannelTarget, ChannelAttempt), AppError>;
    async fn finish(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        outcome: ChannelOutcome,
        received_at: DateTime<Utc>,
    ) -> Result<ChannelTargetView, AppError>;
    /// Save an opaque encrypted execution envelope on the original publication
    /// attempt before its outcome. Identical retries are safe; different bytes
    /// cannot replace an existing binding, including after the attempt finishes.
    async fn store_publication_binding(
        &self,
        _scope: &TenantScope,
        _target_id: Uuid,
        _attempt_id: Uuid,
        _binding: ChannelSecret,
    ) -> Result<(), AppError> {
        Err(AppError::capability_missing(
            "publication execution binding storage unavailable",
        ))
    }
    /// Internal-only read; this secret must never be serialized into HTTP views.
    async fn get_publication_binding(
        &self,
        _scope: &TenantScope,
        _target_id: Uuid,
        _attempt_id: Uuid,
    ) -> Result<Option<ChannelSecret>, AppError> {
        Err(AppError::capability_missing(
            "publication execution binding storage unavailable",
        ))
    }
    async fn get_target(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
    ) -> Result<ChannelTargetView, AppError>;
    async fn cycle_inputs(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        as_of: DateTime<Utc>,
    ) -> Result<ChannelCycleInputs, AppError>;
}

fn scope_key(scope: &TenantScope) -> Result<(Uuid, Uuid, Uuid), AppError> {
    Ok((
        scope.operator_id.as_uuid(),
        scope.tenant_id.as_uuid(),
        scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?
            .as_uuid(),
    ))
}

pub fn frozen_cycle_inputs(
    plan: &ChannelPlan,
    attempts: &HashMap<Uuid, Vec<ChannelAttempt>>,
    as_of: DateTime<Utc>,
) -> ChannelCycleInputs {
    let evidence = |target_id: Uuid, attempt: &ChannelAttempt| {
        attempt
            .outcome
            .as_ref()
            .map(|outcome| ReportEvidenceReference {
                evidence_id: attempt.attempt_id,
                kind: if matches!(
                    outcome.status,
                    ChannelOutcomeStatus::Observed | ChannelOutcomeStatus::Refused
                ) && !outcome.fixture
                {
                    "observation".into()
                } else if outcome.status == ChannelOutcomeStatus::Verified && !outcome.fixture {
                    "public_verification".into()
                } else if outcome.status == ChannelOutcomeStatus::Published && !outcome.fixture {
                    "publication_receipt".into()
                } else {
                    "channel_execution".into()
                },
                resource_id: target_id,
                resource_version: outcome.connector_version.clone(),
                occurred_at: Some(outcome.occurred_at),
                received_at: attempt.received_at,
                summary: outcome
                    .detail
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", outcome.status)),
            })
    };
    let mut publications = Vec::new();
    let mut measurements = Vec::new();
    for target in &plan.targets {
        let eligible: Vec<_> = attempts
            .get(&target.target_id)
            .into_iter()
            .flatten()
            .filter(|attempt| attempt.claimed_at <= as_of)
            .collect();
        let last = eligible.last().copied();
        let known = last.filter(|attempt| attempt.received_at.is_some_and(|at| at <= as_of));
        let outcome = known.and_then(|attempt| attempt.outcome.as_ref());
        let refs = known
            .and_then(|attempt| evidence(target.target_id, attempt))
            .into_iter()
            .collect();
        match &target.input {
            ChannelTargetInput::Publish { platform, .. }
            | ChannelTargetInput::GeneratedPublish { platform, .. } => {
                publications.push(ReportPublicationTarget {
                    target_id: target.target_id,
                    platform_id: platform.clone(),
                    status: match outcome.map(|value| value.status) {
                        Some(ChannelOutcomeStatus::Verified) => ReportPublicationStatus::Verified,
                        Some(ChannelOutcomeStatus::Published) => ReportPublicationStatus::Published,
                        Some(ChannelOutcomeStatus::Failed) => ReportPublicationStatus::Failed,
                        Some(
                            ChannelOutcomeStatus::LoginRequired | ChannelOutcomeStatus::Unsupported,
                        ) => ReportPublicationStatus::Deferred,
                        Some(ChannelOutcomeStatus::Unknown) => ReportPublicationStatus::Unknown,
                        _ if last.is_some() => ReportPublicationStatus::Unknown,
                        _ => ReportPublicationStatus::Planned,
                    },
                    reason: outcome.and_then(|o| o.detail.clone()),
                    evidence: refs,
                })
            }
            ChannelTargetInput::Measure {
                scheduled_at,
                question_binding,
                ..
            } => measurements.push(ReportMeasurementTarget {
                target_id: target.target_id,
                comparison_key: target.input.comparison_key().unwrap_or_default(),
                question_binding: question_binding.clone(),
                scheduled_at: *scheduled_at,
                status: match outcome.map(|value| value.status) {
                    Some(ChannelOutcomeStatus::Observed) => ReportMeasurementStatus::Observed,
                    Some(ChannelOutcomeStatus::Refused) => ReportMeasurementStatus::Refused,
                    Some(
                        ChannelOutcomeStatus::Missing
                        | ChannelOutcomeStatus::Unsupported
                        | ChannelOutcomeStatus::LoginRequired,
                    ) => ReportMeasurementStatus::Missing,
                    _ if last.is_some() => ReportMeasurementStatus::Missing,
                    _ => ReportMeasurementStatus::Pending,
                },
                missing_reason: outcome.and_then(|o| o.detail.clone()),
                evidence: refs,
            }),
        }
    }
    let pub_count = publications.len() as u64;
    let measure_count = measurements.len() as u64;
    ChannelCycleInputs {
        manifests: vec![
            ReportManifestRef {
                kind: ReportManifestKind::Distribution,
                manifest_id: plan.plan_id,
                revision: plan.revision,
                sealed: true,
                expected_count: Some(pub_count),
            },
            ReportManifestRef {
                kind: ReportManifestKind::Measurement,
                manifest_id: plan.plan_id,
                revision: plan.revision,
                sealed: true,
                expected_count: Some(measure_count),
            },
        ],
        publications: Some(publications),
        measurements: Some(measurements),
    }
}

#[derive(Default, Clone)]
pub struct MemoryChannelJobRepository(
    Arc<Mutex<HashMap<ChannelScopeKey, MemoryCycle>>>,
    Arc<Mutex<HashMap<AccountReservationKey, AccountReservation>>>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ChannelOwner {
    Cycle(Uuid),
    Measurement(Uuid),
}

type ChannelScopeKey = (Uuid, Uuid, Uuid, ChannelOwner);
type AccountReservationKey = (Uuid, Uuid);
type AccountReservation = (Uuid, DateTime<Utc>);

#[derive(Default)]
struct MemoryCycle {
    plan: Option<ChannelPlan>,
    measurement: Option<StandaloneMeasurementPlan>,
    measurement_request: Option<(String, String)>,
    generated: HashMap<Uuid, ChannelTarget>,
    attempts: HashMap<Uuid, Vec<ChannelAttempt>>,
    publication_bindings: HashMap<Uuid, ChannelSecret>,
}

impl MemoryCycle {
    fn target(&self, id: Uuid) -> Option<&ChannelTarget> {
        self.generated.get(&id).or_else(|| {
            self.plan
                .as_ref()
                .and_then(|plan| plan.targets.iter().find(|target| target.target_id == id))
                .or_else(|| {
                    self.measurement
                        .as_ref()
                        .and_then(|plan| plan.targets.iter().find(|target| target.target_id == id))
                })
        })
    }

    fn targets(&self) -> impl Iterator<Item = &ChannelTarget> {
        self.generated
            .values()
            .chain(self.plan.iter().flat_map(|plan| plan.targets.iter()))
            .chain(self.measurement.iter().flat_map(|plan| plan.targets.iter()))
    }
}

#[async_trait]
impl ChannelJobRepository for MemoryChannelJobRepository {
    async fn replay_measurement_plan(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<StandaloneMeasurementPlan>, AppError> {
        let identity = scope_key(scope)?;
        let all = self.0.lock().await;
        for ((o, t, p, _), entry) in all.iter() {
            if (*o, *t, *p) == identity
                && let Some((existing_key, hash)) = &entry.measurement_request
                && existing_key == key
            {
                return if hash == request_hash {
                    Ok(entry.measurement.clone())
                } else {
                    Err(AppError::conflict("measurement idempotency key differs"))
                };
            }
        }
        Ok(None)
    }
    async fn create_measurement_plan(
        &self,
        scope: &TenantScope,
        key: &str,
        request_hash: &str,
        plan: StandaloneMeasurementPlan,
    ) -> Result<StandaloneMeasurementPlan, AppError> {
        plan.validate(scope)?;
        if key.trim().is_empty() || request_hash.is_empty() {
            return Err(AppError::invalid_request(
                "measurement idempotency identity required",
            ));
        }
        let (o, t, p) = scope_key(scope)?;
        let mut all = self.0.lock().await;
        for ((eo, et, ep, _), entry) in all.iter() {
            if (*eo, *et, *ep) == (o, t, p)
                && let Some((existing_key, hash)) = &entry.measurement_request
                && existing_key == key
            {
                return if hash == request_hash {
                    Ok(entry
                        .measurement
                        .as_ref()
                        .expect("measurement owner")
                        .clone())
                } else {
                    Err(AppError::conflict("measurement idempotency key differs"))
                };
            }
        }
        if all.contains_key(&(o, t, p, ChannelOwner::Measurement(plan.plan_id)))
            || all.values().any(|entry| {
                plan.targets
                    .iter()
                    .any(|target| entry.target(target.target_id).is_some())
            })
        {
            return Err(AppError::conflict(
                "measurement plan or target already exists",
            ));
        }
        all.insert(
            (o, t, p, ChannelOwner::Measurement(plan.plan_id)),
            MemoryCycle {
                measurement: Some(plan.clone()),
                measurement_request: Some((key.to_owned(), request_hash.to_owned())),
                ..Default::default()
            },
        );
        Ok(plan)
    }

    async fn get_measurement_plan(
        &self,
        scope: &TenantScope,
        plan_id: Uuid,
    ) -> Result<Option<StandaloneMeasurementPlan>, AppError> {
        let (o, t, p) = scope_key(scope)?;
        Ok(self
            .0
            .lock()
            .await
            .get(&(o, t, p, ChannelOwner::Measurement(plan_id)))
            .and_then(|entry| entry.measurement.clone()))
    }

    async fn list_measurement_plans(
        &self,
        scope: &TenantScope,
        after_plan_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<StandaloneMeasurementPlan>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid measurement page size"));
        }
        let key = scope_key(scope)?;
        let all = self.0.lock().await;
        let mut plans: Vec<_> = all
            .iter()
            .filter(|((o, t, p, _), _)| (*o, *t, *p) == key)
            .filter_map(|(_, entry)| entry.measurement.as_ref())
            .filter(|plan| after_plan_id.is_none_or(|after| plan.plan_id > after))
            .cloned()
            .collect();
        plans.sort_unstable_by_key(|plan| plan.plan_id);
        plans.truncate(limit);
        Ok(plans)
    }
    async fn list_optimization_measurement_plans(
        &self,
        scope: &TenantScope,
        after_plan_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<StandaloneMeasurementPlan>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid measurement page size"));
        }
        let key = scope_key(scope)?;
        let all = self.0.lock().await;
        let mut plans: Vec<_> = all
            .iter()
            .filter(|((o, t, p, _), _)| (*o, *t, *p) == key)
            .filter_map(|(_, entry)| entry.measurement.as_ref())
            .filter(|plan| after_plan_id.is_none_or(|after| plan.plan_id > after))
            .filter(|plan| {
                plan.targets.iter().any(|target| {
                    matches!(
                        &target.input,
                        ChannelTargetInput::Measure {
                            question_binding: Some(FrozenQuestionBinding {
                                purpose: QuestionPurpose::Optimization,
                                ..
                            }),
                            ..
                        }
                    )
                })
            })
            .cloned()
            .collect();
        plans.sort_unstable_by_key(|plan| plan.plan_id);
        plans.truncate(limit);
        Ok(plans)
    }
    async fn materialize_pending_commands(
        &self,
        _after_command_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ChannelDispatchCandidate>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid command page size"));
        }
        // Independent memory stores cannot implement a cross-repository transaction.
        Ok(vec![])
    }

    async fn insert_generated_target(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        command_id: Uuid,
        target: ChannelTarget,
    ) -> Result<ChannelTarget, AppError> {
        if target.target_id != command_id
            || !matches!(target.input, ChannelTargetInput::GeneratedPublish { .. })
        {
            return Err(AppError::invalid_request(
                "invalid generated command target",
            ));
        }
        let (o, t, p) = scope_key(scope)?;
        let mut all = self.0.lock().await;
        if let Some(existing) = all.iter().find_map(|(&(eo, et, ep, _), cycle)| {
            (eo == o && et == t && ep == p)
                .then(|| cycle.target(command_id))
                .flatten()
        }) {
            return if existing == &target {
                Ok(existing.clone())
            } else {
                Err(AppError::conflict("generated command target differs"))
            };
        }
        all.entry((o, t, p, ChannelOwner::Cycle(cycle_id)))
            .or_default()
            .generated
            .insert(command_id, target.clone());
        Ok(target)
    }
    async fn reserve_account(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        reservation_id: Uuid,
        at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        scope_key(scope)?;
        if expires_at <= at || expires_at - at > chrono::Duration::minutes(5) {
            return Err(AppError::invalid_request(
                "invalid account reservation duration",
            ));
        }
        let mut reservations = self.1.lock().await;
        let key = (scope.operator_id.as_uuid(), account_id);
        if reservations
            .get(&key)
            .is_some_and(|(_, expiry)| *expiry > at)
        {
            return Err(AppError::conflict("channel account preflight busy"));
        }
        reservations.insert(key, (reservation_id, expires_at));
        Ok(())
    }

    async fn release_account(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        reservation_id: Uuid,
    ) -> Result<(), AppError> {
        scope_key(scope)?;
        let mut reservations = self.1.lock().await;
        let key = (scope.operator_id.as_uuid(), account_id);
        if reservations
            .get(&key)
            .is_some_and(|(owner, _)| *owner == reservation_id)
        {
            reservations.remove(&key);
        }
        Ok(())
    }

    async fn claim_reserved(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        reservation_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<(ChannelTarget, ChannelAttempt), AppError> {
        let reservations = self.1.lock().await;
        let target = self.get_target(scope, target_id).await?.target;
        if !reservations
            .get(&(scope.operator_id.as_uuid(), target.input.account_id()))
            .is_some_and(|(owner, expiry)| *owner == reservation_id && *expiry > at)
        {
            return Err(AppError::conflict("account preflight reservation expired"));
        }
        self.claim(scope, target_id, attempt_id, at).await
    }
    async fn scan_pending(
        &self,
        after_target_id: Option<Uuid>,
        as_of: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ChannelDispatchCandidate>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request(
                "invalid channel dispatch page size",
            ));
        }
        let all = self.0.lock().await;
        let mut candidates = all
            .iter()
            .flat_map(|(&(operator, tenant, project, _), cycle)| {
                cycle.targets().filter_map(move |target| {
                        if after_target_id.is_some_and(|after| target.target_id <= after)
                            || cycle.attempts.contains_key(&target.target_id)
                            || matches!(&target.input, ChannelTargetInput::Measure { scheduled_at, .. } if *scheduled_at > as_of)
                        {
                            return None;
                        }
                        Some(ChannelDispatchCandidate {
                            scope: TenantScope::new(
                                crate::OperatorId::new(operator),
                                crate::TenantId::new(tenant),
                                Some(ProjectId::new(project)),
                            ),
                            target_id: target.target_id,
                        })
                    })
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|candidate| candidate.target_id);
        candidates.truncate(limit);
        Ok(candidates)
    }
    async fn create_plan(
        &self,
        scope: &TenantScope,
        plan: ChannelPlan,
    ) -> Result<ChannelPlan, AppError> {
        if scope.project_id != Some(plan.project_id) {
            return Err(AppError::forbidden("plan outside project"));
        }
        if plan
            .targets
            .iter()
            .any(|target| matches!(target.input, ChannelTargetInput::GeneratedPublish { .. }))
        {
            return Err(AppError::invalid_request(
                "generated publication cannot be frozen into a legacy plan",
            ));
        }
        let mut all = self.0.lock().await;
        let cycle = all
            .entry((
                scope_key(scope)?.0,
                scope_key(scope)?.1,
                scope_key(scope)?.2,
                ChannelOwner::Cycle(plan.cycle_id),
            ))
            .or_default();
        if let Some(existing) = &cycle.plan {
            if existing.input_hash == plan.input_hash {
                return Ok(existing.clone());
            }
            return Err(AppError::conflict("channel plan already frozen"));
        }
        cycle.plan = Some(plan.clone());
        Ok(plan)
    }

    async fn get_plan(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<Option<ChannelPlan>, AppError> {
        let (o, t, p) = scope_key(scope)?;
        Ok(self
            .0
            .lock()
            .await
            .get(&(o, t, p, ChannelOwner::Cycle(cycle_id)))
            .and_then(|cycle| cycle.plan.clone()))
    }

    async fn claim(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<(ChannelTarget, ChannelAttempt), AppError> {
        let mut all = self.0.lock().await;
        let key = scope_key(scope)?;
        let pending_target = all
            .iter()
            .filter(|((o, t, p, _), _)| (*o, *t, *p) == key)
            .flat_map(|(_, cycle)| cycle.targets())
            .find(|target| target.target_id == target_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("target not found"))?;
        if pending_target.input.is_publication()
            && all
                .iter()
                // A pool account can serve several tenants/projects under one
                // operator; its write lease is account-wide, not project-wide.
                .filter(|((o, _, _, _), _)| *o == key.0)
                .any(|(_, cycle)| {
                    cycle.targets().any(|target| {
                        target.input.is_publication()
                            && target.input.account_id() == pending_target.input.account_id()
                            && cycle
                                .attempts
                                .get(&target.target_id)
                                .is_some_and(|attempts| {
                                    attempts.iter().any(|attempt| attempt.received_at.is_none())
                                })
                    })
                })
        {
            return Err(AppError::conflict(
                "channel account already has a publication in flight",
            ));
        }
        let cycle = all
            .iter_mut()
            .find(|((o, t, p, _), cycle)| (*o, *t, *p) == key && cycle.target(target_id).is_some())
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let target = cycle
            .target(target_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let attempts = cycle.attempts.entry(target_id).or_default();
        if !attempts.is_empty() {
            return Err(AppError::conflict(
                "target already attempted; inspect or reconcile existing outcome",
            ));
        }
        let attempt = ChannelAttempt {
            attempt_id,
            target_id,
            claimed_at: at,
            outcome: None,
            received_at: None,
        };
        attempts.push(attempt.clone());
        Ok((target, attempt))
    }

    async fn finish(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        outcome: ChannelOutcome,
        received_at: DateTime<Utc>,
    ) -> Result<ChannelTargetView, AppError> {
        let mut all = self.0.lock().await;
        let key = scope_key(scope)?;
        let cycle = all
            .iter_mut()
            .find(|((o, t, p, _), cycle)| (*o, *t, *p) == key && cycle.target(target_id).is_some())
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let target = cycle
            .target(target_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let attempts = cycle
            .attempts
            .get_mut(&target_id)
            .ok_or_else(|| AppError::not_found("attempt not found"))?;
        let attempt = attempts
            .iter_mut()
            .find(|attempt| attempt.attempt_id == attempt_id)
            .ok_or_else(|| AppError::not_found("attempt not found"))?;
        if let Some(previous) = &attempt.outcome {
            if previous != &outcome {
                return Err(AppError::conflict("attempt outcome already recorded"));
            }
        } else {
            attempt.outcome = Some(outcome);
            attempt.received_at = Some(received_at);
        }
        Ok(ChannelTargetView {
            target,
            attempts: attempts.clone(),
        })
    }

    async fn store_publication_binding(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        binding: ChannelSecret,
    ) -> Result<(), AppError> {
        if binding.encrypted_bytes().is_empty() {
            return Err(AppError::invalid_request(
                "empty encrypted publication binding",
            ));
        }
        let mut all = self.0.lock().await;
        let key = scope_key(scope)?;
        let cycle = all
            .iter_mut()
            .find(|((o, t, p, _), cycle)| (*o, *t, *p) == key && cycle.target(target_id).is_some())
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        if !cycle
            .target(target_id)
            .is_some_and(|target| target.input.is_publication())
        {
            return Err(AppError::invalid_request(
                "binding requires a publication target",
            ));
        }
        let attempt = cycle
            .attempts
            .get(&target_id)
            .and_then(|attempts| {
                attempts
                    .iter()
                    .find(|attempt| attempt.attempt_id == attempt_id)
            })
            .ok_or_else(|| AppError::not_found("attempt not found"))?;
        if let Some(existing) = cycle.publication_bindings.get(&attempt_id) {
            return if existing.encrypted_bytes() == binding.encrypted_bytes() {
                Ok(())
            } else {
                Err(AppError::conflict("publication binding already recorded"))
            };
        }
        if attempt.received_at.is_some() {
            return Err(AppError::conflict("publication attempt already finished"));
        }
        cycle.publication_bindings.insert(attempt_id, binding);
        Ok(())
    }

    async fn get_publication_binding(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<ChannelSecret>, AppError> {
        let all = self.0.lock().await;
        let key = scope_key(scope)?;
        let cycle = all
            .iter()
            .find(|((o, t, p, _), cycle)| (*o, *t, *p) == key && cycle.target(target_id).is_some())
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        if !cycle
            .target(target_id)
            .is_some_and(|target| target.input.is_publication())
        {
            return Err(AppError::invalid_request(
                "binding requires a publication target",
            ));
        }
        if !cycle.attempts.get(&target_id).is_some_and(|attempts| {
            attempts
                .iter()
                .any(|attempt| attempt.attempt_id == attempt_id)
        }) {
            return Err(AppError::not_found("attempt not found"));
        }
        Ok(cycle.publication_bindings.get(&attempt_id).cloned())
    }

    async fn get_target(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
    ) -> Result<ChannelTargetView, AppError> {
        let all = self.0.lock().await;
        let key = scope_key(scope)?;
        let cycle = all
            .iter()
            .find(|((o, t, p, _), cycle)| (*o, *t, *p) == key && cycle.target(target_id).is_some())
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let target = cycle
            .target(target_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("target not found"))?;
        Ok(ChannelTargetView {
            target,
            attempts: cycle.attempts.get(&target_id).cloned().unwrap_or_default(),
        })
    }

    async fn cycle_inputs(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
        as_of: DateTime<Utc>,
    ) -> Result<ChannelCycleInputs, AppError> {
        let all = self.0.lock().await;
        let (o, t, p) = scope_key(scope)?;
        let cycle = all.get(&(o, t, p, ChannelOwner::Cycle(cycle_id)));
        Ok(match cycle.and_then(|cycle| cycle.plan.as_ref()) {
            Some(plan) if plan.created_at <= as_of => {
                frozen_cycle_inputs(plan, &cycle.expect("matched cycle").attempts, as_of)
            }
            _ => ChannelCycleInputs {
                manifests: vec![],
                publications: None,
                measurements: None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OperatorId, QuestionReference, TenantId};

    #[tokio::test]
    async fn standalone_measurements_replay_scope_dispatch_and_never_enter_cycle_reports() {
        let repo = MemoryChannelJobRepository::default();
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let now = Utc::now();
        let plan = StandaloneMeasurementPlan {
            plan_id: Uuid::new_v4(),
            project_id: scope.project_id.unwrap(),
            title: "General topic".into(),
            input_hash: "frozen".into(),
            revision: 1,
            created_at: now,
            targets: vec![ChannelTarget {
                target_id: Uuid::new_v4(),
                input: ChannelTargetInput::Measure {
                    account_id: Uuid::new_v4(),
                    provider: "fixture".into(),
                    model: "fixed".into(),
                    surface: "consumer_web".into(),
                    search_mode: "web_search".into(),
                    protocol_version: "v1".into(),
                    question_set_version: "ad-hoc".into(),
                    question: "How are eclipses predicted?".into(),
                    market: "global".into(),
                    language: "en".into(),
                    scheduled_at: now,
                    sample_ordinal: 0,
                    question_binding: None,
                },
            }],
        };
        assert_eq!(
            repo.create_measurement_plan(&scope, "key", "request", plan.clone())
                .await
                .unwrap(),
            plan
        );
        let mut retry = plan.clone();
        retry.plan_id = Uuid::new_v4();
        assert_eq!(
            repo.create_measurement_plan(&scope, "key", "request", retry.clone())
                .await
                .unwrap(),
            plan
        );
        assert_eq!(
            repo.create_measurement_plan(&scope, "key", "different", retry)
                .await
                .unwrap_err()
                .code,
            crate::ErrorCode::Conflict
        );
        assert_eq!(
            repo.replay_measurement_plan(&scope, "key", "request")
                .await
                .unwrap(),
            Some(plan.clone())
        );
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        assert!(
            repo.get_measurement_plan(&other, plan.plan_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.replay_measurement_plan(&other, "key", "request")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repo.list_measurement_plans(&scope, None, 1).await.unwrap(),
            vec![plan.clone()]
        );
        assert!(
            repo.list_measurement_plans(&scope, Some(plan.plan_id), 1)
                .await
                .unwrap()
                .is_empty()
        );
        // An owner UUID collision cannot make standalone measurements appear in a cycle.
        assert!(
            repo.cycle_inputs(&scope, plan.plan_id, now)
                .await
                .unwrap()
                .measurements
                .is_none()
        );
        assert_eq!(repo.scan_pending(None, now, 10).await.unwrap().len(), 1);
        let reservation = Uuid::new_v4();
        repo.reserve_account(
            &scope,
            plan.targets[0].input.account_id(),
            reservation,
            now,
            now + chrono::Duration::seconds(30),
        )
        .await
        .unwrap();
        let attempt = Uuid::new_v4();
        repo.claim_reserved(&scope, plan.targets[0].target_id, attempt, reservation, now)
            .await
            .unwrap();
        assert!(repo.scan_pending(None, now, 10).await.unwrap().is_empty());
        assert_eq!(
            repo.clone()
                .get_target(&scope, plan.targets[0].target_id)
                .await
                .unwrap()
                .attempts[0]
                .attempt_id,
            attempt
        );
        let mut invalid = plan;
        invalid.targets.clear();
        assert!(
            repo.create_measurement_plan(&scope, "invalid", "request", invalid)
                .await
                .is_err()
        );
    }

    #[test]
    fn legacy_measure_serialization_is_byte_stable_and_does_not_acquire_purpose() {
        let old = r#"{"kind":"measure","account_id":"00000000-0000-0000-0000-000000000001","provider":"kimi","model":"fixed","surface":"consumer_web","search_mode":"web_search","protocol_version":"v1","question_set_version":"free-text-version","question":"Question?","market":"CN","language":"en","scheduled_at":"2026-09-25T00:00:00Z","sample_ordinal":0}"#;
        let input: ChannelTargetInput = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&input).unwrap(), old);
        assert_eq!(
            input.comparison_key().as_deref(),
            Some("kimi|fixed|consumer_web|web_search|v1|free-text-version|CN|en")
        );
        let mut changed = input.clone();
        if let ChannelTargetInput::Measure {
            question_binding, ..
        } = &mut changed
        {
            *question_binding = Some(FrozenQuestionBinding {
                reference: QuestionReference {
                    question_set_id: Uuid::new_v4(),
                    question_set_version_id: Uuid::new_v4(),
                    question_id: Uuid::new_v4(),
                    question_revision_id: Uuid::new_v4(),
                },
                purpose: QuestionPurpose::Optimization,
                split_policy_version: "project_registry_nfkc_v1".into(),
            });
        }
        assert_ne!(
            serde_json::to_vec(&input).unwrap(),
            serde_json::to_vec(&changed).unwrap()
        );
    }

    #[test]
    fn optimizer_projection_excludes_heldout_unclassified_fixture_and_nested_canary() {
        let now = Utc::now();
        let cycle = Uuid::new_v4();
        let version = Uuid::new_v4();
        let make = |purpose: Option<QuestionPurpose>, question: &str| {
            let target_id = Uuid::new_v4();
            let binding = purpose.map(|purpose| FrozenQuestionBinding {
                reference: QuestionReference {
                    question_set_id: Uuid::new_v4(),
                    question_set_version_id: version,
                    question_id: Uuid::new_v4(),
                    question_revision_id: Uuid::new_v4(),
                },
                purpose,
                split_policy_version: "project_registry_nfkc_v1".into(),
            });
            ChannelTarget {
                target_id,
                input: ChannelTargetInput::Measure {
                    account_id: Uuid::new_v4(),
                    provider: "kimi".into(),
                    model: "fixed".into(),
                    surface: "consumer_web".into(),
                    search_mode: "web_search".into(),
                    protocol_version: "v1".into(),
                    question_set_version: if binding.is_some() {
                        version.to_string()
                    } else {
                        "legacy".into()
                    },
                    question: question.into(),
                    market: "global".into(),
                    language: "en".into(),
                    scheduled_at: now,
                    sample_ordinal: 0,
                    question_binding: binding,
                },
            }
        };
        let targets = vec![
            make(Some(QuestionPurpose::Optimization), "eligible"),
            make(
                Some(QuestionPurpose::FrozenEvaluation),
                "HELDOUT_QUESTION_CANARY",
            ),
            make(None, "UNCLASSIFIED_QUESTION_CANARY"),
            make(
                Some(QuestionPurpose::Optimization),
                "FIXTURE_QUESTION_CANARY",
            ),
            make(
                Some(QuestionPurpose::Optimization),
                "INVALID_EVIDENCE_CANARY",
            ),
        ];
        let plan = ChannelPlan {
            plan_id: Uuid::new_v4(),
            project_id: ProjectId::new(Uuid::new_v4()),
            cycle_id: cycle,
            input_hash: "test".into(),
            revision: 1,
            created_at: now,
            targets: targets.clone(),
        };
        let mut attempts = HashMap::new();
        for (index, target) in targets.iter().enumerate() {
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
            let mut proof = serde_json::json!({
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
                "connector_version":"official_search_verified.v1",
                "provenance":"live",
                "disposition":"observed",
                "raw_answer": if index == 1 { "HELDOUT_ANSWER_CANARY" } else { "observed answer" },
                "search_event":{"kind":"official_search_event","provenance":"live","source":"provider_search_event","occurred_at":now},
                "nested_canary":"HELDOUT_NESTED_EVIDENCE_CANARY"
            });
            if index == 4 {
                proof["question_sha256"] = serde_json::json!("tampered");
            }
            attempts.insert(
                target.target_id,
                vec![ChannelAttempt {
                    attempt_id: Uuid::new_v4(),
                    target_id: target.target_id,
                    claimed_at: now,
                    outcome: Some(ChannelOutcome {
                        status: ChannelOutcomeStatus::Observed,
                        detail: Some("HELDOUT_DETAIL_CANARY".into()),
                        occurred_at: now,
                        raw_answer: Some(
                            if index == 1 {
                                "HELDOUT_ANSWER_CANARY"
                            } else {
                                "observed answer"
                            }
                            .into(),
                        ),
                        citations: vec!["HELDOUT_CITATION_CANARY".into()],
                        public_url: None,
                        screenshot_ref: Some("HELDOUT_SCREENSHOT_CANARY".into()),
                        connector_version: Some("official_search_verified.v1".into()),
                        runner_evidence: vec![proof],
                        fixture: index == 3,
                    }),
                    received_at: Some(now),
                }],
            );
        }
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(plan.project_id),
        );
        let projected = optimization_eligible_projection(&scope, &plan, &attempts, now).unwrap();
        let foreign = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        assert!(optimization_eligible_projection(&foreign, &plan, &attempts, now).is_err());
        assert_eq!(projected.observations.len(), 1);
        assert_eq!(projected.observations[0].target_id, targets[0].target_id);
        let serialized = serde_json::to_string(&projected).unwrap();
        for canary in ["HELDOUT", "CANARY", "observed answer", "eligible"] {
            assert!(!serialized.contains(canary));
        }
    }

    #[tokio::test]
    async fn publication_binding_is_scoped_write_once_and_survives_outcome() {
        let repo = MemoryChannelJobRepository::default();
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let foreign = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let measurement = Uuid::new_v4();
        let now = Utc::now();
        repo.create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id: Uuid::new_v4(),
                input_hash: "fixture".into(),
                revision: 1,
                created_at: now,
                targets: vec![
                    ChannelTarget {
                        target_id: first,
                        input: ChannelTargetInput::Publish {
                            source_id: Uuid::new_v4(),
                            source_version_id: Uuid::new_v4(),
                            platform: "fixture".into(),
                            account_id: Uuid::new_v4(),
                            title: "title".into(),
                            body: "body".into(),
                            body_sha256: "fixture".into(),
                        },
                    },
                    ChannelTarget {
                        target_id: second,
                        input: ChannelTargetInput::Publish {
                            source_id: Uuid::new_v4(),
                            source_version_id: Uuid::new_v4(),
                            platform: "fixture".into(),
                            account_id: Uuid::new_v4(),
                            title: "title".into(),
                            body: "body".into(),
                            body_sha256: "fixture".into(),
                        },
                    },
                    ChannelTarget {
                        target_id: measurement,
                        input: ChannelTargetInput::Measure {
                            account_id: Uuid::new_v4(),
                            provider: "fixture".into(),
                            model: "fixture".into(),
                            surface: "web".into(),
                            search_mode: "off".into(),
                            protocol_version: "v1".into(),
                            question_set_version: "v1".into(),
                            question: "question".into(),
                            market: "generic".into(),
                            language: "en".into(),
                            scheduled_at: now,
                            sample_ordinal: 0,
                            question_binding: None,
                        },
                    },
                ],
            },
        )
        .await
        .unwrap();
        let attempt = Uuid::new_v4();
        let measured = Uuid::new_v4();
        repo.claim(&scope, first, attempt, now).await.unwrap();
        repo.claim(&scope, measurement, measured, now)
            .await
            .unwrap();
        assert!(
            repo.get_publication_binding(&scope, first, attempt)
                .await
                .unwrap()
                .is_none()
        );
        let envelope = ChannelSecret::new(vec![1, 2, 3]);
        repo.store_publication_binding(&scope, first, attempt, envelope.clone())
            .await
            .unwrap();
        repo.store_publication_binding(&scope, first, attempt, envelope.clone())
            .await
            .unwrap();
        assert_eq!(
            repo.store_publication_binding(&scope, first, attempt, ChannelSecret::new(vec![4]))
                .await
                .unwrap_err()
                .code,
            crate::ErrorCode::Conflict
        );
        for (tested_scope, target) in [(&foreign, first), (&scope, second)] {
            assert_eq!(
                repo.get_publication_binding(tested_scope, target, attempt)
                    .await
                    .err()
                    .unwrap()
                    .code,
                crate::ErrorCode::NotFound
            );
        }
        assert_eq!(
            repo.store_publication_binding(
                &scope,
                measurement,
                measured,
                ChannelSecret::new(vec![4])
            )
            .await
            .unwrap_err()
            .code,
            crate::ErrorCode::InvalidRequest
        );
        repo.finish(
            &scope,
            first,
            attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Unknown,
                detail: None,
                occurred_at: now,
                raw_answer: None,
                citations: vec![],
                public_url: None,
                screenshot_ref: None,
                connector_version: None,
                runner_evidence: vec![],
                fixture: true,
            },
            now,
        )
        .await
        .unwrap();
        assert_eq!(
            repo.get_publication_binding(&scope, first, attempt)
                .await
                .unwrap()
                .unwrap()
                .encrypted_bytes(),
            envelope.encrypted_bytes()
        );
        repo.store_publication_binding(&scope, first, attempt, envelope)
            .await
            .unwrap();
        let second_attempt = Uuid::new_v4();
        repo.claim(&scope, second, second_attempt, now)
            .await
            .unwrap();
        repo.finish(
            &scope,
            second,
            second_attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Unknown,
                detail: None,
                occurred_at: now,
                raw_answer: None,
                citations: vec![],
                public_url: None,
                screenshot_ref: None,
                connector_version: None,
                runner_evidence: vec![],
                fixture: true,
            },
            now,
        )
        .await
        .unwrap();
        assert_eq!(
            repo.store_publication_binding(
                &scope,
                second,
                second_attempt,
                ChannelSecret::new(vec![9])
            )
            .await
            .unwrap_err()
            .code,
            crate::ErrorCode::Conflict
        );
    }

    #[tokio::test]
    async fn generated_memory_target_is_idempotent_and_uses_publication_account_lease() {
        let repo = MemoryChannelJobRepository::default();
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let cycle = Uuid::new_v4();
        let account = Uuid::new_v4();
        let command = Uuid::new_v4();
        let target = ChannelTarget {
            target_id: command,
            input: ChannelTargetInput::GeneratedPublish {
                content_revision_id: Uuid::new_v4(),
                variant_id: Uuid::new_v4(),
                publication_intent_id: Uuid::new_v4(),
                distribution_target_id: Uuid::new_v4(),
                platform: "zhihu".into(),
                account_id: account,
                title: "title".into(),
                body: "body".into(),
                body_sha256: "hash".into(),
                payload_hash: "payload".into(),
                evidence: vec![],
            },
        };
        let measurement = ChannelPlan {
            plan_id: Uuid::new_v4(),
            project_id: scope.project_id.unwrap(),
            cycle_id: cycle,
            input_hash: "separate".into(),
            revision: 1,
            created_at: Utc::now(),
            targets: vec![],
        };
        repo.create_plan(&scope, measurement.clone()).await.unwrap();
        assert_eq!(
            repo.insert_generated_target(&scope, cycle, command, target.clone())
                .await
                .unwrap(),
            target
        );
        assert_eq!(
            repo.insert_generated_target(&scope, cycle, command, target.clone())
                .await
                .unwrap(),
            target
        );
        assert_eq!(
            repo.get_plan(&scope, cycle).await.unwrap(),
            Some(measurement)
        );
        assert_eq!(
            repo.scan_pending(None, Utc::now(), 10).await.unwrap().len(),
            1
        );
        let now = Utc::now();
        let reservation = Uuid::new_v4();
        repo.reserve_account(
            &scope,
            account,
            reservation,
            now,
            now + chrono::Duration::seconds(30),
        )
        .await
        .unwrap();
        let attempt = Uuid::new_v4();
        repo.claim_reserved(&scope, command, attempt, reservation, now)
            .await
            .unwrap();
        assert!(repo.scan_pending(None, now, 10).await.unwrap().is_empty());
        assert!(
            repo.claim(&scope, command, Uuid::new_v4(), now)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn frozen_denominator_and_crash_unknown() {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let cycle = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let repo = MemoryChannelJobRepository::default();
        repo.create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id: cycle,
                input_hash: "a".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![ChannelTarget {
                    target_id,
                    input: ChannelTargetInput::Publish {
                        source_id: Uuid::new_v4(),
                        source_version_id: Uuid::new_v4(),
                        platform: "zhihu".into(),
                        account_id: Uuid::new_v4(),
                        title: "t".into(),
                        body: "b".into(),
                        body_sha256: "h".into(),
                    },
                }],
            },
        )
        .await
        .unwrap();
        let before = Utc::now();
        let attempt = Uuid::new_v4();
        repo.claim(&scope, target_id, attempt, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            repo.cycle_inputs(&scope, cycle, before)
                .await
                .unwrap()
                .publications
                .unwrap()[0]
                .status,
            ReportPublicationStatus::Planned
        );
        assert_eq!(
            repo.cycle_inputs(&scope, cycle, Utc::now())
                .await
                .unwrap()
                .publications
                .unwrap()[0]
                .status,
            ReportPublicationStatus::Unknown
        );
        assert!(
            repo.claim(&scope, target_id, Uuid::new_v4(), Utc::now())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn raw_observation_refusal_and_fixture_never_shrink_samples() {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let cycle_id = Uuid::new_v4();
        let now = Utc::now();
        let repo = MemoryChannelJobRepository::default();
        let input = |ordinal| ChannelTargetInput::Measure {
            account_id: Uuid::new_v4(),
            provider: "kimi".into(),
            model: "fixed".into(),
            surface: "consumer_web".into(),
            search_mode: "web_search".into(),
            protocol_version: "v1".into(),
            question_set_version: "v1".into(),
            question: "Raw prompt".into(),
            market: "global".into(),
            language: "en".into(),
            scheduled_at: now,
            sample_ordinal: ordinal,
            question_binding: None,
        };
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        repo.create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id,
                input_hash: "hash".into(),
                revision: 1,
                created_at: now,
                targets: vec![
                    ChannelTarget {
                        target_id: a,
                        input: input(0),
                    },
                    ChannelTarget {
                        target_id: b,
                        input: input(1),
                    },
                ],
            },
        )
        .await
        .unwrap();
        let at = now + chrono::Duration::seconds(1);
        let attempt = Uuid::new_v4();
        repo.claim(&scope, a, attempt, at).await.unwrap();
        repo.finish(
            &scope,
            a,
            attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Refused,
                detail: Some("model refusal".into()),
                occurred_at: at,
                raw_answer: Some("I cannot answer".into()),
                citations: vec![],
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("v1".into()),
                runner_evidence: vec![],
                fixture: false,
            },
            at,
        )
        .await
        .unwrap();
        let result = repo.cycle_inputs(&scope, cycle_id, at).await.unwrap();
        assert_eq!(result.manifests[1].expected_count, Some(2));
        assert_eq!(
            result.measurements.as_ref().unwrap()[0].status,
            ReportMeasurementStatus::Refused
        );
        assert_eq!(
            result.measurements.as_ref().unwrap()[0].evidence[0].kind,
            "observation"
        );
        assert_eq!(
            result.measurements.unwrap()[1].status,
            ReportMeasurementStatus::Pending
        );
        let fixture_attempt = Uuid::new_v4();
        repo.claim(&scope, b, fixture_attempt, at).await.unwrap();
        repo.finish(
            &scope,
            b,
            fixture_attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Observed,
                detail: None,
                occurred_at: at,
                raw_answer: Some("fixture".into()),
                citations: vec![],
                public_url: None,
                screenshot_ref: None,
                connector_version: Some("fixture".into()),
                runner_evidence: vec![],
                fixture: true,
            },
            at,
        )
        .await
        .unwrap();
        let result = repo.cycle_inputs(&scope, cycle_id, at).await.unwrap();
        assert_eq!(
            result.measurements.as_ref().unwrap()[1].evidence[0].kind,
            "channel_execution"
        );
        assert_eq!(result.manifests[1].expected_count, Some(2));
    }

    #[tokio::test]
    async fn account_publication_claim_is_single_flight_across_targets() {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let account_id = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let input = |source_version_id| ChannelTargetInput::Publish {
            source_id: Uuid::new_v4(),
            source_version_id,
            platform: "zhihu".into(),
            account_id,
            title: "title".into(),
            body: "body".into(),
            body_sha256: "hash".into(),
        };
        let repo = MemoryChannelJobRepository::default();
        repo.create_plan(
            &scope,
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scope.project_id.unwrap(),
                cycle_id: Uuid::new_v4(),
                input_hash: "frozen".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![
                    ChannelTarget {
                        target_id: first,
                        input: input(Uuid::new_v4()),
                    },
                    ChannelTarget {
                        target_id: second,
                        input: input(Uuid::new_v4()),
                    },
                ],
            },
        )
        .await
        .unwrap();
        let attempt = Uuid::new_v4();
        repo.claim(&scope, first, attempt, Utc::now())
            .await
            .unwrap();
        assert!(
            repo.claim(&scope, second, Uuid::new_v4(), Utc::now())
                .await
                .is_err()
        );
        repo.finish(
            &scope,
            first,
            attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Unknown,
                detail: Some("ambiguous".into()),
                occurred_at: Utc::now(),
                raw_answer: None,
                citations: vec![],
                public_url: None,
                screenshot_ref: None,
                connector_version: None,
                runner_evidence: vec![],
                fixture: false,
            },
            Utc::now(),
        )
        .await
        .unwrap();
        assert!(
            repo.claim(&scope, second, Uuid::new_v4(), Utc::now())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn shared_pool_account_is_single_flight_across_projects_and_tenants() {
        let operator = OperatorId::new(Uuid::new_v4());
        let account_id = Uuid::new_v4();
        let first_scope = TenantScope::new(
            operator,
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let other_scope = TenantScope::new(
            operator,
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let repo = MemoryChannelJobRepository::default();
        let target_ids = [Uuid::new_v4(), Uuid::new_v4()];
        for (scope, target_id) in [(&first_scope, target_ids[0]), (&other_scope, target_ids[1])] {
            repo.create_plan(
                scope,
                ChannelPlan {
                    plan_id: Uuid::new_v4(),
                    project_id: scope.project_id.unwrap(),
                    cycle_id: Uuid::new_v4(),
                    input_hash: Uuid::new_v4().to_string(),
                    revision: 1,
                    created_at: Utc::now(),
                    targets: vec![ChannelTarget {
                        target_id,
                        input: ChannelTargetInput::Publish {
                            source_id: Uuid::new_v4(),
                            source_version_id: Uuid::new_v4(),
                            platform: "zhihu".into(),
                            account_id,
                            title: "title".into(),
                            body: "body".into(),
                            body_sha256: "hash".into(),
                        },
                    }],
                },
            )
            .await
            .unwrap();
        }
        let attempt = Uuid::new_v4();
        repo.claim(&first_scope, target_ids[0], attempt, Utc::now())
            .await
            .unwrap();
        assert!(
            repo.claim(&other_scope, target_ids[1], Uuid::new_v4(), Utc::now())
                .await
                .is_err()
        );
        repo.finish(
            &first_scope,
            target_ids[0],
            attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Unknown,
                detail: Some("ambiguous".into()),
                occurred_at: Utc::now(),
                raw_answer: None,
                citations: vec![],
                public_url: None,
                screenshot_ref: None,
                connector_version: None,
                runner_evidence: vec![],
                fixture: false,
            },
            Utc::now(),
        )
        .await
        .unwrap();
        assert!(
            repo.claim(&other_scope, target_ids[1], Uuid::new_v4(), Utc::now())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn preflight_reservation_is_account_wide_and_stale_owner_cannot_claim() {
        let repo = MemoryChannelJobRepository::default();
        let operator = OperatorId::new(Uuid::new_v4());
        let account_id = Uuid::new_v4();
        let scopes = [0, 1].map(|_| {
            TenantScope::new(
                operator,
                TenantId::new(Uuid::new_v4()),
                Some(ProjectId::new(Uuid::new_v4())),
            )
        });
        let target = Uuid::new_v4();
        repo.create_plan(
            &scopes[0],
            ChannelPlan {
                plan_id: Uuid::new_v4(),
                project_id: scopes[0].project_id.unwrap(),
                cycle_id: Uuid::new_v4(),
                input_hash: "fixture".into(),
                revision: 1,
                created_at: Utc::now(),
                targets: vec![ChannelTarget {
                    target_id: target,
                    input: ChannelTargetInput::Measure {
                        account_id,
                        provider: "fixture".into(),
                        model: "fixture".into(),
                        surface: "consumer_web".into(),
                        search_mode: "web_search".into(),
                        protocol_version: "v1".into(),
                        question_set_version: "v1".into(),
                        question: "Question".into(),
                        market: "US".into(),
                        language: "en".into(),
                        scheduled_at: Utc::now(),
                        sample_ordinal: 0,
                        question_binding: None,
                    },
                }],
            },
        )
        .await
        .unwrap();
        let now = Utc::now();
        let old = Uuid::new_v4();
        repo.reserve_account(
            &scopes[0],
            account_id,
            old,
            now,
            now + chrono::Duration::seconds(2),
        )
        .await
        .unwrap();
        assert!(
            repo.reserve_account(
                &scopes[1],
                account_id,
                Uuid::new_v4(),
                now,
                now + chrono::Duration::minutes(5)
            )
            .await
            .is_err()
        );
        let later = now + chrono::Duration::seconds(3);
        let fresh = Uuid::new_v4();
        repo.reserve_account(
            &scopes[1],
            account_id,
            fresh,
            later,
            later + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
        assert!(
            repo.claim_reserved(&scopes[0], target, Uuid::new_v4(), old, later)
                .await
                .is_err()
        );
        repo.release_account(&scopes[0], account_id, old)
            .await
            .unwrap();
        assert!(
            repo.claim_reserved(&scopes[0], target, Uuid::new_v4(), fresh, later)
                .await
                .is_ok()
        );
    }
}
