//! Second-stage, project-scoped distribution. The request supplies selectors,
//! never publishable content, capability claims, or a success decision.
//! Publication commands are durable intent, not evidence of external delivery.

use std::{collections::BTreeSet, sync::Arc};

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use geo_domain::{
    AppError, ChannelAccount, ChannelRepository, ChannelStatus, ConnectorAvailability,
    ConnectorCapabilityRepository, ConnectorKey, ContentExecutionStatus, ContentItemStatus,
    ContentPublicEligibility, ContentRepository, ContentRevision, DistributionDeferralReason,
    DistributionManifest, DistributionRepository, DistributionScopeMode, DistributionTarget,
    DistributionTargetPage, DistributionTargetStatus, FreezeDistribution, KnowledgePurpose,
    KnowledgeRepository, PlatformPlacement, PreparedDistribution, ProjectId, ProjectRepository,
    ProjectStatus, PublicationOrigin, SourceState, TenantScope,
    publication_format_for_semantic_type,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};

const PAGE_LIMIT: usize = 64;
const MAX_PAGES_PER_REQUEST: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicationTargetReference {
    pub distribution_target_id: Uuid,
    pub publication_intent_id: Uuid,
    pub channel_target_id: Uuid,
}

/// The default is a conservative server-side capability registry. These
/// connectors have not been independently verified for this execution, so a
/// project can inspect their frozen coverage without producing a send command.
/// An authenticated connector probe may supply a versioned snapshot instead.
fn default_capabilities() -> Vec<PlatformPlacement> {
    ["baidu_creator", "xiaohongshu", "zhihu"]
        .into_iter()
        .map(|id| PlatformPlacement {
            platform_id: id.into(),
            placement_slot: "primary".into(),
            capability_version: "unverified-v1".into(),
            supported_formats: Vec::new(),
            unavailable_reason: Some("connector_unverified".into()),
            fixture: false,
        })
        .collect()
}

#[derive(Clone)]
pub struct DistributionService {
    distribution: Arc<dyn DistributionRepository>,
    content: Arc<dyn ContentRepository>,
    knowledge: Arc<dyn KnowledgeRepository>,
    projects: Arc<dyn ProjectRepository>,
    channels: Arc<dyn ChannelRepository>,
    capabilities: Arc<Vec<PlatformPlacement>>,
    registry: Option<Arc<dyn ConnectorCapabilityRepository>>,
    browser: Option<crate::BrowserBridge>,
}

impl DistributionService {
    pub fn new(
        distribution: Arc<dyn DistributionRepository>,
        content: Arc<dyn ContentRepository>,
        knowledge: Arc<dyn KnowledgeRepository>,
        projects: Arc<dyn ProjectRepository>,
        channels: Arc<dyn ChannelRepository>,
    ) -> Self {
        Self {
            distribution,
            content,
            knowledge,
            projects,
            channels,
            capabilities: Arc::new(default_capabilities()),
            registry: None,
            browser: None,
        }
    }

    /// The production resolver reads trusted deployed versions from the
    /// authenticated runner and independent operator verification at freeze.
    pub fn with_connector_registry(
        mut self,
        registry: Arc<dyn ConnectorCapabilityRepository>,
        browser: Option<crate::BrowserBridge>,
    ) -> Self {
        self.registry = Some(registry);
        self.browser = browser;
        self
    }

    /// Only a trusted, server-side capability probe can supply this snapshot.
    /// It is copied into the immutable manifest at freeze, never accepted in
    /// request JSON and never inferred from the number of attached accounts.
    pub fn with_capability_snapshot(mut self, capabilities: Vec<PlatformPlacement>) -> Self {
        self.capabilities = Arc::new(capabilities);
        self
    }

    pub fn repository(&self) -> Arc<dyn DistributionRepository> {
        Arc::clone(&self.distribution)
    }

    pub(crate) async fn current_capabilities(
        &self,
        operator: geo_domain::OperatorId,
        semantic_types: &BTreeSet<String>,
    ) -> Result<Vec<PlatformPlacement>, AppError> {
        let Some(registry) = &self.registry else {
            return Ok(self.capabilities.as_ref().clone());
        };
        let configured = registry.list(operator).await?;
        let mut keys: BTreeSet<(String, String)> = default_capabilities()
            .into_iter()
            .map(|place| (place.platform_id, place.placement_slot))
            .collect();
        keys.extend(configured.iter().map(|item| {
            (
                item.key.platform_id.clone(),
                item.key.placement_slot.clone(),
            )
        }));
        let connectors = if let Some(browser) = &self.browser {
            browser
                .capabilities()
                .await
                .map(|response| response.connectors)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut placements = Vec::with_capacity(keys.len());
        for (platform_id, placement_slot) in keys {
            let key = ConnectorKey {
                platform_id: platform_id.clone(),
                placement_slot: placement_slot.clone(),
            };
            let deployed =
                crate::connector_capabilities::deployed_version(&connectors, &key).unwrap_or("");
            let settings = registry.get(operator, &key).await?;
            let mut supported_formats = Vec::new();
            let reason = match settings {
                None => Some("connector_unverified"),
                Some(settings) if !settings.enabled => Some("connector_disabled"),
                Some(settings) => {
                    let mut failure = Some("connector_unverified");
                    for semantic in semantic_types {
                        let Some(proof_format) =
                            crate::connector_capabilities::configured_publication_format(
                                &settings, semantic,
                            )
                        else {
                            continue;
                        };
                        let resolved = registry
                            .resolve(operator, &key, deployed, proof_format)
                            .await?;
                        if resolved.availability == ConnectorAvailability::Available {
                            // A frozen placement describes document semantics,
                            // not the format of the bytes delivered to the runner.
                            supported_formats.push(semantic.clone());
                            failure = None;
                        } else if resolved.availability == ConnectorAvailability::VersionMismatch {
                            failure = Some("connector_version_mismatch");
                        }
                    }
                    failure
                }
            };
            placements.push(PlatformPlacement {
                platform_id,
                placement_slot,
                capability_version: if deployed.is_empty() {
                    "unverified-v1".into()
                } else {
                    deployed.into()
                },
                supported_formats,
                unavailable_reason: reason.map(str::to_owned),
                fixture: false,
            });
        }
        Ok(placements)
    }

    async fn require_active(&self, scope: &TenantScope) -> Result<ProjectId, AppError> {
        let project_id = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        let project = self
            .projects
            .get(scope, project_id)
            .await?
            .ok_or_else(|| AppError::not_found("project not found"))?;
        if project.status != ProjectStatus::Active {
            return Err(AppError::conflict("project is not active"));
        }
        Ok(project_id)
    }

    pub async fn latest_for_cycle(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<Option<DistributionManifest>, AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        self.projects
            .get_report_cycle(scope, project, cycle_id)
            .await?
            .ok_or_else(|| AppError::not_found("cycle not found"))?;
        self.distribution.latest_for_cycle(scope, cycle_id).await
    }

    pub async fn get(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
    ) -> Result<DistributionManifest, AppError> {
        self.distribution.get(scope, manifest_id).await
    }

    pub async fn targets(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        after_ordinal: Option<u64>,
        limit: usize,
    ) -> Result<DistributionTargetPage, AppError> {
        if !(1..=100).contains(&limit) {
            return Err(AppError::invalid_request("limit must be between 1 and 100"));
        }
        self.distribution
            .list_targets(scope, manifest_id, after_ordinal, limit)
            .await
    }

    pub async fn target(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        target_id: Uuid,
    ) -> Result<DistributionTarget, AppError> {
        self.distribution
            .get_target(scope, manifest_id, target_id)
            .await
    }

    /// Resolve a coverage cell's persisted intent to the original send target.
    /// Reused cells may point to a target in an earlier manifest or cycle.
    pub async fn publication_target(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        target_id: Uuid,
    ) -> Result<Option<PublicationTargetReference>, AppError> {
        let manifest = self.distribution.get(scope, manifest_id).await?;
        let target = self
            .distribution
            .get_target(scope, manifest_id, target_id)
            .await?;
        let Some(intent_id) = target.publication_intent_id else {
            return Ok(None);
        };
        let bundle = self
            .distribution
            .get_publication_bundle(scope, intent_id)
            .await?;
        bundle.validate_origin()?;
        if target.target_id != target_id
            || target.manifest_id != manifest.manifest_id
            || bundle.intent.intent_id != intent_id
            || bundle.intent.project_id != manifest.project_id
            || Some(bundle.intent.content_revision_id) != target.content_revision_id
            || bundle.intent.platform_id != target.platform_id
            || bundle.intent.placement_slot != target.placement_slot
            || !match &bundle.origin {
                PublicationOrigin::CoverageTarget { target: original } => {
                    bundle.intent.channel_target_id == original.target_id
                        && bundle.command.target_id == original.target_id
                }
                PublicationOrigin::ContentRequest { request } => {
                    request.scope == *scope
                        && request.publication_intent_id == Some(intent_id)
                        && bundle.intent.channel_target_id.is_nil()
                        && bundle.command.target_id.is_nil()
                }
            }
        {
            return Err(AppError::conflict(
                "publication target binding is inconsistent",
            ));
        }
        Ok(Some(PublicationTargetReference {
            distribution_target_id: target.target_id,
            publication_intent_id: intent_id,
            // Channel jobs use command_id. channel_target_id on the old
            // intent JSON is a COVERAGE target, not a channel job identity.
            channel_target_id: bundle.command.command_id,
        }))
    }

    pub async fn freeze(
        &self,
        scope: &TenantScope,
        cycle_id: Uuid,
    ) -> Result<DistributionManifest, AppError> {
        self.require_active(scope).await?;
        // Once frozen, edits to project settings, account rosters, or connector
        // capabilities cannot change the historical coverage denominator.
        if let Some(frozen) = self.distribution.latest_for_cycle(scope, cycle_id).await? {
            return Ok(frozen);
        }
        let project_id = scope.project_id.expect("require_active checked project");
        let cycle = self
            .projects
            .get_report_cycle(scope, project_id, cycle_id)
            .await?
            .ok_or_else(|| AppError::not_found("cycle not found"))?;
        let planned = cycle
            .document_manifest
            .ok_or_else(|| AppError::conflict("cycle has no document manifest"))?;
        if cycle.distribution_manifest.is_none() {
            return Err(AppError::conflict(
                "cycle has no distribution manifest selector",
            ));
        }
        let document_manifest = self
            .knowledge
            .get_document_manifest(scope, planned.manifest_id)
            .await?
            .ok_or_else(|| AppError::conflict("frozen document manifest unavailable"))?;
        if !document_manifest.sealed
            || document_manifest.manifest_id != planned.manifest_id
            || document_manifest.revision != planned.revision
        {
            return Err(AppError::conflict(
                "cycle document manifest is not sealed at the accepted revision",
            ));
        }
        let mut executions = self.content.list_executions(scope, cycle_id).await?;
        executions.retain(|execution| {
            execution.manifest_id == planned.manifest_id
                && execution.manifest_revision == planned.revision
                && execution.status == ContentExecutionStatus::Closed
        });
        if executions.len() != 1 {
            return Err(AppError::conflict(
                "cycle requires exactly one closed matching content handoff",
            ));
        }
        let execution = executions.remove(0);
        let handoff = self
            .content
            .get_handoff(scope, execution.execution_id)
            .await?
            .ok_or_else(|| AppError::conflict("closed content handoff unavailable"))?;
        if execution.handoff_id != Some(handoff.handoff_id) {
            return Err(AppError::conflict("content handoff has changed"));
        }
        let settings = self
            .projects
            .get_cycle_settings(scope, project_id, cycle_id)
            .await?
            .ok_or_else(|| AppError::conflict("frozen cycle settings unavailable"))?;
        let platforms = &settings.distribution_scope;
        let semantic_types = document_manifest
            .items
            .iter()
            .map(|item| item.content_type.clone())
            .filter(|semantic| publication_format_for_semantic_type(semantic).is_some())
            .collect();
        let capabilities = self
            .current_capabilities(scope.operator_id, &semantic_types)
            .await?;
        let excluded: BTreeSet<_> = platforms.excluded_platform_ids.iter().cloned().collect();
        let mut selected: BTreeSet<String> = match platforms.mode {
            DistributionScopeMode::Explicit => {
                platforms.included_platform_ids.iter().cloned().collect()
            }
            DistributionScopeMode::AllEligible => capabilities
                .iter()
                .map(|placement| placement.platform_id.clone())
                .collect(),
        };
        selected.retain(|platform| !excluded.contains(platform));
        if selected.is_empty() {
            return Err(AppError::conflict(
                "frozen distribution scope has no platform placements",
            ));
        }
        let placements = selected
            .into_iter()
            .map(|platform_id| {
                capabilities
                    .iter()
                    .find(|entry| entry.platform_id == platform_id)
                    .cloned()
                    .unwrap_or(PlatformPlacement {
                        platform_id,
                        placement_slot: "primary".into(),
                        capability_version: "unconfigured-v1".into(),
                        supported_formats: Vec::new(),
                        unavailable_reason: Some("connector_unconfigured".into()),
                        fixture: false,
                    })
            })
            .collect();
        self.distribution
            .freeze(
                scope,
                FreezeDistribution {
                    cycle_id,
                    revision: 1,
                    document_manifest,
                    content_execution: execution,
                    content_handoff: handoff,
                    placements,
                    sealed_at: Utc::now(),
                },
            )
            .await
    }

    /// Bounded materialization; repeated calls continue from the durable
    /// cursor. A frozen variant references the ready revision at handoff, not
    /// an editable asset's current revision.
    pub async fn resume(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        max_pages: usize,
    ) -> Result<DistributionManifest, AppError> {
        self.resume_from(scope, manifest_id, max_pages, None).await
    }

    /// `after_ordinal` lets a caller revisit every materialized cell after a
    /// crash or an account/source change, without scanning the Cartesian
    /// product in one request. The normal first pass still commits frozen
    /// coverage pages independently of whether any account is available.
    pub async fn resume_from(
        &self,
        scope: &TenantScope,
        manifest_id: Uuid,
        max_pages: usize,
        after_ordinal: Option<u64>,
    ) -> Result<DistributionManifest, AppError> {
        self.require_active(scope).await?;
        if !(1..=MAX_PAGES_PER_REQUEST).contains(&max_pages) {
            return Err(AppError::invalid_request("invalid expansion page count"));
        }
        let mut manifest = self.distribution.get(scope, manifest_id).await?;
        for _ in 0..max_pages {
            if manifest.complete {
                break;
            }
            let page = self
                .distribution
                .expansion_page(scope, manifest_id, manifest.expansion_cursor, PAGE_LIMIT)
                .await?;
            if page.cursor != manifest.expansion_cursor || page.rows.is_empty() {
                return Err(AppError::conflict("distribution expansion did not advance"));
            }
            manifest = self
                .distribution
                .commit_expansion_page(scope, manifest_id, page.cursor, page.rows.clone())
                .await?;
        }
        let accounts = self.available_accounts(scope).await?;
        let mut cursor = after_ordinal;
        for _ in 0..max_pages {
            let page = self
                .distribution
                .list_targets(scope, manifest_id, cursor, PAGE_LIMIT)
                .await?;
            if page.rows.is_empty() {
                break;
            }
            for row in page.rows {
                if row.status == DistributionTargetStatus::Pending
                    || matches!(
                        row.reason.as_deref(),
                        Some(
                            "account_unassigned"
                                | "source_unavailable"
                                | "source_changed"
                                | "content_unsupported"
                        )
                    )
                {
                    self.materialize_cell(scope, &manifest, row, &accounts)
                        .await?;
                }
            }
            let Some(next) = page.next_ordinal else {
                break;
            };
            cursor = Some(next);
        }
        Ok(manifest)
    }

    async fn available_accounts(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<ChannelAccount>, AppError> {
        let mut accounts = self.channels.list_accounts(scope).await?;
        accounts.extend(
            self.channels
                .list_assigned_pool_accounts(scope)
                .await?
                .into_iter()
                .map(|account| account.assigned_view(scope.project_id.expect("scoped"))),
        );
        accounts.sort_by_key(|account| account.account_id);
        Ok(accounts)
    }

    async fn materialize_cell(
        &self,
        scope: &TenantScope,
        manifest: &DistributionManifest,
        row: DistributionTarget,
        accounts: &[ChannelAccount],
    ) -> Result<(), AppError> {
        let account_id = accounts
            .iter()
            .find(|account| {
                account.platform == row.platform_id
                    && account.enabled
                    && account.status == ChannelStatus::Ready
            })
            .map(|account| account.account_id);
        let checked = self.checked_revision(scope, manifest, &row).await?;
        let (revision, eligibility, defer_reason) = match checked {
            Ok((revision, eligibility)) if account_id.is_some() => {
                (Some(revision), Some(eligibility), None)
            }
            Ok(_) => (None, None, None),
            Err(reason) => (None, None, Some(reason)),
        };
        let prepared = PreparedDistribution {
            manifest_id: manifest.manifest_id,
            target_id: row.target_id,
            revision,
            account_id,
            defer_reason,
        };
        if let Some(eligibility) = eligibility {
            // All repository reads above are preliminary. Keep the project
            // and complete frozen source dependencies locked across the
            // outbox commit, in that order. PostgreSQL's transactional guard
            // delegates the same checks to materialize's own transaction.
            let project_guard = self
                .projects
                .hold_content_project(scope, manifest.project_id)
                .await?;
            let knowledge_guard = self
                .knowledge
                .hold_content_evidence(scope, &[eligibility])
                .await?;
            let materialized = self.distribution.materialize(scope, prepared).await;
            drop(knowledge_guard);
            drop(project_guard);
            materialized?;
        } else {
            self.distribution.materialize(scope, prepared).await?;
        }
        Ok(())
    }

    async fn checked_revision(
        &self,
        scope: &TenantScope,
        manifest: &DistributionManifest,
        row: &DistributionTarget,
    ) -> Result<
        Result<(ContentRevision, ContentPublicEligibility), DistributionDeferralReason>,
        AppError,
    > {
        let Some(item) = self
            .content
            .get_item(scope, manifest.content_execution_id, row.document_item_id)
            .await?
        else {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        };
        let Some(frozen_id) = row.content_revision_id else {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        };
        if item.execution_id != manifest.content_execution_id {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        }
        // Editing after freeze may make the *current* item Drafted and start a
        // new handoff. It must neither overwrite nor invalidate the original
        // checked revision frozen by this distribution manifest.
        let Some(handoff) = self
            .content
            .list_handoffs(scope, manifest.content_execution_id)
            .await?
            .into_iter()
            .find(|handoff| handoff.handoff_id == manifest.content_handoff_id)
        else {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        };
        if !handoff.items.iter().any(|covered| {
            covered.item_id == row.document_item_id
                && covered.status == ContentItemStatus::Ready
                && covered.revision_id == Some(frozen_id)
        }) {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        }
        // The repository resolves both local revisions and an explicitly
        // bound, independently checked origin. A reused asset is never
        // misrepresented as belonging to this destination execution.
        let Some(revision) = self
            .content
            .resolve_checked_revision(
                scope,
                manifest.content_execution_id,
                row.document_item_id,
                frozen_id,
            )
            .await?
        else {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        };
        if revision.revision_id != frozen_id
            || revision.findings.iter().any(|finding| finding.blocking)
            || revision.evidence.is_empty()
            || revision.markdown != revision.document.markdown()
        {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        }
        let Some(planned) = self
            .knowledge
            .get_document_manifest(scope, manifest.document_manifest_id)
            .await?
        else {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        };
        if !planned.sealed || planned.revision != manifest.document_manifest_revision {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        }
        let Some(planned_item) = planned
            .items
            .iter()
            .find(|planned| planned.document_manifest_item_id == row.document_item_id)
        else {
            return Ok(Err(DistributionDeferralReason::ContentUnsupported));
        };
        let Some(release) = self
            .knowledge
            .get_release(scope, planned.knowledge_release_id)
            .await?
        else {
            return Ok(Err(DistributionDeferralReason::SourceUnavailable));
        };
        let sources = self.knowledge.list_sources(scope).await?;
        for reference in &revision.evidence {
            if !planned_item
                .source_version_refs
                .contains(&reference.source_version_id)
                || !release
                    .source_version_refs
                    .contains(&reference.source_version_id)
            {
                return Ok(Err(DistributionDeferralReason::SourceUnavailable));
            }
            let Some(source) = sources
                .iter()
                .find(|source| source.current_version_id == Some(reference.source_version_id))
            else {
                return Ok(Err(DistributionDeferralReason::SourceChanged));
            };
            if source.state != SourceState::Active || source.purpose != KnowledgePurpose::Public {
                return Ok(Err(DistributionDeferralReason::SourceUnavailable));
            }
            let Some(detail) = self
                .knowledge
                .get_source_detail(scope, source.source_id)
                .await?
            else {
                return Ok(Err(DistributionDeferralReason::SourceUnavailable));
            };
            if !detail.versions.iter().any(|version| {
                version.source_version_id == reference.source_version_id
                    && version.source_id == source.source_id
            }) || !detail.chunks.iter().any(|chunk| {
                Some(chunk.chunk_id) == reference.chunk_id
                    && chunk.source_version_id == reference.source_version_id
                    && chunk.locator == reference.locator
                    && revision.quotes.iter().any(|quote| {
                        quote.reference == *reference
                            && if matches!(chunk.locator, geo_domain::ChunkLocator::Csv { .. }) {
                                chunk.text == quote.exact_quote
                            } else {
                                chunk
                                    .text
                                    .chars()
                                    .take(geo_domain::CONTENT_EVIDENCE_MAX_QUOTE_CHARS)
                                    .collect::<String>()
                                    == quote.exact_quote
                            }
                    })
            }) {
                return Ok(Err(DistributionDeferralReason::SourceUnavailable));
            }
        }
        let eligibility = ContentPublicEligibility {
            document_manifest_id: manifest.document_manifest_id,
            document_manifest_item_id: row.document_item_id,
            source_version_ids: planned_item.source_version_refs.clone(),
            evidence: revision.quotes.clone(),
        };
        Ok(Ok((revision, eligibility)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetQuery {
    pub after_ordinal: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeQuery {
    /// Optional scanning cursor for existing, previously expanded cells.
    /// This is not the immutable expansion cursor.
    pub after_ordinal: Option<u64>,
}

fn err(error: AppError, context: RequestContext) -> ApiError {
    api_error(error, context.request_id)
}

async fn scoped(
    state: &AppState,
    tenant: &TenantScope,
    project_id: ProjectId,
) -> Result<TenantScope, AppError> {
    state
        .project_repository()
        .get(tenant, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(TenantScope::new(
        tenant.operator_id,
        tenant.tenant_id,
        Some(project_id),
    ))
}

pub async fn cycle_manifest(
    State(state): State<AppState>,
    Path((project_id, cycle_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<DistributionManifest>, ApiError> {
    let scope = scoped(&state, &tenant, project_id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .distribution_service()
        .latest_for_cycle(&scope, cycle_id)
        .await
        .map_err(|e| err(e, context))?
        .map(Json)
        .ok_or_else(|| err(AppError::not_found("distribution not frozen"), context))
}

pub async fn freeze(
    State(state): State<AppState>,
    Path((project_id, cycle_id)): Path<(ProjectId, Uuid)>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<(StatusCode, Json<DistributionManifest>), ApiError> {
    require_project_writer(&auth).map_err(|e| err(e, context))?;
    let scope = scoped(&state, &auth.scope, project_id)
        .await
        .map_err(|e| err(e, context))?;
    let service = state.distribution_service();
    let manifest = service
        .freeze(&scope, cycle_id)
        .await
        .map_err(|e| err(e, context))?;
    let manifest = service
        .resume(&scope, manifest.manifest_id, 1)
        .await
        .map_err(|e| err(e, context))?;
    Ok((StatusCode::ACCEPTED, Json(manifest)))
}

pub async fn manifest(
    State(state): State<AppState>,
    Path((project_id, manifest_id)): Path<(ProjectId, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<DistributionManifest>, ApiError> {
    let scope = scoped(&state, &tenant, project_id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .distribution_service()
        .get(&scope, manifest_id)
        .await
        .map(Json)
        .map_err(|e| err(e, context))
}

pub async fn targets(
    State(state): State<AppState>,
    Path((project_id, manifest_id)): Path<(ProjectId, Uuid)>,
    Query(query): Query<TargetQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<DistributionTargetPage>, ApiError> {
    let scope = scoped(&state, &tenant, project_id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .distribution_service()
        .targets(
            &scope,
            manifest_id,
            query.after_ordinal,
            query.limit.unwrap_or(PAGE_LIMIT),
        )
        .await
        .map(Json)
        .map_err(|e| err(e, context))
}

pub async fn target(
    State(state): State<AppState>,
    Path((project_id, manifest_id, target_id)): Path<(ProjectId, Uuid, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<DistributionTarget>, ApiError> {
    let scope = scoped(&state, &tenant, project_id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .distribution_service()
        .target(&scope, manifest_id, target_id)
        .await
        .map(Json)
        .map_err(|e| err(e, context))
}

pub async fn publication_target(
    State(state): State<AppState>,
    Path((project_id, manifest_id, target_id)): Path<(ProjectId, Uuid, Uuid)>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Option<PublicationTargetReference>>, ApiError> {
    let scope = scoped(&state, &tenant, project_id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .distribution_service()
        .publication_target(&scope, manifest_id, target_id)
        .await
        .map(Json)
        .map_err(|e| err(e, context))
}

pub async fn resume(
    State(state): State<AppState>,
    Path((project_id, manifest_id)): Path<(ProjectId, Uuid)>,
    Query(query): Query<ResumeQuery>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<(StatusCode, Json<DistributionManifest>), ApiError> {
    require_project_writer(&auth).map_err(|e| err(e, context))?;
    let scope = scoped(&state, &auth.scope, project_id)
        .await
        .map_err(|e| err(e, context))?;
    let manifest = state
        .distribution_service()
        .resume_from(
            &scope,
            manifest_id,
            MAX_PAGES_PER_REQUEST,
            query.after_ordinal,
        )
        .await
        .map_err(|e| err(e, context))?;
    Ok((StatusCode::ACCEPTED, Json(manifest)))
}
