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
agent_id!(CheckpointId);
agent_id!(ToolCallLedgerId);

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

/// A permission or budget verdict recorded before a tool call is attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallDecision {
    Allowed,
    Denied,
}

/// Durable lifecycle of one tool call.  `unknown` is a first-class outcome: a
/// crash after an external send and before the receipt must never be retried
/// blindly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallOutcome {
    Intent,
    Attempted,
    Succeeded,
    Failed,
    Unknown,
}

/// A durable run checkpoint.  `(run_id, checkpoint_scope, step_key)` is unique
/// and the checkpoint may only be reused while its input digest still matches.
/// This is host state owned by Rust, never a JavaScript heap snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentCheckpoint {
    pub id: CheckpointId,
    pub run_id: RunId,
    pub conversation_id: ConversationId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub checkpoint_scope: String,
    pub step_key: String,
    pub input_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<ObjectRef>,
    pub version: u64,
    pub state: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentCheckpoint {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.project_id))
    }
}

/// Request payload for storing or refreshing a run checkpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StoreCheckpoint {
    pub checkpoint_scope: String,
    pub step_key: String,
    pub input_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<ObjectRef>,
    pub state: Value,
}

/// One append-only tool-call ledger entry.  Conversation and turn ownership are
/// derived from the run rather than trusted from the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ToolCallLedgerEntry {
    pub id: ToolCallLedgerId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub conversation_id: ConversationId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_hash: String,
    pub idempotency_key_hash: String,
    pub permission: ToolCallDecision,
    pub budget: ToolCallDecision,
    pub intent: Value,
    pub attempt_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<ObjectRef>,
    pub outcome: ToolCallOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ToolCallLedgerEntry {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.project_id))
    }
}

/// Request payload for appending one tool-call ledger entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RecordToolCall {
    pub run_id: RunId,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_hash: String,
    pub idempotency_key_hash: String,
    pub permission: ToolCallDecision,
    pub budget: ToolCallDecision,
    #[serde(default)]
    pub intent: Value,
    #[serde(default)]
    pub attempt_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<ObjectRef>,
    pub outcome: ToolCallOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

/// Bounded identifier for a checkpoint scope, step or tool call.
fn validate_agent_key(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() || value.chars().count() > 200 {
        return Err(AppError::invalid_request(format!(
            "{field} must be between 1 and 200 characters"
        )));
    }
    Ok(())
}

/// Opaque digest of an input, argument list, idempotency key or serialized
/// request.  The repository never inspects the digest, only its stability.
fn validate_agent_digest(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() || value.chars().count() > 512 {
        return Err(AppError::invalid_request(format!(
            "{field} must be between 1 and 512 characters"
        )));
    }
    Ok(())
}

/// The longest message body either repository will persist.  Named once so the
/// user and assistant paths cannot disagree about it.
pub const MAX_MESSAGE_CHARS: usize = 100_000;

/// Shared validation for a user message submission.  Both the in-memory and the
/// PostgreSQL repository call this so their rejections cannot drift apart.
/// Returns the trimmed content that must be persisted.
pub fn validate_append_message(input: &AppendMessage) -> Result<String, AppError> {
    let content = input.content.trim().to_owned();
    if content.is_empty() && input.attachments.is_empty() {
        return Err(AppError::invalid_request(
            "message content or at least one attachment is required",
        ));
    }
    if content.chars().count() > MAX_MESSAGE_CHARS {
        return Err(AppError::invalid_request(format!(
            "message content must be at most {MAX_MESSAGE_CHARS} characters"
        )));
    }
    if input.attachments.len() > 100 {
        return Err(AppError::invalid_request(
            "a message may contain at most 100 attachments",
        ));
    }
    for attachment in &input.attachments {
        if attachment.object_id.trim().is_empty() || attachment.object_id.chars().count() > 500 {
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
    Ok(content)
}

/// Shared validation for a persisted message body that has no attachment
/// escape hatch.  A user message may be attachment-only; an answer may not,
/// because an assistant message with nothing in it is indistinguishable from a
/// fabricated one.  Both repositories call this so their rejections cannot
/// drift apart.  Returns the trimmed content that must be persisted.
pub fn validate_message_content(content: &str) -> Result<String, AppError> {
    let content = content.trim().to_owned();
    if content.is_empty() {
        return Err(AppError::invalid_request(
            "message content must not be empty",
        ));
    }
    if content.chars().count() > MAX_MESSAGE_CHARS {
        return Err(AppError::invalid_request(format!(
            "message content must be at most {MAX_MESSAGE_CHARS} characters"
        )));
    }
    Ok(content)
}

/// Shared validation for a checkpoint write.  Both the in-memory and the
/// PostgreSQL repository call this so their rejections cannot drift apart.
pub fn validate_checkpoint_write(input: &StoreCheckpoint) -> Result<(), AppError> {
    validate_agent_key("checkpoint_scope", &input.checkpoint_scope)?;
    validate_agent_key("step_key", &input.step_key)?;
    validate_agent_digest("input_hash", &input.input_hash)?;
    validate_object_ref(input.result_ref.as_ref())?;
    Ok(())
}

/// Shared validation for a tool-call ledger append.
pub fn validate_tool_call_write(input: &RecordToolCall) -> Result<(), AppError> {
    validate_agent_key("tool_call_id", &input.tool_call_id)?;
    validate_agent_key("tool_name", &input.tool_name)?;
    validate_agent_digest("arguments_hash", &input.arguments_hash)?;
    validate_agent_digest("idempotency_key_hash", &input.idempotency_key_hash)?;
    validate_object_ref(input.result_ref.as_ref())?;
    if input.currency.as_ref().is_some_and(|currency| {
        currency.chars().count() != 3
            || !currency
                .chars()
                .all(|character| character.is_ascii_uppercase())
    }) {
        return Err(AppError::invalid_request(
            "currency must be a three letter uppercase code",
        ));
    }
    if input.cost_minor.is_some_and(|cost| cost < 0) {
        return Err(AppError::invalid_request("cost_minor must not be negative"));
    }
    Ok(())
}

fn validate_object_ref(object_ref: Option<&ObjectRef>) -> Result<(), AppError> {
    let Some(object_ref) = object_ref else {
        return Ok(());
    };
    if object_ref.object_id.trim().is_empty() || object_ref.object_id.chars().count() > 500 {
        return Err(AppError::invalid_request(
            "object_id must be between 1 and 500 characters",
        ));
    }
    Ok(())
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

/// What one turn needs in order to run.
///
/// Assembled by the executor from the run the repository actually claimed, not
/// from the request: a runtime is never told an identifier or a scope the store
/// did not accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TurnInput {
    pub conversation_id: ConversationId,
    pub turn_id: TurnId,
    pub run_id: RunId,
    pub prompt: String,
}

/// What a runtime reports back for a turn that ran.
///
/// The content is not optional: there is no "succeeded with nothing to show"
/// shape, because a run that produced no answer is indistinguishable from a
/// fabricated one once it is persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TurnReport {
    pub content: String,
    #[serde(default)]
    pub metadata: Value,
}

/// The terminal outcome of one run.
///
/// Deliberately has no "succeeded without an answer" variant, so an answer and
/// a success cannot be recorded separately from each other.
#[derive(Debug, Clone, PartialEq)]
pub enum RunCompletion {
    Succeeded { content: String, metadata: Value },
    Failed { error: AppError },
}

/// Everything one terminal transition wrote, so a caller can report what
/// happened without re-reading the store.
#[derive(Debug, Clone, PartialEq)]
pub struct RunTransition {
    pub run: Run,
    pub turn: Turn,
    pub message: Option<Message>,
}

/// The boundary for a Rust-hosted runtime worker.  The API never fabricates a
/// model answer: when the capability is missing it records a failed run.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    async fn capability(&self) -> RuntimeCapability;

    /// Runs exactly one turn and reports what it produced.
    ///
    /// This lives on the runtime rather than beside it so that a configuration
    /// cannot report `available` without also being the thing that runs a turn.
    /// An assembly with no executor reports `capability_missing`, and the
    /// repository then refuses the work up front instead of accepting runs
    /// nothing will ever drive.
    async fn run_turn(&self, scope: &TenantScope, input: TurnInput)
    -> Result<TurnReport, AppError>;
}

#[derive(Debug, Default)]
pub struct MissingAgentRuntime;

/// The one reason an unconfigured assembly gives, whether it is refusing work at
/// acceptance time or reporting why a turn could not run.
pub const RUNTIME_NOT_CONFIGURED: &str = "embedded JavaScript runtime is not configured";

#[async_trait]
impl AgentRuntime for MissingAgentRuntime {
    async fn capability(&self) -> RuntimeCapability {
        RuntimeCapability::missing(RUNTIME_NOT_CONFIGURED)
    }

    async fn run_turn(
        &self,
        _scope: &TenantScope,
        _input: TurnInput,
    ) -> Result<TurnReport, AppError> {
        Err(AppError::capability_missing(RUNTIME_NOT_CONFIGURED))
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
    /// Claims a queued run for execution.
    ///
    /// `Queued → Running` is one guarded transition, so claiming is atomic:
    /// `None` means the run was never this caller's to run — already claimed,
    /// already terminal, or cancelled between acceptance and dispatch.  This
    /// transition, not a status check in the caller, is the guard, because a
    /// replayed Idempotency-Key returns the *stored* acceptance whose
    /// `run.status` still reads `queued` long after the run finished.
    async fn begin_run(&self, scope: &TenantScope, run_id: RunId) -> Result<Option<Run>, AppError>;
    /// Records a run's terminal outcome, its assistant message and the turn's
    /// terminal status together.
    ///
    /// `None` means the run was already terminal — a concurrent `cancel_turn`
    /// won — and nothing at all was written.  Success carries its answer by
    /// construction, so a `succeeded` run with no message and an answer
    /// attached to a still-`running` run are both unrepresentable.  An answer
    /// that fails validation produces a failed run with a typed error and no
    /// message, never a silently truncated one.
    async fn finish_run(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        completion: RunCompletion,
    ) -> Result<Option<RunTransition>, AppError>;
    async fn replay_events(
        &self,
        scope: &TenantScope,
        conversation_id: ConversationId,
        after: Option<u64>,
    ) -> Result<Vec<ConversationEvent>, AppError>;
    /// Stores or refreshes one step checkpoint.  Reusing a checkpoint whose
    /// `input_hash` changed is a conflict, never a silent overwrite.
    async fn store_checkpoint(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        checkpoint: StoreCheckpoint,
    ) -> Result<AgentCheckpoint, AppError>;
    /// Loads one checkpoint so a restarted worker can resume without replaying
    /// already completed steps.
    async fn load_checkpoint(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        checkpoint_scope: &str,
        step_key: &str,
    ) -> Result<Option<AgentCheckpoint>, AppError>;
    /// Appends one tool-call ledger entry.  The same `tool_call_id` may be
    /// replayed with the same digests but never with different arguments.
    async fn append_tool_call(
        &self,
        scope: &TenantScope,
        input: RecordToolCall,
    ) -> Result<ToolCallLedgerEntry, AppError>;
    async fn list_tool_calls(
        &self,
        scope: &TenantScope,
        run_id: RunId,
    ) -> Result<Vec<ToolCallLedgerEntry>, AppError>;
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
    checkpoints: HashMap<(RunId, String, String), AgentCheckpoint>,
    tool_calls: HashMap<(RunId, String), ToolCallLedgerEntry>,
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

    /// Resolves the run that owns a checkpoint or ledger entry, enforcing the
    /// same tenant/project boundary as every other read.
    fn visible_run(
        state: &AgentState,
        scope: &TenantScope,
        run_id: RunId,
    ) -> Result<Run, AppError> {
        state
            .runs
            .get(&run_id)
            .filter(|run| scope.contains(&run.scope()))
            .cloned()
            .ok_or_else(|| AppError::not_found("run not found"))
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
        let content = validate_append_message(&input)?;
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

    async fn begin_run(&self, scope: &TenantScope, run_id: RunId) -> Result<Option<Run>, AppError> {
        let mut state = self.state.write().await;
        let Some(run) = state
            .runs
            .get(&run_id)
            .filter(|run| scope.contains(&run.scope()))
            .cloned()
        else {
            return Ok(None);
        };
        if run.status != RunStatus::Queued {
            return Ok(None);
        }
        let conversation = state
            .conversations
            .get(&run.conversation_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("conversation not found"))?;
        let now = Utc::now();
        let mut claimed = run;
        claimed.status = RunStatus::Running;
        claimed.updated_at = now;
        state.runs.insert(claimed.id, claimed.clone());
        if let Some(turn) = state.turns.get_mut(&claimed.turn_id)
            && turn.status == TurnStatus::Queued
        {
            turn.status = TurnStatus::Running;
            turn.updated_at = now;
        }
        self.emit(
            &mut state,
            &conversation,
            "run.running",
            Some(claimed.turn_id),
            Some(claimed.id),
            json!({"run_id": claimed.id, "status": claimed.status}),
        )
        .await;
        Ok(Some(claimed))
    }

    async fn finish_run(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        completion: RunCompletion,
    ) -> Result<Option<RunTransition>, AppError> {
        let mut state = self.state.write().await;
        let Some(run) = state
            .runs
            .get(&run_id)
            .filter(|run| scope.contains(&run.scope()))
            .cloned()
        else {
            return Ok(None);
        };
        // Re-read under the write lock: a `cancel_turn` that landed while the
        // turn was executing has already made this run terminal, and its
        // verdict outranks ours.
        if run.status != RunStatus::Running {
            return Ok(None);
        }
        let conversation = state
            .conversations
            .get(&run.conversation_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("conversation not found"))?;
        let mut turn = state
            .turns
            .get(&run.turn_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("turn not found"))?;

        // An answer that cannot be stored is a failed run, never a truncated
        // one, and never a success with nothing to show.
        let outcome = match completion {
            RunCompletion::Succeeded { content, metadata } => {
                validate_message_content(&content).map(|content| (content, metadata))
            }
            RunCompletion::Failed { error } => Err(error),
        };
        let now = Utc::now();
        let (status, message, error) = match outcome {
            Ok((content, metadata)) => {
                let sequence = {
                    let next = state
                        .next_message_sequence
                        .entry(conversation.id)
                        .and_modify(|sequence| *sequence += 1)
                        .or_insert(1);
                    *next
                };
                let message = Message {
                    id: MessageId::from(Uuid::new_v4()),
                    conversation_id: conversation.id,
                    operator_id: conversation.operator_id,
                    tenant_id: conversation.tenant_id,
                    project_id: conversation.project_id,
                    turn_id: Some(run.turn_id),
                    role: MessageRole::Assistant,
                    content,
                    attachments: Vec::new(),
                    metadata,
                    sequence,
                    created_at: now,
                };
                (RunStatus::Succeeded, Some(message), None)
            }
            Err(error) => (RunStatus::Failed, None, Some(error)),
        };

        let mut finished = run;
        finished.status = status;
        finished.error = error;
        finished.updated_at = now;
        turn.status = match status {
            RunStatus::Succeeded => TurnStatus::Succeeded,
            _ => TurnStatus::Failed,
        };
        turn.updated_at = now;

        let mut updated_conversation = conversation;
        updated_conversation.revision += 1;
        updated_conversation.updated_at = now;

        state.runs.insert(finished.id, finished.clone());
        state.turns.insert(turn.id, turn.clone());
        state
            .conversations
            .insert(updated_conversation.id, updated_conversation.clone());
        if let Some(message) = &message {
            state.messages.insert(message.id, message.clone());
            self.emit(
                &mut state,
                &updated_conversation,
                "message.created",
                Some(turn.id),
                Some(finished.id),
                json!({"message": message}),
            )
            .await;
        }
        let (event_type, payload) = match &finished.error {
            Some(error) => (
                "run.failed",
                json!({"run_id": finished.id, "status": finished.status, "error": error}),
            ),
            None => (
                "run.succeeded",
                json!({"run_id": finished.id, "status": finished.status}),
            ),
        };
        self.emit(
            &mut state,
            &updated_conversation,
            event_type,
            Some(turn.id),
            Some(finished.id),
            payload,
        )
        .await;

        Ok(Some(RunTransition {
            run: finished,
            turn,
            message,
        }))
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

    async fn store_checkpoint(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        checkpoint: StoreCheckpoint,
    ) -> Result<AgentCheckpoint, AppError> {
        validate_checkpoint_write(&checkpoint)?;
        let mut state = self.state.write().await;
        let run = Self::visible_run(&state, scope, run_id)?;
        let key = (
            run.id,
            checkpoint.checkpoint_scope.clone(),
            checkpoint.step_key.clone(),
        );
        let now = Utc::now();
        let existing = state.checkpoints.get(&key).cloned();
        let stored = match existing {
            Some(existing) if existing.input_hash == checkpoint.input_hash => AgentCheckpoint {
                result_ref: checkpoint.result_ref,
                state: checkpoint.state,
                version: existing.version + 1,
                updated_at: now,
                ..existing
            },
            Some(_) => {
                return Err(AppError::conflict(
                    "checkpoint was already stored for a different input",
                ));
            }
            None => AgentCheckpoint {
                id: CheckpointId::from(Uuid::new_v4()),
                run_id: run.id,
                conversation_id: run.conversation_id,
                operator_id: run.operator_id,
                tenant_id: run.tenant_id,
                project_id: run.project_id,
                checkpoint_scope: checkpoint.checkpoint_scope,
                step_key: checkpoint.step_key,
                input_hash: checkpoint.input_hash,
                result_ref: checkpoint.result_ref,
                version: 1,
                state: checkpoint.state,
                created_at: now,
                updated_at: now,
            },
        };
        state.checkpoints.insert(key, stored.clone());
        Ok(stored)
    }

    async fn load_checkpoint(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        checkpoint_scope: &str,
        step_key: &str,
    ) -> Result<Option<AgentCheckpoint>, AppError> {
        let state = self.state.read().await;
        Self::visible_run(&state, scope, run_id)?;
        Ok(state
            .checkpoints
            .get(&(run_id, checkpoint_scope.to_owned(), step_key.to_owned()))
            .cloned())
    }

    async fn append_tool_call(
        &self,
        scope: &TenantScope,
        input: RecordToolCall,
    ) -> Result<ToolCallLedgerEntry, AppError> {
        validate_tool_call_write(&input)?;
        let mut state = self.state.write().await;
        let run = Self::visible_run(&state, scope, input.run_id)?;
        let key = (run.id, input.tool_call_id.clone());
        if let Some(existing) = state.tool_calls.get(&key) {
            if existing.arguments_hash != input.arguments_hash
                || existing.idempotency_key_hash != input.idempotency_key_hash
            {
                return Err(AppError::conflict(
                    "tool call was already recorded with different arguments",
                ));
            }
            return Ok(existing.clone());
        }
        let now = Utc::now();
        let entry = ToolCallLedgerEntry {
            id: ToolCallLedgerId::from(Uuid::new_v4()),
            run_id: run.id,
            turn_id: run.turn_id,
            conversation_id: run.conversation_id,
            operator_id: run.operator_id,
            tenant_id: run.tenant_id,
            project_id: run.project_id,
            tool_call_id: input.tool_call_id,
            tool_name: input.tool_name,
            arguments_hash: input.arguments_hash,
            idempotency_key_hash: input.idempotency_key_hash,
            permission: input.permission,
            budget: input.budget,
            intent: input.intent,
            attempt_count: input.attempt_count,
            result_ref: input.result_ref,
            outcome: input.outcome,
            cost_minor: input.cost_minor,
            currency: input.currency,
            created_at: now,
            updated_at: now,
        };
        state.tool_calls.insert(key, entry.clone());
        Ok(entry)
    }

    async fn list_tool_calls(
        &self,
        scope: &TenantScope,
        run_id: RunId,
    ) -> Result<Vec<ToolCallLedgerEntry>, AppError> {
        let state = self.state.read().await;
        Self::visible_run(&state, scope, run_id)?;
        let mut result = state
            .tool_calls
            .values()
            .filter(|entry| entry.run_id == run_id)
            .cloned()
            .collect::<Vec<_>>();
        result.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(result)
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

    async fn accepted_run(repository: &MemoryAgentRepository, scope: &TenantScope) -> Run {
        let conversation = repository
            .create_conversation(scope, None, CreateConversation::default())
            .await
            .expect("create");
        repository
            .append_message(
                scope,
                conversation.id,
                message("hello"),
                "accepted-key".to_owned(),
                "accepted-body".to_owned(),
                RuntimeCapability::available("test", Some("1".to_owned())),
            )
            .await
            .expect("append")
            .run
    }

    #[tokio::test]
    async fn checkpoints_are_restorable_and_reject_a_changed_input() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let run = accepted_run(&repository, &scope).await;
        let checkpoint = |input_hash: &str, cursor: i64| StoreCheckpoint {
            checkpoint_scope: "loop".to_owned(),
            step_key: "collect".to_owned(),
            input_hash: input_hash.to_owned(),
            result_ref: None,
            state: json!({"cursor": cursor}),
        };
        let stored = repository
            .store_checkpoint(&scope, run.id, checkpoint("digest-a", 3))
            .await
            .expect("store");
        assert_eq!(stored.version, 1);
        let loaded = repository
            .load_checkpoint(&scope, run.id, "loop", "collect")
            .await
            .expect("load")
            .expect("present");
        assert_eq!(loaded, stored);
        let refreshed = repository
            .store_checkpoint(&scope, run.id, checkpoint("digest-a", 4))
            .await
            .expect("refresh");
        assert_eq!(refreshed.version, 2);
        assert_eq!(refreshed.id, stored.id);
        assert_eq!(refreshed.state, json!({"cursor": 4}));
        let conflict = repository
            .store_checkpoint(&scope, run.id, checkpoint("digest-b", 5))
            .await
            .expect_err("changed input");
        assert_eq!(conflict.code, crate::ErrorCode::Conflict);
        let other_project = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        let cross_project = repository
            .load_checkpoint(&other_project, run.id, "loop", "collect")
            .await
            .expect_err("cross-project checkpoint");
        assert_eq!(cross_project.code, crate::ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn tool_call_ledger_is_idempotent_per_tool_call_id() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let run = accepted_run(&repository, &scope).await;
        let record = |tool_call_id: &str, arguments_hash: &str| RecordToolCall {
            run_id: run.id,
            tool_call_id: tool_call_id.to_owned(),
            tool_name: "geo.publish".to_owned(),
            arguments_hash: arguments_hash.to_owned(),
            idempotency_key_hash: "ledger-key".to_owned(),
            permission: ToolCallDecision::Allowed,
            budget: ToolCallDecision::Allowed,
            intent: json!({"document_id": "doc-1"}),
            attempt_count: 0,
            result_ref: None,
            outcome: ToolCallOutcome::Intent,
            cost_minor: Some(12),
            currency: Some("CNY".to_owned()),
        };
        let appended = repository
            .append_tool_call(&scope, record("call-1", "args-a"))
            .await
            .expect("append");
        assert_eq!(appended.turn_id, run.turn_id);
        assert_eq!(
            repository
                .append_tool_call(&scope, record("call-1", "args-a"))
                .await
                .expect("replay"),
            appended
        );
        let conflict = repository
            .append_tool_call(&scope, record("call-1", "args-b"))
            .await
            .expect_err("different arguments");
        assert_eq!(conflict.code, crate::ErrorCode::Conflict);
        let listed = repository
            .list_tool_calls(&scope, run.id)
            .await
            .expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].outcome, ToolCallOutcome::Intent);
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

    /// Claims a queued run exactly once: a second claim, or a claim after the
    /// turn was cancelled, is not this caller's work to do.
    #[tokio::test]
    async fn a_queued_run_is_claimed_exactly_once() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let run = accepted_run(&repository, &scope).await;
        assert_eq!(run.status, RunStatus::Queued);

        let claimed = repository
            .begin_run(&scope, run.id)
            .await
            .expect("begin")
            .expect("a queued run is claimable");
        assert_eq!(claimed.status, RunStatus::Running);
        assert!(
            repository
                .begin_run(&scope, run.id)
                .await
                .expect("begin")
                .is_none(),
            "a run already claimed must not be claimable twice"
        );

        let second = accepted_run(&repository, &scope).await;
        repository
            .cancel_turn(&scope, second.turn_id)
            .await
            .expect("cancel");
        assert!(
            repository
                .begin_run(&scope, second.id)
                .await
                .expect("begin")
                .is_none(),
            "a cancelled run must not be claimable"
        );
    }

    /// A run is not claimable through another project's scope, so the executor
    /// cannot be pointed at a conversation it does not own.
    #[tokio::test]
    async fn a_run_is_not_claimable_outside_its_scope() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let run = accepted_run(&repository, &scope).await;
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert!(
            repository
                .begin_run(&other, run.id)
                .await
                .expect("begin")
                .is_none()
        );
        assert!(
            repository
                .finish_run(
                    &other,
                    run.id,
                    RunCompletion::Succeeded {
                        content: "answer".to_owned(),
                        metadata: Value::Null,
                    },
                )
                .await
                .expect("finish")
                .is_none()
        );
    }

    /// A successful turn writes exactly one assistant message, terminalises the
    /// turn and the run together, and reports all of it through the event log.
    #[tokio::test]
    async fn finishing_a_run_records_the_answer_once() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let run = accepted_run(&repository, &scope).await;
        repository
            .begin_run(&scope, run.id)
            .await
            .expect("begin")
            .expect("claim");

        let transition = repository
            .finish_run(
                &scope,
                run.id,
                RunCompletion::Succeeded {
                    content: "  bridge:hello  ".to_owned(),
                    metadata: json!({"model": "test"}),
                },
            )
            .await
            .expect("finish")
            .expect("a running run is finishable");

        assert_eq!(transition.run.status, RunStatus::Succeeded);
        assert!(transition.run.error.is_none());
        assert_eq!(transition.turn.status, TurnStatus::Succeeded);
        let message = transition.message.expect("a success carries its answer");
        assert_eq!(message.role, MessageRole::Assistant);
        assert_eq!(message.content, "bridge:hello", "the answer is trimmed");
        assert_eq!(message.turn_id, Some(run.turn_id));
        assert_eq!(message.sequence, 2, "it follows the user's message");

        let detail = repository
            .get_conversation(&scope, run.conversation_id)
            .await
            .expect("detail")
            .expect("the conversation exists");
        assert_eq!(detail.messages.len(), 2);
        assert_eq!(
            detail
                .messages
                .iter()
                .filter(|message| message.role == MessageRole::Assistant)
                .count(),
            1
        );

        let events = repository
            .replay_events(&scope, run.conversation_id, Some(0))
            .await
            .expect("events");
        let types = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            types,
            vec![
                "conversation.created",
                "message.created",
                "turn.accepted",
                "run.running",
                "message.created",
                "run.succeeded"
            ],
            "a turn must be observable from acceptance to terminal state"
        );
        assert!(
            repository
                .finish_run(
                    &scope,
                    run.id,
                    RunCompletion::Succeeded {
                        content: "second".to_owned(),
                        metadata: Value::Null,
                    },
                )
                .await
                .expect("finish")
                .is_none(),
            "a terminal run must not be finishable again"
        );
        let detail = repository
            .get_conversation(&scope, run.conversation_id)
            .await
            .expect("detail")
            .expect("the conversation exists");
        assert_eq!(
            detail.messages.len(),
            2,
            "a rejected second finish must not append a second answer"
        );
    }

    /// A failed turn records the typed error and no answer at all: an empty
    /// assistant message would be indistinguishable from a fabricated one.
    #[tokio::test]
    async fn a_failed_run_records_no_answer() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let run = accepted_run(&repository, &scope).await;
        repository
            .begin_run(&scope, run.id)
            .await
            .expect("begin")
            .expect("claim");

        let transition = repository
            .finish_run(
                &scope,
                run.id,
                RunCompletion::Failed {
                    error: AppError::capability_missing("no model provider"),
                },
            )
            .await
            .expect("finish")
            .expect("a running run is finishable");

        assert_eq!(transition.run.status, RunStatus::Failed);
        assert_eq!(
            transition.run.error.as_ref().map(|error| error.code),
            Some(crate::ErrorCode::CapabilityMissing)
        );
        assert_eq!(transition.turn.status, TurnStatus::Failed);
        assert!(transition.message.is_none());
        let detail = repository
            .get_conversation(&scope, run.conversation_id)
            .await
            .expect("detail")
            .expect("the conversation exists");
        assert_eq!(
            detail.messages.len(),
            1,
            "a failed turn must not leave an assistant message behind"
        );
    }

    /// An answer that cannot be stored fails the run with a typed error instead
    /// of being truncated, and writes no message.
    #[tokio::test]
    async fn an_unstorable_answer_fails_the_run_without_storing_it() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        for content in ["   ".to_owned(), "x".repeat(MAX_MESSAGE_CHARS + 1)] {
            let run = accepted_run(&repository, &scope).await;
            repository
                .begin_run(&scope, run.id)
                .await
                .expect("begin")
                .expect("claim");
            let transition = repository
                .finish_run(
                    &scope,
                    run.id,
                    RunCompletion::Succeeded {
                        content,
                        metadata: Value::Null,
                    },
                )
                .await
                .expect("finish")
                .expect("a running run is finishable");
            assert_eq!(transition.run.status, RunStatus::Failed);
            assert_eq!(
                transition.run.error.as_ref().map(|error| error.code),
                Some(crate::ErrorCode::InvalidRequest)
            );
            assert!(transition.message.is_none());
            let detail = repository
                .get_conversation(&scope, run.conversation_id)
                .await
                .expect("detail")
                .expect("the conversation exists");
            assert_eq!(detail.messages.len(), 1);
        }
    }

    /// Cancellation outranks completion: a run cancelled while the turn was
    /// executing keeps the cancellation, and the answer is dropped rather than
    /// attached to a cancelled run.
    #[tokio::test]
    async fn cancellation_outranks_a_late_completion() {
        let repository = MemoryAgentRepository::new();
        let scope = scope(Uuid::new_v4());
        let run = accepted_run(&repository, &scope).await;
        repository
            .begin_run(&scope, run.id)
            .await
            .expect("begin")
            .expect("claim");
        repository
            .cancel_turn(&scope, run.turn_id)
            .await
            .expect("cancel");

        assert!(
            repository
                .finish_run(
                    &scope,
                    run.id,
                    RunCompletion::Succeeded {
                        content: "too late".to_owned(),
                        metadata: Value::Null,
                    },
                )
                .await
                .expect("finish")
                .is_none(),
            "a cancelled run is already terminal"
        );
        let detail = repository
            .get_conversation(&scope, run.conversation_id)
            .await
            .expect("detail")
            .expect("the conversation exists");
        assert_eq!(detail.runs[0].status, RunStatus::Cancelled);
        assert_eq!(detail.turns[0].status, TurnStatus::Cancelled);
        assert_eq!(detail.messages.len(), 1);
    }
}
