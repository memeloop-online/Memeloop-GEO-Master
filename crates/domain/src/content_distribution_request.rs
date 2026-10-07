//! A single-article request is an immutable acceptance and an optional link to
//! the existing publication ledger. It never represents delivery or success.
use crate::{
    AppError, CHANNEL_VARIANT_POLICY, ChannelAccount, ChannelVariant, ContentRevision,
    DistributionRepository, PublicationBundle, PublicationIntent, RICH_CHANNEL_VARIANT_POLICY,
    TenantScope,
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
    {
        return Err(AppError::conflict(
            "publication intent differs from frozen request",
        ));
    }
    Ok(())
}

#[async_trait]
pub trait ContentDistributionRequestRepository: Send + Sync {
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
}

#[async_trait]
pub trait ContentDistributionIntentLookup: Send + Sync {
    async fn get_existing_publication(
        &self,
        scope: &TenantScope,
        intent_id: Uuid,
    ) -> Result<PublicationBundle, AppError>;
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
}

#[derive(Default)]
struct MemoryRequestState {
    by_key: HashMap<(TenantScope, String), Uuid>,
    by_id: HashMap<(TenantScope, Uuid), ContentDistributionRequest>,
}

pub struct MemoryContentDistributionRequestRepository {
    state: RwLock<MemoryRequestState>,
    distribution: Arc<dyn ContentDistributionIntentLookup>,
}

impl MemoryContentDistributionRequestRepository {
    pub fn new(distribution: Arc<dyn ContentDistributionIntentLookup>) -> Self {
        Self {
            state: RwLock::new(MemoryRequestState::default()),
            distribution,
        }
    }
}

#[async_trait]
impl ContentDistributionRequestRepository for MemoryContentDistributionRequestRepository {
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
        Ok(request.clone())
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
        IntentVerification, PublicationBundle, PublicationCommand, StructuredDocument,
    };

    struct TestLookup(PublicationBundle);

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
                target,
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
            repository.accept(&scope, input.clone()).await.unwrap(),
            first
        );
        let mut changed = input.clone();
        changed.format = RICH_DISTRIBUTION_FORMAT.into();
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
