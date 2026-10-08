//! One-shot authorization of an already claimed rich publication attempt.
//! Registration is a trusted Rust preflight operation, never a runner endpoint.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    AppError, DistributionRepository, KnowledgeRepository, MediaObjectKey,
    MemoryChannelJobRepository, MemoryContentMediaRepository, ProjectRepository, PublicationBundle,
    TenantScope, sha256_hex,
};

/// The original preflight's browser session and immutable encrypted binding.
/// No media upload, autosave, or external send is permitted by registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterPublicationSend {
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    pub account_id: Uuid,
    pub runner_session_id: Uuid,
    pub encrypted_binding_sha256: String,
    pub send_not_after: DateTime<Utc>,
}

/// Comparison-only callback input. The callback cannot choose an account,
/// browser session, deadline, content, or replacement media manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizePublicationSend {
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    pub account_id: Uuid,
    pub runner_session_id: Uuid,
    pub publication_intent_id: Uuid,
    pub payload_hash: String,
    pub encrypted_binding_sha256: String,
}

/// The only executable response. Never return secrets, bytes, or rich content
/// to the callback. The already staged immutable content belongs to the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationSendGrant {
    pub attempt_id: Uuid,
    pub runner_session_id: Uuid,
    pub payload_hash: String,
    pub send_not_after: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationSendDecision {
    Granted(PublicationSendGrant),
    /// A lost response cannot be replayed as another executable permission.
    AlreadyConsumed,
}

#[async_trait]
pub trait PublicationSendAuthorizationRepository: Send + Sync {
    /// Called exclusively by the trusted preflight after the encrypted
    /// publication binding has been committed for this exact attempt.
    async fn register_publication_send(
        &self,
        scope: &TenantScope,
        registration: &RegisterPublicationSend,
    ) -> Result<(), AppError>;

    /// The linearization point for one rich external publication. A returned
    /// grant authorizes at most one execution, never a published result.
    async fn authorize_publication_send(
        &self,
        scope: &TenantScope,
        expected: &AuthorizePublicationSend,
    ) -> Result<PublicationSendDecision, AppError>;
}

pub(crate) struct MemorySendRegistration {
    pub(crate) session_id: Uuid,
    pub(crate) deadline: DateTime<Utc>,
    pub(crate) binding_sha256: String,
    pub(crate) send_authorized_at: Option<DateTime<Utc>>,
}

/// In-memory development equivalent. These MUST be the same repository
/// instances used by publishing and withdrawal, never parallel stores.
#[derive(Clone)]
pub struct MemoryPublicationSendAuthorizationRepository {
    projects: Arc<dyn ProjectRepository>,
    jobs: MemoryChannelJobRepository,
    distribution: Arc<dyn DistributionRepository>,
    media: MemoryContentMediaRepository,
    knowledge: Arc<dyn KnowledgeRepository>,
}

impl MemoryPublicationSendAuthorizationRepository {
    pub fn new(
        projects: Arc<dyn ProjectRepository>,
        jobs: MemoryChannelJobRepository,
        distribution: Arc<dyn DistributionRepository>,
        media: MemoryContentMediaRepository,
        knowledge: Arc<dyn KnowledgeRepository>,
    ) -> Self {
        Self {
            projects,
            jobs,
            distribution,
            media,
            knowledge,
        }
    }

    async fn verify_actual_bytes(
        &self,
        scope: &TenantScope,
        bundle: &PublicationBundle,
    ) -> Result<Vec<MediaObjectKey>, AppError> {
        let payload = bundle
            .variant
            .rich_payload
            .as_ref()
            .ok_or_else(|| AppError::conflict("rich publication payload missing"))?;
        let mut keys = payload
            .media
            .iter()
            .map(|item| item.object.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        for key in &keys {
            let snapshot = self
                .knowledge
                .get_attachment_object_bytes(scope, key.object_id, key.object_version, &key.sha256)
                .await?
                .ok_or_else(|| AppError::conflict("publication media attachment unavailable"))?;
            if snapshot.object.object_version != key.object_version
                || snapshot.object.sha256 != key.sha256
                || snapshot.bytes.len() as u64 != snapshot.object.actual_size
                || sha256_hex(&snapshot.bytes) != key.sha256
            {
                return Err(AppError::conflict(
                    "publication media attachment bytes differ",
                ));
            }
        }
        Ok(keys)
    }
}

#[async_trait]
impl PublicationSendAuthorizationRepository for MemoryPublicationSendAuthorizationRepository {
    async fn register_publication_send(
        &self,
        scope: &TenantScope,
        registration: &RegisterPublicationSend,
    ) -> Result<(), AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("publication requires project scope"))?;
        let _guard = self.projects.hold_content_project(scope, project).await?;
        // No second project lock inside the channel ledger operation.
        self.jobs.register_rich_send(scope, registration).await
    }

    async fn authorize_publication_send(
        &self,
        scope: &TenantScope,
        expected: &AuthorizePublicationSend,
    ) -> Result<PublicationSendDecision, AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("publication requires project scope"))?;
        // Attachment bytes must be fetched before acquiring the project/media
        // guards: the knowledge store may itself acquire project/knowledge
        // locks. Committed objects are immutable in the memory implementation.
        let early = self
            .distribution
            .get_publication_bundle(scope, expected.publication_intent_id)
            .await?;
        let keys = self.verify_actual_bytes(scope, &early).await?;
        let _project = self.projects.hold_content_project(scope, project).await?;
        let bundle = self
            .distribution
            .get_publication_bundle(scope, expected.publication_intent_id)
            .await?;
        if bundle != early {
            return Err(AppError::conflict(
                "publication bundle changed during byte check",
            ));
        }
        let held = self.media.read_guard().await;
        let verified = held.validate(scope, &keys)?;
        // No I/O under the channel-ledger mutex; the project/media guards
        // remain held through the one-shot state transition.
        self.jobs
            .authorize_rich_send(scope, expected, &bundle, &verified)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChannelJobRepository, ChannelOutcome, ChannelOutcomeStatus, ChannelSecret, ChannelTarget,
        ChannelTargetInput, OperatorId, ProjectId, RichPublicationPayload, StructuredDocument,
        TenantId,
    };

    #[tokio::test]
    async fn memory_original_attempt_registration_is_scoped_write_once_and_nonexecuting() {
        let jobs = MemoryChannelJobRepository::default();
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let target_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let account = Uuid::new_v4();
        let revision = Uuid::new_v4();
        let intent = Uuid::new_v4();
        let target = ChannelTarget {
            target_id,
            input: ChannelTargetInput::GeneratedPublish {
                content_revision_id: revision,
                variant_id: Uuid::new_v4(),
                publication_intent_id: intent,
                distribution_target_id: Uuid::nil(),
                origin_request_id: Some(Uuid::new_v4()),
                platform: "generic".into(),
                account_id: account,
                title: "Title".into(),
                body: "Body".into(),
                body_sha256: sha256_hex(b"Body"),
                payload_hash: "hash".into(),
                evidence: vec![],
                rich_payload: Some(RichPublicationPayload {
                    schema_version: 2,
                    format: "rich_markdown.v2".into(),
                    content_revision_id: revision,
                    policy_version: "policy".into(),
                    document: StructuredDocument {
                        title: "Title".into(),
                        blocks: vec![],
                        schema_version: Some(2),
                    },
                    media: vec![],
                }),
            },
        };
        jobs.insert_generated_target(&scope, Uuid::new_v4(), target_id, target)
            .await
            .unwrap();
        let now = Utc::now();
        jobs.claim(&scope, target_id, attempt_id, now)
            .await
            .unwrap();
        let sealed = ChannelSecret::new(vec![3, 5, 8]);
        jobs.store_publication_binding(&scope, target_id, attempt_id, sealed.clone())
            .await
            .unwrap();
        let registration = RegisterPublicationSend {
            target_id,
            attempt_id,
            account_id: account,
            runner_session_id: Uuid::new_v4(),
            encrypted_binding_sha256: sha256_hex(sealed.encrypted_bytes()),
            send_not_after: now + chrono::Duration::minutes(3),
        };
        jobs.register_rich_send(&scope, &registration)
            .await
            .unwrap();
        jobs.register_rich_send(&scope, &registration)
            .await
            .unwrap();
        let mut other = registration.clone();
        other.runner_session_id = Uuid::new_v4();
        assert!(jobs.register_rich_send(&scope, &other).await.is_err());
        let foreign = TenantScope::new(
            scope.operator_id,
            TenantId::new(Uuid::new_v4()),
            scope.project_id,
        );
        assert!(
            jobs.register_rich_send(&foreign, &registration)
                .await
                .is_err()
        );
        assert!(
            jobs.get_target(&scope, target_id).await.unwrap().attempts[0]
                .outcome
                .is_none()
        );
        jobs.finish(
            &scope,
            target_id,
            attempt_id,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Verified,
                detail: None,
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
        assert_eq!(
            jobs.get_target(&scope, target_id).await.unwrap().attempts[0]
                .outcome
                .as_ref()
                .unwrap()
                .status,
            ChannelOutcomeStatus::Unknown
        );
        assert!(
            jobs.register_rich_send(&scope, &registration)
                .await
                .is_err()
        );
        let next_id = Uuid::new_v4();
        let next_attempt = Uuid::new_v4();
        let mut next = jobs.get_target(&scope, target_id).await.unwrap().target;
        next.target_id = next_id;
        jobs.insert_generated_target(&scope, Uuid::new_v4(), next_id, next)
            .await
            .unwrap();
        jobs.claim(&scope, next_id, next_attempt, Utc::now())
            .await
            .unwrap();
        jobs.finish(
            &scope,
            next_id,
            next_attempt,
            ChannelOutcome {
                status: ChannelOutcomeStatus::Published,
                detail: None,
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
        assert_eq!(
            jobs.get_target(&scope, next_id).await.unwrap().attempts[0]
                .outcome
                .as_ref()
                .unwrap()
                .status,
            ChannelOutcomeStatus::Unknown
        );
    }
}
