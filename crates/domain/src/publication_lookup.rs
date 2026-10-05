//! Read-only reconciliation of an ambiguous publication attempt.
//! A lookup can observe an asset, but cannot prove that this send created it.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppError, ChannelTargetInput, TenantScope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationLookupJob {
    pub attempt_id: Uuid,
    pub target_id: Uuid,
    pub account_id: Uuid,
    pub frozen_input: ChannelTargetInput,
    pub connector_version: Option<String>,
    /// Unverified hint from the original attempt, never a publication receipt.
    pub candidate_public_url: Option<String>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub lease_execution_id: Option<Uuid>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub query_count: i32,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationLookupCandidate {
    pub scope: TenantScope,
    pub attempt_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationLookupFinding {
    Unknown,
    AssetObserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationLookupObservation {
    pub execution_id: Uuid,
    pub attempt_id: Uuid,
    pub finding: PublicationLookupFinding,
    /// Data retained for later, independent validation of send causality.
    pub evidence: serde_json::Value,
    pub observed_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub error_code: Option<String>,
}

#[async_trait]
pub trait PublicationLookupRepository: Send + Sync {
    /// Create one job per original attempt. Read its bound target, outcome,
    /// and candidate from the database; callers cannot supply asset hints.
    async fn enqueue(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError>;

    /// Trusted cross-scope keyset page, including recoverable expired leases.
    async fn scan_due(
        &self,
        after_attempt_id: Option<Uuid>,
        as_of: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<PublicationLookupCandidate>, AppError>;

    /// The fresh execution ID is also the lease fence. Expiry permits only
    /// another read-only lookup, never another publication.
    async fn claim(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
        execution_id: Uuid,
        at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<PublicationLookupJob, AppError>;

    /// Append one observation for the currently held execution and schedule
    /// another lookup (or stop scheduling with None). Never project a receipt.
    async fn finish(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
        observation: PublicationLookupObservation,
        next_due_at: Option<DateTime<Utc>>,
    ) -> Result<PublicationLookupJob, AppError>;

    async fn get(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
    ) -> Result<PublicationLookupJob, AppError>;

    async fn observations(
        &self,
        scope: &TenantScope,
        attempt_id: Uuid,
    ) -> Result<Vec<PublicationLookupObservation>, AppError>;
}
