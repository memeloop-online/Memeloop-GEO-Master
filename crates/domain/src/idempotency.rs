use crate::{AppError, TenantScope};
use async_trait::async_trait;

/// The response retained for a completed idempotent command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// A reservation returned by an idempotency store for a new request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyToken {
    pub scope: String,
    pub key: String,
    pub body_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyDecision {
    New(IdempotencyToken),
    Replay(StoredResponse),
    InFlight,
}

/// Durable idempotency contract shared by the HTTP middleware and persistence
/// implementations. A key is always bound to the complete server-side scope
/// and request body hash.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    async fn begin(
        &self,
        scope: &TenantScope,
        key: &str,
        body_hash: &str,
    ) -> Result<IdempotencyDecision, AppError>;

    async fn complete(
        &self,
        token: &IdempotencyToken,
        response: StoredResponse,
    ) -> Result<(), AppError>;
}
