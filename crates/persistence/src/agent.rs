//! PostgreSQL implementation of the P00 agent repository.
//!
//! Every statement filters on the explicit operator/tenant/project columns and
//! every write runs inside a transaction that first sets the transaction-local
//! scope, so the mapping is ready for FORCE RLS once 0004 stops being a no-op.
//! Writes that allocate a per-conversation sequence or decide a state-machine
//! transition first take a row lock, which is what makes the sequence monotonic
//! and cancel-vs-completion a single serialized decision.
//!
//! A missing migration or an unreachable database surfaces as
//! `dependency_unavailable`; there is deliberately no in-memory fallback.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AgentCheckpoint, AgentRepository, AppError, AppendMessage, AttachmentId, AttachmentReference,
    CheckpointId, Conversation, ConversationDetail, ConversationEvent, ConversationEventId,
    ConversationId, ConversationStatus, CreateConversation, Message, MessageId, MessageRole,
    RecordToolCall, Run, RunCompletion, RunId, RunStatus, RunTransition, RuntimeCapability,
    StoreCheckpoint, SubmitAcceptance, TenantScope, ToolCallDecision, ToolCallLedgerEntry,
    ToolCallLedgerId, ToolCallOutcome, Turn, TurnId, TurnStatus, UserId, validate_append_message,
    validate_checkpoint_write, validate_message_content, validate_tool_call_write,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{Database, set_local_scope};

/// Live delivery fan-out for one process.  Durable replay is the source of
/// truth; live tailing stays per-process, exactly as in the in-memory store.
const EVENT_CHANNEL_CAPACITY: usize = 512;

#[derive(Clone)]
pub struct PgAgentRepository {
    pool: PgPool,
    events: broadcast::Sender<ConversationEvent>,
}

impl PgAgentRepository {
    pub fn new(pool: PgPool) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self { pool, events }
    }

    pub fn from_database(database: &Database) -> Self {
        Self::new(database.pool().clone())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn transaction(
        &self,
        scope: &TenantScope,
    ) -> Result<Transaction<'_, Postgres>, AppError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        set_local_scope(&mut transaction, scope)
            .await
            .map_err(database_error)?;
        Ok(transaction)
    }

    fn publish(&self, events: impl IntoIterator<Item = ConversationEvent>) {
        for event in events {
            let _ = self.events.send(event);
        }
    }
}

#[async_trait]
impl AgentRepository for PgAgentRepository {
    async fn list_conversations(&self, scope: &TenantScope) -> Result<Vec<Conversation>, AppError> {
        let mut transaction = self.transaction(scope).await?;
        let rows = sqlx::query_as::<_, ConversationRow>(
            r#"SELECT conversation_id, operator_id, tenant_id, project_id, created_by, title,
                      status, revision, created_at, updated_at
                 FROM agent_conversations
                WHERE operator_id = $1 AND tenant_id = $2
                  AND ($3::UUID IS NULL OR project_id = $3)
                ORDER BY updated_at DESC, conversation_id ASC"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.into_iter().map(Conversation::try_from).collect()
    }

    async fn get_conversation(
        &self,
        scope: &TenantScope,
        id: ConversationId,
    ) -> Result<Option<ConversationDetail>, AppError> {
        let mut transaction = self.transaction(scope).await?;
        let Some(conversation) = fetch_conversation(&mut transaction, scope, id).await? else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let mut attachments = fetch_attachments(&mut transaction, scope, id).await?;
        let messages = sqlx::query_as::<_, MessageRow>(
            r#"SELECT message_id, conversation_id, operator_id, tenant_id, project_id, turn_id,
                      role, content, metadata, sequence, created_at
                 FROM agent_messages
                WHERE operator_id = $1 AND tenant_id = $2 AND conversation_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)
                ORDER BY sequence ASC"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|row| {
            let message_attachments = attachments.remove(&row.message_id).unwrap_or_default();
            message_from_row(row, message_attachments)
        })
        .collect::<Result<Vec<_>, _>>()?;
        let turns = sqlx::query_as::<_, TurnRow>(
            r#"SELECT turn_id, conversation_id, root_message_id, previous_turn_id, run_id,
                      status, cancel_version, created_at, updated_at
                 FROM agent_turns
                WHERE operator_id = $1 AND tenant_id = $2 AND conversation_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)
                ORDER BY created_at ASC, turn_id ASC"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?
        .into_iter()
        .map(Turn::try_from)
        .collect::<Result<Vec<_>, _>>()?;
        let runs = sqlx::query_as::<_, RunRow>(
            r#"SELECT run_id, conversation_id, turn_id, operator_id, tenant_id, project_id,
                      status, capability, error, cancel_version, created_at, updated_at
                 FROM agent_runs
                WHERE operator_id = $1 AND tenant_id = $2 AND conversation_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)
                ORDER BY created_at ASC, run_id ASC"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?
        .into_iter()
        .map(Run::try_from)
        .collect::<Result<Vec<_>, _>>()?;
        transaction.commit().await.map_err(database_error)?;
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
        let mut transaction = self.transaction(scope).await?;
        sqlx::query(
            r#"INSERT INTO agent_conversations
                (conversation_id, operator_id, tenant_id, project_id, created_by, title,
                 status, revision, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        )
        .bind(conversation.id.as_uuid())
        .bind(conversation.operator_id.as_uuid())
        .bind(conversation.tenant_id.as_uuid())
        .bind(conversation.project_id.as_uuid())
        .bind(conversation.created_by.map(|user_id| user_id.as_uuid()))
        .bind(conversation.title.as_deref())
        .bind(conversation_status_text(conversation.status))
        .bind(counter_i64(conversation.revision)?)
        .bind(conversation.created_at)
        .bind(conversation.updated_at)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let event = insert_event(
            &mut transaction,
            &conversation,
            "conversation.created",
            None,
            None,
            json!({"conversation_id": conversation.id}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        self.publish([event]);
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
        let mut transaction = self.transaction(scope).await?;
        // The conversation row is the serialization point for every write that
        // appends to this conversation: idempotency replay, the single active
        // turn rule, message ordering and the event cursor.
        let Some(conversation) =
            lock_conversation(&mut transaction, scope, conversation_id).await?
        else {
            return Err(AppError::not_found("conversation not found"));
        };
        let submission = sqlx::query_as::<_, SubmissionRow>(
            r#"SELECT request_hash, message_id, turn_id, run_id, acceptance
                 FROM agent_submissions
                WHERE operator_id = $1 AND tenant_id = $2 AND conversation_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)
                  AND idempotency_key_hash = $5"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(conversation_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .bind(&idempotency_key_hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if let Some(submission) = submission {
            if submission.request_hash != request_hash {
                return Err(AppError::conflict(
                    "Idempotency-Key was already used with a different message request",
                ));
            }
            let acceptance = serde_json::from_value::<SubmitAcceptance>(submission.acceptance)
                .map_err(serialization_error)?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(acceptance);
        }
        if conversation.status != ConversationStatus::Active {
            return Err(AppError::conflict("conversation is archived"));
        }
        let active_turn: Option<Uuid> = sqlx::query_scalar(
            r#"SELECT turn_id FROM agent_turns
                WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3
                  AND conversation_id = $4 AND status IN ('queued', 'running')
                ORDER BY created_at DESC, turn_id DESC
                LIMIT 1"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(conversation.project_id.as_uuid())
        .bind(conversation_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if active_turn.is_some() {
            return Err(AppError::conflict(
                "conversation already has an active turn",
            ));
        }
        let previous_turn_id: Option<Uuid> = sqlx::query_scalar(
            r#"SELECT turn_id FROM agent_turns
                WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3
                  AND conversation_id = $4
                ORDER BY created_at DESC, turn_id DESC
                LIMIT 1"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(conversation.project_id.as_uuid())
        .bind(conversation_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let message_sequence: i64 = sqlx::query_scalar(
            r#"SELECT COALESCE(MAX(sequence), 0) + 1 FROM agent_messages
                WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3
                  AND conversation_id = $4"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(conversation.project_id.as_uuid())
        .bind(conversation_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let turn_id = TurnId::from(Uuid::new_v4());
        let run_id = RunId::from(Uuid::new_v4());
        let now = Utc::now();
        let mut updated_conversation = conversation.clone();
        updated_conversation.revision += 1;
        updated_conversation.updated_at = now;
        sqlx::query(
            r#"UPDATE agent_conversations
                  SET revision = revision + 1, updated_at = $4
                WHERE conversation_id = $1 AND operator_id = $2 AND tenant_id = $3"#,
        )
        .bind(conversation_id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
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
            sequence: counter_u64(message_sequence)?,
            created_at: now,
        };
        insert_message(&mut transaction, &message).await?;
        let mut turn = Turn {
            id: turn_id,
            conversation_id,
            root_message_id: message.id,
            previous_turn_id: previous_turn_id.map(TurnId::from),
            run_id: Some(run_id),
            status: TurnStatus::Queued,
            cancel_version: 0,
            created_at: now,
            updated_at: now,
        };
        insert_turn(&mut transaction, &updated_conversation, &turn).await?;
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
        insert_run(&mut transaction, &run).await?;
        let mut published = Vec::with_capacity(3);
        published.push(
            insert_event(
                &mut transaction,
                &updated_conversation,
                "message.created",
                Some(turn.id),
                Some(run.id),
                json!({"message": message}),
            )
            .await?,
        );
        published.push(
            insert_event(
                &mut transaction,
                &updated_conversation,
                "turn.accepted",
                Some(turn.id),
                Some(run.id),
                json!({"turn_id": turn.id, "run_id": run.id}),
            )
            .await?,
        );
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
            update_turn_status(&mut transaction, &updated_conversation, &turn).await?;
            update_run_status(&mut transaction, &run).await?;
            published.push(
                insert_event(
                    &mut transaction,
                    &updated_conversation,
                    "run.failed",
                    Some(turn.id),
                    Some(run.id),
                    json!({"run_id": run.id, "status": run.status, "error": error}),
                )
                .await?,
            );
        }
        let acceptance = SubmitAcceptance {
            conversation: updated_conversation,
            message,
            turn,
            run,
            events_url: format!("/api/v1/agent/conversations/{}/events", conversation_id),
        };
        sqlx::query(
            r#"INSERT INTO agent_submissions
                (submission_id, conversation_id, operator_id, tenant_id, project_id,
                 idempotency_key_hash, request_hash, message_id, turn_id, run_id,
                 acceptance, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
        )
        .bind(Uuid::new_v4())
        .bind(conversation_id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(conversation.project_id.as_uuid())
        .bind(&idempotency_key_hash)
        .bind(&request_hash)
        .bind(acceptance.message.id.as_uuid())
        .bind(acceptance.turn.id.as_uuid())
        .bind(acceptance.run.id.as_uuid())
        .bind(serde_json::to_value(&acceptance).map_err(serialization_error)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            conflict_or_database_error(
                error,
                "Idempotency-Key was already used with a different message request",
            )
        })?;
        transaction.commit().await.map_err(database_error)?;
        self.publish(published);
        Ok(acceptance)
    }

    async fn cancel_turn(&self, scope: &TenantScope, turn_id: TurnId) -> Result<Run, AppError> {
        let mut transaction = self.transaction(scope).await?;
        let turn = sqlx::query_as::<_, TurnRow>(
            r#"SELECT turn_id, conversation_id, root_message_id, previous_turn_id, run_id,
                      status, cancel_version, created_at, updated_at
                 FROM agent_turns
                WHERE turn_id = $1 AND operator_id = $2 AND tenant_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)"#,
        )
        .bind(turn_id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("turn not found"))?;
        let run_id = turn
            .run_id
            .ok_or_else(|| AppError::not_found("run not found"))?;
        // Locking the run makes cancel and completion one decision: whoever
        // commits second observes the terminal status and leaves it untouched.
        let run = sqlx::query_as::<_, RunRow>(
            r#"SELECT run_id, conversation_id, turn_id, operator_id, tenant_id, project_id,
                      status, capability, error, cancel_version, created_at, updated_at
                 FROM agent_runs
                WHERE run_id = $1 AND operator_id = $2 AND tenant_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)
                FOR UPDATE"#,
        )
        .bind(run_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("run not found"))?;
        let run = Run::try_from(run)?;
        if matches!(
            run.status,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Cancelled
        ) {
            transaction.commit().await.map_err(database_error)?;
            return Ok(run);
        }
        let conversation_id = ConversationId::from(turn.conversation_id);
        let conversation = fetch_conversation(&mut transaction, scope, conversation_id)
            .await?
            .ok_or_else(|| AppError::not_found("conversation not found"))?;
        let now = Utc::now();
        let cancel_version = counter_u64(turn.cancel_version)? + 1;
        let mut cancelled_turn = Turn::try_from(turn)?;
        cancelled_turn.status = TurnStatus::Cancelled;
        cancelled_turn.cancel_version = cancel_version;
        cancelled_turn.updated_at = now;
        let mut cancelled_run = run;
        cancelled_run.status = RunStatus::Cancelled;
        cancelled_run.cancel_version = cancel_version;
        cancelled_run.updated_at = now;
        update_turn_status(&mut transaction, &conversation, &cancelled_turn).await?;
        update_run_status(&mut transaction, &cancelled_run).await?;
        let event = insert_event(
            &mut transaction,
            &conversation,
            "run.cancelled",
            Some(cancelled_turn.id),
            Some(cancelled_run.id),
            json!({"run_id": cancelled_run.id, "cancel_version": cancelled_run.cancel_version}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        self.publish([event]);
        Ok(cancelled_run)
    }

    async fn begin_run(&self, scope: &TenantScope, run_id: RunId) -> Result<Option<Run>, AppError> {
        let mut transaction = self.transaction(scope).await?;
        // The conversation is located first and locked first: the guarded run
        // update below is what decides the claim, but every transaction that
        // touches both rows takes them in this order, so none can deadlock
        // against another.
        let Some(existing) = fetch_run(&mut transaction, scope, run_id).await? else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let Some(conversation) =
            lock_conversation(&mut transaction, scope, existing.conversation_id).await?
        else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        // `queued -> running` in one statement is the claim.  A run that is
        // already running, already terminal, or cancelled between acceptance
        // and dispatch matches nothing and is not this caller's to run.
        let Some(claimed) = sqlx::query_as::<_, RunRow>(
            r#"UPDATE agent_runs
                  SET status = 'running', updated_at = $5
                WHERE run_id = $1 AND operator_id = $2 AND tenant_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)
                  AND status = 'queued'
            RETURNING run_id, conversation_id, turn_id, operator_id, tenant_id, project_id,
                      status, capability, error, cancel_version, created_at, updated_at"#,
        )
        .bind(run_id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .bind(Utc::now())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let claimed = Run::try_from(claimed)?;
        // The turn follows its run.  A turn that another writer already made
        // terminal cannot be moved back, so the update is guarded too.
        sqlx::query(
            r#"UPDATE agent_turns
                  SET status = 'running', updated_at = $3
                WHERE turn_id = $1 AND conversation_id = $2 AND status = 'queued'"#,
        )
        .bind(claimed.turn_id.as_uuid())
        .bind(claimed.conversation_id.as_uuid())
        .bind(claimed.updated_at)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let event = insert_event(
            &mut transaction,
            &conversation,
            "run.running",
            Some(claimed.turn_id),
            Some(claimed.id),
            json!({"run_id": claimed.id, "status": claimed.status}),
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        self.publish([event]);
        Ok(Some(claimed))
    }

    async fn finish_run(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        completion: RunCompletion,
    ) -> Result<Option<RunTransition>, AppError> {
        let mut transaction = self.transaction(scope).await?;
        let Some(existing) = fetch_run(&mut transaction, scope, run_id).await? else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let Some(conversation) =
            lock_conversation(&mut transaction, scope, existing.conversation_id).await?
        else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        // Locking the run makes completion and cancellation one decision.
        // Under READ COMMITTED a blocked `FOR UPDATE` re-reads the row after
        // the lock is granted, so a `Cancelled` committed in the meantime is
        // observed here rather than overwritten.
        let Some(row) = sqlx::query_as::<_, RunRow>(
            r#"SELECT run_id, conversation_id, turn_id, operator_id, tenant_id, project_id,
                      status, capability, error, cancel_version, created_at, updated_at
                 FROM agent_runs
                WHERE run_id = $1 AND operator_id = $2 AND tenant_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4)
                FOR UPDATE"#,
        )
        .bind(run_id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let run = Run::try_from(row)?;
        if run.status != RunStatus::Running {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        }
        let Some(turn_row) = sqlx::query_as::<_, TurnRow>(
            r#"SELECT turn_id, conversation_id, root_message_id, previous_turn_id, run_id,
                      status, cancel_version, created_at, updated_at
                 FROM agent_turns
                WHERE turn_id = $1 AND conversation_id = $2"#,
        )
        .bind(run.turn_id.as_uuid())
        .bind(run.conversation_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        else {
            return Err(AppError::not_found("turn not found"));
        };
        let mut turn = Turn::try_from(turn_row)?;

        // An answer that cannot be stored is a failed run, never a truncated
        // one, and never a success with nothing to show.  Checking here rather
        // than letting the `agent_messages` constraint reject it keeps the
        // outcome a typed domain error instead of an opaque database failure.
        let outcome = match completion {
            RunCompletion::Succeeded { content, metadata } => {
                validate_message_content(&content).map(|content| (content, metadata))
            }
            RunCompletion::Failed { error } => Err(error),
        };
        let now = Utc::now();
        let (status, message, error) = match outcome {
            Ok((content, metadata)) => {
                let message_sequence: i64 = sqlx::query_scalar(
                    r#"SELECT COALESCE(MAX(sequence), 0) + 1 FROM agent_messages
                        WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3
                          AND conversation_id = $4"#,
                )
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(conversation.project_id.as_uuid())
                .bind(run.conversation_id.as_uuid())
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
                (
                    RunStatus::Succeeded,
                    Some(Message {
                        id: MessageId::from(Uuid::new_v4()),
                        conversation_id: run.conversation_id,
                        operator_id: conversation.operator_id,
                        tenant_id: conversation.tenant_id,
                        project_id: conversation.project_id,
                        turn_id: Some(run.turn_id),
                        role: MessageRole::Assistant,
                        content,
                        attachments: Vec::new(),
                        metadata,
                        sequence: counter_u64(message_sequence)?,
                        created_at: now,
                    }),
                    None,
                )
            }
            Err(error) => (RunStatus::Failed, None, Some(error)),
        };

        let conversation_id = run.conversation_id;
        let mut finished = run;
        finished.status = status;
        finished.error = error;
        finished.updated_at = now;
        turn.status = match status {
            RunStatus::Succeeded => TurnStatus::Succeeded,
            _ => TurnStatus::Failed,
        };
        turn.updated_at = now;
        update_run_status(&mut transaction, &finished).await?;
        update_turn_status(&mut transaction, &conversation, &turn).await?;
        sqlx::query(
            r#"UPDATE agent_conversations
                  SET revision = revision + 1, updated_at = $4
                WHERE conversation_id = $1 AND operator_id = $2 AND tenant_id = $3"#,
        )
        .bind(conversation_id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let mut updated_conversation = conversation;
        updated_conversation.revision += 1;
        updated_conversation.updated_at = now;

        let mut published = Vec::with_capacity(2);
        if let Some(message) = &message {
            insert_message(&mut transaction, message).await?;
            published.push(
                insert_event(
                    &mut transaction,
                    &updated_conversation,
                    "message.created",
                    Some(turn.id),
                    Some(finished.id),
                    json!({"message": message}),
                )
                .await?,
            );
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
        published.push(
            insert_event(
                &mut transaction,
                &updated_conversation,
                event_type,
                Some(turn.id),
                Some(finished.id),
                payload,
            )
            .await?,
        );
        transaction.commit().await.map_err(database_error)?;
        self.publish(published);
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
        let mut transaction = self.transaction(scope).await?;
        if fetch_conversation(&mut transaction, scope, conversation_id)
            .await?
            .is_none()
        {
            return Err(AppError::not_found("conversation not found"));
        }
        let events = sqlx::query_as::<_, EventRow>(
            r#"SELECT event_id, operator_id, tenant_id, project_id, conversation_id, sequence,
                      event_type, turn_id, run_id, payload, occurred_at
                 FROM agent_conversation_events
                WHERE operator_id = $1 AND tenant_id = $2 AND conversation_id = $3
                  AND ($4::UUID IS NULL OR project_id = $4) AND sequence > $5
                ORDER BY sequence ASC"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(conversation_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .bind(counter_i64(after.unwrap_or(0))?)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        events
            .into_iter()
            .map(ConversationEvent::try_from)
            .collect()
    }

    async fn store_checkpoint(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        checkpoint: StoreCheckpoint,
    ) -> Result<AgentCheckpoint, AppError> {
        validate_checkpoint_write(&checkpoint)?;
        let mut transaction = self.transaction(scope).await?;
        let run = fetch_run(&mut transaction, scope, run_id)
            .await?
            .ok_or_else(|| AppError::not_found("run not found"))?;
        let existing = sqlx::query_as::<_, CheckpointRow>(
            r#"SELECT checkpoint_id, run_id, conversation_id, operator_id, tenant_id, project_id,
                      checkpoint_scope, step_key, input_hash, result_ref, version, state,
                      created_at, updated_at
                 FROM agent_checkpoints
                WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3
                  AND run_id = $4 AND checkpoint_scope = $5 AND step_key = $6
                FOR UPDATE"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run_id.as_uuid())
        .bind(&checkpoint.checkpoint_scope)
        .bind(&checkpoint.step_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let row = match existing {
            Some(existing) if existing.input_hash == checkpoint.input_hash => {
                sqlx::query_as::<_, CheckpointRow>(
                    r#"UPDATE agent_checkpoints
                          SET result_ref = $1, state = $2, version = version + 1, updated_at = $3
                        WHERE checkpoint_id = $4 AND operator_id = $5 AND tenant_id = $6
                          AND project_id = $7 AND run_id = $8 AND checkpoint_scope = $9
                          AND step_key = $10
                    RETURNING checkpoint_id, run_id, conversation_id, operator_id, tenant_id,
                              project_id, checkpoint_scope, step_key, input_hash, result_ref,
                              version, state, created_at, updated_at"#,
                )
                .bind(serde_json::to_value(&checkpoint.result_ref).map_err(serialization_error)?)
                .bind(checkpoint.state)
                .bind(Utc::now())
                .bind(existing.checkpoint_id)
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(run.project_id.as_uuid())
                .bind(run_id.as_uuid())
                .bind(&checkpoint.checkpoint_scope)
                .bind(&checkpoint.step_key)
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?
            }
            Some(_) => {
                return Err(AppError::conflict(
                    "checkpoint was already stored for a different input",
                ));
            }
            None => sqlx::query_as::<_, CheckpointRow>(
                r#"INSERT INTO agent_checkpoints
                        (checkpoint_id, run_id, conversation_id, operator_id, tenant_id, project_id,
                         checkpoint_scope, step_key, input_hash, result_ref, version, state,
                         created_at, updated_at)
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1, $11, $12, $12)
                    RETURNING checkpoint_id, run_id, conversation_id, operator_id, tenant_id,
                              project_id, checkpoint_scope, step_key, input_hash, result_ref,
                              version, state, created_at, updated_at"#,
            )
            .bind(Uuid::new_v4())
            .bind(run_id.as_uuid())
            .bind(run.conversation_id.as_uuid())
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(run.project_id.as_uuid())
            .bind(&checkpoint.checkpoint_scope)
            .bind(&checkpoint.step_key)
            .bind(&checkpoint.input_hash)
            .bind(serde_json::to_value(&checkpoint.result_ref).map_err(serialization_error)?)
            .bind(checkpoint.state)
            .bind(Utc::now())
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| {
                conflict_or_database_error(
                    error,
                    "checkpoint was already stored for a different input",
                )
            })?,
        };
        transaction.commit().await.map_err(database_error)?;
        AgentCheckpoint::try_from(row)
    }

    async fn load_checkpoint(
        &self,
        scope: &TenantScope,
        run_id: RunId,
        checkpoint_scope: &str,
        step_key: &str,
    ) -> Result<Option<AgentCheckpoint>, AppError> {
        let mut transaction = self.transaction(scope).await?;
        let run = fetch_run(&mut transaction, scope, run_id)
            .await?
            .ok_or_else(|| AppError::not_found("run not found"))?;
        let row = sqlx::query_as::<_, CheckpointRow>(
            r#"SELECT checkpoint_id, run_id, conversation_id, operator_id, tenant_id, project_id,
                      checkpoint_scope, step_key, input_hash, result_ref, version, state,
                      created_at, updated_at
                 FROM agent_checkpoints
                WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3
                  AND run_id = $4 AND checkpoint_scope = $5 AND step_key = $6"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run_id.as_uuid())
        .bind(checkpoint_scope)
        .bind(step_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        row.map(AgentCheckpoint::try_from).transpose()
    }

    async fn append_tool_call(
        &self,
        scope: &TenantScope,
        input: RecordToolCall,
    ) -> Result<ToolCallLedgerEntry, AppError> {
        validate_tool_call_write(&input)?;
        let mut transaction = self.transaction(scope).await?;
        let run = fetch_run(&mut transaction, scope, input.run_id)
            .await?
            .ok_or_else(|| AppError::not_found("run not found"))?;
        let existing = sqlx::query_as::<_, ToolCallRow>(
            r#"SELECT ledger_entry_id, run_id, turn_id, conversation_id, operator_id, tenant_id,
                      project_id, tool_call_id, tool_name, arguments_hash, idempotency_key_hash,
                      permission, budget, intent, attempt_count, result_ref, outcome, cost_minor,
                      currency, created_at, updated_at
                 FROM agent_tool_call_ledger
                WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3
                  AND run_id = $4 AND tool_call_id = $5
                FOR UPDATE"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run.id.as_uuid())
        .bind(&input.tool_call_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if let Some(existing) = existing {
            if existing.arguments_hash != input.arguments_hash
                || existing.idempotency_key_hash != input.idempotency_key_hash
            {
                return Err(AppError::conflict(
                    "tool call was already recorded with different arguments",
                ));
            }
            transaction.commit().await.map_err(database_error)?;
            return ToolCallLedgerEntry::try_from(existing);
        }
        let now = Utc::now();
        let row = sqlx::query_as::<_, ToolCallRow>(
            r#"INSERT INTO agent_tool_call_ledger
                (ledger_entry_id, run_id, turn_id, conversation_id, operator_id, tenant_id,
                 project_id, tool_call_id, tool_name, arguments_hash, idempotency_key_hash,
                 permission, budget, intent, attempt_count, result_ref, outcome, cost_minor,
                 currency, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                       $17, $18, $19, $20, $20)
            RETURNING ledger_entry_id, run_id, turn_id, conversation_id, operator_id, tenant_id,
                      project_id, tool_call_id, tool_name, arguments_hash, idempotency_key_hash,
                      permission, budget, intent, attempt_count, result_ref, outcome, cost_minor,
                      currency, created_at, updated_at"#,
        )
        .bind(Uuid::new_v4())
        .bind(run.id.as_uuid())
        .bind(run.turn_id.as_uuid())
        .bind(run.conversation_id.as_uuid())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(&input.tool_call_id)
        .bind(&input.tool_name)
        .bind(&input.arguments_hash)
        .bind(&input.idempotency_key_hash)
        .bind(tool_call_decision_text(input.permission))
        .bind(tool_call_decision_text(input.budget))
        .bind(input.intent)
        .bind(counter_i64(input.attempt_count)?)
        .bind(serde_json::to_value(&input.result_ref).map_err(serialization_error)?)
        .bind(tool_call_outcome_text(input.outcome))
        .bind(input.cost_minor)
        .bind(input.currency.as_deref())
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| {
            conflict_or_database_error(
                error,
                "tool call was already recorded with different arguments",
            )
        })?;
        transaction.commit().await.map_err(database_error)?;
        ToolCallLedgerEntry::try_from(row)
    }

    async fn list_tool_calls(
        &self,
        scope: &TenantScope,
        run_id: RunId,
    ) -> Result<Vec<ToolCallLedgerEntry>, AppError> {
        let mut transaction = self.transaction(scope).await?;
        let run = fetch_run(&mut transaction, scope, run_id)
            .await?
            .ok_or_else(|| AppError::not_found("run not found"))?;
        let rows = sqlx::query_as::<_, ToolCallRow>(
            r#"SELECT ledger_entry_id, run_id, turn_id, conversation_id, operator_id, tenant_id,
                      project_id, tool_call_id, tool_name, arguments_hash, idempotency_key_hash,
                      permission, budget, intent, attempt_count, result_ref, outcome, cost_minor,
                      currency, created_at, updated_at
                 FROM agent_tool_call_ledger
                WHERE operator_id = $1 AND tenant_id = $2 AND project_id = $3 AND run_id = $4
                ORDER BY created_at ASC, ledger_entry_id ASC"#,
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run_id.as_uuid())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.into_iter()
            .map(ToolCallLedgerEntry::try_from)
            .collect()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<ConversationEvent> {
        self.events.subscribe()
    }
}

async fn fetch_conversation(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    conversation_id: ConversationId,
) -> Result<Option<Conversation>, AppError> {
    let row = sqlx::query_as::<_, ConversationRow>(
        r#"SELECT conversation_id, operator_id, tenant_id, project_id, created_by, title,
                  status, revision, created_at, updated_at
             FROM agent_conversations
            WHERE conversation_id = $1 AND operator_id = $2 AND tenant_id = $3
              AND ($4::UUID IS NULL OR project_id = $4)"#,
    )
    .bind(conversation_id.as_uuid())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    row.map(Conversation::try_from).transpose()
}

/// Reads the conversation while holding its row lock, so concurrent appends to
/// the same conversation are serialized rather than racing on the sequence.
async fn lock_conversation(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    conversation_id: ConversationId,
) -> Result<Option<Conversation>, AppError> {
    let row = sqlx::query_as::<_, ConversationRow>(
        r#"SELECT conversation_id, operator_id, tenant_id, project_id, created_by, title,
                  status, revision, created_at, updated_at
             FROM agent_conversations
            WHERE conversation_id = $1 AND operator_id = $2 AND tenant_id = $3
              AND ($4::UUID IS NULL OR project_id = $4)
            FOR UPDATE"#,
    )
    .bind(conversation_id.as_uuid())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    row.map(Conversation::try_from).transpose()
}

async fn fetch_run(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    run_id: RunId,
) -> Result<Option<Run>, AppError> {
    let row = sqlx::query_as::<_, RunRow>(
        r#"SELECT run_id, conversation_id, turn_id, operator_id, tenant_id, project_id,
                  status, capability, error, cancel_version, created_at, updated_at
             FROM agent_runs
            WHERE run_id = $1 AND operator_id = $2 AND tenant_id = $3
              AND ($4::UUID IS NULL OR project_id = $4)"#,
    )
    .bind(run_id.as_uuid())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    row.map(Run::try_from).transpose()
}

async fn fetch_attachments(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    conversation_id: ConversationId,
) -> Result<std::collections::HashMap<Uuid, Vec<AttachmentReference>>, AppError> {
    let rows = sqlx::query_as::<_, AttachmentRow>(
        r#"SELECT message_id, attachment_id, object_id, filename, media_type, size_bytes,
                  sha256, object_version
             FROM agent_message_attachments
            WHERE operator_id = $1 AND tenant_id = $2 AND conversation_id = $3
              AND ($4::UUID IS NULL OR project_id = $4)
            ORDER BY message_id ASC, ordinal ASC"#,
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(conversation_id.as_uuid())
    .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let mut grouped: std::collections::HashMap<Uuid, Vec<AttachmentReference>> =
        std::collections::HashMap::new();
    for row in rows {
        grouped
            .entry(row.message_id)
            .or_default()
            .push(AttachmentReference::try_from(row)?);
    }
    Ok(grouped)
}

async fn insert_message(
    transaction: &mut Transaction<'_, Postgres>,
    message: &Message,
) -> Result<(), AppError> {
    sqlx::query(
        r#"INSERT INTO agent_messages
            (message_id, conversation_id, operator_id, tenant_id, project_id, turn_id, role,
             content, metadata, sequence, created_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
    )
    .bind(message.id.as_uuid())
    .bind(message.conversation_id.as_uuid())
    .bind(message.operator_id.as_uuid())
    .bind(message.tenant_id.as_uuid())
    .bind(message.project_id.as_uuid())
    .bind(message.turn_id.map(|turn_id| turn_id.as_uuid()))
    .bind(message_role_text(message.role))
    .bind(&message.content)
    .bind(&message.metadata)
    .bind(counter_i64(message.sequence)?)
    .bind(message.created_at)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    for (ordinal, attachment) in message.attachments.iter().enumerate() {
        let ordinal = i32::try_from(ordinal).map_err(|_| {
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "agent message has more attachments than PostgreSQL can index",
            )
        })?;
        sqlx::query(
            r#"INSERT INTO agent_message_attachments
                (attachment_id, message_id, conversation_id, operator_id, tenant_id, project_id,
                 ordinal, object_id, filename, media_type, size_bytes, sha256, object_version,
                 created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
        )
        .bind(attachment.attachment_id.as_uuid())
        .bind(message.id.as_uuid())
        .bind(message.conversation_id.as_uuid())
        .bind(message.operator_id.as_uuid())
        .bind(message.tenant_id.as_uuid())
        .bind(message.project_id.as_uuid())
        .bind(ordinal)
        .bind(&attachment.object_id)
        .bind(&attachment.filename)
        .bind(attachment.media_type.as_deref())
        .bind(attachment.size_bytes.map(counter_i64).transpose()?)
        .bind(attachment.sha256.as_deref())
        .bind(attachment.object_version.as_deref())
        .bind(message.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn insert_turn(
    transaction: &mut Transaction<'_, Postgres>,
    conversation: &Conversation,
    turn: &Turn,
) -> Result<(), AppError> {
    sqlx::query(
        r#"INSERT INTO agent_turns
            (turn_id, conversation_id, operator_id, tenant_id, project_id, root_message_id,
             previous_turn_id, run_id, status, cancel_version, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
    )
    .bind(turn.id.as_uuid())
    .bind(turn.conversation_id.as_uuid())
    .bind(conversation.operator_id.as_uuid())
    .bind(conversation.tenant_id.as_uuid())
    .bind(conversation.project_id.as_uuid())
    .bind(turn.root_message_id.as_uuid())
    .bind(turn.previous_turn_id.map(|turn_id| turn_id.as_uuid()))
    .bind(turn.run_id.map(|run_id| run_id.as_uuid()))
    .bind(turn_status_text(turn.status))
    .bind(counter_i64(turn.cancel_version)?)
    .bind(turn.created_at)
    .bind(turn.updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn insert_run(
    transaction: &mut Transaction<'_, Postgres>,
    run: &Run,
) -> Result<(), AppError> {
    sqlx::query(
        r#"INSERT INTO agent_runs
            (run_id, conversation_id, turn_id, operator_id, tenant_id, project_id, status,
             capability, error, cancel_version, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
    )
    .bind(run.id.as_uuid())
    .bind(run.conversation_id.as_uuid())
    .bind(run.turn_id.as_uuid())
    .bind(run.operator_id.as_uuid())
    .bind(run.tenant_id.as_uuid())
    .bind(run.project_id.as_uuid())
    .bind(run_status_text(run.status))
    .bind(serde_json::to_value(&run.capability).map_err(serialization_error)?)
    .bind(
        run.error
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(serialization_error)?,
    )
    .bind(counter_i64(run.cancel_version)?)
    .bind(run.created_at)
    .bind(run.updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn update_turn_status(
    transaction: &mut Transaction<'_, Postgres>,
    conversation: &Conversation,
    turn: &Turn,
) -> Result<(), AppError> {
    sqlx::query(
        r#"UPDATE agent_turns
              SET status = $1, cancel_version = $2, updated_at = $3
            WHERE turn_id = $4 AND operator_id = $5 AND tenant_id = $6 AND project_id = $7
              AND conversation_id = $8"#,
    )
    .bind(turn_status_text(turn.status))
    .bind(counter_i64(turn.cancel_version)?)
    .bind(turn.updated_at)
    .bind(turn.id.as_uuid())
    .bind(conversation.operator_id.as_uuid())
    .bind(conversation.tenant_id.as_uuid())
    .bind(conversation.project_id.as_uuid())
    .bind(turn.conversation_id.as_uuid())
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn update_run_status(
    transaction: &mut Transaction<'_, Postgres>,
    run: &Run,
) -> Result<(), AppError> {
    sqlx::query(
        r#"UPDATE agent_runs
              SET status = $1, error = $2, cancel_version = $3, updated_at = $4
            WHERE run_id = $5 AND operator_id = $6 AND tenant_id = $7 AND project_id = $8
              AND conversation_id = $9"#,
    )
    .bind(run_status_text(run.status))
    .bind(
        run.error
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(serialization_error)?,
    )
    .bind(counter_i64(run.cancel_version)?)
    .bind(run.updated_at)
    .bind(run.id.as_uuid())
    .bind(run.operator_id.as_uuid())
    .bind(run.tenant_id.as_uuid())
    .bind(run.project_id.as_uuid())
    .bind(run.conversation_id.as_uuid())
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn insert_event(
    transaction: &mut Transaction<'_, Postgres>,
    conversation: &Conversation,
    event_type: &str,
    turn_id: Option<TurnId>,
    run_id: Option<RunId>,
    payload: Value,
) -> Result<ConversationEvent, AppError> {
    let row = sqlx::query_as::<_, EventRow>(
        r#"INSERT INTO agent_conversation_events
            (event_id, operator_id, tenant_id, project_id, conversation_id, sequence,
             event_type, turn_id, run_id, payload, occurred_at)
           VALUES ($1, $2, $3, $4, $5,
                   (SELECT COALESCE(MAX(sequence), 0) + 1
                      FROM agent_conversation_events
                     WHERE operator_id = $2 AND tenant_id = $3 AND project_id = $4
                       AND conversation_id = $5),
                   $6, $7, $8, $9, $10)
        RETURNING event_id, operator_id, tenant_id, project_id, conversation_id, sequence,
                  event_type, turn_id, run_id, payload, occurred_at"#,
    )
    .bind(Uuid::new_v4())
    .bind(conversation.operator_id.as_uuid())
    .bind(conversation.tenant_id.as_uuid())
    .bind(conversation.project_id.as_uuid())
    .bind(conversation.id.as_uuid())
    .bind(event_type)
    .bind(turn_id.map(|turn_id| turn_id.as_uuid()))
    .bind(run_id.map(|run_id| run_id.as_uuid()))
    .bind(payload)
    .bind(Utc::now())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    ConversationEvent::try_from(row)
}

#[derive(Debug, sqlx::FromRow)]
struct ConversationRow {
    conversation_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    created_by: Option<Uuid>,
    title: Option<String>,
    status: String,
    revision: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ConversationRow> for Conversation {
    type Error = AppError;

    fn try_from(row: ConversationRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ConversationId::from(row.conversation_id),
            operator_id: row.operator_id.into(),
            tenant_id: row.tenant_id.into(),
            project_id: row.project_id.into(),
            created_by: row.created_by.map(UserId::from),
            title: row.title,
            status: conversation_status_from_text(&row.status)?,
            revision: counter_u64(row.revision)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct MessageRow {
    message_id: Uuid,
    conversation_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    turn_id: Option<Uuid>,
    role: String,
    content: String,
    metadata: Value,
    sequence: i64,
    created_at: DateTime<Utc>,
}

fn message_from_row(
    row: MessageRow,
    attachments: Vec<AttachmentReference>,
) -> Result<Message, AppError> {
    Ok(Message {
        id: MessageId::from(row.message_id),
        conversation_id: ConversationId::from(row.conversation_id),
        operator_id: row.operator_id.into(),
        tenant_id: row.tenant_id.into(),
        project_id: row.project_id.into(),
        turn_id: row.turn_id.map(TurnId::from),
        role: message_role_from_text(&row.role)?,
        content: row.content,
        attachments,
        metadata: row.metadata,
        sequence: counter_u64(row.sequence)?,
        created_at: row.created_at,
    })
}

#[derive(Debug, sqlx::FromRow)]
struct AttachmentRow {
    message_id: Uuid,
    attachment_id: Uuid,
    object_id: String,
    filename: String,
    media_type: Option<String>,
    size_bytes: Option<i64>,
    sha256: Option<String>,
    object_version: Option<String>,
}

impl TryFrom<AttachmentRow> for AttachmentReference {
    type Error = AppError;

    fn try_from(row: AttachmentRow) -> Result<Self, Self::Error> {
        Ok(Self {
            attachment_id: AttachmentId::from(row.attachment_id),
            object_id: row.object_id,
            filename: row.filename,
            media_type: row.media_type,
            size_bytes: row.size_bytes.map(counter_u64).transpose()?,
            sha256: row.sha256,
            object_version: row.object_version,
        })
    }
}

/// The domain turn contract carries no scope columns; ownership is asserted by
/// the `WHERE` clause of every statement instead of being re-read into memory.
#[derive(Debug, sqlx::FromRow)]
struct TurnRow {
    turn_id: Uuid,
    conversation_id: Uuid,
    root_message_id: Uuid,
    previous_turn_id: Option<Uuid>,
    run_id: Option<Uuid>,
    status: String,
    cancel_version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<TurnRow> for Turn {
    type Error = AppError;

    fn try_from(row: TurnRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: TurnId::from(row.turn_id),
            conversation_id: ConversationId::from(row.conversation_id),
            root_message_id: MessageId::from(row.root_message_id),
            previous_turn_id: row.previous_turn_id.map(TurnId::from),
            run_id: row.run_id.map(RunId::from),
            status: turn_status_from_text(&row.status)?,
            cancel_version: counter_u64(row.cancel_version)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct RunRow {
    run_id: Uuid,
    conversation_id: Uuid,
    turn_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    status: String,
    capability: Value,
    error: Option<Value>,
    cancel_version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<RunRow> for Run {
    type Error = AppError;

    fn try_from(row: RunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: RunId::from(row.run_id),
            conversation_id: ConversationId::from(row.conversation_id),
            turn_id: TurnId::from(row.turn_id),
            operator_id: row.operator_id.into(),
            tenant_id: row.tenant_id.into(),
            project_id: row.project_id.into(),
            status: run_status_from_text(&row.status)?,
            capability: serde_json::from_value(row.capability).map_err(serialization_error)?,
            error: row
                .error
                .map(|error| serde_json::from_value(error).map_err(serialization_error))
                .transpose()?,
            cancel_version: counter_u64(row.cancel_version)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct EventRow {
    event_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    conversation_id: Uuid,
    sequence: i64,
    event_type: String,
    turn_id: Option<Uuid>,
    run_id: Option<Uuid>,
    payload: Value,
    occurred_at: DateTime<Utc>,
}

impl TryFrom<EventRow> for ConversationEvent {
    type Error = AppError;

    fn try_from(row: EventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ConversationEventId::from(row.event_id),
            conversation_id: ConversationId::from(row.conversation_id),
            operator_id: row.operator_id.into(),
            tenant_id: row.tenant_id.into(),
            project_id: row.project_id.into(),
            sequence: counter_u64(row.sequence)?,
            event_type: row.event_type,
            turn_id: row.turn_id.map(TurnId::from),
            run_id: row.run_id.map(RunId::from),
            payload: row.payload,
            occurred_at: row.occurred_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct SubmissionRow {
    request_hash: String,
    acceptance: Value,
}

#[derive(Debug, sqlx::FromRow)]
struct CheckpointRow {
    checkpoint_id: Uuid,
    run_id: Uuid,
    conversation_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    checkpoint_scope: String,
    step_key: String,
    input_hash: String,
    result_ref: Option<Value>,
    version: i64,
    state: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<CheckpointRow> for AgentCheckpoint {
    type Error = AppError;

    fn try_from(row: CheckpointRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: CheckpointId::from(row.checkpoint_id),
            run_id: RunId::from(row.run_id),
            conversation_id: ConversationId::from(row.conversation_id),
            operator_id: row.operator_id.into(),
            tenant_id: row.tenant_id.into(),
            project_id: row.project_id.into(),
            checkpoint_scope: row.checkpoint_scope,
            step_key: row.step_key,
            input_hash: row.input_hash,
            result_ref: row
                .result_ref
                .map(|result_ref| serde_json::from_value(result_ref).map_err(serialization_error))
                .transpose()?,
            version: counter_u64(row.version)?,
            state: row.state,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ToolCallRow {
    ledger_entry_id: Uuid,
    run_id: Uuid,
    turn_id: Uuid,
    conversation_id: Uuid,
    operator_id: Uuid,
    tenant_id: Uuid,
    project_id: Uuid,
    tool_call_id: String,
    tool_name: String,
    arguments_hash: String,
    idempotency_key_hash: String,
    permission: String,
    budget: String,
    intent: Value,
    attempt_count: i64,
    result_ref: Option<Value>,
    outcome: String,
    cost_minor: Option<i64>,
    currency: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ToolCallRow> for ToolCallLedgerEntry {
    type Error = AppError;

    fn try_from(row: ToolCallRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ToolCallLedgerId::from(row.ledger_entry_id),
            run_id: RunId::from(row.run_id),
            turn_id: TurnId::from(row.turn_id),
            conversation_id: ConversationId::from(row.conversation_id),
            operator_id: row.operator_id.into(),
            tenant_id: row.tenant_id.into(),
            project_id: row.project_id.into(),
            tool_call_id: row.tool_call_id,
            tool_name: row.tool_name,
            arguments_hash: row.arguments_hash,
            idempotency_key_hash: row.idempotency_key_hash,
            permission: tool_call_decision_from_text(&row.permission)?,
            budget: tool_call_decision_from_text(&row.budget)?,
            intent: row.intent,
            attempt_count: counter_u64(row.attempt_count)?,
            result_ref: row
                .result_ref
                .map(|result_ref| serde_json::from_value(result_ref).map_err(serialization_error))
                .transpose()?,
            outcome: tool_call_outcome_from_text(&row.outcome)?,
            cost_minor: row.cost_minor,
            currency: row.currency,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn conversation_status_text(status: ConversationStatus) -> &'static str {
    match status {
        ConversationStatus::Active => "active",
        ConversationStatus::Archived => "archived",
    }
}

fn conversation_status_from_text(value: &str) -> Result<ConversationStatus, AppError> {
    match value {
        "active" => Ok(ConversationStatus::Active),
        "archived" => Ok(ConversationStatus::Archived),
        other => Err(stored_text_error("conversation status", other)),
    }
}

fn message_role_text(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}

fn message_role_from_text(value: &str) -> Result<MessageRole, AppError> {
    match value {
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "system" => Ok(MessageRole::System),
        "tool" => Ok(MessageRole::Tool),
        other => Err(stored_text_error("message role", other)),
    }
}

fn turn_status_text(status: TurnStatus) -> &'static str {
    match status {
        TurnStatus::Queued => "queued",
        TurnStatus::Running => "running",
        TurnStatus::Succeeded => "succeeded",
        TurnStatus::Failed => "failed",
        TurnStatus::Cancelled => "cancelled",
    }
}

fn turn_status_from_text(value: &str) -> Result<TurnStatus, AppError> {
    match value {
        "queued" => Ok(TurnStatus::Queued),
        "running" => Ok(TurnStatus::Running),
        "succeeded" => Ok(TurnStatus::Succeeded),
        "failed" => Ok(TurnStatus::Failed),
        "cancelled" => Ok(TurnStatus::Cancelled),
        other => Err(stored_text_error("turn status", other)),
    }
}

fn run_status_text(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn run_status_from_text(value: &str) -> Result<RunStatus, AppError> {
    match value {
        "queued" => Ok(RunStatus::Queued),
        "running" => Ok(RunStatus::Running),
        "succeeded" => Ok(RunStatus::Succeeded),
        "failed" => Ok(RunStatus::Failed),
        "cancelled" => Ok(RunStatus::Cancelled),
        other => Err(stored_text_error("run status", other)),
    }
}

fn tool_call_decision_text(decision: ToolCallDecision) -> &'static str {
    match decision {
        ToolCallDecision::Allowed => "allowed",
        ToolCallDecision::Denied => "denied",
    }
}

fn tool_call_decision_from_text(value: &str) -> Result<ToolCallDecision, AppError> {
    match value {
        "allowed" => Ok(ToolCallDecision::Allowed),
        "denied" => Ok(ToolCallDecision::Denied),
        other => Err(stored_text_error("tool call decision", other)),
    }
}

fn tool_call_outcome_text(outcome: ToolCallOutcome) -> &'static str {
    match outcome {
        ToolCallOutcome::Intent => "intent",
        ToolCallOutcome::Attempted => "attempted",
        ToolCallOutcome::Succeeded => "succeeded",
        ToolCallOutcome::Failed => "failed",
        ToolCallOutcome::Unknown => "unknown",
    }
}

fn tool_call_outcome_from_text(value: &str) -> Result<ToolCallOutcome, AppError> {
    match value {
        "intent" => Ok(ToolCallOutcome::Intent),
        "attempted" => Ok(ToolCallOutcome::Attempted),
        "succeeded" => Ok(ToolCallOutcome::Succeeded),
        "failed" => Ok(ToolCallOutcome::Failed),
        "unknown" => Ok(ToolCallOutcome::Unknown),
        other => Err(stored_text_error("tool call outcome", other)),
    }
}

/// PostgreSQL has no unsigned integer columns; counters round-trip explicitly so
/// a value that cannot be represented fails loudly instead of wrapping.
fn counter_i64(value: u64) -> Result<i64, AppError> {
    i64::try_from(value).map_err(|_| {
        AppError::new(
            geo_domain::ErrorCode::Internal,
            "agent counter exceeds the PostgreSQL BIGINT range",
        )
    })
}

fn counter_u64(value: i64) -> Result<u64, AppError> {
    u64::try_from(value).map_err(|_| {
        AppError::new(
            geo_domain::ErrorCode::Internal,
            "agent counter is negative in PostgreSQL",
        )
    })
}

fn stored_text_error(field: &str, value: &str) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::Internal,
        format!("agent {field} {value:?} is not a known value"),
    )
}

fn database_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        format!("agent persistence is unavailable: {error}"),
    )
}

fn conflict_or_database_error(error: sqlx::Error, conflict: &str) -> AppError {
    if let sqlx::Error::Database(database) = &error
        && database.code().as_deref() == Some("23505")
    {
        return AppError::conflict(conflict);
    }
    database_error(error)
}

fn serialization_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::Internal,
        format!("agent serialization failed: {error}"),
    )
}
