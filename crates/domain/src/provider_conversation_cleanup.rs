//! Durable cleanup scheduling, not authorization to perform a remote deletion.
//! Dispatch must additionally verify account identity, complete retained evidence
//! and no active use under the shared account execution lock.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppError, ObservationProviderIdentity, TenantScope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCleanupAction {
    Delete,
    /// A previous request may have reached the provider. Only read existence.
    Reconcile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCleanupClaim {
    pub cleanup_id: Uuid,
    pub capture_id: Uuid,
    pub account_id: Uuid,
    pub provider: String,
    pub external_conversation_id: String,
    pub original_identity: ObservationProviderIdentity,
    pub lease_id: Uuid,
    pub lease_until: DateTime<Utc>,
    pub action: ProviderCleanupAction,
    /// Recomputed from immutable, complete retained raw evidence. Missing on
    /// legacy/incomplete claims, which never authorize destructive cleanup.
    #[serde(default)]
    pub retained_message_inventory_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCleanupOutcome {
    /// An explicit deletion receipt or read-only confirmation of absence.
    Deleted,
    /// Only after deletion is unsupported and an archive receipt is confirmed.
    Archived,
    /// Timeout/ambiguous response: next work must be read-only reconciliation.
    Unknown,
    /// Confirmed no deletion happened. Retry is independent of measurement.
    Failed,
    /// Read-only reconciliation positively found the exact conversation.
    Present,
    NeedsLogin,
}

impl ProviderCleanupOutcome {
    pub fn next_state(self, action: ProviderCleanupAction) -> Result<&'static str, AppError> {
        match (self, action) {
            (Self::Deleted, _) => Ok("deleted"),
            (Self::Archived, ProviderCleanupAction::Delete) => Ok("archived"),
            (Self::Present, ProviderCleanupAction::Reconcile) => Ok("pending"),
            (Self::Unknown, _) => Ok("unknown"),
            (Self::NeedsLogin, _) => Ok("needs_login"),
            (Self::Failed, ProviderCleanupAction::Delete) => Ok("failed"),
            // Failed existence lookup cannot authorize another destructive call.
            (Self::Failed, ProviderCleanupAction::Reconcile) => Ok("unknown"),
            _ => Err(AppError::invalid_request(
                "cleanup result does not match action",
            )),
        }
    }
}

#[async_trait]
pub trait ProviderConversationCleanupRepository: Send + Sync {
    /// Service-only global discovery; writes must bind the returned scope.
    /// Keep `as_of` fixed across pages; UUID cursors are exclusive.
    async fn scan_unqueued(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ProviderCleanupBackfillItem>, AppError>;
    /// Scheduling candidates only, never permission for remote deletion.
    async fn scan_due(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ProviderCleanupDueItem>, AppError>;
    /// No caller-supplied account, provider or remote ID is accepted.
    async fn enqueue(&self, scope: &TenantScope, capture_id: Uuid) -> Result<Uuid, AppError>;
    /// Atomically claim the exact discovered job, rechecking its eligibility.
    async fn claim(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
    ) -> Result<Option<ProviderCleanupClaim>, AppError>;
    /// Verify complete retained evidence under the shared account reservation
    /// immediately before deletion; a scheduling claim alone never authorizes it.
    async fn authorize_delete(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
        lease_id: Uuid,
        reservation_id: Uuid,
    ) -> Result<ProviderCleanupClaim, AppError>;
    /// Claims at most one item. Expired leases are always reconciled.
    async fn claim_due(
        &self,
        scope: &TenantScope,
    ) -> Result<Option<ProviderCleanupClaim>, AppError>;
    /// Fenced by the current, unexpired lease. No raw provider error is stored.
    async fn finish(
        &self,
        scope: &TenantScope,
        cleanup_id: Uuid,
        lease_id: Uuid,
        outcome: ProviderCleanupOutcome,
    ) -> Result<(), AppError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCleanupBackfillItem {
    pub scope: TenantScope,
    pub capture_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCleanupDueItem {
    pub scope: TenantScope,
    pub cleanup_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_or_failed_lookup_never_authorizes_blind_deletion() {
        assert_eq!(
            ProviderCleanupOutcome::Unknown
                .next_state(ProviderCleanupAction::Delete)
                .unwrap(),
            "unknown"
        );
        assert_eq!(
            ProviderCleanupOutcome::Failed
                .next_state(ProviderCleanupAction::Reconcile)
                .unwrap(),
            "unknown"
        );
        assert_eq!(
            ProviderCleanupOutcome::Present
                .next_state(ProviderCleanupAction::Reconcile)
                .unwrap(),
            "pending"
        );
        assert!(
            ProviderCleanupOutcome::Present
                .next_state(ProviderCleanupAction::Delete)
                .is_err()
        );
        assert!(
            ProviderCleanupOutcome::Archived
                .next_state(ProviderCleanupAction::Reconcile)
                .is_err()
        );
    }
}
