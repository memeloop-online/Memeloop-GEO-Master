use async_trait::async_trait;
use geo_domain::{
    AgentRepository, AppError, AppendMessage, Conversation, ConversationDetail, ConversationEvent,
    ConversationId, CreateConversation, Run, RuntimeCapability, SubmitAcceptance, TenantScope,
    TurnId, UserId,
};
use sqlx::PgPool;
use tokio::sync::broadcast;

/// PostgreSQL boundary for P00 agent state.
///
/// The canonical contract is defined now so API and worker code do not depend
/// on a storage-specific shape.  Agent tables are intentionally not migrated
/// until their transaction/checkpoint schema is finalized; production callers
/// therefore fail closed instead of silently falling back to process memory.
#[derive(Clone)]
pub struct PgAgentRepository {
    _pool: PgPool,
}

impl PgAgentRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { _pool: pool }
    }

    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }

    fn unavailable() -> AppError {
        AppError::new(
            geo_domain::ErrorCode::DependencyUnavailable,
            "agent persistence is not available until the conversation schema migration is installed",
        )
    }
}

#[async_trait]
impl AgentRepository for PgAgentRepository {
    async fn list_conversations(
        &self,
        _scope: &TenantScope,
    ) -> Result<Vec<Conversation>, AppError> {
        Err(Self::unavailable())
    }

    async fn get_conversation(
        &self,
        _scope: &TenantScope,
        _id: ConversationId,
    ) -> Result<Option<ConversationDetail>, AppError> {
        Err(Self::unavailable())
    }

    async fn create_conversation(
        &self,
        _scope: &TenantScope,
        _created_by: Option<UserId>,
        _input: CreateConversation,
    ) -> Result<Conversation, AppError> {
        Err(Self::unavailable())
    }

    async fn append_message(
        &self,
        _scope: &TenantScope,
        _conversation_id: ConversationId,
        _input: AppendMessage,
        _idempotency_key_hash: String,
        _request_hash: String,
        _capability: RuntimeCapability,
    ) -> Result<SubmitAcceptance, AppError> {
        Err(Self::unavailable())
    }

    async fn cancel_turn(&self, _scope: &TenantScope, _turn_id: TurnId) -> Result<Run, AppError> {
        Err(Self::unavailable())
    }

    async fn replay_events(
        &self,
        _scope: &TenantScope,
        _conversation_id: ConversationId,
        _after: Option<u64>,
    ) -> Result<Vec<ConversationEvent>, AppError> {
        Err(Self::unavailable())
    }

    fn subscribe_events(&self) -> broadcast::Receiver<ConversationEvent> {
        let (_, receiver) = broadcast::channel(1);
        receiver
    }
}
