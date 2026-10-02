//! Frozen external channel targets and server-owned execution evidence.
//! Browser observations are not content-generation or customer-supplied receipts.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    AppError, ProjectId, ReportEvidenceReference, ReportManifestKind, ReportManifestRef,
    ReportMeasurementStatus, ReportMeasurementTarget, ReportPublicationStatus,
    ReportPublicationTarget, TenantScope,
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
    },
}

impl ChannelTargetInput {
    pub fn account_id(&self) -> Uuid {
        match self {
            Self::Publish { account_id, .. } | Self::Measure { account_id, .. } => *account_id,
        }
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
                ..
            } => Some(format!(
                "{provider}|{model}|{surface}|{search_mode}|{protocol_version}|{question_set_version}|{market}|{language}"
            )),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCycleInputs {
    pub manifests: Vec<ReportManifestRef>,
    pub publications: Option<Vec<ReportPublicationTarget>>,
    pub measurements: Option<Vec<ReportMeasurementTarget>>,
}

#[async_trait]
pub trait ChannelJobRepository: Send + Sync {
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
            ChannelTargetInput::Publish { platform, .. } => {
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
            ChannelTargetInput::Measure { scheduled_at, .. } => {
                measurements.push(ReportMeasurementTarget {
                    target_id: target.target_id,
                    comparison_key: target.input.comparison_key().unwrap_or_default(),
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
                })
            }
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
pub struct MemoryChannelJobRepository(Arc<Mutex<HashMap<ChannelScopeKey, MemoryCycle>>>);

type ChannelScopeKey = (Uuid, Uuid, Uuid, Uuid);

#[derive(Default)]
struct MemoryCycle {
    plan: Option<ChannelPlan>,
    attempts: HashMap<Uuid, Vec<ChannelAttempt>>,
}

#[async_trait]
impl ChannelJobRepository for MemoryChannelJobRepository {
    async fn create_plan(
        &self,
        scope: &TenantScope,
        plan: ChannelPlan,
    ) -> Result<ChannelPlan, AppError> {
        if scope.project_id != Some(plan.project_id) {
            return Err(AppError::forbidden("plan outside project"));
        }
        let mut all = self.0.lock().await;
        let cycle = all
            .entry((
                scope_key(scope)?.0,
                scope_key(scope)?.1,
                scope_key(scope)?.2,
                plan.cycle_id,
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
            .get(&(o, t, p, cycle_id))
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
            .flat_map(|(_, cycle)| cycle.plan.iter().flat_map(|plan| plan.targets.iter()))
            .find(|target| target.target_id == target_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("target not found"))?;
        if matches!(pending_target.input, ChannelTargetInput::Publish { .. })
            && all
                .iter()
                // A pool account can serve several tenants/projects under one
                // operator; its write lease is account-wide, not project-wide.
                .filter(|((o, _, _, _), _)| *o == key.0)
                .any(|(_, cycle)| {
                    cycle.plan.as_ref().is_some_and(|plan| {
                        plan.targets.iter().any(|target| {
                            matches!(target.input, ChannelTargetInput::Publish { .. })
                                && target.input.account_id() == pending_target.input.account_id()
                                && cycle
                                    .attempts
                                    .get(&target.target_id)
                                    .is_some_and(|attempts| {
                                        attempts.iter().any(|attempt| attempt.received_at.is_none())
                                    })
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
            .find(|((o, t, p, _), cycle)| {
                (*o, *t, *p) == key
                    && cycle.plan.as_ref().is_some_and(|plan| {
                        plan.targets
                            .iter()
                            .any(|target| target.target_id == target_id)
                    })
            })
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let target = cycle
            .plan
            .as_ref()
            .and_then(|plan| {
                plan.targets
                    .iter()
                    .find(|target| target.target_id == target_id)
            })
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
            .find(|((o, t, p, _), cycle)| {
                (*o, *t, *p) == key
                    && cycle.plan.as_ref().is_some_and(|plan| {
                        plan.targets
                            .iter()
                            .any(|target| target.target_id == target_id)
                    })
            })
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let target = cycle
            .plan
            .as_ref()
            .and_then(|plan| {
                plan.targets
                    .iter()
                    .find(|target| target.target_id == target_id)
            })
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

    async fn get_target(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
    ) -> Result<ChannelTargetView, AppError> {
        let all = self.0.lock().await;
        let key = scope_key(scope)?;
        let cycle = all
            .iter()
            .find(|((o, t, p, _), cycle)| {
                (*o, *t, *p) == key
                    && cycle.plan.as_ref().is_some_and(|plan| {
                        plan.targets
                            .iter()
                            .any(|target| target.target_id == target_id)
                    })
            })
            .map(|(_, cycle)| cycle)
            .ok_or_else(|| AppError::not_found("target not found"))?;
        let target = cycle
            .plan
            .as_ref()
            .and_then(|plan| {
                plan.targets
                    .iter()
                    .find(|target| target.target_id == target_id)
            })
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
        let cycle = all.get(&(o, t, p, cycle_id));
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
    use crate::{OperatorId, TenantId};
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
}
