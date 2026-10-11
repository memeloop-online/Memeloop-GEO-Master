//! Durable, fail-closed ownership of a browser and its account gate.
use crate::{AppError, ChannelSecret, OperatorId, ProjectId, TenantId};

/// Trusted verification result plus the account/session snapshot read before
/// verification. Secret bytes never implement Debug or public serialization.
pub struct VerifiedLoginStorage {
    pub account_id: Uuid,
    pub secret_scope: crate::TenantScope,
    pub owner_kind: crate::ChannelOwnerKind,
    pub platform: String,
    pub expected_platform_account_id: Option<String>,
    pub expected_session: Option<ChannelSecret>,
    pub platform_account_id: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub session: ChannelSecret,
}
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrowserOperationScope {
    pub operator_id: OperatorId,
    pub tenant_id: Option<TenantId>,
    pub project_id: Option<ProjectId>,
    pub owner_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrowserOperationPurpose {
    Login,
    Discovery,
    Measure,
    Publish,
    Lookup,
    Cleanup,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrowserOperationState {
    Prepared,
    StartRequested,
    Closing,
    Disposed,
}

#[derive(Clone)]
pub struct BeginBrowserOperation {
    pub operation_id: Uuid,
    pub session_id: Uuid,
    pub account_id: Uuid,
    pub purpose: BrowserOperationPurpose,
    pub runner_generation: Uuid,
    pub effective_network: ChannelSecret,
    pub reservation_id: Option<Uuid>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct BrowserOperation {
    pub operation_id: Uuid,
    pub session_id: Uuid,
    pub account_id: Uuid,
    pub scope: BrowserOperationScope,
    pub purpose: BrowserOperationPurpose,
    pub runner_generation: Uuid,
    pub effective_network: ChannelSecret,
    pub reservation_id: Uuid,
    pub state: BrowserOperationState,
    pub version: i64,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub next_check_at: DateTime<Utc>,
    pub released_unstarted: bool,
}

impl BrowserOperation {
    pub fn validate_login_commit(
        &self,
        scope: &BrowserOperationScope,
        storage: &VerifiedLoginStorage,
    ) -> Result<(), AppError> {
        if &self.scope != scope
            || self.account_id != storage.account_id
            || self.purpose != BrowserOperationPurpose::Login
            || self.state != BrowserOperationState::StartRequested
            || scope.operator_id != storage.secret_scope.operator_id
            || storage
                .expected_platform_account_id
                .as_ref()
                .is_some_and(|id| id != &storage.platform_account_id)
            || storage.platform_account_id.trim().is_empty()
            || storage.session.encrypted_bytes().is_empty()
        {
            return Err(AppError::conflict("login no longer owns browser operation"));
        }
        match storage.owner_kind {
            crate::ChannelOwnerKind::Customer
                if scope.tenant_id == Some(storage.secret_scope.tenant_id)
                    && scope.project_id.is_some()
                    && scope.project_id == storage.secret_scope.project_id =>
            {
                Ok(())
            }
            crate::ChannelOwnerKind::OperatorPool
                if scope.tenant_id.is_none() && scope.project_id.is_none() =>
            {
                Ok(())
            }
            _ => Err(AppError::forbidden("login storage scope differs")),
        }
    }
    pub fn prepare(
        scope: &BrowserOperationScope,
        input: &BeginBrowserOperation,
        at: DateTime<Utc>,
    ) -> Result<Self, AppError> {
        if input.operation_id.is_nil()
            || input.session_id.is_nil()
            || input.account_id.is_nil()
            || input.runner_generation.is_nil()
            || scope.owner_id.is_nil()
            || (scope.project_id.is_some() && scope.tenant_id.is_none())
            || input.effective_network.encrypted_bytes().is_empty()
        {
            return Err(AppError::invalid_request("invalid browser ownership"));
        }
        Ok(Self {
            operation_id: input.operation_id,
            session_id: input.session_id,
            account_id: input.account_id,
            scope: scope.clone(),
            purpose: input.purpose,
            runner_generation: input.runner_generation,
            effective_network: input.effective_network.clone(),
            reservation_id: input.reservation_id.unwrap_or(input.operation_id),
            state: BrowserOperationState::Prepared,
            version: 1,
            expires_at: input.expires_at,
            created_at: at,
            updated_at: at,
            next_check_at: input.expires_at,
            released_unstarted: false,
        })
    }
    pub fn matches_begin(&self, other: &Self) -> bool {
        self.operation_id == other.operation_id
            && self.session_id == other.session_id
            && self.account_id == other.account_id
            && self.scope == other.scope
            && self.purpose == other.purpose
            && self.runner_generation == other.runner_generation
            && self.reservation_id == other.reservation_id
            && self.expires_at.timestamp_micros() == other.expires_at.timestamp_micros()
            && self.effective_network.encrypted_bytes() == other.effective_network.encrypted_bytes()
    }
    pub fn transition(
        &mut self,
        scope: &BrowserOperationScope,
        action: BrowserOperationTransition,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        if &self.scope != scope {
            return Err(AppError::forbidden("browser ownership scope mismatch"));
        }
        use BrowserOperationState::*;
        let next = match action {
            BrowserOperationTransition::Start => match self.state {
                Prepared if at < self.expires_at => StartRequested,
                StartRequested => StartRequested,
                _ => return Err(AppError::conflict("browser cannot start")),
            },
            BrowserOperationTransition::Close => match self.state {
                Disposed => Disposed,
                Closing => Closing,
                _ => {
                    self.next_check_at = at;
                    Closing
                }
            },
            BrowserOperationTransition::ReleaseUnstarted => match self.state {
                Prepared => {
                    self.released_unstarted = true;
                    Disposed
                }
                Disposed if self.released_unstarted => Disposed,
                _ => return Err(AppError::conflict("browser start may have occurred")),
            },
            BrowserOperationTransition::Defer { next_check_at } => {
                if self.state == Disposed || next_check_at <= at {
                    return Err(AppError::conflict(
                        "invalid browser reconciliation schedule",
                    ));
                }
                self.next_check_at = next_check_at;
                self.version += 1;
                self.updated_at = at;
                return Ok(());
            }
            BrowserOperationTransition::Disposed {
                session_id,
                runner_generation,
            } => {
                if self.session_id != session_id || self.runner_generation != runner_generation {
                    return Err(AppError::conflict("browser disposal identity mismatch"));
                }
                Disposed
            }
        };
        if self.state != next {
            self.state = next;
            self.version += 1;
            self.updated_at = at;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub enum BrowserOperationTransition {
    Start,
    Close,
    ReleaseUnstarted,
    Disposed {
        session_id: Uuid,
        runner_generation: Uuid,
    },
    Defer {
        next_check_at: DateTime<Utc>,
    },
}

#[async_trait]
pub trait BrowserOperationRepository: Send + Sync {
    async fn save_owned_login_session(
        &self,
        scope: &BrowserOperationScope,
        operation_id: Uuid,
        storage: &VerifiedLoginStorage,
        channels: &dyn crate::ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<(), AppError>;
    async fn browser_operation_exists(
        &self,
        operator: OperatorId,
        session_id: Uuid,
    ) -> Result<bool, AppError>;
    async fn get_active_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        account_id: Uuid,
    ) -> Result<Option<BrowserOperation>, AppError>;
    async fn begin_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        input: &BeginBrowserOperation,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError>;
    async fn get_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        operation_id: Uuid,
    ) -> Result<Option<BrowserOperation>, AppError>;
    /// Trusted reconciliation only; expiry never grants another owner access.
    async fn list_due_browser_operations(
        &self,
        at: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<BrowserOperation>, AppError>;
    async fn transition_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        operation_id: Uuid,
        action: BrowserOperationTransition,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError>;
    async fn defer_browser_reconciliation(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
        next_check_at: DateTime<Utc>,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError> {
        self.transition_browser_operation(
            scope,
            id,
            BrowserOperationTransition::Defer { next_check_at },
            at,
        )
        .await
    }
    async fn mark_browser_start_requested(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError> {
        self.transition_browser_operation(scope, id, BrowserOperationTransition::Start, at)
            .await
    }
    async fn request_browser_close(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError> {
        self.transition_browser_operation(scope, id, BrowserOperationTransition::Close, at)
            .await
    }
    async fn release_browser_unstarted(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError> {
        self.transition_browser_operation(
            scope,
            id,
            BrowserOperationTransition::ReleaseUnstarted,
            at,
        )
        .await
    }
    async fn confirm_browser_disposed(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
        session_id: Uuid,
        runner_generation: Uuid,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError> {
        self.transition_browser_operation(
            scope,
            id,
            BrowserOperationTransition::Disposed {
                session_id,
                runner_generation,
            },
            at,
        )
        .await
    }
}
