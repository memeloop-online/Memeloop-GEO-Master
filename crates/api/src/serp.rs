//! Scoped, raw-first search observation orchestration. Reads never submit work.
use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    middleware,
    routing::{get, post},
};
use chrono::{DateTime, Duration, Utc};
use geo_domain::{
    AppError, ProjectId, ProjectRepository, ProjectSerpDispatchSource, ProjectSerpSettingsCursor,
    ProjectStatus, QuestionReference, QuestionRepository, SERP_TARGET_RULE_VERSION,
    SerpEvidenceOperation, SerpMeasurement, SerpObservation, SerpProtocol, SerpProviderTask,
    SerpRawEvidence, SerpRawReceipt, SerpRepository, SerpSendCertainty, SerpSendingIntent,
    SerpStoredRaw, SerpTarget, SerpTaskState, TenantScope, sha256_hex,
};
use geo_provider::serp::{SerpOperation, SerpRawResponse, SerpSentCertainty};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};

const LEASE_SECONDS: i64 = 120;
const POLL_SECONDS: i64 = 60;

/// Exact prepared wire bytes. Neither source credentials nor provider-wide
/// discovery results belong in this value.
#[derive(Clone)]
pub struct SerpPreparedSubmission {
    pub request_sha256: String,
    pub correlation_tag: String,
    pub body: Vec<u8>,
}

pub enum SerpReadOutcome {
    Pending,
    Failed,
    Observation(Box<SerpObservation>),
}

/// Trusted adapter boundary. Decoders receive only a persisted raw receipt.
/// Implementations must verify exact task/tag/frozen protocol binding.
#[async_trait]
pub trait SerpSource: Send + Sync {
    fn protocol(&self, query: &str) -> SerpProtocol;
    fn parser_version(&self) -> &str;
    fn prepare(
        &self,
        measurement: &SerpMeasurement,
        correlation_tag: &str,
    ) -> Result<SerpPreparedSubmission, AppError>;
    async fn send(&self, prepared: &SerpPreparedSubmission) -> SerpRawResponse;
    fn read_request_sha256(&self, task_id: &str) -> Result<String, AppError>;
    async fn read(&self, task_id: &str) -> SerpRawResponse;
    fn decode_submission(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
    ) -> Result<String, AppError>;
    fn decode_result(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
        task_id: &str,
        observation_id: Uuid,
        analyzed_at: DateTime<Utc>,
    ) -> Result<SerpReadOutcome, AppError>;
    fn verify_recovery(
        &self,
        measurement: &SerpMeasurement,
        raw: &SerpStoredRaw,
        intent: &SerpSendingIntent,
        task_id: &str,
    ) -> Result<(), AppError>;
}

pub struct ResolvedSerpSource {
    pub source: Arc<dyn SerpSource>,
    pub credential_revision: Option<i64>,
}

#[async_trait]
pub trait SerpSourceResolver: Send + Sync {
    async fn capabilities(&self, scope: &TenantScope) -> Result<Vec<SerpCapability>, AppError>;
    async fn current(
        &self,
        scope: &TenantScope,
        key: &str,
        protocol: Option<&SerpProtocol>,
    ) -> Result<ResolvedSerpSource, AppError>;
    async fn bound(
        &self,
        scope: &TenantScope,
        key: &str,
        protocol: &SerpProtocol,
        credential_revision: i64,
    ) -> Result<Arc<dyn SerpSource>, AppError>;
    async fn dispatch_sources(
        &self,
        after: Option<ProjectSerpSettingsCursor>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpDispatchSource>, AppError>;
}

#[derive(Clone)]
struct SourceRoute {
    scope: TenantScope,
    key: String,
    source: Arc<dyn SerpSource>,
}

#[derive(Clone)]
pub struct SerpService {
    repository: Arc<dyn SerpRepository>,
    projects: Arc<dyn ProjectRepository>,
    questions: Arc<dyn QuestionRepository>,
    sources: Vec<SourceRoute>,
    resolver: Option<Arc<dyn SerpSourceResolver>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptSerpMeasurement {
    pub idempotency_key: String,
    pub source_key: String,
    pub query: String,
    pub target: Option<SerpTarget>,
    pub question_reference: Option<QuestionReference>,
    /// Caller retains this value across retries; server-generated timestamps
    /// never silently change an idempotent command.
    pub scheduled_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct SerpCapability {
    pub source_key: String,
    /// Query is empty in this template; all other fields are server-owned.
    pub protocol_defaults: SerpProtocol,
}

#[derive(Serialize)]
pub struct SerpMeasurementPage {
    pub items: Vec<SerpMeasurement>,
    pub next_after: Option<Uuid>,
}

#[derive(Serialize)]
pub struct SerpMeasurementDetail {
    pub measurement: SerpMeasurement,
    pub observations: Vec<SerpObservation>,
    pub next_after: Option<Uuid>,
    pub execution: Option<SerpExecutionView>,
}

#[derive(Serialize)]
pub struct SerpExecutionView {
    pub attempt_id: Option<Uuid>,
    pub provider_task_id: Option<String>,
    pub next_poll_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct SerpSourcePage {
    pub items: Vec<SerpRawReceipt>,
    pub next_after: Option<Uuid>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReparseSerpSource {
    pub evidence_id: Uuid,
    pub idempotency_key: String,
}

impl SerpService {
    /// Report projection reads only persisted metadata. It never resolves
    /// source credentials, requests capabilities, or starts provider work.
    pub(crate) fn report_repository(&self) -> &dyn SerpRepository {
        self.repository.as_ref()
    }

    /// An empty source map preserves history access without granting any paid
    /// execution capability. Production wiring starts in this state.
    pub fn new(
        repository: Arc<dyn SerpRepository>,
        projects: Arc<dyn ProjectRepository>,
        questions: Arc<dyn QuestionRepository>,
    ) -> Self {
        Self {
            repository,
            projects,
            questions,
            sources: Vec::new(),
            resolver: None,
        }
    }

    pub fn with_source_resolver(mut self, resolver: Arc<dyn SerpSourceResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    pub fn with_source(
        mut self,
        scope: TenantScope,
        key: String,
        source: Arc<dyn SerpSource>,
    ) -> Result<Self, AppError> {
        if scope.project_id.is_none()
            || key.is_empty()
            || key.len() > 128
            || key.chars().any(char::is_control)
            || self
                .sources
                .iter()
                .any(|route| route.scope == scope && route.key == key)
        {
            return Err(AppError::invalid_request("invalid search source mapping"));
        }
        source.protocol("protocol validation").validate()?;
        self.sources.push(SourceRoute { scope, key, source });
        Ok(self)
    }

    pub async fn capabilities(&self, scope: &TenantScope) -> Result<Vec<SerpCapability>, AppError> {
        if let Some(resolver) = &self.resolver {
            return resolver.capabilities(scope).await;
        }
        Ok(self
            .sources
            .iter()
            .filter(|route| route.scope == *scope)
            .map(|route| SerpCapability {
                source_key: route.key.clone(),
                protocol_defaults: route.source.protocol(""),
            })
            .collect())
    }

    async fn current_source(
        &self,
        scope: &TenantScope,
        key: &str,
        protocol: Option<&SerpProtocol>,
    ) -> Result<ResolvedSerpSource, AppError> {
        if let Some(resolver) = &self.resolver {
            return resolver.current(scope, key, protocol).await;
        }
        let source = self
            .sources
            .iter()
            .find(|route| route.scope == *scope && route.key == key)
            .map(|route| route.source.clone())
            .ok_or_else(|| AppError::capability_missing("search source unavailable"))?;
        Ok(ResolvedSerpSource {
            source,
            credential_revision: None,
        })
    }

    async fn bound_source(
        &self,
        scope: &TenantScope,
        measurement: &SerpMeasurement,
        intent: &SerpSendingIntent,
    ) -> Result<Arc<dyn SerpSource>, AppError> {
        if let Some(resolver) = &self.resolver {
            let revision = intent
                .credential_revision
                .ok_or_else(|| AppError::not_ready("search attempt has no bound credentials"))?;
            return resolver
                .bound(
                    scope,
                    &measurement.source_key,
                    &measurement.protocol,
                    revision,
                )
                .await;
        }
        if intent.credential_revision.is_some() {
            return Err(AppError::not_ready("bound search credentials unavailable"));
        }
        self.static_source_for(scope, measurement)
    }

    fn static_source_for(
        &self,
        scope: &TenantScope,
        measurement: &SerpMeasurement,
    ) -> Result<Arc<dyn SerpSource>, AppError> {
        self.sources
            .iter()
            .find(|route| {
                route.scope == *scope
                    && route.key == measurement.source_key
                    && route.source.protocol(&measurement.protocol.query) == measurement.protocol
            })
            .map(|route| route.source.clone())
            .ok_or_else(|| AppError::capability_missing("search source unavailable"))
    }

    pub async fn accept(
        &self,
        scope: &TenantScope,
        input: AcceptSerpMeasurement,
    ) -> Result<SerpMeasurement, AppError> {
        let project_id = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        let project = self
            .projects
            .get(scope, project_id)
            .await?
            .ok_or_else(|| AppError::not_found("project not found"))?;
        if matches!(
            project.status,
            ProjectStatus::Paused | ProjectStatus::Archived
        ) {
            return Err(AppError::conflict("project is inactive"));
        }
        let resolved_source = self.current_source(scope, &input.source_key, None).await?;
        let protocol = resolved_source.source.protocol(&input.query);
        let binding = if let Some(reference) = input.question_reference {
            let resolved = self.questions.resolve_question(scope, reference).await?;
            if resolved.revision.text != input.query
                || resolved.revision.language != protocol.language
            {
                return Err(AppError::conflict(
                    "search question differs from frozen revision",
                ));
            }
            Some(resolved.binding)
        } else {
            None
        };
        let measurement = SerpMeasurement {
            measurement_id: Uuid::new_v4(),
            source_key: input.source_key,
            protocol,
            target: input.target.map(|target| target.normalized()).transpose()?,
            target_rule_version: SERP_TARGET_RULE_VERSION.into(),
            question_binding: binding,
            scheduled_at: input.scheduled_at,
            created_at: Utc::now(),
            state: SerpTaskState::Queued,
        };
        self.repository
            .accept(scope, &input.idempotency_key, measurement)
            .await
    }

    pub async fn list(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<SerpMeasurementPage, AppError> {
        validate_limit(limit)?;
        let mut items = self.repository.list(scope, after, limit + 1).await?;
        let more = items.len() > limit;
        items.truncate(limit);
        let next_after = more.then(|| items.last().expect("nonempty page").measurement_id);
        Ok(SerpMeasurementPage { items, next_after })
    }

    pub async fn detail(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<SerpMeasurementDetail, AppError> {
        validate_limit(limit)?;
        let measurement = self.measurement(scope, id).await?;
        let mut observations = self
            .repository
            .list_observations(scope, id, after, limit + 1)
            .await?;
        let more = observations.len() > limit;
        observations.truncate(limit);
        let next_after = more.then(|| observations.last().expect("nonempty page").observation_id);
        let execution = self
            .repository
            .get_execution(scope, id)
            .await?
            .map(|execution| SerpExecutionView {
                attempt_id: execution
                    .intent
                    .as_ref()
                    .map(|intent| intent.attempt_id)
                    .or_else(|| execution.claim.as_ref().map(|claim| claim.attempt_id)),
                provider_task_id: execution.provider_task.map(|task| task.provider_task_id),
                next_poll_at: execution.next_poll_at,
            });
        Ok(SerpMeasurementDetail {
            measurement,
            observations,
            next_after,
            execution,
        })
    }

    async fn measurement(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<SerpMeasurement, AppError> {
        self.repository
            .get(scope, id)
            .await?
            .ok_or_else(|| AppError::not_found("search measurement not found"))
    }

    pub async fn cancel(&self, scope: &TenantScope, id: Uuid) -> Result<SerpMeasurement, AppError> {
        self.repository.cancel(scope, id, Utc::now()).await?;
        self.measurement(scope, id).await
    }

    pub async fn raw(
        &self,
        scope: &TenantScope,
        id: Uuid,
        evidence_id: Uuid,
    ) -> Result<SerpStoredRaw, AppError> {
        self.measurement(scope, id).await?;
        self.repository
            .get_raw(scope, evidence_id)
            .await?
            .filter(|raw| raw.evidence.measurement_id == id)
            .ok_or_else(|| AppError::not_found("search source not found"))
    }

    pub async fn sources(
        &self,
        scope: &TenantScope,
        id: Uuid,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<SerpSourcePage, AppError> {
        validate_limit(limit)?;
        self.measurement(scope, id).await?;
        let mut items = self
            .repository
            .list_raw(scope, id, after, limit + 1)
            .await?;
        let more = items.len() > limit;
        items.truncate(limit);
        let next_after = more.then(|| items.last().expect("nonempty page").evidence_id);
        Ok(SerpSourcePage { items, next_after })
    }

    /// Parsing retained evidence is local and append-only. It never touches a
    /// provider or changes the original task state, including cancellation.
    pub async fn reparse(
        &self,
        scope: &TenantScope,
        id: Uuid,
        input: ReparseSerpSource,
    ) -> Result<SerpObservation, AppError> {
        if input.idempotency_key.is_empty()
            || input.idempotency_key.len() > 256
            || input.idempotency_key.chars().any(char::is_control)
        {
            return Err(AppError::invalid_request(
                "invalid analysis idempotency key",
            ));
        }
        let measurement = self.measurement(scope, id).await?;
        let raw = self.raw(scope, id, input.evidence_id).await?;
        let intent = self
            .repository
            .get_sending_intent(scope, id, raw.evidence.attempt_id)
            .await?
            .ok_or_else(|| AppError::not_found("search sending record not found"))?;
        let source = self.bound_source(scope, &measurement, &intent).await?;
        let bytes = serde_json::to_vec(&(scope, id, &input.idempotency_key))
            .map_err(|_| AppError::invalid_request("invalid analysis identity"))?;
        let hash = hex::decode(sha256_hex(&bytes)).expect("sha256 hex");
        let observation_id = Uuid::from_bytes(hash[..16].try_into().expect("sha256 length"));
        if let Some(prior) = self
            .repository
            .get_observation(scope, id, observation_id)
            .await?
        {
            if prior.raw_evidence_id != input.evidence_id
                || prior.parser_version != source.parser_version()
            {
                return Err(AppError::conflict("analysis idempotency request differs"));
            }
            return Ok(prior);
        }
        let task_id = raw
            .evidence
            .provider_task_id
            .as_deref()
            .ok_or_else(|| AppError::invalid_request("search source has no result task"))?;
        let SerpReadOutcome::Observation(observation) = source.decode_result(
            &measurement,
            &raw,
            &intent,
            task_id,
            observation_id,
            Utc::now(),
        )?
        else {
            return Err(AppError::conflict("search source has no completed result"));
        };
        observation.validate_source(&measurement, &raw)?;
        match self
            .repository
            .append_observation(scope, *observation.clone())
            .await
        {
            Ok(value) => Ok(value),
            Err(error) => {
                // Concurrent exact replay may have a different local analysis
                // timestamp; recover only the same immutable source/parser.
                if let Some(prior) = self
                    .repository
                    .get_observation(scope, id, observation_id)
                    .await?
                    && prior.raw_evidence_id == observation.raw_evidence_id
                    && prior.parser_version == observation.parser_version
                {
                    return Ok(prior);
                }
                Err(error)
            }
        }
    }

    /// One bounded paid-send attempt. The repository is the sole authority for
    /// whether a POST may occur, including concurrent workers and restarts.
    pub async fn submit_once(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let measurement = self.measurement(scope, id).await?;
        let project = self
            .projects
            .get(
                scope,
                scope
                    .project_id
                    .ok_or_else(|| AppError::forbidden("project scope required"))?,
            )
            .await?
            .ok_or_else(|| AppError::not_found("project not found"))?;
        if matches!(
            project.status,
            ProjectStatus::Paused | ProjectStatus::Archived
        ) {
            return Err(AppError::conflict("project is inactive"));
        }
        let resolved = self
            .current_source(scope, &measurement.source_key, Some(&measurement.protocol))
            .await?;
        let source = resolved.source;
        if source.protocol(&measurement.protocol.query) != measurement.protocol {
            return Err(AppError::conflict(
                "search source protocol differs from frozen request",
            ));
        }
        let now = Utc::now();
        let Some(claim) = self
            .repository
            .claim(scope, id, now, now + Duration::seconds(LEASE_SECONDS))
            .await?
        else {
            return Ok(());
        };
        let tag = format!("geo-serp-{}", claim.attempt_id);
        let prepared = source.prepare(&claim.measurement, &tag)?;
        if prepared.correlation_tag != tag || prepared.request_sha256 != sha256_hex(&prepared.body)
        {
            return Err(AppError::conflict(
                "search prepared request binding differs",
            ));
        }
        // Serialize project pause/archive with the durable send authorization.
        // PostgreSQL rechecks under the intent transaction's project row lock;
        // the memory repository holds its project read guard until commit.
        let project_guard = self
            .projects
            .hold_measurement_project(scope, project.id)
            .await?;
        let intent = self
            .repository
            .begin_send(
                scope,
                &claim,
                &prepared.request_sha256,
                &prepared.correlation_tag,
                resolved.credential_revision,
                Utc::now(),
            )
            .await?;
        drop(project_guard);
        let Some(intent) = intent else {
            return Ok(());
        };
        let response = source.send(&prepared).await;
        let raw = self
            .archive(
                scope,
                &intent,
                SerpEvidenceOperation::Submission,
                None,
                prepared.request_sha256,
                response,
            )
            .await?;
        match source.decode_submission(&claim.measurement, &raw, &intent) {
            Ok(task_id) => {
                self.repository
                    .bind_provider_task(
                        scope,
                        &intent,
                        SerpProviderTask {
                            measurement_id: id,
                            attempt_id: claim.attempt_id,
                            binding_evidence_id: raw.evidence.evidence_id,
                            provider_task_id: task_id,
                            correlation_tag: intent.correlation_tag.clone(),
                        },
                    )
                    .await?;
                self.repository
                    .finish(scope, &claim, SerpTaskState::AwaitingResult, Utc::now())
                    .await?;
            }
            Err(_) => {
                self.repository
                    .finish(scope, &claim, SerpTaskState::Unknown, Utc::now())
                    .await?;
            }
        }
        Ok(())
    }

    /// One exact-task read under a fresh fence. Pending is normal, including
    /// tasks queued longer than 45 minutes; there is no guessed completion TTL.
    pub async fn poll_once(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let measurement = self.measurement(scope, id).await?;
        let now = Utc::now();
        let Some(claim) = self
            .repository
            .claim_read(scope, id, now, now + Duration::seconds(LEASE_SECONDS))
            .await?
        else {
            return Ok(());
        };
        let intent = self
            .repository
            .get_sending_intent(scope, id, claim.attempt_id)
            .await?
            .ok_or_else(|| AppError::not_found("search sending record not found"))?;
        let source = self.bound_source(scope, &measurement, &intent).await?;
        let task = self
            .repository
            .get_provider_task(scope, id, claim.attempt_id)
            .await?
            .ok_or_else(|| AppError::not_found("search provider task not found"))?;
        let request_sha256 = source.read_request_sha256(&task.provider_task_id)?;
        let response = source.read(&task.provider_task_id).await;
        let raw = self
            .archive(
                scope,
                &intent,
                SerpEvidenceOperation::ResultRead,
                Some(task.provider_task_id.clone()),
                request_sha256,
                response,
            )
            .await?;
        match source.decode_result(
            &measurement,
            &raw,
            &intent,
            &task.provider_task_id,
            Uuid::new_v4(),
            Utc::now(),
        ) {
            Ok(SerpReadOutcome::Pending) => {
                let now = Utc::now();
                self.repository
                    .release_read(scope, &claim, now + Duration::seconds(POLL_SECONDS), now)
                    .await?;
            }
            Ok(SerpReadOutcome::Failed) => {
                self.repository
                    .finish(scope, &claim, SerpTaskState::Failed, Utc::now())
                    .await?;
            }
            Ok(SerpReadOutcome::Observation(observation)) => {
                observation.validate_source(&measurement, &raw)?;
                self.repository
                    .append_observation(scope, *observation)
                    .await?;
                self.repository
                    .finish(scope, &claim, SerpTaskState::Completed, Utc::now())
                    .await?;
            }
            Err(_) => {
                self.repository
                    .finish(scope, &claim, SerpTaskState::Unknown, Utc::now())
                    .await?;
            }
        }
        Ok(())
    }

    /// Recovery never asks a client for an external task ID and never submits.
    /// A candidate must originate in this measurement's own retained evidence;
    /// account-wide supplier discovery is deliberately outside tenant storage.
    pub async fn recover_once(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let measurement = self.measurement(scope, id).await?;
        if measurement.state != SerpTaskState::Unknown {
            return Ok(());
        }
        let Some(execution) = self.repository.get_execution(scope, id).await? else {
            return Ok(());
        };
        if execution.next_poll_at.is_some_and(|at| at > Utc::now()) {
            return Ok(());
        }
        let Some(intent) = execution.intent else {
            return Ok(());
        };
        let source = self.bound_source(scope, &measurement, &intent).await?;
        let mut candidate = execution.provider_task.map(|task| task.provider_task_id);
        if candidate.is_none() {
            for receipt in self.repository.list_raw(scope, id, None, 50).await? {
                if receipt.attempt_id != intent.attempt_id {
                    continue;
                }
                if let Some(id) = receipt.provider_task_id {
                    candidate = Some(id);
                    break;
                }
                if receipt.operation == SerpEvidenceOperation::Submission {
                    let raw = self.raw(scope, id, receipt.evidence_id).await?;
                    candidate = source.decode_submission(&measurement, &raw, &intent).ok();
                    if candidate.is_some() {
                        break;
                    }
                }
            }
        }
        let now = Utc::now();
        // Persist backoff before an uncertain read, including no candidate. A
        // process restart cannot turn this into a tight polling loop.
        self.repository
            .defer_recovery(scope, id, now + Duration::minutes(5), now)
            .await?;
        let Some(task_id) = candidate else {
            return Ok(());
        };
        let request_sha256 = source.read_request_sha256(&task_id)?;
        let response = source.read(&task_id).await;
        let raw = self
            .archive(
                scope,
                &intent,
                SerpEvidenceOperation::RecoveryRead,
                Some(task_id.clone()),
                request_sha256,
                response,
            )
            .await?;
        if source
            .verify_recovery(&measurement, &raw, &intent, &task_id)
            .is_ok()
        {
            self.repository
                .recover_provider_task(
                    scope,
                    &intent,
                    SerpProviderTask {
                        measurement_id: id,
                        attempt_id: intent.attempt_id,
                        binding_evidence_id: raw.evidence.evidence_id,
                        provider_task_id: task_id,
                        correlation_tag: intent.correlation_tag.clone(),
                    },
                    Utc::now(),
                )
                .await?;
        }
        Ok(())
    }

    /// Restart-safe bounded dispatcher hook. Scheduling is repository-owned;
    /// callers retain only the page cursor, never an in-memory polling lease.
    pub async fn dispatch_due_page(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Option<Uuid>, AppError> {
        validate_limit(limit)?;
        self.repository
            .expire_claims(scope, Utc::now(), 100)
            .await?;
        let items = self
            .repository
            .list_due(scope, Utc::now(), after, limit)
            .await?;
        let next =
            (items.len() == limit).then(|| items.last().expect("nonempty page").measurement_id);
        for measurement in items {
            let result = match measurement.state {
                SerpTaskState::Queued => self.submit_once(scope, measurement.measurement_id).await,
                SerpTaskState::AwaitingResult => {
                    self.poll_once(scope, measurement.measurement_id).await
                }
                SerpTaskState::Unknown => {
                    self.recover_once(scope, measurement.measurement_id).await
                }
                _ => Ok(()),
            };
            if result.is_err() {
                tracing::warn!("search observation dispatch did not complete");
            }
        }
        Ok(next)
    }

    async fn archive(
        &self,
        scope: &TenantScope,
        intent: &SerpSendingIntent,
        operation: SerpEvidenceOperation,
        provider_task_id: Option<String>,
        request_sha256: String,
        response: SerpRawResponse,
    ) -> Result<SerpStoredRaw, AppError> {
        let expected = if operation == SerpEvidenceOperation::Submission {
            SerpOperation::PostTask
        } else {
            SerpOperation::GetTask
        };
        if response.operation != expected {
            return Err(AppError::conflict("search response operation differs"));
        }
        self.repository
            .append_raw(
                scope,
                intent,
                SerpRawEvidence {
                    evidence_id: Uuid::new_v4(),
                    measurement_id: intent.measurement_id,
                    attempt_id: intent.attempt_id,
                    operation,
                    provider_task_id,
                    request_sha256,
                    intent_request_sha256: intent.request_sha256.clone(),
                    response_sha256: sha256_hex(&response.body),
                    body: response.body,
                    body_complete: response.body_complete && response.error.is_none(),
                    http_status: response.http_status,
                    send_certainty: match response.sent {
                        SerpSentCertainty::NotSent => SerpSendCertainty::NotSent,
                        SerpSentCertainty::PossiblySent => SerpSendCertainty::MayHaveBeenSent,
                        SerpSentCertainty::ResponseReceived => SerpSendCertainty::ResponseReceived,
                    },
                    captured_at: Utc::now(),
                },
            )
            .await
    }
}

/// The only in-memory state here is a timer. All due times, execution ownership,
/// send authorization and evidence survive process restarts in the repository.
/// No configured source means no dispatcher and no externally visible effects.
pub fn spawn_serp_dispatcher(service: SerpService) -> Option<tokio::task::JoinHandle<()>> {
    let mut scopes = Vec::new();
    for route in &service.sources {
        if !scopes.contains(&route.scope) {
            scopes.push(route.scope.clone());
        }
    }
    if scopes.is_empty() && service.resolver.is_none() {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let mut current_scopes = scopes.clone();
            if let Some(resolver) = &service.resolver {
                let mut after = None;
                loop {
                    let page = match resolver.dispatch_sources(after, 100).await {
                        Ok(page) => page,
                        Err(_) => {
                            tracing::warn!("search source inventory unavailable");
                            break;
                        }
                    };
                    let done = page.len() < 100;
                    after = page.last().and_then(|source| source.cursor().ok());
                    for source in page {
                        if !current_scopes.contains(&source.scope) {
                            current_scopes.push(source.scope);
                        }
                    }
                    if done {
                        break;
                    }
                }
            }
            for scope in &current_scopes {
                let mut after = None;
                for _ in 0..5 {
                    match service.dispatch_due_page(scope, after, 50).await {
                        Ok(Some(cursor)) => after = Some(cursor),
                        Ok(None) => break,
                        Err(_) => {
                            tracing::warn!("search observation scan unavailable");
                            break;
                        }
                    }
                }
            }
        }
    }))
}

fn validate_limit(limit: usize) -> Result<(), AppError> {
    if !(1..=50).contains(&limit) {
        return Err(AppError::invalid_request("limit must be 1 to 50"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    after: Option<Uuid>,
    limit: Option<usize>,
    tenant_id: Option<String>,
    project_id: Option<ProjectId>,
}

fn service(state: &AppState) -> Result<&SerpService, AppError> {
    state
        .serp_service()
        .ok_or_else(|| AppError::capability_missing("search observation storage unavailable"))
}

async fn capabilities(
    State(state): State<AppState>,
    Path(project): Path<ProjectId>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Vec<SerpCapability>>, ApiError> {
    let map = |error| api_error(error, context.request_id);
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .capabilities(&scope)
            .await
            .map_err(map)?,
    ))
}

async fn accept(
    State(state): State<AppState>,
    Path(project): Path<ProjectId>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<AcceptSerpMeasurement>,
) -> Result<(StatusCode, Json<SerpMeasurement>), ApiError> {
    let map = |error| api_error(error, context.request_id);
    require_project_writer(&auth).map_err(map)?;
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    let measurement = service(&state)
        .map_err(map)?
        .accept(&scope, input)
        .await
        .map_err(map)?;
    Ok((StatusCode::ACCEPTED, Json(measurement)))
}

async fn list(
    State(state): State<AppState>,
    Path(project): Path<ProjectId>,
    Query(query): Query<PageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SerpMeasurementPage>, ApiError> {
    let map = |error| api_error(error, context.request_id);
    let _ = (query.tenant_id, query.project_id);
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .list(&scope, query.after, query.limit.unwrap_or(20))
            .await
            .map_err(map)?,
    ))
}

async fn detail(
    State(state): State<AppState>,
    Path((project, id)): Path<(ProjectId, Uuid)>,
    Query(query): Query<PageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SerpMeasurementDetail>, ApiError> {
    let map = |error| api_error(error, context.request_id);
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .detail(&scope, id, query.after, query.limit.unwrap_or(20))
            .await
            .map_err(map)?,
    ))
}

async fn cancel(
    State(state): State<AppState>,
    Path((project, id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SerpMeasurement>, ApiError> {
    let map = |error| api_error(error, context.request_id);
    require_project_writer(&auth).map_err(map)?;
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .cancel(&scope, id)
            .await
            .map_err(map)?,
    ))
}

async fn raw(
    State(state): State<AppState>,
    Path((project, id, evidence)): Path<(ProjectId, Uuid, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SerpStoredRaw>, ApiError> {
    let map = |error| api_error(error, context.request_id);
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .raw(&scope, id, evidence)
            .await
            .map_err(map)?,
    ))
}

async fn reparse(
    State(state): State<AppState>,
    Path((project, id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<ReparseSerpSource>,
) -> Result<Json<SerpObservation>, ApiError> {
    let map = |error| api_error(error, context.request_id);
    require_project_writer(&auth).map_err(map)?;
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .reparse(&scope, id, input)
            .await
            .map_err(map)?,
    ))
}

async fn sources(
    State(state): State<AppState>,
    Path((project, id)): Path<(ProjectId, Uuid)>,
    Query(query): Query<PageQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<SerpSourcePage>, ApiError> {
    let map = |error| api_error(error, context.request_id);
    let scope = crate::channel_jobs::scope(&state, &tenant, project)
        .await
        .map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .sources(&scope, id, query.after, query.limit.unwrap_or(20))
            .await
            .map_err(map)?,
    ))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/serp-capabilities",
            get(capabilities),
        )
        .route(
            "/projects/{project_id}/serp-measurements",
            get(list).post(accept),
        )
        .route("/projects/{project_id}/serp-measurements/{id}", get(detail))
        .route(
            "/projects/{project_id}/serp-measurements/{id}/cancel",
            post(cancel),
        )
        .route(
            "/projects/{project_id}/serp-measurements/{id}/raw/{evidence_id}",
            get(raw),
        )
        .route(
            "/projects/{project_id}/serp-measurements/{id}/reparse",
            post(reparse),
        )
        .route(
            "/projects/{project_id}/serp-measurements/{id}/sources",
            get(sources),
        )
        .layer(middleware::from_fn(crate::csrf_origin_from_request))
        .layer(middleware::from_fn(crate::auth_scope_from_request))
        .layer(middleware::from_fn(crate::no_store_middleware))
}
