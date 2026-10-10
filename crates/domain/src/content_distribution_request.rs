//! A single-article request is an immutable acceptance and an optional link to
//! the existing publication ledger. It never represents delivery or success.
use crate::{
    AppError, CHANNEL_VARIANT_POLICY, ChannelAccount, ChannelRepository, ChannelStatus,
    ChannelVariant, ConnectorCapabilityRepository, ConnectorKey, ContentMediaBinding,
    ContentMediaRepository, ContentRepository, ContentRevision, DistributionRepository,
    KnowledgePurpose, KnowledgeRepository, MediaObjectKey, ProjectRepository, ProjectStatus,
    PublicationBundle, PublicationIntent, RICH_CHANNEL_VARIANT_POLICY, RICH_MARKDOWN_FORMAT,
    SourceState, TenantScope,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

pub const CONTENT_DISTRIBUTION_REQUEST_SCHEMA_VERSION: i32 = 1;
pub const TEXT_DISTRIBUTION_FORMAT: &str = "markdown.v1";
pub const RICH_DISTRIBUTION_FORMAT: &str = "rich_markdown.v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRequestDeferralReason {
    ProjectPaused,
    AccountUnavailable,
    ConnectorUnavailable,
    ContentNotReady,
    SourceUnavailable,
    FormatUnsupported,
    TemporaryFailure,
    InternalError,
}

impl ContentRequestDeferralReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectPaused => "project_paused",
            Self::AccountUnavailable => "account_unavailable",
            Self::ConnectorUnavailable => "connector_unavailable",
            Self::ContentNotReady => "content_not_ready",
            Self::SourceUnavailable => "source_unavailable",
            Self::FormatUnsupported => "format_unsupported",
            Self::TemporaryFailure => "temporary_failure",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRequestDeferral {
    pub reason: ContentRequestDeferralReason,
    pub attempts: u32,
    pub next_retry_at: DateTime<Utc>,
}

/// An explicitly classified live dependency failure. Arbitrary error text and
/// generic conflicts are never persisted as a more specific diagnosis.
pub struct ContentRequestMaterializationFailure {
    pub error: AppError,
    pub reason: ContentRequestDeferralReason,
}

impl From<AppError> for ContentRequestMaterializationFailure {
    fn from(error: AppError) -> Self {
        let reason = if error.code == crate::ErrorCode::DependencyUnavailable
            || error.code == crate::ErrorCode::CapabilityMissing
        {
            ContentRequestDeferralReason::TemporaryFailure
        } else {
            ContentRequestDeferralReason::InternalError
        };
        Self { error, reason }
    }
}

pub fn classified_request_failure(
    reason: ContentRequestDeferralReason,
    error: AppError,
) -> ContentRequestMaterializationFailure {
    ContentRequestMaterializationFailure { reason, error }
}

pub fn next_request_deferral(
    reason: ContentRequestDeferralReason,
    prior: Option<&ContentRequestDeferral>,
    now: DateTime<Utc>,
) -> ContentRequestDeferral {
    let attempts = prior.map_or(1, |old| old.attempts.saturating_add(1));
    let seconds = 2_i64
        .saturating_mul(1_i64 << attempts.saturating_sub(1).min(8))
        .min(300);
    ContentRequestDeferral {
        reason,
        attempts,
        next_retry_at: now + chrono::Duration::seconds(seconds),
    }
}

#[derive(Debug, Clone)]
pub struct AcceptContentDistributionRequest {
    /// These values are resolved from trusted, project-scoped Rust repositories,
    /// not supplied as a client-selectable tenant or project.
    pub revision: ContentRevision,
    pub account: ChannelAccount,
    pub placement_slot: String,
    pub format: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDistributionRequest {
    pub request_id: Uuid,
    pub scope: TenantScope,
    pub schema_version: i32,
    pub content_revision_id: Uuid,
    pub content_asset_id: Uuid,
    pub platform_id: String,
    pub placement_slot: String,
    pub account_id: Uuid,
    pub account_owner_kind: String,
    pub format: String,
    pub idempotency_key_hash: String,
    pub request_hash: String,
    /// An existing intent; its attempt, unknown outcome and verification remain
    /// exclusively in the existing publication ledger.
    pub publication_intent_id: Option<Uuid>,
    /// Mutable recovery metadata, not part of the accepted request identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization_deferral: Option<ContentRequestDeferral>,
    pub created_at: DateTime<Utc>,
}

fn hash(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    hex::encode(digest.finalize())
}

/// Hash the opaque idempotency key without consulting mutable resource state.
pub fn distribution_request_key_hash(key: &str) -> Result<String, AppError> {
    if key.is_empty() || key.len() > 256 {
        return Err(AppError::invalid_request("invalid Idempotency-Key header"));
    }
    Ok(hash(&[key]))
}

pub fn prepare_content_distribution_request(
    scope: &TenantScope,
    input: &AcceptContentDistributionRequest,
) -> Result<ContentDistributionRequest, AppError> {
    let Some(project_id) = scope.project_id else {
        return Err(AppError::forbidden("project scope required"));
    };
    if input.account.project_id != project_id {
        return Err(AppError::not_found("account not found"));
    }
    if input.account.platform.is_empty()
        || input.account.platform.len() > 120
        || input.placement_slot.is_empty()
        || input.placement_slot.len() > 120
        || ![TEXT_DISTRIBUTION_FORMAT, RICH_DISTRIBUTION_FORMAT].contains(&input.format.as_str())
        || input.idempotency_key.is_empty()
        || input.idempotency_key.len() > 256
    {
        return Err(AppError::invalid_request(
            "invalid single-article distribution request",
        ));
    }
    if (input.revision.document.schema_version == Some(2))
        != (input.format == RICH_DISTRIBUTION_FORMAT)
    {
        return Err(AppError::invalid_request(
            "single-article format does not match content schema",
        ));
    }
    Ok(ContentDistributionRequest {
        request_id: Uuid::new_v4(),
        scope: scope.clone(),
        schema_version: CONTENT_DISTRIBUTION_REQUEST_SCHEMA_VERSION,
        content_revision_id: input.revision.revision_id,
        content_asset_id: input.revision.asset_id,
        platform_id: input.account.platform.clone(),
        placement_slot: input.placement_slot.clone(),
        account_id: input.account.account_id,
        account_owner_kind: match input.account.owner_kind {
            crate::ChannelOwnerKind::Customer => "customer",
            crate::ChannelOwnerKind::OperatorPool => "operator_pool",
        }
        .into(),
        format: input.format.clone(),
        idempotency_key_hash: hash(&[&input.idempotency_key]),
        request_hash: hash(&[
            &input.revision.revision_id.to_string(),
            &input.revision.asset_id.to_string(),
            &input.account.platform,
            &input.placement_slot,
            &input.account.account_id.to_string(),
            match input.account.owner_kind {
                crate::ChannelOwnerKind::Customer => "customer",
                crate::ChannelOwnerKind::OperatorPool => "operator_pool",
            },
            &input.format,
        ]),
        publication_intent_id: None,
        materialization_deferral: None,
        created_at: Utc::now(),
    })
}

pub fn validate_distribution_request_intent(
    request: &ContentDistributionRequest,
    intent: &PublicationIntent,
    variant: &ChannelVariant,
) -> Result<(), AppError> {
    let policy = match request.format.as_str() {
        TEXT_DISTRIBUTION_FORMAT => CHANNEL_VARIANT_POLICY,
        RICH_DISTRIBUTION_FORMAT => RICH_CHANNEL_VARIANT_POLICY,
        _ => return Err(AppError::conflict("request format not supported")),
    };
    if intent.project_id != request.scope.project_id.expect("validated project")
        || intent.content_revision_id != request.content_revision_id
        || intent.platform_id != request.platform_id
        || intent.placement_slot != request.placement_slot
        || intent.account_id != request.account_id
        || intent.variant_id != variant.variant_id
        || intent.payload_hash != variant.payload_hash
        || variant.content_revision_id != request.content_revision_id
        || variant.platform_id != request.platform_id
        || variant.placement_slot != request.placement_slot
        || variant.policy_version != policy
        || (request.format == RICH_DISTRIBUTION_FORMAT) != variant.rich_payload.is_some()
    {
        return Err(AppError::conflict(
            "publication intent differs from frozen request",
        ));
    }
    if variant.rich_payload.is_some() {
        crate::validate_rich_publication_payload(variant)?;
    }
    Ok(())
}

#[async_trait]
pub trait ContentDistributionRequestRepository: Send + Sync {
    /// Read an already accepted receipt before resolving mutable revision and
    /// account authorities. Only this exact tenant/project may replay it.
    async fn get_by_idempotency_key(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> Result<Option<ContentDistributionRequest>, AppError>;
    async fn accept(
        &self,
        scope: &TenantScope,
        input: AcceptContentDistributionRequest,
    ) -> Result<ContentDistributionRequest, AppError>;
    async fn get(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError>;
    /// Link only an existing publication intent/outbox. No status transition,
    /// new intent or new send is authorized by this method.
    async fn link_intent(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
        intent_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError>;
    /// Creates or reuses the project's original immutable publication intent
    /// and command, linking this accepted request in the SAME storage commit.
    async fn materialize(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError>;
    /// Global bounded scanner; each returned row contains its trusted scope.
    /// An unlinked request is only accepted, not necessarily publishable.
    async fn list_unlinked(
        &self,
        after_request_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ContentDistributionRequest>, AppError>;
}

#[async_trait]
pub trait ContentDistributionIntentLookup: Send + Sync {
    async fn get_existing_publication(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<PublicationBundle, AppError>;
    async fn materialize_accepted_request(
        &self,
        scope: &TenantScope,
        request: &ContentDistributionRequest,
        revision: &ContentRevision,
    ) -> Result<PublicationIntent, AppError> {
        let _ = (scope, request, revision);
        Err(AppError::capability_missing(
            "request materialization unavailable",
        ))
    }
    async fn materialize_accepted_rich_request(
        &self,
        scope: &TenantScope,
        request: &ContentDistributionRequest,
        revision: &ContentRevision,
        bindings: Vec<ContentMediaBinding>,
    ) -> Result<PublicationIntent, AppError> {
        let _ = (scope, request, revision, bindings);
        Err(AppError::capability_missing(
            "rich request materialization unavailable",
        ))
    }
}

#[async_trait]
impl<T: DistributionRepository + ?Sized> ContentDistributionIntentLookup for T {
    async fn get_existing_publication(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<PublicationBundle, AppError> {
        self.get_publication_bundle(scope, intent_id).await
    }
    async fn materialize_accepted_request(
        &self,
        scope: &TenantScope,
        request: &ContentDistributionRequest,
        revision: &ContentRevision,
    ) -> Result<PublicationIntent, AppError> {
        self.materialize_request_origin(scope, request, revision)
            .await
    }
    async fn materialize_accepted_rich_request(
        &self,
        scope: &TenantScope,
        request: &ContentDistributionRequest,
        revision: &ContentRevision,
        bindings: Vec<ContentMediaBinding>,
    ) -> Result<PublicationIntent, AppError> {
        self.materialize_rich_request_origin(scope, request, revision, bindings)
            .await
    }
}

#[derive(Default)]
struct MemoryRequestState {
    by_key: HashMap<(TenantScope, String), Uuid>,
    by_id: HashMap<(TenantScope, Uuid), ContentDistributionRequest>,
    revisions: HashMap<(TenantScope, Uuid), ContentRevision>,
}

#[derive(Clone)]
pub struct MemoryContentDistributionRequestRepository {
    state: Arc<RwLock<MemoryRequestState>>,
    distribution: Arc<dyn ContentDistributionIntentLookup>,
    authorities: Option<MemoryRequestAuthorities>,
}

#[derive(Clone)]
struct MemoryRequestAuthorities {
    content: Arc<dyn ContentRepository>,
    knowledge: Arc<dyn KnowledgeRepository>,
    projects: Arc<dyn ProjectRepository>,
    channels: Arc<dyn ChannelRepository>,
    connectors: Arc<dyn ConnectorCapabilityRepository>,
    media: Option<Arc<dyn ContentMediaRepository>>,
}

impl MemoryContentDistributionRequestRepository {
    pub fn new(distribution: Arc<dyn ContentDistributionIntentLookup>) -> Self {
        Self {
            state: Arc::new(RwLock::new(MemoryRequestState::default())),
            distribution,
            authorities: None,
        }
    }

    /// Rebind a forked AppState without losing accepted request/intent links.
    /// The original wrapper retains its own lookup and cannot inherit a
    /// builder override made to another AppState clone.
    pub fn with_distribution_lookup(
        mut self,
        distribution: Arc<dyn ContentDistributionIntentLookup>,
    ) -> Self {
        self.distribution = distribution;
        self
    }

    pub fn with_authorities(
        mut self,
        content: Arc<dyn ContentRepository>,
        knowledge: Arc<dyn KnowledgeRepository>,
        projects: Arc<dyn ProjectRepository>,
        channels: Arc<dyn ChannelRepository>,
        connectors: Arc<dyn ConnectorCapabilityRepository>,
    ) -> Self {
        self.authorities = Some(MemoryRequestAuthorities {
            content,
            knowledge,
            projects,
            channels,
            connectors,
            media: None,
        });
        self
    }

    pub fn with_media_repository(mut self, media: Arc<dyn ContentMediaRepository>) -> Self {
        if let Some(authority) = self.authorities.as_mut() {
            authority.media = Some(media);
        }
        self
    }

    async fn rich_media_bindings(
        &self,
        scope: &TenantScope,
        revision: &ContentRevision,
    ) -> Result<Vec<ContentMediaBinding>, AppError> {
        let references = revision.document.media_references();
        let keys: Vec<_> = references
            .iter()
            .map(|reference| MediaObjectKey {
                object_id: reference.object_id,
                object_version: reference.object_version,
                sha256: reference.sha256.clone(),
            })
            .collect();
        let ordered = crate::ordered_media_snapshot_keys(&keys)?;
        if ordered.is_empty() {
            return Ok(Vec::new());
        }
        let repository = self
            .authorities
            .as_ref()
            .and_then(|authority| authority.media.as_ref())
            .ok_or_else(|| AppError::capability_missing("media authority unavailable"))?;
        // Validates actual committed bytes, digest and active project binding.
        // This preparation snapshot is NOT authorization to send after a
        // concurrent withdrawal; the send bridge must recheck atomically.
        let snapshots = repository
            .snapshot_authorized_images(scope, &ordered)
            .await?;
        if snapshots.len() != ordered.len() {
            return Err(AppError::conflict(
                "publication media snapshot is incomplete",
            ));
        }
        let verified: HashMap<_, _> = snapshots
            .into_iter()
            .map(|snapshot| (snapshot.image.key.clone(), snapshot.image))
            .collect();
        let mut found = HashMap::new();
        let mut after = None;
        // Existing repository pagination is by binding ID, not by object key.
        // Keep the metadata walk bounded, including for unexpectedly large
        // projects, and fail explicitly if an exact binding cannot be found.
        for _ in 0..1000 {
            let page = repository.list_bindings(scope, after, 100).await?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|binding| binding.binding_id);
            for binding in page.iter() {
                if let Some(image) = verified.get(&binding.image.key) {
                    if &binding.image != image {
                        return Err(AppError::conflict("publication image metadata changed"));
                    }
                    found.insert(binding.image.key.clone(), binding.clone());
                }
            }
            if found.len() == verified.len() {
                break;
            }
            if page.len() < 100 {
                break;
            }
        }
        ordered
            .iter()
            .map(|key| {
                found
                    .remove(key)
                    .ok_or_else(|| AppError::conflict("publication media binding unavailable"))
            })
            .collect()
    }

    async fn record_deferral(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
        reason: ContentRequestDeferralReason,
    ) {
        let mut state = self.state.write().await;
        if let Some(request) = state.by_id.get_mut(&(scope.clone(), request_id))
            && request.publication_intent_id.is_none()
        {
            request.materialization_deferral = Some(next_request_deferral(
                reason,
                request.materialization_deferral.as_ref(),
                Utc::now(),
            ));
        }
    }

    async fn check_live(
        &self,
        request: &ContentDistributionRequest,
        accepted_revision: &ContentRevision,
    ) -> Result<ContentRevision, ContentRequestMaterializationFailure> {
        let authority = self.authorities.as_ref().ok_or_else(|| {
            AppError::capability_missing("publication validation authorities unavailable")
        })?;
        let scope = &request.scope;
        let project = scope.project_id.expect("validated request scope");
        let project = authority
            .projects
            .get(scope, project)
            .await?
            .ok_or_else(|| AppError::not_found("project not found"))?;
        if matches!(
            project.status,
            ProjectStatus::Paused | ProjectStatus::Archived
        ) {
            return Err(classified_request_failure(
                ContentRequestDeferralReason::ProjectPaused,
                AppError::conflict("project is not active"),
            ));
        }
        let mut accounts = authority.channels.list_accounts(scope).await?;
        accounts.extend(
            authority
                .channels
                .list_assigned_pool_accounts(scope)
                .await?
                .iter()
                .map(|account| account.assigned_view(project.id)),
        );
        if !accounts.iter().any(|account| {
            account.account_id == request.account_id
                && account.platform == request.platform_id
                && account.enabled
                && account.status == ChannelStatus::Ready
                && match account.owner_kind {
                    crate::ChannelOwnerKind::Customer => request.account_owner_kind == "customer",
                    crate::ChannelOwnerKind::OperatorPool => {
                        request.account_owner_kind == "operator_pool"
                    }
                }
        }) {
            return Err(classified_request_failure(
                ContentRequestDeferralReason::AccountUnavailable,
                AppError::conflict("publication account is not ready"),
            ));
        }
        if request.format != TEXT_DISTRIBUTION_FORMAT && request.format != RICH_DISTRIBUTION_FORMAT
        {
            return Err(classified_request_failure(
                ContentRequestDeferralReason::FormatUnsupported,
                AppError::conflict("publication format is not supported"),
            ));
        }
        let key = ConnectorKey {
            platform_id: request.platform_id.clone(),
            placement_slot: request.placement_slot.clone(),
        };
        let configured = authority.connectors.get(scope.operator_id, &key).await?;
        let semantic = if let Some(asset) = authority
            .content
            .get_asset(scope, request.content_asset_id)
            .await?
        {
            authority
                .content
                .get_item(scope, asset.execution_id, asset.item_id)
                .await?
                .map(|item| item.content_type)
        } else {
            None
        };
        let proof_format = configured
            .filter(|settings| settings.enabled)
            .and_then(|settings| {
                if request.format == RICH_DISTRIBUTION_FORMAT {
                    return settings
                        .content_types
                        .contains(&RICH_MARKDOWN_FORMAT.to_owned())
                        .then(|| RICH_MARKDOWN_FORMAT.to_owned());
                }
                if settings
                    .content_types
                    .iter()
                    .any(|format| format == crate::PLAIN_TEXT_ARTICLE_FORMAT)
                {
                    Some(crate::PLAIN_TEXT_ARTICLE_FORMAT.to_owned())
                } else {
                    semantic.filter(|semantic| {
                        crate::publication_format_for_semantic_type(semantic).is_some()
                            && settings.content_types.contains(semantic)
                    })
                }
            })
            .ok_or_else(|| {
                classified_request_failure(
                    ContentRequestDeferralReason::ConnectorUnavailable,
                    AppError::conflict("publication format is not available"),
                )
            })?;
        if !authority
            .connectors
            .history(scope.operator_id, &key)
            .await?
            .iter()
            .any(|proof| proof.content_type == proof_format)
        {
            return Err(classified_request_failure(
                ContentRequestDeferralReason::ConnectorUnavailable,
                AppError::conflict("publication format is not available"),
            ));
        }
        let revision = authority
            .content
            .get_revision(scope, request.content_asset_id, request.content_revision_id)
            .await?
            .ok_or_else(|| {
                classified_request_failure(
                    ContentRequestDeferralReason::ContentNotReady,
                    AppError::conflict("publication revision unavailable"),
                )
            })?;
        if (revision.document.schema_version == Some(2))
            != (request.format == RICH_DISTRIBUTION_FORMAT)
        {
            return Err(classified_request_failure(
                ContentRequestDeferralReason::FormatUnsupported,
                AppError::conflict("publication format differs from revision schema"),
            ));
        }
        if revision != *accepted_revision
            || revision.findings.iter().any(|finding| finding.blocking)
            || !authority
                .content
                .list_checks(scope, request.content_revision_id)
                .await?
                .iter()
                .any(|check| {
                    check.revision_id == request.content_revision_id
                        && !check.findings.iter().any(|finding| finding.blocking)
                })
        {
            return Err(classified_request_failure(
                ContentRequestDeferralReason::ContentNotReady,
                AppError::conflict("independent content check unavailable"),
            ));
        }
        let sources = authority.knowledge.list_sources(scope).await?;
        if revision.evidence.is_empty() || revision.quotes.is_empty() {
            return Err(classified_request_failure(
                ContentRequestDeferralReason::SourceUnavailable,
                AppError::conflict("publication requires public evidence"),
            ));
        }
        for reference in &revision.evidence {
            let source = sources.iter().find(|source| {
                source.current_version_id == Some(reference.source_version_id)
                    && source.state == SourceState::Active
                    && source.purpose == KnowledgePurpose::Public
            });
            let source = source.ok_or_else(|| {
                classified_request_failure(
                    ContentRequestDeferralReason::SourceUnavailable,
                    AppError::conflict("publication source is not public"),
                )
            })?;
            let detail = authority
                .knowledge
                .get_source_detail(scope, source.source_id)
                .await?
                .ok_or_else(|| {
                    classified_request_failure(
                        ContentRequestDeferralReason::SourceUnavailable,
                        AppError::conflict("publication source unavailable"),
                    )
                })?;
            let quote = revision
                .quotes
                .iter()
                .find(|quote| quote.reference == *reference)
                .ok_or_else(|| {
                    classified_request_failure(
                        ContentRequestDeferralReason::SourceUnavailable,
                        AppError::conflict("publication quote unavailable"),
                    )
                })?;
            if !detail.chunks.iter().any(|chunk| {
                Some(chunk.chunk_id) == reference.chunk_id
                    && chunk.source_version_id == reference.source_version_id
                    && chunk.locator == reference.locator
                    && if matches!(chunk.locator, crate::ChunkLocator::Csv { .. }) {
                        chunk.text == quote.exact_quote
                    } else {
                        chunk
                            .text
                            .chars()
                            .take(crate::CONTENT_EVIDENCE_MAX_QUOTE_CHARS)
                            .collect::<String>()
                            == quote.exact_quote
                    }
            }) {
                return Err(classified_request_failure(
                    ContentRequestDeferralReason::SourceUnavailable,
                    AppError::conflict("publication quote changed"),
                ));
            }
        }
        Ok(revision)
    }
}

#[async_trait]
impl ContentDistributionRequestRepository for MemoryContentDistributionRequestRepository {
    async fn get_by_idempotency_key(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> Result<Option<ContentDistributionRequest>, AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        let key_hash = distribution_request_key_hash(key)?;
        let state = self.state.read().await;
        Ok(state
            .by_key
            .get(&(scope.clone(), key_hash))
            .and_then(|id| state.by_id.get(&(scope.clone(), *id)))
            .cloned())
    }

    async fn accept(
        &self,
        scope: &TenantScope,
        input: AcceptContentDistributionRequest,
    ) -> Result<ContentDistributionRequest, AppError> {
        let request = prepare_content_distribution_request(scope, &input)?;
        let mut state = self.state.write().await;
        let key = (scope.clone(), request.idempotency_key_hash.clone());
        if let Some(id) = state.by_key.get(&key) {
            let old = state.by_id[&(scope.clone(), *id)].clone();
            return if old.request_hash == request.request_hash {
                Ok(old)
            } else {
                Err(AppError::conflict(
                    "idempotency key reused for another request",
                ))
            };
        }
        state.by_key.insert(key, request.request_id);
        state
            .by_id
            .insert((scope.clone(), request.request_id), request.clone());
        state
            .revisions
            .insert((scope.clone(), request.request_id), input.revision);
        Ok(request)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError> {
        state_get(&*self.state.read().await, scope, request_id)
    }

    async fn link_intent(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
        intent_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError> {
        let bundle = self
            .distribution
            .get_existing_publication(scope, intent_id)
            .await?;
        let mut state = self.state.write().await;
        let request = state
            .by_id
            .get_mut(&(scope.clone(), request_id))
            .ok_or_else(|| AppError::not_found("distribution request not found"))?;
        validate_distribution_request_intent(request, &bundle.intent, &bundle.variant)?;
        if request
            .publication_intent_id
            .is_some_and(|old| old != intent_id)
        {
            return Err(AppError::conflict(
                "distribution request already linked to another intent",
            ));
        }
        request.publication_intent_id = Some(intent_id);
        request.materialization_deferral = None;
        Ok(request.clone())
    }

    async fn materialize(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError> {
        let result: Result<_, ContentRequestMaterializationFailure> = async {
            // Validate against live authorities without holding the request write
            // lock through independent repository I/O. The accepted revision is
            // immutable; the second read below serializes only linking and the
            // shared distribution write (request → distribution lock order).
            let (request, revision) = {
                let state = self.state.read().await;
                let request = state_get(&state, scope, request_id)?;
                if request.publication_intent_id.is_some() {
                    return Ok(request);
                }
                let revision = state
                    .revisions
                    .get(&(scope.clone(), request_id))
                    .ok_or_else(|| AppError::not_found("accepted revision not found"))?
                    .clone();
                (request, revision)
            };
            let revision = self.check_live(&request, &revision).await?;
            let rich_bindings = if request.format == RICH_DISTRIBUTION_FORMAT {
                Some(self.rich_media_bindings(scope, &revision).await?)
            } else {
                None
            };
            let mut state = self.state.write().await;
            let latest = state_get(&state, scope, request_id)?;
            if latest.publication_intent_id.is_some() {
                return Ok(latest);
            }
            if latest.request_hash != request.request_hash
                || latest.content_revision_id != request.content_revision_id
            {
                return Err(
                    AppError::conflict("accepted request changed during validation").into(),
                );
            }
            let intent = if let Some(bindings) = rich_bindings {
                self.distribution
                    .materialize_accepted_rich_request(scope, &request, &revision, bindings)
                    .await?
            } else {
                self.distribution
                    .materialize_accepted_request(scope, &request, &revision)
                    .await?
            };
            let saved = state
                .by_id
                .get_mut(&(scope.clone(), request_id))
                .expect("held request row");
            saved.publication_intent_id = Some(intent.intent_id);
            saved.materialization_deferral = None;
            Ok(saved.clone())
        }
        .await;
        match result {
            Ok(request) => Ok(request),
            Err(failure) => {
                self.record_deferral(scope, request_id, failure.reason)
                    .await;
                Err(failure.error)
            }
        }
    }

    async fn list_unlinked(
        &self,
        after_request_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ContentDistributionRequest>, AppError> {
        if !(1..=1000).contains(&limit) {
            return Err(AppError::invalid_request("invalid request scan page size"));
        }
        let state = self.state.read().await;
        let mut rows: Vec<_> = state
            .by_id
            .values()
            .filter(|request| {
                request.publication_intent_id.is_none()
                    && request
                        .materialization_deferral
                        .as_ref()
                        .is_none_or(|deferral| deferral.next_retry_at <= Utc::now())
                    && after_request_id.is_none_or(|after| request.request_id > after)
            })
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.request_id);
        rows.truncate(limit);
        Ok(rows)
    }
}

fn state_get(
    state: &MemoryRequestState,
    scope: &TenantScope,
    request_id: Uuid,
) -> Result<ContentDistributionRequest, AppError> {
    state
        .by_id
        .get(&(scope.clone(), request_id))
        .cloned()
        .ok_or_else(|| AppError::not_found("distribution request not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChannelOwnerKind, ChannelStatus, DistributionTarget, DistributionTargetStatus, ErrorCode,
        IntentVerification, PublicationBundle, PublicationCommand, PublicationOrigin,
        StructuredDocument,
    };

    struct TestLookup(PublicationBundle);

    #[test]
    fn deferral_delay_grows_and_caps_without_a_terminal_retry_state() {
        let now = Utc::now();
        let mut previous = None;
        for attempt in 1..=12 {
            let current = next_request_deferral(
                ContentRequestDeferralReason::ContentNotReady,
                previous.as_ref(),
                now,
            );
            assert_eq!(current.attempts, attempt);
            let expected = (2_i64 * (1_i64 << (attempt - 1).min(8))).min(300);
            assert_eq!((current.next_retry_at - now).num_seconds(), expected);
            previous = Some(current);
        }
        assert_eq!(previous.unwrap().attempts, 12);
    }

    #[async_trait]
    impl ContentDistributionIntentLookup for TestLookup {
        async fn get_existing_publication(
            &self,
            scope: &TenantScope,
            intent_id: Uuid,
        ) -> Result<PublicationBundle, AppError> {
            if self.0.intent.intent_id == intent_id
                && self.0.intent.project_id == scope.project_id.unwrap()
            {
                Ok(self.0.clone())
            } else {
                Err(AppError::not_found("publication intent not found"))
            }
        }
    }

    fn fixture(scope: &TenantScope) -> (AcceptContentDistributionRequest, PublicationBundle) {
        let project_id = scope.project_id.unwrap();
        let revision = ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id: Uuid::new_v4(),
            revision: 1,
            base_revision_id: None,
            derived_from_revision_id: None,
            document: StructuredDocument {
                title: "Example".into(),
                blocks: vec![],
                schema_version: None,
            },
            markdown: "# Example".into(),
            evidence: vec![],
            quotes: vec![],
            findings: vec![],
            created_at: Utc::now(),
        };
        let account = ChannelAccount {
            account_id: Uuid::new_v4(),
            project_id,
            owner_kind: ChannelOwnerKind::Customer,
            platform: "platform".into(),
            group_id: None,
            status: ChannelStatus::Ready,
            display_name: None,
            platform_account_id: None,
            avatar_url: None,
            enabled: true,
            proxy_configured: false,
            proxy_server: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let input = AcceptContentDistributionRequest {
            revision: revision.clone(),
            account: account.clone(),
            placement_slot: "primary".into(),
            format: TEXT_DISTRIBUTION_FORMAT.into(),
            idempotency_key: "same-key".into(),
        };
        let variant = ChannelVariant {
            variant_id: Uuid::new_v4(),
            content_revision_id: revision.revision_id,
            platform_id: account.platform.clone(),
            placement_slot: input.placement_slot.clone(),
            policy_version: CHANNEL_VARIANT_POLICY.into(),
            title: "Example".into(),
            markdown: "# Example".into(),
            payload_hash: "payload".into(),
            evidence: vec![],
            rich_payload: None,
        };
        let intent = PublicationIntent {
            intent_id: Uuid::new_v4(),
            project_id,
            channel_target_id: Uuid::new_v4(),
            variant_id: variant.variant_id,
            content_revision_id: revision.revision_id,
            platform_id: account.platform,
            placement_slot: input.placement_slot.clone(),
            account_id: account.account_id,
            payload_hash: variant.payload_hash.clone(),
            logical_key: "stable".into(),
            verification: IntentVerification::Unknown,
            verification_evidence_id: Some(Uuid::new_v4()),
            created_at: Utc::now(),
        };
        let target = DistributionTarget {
            target_id: intent.channel_target_id,
            manifest_id: Uuid::new_v4(),
            ordinal: 0,
            document_item_id: Uuid::new_v4(),
            content_revision_id: Some(revision.revision_id),
            platform_id: intent.platform_id.clone(),
            placement_slot: intent.placement_slot.clone(),
            variant_id: Some(variant.variant_id),
            account_id: Some(intent.account_id),
            publication_intent_id: Some(intent.intent_id),
            status: DistributionTargetStatus::ReusedUnknown,
            reason: None,
            version: 1,
        };
        let command = PublicationCommand {
            command_id: Uuid::new_v4(),
            intent_id: intent.intent_id,
            target_id: target.target_id,
            payload_hash: intent.payload_hash.clone(),
            fixture: true,
        };
        (
            input,
            PublicationBundle {
                revision,
                variant,
                intent,
                origin: PublicationOrigin::CoverageTarget { target },
                command,
            },
        )
    }

    #[tokio::test]
    async fn acceptance_is_scoped_idempotent_and_does_not_create_an_intent() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let (input, bundle) = fixture(&scope);
        let repository =
            MemoryContentDistributionRequestRepository::new(Arc::new(TestLookup(bundle)));
        let first = repository.accept(&scope, input.clone()).await.unwrap();
        assert_eq!(first.publication_intent_id, None);
        assert_eq!(
            repository
                .get_by_idempotency_key(&scope, &input.idempotency_key)
                .await
                .unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            repository.accept(&scope, input.clone()).await.unwrap(),
            first
        );
        let mut changed = input.clone();
        changed.format = RICH_DISTRIBUTION_FORMAT.into();
        assert_eq!(
            repository.accept(&scope, changed).await.unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        let mut changed = input.clone();
        changed.placement_slot = "different".into();
        assert_eq!(
            repository.accept(&scope, changed).await.unwrap_err().code,
            ErrorCode::Conflict
        );
        let another = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert_eq!(
            repository
                .get_by_idempotency_key(&another, &input.idempotency_key)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            repository
                .get(&another, first.request_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            repository.accept(&another, input).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn deferred_memory_requests_keep_only_allowlisted_reasons_and_due_paging() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let (input, bundle) = fixture(&scope);
        let intent_id = bundle.intent.intent_id;
        let repository =
            MemoryContentDistributionRequestRepository::new(Arc::new(TestLookup(bundle)));
        let first = repository.accept(&scope, input.clone()).await.unwrap();
        assert!(
            serde_json::to_string(&first)
                .unwrap()
                .find("materialization_deferral")
                .is_none()
        );
        let error = repository
            .materialize(&scope, first.request_id)
            .await
            .unwrap_err();
        let deferred = repository.get(&scope, first.request_id).await.unwrap();
        assert_eq!(
            deferred.materialization_deferral.as_ref().unwrap().reason,
            ContentRequestDeferralReason::TemporaryFailure
        );
        assert_eq!(
            deferred.materialization_deferral.as_ref().unwrap().attempts,
            1
        );
        let json = serde_json::to_string(&deferred).unwrap();
        assert!(json.contains("temporary_failure"));
        assert!(!json.contains(&error.message));

        let mut second_input = input;
        second_input.idempotency_key = "second-key".into();
        let second = repository.accept(&scope, second_input).await.unwrap();
        let due = repository.list_unlinked(None, 100).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].request_id, second.request_id);
        assert!(
            repository
                .list_unlinked(Some(second.request_id), 100)
                .await
                .unwrap()
                .is_empty()
        );

        // Expiring the clock is enough for automatic recovery; no terminal
        // status or explicit retry toggle is needed.
        repository
            .state
            .write()
            .await
            .by_id
            .get_mut(&(scope.clone(), first.request_id))
            .unwrap()
            .materialization_deferral
            .as_mut()
            .unwrap()
            .next_retry_at = Utc::now() - chrono::Duration::seconds(1);
        assert!(
            repository
                .list_unlinked(None, 100)
                .await
                .unwrap()
                .iter()
                .any(|request| request.request_id == first.request_id)
        );
        let linked = repository
            .link_intent(&scope, first.request_id, intent_id)
            .await
            .unwrap();
        assert!(linked.materialization_deferral.is_none());
        // Model a slow, already-failed validator reaching persistence after
        // another task completed the link. The stale result cannot regress it.
        repository
            .record_deferral(
                &scope,
                first.request_id,
                ContentRequestDeferralReason::InternalError,
            )
            .await;
        let still_linked = repository.get(&scope, first.request_id).await.unwrap();
        assert_eq!(still_linked.publication_intent_id, Some(intent_id));
        assert!(still_linked.materialization_deferral.is_none());
        assert!(
            repository
                .list_unlinked(None, 100)
                .await
                .unwrap()
                .iter()
                .all(|request| request.request_id != first.request_id)
        );
    }

    #[tokio::test]
    async fn linking_reuses_an_unknown_intent_and_rejects_conflicting_link_or_fields() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let (input, bundle) = fixture(&scope);
        let intent_id = bundle.intent.intent_id;
        let repository =
            MemoryContentDistributionRequestRepository::new(Arc::new(TestLookup(bundle.clone())));
        let request = repository.accept(&scope, input.clone()).await.unwrap();
        let linked = repository
            .link_intent(&scope, request.request_id, intent_id)
            .await
            .unwrap();
        assert_eq!(linked.publication_intent_id, Some(intent_id));
        assert_eq!(
            repository
                .link_intent(&scope, request.request_id, intent_id)
                .await
                .unwrap(),
            linked
        );
        assert_eq!(
            repository
                .link_intent(&scope, request.request_id, Uuid::new_v4())
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        let mut different_account = input;
        different_account.idempotency_key = "other".into();
        different_account.account.account_id = Uuid::new_v4();
        let other = repository.accept(&scope, different_account).await.unwrap();
        assert_eq!(
            repository
                .link_intent(&scope, other.request_id, intent_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            repository
                .get(&scope, other.request_id)
                .await
                .unwrap()
                .publication_intent_id,
            None
        );
        let other_scope = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert_eq!(
            repository
                .link_intent(&other_scope, request.request_id, intent_id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
}
