use crate::PgChannelJobRepository;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::*;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

fn db(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "account network ledger unavailable",
    )
}
fn encode<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(v)
        .map_err(|_| AppError::new(ErrorCode::Internal, "account network encoding failed"))
}
fn decode<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(v)
        .map_err(|_| AppError::new(ErrorCode::Internal, "account network data invalid"))
}

/// Every binding change, new browser acquisition and disposal uses this lock
/// first. The immutable head scope cannot be reassigned to another account owner.
pub(crate) async fn lock_head(
    tx: &mut Transaction<'_, Postgres>,
    operator: OperatorId,
    account: Uuid,
) -> Result<(), AppError> {
    // Also covers the absent-head case, without creating state during reads.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("account-network:{}:{account}", operator.as_uuid()))
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    sqlx::query("SELECT account_id FROM account_network_heads WHERE operator_id=$1 AND account_id=$2 FOR UPDATE")
        .bind(operator.as_uuid()).bind(account).fetch_optional(&mut **tx).await.map_err(db)?;
    Ok(())
}

pub(crate) async fn lock_project(
    tx: &mut Transaction<'_, Postgres>,
    scope: &AccountNetworkScope,
) -> Result<(), AppError> {
    if let (Some(t), Some(p)) = (scope.tenant_id, scope.project_id) {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("project-network:{}:{t}:{p}", scope.operator_id))
            .execute(&mut **tx)
            .await
            .map_err(db)?;
    }
    Ok(())
}

pub(crate) async fn validate_assignment(
    tx: &mut Transaction<'_, Postgres>,
    scope: &AccountNetworkScope,
    source: &AccountNetworkSource,
) -> Result<(), AppError> {
    scope.validate()?;
    let exists = match (scope.tenant_id,scope.project_id) {
        (Some(t),Some(p)) => sqlx::query("SELECT account_id FROM channel_accounts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4 AND metadata->>'owner_kind'='customer' FOR KEY SHARE")
            .bind(scope.operator_id.as_uuid()).bind(t.as_uuid()).bind(p.as_uuid()).bind(scope.account_id).fetch_optional(&mut **tx).await.map_err(db)?.is_some(),
        (None,None) => sqlx::query("SELECT account_id FROM operator_channel_accounts WHERE operator_id=$1 AND account_id=$2 FOR KEY SHARE")
            .bind(scope.operator_id.as_uuid()).bind(scope.account_id).fetch_optional(&mut **tx).await.map_err(db)?.is_some(),
        _ => false,
    };
    if !exists {
        return Err(AppError::not_found("network account not found"));
    }
    if let AccountNetworkSource::ProjectDefault {
        settings_updated_at,
    } = source
    {
        let (Some(t), Some(p)) = (scope.tenant_id, scope.project_id) else {
            return Err(AppError::forbidden("project default scope required"));
        };
        let metadata:Option<serde_json::Value>=sqlx::query_scalar("SELECT metadata FROM channel_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3")
            .bind(scope.operator_id.as_uuid()).bind(t.as_uuid()).bind(p.as_uuid()).fetch_optional(&mut **tx).await.map_err(db)?;
        let actual = metadata
            .map(decode::<ChannelSettings>)
            .transpose()?
            .map(|s| s.updated_at);
        if actual != *settings_updated_at {
            return Err(AppError::conflict("project default changed"));
        }
    }
    if let AccountNetworkSource::PoolRevision {
        pool_id,
        member_id,
        revision_id,
    } = source
    {
        // Match inventory edits' pool-then-member lock order.
        let pool=sqlx::query("SELECT pool_id FROM operator_proxy_pools WHERE operator_id=$1 AND pool_id=$2 AND payload->>'enabled'='true' FOR SHARE")
            .bind(scope.operator_id.as_uuid()).bind(pool_id).fetch_optional(&mut **tx).await.map_err(db)?;
        if pool.is_none() {
            return Err(AppError::conflict("proxy pool is unavailable"));
        }
        let row = sqlx::query("SELECT m.member_id FROM operator_proxy_members m JOIN operator_proxy_revisions r \
            ON r.operator_id=m.operator_id AND r.pool_id=m.pool_id AND r.member_id=m.member_id AND r.revision_id=$4 \
            WHERE m.operator_id=$1 AND m.pool_id=$2 AND m.member_id=$3 AND m.revision_id=$4 \
            AND m.payload->>'enabled'='true' FOR SHARE OF m")
            .bind(scope.operator_id.as_uuid()).bind(pool_id).bind(member_id).bind(revision_id).fetch_optional(&mut **tx).await.map_err(db)?;
        if row.is_none() {
            return Err(AppError::conflict("proxy member revision is unavailable"));
        }
    }
    Ok(())
}

pub(crate) async fn binding_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: &AccountNetworkScope,
    id: Uuid,
) -> Result<AccountNetworkBinding, AppError> {
    let row = sqlx::query("SELECT b.* FROM account_network_bindings b JOIN account_network_heads h USING(operator_id,account_id) \
        WHERE b.operator_id=$1 AND b.account_id=$2 AND b.binding_id=$3 AND h.scope=$4")
        .bind(scope.operator_id.as_uuid()).bind(scope.account_id).bind(id).bind(encode(scope)?).fetch_optional(&mut **tx).await.map_err(db)?
        .ok_or_else(||AppError::not_found("network binding not found"))?;
    Ok(AccountNetworkBinding {
        binding_id: id,
        scope: scope.clone(),
        source: decode(row.try_get("source").map_err(db)?)?,
        effective_network: ChannelSecret::new(row.try_get("effective_network").map_err(db)?),
        created_at: row.try_get("created_at").map_err(db)?,
    })
}
pub(crate) async fn state_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: &AccountNetworkScope,
) -> Result<Option<AccountNetworkBindings>, AppError> {
    let row = sqlx::query("SELECT version,scope,active_binding_id,pending_binding_id FROM account_network_heads WHERE operator_id=$1 AND account_id=$2")
        .bind(scope.operator_id.as_uuid()).bind(scope.account_id).fetch_optional(&mut **tx).await.map_err(db)?;
    let Some(row) = row else {
        return Ok(None);
    };
    if decode::<AccountNetworkScope>(row.try_get("scope").map_err(db)?)? != *scope {
        return Err(AppError::forbidden("network account scope differs"));
    }
    let active: Option<Uuid> = row.try_get("active_binding_id").map_err(db)?;
    let Some(active) = active else {
        return Ok(None);
    };
    let pending: Option<Uuid> = row.try_get("pending_binding_id").map_err(db)?;
    let busy:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM channel_account_preflight_reservations WHERE operator_id=$1 AND account_id=$2 AND browser_operation_id IS NOT NULL)")
        .bind(scope.operator_id.as_uuid()).bind(scope.account_id).fetch_one(&mut **tx).await.map_err(db)?;
    Ok(Some(AccountNetworkBindings {
        version: row.try_get("version").map_err(db)?,
        active: binding_tx(tx, scope, active).await?,
        pending: match pending {
            Some(id) => Some(binding_tx(tx, scope, id).await?),
            None => None,
        },
        busy,
    }))
}
async fn insert_binding(
    tx: &mut Transaction<'_, Postgres>,
    binding: &AccountNetworkBinding,
) -> Result<(), AppError> {
    let (pool, member, revision) = match binding.source {
        AccountNetworkSource::PoolRevision {
            pool_id,
            member_id,
            revision_id,
        } => (Some(pool_id), Some(member_id), Some(revision_id)),
        _ => (None, None, None),
    };
    let result = sqlx::query("INSERT INTO account_network_bindings(operator_id,account_id,binding_id,source,effective_network,created_at,pool_id,member_id,revision_id) \
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING")
        .bind(binding.scope.operator_id.as_uuid()).bind(binding.scope.account_id).bind(binding.binding_id).bind(encode(&binding.source)?)
        .bind(binding.effective_network.encrypted_bytes()).bind(binding.created_at).bind(pool).bind(member).bind(revision)
        .execute(&mut **tx).await.map_err(db)?;
    if result.rows_affected() != 1 {
        return Err(AppError::conflict("network binding already exists"));
    }
    Ok(())
}
pub(crate) async fn promote_pending(
    tx: &mut Transaction<'_, Postgres>,
    operator: OperatorId,
    account: Uuid,
) -> Result<(), AppError> {
    sqlx::query("UPDATE account_network_heads SET active_binding_id=pending_binding_id,pending_binding_id=NULL,version=version+1 \
        WHERE operator_id=$1 AND account_id=$2 AND pending_binding_id IS NOT NULL")
        .bind(operator.as_uuid()).bind(account).execute(&mut **tx).await.map_err(db)?;
    Ok(())
}

async fn set_binding_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: &AccountNetworkScope,
    expected: Option<i64>,
    binding: &AccountNetworkBinding,
    legacy: Option<&LegacyNetworkBootstrap>,
) -> Result<AccountNetworkBindings, AppError> {
    validate_network_binding(scope, binding)?;
    lock_head(tx, scope.operator_id, scope.account_id).await?;
    let current = state_tx(tx, scope).await?;
    if current.as_ref().map(|h| h.version) != expected {
        return Err(AppError::conflict("account network version changed"));
    }
    let active_op: Option<Uuid> = sqlx::query_scalar(
        "SELECT browser_operation_id FROM channel_account_preflight_reservations \
        WHERE operator_id=$1 AND account_id=$2 AND browser_operation_id IS NOT NULL FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.account_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    let busy = active_op.is_some();
    validate_assignment(tx, scope, &binding.source).await?;
    if let Some(legacy) = legacy {
        validate_network_binding(scope, &legacy.binding)?;
        if legacy.operation_id != active_op
            || legacy.binding.source != AccountNetworkSource::LegacySnapshot
        {
            return Err(AppError::conflict("legacy network owner changed"));
        }
    }
    if current.is_none() {
        sqlx::query(
            "INSERT INTO account_network_heads(operator_id,account_id,scope) VALUES($1,$2,$3)",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.account_id)
        .bind(encode(scope)?)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    }
    insert_binding(tx, binding).await?;
    let (active, pending) = match current.as_ref() {
        Some(head) if busy => (head.active.binding_id, Some(binding.binding_id)),
        None if busy => {
            let legacy = legacy
                .ok_or_else(|| AppError::conflict("busy legacy network requires initialization"))?;
            insert_binding(tx, &legacy.binding).await?;
            (legacy.binding.binding_id, Some(binding.binding_id))
        }
        _ => (binding.binding_id, None),
    };
    sqlx::query("UPDATE account_network_heads SET active_binding_id=$3,pending_binding_id=$4,version=version+1 WHERE operator_id=$1 AND account_id=$2")
        .bind(scope.operator_id.as_uuid()).bind(scope.account_id).bind(active).bind(pending).execute(&mut **tx).await.map_err(db)?;
    state_tx(tx, scope)
        .await?
        .ok_or_else(|| AppError::conflict("network initialization failed"))
}

#[async_trait]
impl AccountNetworkBindingRepository for PgChannelJobRepository {
    async fn set_project_network_defaults(
        &self,
        scope: &TenantScope,
        expected: Option<&ChannelSettingsRecord>,
        record: &ChannelSettingsRecord,
        updates: &[ProjectNetworkBindingUpdate],
        _channels: &dyn ChannelRepository,
        _at: DateTime<Utc>,
    ) -> Result<ChannelSettings, AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?;
        let network_scope = AccountNetworkScope {
            operator_id: scope.operator_id,
            account_id: Uuid::nil(),
            tenant_id: Some(scope.tenant_id),
            project_id: Some(project),
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_project(&mut tx, &network_scope).await?;
        crate::channels::compare_set_network_settings_tx(&mut tx, scope, expected, record).await?;
        let changed = expected
            .and_then(|e| e.proxy.as_ref())
            .map(ChannelSecret::encrypted_bytes)
            != record.proxy.as_ref().map(ChannelSecret::encrypted_bytes);
        let mut required = std::collections::HashSet::<Uuid>::new();
        if changed {
            let inherited:Vec<Uuid>=sqlx::query_scalar("SELECT h.account_id FROM account_network_heads h JOIN account_network_bindings b \
                ON b.operator_id=h.operator_id AND b.account_id=h.account_id AND b.binding_id=COALESCE(h.pending_binding_id,h.active_binding_id) \
                WHERE h.operator_id=$1 AND h.scope->>'tenant_id'=$2 AND h.scope->>'project_id'=$3 AND b.source->>'kind'='project_default' \
                UNION SELECT a.account_id FROM channel_accounts a JOIN channel_account_preflight_reservations g \
                ON g.operator_id=a.operator_id AND g.account_id=a.account_id WHERE a.operator_id=$1 AND a.tenant_id::text=$2 AND a.project_id::text=$3 \
                AND a.encrypted_proxy IS NULL AND a.metadata->>'owner_kind'='customer' AND g.browser_operation_id IS NOT NULL \
                AND NOT EXISTS(SELECT 1 FROM account_network_heads h WHERE h.operator_id=a.operator_id AND h.account_id=a.account_id)")
                .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.to_string()).bind(project.to_string()).fetch_all(&mut *tx).await.map_err(db)?;
            required.extend(inherited);
        }
        let supplied: std::collections::HashSet<_> =
            updates.iter().map(|u| u.binding.scope.account_id).collect();
        if required != supplied || supplied.len() != updates.len() {
            return Err(AppError::conflict("inherited account network set changed"));
        }
        let mut ordered: Vec<_> = updates.iter().collect();
        ordered.sort_by_key(|u| u.binding.scope.account_id);
        for update in ordered {
            let binding = &update.binding;
            if binding.scope.operator_id != scope.operator_id
                || binding.scope.tenant_id != Some(scope.tenant_id)
                || binding.scope.project_id != Some(project)
                || binding.source
                    != (AccountNetworkSource::ProjectDefault {
                        settings_updated_at: Some(record.settings.updated_at),
                    })
            {
                return Err(AppError::conflict("project default binding differs"));
            }
            set_binding_tx(
                &mut tx,
                &binding.scope,
                update.expected_version,
                binding,
                update.legacy.as_ref(),
            )
            .await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(record.settings.clone())
    }
    async fn get_account_network(
        &self,
        scope: &AccountNetworkScope,
    ) -> Result<Option<AccountNetworkBindings>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_head(&mut tx, scope.operator_id, scope.account_id).await?;
        state_tx(&mut tx, scope).await
    }
    async fn get_network_binding(
        &self,
        scope: &AccountNetworkScope,
        id: Uuid,
    ) -> Result<AccountNetworkBinding, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        binding_tx(&mut tx, scope, id).await
    }
    #[allow(clippy::too_many_arguments)]
    async fn set_account_network_with_metadata(
        &self,
        scope: &AccountNetworkScope,
        expected: Option<i64>,
        binding: &AccountNetworkBinding,
        legacy: Option<&LegacyNetworkBootstrap>,
        metadata: Option<&AccountNetworkMetadataUpdate>,
        _channels: &dyn ChannelRepository,
        _at: DateTime<Utc>,
    ) -> Result<AccountNetworkBindings, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_project(&mut tx, scope).await?;
        let result = set_binding_tx(&mut tx, scope, expected, binding, legacy).await?;
        if let Some(metadata) = metadata {
            crate::channels::save_network_metadata_tx(&mut tx, scope, metadata).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
}
