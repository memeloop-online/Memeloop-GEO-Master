use crate::PgChannelJobRepository;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::*;
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;

fn db(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "browser ownership ledger unavailable",
    )
}
fn json<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(v)
        .map_err(|_| AppError::new(ErrorCode::Internal, "browser ownership encoding failed"))
}
fn decode<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(v)
        .map_err(|_| AppError::new(ErrorCode::Internal, "browser ownership data invalid"))
}
pub(crate) fn record(row: PgRow) -> Result<BrowserOperation, AppError> {
    Ok(BrowserOperation {
        login_verified_at: row.try_get("login_verified_at").map_err(db)?,
        network_binding_id: row.try_get("network_binding_id").map_err(db)?,
        network_scope: row
            .try_get::<Option<serde_json::Value>, _>("network_scope")
            .map_err(db)?
            .map(decode)
            .transpose()?,
        operation_id: row.try_get("operation_id").map_err(db)?,
        session_id: row.try_get("session_id").map_err(db)?,
        account_id: row.try_get("account_id").map_err(db)?,
        scope: decode(row.try_get("scope").map_err(db)?)?,
        purpose: decode(row.try_get("purpose").map_err(db)?)?,
        runner_generation: row.try_get("runner_generation").map_err(db)?,
        effective_network: ChannelSecret::new(row.try_get("effective_network").map_err(db)?),
        reservation_id: row.try_get("reservation_id").map_err(db)?,
        state: decode(row.try_get("state").map_err(db)?)?,
        version: row.try_get("version").map_err(db)?,
        expires_at: row.try_get("expires_at").map_err(db)?,
        created_at: row.try_get("created_at").map_err(db)?,
        updated_at: row.try_get("updated_at").map_err(db)?,
        next_check_at: row.try_get("next_check_at").map_err(db)?,
        released_unstarted: row.try_get("released_unstarted").map_err(db)?,
    })
}

pub(crate) async fn begin_browser_operation_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &BrowserOperationScope,
    input: &BeginBrowserOperation,
    at: DateTime<Utc>,
) -> Result<BrowserOperation, AppError> {
    let mut op = BrowserOperation::prepare(scope, input, at)?;
    super::account_networks::lock_head(tx, scope.operator_id, input.account_id).await?;
    // Serialize idempotent replays independently of the shared account gate.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(op.operation_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    if let Some(row) = sqlx::query("SELECT * FROM browser_operations WHERE operation_id=$1")
        .bind(op.operation_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
    {
        let existing = record(row)?;
        op.replay_network(input.network_selection.as_ref(), &existing)?;
        return if existing.matches_begin(&op) {
            Ok(existing)
        } else {
            Err(AppError::conflict("browser operation differs"))
        };
    }
    if input.expires_at <= at {
        return Err(AppError::invalid_request(
            "browser ownership deadline elapsed",
        ));
    }
    let acquired = if let Some(reservation_id) = input.reservation_id {
        sqlx::query(
            "UPDATE channel_account_preflight_reservations SET expires_at=$4 \
                WHERE operator_id=$1 AND account_id=$2 AND reservation_id=$3 \
                AND expires_at>$5 AND browser_operation_id IS NULL",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(input.account_id)
        .bind(reservation_id)
        .bind(input.expires_at)
        .bind(at)
        .execute(&mut **tx)
        .await
        .map_err(db)?
        .rows_affected()
    } else {
        sqlx::query("INSERT INTO channel_account_preflight_reservations (operator_id,account_id,reservation_id,expires_at) \
                VALUES ($1,$2,$3,$4) ON CONFLICT (operator_id,account_id) DO UPDATE \
                SET reservation_id=EXCLUDED.reservation_id,expires_at=EXCLUDED.expires_at \
                WHERE channel_account_preflight_reservations.expires_at<=$5 \
                AND channel_account_preflight_reservations.browser_operation_id IS NULL")
                .bind(scope.operator_id.as_uuid()).bind(input.account_id).bind(op.reservation_id)
                .bind(input.expires_at).bind(at).execute(&mut **tx).await.map_err(db)?.rows_affected()
    };
    if acquired != 1 {
        return Err(AppError::conflict("channel account browser busy"));
    }
    // Retained pre-migration browser references have no exact runner proof.
    // Never infer their generation or permit expiry-based takeover.
    let legacy: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ( \
            SELECT operator_id,account_id,session_id FROM channel_login_sessions UNION ALL \
            SELECT operator_id,account_id,session_id FROM operator_channel_login_sessions) login \
            WHERE login.operator_id=$1 AND login.account_id=$2 AND NOT EXISTS( \
            SELECT 1 FROM browser_operations op WHERE op.operator_id=login.operator_id AND op.session_id=login.session_id))")
            .bind(scope.operator_id.as_uuid()).bind(op.account_id).fetch_one(&mut **tx).await.map_err(db)?;
    if legacy {
        return Err(AppError::conflict(
            "retained login requires browser ownership reconciliation",
        ));
    }
    if let Some(selection) = &input.network_selection {
        let head = super::account_networks::state_tx(tx, &selection.scope)
            .await?
            .ok_or_else(|| AppError::conflict("account network is not initialized"))?;
        let binding = match selection.historical_binding_id {
            Some(id) => super::account_networks::binding_tx(tx, &selection.scope, id).await?,
            None => head.active,
        };
        // Account ownership remains required for historical routes, but
        // inventory disable/rotation cannot rewrite their immutable bytes.
        super::account_networks::validate_assignment(
            tx,
            &selection.scope,
            &AccountNetworkSource::LegacySnapshot,
        )
        .await?;
        op.pin_network(selection, &binding)?;
    }
    let inserted = sqlx::query("INSERT INTO browser_operations \
            (operation_id,operator_id,account_id,session_id,reservation_id,scope,purpose,runner_generation,effective_network,state,version,expires_at,created_at,updated_at,next_check_at,network_binding_id,network_scope) \
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$13,$12,$14,$15) ON CONFLICT DO NOTHING")
            .bind(op.operation_id).bind(scope.operator_id.as_uuid()).bind(op.account_id).bind(op.session_id)
            .bind(op.reservation_id).bind(json(scope)?).bind(json(&op.purpose)?).bind(op.runner_generation)
            .bind(op.effective_network.encrypted_bytes()).bind(json(&op.state)?).bind(op.version)
            .bind(op.expires_at).bind(at).bind(op.network_binding_id).bind(op.network_scope.as_ref().map(json).transpose()?).execute(&mut **tx).await.map_err(db)?;
    if inserted.rows_affected() != 1 {
        return Err(AppError::conflict("browser session already owned"));
    }
    sqlx::query(
        "UPDATE channel_account_preflight_reservations SET browser_operation_id=$4 \
            WHERE operator_id=$1 AND account_id=$2 AND reservation_id=$3",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(op.account_id)
    .bind(op.reservation_id)
    .bind(op.operation_id)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(op)
}
#[async_trait]
impl BrowserOperationRepository for PgChannelJobRepository {
    async fn claim_browser_start(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<Option<BrowserOperation>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let account: Uuid = sqlx::query_scalar(
            "SELECT account_id FROM browser_operations WHERE operation_id=$1 AND operator_id=$2",
        )
        .bind(id)
        .bind(scope.operator_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("browser operation not found"))?;
        super::account_networks::lock_head(&mut tx, scope.operator_id, account).await?;
        let row = sqlx::query(
            "SELECT * FROM browser_operations WHERE operation_id=$1 AND operator_id=$2 FOR UPDATE",
        )
        .bind(id)
        .bind(scope.operator_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let mut op = record(row)?;
        if &op.scope != scope {
            return Err(AppError::forbidden("browser ownership scope mismatch"));
        }
        if op.state == BrowserOperationState::StartRequested {
            return Ok(None);
        }
        if op.state != BrowserOperationState::Prepared {
            return Err(AppError::conflict("browser cannot start"));
        }
        op.transition(scope, BrowserOperationTransition::Start, at)?;
        sqlx::query(
            "UPDATE browser_operations SET state=$2,version=$3,updated_at=$4 WHERE operation_id=$1",
        )
        .bind(id)
        .bind(json(&op.state)?)
        .bind(op.version)
        .bind(op.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(Some(op))
    }
    async fn get_account_browser_operation(
        &self,
        operator: OperatorId,
        account_id: Uuid,
    ) -> Result<Option<BrowserOperation>, AppError> {
        sqlx::query("SELECT op.* FROM browser_operations op JOIN channel_account_preflight_reservations gate ON gate.browser_operation_id=op.operation_id \
            WHERE op.operator_id=$1 AND op.account_id=$2 AND op.state<>'\"Disposed\"'::jsonb")
            .bind(operator.as_uuid()).bind(account_id).fetch_optional(&self.pool).await.map_err(db)?.map(record).transpose()
    }
    async fn save_owned_login_session(
        &self,
        scope: &BrowserOperationScope,
        operation_id: Uuid,
        storage: &VerifiedLoginStorage,
        _channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT * FROM browser_operations WHERE operation_id=$1 AND operator_id=$2 FOR UPDATE",
        )
        .bind(operation_id)
        .bind(scope.operator_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("browser operation not found"))?;
        let op = record(row)?;
        op.validate_login_commit(scope, storage)?;
        let gate: Option<Uuid> = sqlx::query_scalar("SELECT browser_operation_id FROM channel_account_preflight_reservations \
            WHERE operator_id=$1 AND account_id=$2 AND reservation_id=$3 AND browser_operation_id=$4 FOR UPDATE")
            .bind(scope.operator_id.as_uuid()).bind(storage.account_id).bind(op.reservation_id).bind(operation_id)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        if gate != Some(operation_id) {
            return Err(AppError::conflict("login account gate differs"));
        }
        super::channels::save_verified_login_storage_tx(&mut tx, storage, at).await?;
        sqlx::query("UPDATE browser_operations SET login_verified_at=COALESCE(login_verified_at,$2) WHERE operation_id=$1")
            .bind(operation_id).bind(at).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }
    async fn browser_operation_exists(
        &self,
        operator: OperatorId,
        session_id: Uuid,
    ) -> Result<bool, AppError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM browser_operations WHERE operator_id=$1 AND session_id=$2)")
            .bind(operator.as_uuid()).bind(session_id).fetch_one(&self.pool).await.map_err(db)
    }
    async fn get_active_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        account_id: Uuid,
    ) -> Result<Option<BrowserOperation>, AppError> {
        sqlx::query("SELECT op.* FROM browser_operations op JOIN channel_account_preflight_reservations gate ON gate.browser_operation_id=op.operation_id \
            WHERE op.operator_id=$1 AND op.account_id=$2 AND op.scope=$3 AND op.state<>'\"Disposed\"'::jsonb")
            .bind(scope.operator_id.as_uuid()).bind(account_id).bind(json(scope)?)
            .fetch_optional(&self.pool).await.map_err(db)?.map(record).transpose()
    }
    async fn begin_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        input: &BeginBrowserOperation,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let op = begin_browser_operation_tx(&mut tx, scope, input, at).await?;
        tx.commit().await.map_err(db)?;
        Ok(op)
    }
    async fn get_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
    ) -> Result<Option<BrowserOperation>, AppError> {
        sqlx::query("SELECT * FROM browser_operations WHERE operation_id=$1 AND operator_id=$2 AND scope=$3")
            .bind(id).bind(scope.operator_id.as_uuid()).bind(json(scope)?)
            .fetch_optional(&self.pool).await.map_err(db)?.map(record).transpose()
    }
    async fn list_due_browser_operations(
        &self,
        at: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<BrowserOperation>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid browser scan limit"));
        }
        sqlx::query(
            "SELECT * FROM browser_operations WHERE state<>'\"Disposed\"'::jsonb \
            AND next_check_at<=$1 ORDER BY next_check_at,operation_id LIMIT $2",
        )
        .bind(at)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?
        .into_iter()
        .map(record)
        .collect()
    }
    async fn transition_browser_operation(
        &self,
        scope: &BrowserOperationScope,
        id: Uuid,
        action: BrowserOperationTransition,
        at: DateTime<Utc>,
    ) -> Result<BrowserOperation, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let account: Uuid = sqlx::query_scalar(
            "SELECT account_id FROM browser_operations WHERE operation_id=$1 AND operator_id=$2",
        )
        .bind(id)
        .bind(scope.operator_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("browser operation not found"))?;
        super::account_networks::lock_head(&mut tx, scope.operator_id, account).await?;
        let row = sqlx::query(
            "SELECT * FROM browser_operations WHERE operation_id=$1 AND operator_id=$2 FOR UPDATE",
        )
        .bind(id)
        .bind(scope.operator_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("browser operation not found"))?;
        let mut op = record(row)?;
        op.transition(scope, action, at)?;
        sqlx::query("UPDATE browser_operations SET state=$2,version=$3,updated_at=$4,next_check_at=$5,released_unstarted=$6 WHERE operation_id=$1")
            .bind(id).bind(json(&op.state)?).bind(op.version).bind(op.updated_at).bind(op.next_check_at).bind(op.released_unstarted).execute(&mut *tx).await.map_err(db)?;
        if op.state == BrowserOperationState::Disposed {
            let released=sqlx::query("DELETE FROM channel_account_preflight_reservations \
                WHERE operator_id=$1 AND account_id=$2 AND reservation_id=$3 AND browser_operation_id=$4")
                .bind(scope.operator_id.as_uuid()).bind(op.account_id).bind(op.reservation_id).bind(op.operation_id)
                .execute(&mut *tx).await.map_err(db)?;
            if released.rows_affected() == 1 {
                super::account_networks::promote_pending(&mut tx, scope.operator_id, op.account_id)
                    .await?;
            }
        }
        tx.commit().await.map_err(db)?;
        Ok(op)
    }
}
