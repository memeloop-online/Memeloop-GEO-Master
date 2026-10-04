//! Narrow, project-scoped channel capabilities for the Agent runtime.
//! No browser session, proxy, publication body, or runner evidence crosses
//! this boundary.

use async_trait::async_trait;
use geo_domain::{
    AppError, ChannelOutcomeStatus, ChannelOwnerKind, ChannelTargetInput, ChannelTargetView,
    ErrorCode, KnowledgePurpose, SourceKind, SourceState, TenantScope, sha256_hex,
};
use geo_worker::{
    ChannelDiscoverRequest, ChannelDiscoveryItem, ChannelDiscoveryKind, ChannelDiscoveryPage,
    ChannelExecutionResult, ChannelExecutionState, ChannelManifestPage, ChannelManifestReadRequest,
    ChannelMeasurementPlanItem, ChannelPlanReceipt, ChannelPlanRequest, ChannelPublicationPlanItem,
    ChannelTargetExecuteRequest, ChannelTargetKind, ChannelTargetSummary,
};
use uuid::Uuid;

use crate::{
    AppState,
    channel_jobs::{
        ChannelDispatchDeferred, ChannelDispatchResult, MeasurementRequest, PlanRequest,
        PublicationRequest, create_channel_plan, execute_channel_target,
    },
};

#[async_trait]
pub(crate) trait ChannelToolService: Send + Sync {
    async fn discover(
        &self,
        scope: &TenantScope,
        request: ChannelDiscoverRequest,
    ) -> Result<ChannelDiscoveryPage, AppError>;
    async fn plan(
        &self,
        scope: &TenantScope,
        request: ChannelPlanRequest,
    ) -> Result<ChannelPlanReceipt, AppError>;
    async fn manifest_read(
        &self,
        scope: &TenantScope,
        request: ChannelManifestReadRequest,
    ) -> Result<ChannelManifestPage, AppError>;
    async fn target_execute(
        &self,
        scope: &TenantScope,
        request: ChannelTargetExecuteRequest,
    ) -> Result<ChannelExecutionResult, AppError>;
}

fn project(scope: &TenantScope) -> Result<geo_domain::ProjectId, AppError> {
    scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

async fn cycle(
    state: &AppState,
    scope: &TenantScope,
    requested: Option<Uuid>,
) -> Result<Uuid, AppError> {
    let project_id = project(scope)?;
    if let Some(id) = requested {
        state
            .project_repository()
            .get_report_cycle(scope, project_id, id)
            .await?
            .ok_or_else(|| AppError::not_found("cycle not found"))?;
        Ok(id)
    } else {
        state
            .project_repository()
            .get_current_cycle(scope, project_id)
            .await?
            .map(|cycle| cycle.cycle_id)
            .ok_or_else(|| AppError::not_found("current cycle not found"))
    }
}

fn page_offset(cursor: Option<&str>, digest: impl Fn(usize) -> String) -> Result<usize, AppError> {
    let Some(cursor) = cursor else { return Ok(0) };
    let mut parts = cursor.split('.');
    let (Some("v1"), Some(position), Some(hash), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(AppError::invalid_request("invalid channel cursor"));
    };
    let offset = position
        .parse::<usize>()
        .map_err(|_| AppError::invalid_request("invalid channel cursor"))?;
    if hash != digest(offset) {
        return Err(AppError::invalid_request(
            "channel cursor does not match the scoped snapshot",
        ));
    }
    Ok(offset)
}

fn next_cursor(end: usize, len: usize, digest: impl Fn(usize) -> String) -> Option<String> {
    (end < len).then(|| format!("v1.{end}.{}", digest(end)))
}

fn execution(view: &ChannelTargetView) -> ChannelExecutionResult {
    let last = view.attempts.last();
    let outcome = last.and_then(|attempt| attempt.outcome.as_ref());
    let state = match outcome.map(|outcome| outcome.status) {
        None if last.is_some() => ChannelExecutionState::UnknownResult,
        None => ChannelExecutionState::Pending,
        Some(ChannelOutcomeStatus::Unknown) => ChannelExecutionState::UnknownResult,
        Some(_) => ChannelExecutionState::Completed,
    };
    ChannelExecutionResult {
        target_id: view.target.target_id,
        state,
        attempt_id: last.map(|attempt| attempt.attempt_id),
        outcome_status: outcome.map(|outcome| {
            serde_json::to_value(outcome.status)
                .expect("serializable channel outcome")
                .as_str()
                .expect("channel outcome serializes as string")
                .to_owned()
        }),
        deferred_reason: None,
        // URLs can contain platform account identifiers. The tool only needs
        // outcome state and provenance; evidence detail remains in scoped UI.
        public_url: None,
        evidence_ref: None,
        fixture: outcome.map(|outcome| outcome.fixture),
    }
}

fn summary(view: ChannelTargetView) -> ChannelTargetSummary {
    let execution = execution(&view);
    let (kind, account_id, platform_or_provider, source_id, source_version_id, scheduled_at) =
        match view.target.input {
            ChannelTargetInput::Publish {
                source_id,
                source_version_id,
                account_id,
                platform,
                ..
            } => (
                ChannelTargetKind::Publish,
                account_id,
                platform,
                Some(source_id),
                Some(source_version_id),
                None,
            ),
            ChannelTargetInput::GeneratedPublish {
                account_id,
                platform,
                ..
            } => (
                ChannelTargetKind::Publish,
                account_id,
                platform,
                None,
                None,
                None,
            ),
            ChannelTargetInput::Measure {
                account_id,
                provider,
                scheduled_at,
                ..
            } => (
                ChannelTargetKind::Measure,
                account_id,
                provider,
                None,
                None,
                Some(scheduled_at),
            ),
        };
    ChannelTargetSummary {
        target_id: view.target.target_id,
        kind,
        account_id,
        platform_or_provider,
        source_id,
        source_version_id,
        scheduled_at,
        execution,
    }
}

fn deferred(target_id: Uuid, reason: ChannelDispatchDeferred) -> ChannelExecutionResult {
    ChannelExecutionResult {
        target_id,
        state: ChannelExecutionState::Deferred,
        attempt_id: None,
        outcome_status: None,
        deferred_reason: Some(
            match reason {
                ChannelDispatchDeferred::ProjectInactive => "project_inactive",
                ChannelDispatchDeferred::ScheduledForLater => "scheduled_for_later",
                ChannelDispatchDeferred::AccountUnavailable => "account_unavailable",
                ChannelDispatchDeferred::AccountBusy => "account_busy",
                ChannelDispatchDeferred::RunnerUnavailable => "runner_unavailable",
                ChannelDispatchDeferred::SourceUnavailable => "source_unavailable",
                ChannelDispatchDeferred::ConnectorUnavailable => "connector_unavailable",
                ChannelDispatchDeferred::FixtureOnly => "fixture_only",
            }
            .to_owned(),
        ),
        public_url: None,
        evidence_ref: None,
        fixture: None,
    }
}

#[async_trait]
impl ChannelToolService for AppState {
    async fn discover(
        &self,
        scope: &TenantScope,
        request: ChannelDiscoverRequest,
    ) -> Result<ChannelDiscoveryPage, AppError> {
        let project_id = project(scope)?;
        self.project_repository()
            .get(scope, project_id)
            .await?
            .ok_or_else(|| AppError::not_found("project not found"))?;
        let mut items = match request.kind {
            ChannelDiscoveryKind::PublicSources => {
                let mut allowed = Vec::new();
                for source in self.knowledge_repository().list_sources(scope).await? {
                    if source.state != SourceState::Active
                        || source.purpose != KnowledgePurpose::Public
                        || !matches!(source.kind, SourceKind::Text | SourceKind::Object)
                    {
                        continue;
                    }
                    let Some(version_id) = source.current_version_id else {
                        continue;
                    };
                    let Some(detail) = self
                        .knowledge_repository()
                        .get_source_detail(scope, source.source_id)
                        .await?
                    else {
                        continue;
                    };
                    if !detail
                        .versions
                        .iter()
                        .any(|version| version.source_version_id == version_id)
                        || !detail
                            .chunks
                            .iter()
                            .any(|chunk| chunk.source_version_id == version_id)
                    {
                        continue;
                    }
                    allowed.push(ChannelDiscoveryItem::PublicSource {
                        source_id: source.source_id,
                        source_version_id: version_id,
                        name: source.name,
                        // The publication input is the parser's plain-text
                        // chunks, not the original uploaded media. Repository
                        // source metadata does not expose original MIME here.
                        media_type: "text/plain".to_owned(),
                    });
                }
                allowed
            }
            ChannelDiscoveryKind::Accounts => {
                let repository = &self.channel_service().repository;
                let mut accounts = repository.list_accounts(scope).await?;
                accounts.extend(
                    repository
                        .list_assigned_pool_accounts(scope)
                        .await?
                        .into_iter()
                        .map(|account| account.assigned_view(project_id)),
                );
                accounts
                    .into_iter()
                    .map(|account| ChannelDiscoveryItem::Account {
                        account_id: account.account_id,
                        platform: account.platform,
                        display_name: account.display_name,
                        status: serde_json::to_value(account.status)
                            .expect("serializable channel status")
                            .as_str()
                            .expect("channel status serializes as string")
                            .to_owned(),
                        enabled: account.enabled,
                        owner_kind: match account.owner_kind {
                            ChannelOwnerKind::Customer => "customer",
                            ChannelOwnerKind::OperatorPool => "operator_pool",
                        }
                        .to_owned(),
                    })
                    .collect()
            }
        };
        items.sort_by_key(|item| match item {
            ChannelDiscoveryItem::PublicSource { source_id, .. } => *source_id,
            ChannelDiscoveryItem::Account { account_id, .. } => *account_id,
        });
        let identities = items
            .iter()
            .map(|item| serde_json::to_string(item).expect("serializable discovery item"))
            .collect::<Vec<_>>()
            .join("|");
        let digest = |offset| {
            sha256_hex(
                format!(
                    "geo.channel.discovery.v1|{}|{:?}|{identities}|{offset}",
                    scope.storage_key(),
                    request.kind
                )
                .as_bytes(),
            )
        };
        let offset = page_offset(request.cursor.as_deref(), digest)?;
        if offset >= items.len() && request.cursor.is_some() {
            return Err(AppError::invalid_request("channel cursor is out of range"));
        }
        let end = offset
            .saturating_add(request.limit.unwrap_or(25) as usize)
            .min(items.len());
        let next_cursor = next_cursor(end, items.len(), digest);
        let current_cycle_id = self
            .project_repository()
            .get_current_cycle(scope, project_id)
            .await?
            .map(|cycle| cycle.cycle_id);
        Ok(ChannelDiscoveryPage {
            kind: request.kind,
            current_cycle_id,
            items: items[offset..end].to_vec(),
            next_cursor,
        })
    }

    async fn plan(
        &self,
        scope: &TenantScope,
        request: ChannelPlanRequest,
    ) -> Result<ChannelPlanReceipt, AppError> {
        let cycle_id = cycle(self, scope, request.cycle_id).await?;
        let plan = create_channel_plan(
            self,
            scope,
            cycle_id,
            PlanRequest {
                publications: request
                    .publications
                    .into_iter()
                    .map(
                        |ChannelPublicationPlanItem {
                             source_id,
                             source_version_id,
                             platform,
                             account_id,
                         }| PublicationRequest {
                            source_id,
                            source_version_id,
                            platform,
                            account_id,
                        },
                    )
                    .collect(),
                measurements: request
                    .measurements
                    .into_iter()
                    .map(
                        |ChannelMeasurementPlanItem {
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
                         }| MeasurementRequest {
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
                        },
                    )
                    .collect(),
            },
        )
        .await?;
        Ok(ChannelPlanReceipt {
            plan_id: plan.plan_id,
            cycle_id: plan.cycle_id,
            revision: plan.revision,
            expected_count: plan.targets.len() as u64,
            dispatch_state: "pending".to_owned(),
        })
    }

    async fn manifest_read(
        &self,
        scope: &TenantScope,
        request: ChannelManifestReadRequest,
    ) -> Result<ChannelManifestPage, AppError> {
        let cycle_id = cycle(self, scope, request.cycle_id).await?;
        let plan = self
            .channel_job_repository()
            .get_plan(scope, cycle_id)
            .await?
            .ok_or_else(|| AppError::not_found("channel plan not found"))?;
        if request
            .revision
            .is_some_and(|revision| revision != plan.revision)
        {
            return Err(AppError::invalid_request(
                "channel plan revision does not match",
            ));
        }
        let digest = |offset| {
            sha256_hex(
                format!(
                    "geo.channel.manifest.v1|{}|{}|{cycle_id}|{}|{offset}",
                    scope.storage_key(),
                    plan.plan_id,
                    plan.revision
                )
                .as_bytes(),
            )
        };
        let offset = page_offset(request.cursor.as_deref(), digest)?;
        if offset >= plan.targets.len() && request.cursor.is_some() {
            return Err(AppError::invalid_request("channel cursor is out of range"));
        }
        let end = offset
            .saturating_add(request.limit.unwrap_or(25) as usize)
            .min(plan.targets.len());
        let mut items = Vec::with_capacity(end - offset);
        for target in &plan.targets[offset..end] {
            let view = self
                .channel_job_repository()
                .get_target(scope, target.target_id)
                .await?;
            items.push(summary(view));
        }
        Ok(ChannelManifestPage {
            plan_id: plan.plan_id,
            cycle_id,
            revision: plan.revision,
            sealed: true,
            expected_count: plan.targets.len() as u64,
            items,
            next_cursor: next_cursor(end, plan.targets.len(), digest),
        })
    }

    async fn target_execute(
        &self,
        scope: &TenantScope,
        request: ChannelTargetExecuteRequest,
    ) -> Result<ChannelExecutionResult, AppError> {
        project(scope)?;
        let repository = self.channel_job_repository();
        let existing = repository.get_target(scope, request.target_id).await?;
        if !existing.attempts.is_empty() {
            return Ok(execution(&existing));
        }
        match execute_channel_target(self, scope, request.target_id).await {
            Ok(ChannelDispatchResult::Executed(view)) => Ok(execution(&view)),
            Ok(ChannelDispatchResult::Deferred(reason)) => Ok(deferred(request.target_id, reason)),
            Err(error) if error.code == ErrorCode::Conflict => {
                // A race is replayable only if a real attempt now exists.
                match repository.get_target(scope, request.target_id).await {
                    Ok(current) if !current.attempts.is_empty() => Ok(execution(&current)),
                    _ => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }
}
