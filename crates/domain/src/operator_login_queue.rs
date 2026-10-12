//! Creator-scoped, ordered operator account onboarding. No credential projections.
use crate::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorLoginQueueScope {
    pub operator_id: OperatorId,
    pub creator_user_id: Uuid,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorLoginQueueStatus {
    Running,
    Paused,
    Completed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorLoginQueueItemStatus {
    Pending,
    Current,
    Succeeded,
    Skipped,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorLoginQueueSelection {
    pub row_key: String,
    pub account_id: Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorLoginQueueItem {
    pub position: u32,
    pub row_key: String,
    pub account_id: Uuid,
    pub status: OperatorLoginQueueItemStatus,
    pub attempt: u32,
    pub operation_id: Option<Uuid>,
    pub owner_session_id: Option<Uuid>,
    pub error_code: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorLoginQueue {
    pub queue_id: Uuid,
    pub scope: OperatorLoginQueueScope,
    pub import_key: String,
    pub status: OperatorLoginQueueStatus,
    pub version: i64,
    pub items: Vec<OperatorLoginQueueItem>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorLoginQueueAction {
    Pause,
    Resume,
    ReconcileCurrent,
    RetryCurrent,
    SkipCurrent,
}

pub fn validate_login_queue_error(code: &str) -> Result<(), AppError> {
    if matches!(
        code,
        "login_not_completed"
            | "login_failed"
            | "identity_mismatch"
            | "network_unavailable"
            | "account_disabled"
            | "start_failed"
            | "runner_unavailable"
            | "session_recovery_required"
            | "disposal_unconfirmed"
            | "browser_unavailable"
    ) {
        Ok(())
    } else {
        Err(AppError::invalid_request("invalid login queue error code"))
    }
}
impl OperatorLoginQueue {
    pub fn new(
        scope: &OperatorLoginQueueScope,
        queue_id: Uuid,
        import_key: &str,
        selections: &[OperatorLoginQueueSelection],
        at: DateTime<Utc>,
    ) -> Result<Self, AppError> {
        let mut rows = std::collections::HashSet::new();
        let mut accounts = std::collections::HashSet::new();
        if queue_id.is_nil()
            || scope.creator_user_id.is_nil()
            || scope.operator_id.as_uuid().is_nil()
            || import_key.trim().is_empty()
            || import_key.len() > 200
            || import_key.chars().any(char::is_control)
            || selections.is_empty()
            || selections.len() > 200
        {
            return Err(AppError::invalid_request("invalid login queue selection"));
        }
        for row in selections {
            if row.row_key.trim().is_empty()
                || row.row_key.len() > 200
                || row.row_key.chars().any(char::is_control)
                || row.account_id.is_nil()
                || !rows.insert(&row.row_key)
                || !accounts.insert(row.account_id)
            {
                return Err(AppError::invalid_request(
                    "invalid or duplicate login queue item",
                ));
            }
        }
        Ok(Self {
            queue_id,
            scope: scope.clone(),
            import_key: import_key.into(),
            status: OperatorLoginQueueStatus::Running,
            version: 1,
            items: selections
                .iter()
                .enumerate()
                .map(|(position, row)| OperatorLoginQueueItem {
                    position: position as u32,
                    row_key: row.row_key.clone(),
                    account_id: row.account_id,
                    status: OperatorLoginQueueItemStatus::Pending,
                    attempt: 0,
                    operation_id: None,
                    owner_session_id: None,
                    error_code: None,
                })
                .collect(),
            created_at: at,
            updated_at: at,
        })
    }
    pub fn current(&self) -> Option<&OperatorLoginQueueItem> {
        self.items
            .iter()
            .find(|i| i.status == OperatorLoginQueueItemStatus::Current)
    }
    pub fn matches_creation(&self, candidate: &Self) -> bool {
        self.scope == candidate.scope
            && self.import_key == candidate.import_key
            && self.items.len() == candidate.items.len()
            && self.items.iter().zip(&candidate.items).all(|(a, b)| {
                a.position == b.position && a.row_key == b.row_key && a.account_id == b.account_id
            })
    }
    pub fn check_version(&self, expected: i64) -> Result<(), AppError> {
        if self.version != expected {
            Err(AppError::conflict("login queue version differs"))
        } else {
            Ok(())
        }
    }
    fn changed(&mut self, at: DateTime<Utc>) {
        self.version += 1;
        self.updated_at = at;
    }
    fn finish_if_empty(&mut self) {
        if self.items.iter().all(|i| {
            matches!(
                i.status,
                OperatorLoginQueueItemStatus::Succeeded | OperatorLoginQueueItemStatus::Skipped
            )
        }) {
            self.status = OperatorLoginQueueStatus::Completed;
        }
    }
    pub fn validate_current_operation(
        &self,
        operation: Option<&BrowserOperation>,
    ) -> Result<(), AppError> {
        match (self.current(), operation) {
            (None, None) => Ok(()),
            (Some(item), Some(op))
                if item.operation_id == Some(op.operation_id)
                    && item.owner_session_id == Some(op.scope.owner_id)
                    && item.account_id == op.account_id
                    && op.scope.operator_id == self.scope.operator_id
                    && op.scope.tenant_id.is_none()
                    && op.scope.project_id.is_none()
                    && op.purpose == BrowserOperationPurpose::Login =>
            {
                Ok(())
            }
            _ => Err(AppError::conflict("login queue operation differs")),
        }
    }
    pub fn apply(
        &mut self,
        expected: i64,
        action: OperatorLoginQueueAction,
        operation: Option<&BrowserOperation>,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.check_version(expected)?;
        self.validate_current_operation(operation)?;
        let before = self.clone();
        match action {
            OperatorLoginQueueAction::RetryCurrent | OperatorLoginQueueAction::SkipCurrent => {
                if operation.is_none_or(|op| op.state != BrowserOperationState::Disposed) {
                    return Err(AppError::conflict("login queue browser disposal required"));
                }
                let item = self
                    .items
                    .iter_mut()
                    .find(|i| i.status == OperatorLoginQueueItemStatus::Current)
                    .ok_or_else(|| AppError::conflict("login queue has no current item"))?;
                item.operation_id = None;
                item.owner_session_id = None;
                item.error_code = None;
                if action == OperatorLoginQueueAction::RetryCurrent {
                    item.status = OperatorLoginQueueItemStatus::Pending;
                    item.attempt = item
                        .attempt
                        .checked_add(1)
                        .ok_or_else(|| AppError::conflict("login queue attempts exhausted"))?;
                    self.status = OperatorLoginQueueStatus::Paused;
                } else {
                    item.status = OperatorLoginQueueItemStatus::Skipped;
                }
            }
            _ => {
                if let Some(op) = operation.filter(|op| op.state == BrowserOperationState::Disposed)
                {
                    let item = self
                        .items
                        .iter_mut()
                        .find(|i| i.status == OperatorLoginQueueItemStatus::Current)
                        .expect("validated current");
                    if op.login_verified_at.is_some() {
                        item.status = OperatorLoginQueueItemStatus::Succeeded;
                        item.operation_id = None;
                        item.owner_session_id = None;
                        item.error_code = None;
                    } else {
                        self.status = OperatorLoginQueueStatus::Paused;
                        item.error_code = Some("login_not_completed".into());
                    }
                }
                match action {
                    OperatorLoginQueueAction::Pause
                        if self.status != OperatorLoginQueueStatus::Completed =>
                    {
                        self.status = OperatorLoginQueueStatus::Paused
                    }
                    OperatorLoginQueueAction::Resume
                        if self.status != OperatorLoginQueueStatus::Completed =>
                    {
                        if self
                            .current()
                            .is_some_and(|i| i.error_code.as_deref() == Some("login_not_completed"))
                        {
                            return Err(AppError::conflict("login queue requires retry or skip"));
                        }
                        self.status = OperatorLoginQueueStatus::Running;
                    }
                    _ => {}
                }
            }
        }
        self.finish_if_empty();
        if *self != before {
            self.changed(at);
        }
        Ok(())
    }
    pub fn claim(
        &mut self,
        expected: i64,
        owner: Uuid,
        input: &BeginBrowserOperation,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.check_version(expected)?;
        if self.status != OperatorLoginQueueStatus::Running
            || self.current().is_some()
            || owner.is_nil()
            || input.purpose != BrowserOperationPurpose::Login
        {
            return Err(AppError::conflict("login queue cannot claim item"));
        }
        let item = self
            .items
            .iter_mut()
            .find(|i| i.status == OperatorLoginQueueItemStatus::Pending)
            .ok_or_else(|| AppError::conflict("login queue has no pending item"))?;
        if item.account_id != input.account_id {
            return Err(AppError::conflict("login queue account differs"));
        }
        if input.network_selection.as_ref().is_none_or(|s| {
            s.scope.operator_id != self.scope.operator_id
                || s.scope.account_id != item.account_id
                || s.scope.tenant_id.is_some()
                || s.scope.project_id.is_some()
        }) {
            return Err(AppError::forbidden(
                "login queue pool network scope differs",
            ));
        }
        item.status = OperatorLoginQueueItemStatus::Current;
        item.operation_id = Some(input.operation_id);
        item.owner_session_id = Some(owner);
        item.error_code = None;
        self.changed(at);
        Ok(())
    }
    pub fn pause_error(
        &mut self,
        expected: i64,
        operation_id: Option<Uuid>,
        code: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.check_version(expected)?;
        validate_login_queue_error(code)?;
        if self.status == OperatorLoginQueueStatus::Completed
            || self.current().and_then(|i| i.operation_id) != operation_id
        {
            return Err(AppError::conflict("login queue failure no longer current"));
        }
        self.status = OperatorLoginQueueStatus::Paused;
        let position = self
            .items
            .iter()
            .position(|i| i.status == OperatorLoginQueueItemStatus::Current)
            .or_else(|| {
                self.items
                    .iter()
                    .position(|i| i.status == OperatorLoginQueueItemStatus::Pending)
            });
        if let Some(position) = position {
            self.items[position].error_code = Some(code.into());
        }
        self.changed(at);
        Ok(())
    }
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait OperatorLoginQueueRepository: Send + Sync {
    async fn create_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
        queue_id: Uuid,
        import_key: &str,
        selections: &[OperatorLoginQueueSelection],
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError>;
    async fn get_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
        queue_id: Uuid,
    ) -> Result<Option<OperatorLoginQueue>, AppError>;
    async fn active_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
    ) -> Result<Option<OperatorLoginQueue>, AppError>;
    async fn apply_login_queue_action(
        &self,
        scope: &OperatorLoginQueueScope,
        queue_id: Uuid,
        expected_version: i64,
        action: OperatorLoginQueueAction,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError>;
    async fn begin_login_queue_item(
        &self,
        scope: &OperatorLoginQueueScope,
        queue_id: Uuid,
        expected_version: i64,
        owner_session_id: Uuid,
        input: &BeginBrowserOperation,
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<(OperatorLoginQueue, BrowserOperation), AppError>;
    async fn pause_login_queue_error(
        &self,
        scope: &OperatorLoginQueueScope,
        queue_id: Uuid,
        expected_version: i64,
        operation_id: Option<Uuid>,
        error_code: &str,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError>;
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
impl OperatorLoginQueueRepository for MemoryChannelJobRepository {
    async fn create_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
        queue_id: Uuid,
        import_key: &str,
        selections: &[OperatorLoginQueueSelection],
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError> {
        let candidate = OperatorLoginQueue::new(scope, queue_id, import_key, selections, at)?;
        let mut queues = self.4.lock().await;
        if let Some(existing) = queues.get(&queue_id) {
            return if existing.matches_creation(&candidate) {
                Ok(existing.clone())
            } else {
                Err(AppError::conflict("login queue creation differs"))
            };
        }
        if queues
            .values()
            .any(|q| q.scope == *scope && q.status != OperatorLoginQueueStatus::Completed)
        {
            return Err(AppError::conflict("login queue already active"));
        }
        let imports = channels
            .list_import_rows(scope.operator_id, import_key)
            .await?;
        for selected in selections {
            if !imports.iter().any(|record| {
                record.row.row_key == selected.row_key
                    && record.row.account_id == Some(selected.account_id)
                    && record.row.status == ChannelImportStatus::NeedsLogin
            }) {
                return Err(AppError::conflict("login queue import mapping differs"));
            }
            let account = channels
                .get_pool_account(scope.operator_id, selected.account_id)
                .await?;
            if !account.account.enabled || account.account.status == ChannelStatus::Disabled {
                return Err(AppError::conflict("login queue account disabled"));
            }
        }
        queues.insert(queue_id, candidate.clone());
        Ok(candidate)
    }
    async fn get_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
    ) -> Result<Option<OperatorLoginQueue>, AppError> {
        Ok(self
            .4
            .lock()
            .await
            .get(&id)
            .filter(|q| q.scope == *scope)
            .cloned())
    }
    async fn active_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
    ) -> Result<Option<OperatorLoginQueue>, AppError> {
        Ok(self
            .4
            .lock()
            .await
            .values()
            .find(|q| q.scope == *scope && q.status != OperatorLoginQueueStatus::Completed)
            .cloned())
    }
    async fn apply_login_queue_action(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
        expected: i64,
        action: OperatorLoginQueueAction,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError> {
        let mut queues = self.4.lock().await;
        let queue = queues
            .get(&id)
            .filter(|q| q.scope == *scope)
            .ok_or_else(|| AppError::not_found("login queue not found"))?;
        let operations = self.2.lock().await;
        let op = queue
            .current()
            .and_then(|i| i.operation_id)
            .and_then(|id| operations.get(&id));
        let mut staged = queue.clone();
        staged.apply(expected, action, op, at)?;
        queues.insert(id, staged.clone());
        Ok(staged)
    }
    async fn begin_login_queue_item(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
        expected: i64,
        owner: Uuid,
        input: &BeginBrowserOperation,
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<(OperatorLoginQueue, BrowserOperation), AppError> {
        let mut queues = self.4.lock().await;
        let mut staged = queues
            .get(&id)
            .filter(|q| q.scope == *scope)
            .cloned()
            .ok_or_else(|| AppError::not_found("login queue not found"))?;
        staged.claim(expected, owner, input, at)?;
        let mut gates = self.1.lock().await;
        let mut operations = self.2.lock().await;
        let networks = self.3.lock().await;
        let account = channels
            .get_pool_account(scope.operator_id, input.account_id)
            .await?;
        if !account.account.enabled || account.account.status == ChannelStatus::Disabled {
            return Err(AppError::conflict("login queue account disabled"));
        }
        for session in channels
            .account_login_sessions(scope.operator_id, input.account_id)
            .await?
        {
            if !operations
                .values()
                .any(|op| op.scope.operator_id == scope.operator_id && op.session_id == session)
            {
                return Err(AppError::conflict(
                    "retained login requires browser ownership reconciliation",
                ));
            }
        }
        let browser_scope = BrowserOperationScope {
            operator_id: scope.operator_id,
            tenant_id: None,
            project_id: None,
            owner_id: owner,
        };
        let op = crate::channel_jobs::prepare_memory_browser(
            &browser_scope,
            input,
            at,
            &gates,
            &operations,
            &networks,
        )?;
        if operations.contains_key(&op.operation_id) {
            return Err(AppError::conflict("login queue operation already used"));
        }
        if let Some(selection) = &input.network_selection {
            channels
                .validate_account_network_assignment(
                    &selection.scope,
                    &AccountNetworkSource::LegacySnapshot,
                )
                .await?;
        }
        channels
            .save_pool_login(
                scope.operator_id,
                PoolLoginSession {
                    session_id: op.session_id,
                    account_id: op.account_id,
                    created_at: at,
                },
            )
            .await?;
        gates.insert(
            (scope.operator_id.as_uuid(), op.account_id),
            (op.reservation_id, op.expires_at),
        );
        operations.insert(op.operation_id, op.clone());
        queues.insert(id, staged.clone());
        Ok((staged, op))
    }
    async fn pause_login_queue_error(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
        expected: i64,
        op: Option<Uuid>,
        code: &str,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError> {
        let mut queues = self.4.lock().await;
        let queue = queues
            .get_mut(&id)
            .filter(|q| q.scope == *scope)
            .ok_or_else(|| AppError::not_found("login queue not found"))?;
        queue.pause_error(expected, op, code, at)?;
        Ok(queue.clone())
    }
}
