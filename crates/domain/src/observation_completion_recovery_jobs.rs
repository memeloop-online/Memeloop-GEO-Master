//! Independent, read-only completion recovery scheduling. A lease never proves
//! browser disposal and never permits resubmission or remote conversation deletion.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    AppError, CapturedConversationPurpose, ConversationCorrelation, ObservationCapture,
    ObservationCaptureSnapshot, TenantScope,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCompletionRecoveryRequestBinding {
    pub provider: String,
    pub platform_account_id: String,
    pub external_conversation_id: String,
    pub request_message_id: String,
    pub response_message_id: String,
}

impl ObservationCapture {
    /// Load this capture from authoritative storage under `scope` first. The
    /// receipt validates integrity, not tenant ownership. Transport fields only;
    /// answers, titles and newest-chat heuristics cannot supply missing proof.
    pub fn recovery_request_binding(
        &self,
        scope: &TenantScope,
    ) -> Result<ObservationCompletionRecoveryRequestBinding, AppError> {
        let invalid = || AppError::invalid_request("completion recovery source is not eligible");
        if scope.project_id.is_none()
            || scope.operator_id.as_uuid().is_nil()
            || scope.tenant_id.as_uuid().is_nil()
            || scope.project_id.is_some_and(|id| id.as_uuid().is_nil())
        {
            return Err(AppError::forbidden("completion recovery scope required"));
        }
        if self.receipt.schema_version != 1
            || self.receipt.capture_id != self.input.capture_id
            || self.input.validate(scope)? != self.receipt.digest_sha256
            || self.input.ordinal != 0
            || self.input.completion.is_some()
        {
            return Err(invalid());
        }
        let identity = self.input.original_identity.as_ref().ok_or_else(invalid)?;
        identity.validate()?;
        let owned = self.input.owned_conversation.as_ref().ok_or_else(invalid)?;
        if identity.provider != owned.provider
            || owned.purpose != CapturedConversationPurpose::Measurement
            || owned.correlation != ConversationCorrelation::CreateResponse
        {
            return Err(invalid());
        }
        let ObservationCaptureSnapshot::Source { source_json, .. } = &self.input.snapshot else {
            return Err(invalid());
        };
        let source = serde_json::from_str(source_json).map_err(|_| invalid())?;
        // Explicit supported-provider registry; never infer a provider from body text.
        let binding = match owned.provider.as_str() {
            "deepseek" => {
                crate::provider_conversation_lifecycle::deepseek_recovery_binding(&source)
            }
            _ => None,
        };
        let (chat, request, response) = binding.ok_or_else(invalid)?;
        if chat != owned.external_conversation_id {
            return Err(invalid());
        }
        Ok(ObservationCompletionRecoveryRequestBinding {
            provider: identity.provider.clone(),
            platform_account_id: identity.platform_account_id.clone(),
            external_conversation_id: chat.into(),
            request_message_id: request.to_string(),
            response_message_id: response.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCompletionRecoveryClaim {
    pub job_id: Uuid,
    pub lease_id: Uuid,
    pub lease_until: DateTime<Utc>,
    pub original: ObservationCapture,
    pub request_binding: ObservationCompletionRecoveryRequestBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationCompletionRecoveryJobOutcome {
    /// Repository must independently validate a persisted, bound sidecar.
    Recovered,
    Retry,
    NeedsLogin,
    /// Immutable prerequisite missing or history positively cannot be recovered.
    Retained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCompletionRecoveryBackfillItem {
    pub scope: TenantScope,
    pub capture_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCompletionRecoveryDueItem {
    pub scope: TenantScope,
    pub job_id: Uuid,
}

/// Bounded retry delay. Retrying only rereads the same original history.
pub fn observation_completion_recovery_backoff(attempt: u32, needs_login: bool) -> i64 {
    let seconds = 30_i64 * (1_i64 << attempt.saturating_sub(1).min(7));
    seconds.clamp(if needs_login { 900 } else { 30 }, 3600)
}

#[async_trait]
pub trait ObservationCompletionRecoveryJobRepository: Send + Sync {
    /// Service-only global discovery. Keep `as_of` fixed across keyset pages;
    /// returned candidates must still pass scoped, authoritative validation.
    async fn scan_unqueued(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationCompletionRecoveryBackfillItem>, AppError>;
    async fn scan_due(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationCompletionRecoveryDueItem>, AppError>;
    async fn enqueue(&self, scope: &TenantScope, capture_id: Uuid) -> Result<Uuid, AppError>;
    async fn claim(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
    ) -> Result<Option<ObservationCompletionRecoveryClaim>, AppError>;
    /// Current, unexpired lease only. Does not change captures, sidecars,
    /// cleanup jobs, account reservations or browser operation records.
    async fn finish(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
        lease_id: Uuid,
        outcome: ObservationCompletionRecoveryJobOutcome,
    ) -> Result<(), AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_backoff_is_bounded_even_for_exhausted_counter() {
        assert_eq!(observation_completion_recovery_backoff(1, false), 30);
        assert_eq!(observation_completion_recovery_backoff(2, false), 60);
        assert_eq!(observation_completion_recovery_backoff(1, true), 900);
        for login in [false, true] {
            assert_eq!(
                observation_completion_recovery_backoff(u32::MAX, login),
                3600
            );
        }
    }
}
