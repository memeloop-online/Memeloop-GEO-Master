use crate::PgChannelJobRepository;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::*;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

fn db(error: sqlx::Error) -> AppError {
    if error
        .as_database_error()
        .is_some_and(|e| e.code().as_deref() == Some("23505"))
    {
        AppError::conflict("login queue ownership conflict")
    } else {
        AppError::new(
            ErrorCode::DependencyUnavailable,
            "login queue storage unavailable",
        )
    }
}
fn status<T: serde::de::DeserializeOwned>(value: String) -> Result<T, AppError> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|_| AppError::new(ErrorCode::Internal, "login queue data invalid"))
}
fn encode<T: serde::Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| AppError::new(ErrorCode::Internal, "login queue encoding failed"))
}
async fn record(
    tx: &mut Transaction<'_, Postgres>,
    row: PgRow,
) -> Result<OperatorLoginQueue, AppError> {
    let queue_id = row.try_get("queue_id").map_err(db)?;
    let rows =
        sqlx::query("SELECT * FROM operator_login_queue_items WHERE queue_id=$1 ORDER BY position")
            .bind(queue_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(db)?;
    let mut items = Vec::with_capacity(rows.len());
    for item in rows {
        items.push(OperatorLoginQueueItem {
            position: item.try_get::<i32, _>("position").map_err(db)? as u32,
            row_key: item.try_get("row_key").map_err(db)?,
            account_id: item.try_get("account_id").map_err(db)?,
            status: status(item.try_get("status").map_err(db)?)?,
            attempt: item.try_get::<i32, _>("attempt").map_err(db)? as u32,
            operation_id: item.try_get("operation_id").map_err(db)?,
            owner_session_id: item.try_get("owner_session_id").map_err(db)?,
            error_code: item.try_get("error_code").map_err(db)?,
        });
    }
    Ok(OperatorLoginQueue {
        queue_id,
        scope: OperatorLoginQueueScope {
            operator_id: OperatorId::new(row.try_get("operator_id").map_err(db)?),
            creator_user_id: row.try_get("creator_user_id").map_err(db)?,
        },
        import_key: row.try_get("import_key").map_err(db)?,
        status: status(row.try_get("status").map_err(db)?)?,
        version: row.try_get("version").map_err(db)?,
        items,
        created_at: row.try_get("created_at").map_err(db)?,
        updated_at: row.try_get("updated_at").map_err(db)?,
    })
}
async fn scoped(
    tx: &mut Transaction<'_, Postgres>,
    scope: &OperatorLoginQueueScope,
    id: Uuid,
    lock: bool,
) -> Result<Option<OperatorLoginQueue>, AppError> {
    let sql = if lock {
        "SELECT * FROM operator_login_queues WHERE queue_id=$1 AND operator_id=$2 AND creator_user_id=$3 FOR UPDATE"
    } else {
        "SELECT * FROM operator_login_queues WHERE queue_id=$1 AND operator_id=$2 AND creator_user_id=$3 FOR SHARE"
    };
    match sqlx::query(sql)
        .bind(id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.creator_user_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
    {
        Some(row) => Ok(Some(record(tx, row).await?)),
        None => Ok(None),
    }
}
async fn operation(
    tx: &mut Transaction<'_, Postgres>,
    queue: &OperatorLoginQueue,
) -> Result<Option<BrowserOperation>, AppError> {
    if let Some(id) = queue.current().and_then(|i| i.operation_id) {
        let row = sqlx::query("SELECT * FROM browser_operations WHERE operation_id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::conflict("login queue operation missing"))?;
        Ok(Some(super::browser_operations::record(row)?))
    } else {
        Ok(None)
    }
}
async fn persist(
    tx: &mut Transaction<'_, Postgres>,
    queue: &OperatorLoginQueue,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE operator_login_queues SET status=$2,version=$3,updated_at=$4 WHERE queue_id=$1",
    )
    .bind(queue.queue_id)
    .bind(encode(&queue.status)?)
    .bind(queue.version)
    .bind(queue.updated_at)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    for item in &queue.items {
        sqlx::query("UPDATE operator_login_queue_items SET status=$3,attempt=$4,operation_id=$5,owner_session_id=$6,error_code=$7 WHERE queue_id=$1 AND position=$2").bind(queue.queue_id).bind(item.position as i32).bind(encode(&item.status)?).bind(i32::try_from(item.attempt).map_err(|_|AppError::conflict("login queue attempts exhausted"))?).bind(item.operation_id).bind(item.owner_session_id).bind(&item.error_code).execute(&mut **tx).await.map_err(db)?;
    }
    Ok(())
}
async fn enabled_account(
    tx: &mut Transaction<'_, Postgres>,
    operator: OperatorId,
    id: Uuid,
) -> Result<(), AppError> {
    let metadata:serde_json::Value=sqlx::query_scalar("SELECT metadata FROM operator_channel_accounts WHERE operator_id=$1 AND account_id=$2 FOR SHARE").bind(operator.as_uuid()).bind(id).fetch_optional(&mut **tx).await.map_err(db)?.ok_or_else(||AppError::not_found("pool account not found"))?;
    let account: PoolAccount = serde_json::from_value(metadata)
        .map_err(|_| AppError::new(ErrorCode::Internal, "pool account data invalid"))?;
    if !account.enabled || account.status == ChannelStatus::Disabled {
        return Err(AppError::conflict("login queue account disabled"));
    }
    Ok(())
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
impl OperatorLoginQueueRepository for PgChannelJobRepository {
    async fn create_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
        key: &str,
        selections: &[OperatorLoginQueueSelection],
        _channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError> {
        let candidate = OperatorLoginQueue::new(scope, id, key, selections, at)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("login-queue:{id}"))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        if let Some(row) =
            sqlx::query("SELECT * FROM operator_login_queues WHERE queue_id=$1 FOR SHARE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
        {
            let existing = record(&mut tx, row).await?;
            return if existing.matches_creation(&candidate) {
                Ok(existing)
            } else {
                Err(AppError::conflict("login queue creation differs"))
            };
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "login-queue-creator:{}:{}",
                scope.operator_id.as_uuid(),
                scope.creator_user_id
            ))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operator_login_queues WHERE operator_id=$1 AND creator_user_id=$2 AND status<>'completed')").bind(scope.operator_id.as_uuid()).bind(scope.creator_user_id).fetch_one(&mut *tx).await.map_err(db)?;
        if active {
            return Err(AppError::conflict("login queue already active"));
        }
        for selected in selections {
            let outcome:serde_json::Value=sqlx::query_scalar("SELECT outcome FROM operator_channel_import_rows WHERE operator_id=$1 AND import_key=$2 AND row_key=$3 FOR SHARE").bind(scope.operator_id.as_uuid()).bind(key).bind(&selected.row_key).fetch_optional(&mut *tx).await.map_err(db)?.ok_or_else(||AppError::conflict("login queue import mapping missing"))?;
            let row: ChannelImportRow = serde_json::from_value(outcome).map_err(|_| {
                AppError::new(ErrorCode::Internal, "login queue import data invalid")
            })?;
            if row.row_key != selected.row_key
                || row.account_id != Some(selected.account_id)
                || row.status != ChannelImportStatus::NeedsLogin
            {
                return Err(AppError::conflict("login queue import mapping differs"));
            }
            enabled_account(&mut tx, scope.operator_id, selected.account_id).await?;
        }
        sqlx::query("INSERT INTO operator_login_queues(queue_id,operator_id,creator_user_id,import_key,status,version,created_at,updated_at) VALUES($1,$2,$3,$4,'running',1,$5,$5)").bind(id).bind(scope.operator_id.as_uuid()).bind(scope.creator_user_id).bind(key).bind(at).execute(&mut *tx).await.map_err(db)?;
        for item in &candidate.items {
            sqlx::query("INSERT INTO operator_login_queue_items(queue_id,position,row_key,account_id,status,attempt) VALUES($1,$2,$3,$4,'pending',0)").bind(id).bind(item.position as i32).bind(&item.row_key).bind(item.account_id).execute(&mut *tx).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(candidate)
    }
    async fn get_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
    ) -> Result<Option<OperatorLoginQueue>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let q = scoped(&mut tx, scope, id, false).await?;
        tx.commit().await.map_err(db)?;
        Ok(q)
    }
    async fn active_login_queue(
        &self,
        scope: &OperatorLoginQueueScope,
    ) -> Result<Option<OperatorLoginQueue>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row=sqlx::query("SELECT * FROM operator_login_queues WHERE operator_id=$1 AND creator_user_id=$2 AND status<>'completed' FOR SHARE").bind(scope.operator_id.as_uuid()).bind(scope.creator_user_id).fetch_optional(&mut *tx).await.map_err(db)?;
        let queue = match row {
            Some(row) => Some(record(&mut tx, row).await?),
            None => None,
        };
        tx.commit().await.map_err(db)?;
        Ok(queue)
    }
    async fn apply_login_queue_action(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
        expected: i64,
        action: OperatorLoginQueueAction,
        at: DateTime<Utc>,
    ) -> Result<OperatorLoginQueue, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let mut queue = scoped(&mut tx, scope, id, true)
            .await?
            .ok_or_else(|| AppError::not_found("login queue not found"))?;
        queue.check_version(expected)?;
        let op = operation(&mut tx, &queue).await?;
        queue.apply(expected, action, op.as_ref(), at)?;
        persist(&mut tx, &queue).await?;
        tx.commit().await.map_err(db)?;
        Ok(queue)
    }
    async fn begin_login_queue_item(
        &self,
        scope: &OperatorLoginQueueScope,
        id: Uuid,
        expected: i64,
        owner: Uuid,
        input: &BeginBrowserOperation,
        _channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<(OperatorLoginQueue, BrowserOperation), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let mut queue = scoped(&mut tx, scope, id, true)
            .await?
            .ok_or_else(|| AppError::not_found("login queue not found"))?;
        queue.claim(expected, owner, input, at)?;
        super::account_networks::lock_head(&mut tx, scope.operator_id, input.account_id).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(input.operation_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM browser_operations WHERE operation_id=$1)",
        )
        .bind(input.operation_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if exists {
            return Err(AppError::conflict("login queue operation already used"));
        }
        let browser_scope = BrowserOperationScope {
            operator_id: scope.operator_id,
            tenant_id: None,
            project_id: None,
            owner_id: owner,
        };
        let op = super::browser_operations::begin_browser_operation_tx(
            &mut tx,
            &browser_scope,
            input,
            at,
        )
        .await?;
        // Browser preparation acquires the account gate before account locks.
        // Verification follows gate -> account as well; an earlier FOR SHARE
        // here would deadlock a verifier while this claim waits on its gate.
        // A disabled account still rolls back every prepared operation/gate.
        enabled_account(&mut tx, scope.operator_id, input.account_id).await?;
        let saved=sqlx::query("INSERT INTO operator_channel_login_sessions(operator_id,session_id,account_id,created_at) VALUES($1,$2,$3,$4) ON CONFLICT(operator_id,session_id) DO NOTHING").bind(scope.operator_id.as_uuid()).bind(op.session_id).bind(op.account_id).bind(at).execute(&mut *tx).await.map_err(db)?;
        if saved.rows_affected() != 1 {
            return Err(AppError::conflict("pool login reference already used"));
        }
        persist(&mut tx, &queue).await?;
        tx.commit().await.map_err(db)?;
        Ok((queue, op))
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
        let mut tx = self.pool.begin().await.map_err(db)?;
        let mut queue = scoped(&mut tx, scope, id, true)
            .await?
            .ok_or_else(|| AppError::not_found("login queue not found"))?;
        queue.pause_error(expected, op, code, at)?;
        persist(&mut tx, &queue).await?;
        tx.commit().await.map_err(db)?;
        Ok(queue)
    }
}
