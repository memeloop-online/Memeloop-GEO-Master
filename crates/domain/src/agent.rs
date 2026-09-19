//! Canonical conversation and agent-run contracts.
//!
//! The API and worker use these types as the stable boundary.  Persistence
//! implementations may map their column names to these contracts, but tenant
//! and project ownership must remain explicit on every object.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, fmt, sync::Arc};
use tokio::sync::{RwLock, broadcast};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, OperatorId, ProjectId, TenantId, TenantScope, UserId};

macro_rules! agent_id {
    ($name:ident) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            Serialize,
            Deserialize,
            ToSchema,
        )]
        #[schema(value_type = String)]
        pub struct $name(pub Uuid);

        impl $name {
            pub const fn new(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

agent_id!(ConversationId);
agent_id!(MessageId);
agent_id!(TurnId);
agent_id!(RunId);
agent_id!(ConversationEventId);
agent_id!(AttachmentId);

/// A durable object reference.  The object store is intentionally outside the
/// conversation repository; only immutable references are carried in messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ObjectRef {
    pub object_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Attachment metadata accepted on a user message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AttachmentReference {
    pub attachment_id: AttachmentId,
    pub object_id: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_version: Option<String>,
}

impl AttachmentReference {
    pub fn object_ref(&self) -> ObjectRef {
        ObjectRef {
            object_id: self.object_id.clone(),
            version: self.object_version.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStatus {
    #[default]
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    #[default]
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCapabilityStatus {
    Available,
    Missing,
    Unavailable,
}

/// Runtime capability is deliberately separate from run status.  A run can be
/// accepted and durably failed with `capability_missing` while preserving the
/// capability snapshot that was observed at acceptance time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeCapability {
    pub status: RuntimeCapabilityStatus,
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RuntimeCapability {
    pub fn missing(reason: impl Into<String>) -> Self {
        Self {
            status: RuntimeCapabilityStatus::Missing,
            runtime: "deno_core".to_owned(),
            version: None,
            reason: Some(reason.into()),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            status: RuntimeCapabilityStatus::Unavailable,
            runtime: "deno_core".to_owned(),
            version: None,
            reason: Some(reason.into()),
        }
    }

    pub fn available(runtime: impl Into<String>, version: Option<String>) -> Self {
        Self {
            status: RuntimeCapabilityStatus::Available,
            runtime: runtime.into(),
            version,
            reason: None,
        }
    }

    pub fn is_available(&self) -> bool {
        self.status == RuntimeCapabilityStatus::Available
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Conversation {
    pub id: ConversationId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<UserId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: ConversationStatus,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Conversation {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.project_id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Message {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub role: MessageRole,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<AttachmentReference>,
    #[serde(default)]
    pub metadata: Value,
    pub sequence: u64,
    pub created_at: DateTime<Utc>,
}

impl Message {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.project_id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Turn {
    pub id: TurnId,
    pub conversation_id: ConversationId,
    pub root_message_id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub status: TurnStatus,
    pub cancel_version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Run {
    pub id: RunId,
    pub conversation_id: ConversationId,
    pub turn_id: TurnId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub status: RunStatus,
    pub capability: RuntimeCapability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
    pub cancel_version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Run {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.project_id))
    }
}

/// Durable event record used both for replay and live SSE delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConversationEvent {
    pub id: ConversationEventId,
    pub conversation_id: ConversationId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub sequence: u64,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
}

impl ConversationEvent {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.project_id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, Default)]
pub struct CreateConversation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AppendMessage {
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<AttachmentReference>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SubmitAcceptance {
    pub conversation: Conversation,
    pub message: Message,
    pub turn: Turn,
    pub run: Run,
    pub events_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConversationDetail {
    pub conversation: Conversation,
    pub messages: Vec<Message>,
    pub turns: Vec<Turn>,
    pub runs: Vec<Run>,
}

/// The boundary for a Rust-hosted runtime worker.  The API never fabricates a
/// model answer: when the capability is missing it records a failed run.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    async fn capability(&self) -> RuntimeCapability;
}

#[derive(Debug, Default)]
pub struct MissingAgentRuntime;

#[async_trait]
impl AgentRuntime for MissingAgentRuntime {
    async fn capability(&self) -> RuntimeCapability {
        RuntimeCapability::missing("embedded JavaScript runtime is not configured")
    }
}

#[async_trait]
pub trait AgentRepository: Send + Sync {
    async fn list_conversations(&self, scope: &TenantScope) -> Result<Vec<Conversation>, AppError>;
    async fn get_conversation(
        &self,
        scope: &TenantScope,
        id: ConversationId,
    ) -> Result<Option<ConversationDetail>, AppError>;
    async fn create_conversation(
        &self,
        scope: &TenantScope,
        created_by: Option<UserId>,
        input: CreateConversation,
    ) -> Result<Conversation, AppError>;
    async fn append_message(
        &self,
        scope: &TenantScope,
        conversation_id: ConversationId,
        input: AppendMessage,
        idempotency_key_hash: String,
        request_hash: String,
        capability: RuntimeCapability,
    ) -> Result<SubmitAcceptance, AppError>;
    async fn cancel_turn(&self, scope: &TenantScope, turn_id: TurnId) -> Result<Run, AppError>;
    async fn replay_events(
        &self,
        scope: &TenantScope,
        conversation_id: ConversationId,
        after: Option<u64>,
    ) -> Result<Vec<ConversationEvent>, AppError>;
    fn subscribe_events(&self) -> broadcast::Receiver<ConversationEvent>;
}

#[derive(Debug, Default)]
struct AgentState {
    conversations: HashMap<ConversationId, Conversation>,
    messages: HashMap<MessageId, Message>,
    turns: HashMap<TurnId, Turn>,
    runs: HashMap<RunId, Run>,
    events: HashMap<ConversationId, Vec<ConversationEvent>>,
    next_message_sequence: HashMap<ConversationId, u64>,
    next_event_sequence: HashMap<ConversationId, u64>,
    idempotent_submissions: HashMap<(String, ConversationId), (String, SubmitAcceptance)>,
}

/// Development-only in-memory agent repository.  It is intentionally
/// tenant/project filtered at every read and write.
pub struct MemoryAgentRepository {
    state: RwLock<AgentState>,
    events: broadcast::Sender<ConversationEvent>,
}

impl fmt::Debug for MemoryAgentRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryAgentRepository")
            .finish_non_exhaustive()
    }
}

impl Default for MemoryAgentRepository {
    fn default() -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            state: RwLock::new(AgentState::default()),
            events,
        }
    }
}

impl MemoryAgentRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConversationEvent> {
        self.events.subscribe()
    }

    async fn emit(
        &self,
        state: &mut AgentState,
        conversation: &Conversation,
        event_type: &str,
        turn_id: Option<TurnId>,
        run_id: Option<RunId>,
        payload: Value,
    ) -> ConversationEvent {
        let next = state
            .next_event_sequence
            .entry(conversation.id)
            .and_modify(|sequence| *sequence += 1)
            .or_insert(1);
        let event = ConversationEvent {
            id: ConversationEventId::from(Uuid::new_v4()),
            conversation_id: conversation.id,
            operator_id: conversation.operator_id,
            tenant_id: conversation.tenant_id,
            project_id: conversation.project_id,
            sequence: *next,
            event_type: event_type.to_owned(),
            turn_id,
            run_id,
            payload,
            occurred_at: Utc::now(),
        };
        state
            .events
            .entry(conversation.id)
            .or_default()
            .push(event.clone());
        let _ = self.events.send(event.clone());
        event
    }

    fn visible(conversation: &Conversation, scope: &TenantScope) -> bool {
        scope.contains(&conversation.scope())
    }
}

#[async_trait]
impl AgentRepository for MemoryAgentRepository {
    async fn list_conversations(&self, scope: &TenantScope) -> Result<Vec<Conversation>, AppError> {
        let mut result = self
            .state
            .read()
            .await
            .conversations
            .values()
            .filter(|conversation| Self::visible(conversation, scope))
            .cloned()
            .collect::<Vec<_>>();
        result.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(result)
    }

    async fn get_conversation(
        &self,
        scope: &TenantScope,
        id: ConversationId,
    ) -> Result<Option<ConversationDetail>, AppError> {
        let state = self.state.read().await;
        let Some(conversation) = state
            .conversations
            .get(&id)
            .filter(|conversation| Self::visible(conversation, scope))
            .cloned()
        else {
            return Ok(None);
        };
        let mut messages = state
            .messages
            .values()
            .filter(|message| message.conversation_id == id)
            .cloned()
            .collect::<Vec<_>>();
        messages.sort_by_key(|message| message.sequence);
        let mut turns = state
            .turns
            .values()
            .filter(|turn| turn.conversation_id == id)
            .cloned()
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| turn.created_at);
        let mut runs = state
            .runs
            .values()
            .filter(|run| run.conversation_id == id)
            .cloned()
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| run.created_at);
        Ok(Some(ConversationDetail {
            conversation,
            messages,
            turns,
            runs,
        }))
    }

    async fn create_conversation(
        &self,
        scope: &TenantScope,
        created_by: Option<UserId>,
        input: CreateConversation,
    ) -> Result<Conversation, AppError> {
        let project_id = scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project selector is required"))?;
        let title = input
            .title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty());
        if title
            .as_ref()
            .is_some_and(|title| title.chars().count() > 200)
        {
            return Err(AppError::invalid_request(
                "conversation title must be at most 200 characters",
            ));
        }
        let now = Utc::now();
        let conversation = Conversation {
            id: ConversationId::from(Uuid::new_v4()),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            created_by,
            title,
            status: ConversationStatus::Active,
            revision: 1,
            created_at: now,
            updated_at: now,
        };
        let mut state = self.state.write().await;
        state
            .conversations
            .insert(conversation.id, conversation.clone());
        self.emit(
            &mut state,
            &conversation,
            "conversation.created",
            None,
            None,
            json!({"conversation_id": conversation.id}),
        )
        .await;
        Ok(conversation)
    }

    async fn append_message(
        &self,
        scope: &TenantScope,
        conversation_id: ConversationId,
        input: AppendMessage,
        idempotency_key_hash: String,
        request_hash: String,
        capability: RuntimeCapability,
    ) -> Result<SubmitAcceptance, AppError> {
        let content = input.content.trim().to_owned();
        if content.is_empty() && input.attachments.is_empty() {
            return Err(AppError::invalid_request(
                "message content or at least one attachment is required",
            ));
        }
        if content.chars().count() > 100_000 {
            return Err(AppError::invalid_request(
                "message content must be at most 100000 characters",
            ));
        }
        if input.attachments.len() > 100 {
            return Err(AppError::invalid_request(
                "a message may contain at most 100 attachments",
            ));
        }
        for attachment in &input.attachments {
            if attachment.object_id.trim().is_empty() || attachment.object_id.chars().count() > 500
            {
                return Err(AppError::invalid_request(
                    "attachment object_id must be between 1 and 500 characters",
                ));
            }
            if attachment.filename.trim().is_empty() || attachment.filename.chars().count() > 512 {
                return Err(AppError::invalid_request(
                    "attachment filename must be between 1 and 512 characters",
                ));
            }
            if attachment
                .size_bytes
                .is_some_and(|size| size > 100 * 1024 * 1024)
            {
                return Err(AppError::invalid_request(
                    "attachment size_bytes must be at most 100 MiB",
                ));
            }
        }
        let mut state = self.state.write().await;
        let conversation = state
            .conversations
            .get(&conversation_id)
            .filter(|conversation| Self::visible(conversation, scope))
            .cloned()
            .ok_or_else(|| AppError::not_found("conversation not found"))?;
        let idem_key = (idempotency_key_hash, conversation_id);
        if let Some((existing_hash, existing)) = state.idempotent_submissions.get(&idem_key) {
            if existing_hash != &request_hash {
                return Err(AppError::conflict(
                    "Idempotency-Key was already used with a different message request",
                ));
            }
            return Ok(existing.clone());
        }
        if conversation.status != ConversationStatus::Active {
            return Err(AppError::conflict("conversation is archived"));
        }
        let active_turn = state
            .turns
            .values()
            .filter(|turn| turn.conversation_id == conversation_id)
            .filter(|turn| matches!(turn.status, TurnStatus::Queued | TurnStatus::Running))
            .max_by_key(|turn| turn.created_at)
            .map(|turn| turn.id);
        if active_turn.is_some() {
            return Err(AppError::conflict(
                "conversation already has an active turn",
            ));
        }
        let previous_turn_id = state
            .turns
            .values()
            .filter(|turn| turn.conversation_id == conversation_id)
            .max_by_key(|turn| turn.created_at)
            .map(|turn| turn.id);
        let message_sequence = {
            let sequence = state
                .next_message_sequence
                .entry(conversation_id)
                .and_modify(|sequence| *sequence += 1)
                .or_insert(1);
            *sequence
        };
        let turn_id = TurnId::from(Uuid::new_v4());
        let run_id = RunId::from(Uuid::new_v4());
        let now = Utc::now();
        let mut updated_conversation = conversation.clone();
        updated_conversation.revision += 1;
        updated_conversation.updated_at = now;
        state
            .conversations
            .insert(updated_conversation.id, updated_conversation.clone());
        let conversation = updated_conversation;
        let message = Message {
            id: MessageId::from(Uuid::new_v4()),
            conversation_id,
            operator_id: conversation.operator_id,
            tenant_id: conversation.tenant_id,
            project_id: conversation.project_id,
            turn_id: Some(turn_id),
            role: MessageRole::User,
            content,
            attachments: input.attachments,
            metadata: input.metadata,
            sequence: message_sequence,
            created_at: now,
        };
        let mut turn = Turn {
            id: turn_id,
            conversation_id,
            root_message_id: message.id,
            previous_turn_id,
            run_id: Some(run_id),
            status: TurnStatus::Queued,
            cancel_version: 0,
            created_at: now,
            updated_at: now,
        };
        let mut run = Run {
            id: run_id,
            conversation_id,
            turn_id,
            operator_id: conversation.operator_id,
            tenant_id: conversation.tenant_id,
            project_id: conversation.project_id,
            status: RunStatus::Queued,
            capability: capability.clone(),
            error: None,
            cancel_version: 0,
            created_at: now,
            updated_at: now,
        };
        state.messages.insert(message.id, message.clone());
        state.turns.insert(turn.id, turn.clone());
        state.runs.insert(run.id, run.clone());
        self.emit(
            &mut state,
            &conversation,
            "message.created",
            Some(turn.id),
            Some(run.id),
            json!({"message": message}),
        )
        .await;
        self.emit(
            &mut state,
            &conversation,
            "turn.accepted",
            Some(turn.id),
            Some(run.id),
            json!({"turn_id": turn.id, "run_id": run.id}),
        )
        .await;
        if !capability.is_available() {
            let error = AppError::capability_missing(
                capability
                    .reason
                    .clone()
                    .unwrap_or_else(|| "agent runtime is unavailable".to_owned()),
            );
            turn.status = TurnStatus::Failed;
            turn.updated_at = Utc::now();
            run.status = RunStatus::Failed;
            run.error = Some(error.clone());
            run.updated_at = turn.updated_at;
            state.turns.insert(turn.id, turn.clone());
            state.runs.insert(run.id, run.clone());
            self.emit(
                &mut state,
                &conversation,
                "run.failed",
                Some(turn.id),
                Some(run.id),
                json!({"run_id": run.id, "status": run.status, "error": error}),
            )
            .await;
        }
        let acceptance = SubmitAcceptance {
            conversation: conversation.clone(),
            message,
            turn,
            run,
            events_url: format!("/api/v1/agent/conversations/{}/events", conversation.id),
        };
        state
            .idempotent_submissions
            .insert(idem_key, (request_hash, acceptance.clone()));
        Ok(acceptance)
    }

    async fn cancel_turn(&self, scope: &TenantScope, turn_id: TurnId) -> Result<Run, AppError> {
        let mut state = self.state.write().await;
        let turn = state
            .turns
            .get(&turn_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("turn not found"))?;
        let run_id = turn
            .run_id
            .ok_or_else(|| AppError::not_found("run not found"))?;
        let run = state
            .runs
            .get(&run_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("run not found"))?;
        if !scope.contains(&run.scope()) {
            return Err(AppError::not_found("turn not found"));
        }
        if matches!(
            run.status,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled
        ) {
            return Ok(run);
        }
        let conversation = state
            .conversations
            .get(&turn.conversation_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("conversation not found"))?;
        let now = Utc::now();
        let mut cancelled_turn = turn;
        cancelled_turn.status = TurnStatus::Cancelled;
        cancelled_turn.cancel_version += 1;
        cancelled_turn.updated_at = now;
        let mut cancelled_run = run;
        cancelled_run.status = RunStatus::Cancelled;
        cancelled_run.cancel_version = cancelled_turn.cancel_version;
        cancelled_run.updated_at = now;
        state
            .turns
            .insert(cancelled_turn.id, cancelled_turn.clone());
        state.runs.insert(cancelled_run.id, cancelled_run.clone());
        self.emit(
            &mut state,
            &conversation,
            "run.cancelled",
            Some(cancelled_turn.id),
            Some(cancelled_run.id),
            json!({"run_id": cancelled_run.id, "cancel_version": cancelled_run.cancel_version}),
        )
        .await;
        Ok(cancelled_run)
    }

    async fn replay_events(
        &self,
        scope: &TenantScope,
        conversation_id: ConversationId,
        after: Option<u64>,
    ) -> Result<Vec<ConversationEvent>, AppError> {
        let state = self.state.read().await;
        let conversation = state
            .conversations
            .get(&conversation_id)
            .filter(|conversation| Self::visible(conversation, scope))
            .ok_or_else(|| AppError::not_found("conversation not found"))?;
        let after = after.unwrap_or(0);
        Ok(state
            .events
            .get(&conversation.id)
            .into_iter()
            .flatten()
            .filter(|event| event.sequence > after)
            .cloned()
            .collect())
    }

    fn subscribe_events(&self) -> broadcast::Receiver<ConversationEvent> {
        self.subscribe()
    }
}

/// Small helper used by API implementations that need an owned trait object.
pub type SharedAgentRepository = Arc<dyn AgentRepository>;

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(project_id: Uuid) -> TenantScope {
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(project_id.into()),
        )
    }

    fn message(content: &str) -> AppendMessage {
        AppendMessage {
            content: content.to_owned(),
            attachments: Vec::new(),
            metadata: Value::Null,
        }
    }

    #[tokio::test]
    async fn conversations_are_isolated_by_tenant_and_project() {
        let repository = MemoryAgentRepository::new();
        let first_scope = scope(Uuid::new_v4());
        let second_scope = TenantScope::new(
            first_scope.operator_id,
            first_scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        repository
            .create_conversation(&first_scope, None, CreateConversation::default())
            .await
            .expect("create");
        assert_eq!(
            repository
                .list_conversations(&second_scope)
                .await
                .expect("list")
                .len(),
            0
        );
        let other_tenant = TenantScope::new(
            first_scope.operator_id,
            Uuid::new_v4().into(),
            first_scope.project_id,
        );
        assert!(
            repository
                .list_conversations(&other_tenant)
                .await
                .expect("list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn message_submission_is_idempotent_and_rejects_hash_conflicts() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .expect("create");
        let capability = RuntimeCapability::missing("test");
        let first = repository
            .append_message(
                &scope,
                conversation.id,
                message("hello"),
                "same-key".to_owned(),
                "body-a".to_owned(),
                capability.clone(),
            )
            .await
            .expect("append");
        let replay = repository
            .append_message(
                &scope,
                conversation.id,
                message("hello"),
                "same-key".to_owned(),
                "body-a".to_owned(),
                capability.clone(),
            )
            .await
            .expect("replay");
        assert_eq!(first.run.id, replay.run.id);
        let conflict = repository
            .append_message(
                &scope,
                conversation.id,
                message("different"),
                "same-key".to_owned(),
                "body-b".to_owned(),
                capability,
            )
            .await
            .expect_err("hash conflict");
        assert_eq!(conflict.code, crate::ErrorCode::Conflict);
    }

    #[tokio::test]
    async fn unavailable_runtime_fails_run_and_replays_failure_event() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .expect("create");
        let acceptance = repository
            .append_message(
                &scope,
                conversation.id,
                message("hello"),
                "runtime-key".to_owned(),
                "body".to_owned(),
                RuntimeCapability::missing("runtime missing"),
            )
            .await
            .expect("append");
        assert_eq!(acceptance.run.status, RunStatus::Failed);
        assert_eq!(
            acceptance.run.error.as_ref().map(|error| error.code),
            Some(crate::ErrorCode::CapabilityMissing)
        );
        let events = repository
            .replay_events(&scope, conversation.id, Some(0))
            .await
            .expect("events");
        assert!(events.iter().any(|event| event.event_type == "run.failed"));
        assert!(
            events
                .windows(2)
                .all(|window| window[0].sequence < window[1].sequence)
        );
    }

    #[tokio::test]
    async fn available_runtime_can_be_cancelled_and_replayed() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let conversation = repository
            .create_conversation(&scope, None, CreateConversation::default())
            .await
            .expect("create");
        let acceptance = repository
            .append_message(
                &scope,
                conversation.id,
                message("hello"),
                "cancel-key".to_owned(),
                "body".to_owned(),
                RuntimeCapability::available("test", Some("1".to_owned())),
            )
            .await
            .expect("append");
        let run = repository
            .cancel_turn(&scope, acceptance.turn.id)
            .await
            .expect("cancel");
        assert_eq!(run.status, RunStatus::Cancelled);
        let events = repository
            .replay_events(&scope, conversation.id, Some(2))
            .await
            .expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "run.cancelled")
        );
    }
}
