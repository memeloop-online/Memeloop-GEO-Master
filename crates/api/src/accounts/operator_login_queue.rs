//! Explicit, persisted operator onboarding. The queue schedules existing login
//! operations; browser ownership always remains the creating HTTP session.
use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, channels};
use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, BrowserOperation, BrowserOperationPurpose, BrowserOperationScope,
    BrowserOperationState, ErrorCode, OperatorLoginQueue, OperatorLoginQueueAction,
    OperatorLoginQueueItemStatus, OperatorLoginQueueScope, OperatorLoginQueueSelection,
    OperatorLoginQueueStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateLoginQueue {
    queue_id: Uuid,
    import_key: String,
    items: Vec<QueueSelection>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSelection {
    row_key: String,
    account_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueVersion {
    expected_version: i64,
}
#[derive(Serialize)]
pub struct LoginQueueItemView {
    position: u32,
    row_key: String,
    account_id: Uuid,
    status: OperatorLoginQueueItemStatus,
    attempt: u32,
    error_code: Option<String>,
}
#[derive(Serialize)]
pub struct LoginQueueCurrentView {
    position: u32,
    account_id: Uuid,
    session_id: Option<Uuid>,
    phase: &'static str,
    requires_session_recovery: bool,
}
#[derive(Serialize)]
pub struct LoginQueueView {
    queue_id: Uuid,
    import_key: String,
    status: OperatorLoginQueueStatus,
    version: i64,
    items: Vec<LoginQueueItemView>,
    current: Option<LoginQueueCurrentView>,
    can_drive: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn queue_scope(state: &AppState, auth: &AuthContext) -> Result<OperatorLoginQueueScope, AppError> {
    channels::trusted_pool_scope(state.channel_service(), auth)?;
    Ok(OperatorLoginQueueScope {
        operator_id: auth.operator.id,
        creator_user_id: auth.user.id.as_uuid(),
    })
}
async fn read_queue(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
) -> Result<OperatorLoginQueue, AppError> {
    let scope = queue_scope(state, auth)?;
    state
        .channel_service()
        .operations()?
        .get_login_queue(&scope, id)
        .await?
        .ok_or_else(|| AppError::not_found("login queue not found"))
}
async fn current_operation(
    state: &AppState,
    queue: &OperatorLoginQueue,
) -> Result<Option<BrowserOperation>, AppError> {
    let Some(item) = queue.current() else {
        return Ok(None);
    };
    let (Some(operation_id), Some(owner_id)) = (item.operation_id, item.owner_session_id) else {
        return Err(AppError::conflict("login queue ownership is unknown"));
    };
    let operation = state
        .channel_service()
        .operations()?
        .get_browser_operation(
            &BrowserOperationScope {
                operator_id: queue.scope.operator_id,
                tenant_id: None,
                project_id: None,
                owner_id,
            },
            operation_id,
        )
        .await?;
    if let Some(operation) = &operation {
        queue.validate_current_operation(Some(operation))?;
        if operation.purpose != BrowserOperationPurpose::Login
            || operation.session_id != operation_id
        {
            return Err(AppError::forbidden("login queue operation differs"));
        }
    }
    Ok(operation)
}
async fn project(
    state: &AppState,
    auth: &AuthContext,
    queue: OperatorLoginQueue,
) -> Result<LoginQueueView, AppError> {
    let operation = current_operation(state, &queue).await?;
    let mut can_drive =
        queue.status == OperatorLoginQueueStatus::Running && queue.current().is_none();
    let current = if let Some(item) = queue.current() {
        let same_owner = item.owner_session_id == Some(auth.session.id.as_uuid());
        let live = operation.as_ref().is_some_and(|op| {
            op.state != BrowserOperationState::Disposed && Utc::now() < op.expires_at
        });
        let reference_present = if let Some(op) = &operation {
            state
                .channel_service()
                .repository
                .get_pool_login(queue.scope.operator_id, op.session_id)
                .await
                .is_ok()
        } else {
            false
        };
        let terminal = operation.as_ref().is_some_and(|op| {
            op.state == BrowserOperationState::Disposed || op.login_verified_at.is_some()
        });
        let unknown_start = operation.as_ref().is_some_and(|op| {
            op.state == BrowserOperationState::StartRequested && op.login_verified_at.is_none()
        }) && matches!(
            item.error_code.as_deref(),
            Some(
                "start_failed"
                    | "runner_unavailable"
                    | "browser_unavailable"
                    | "session_recovery_required"
            )
        );
        let recovery = !same_owner
            || operation.is_none()
            || (!live && !terminal)
            || (!reference_present && !terminal)
            || unknown_start;
        let phase = if recovery {
            "recovery_required"
        } else if operation.as_ref().is_some_and(|op| {
            op.login_verified_at.is_some()
                || matches!(
                    op.state,
                    BrowserOperationState::Closing | BrowserOperationState::Disposed
                )
        }) {
            "closing"
        } else if operation
            .as_ref()
            .is_some_and(|op| op.state == BrowserOperationState::Prepared)
        {
            "preparing"
        } else {
            "awaiting_login"
        };
        can_drive = queue.status == OperatorLoginQueueStatus::Running
            && !recovery
            && operation.as_ref().is_some_and(|op| {
                terminal
                    || matches!(
                        op.state,
                        BrowserOperationState::Prepared | BrowserOperationState::Closing
                    )
            });
        Some(LoginQueueCurrentView {
            position: item.position,
            account_id: item.account_id,
            session_id: if !recovery && same_owner && live && reference_present {
                operation.as_ref().map(|op| op.session_id)
            } else {
                None
            },
            phase,
            requires_session_recovery: recovery,
        })
    } else {
        None
    };
    Ok(LoginQueueView {
        queue_id: queue.queue_id,
        import_key: queue.import_key,
        status: queue.status,
        version: queue.version,
        items: queue
            .items
            .into_iter()
            .map(|item| LoginQueueItemView {
                position: item.position,
                row_key: item.row_key,
                account_id: item.account_id,
                status: item.status,
                attempt: item.attempt,
                error_code: item.error_code,
            })
            .collect(),
        current,
        can_drive,
        created_at: queue.created_at,
        updated_at: queue.updated_at,
    })
}
async fn reconcile(
    state: &AppState,
    queue: &OperatorLoginQueue,
) -> Result<OperatorLoginQueue, AppError> {
    state
        .channel_service()
        .operations()?
        .apply_login_queue_action(
            &queue.scope,
            queue.queue_id,
            queue.version,
            OperatorLoginQueueAction::ReconcileCurrent,
            Utc::now(),
        )
        .await
}
async fn pause_error(
    state: &AppState,
    queue: &OperatorLoginQueue,
    operation: Option<Uuid>,
    code: &str,
) {
    // Exact version + linked operation makes a delayed failure harmless after
    // another request has completed, paused, retried or skipped this item.
    if let Ok(repository) = state.channel_service().operations() {
        let _ = repository
            .pause_login_queue_error(
                &queue.scope,
                queue.queue_id,
                queue.version,
                operation,
                code,
                Utc::now(),
            )
            .await;
    }
}
fn safe_error(error: &AppError, fallback: &'static str) -> &'static str {
    match error.message.as_str() {
        "account is disabled" => "account_disabled",
        "login resolved to a different platform account" => "identity_mismatch",
        _ => fallback,
    }
}
async fn dispose(
    state: &AppState,
    queue: &OperatorLoginQueue,
    operation: &BrowserOperation,
) -> Result<(), AppError> {
    if let Err(error) = state
        .channel_service()
        .dispose_owned_operation(operation)
        .await
    {
        pause_error(
            state,
            queue,
            Some(operation.operation_id),
            "disposal_unconfirmed",
        )
        .await;
        return Err(error);
    }
    state
        .channel_service()
        .repository
        .delete_pool_login(queue.scope.operator_id, operation.session_id)
        .await?;
    state.desktop_grants.revoke(operation.session_id).await;
    Ok(())
}
async fn reconcile_exact_after_effect(
    state: &AppState,
    auth: &AuthContext,
    queue: &OperatorLoginQueue,
    operation_id: Uuid,
) -> Result<OperatorLoginQueue, AppError> {
    let latest = read_queue(state, auth, queue.queue_id).await?;
    if latest.current().and_then(|item| item.operation_id) != Some(operation_id) {
        return Ok(latest);
    }
    reconcile(state, &latest).await
}

/// Bounded to one successor start. GET does not call this service.
async fn drive_queue(
    state: &AppState,
    auth: &AuthContext,
    mut queue: OperatorLoginQueue,
) -> Result<OperatorLoginQueue, AppError> {
    queue = reconcile(state, &queue).await?;
    if queue.status != OperatorLoginQueueStatus::Running {
        return Ok(queue);
    }
    if let Some(operation) = current_operation(state, &queue).await? {
        if operation.scope.owner_id != auth.session.id.as_uuid() {
            return Ok(queue);
        }
        if operation.login_verified_at.is_some()
            || operation.state == BrowserOperationState::Closing
        {
            dispose(state, &queue, &operation).await?;
            queue =
                reconcile_exact_after_effect(state, auth, &queue, operation.operation_id).await?;
            if queue.status != OperatorLoginQueueStatus::Running {
                return Ok(queue);
            }
        }
    }
    if queue.current().is_none() {
        let Some(item) = queue
            .items
            .iter()
            .find(|item| item.status == OperatorLoginQueueItemStatus::Pending)
        else {
            return Ok(queue);
        };
        let input = match state
            .channel_service()
            .prepare_pool_login_input(auth, item.account_id, Uuid::new_v4())
            .await
        {
            Ok(input) => input,
            Err(error) => {
                pause_error(
                    state,
                    &queue,
                    None,
                    safe_error(&error, "network_unavailable"),
                )
                .await;
                return Err(error);
            }
        };
        let claim = state
            .channel_service()
            .operations()?
            .begin_login_queue_item(
                &queue.scope,
                queue.queue_id,
                queue.version,
                auth.session.id.as_uuid(),
                &input,
                state.channel_service().repository.as_ref(),
                Utc::now(),
            )
            .await;
        queue = match claim {
            Ok((claimed, _)) => claimed,
            Err(error) => {
                pause_error(state, &queue, None, "browser_unavailable").await;
                return Err(error);
            }
        };
    }
    let Some(operation) = current_operation(state, &queue).await? else {
        return Ok(queue);
    };
    if operation.scope.owner_id != auth.session.id.as_uuid() {
        return Ok(queue);
    }
    if Utc::now() >= operation.expires_at {
        pause_error(
            state,
            &queue,
            Some(operation.operation_id),
            "session_recovery_required",
        )
        .await;
        return read_queue(state, auth, queue.queue_id).await;
    }
    let result = match operation.state {
        BrowserOperationState::Prepared => {
            state
                .channel_service()
                .start_pool_login_operation(auth, &operation)
                .await
        }
        BrowserOperationState::StartRequested => {
            // A restarted runner cannot prove that the pinned old browser is
            // gone; never send another start after this durable transition.
            match state
                .channel_service()
                .browser()?
                .lifecycle_generation()
                .await
            {
                Ok(generation) if generation == operation.runner_generation => state
                    .channel_service()
                    .browser()?
                    .desktop_status(operation.session_id)
                    .await
                    .map(|_| ()),
                _ => Err(AppError::new(
                    ErrorCode::DependencyUnavailable,
                    "login browser unavailable",
                )),
            }
        }
        BrowserOperationState::Closing | BrowserOperationState::Disposed => return Ok(queue),
    };
    if let Err(error) = result {
        pause_error(
            state,
            &queue,
            Some(operation.operation_id),
            safe_error(&error, "start_failed"),
        )
        .await;
        return Err(error);
    }
    let scope = channels::trusted_pool_scope(state.channel_service(), auth)?;
    state
        .desktop_grants
        .bind(operation.session_id, auth, &scope.tenant_id.to_string())
        .await;
    read_queue(state, auth, queue.queue_id).await
}

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<CreateLoginQueue>,
) -> Result<Json<LoginQueueView>, ApiError> {
    let result = async {
        let scope = queue_scope(&state, &auth)?;
        let items: Vec<_> = input
            .items
            .into_iter()
            .map(|item| OperatorLoginQueueSelection {
                row_key: item.row_key,
                account_id: item.account_id,
            })
            .collect();
        let queue = state
            .channel_service()
            .operations()?
            .create_login_queue(
                &scope,
                input.queue_id,
                &input.import_key,
                &items,
                state.channel_service().repository.as_ref(),
                Utc::now(),
            )
            .await?;
        let queue = drive_queue(&state, &auth, queue).await?;
        project(&state, &auth, queue).await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub async fn active(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<Option<LoginQueueView>>, ApiError> {
    let result = async {
        let scope = queue_scope(&state, &auth)?;
        match state
            .channel_service()
            .operations()?
            .active_login_queue(&scope)
            .await?
        {
            Some(queue) => Ok(Some(project(&state, &auth, queue).await?)),
            None => Ok(None),
        }
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub async fn get_queue(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<LoginQueueView>, ApiError> {
    let result = async { project(&state, &auth, read_queue(&state, &auth, id).await?).await }.await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

#[derive(Clone, Copy)]
enum Command {
    Drive,
    Pause,
    Resume,
    Retry,
    Skip,
    Complete,
}
async fn command(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
    expected: i64,
    command: Command,
) -> Result<LoginQueueView, AppError> {
    let initial = read_queue(state, auth, id).await?;
    initial.check_version(expected)?;
    let intended = initial.current().and_then(|item| item.operation_id);
    let mut queue = reconcile(state, &initial).await?;
    match command {
        Command::Pause | Command::Resume => {
            queue = state
                .channel_service()
                .operations()?
                .apply_login_queue_action(
                    &queue.scope,
                    id,
                    queue.version,
                    if matches!(command, Command::Pause) {
                        OperatorLoginQueueAction::Pause
                    } else {
                        OperatorLoginQueueAction::Resume
                    },
                    Utc::now(),
                )
                .await?;
        }
        Command::Retry | Command::Skip => {
            if queue.current().and_then(|item| item.operation_id) == intended {
                let operation = current_operation(state, &queue)
                    .await?
                    .ok_or_else(|| AppError::conflict("login queue has no current operation"))?;
                // Queue scope authorizes only this exact former owner's
                // disposal; it never grants access to their desktop.
                dispose(state, &queue, &operation).await?;
                let latest = read_queue(state, auth, id).await?;
                if latest.current().and_then(|item| item.operation_id) != intended {
                    return project(state, auth, latest).await;
                }
                queue = state
                    .channel_service()
                    .operations()?
                    .apply_login_queue_action(
                        &latest.scope,
                        id,
                        latest.version,
                        if matches!(command, Command::Retry) {
                            OperatorLoginQueueAction::RetryCurrent
                        } else {
                            OperatorLoginQueueAction::SkipCurrent
                        },
                        Utc::now(),
                    )
                    .await?;
            }
        }
        Command::Complete => {
            if queue.current().and_then(|item| item.operation_id) == intended {
                let operation = current_operation(state, &queue)
                    .await?
                    .ok_or_else(|| AppError::conflict("login queue has no current operation"))?;
                if operation.scope.owner_id != auth.session.id.as_uuid() {
                    return Err(AppError::forbidden("login session recovery required"));
                }
                if operation.login_verified_at.is_none() {
                    if operation.state != BrowserOperationState::StartRequested {
                        return Err(AppError::conflict(
                            "login browser is not awaiting completion",
                        ));
                    }
                    if let Err(error) =
                        channels::verify_pool_login(state, auth, operation.session_id).await
                    {
                        let persisted = state
                            .channel_service()
                            .operations()?
                            .get_browser_operation(&operation.scope, operation.operation_id)
                            .await?;
                        if persisted.is_none_or(|op| op.login_verified_at.is_none()) {
                            pause_error(
                                state,
                                &queue,
                                Some(operation.operation_id),
                                safe_error(&error, "login_failed"),
                            )
                            .await;
                            return Err(error);
                        }
                    }
                }
                dispose(state, &queue, &operation).await?;
                queue = reconcile_exact_after_effect(state, auth, &queue, operation.operation_id)
                    .await?;
            }
        }
        Command::Drive => {}
    }
    if matches!(
        command,
        Command::Drive | Command::Resume | Command::Skip | Command::Complete
    ) && queue.status == OperatorLoginQueueStatus::Running
    {
        queue = drive_queue(state, auth, queue).await?;
    }
    project(state, auth, queue).await
}
macro_rules! action_handler {
    ($name:ident, $command:ident) => {
        pub async fn $name(
            State(state): State<AppState>,
            Path(id): Path<Uuid>,
            Extension(auth): Extension<AuthContext>,
            Extension(context): Extension<RequestContext>,
            Json(input): Json<QueueVersion>,
        ) -> Result<Json<LoginQueueView>, ApiError> {
            command(&state, &auth, id, input.expected_version, Command::$command)
                .await
                .map(Json)
                .map_err(|e| api_error(e, context.request_id))
        }
    };
}
action_handler!(drive, Drive);
action_handler!(pause, Pause);
action_handler!(resume, Resume);
action_handler!(retry, Retry);
action_handler!(skip, Skip);
action_handler!(complete, Complete);
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operator/channel-login-queues", post(create))
        .route("/operator/channel-login-queues/active", get(active))
        .route("/operator/channel-login-queues/{id}", get(get_queue))
        .route("/operator/channel-login-queues/{id}/drive", post(drive))
        .route("/operator/channel-login-queues/{id}/pause", post(pause))
        .route("/operator/channel-login-queues/{id}/resume", post(resume))
        .route("/operator/channel-login-queues/{id}/retry", post(retry))
        .route("/operator/channel-login-queues/{id}/skip", post(skip))
        .route(
            "/operator/channel-login-queues/{id}/complete",
            post(complete),
        )
}

#[cfg(test)]
#[path = "operator_login_queue_tests.rs"]
mod tests;
