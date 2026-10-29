use async_trait::async_trait;
use geo_domain::{
    AppError, ChannelAccount, ChannelAccountRecord, ChannelGroup, ChannelOwnerKind,
    ChannelRepository, ChannelSecret, ChannelSessionVersion, ChannelSettings,
    ChannelSettingsRecord, ErrorCode, LoginSession, OperatorId, PoolAccount, PoolAccountRecord,
    PoolAssignment, PoolGroup, PoolLoginSession, TenantScope,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgChannelRepository {
    pub(crate) pool: PgPool,
}

impl PgChannelRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

fn db_error(error: sqlx::Error) -> AppError {
    match &error {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::conflict("channel account identity already exists")
        }
        sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
            AppError::not_found("channel account group or project not found")
        }
        _ => AppError::new(
            ErrorCode::DependencyUnavailable,
            "channel storage unavailable",
        ),
    }
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|project| project.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope is required"))
}

async fn lock_platform_identity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operator: OperatorId,
    platform: &str,
    identity: &str,
) -> Result<(), AppError> {
    // Both account tables use this same transaction-level lock before the
    // cross-table existence check. A reconnect of the same record is allowed.
    let lock_key = format!("{}:{platform}:{identity}", operator.as_uuid());
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}

fn decode_group(json: serde_json::Value) -> Result<ChannelGroup, AppError> {
    serde_json::from_value(json)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored channel group invalid"))
}

pub(crate) async fn compare_set_network_settings_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &TenantScope,
    expected: Option<&ChannelSettingsRecord>,
    record: &ChannelSettingsRecord,
) -> Result<(), AppError> {
    if scope.project_id != Some(record.settings.project_id) {
        return Err(AppError::forbidden("settings outside project"));
    }
    let row=sqlx::query("SELECT metadata,default_group_id,encrypted_proxy FROM channel_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).fetch_optional(&mut **tx).await.map_err(db_error)?;
    let actual = match row {
        Some(row) => {
            let mut settings: ChannelSettings =
                serde_json::from_value(row.try_get("metadata").map_err(db_error)?)
                    .map_err(|_| AppError::new(ErrorCode::Internal, "stored settings invalid"))?;
            settings.default_group_id = row.try_get("default_group_id").map_err(db_error)?;
            Some(ChannelSettingsRecord {
                settings,
                proxy: row
                    .try_get::<Option<Vec<u8>>, _>("encrypted_proxy")
                    .map_err(db_error)?
                    .map(ChannelSecret::new),
            })
        }
        None => None,
    };
    if !geo_domain::settings_snapshot_matches(actual.as_ref(), expected) {
        return Err(AppError::conflict("project settings changed"));
    }
    let metadata = serde_json::to_value(&record.settings)
        .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode settings"))?;
    sqlx::query("INSERT INTO channel_settings(operator_id,tenant_id,project_id,default_group_id,metadata,encrypted_proxy) VALUES($1,$2,$3,$4,$5,$6) \
        ON CONFLICT(operator_id,tenant_id,project_id) DO UPDATE SET default_group_id=EXCLUDED.default_group_id,metadata=EXCLUDED.metadata,encrypted_proxy=EXCLUDED.encrypted_proxy")
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?).bind(record.settings.default_group_id)
        .bind(metadata).bind(record.proxy.as_ref().map(ChannelSecret::encrypted_bytes)).execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}

pub(crate) async fn save_network_metadata_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &geo_domain::AccountNetworkScope,
    update: &geo_domain::AccountNetworkMetadataUpdate,
) -> Result<(), AppError> {
    let encode_error = |_| AppError::new(ErrorCode::Internal, "cannot encode account metadata");
    let (expected, updated, group, session, proxy) = match update {
        geo_domain::AccountNetworkMetadataUpdate::Customer { expected, updated } => {
            if scope.project_id != Some(updated.account.project_id)
                || scope.tenant_id.is_none()
                || updated.account.owner_kind != ChannelOwnerKind::Customer
                || updated.account.account_id != scope.account_id
                || updated.account.platform != expected.account.platform
                || updated.account.platform_account_id != expected.account.platform_account_id
            {
                return Err(AppError::forbidden("account metadata scope differs"));
            }
            (
                serde_json::to_value(&expected.account).map_err(encode_error)?,
                serde_json::to_value(&updated.account).map_err(encode_error)?,
                updated.account.group_id,
                expected
                    .session
                    .as_ref()
                    .map(ChannelSecret::encrypted_bytes),
                expected.proxy.as_ref().map(ChannelSecret::encrypted_bytes),
            )
        }
        geo_domain::AccountNetworkMetadataUpdate::OperatorPool { expected, updated } => {
            if scope.tenant_id.is_some()
                || scope.project_id.is_some()
                || updated.account.account_id != scope.account_id
                || updated.account.platform != expected.account.platform
                || updated.account.platform_account_id != expected.account.platform_account_id
            {
                return Err(AppError::forbidden("account metadata scope differs"));
            }
            (
                serde_json::to_value(&expected.account).map_err(encode_error)?,
                serde_json::to_value(&updated.account).map_err(encode_error)?,
                updated.account.group_id,
                expected
                    .session
                    .as_ref()
                    .map(ChannelSecret::encrypted_bytes),
                expected.proxy.as_ref().map(ChannelSecret::encrypted_bytes),
            )
        }
    };
    let sql = if scope.project_id.is_some() {
        "UPDATE channel_accounts SET metadata=$3 || jsonb_build_object('proxy_configured',metadata->'proxy_configured','proxy_server',metadata->'proxy_server'),group_id=$4 \
         WHERE operator_id=$1 AND account_id=$2 AND (metadata || jsonb_build_object('group_id',group_id))=$5 AND encrypted_session IS NOT DISTINCT FROM $6 \
         AND encrypted_proxy IS NOT DISTINCT FROM $7 AND tenant_id=$8 AND project_id=$9"
    } else {
        "UPDATE operator_channel_accounts SET metadata=$3 || jsonb_build_object('proxy_configured',metadata->'proxy_configured','proxy_server',metadata->'proxy_server'),group_id=$4 \
         WHERE operator_id=$1 AND account_id=$2 AND (metadata || jsonb_build_object('group_id',group_id))=$5 AND encrypted_session IS NOT DISTINCT FROM $6 AND encrypted_proxy IS NOT DISTINCT FROM $7"
    };
    let query = sqlx::query(sql)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.account_id)
        .bind(updated)
        .bind(group)
        .bind(expected)
        .bind(session)
        .bind(proxy);
    let result = match (scope.tenant_id, scope.project_id) {
        (Some(t), Some(p)) => {
            query
                .bind(t.as_uuid())
                .bind(p.as_uuid())
                .execute(&mut **tx)
                .await
        }
        _ => query.execute(&mut **tx).await,
    }
    .map_err(db_error)?;
    if result.rows_affected() != 1 {
        return Err(AppError::conflict("account metadata changed"));
    }
    Ok(())
}

pub(crate) async fn save_verified_login_storage_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    storage: &geo_domain::VerifiedLoginStorage,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), AppError> {
    if let Some(reconnect) = &storage.reconnect {
        let source: Option<Uuid> = sqlx::query_scalar(
            "SELECT account_id FROM channel_accounts WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND account_id=$4 AND platform=$5 AND platform_account_id IS NULL \
             AND encrypted_session IS NULL AND metadata->>'owner_kind'='customer' FOR UPDATE",
        )
        .bind(storage.secret_scope.operator_id.as_uuid())
        .bind(storage.secret_scope.tenant_id.as_uuid())
        .bind(project(&storage.secret_scope)?)
        .bind(reconnect.source_account_id)
        .bind(&storage.platform)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
        if source != Some(reconnect.source_account_id) {
            return Err(AppError::conflict("login source is not an empty account"));
        }
    }
    lock_platform_identity(
        tx,
        storage.secret_scope.operator_id,
        &storage.platform,
        &storage.platform_account_id,
    )
    .await?;
    let cross_table = match storage.owner_kind {
        ChannelOwnerKind::Customer => {
            "SELECT EXISTS(SELECT 1 FROM operator_channel_accounts WHERE operator_id=$1 AND platform=$2 AND platform_account_id=$3)"
        }
        ChannelOwnerKind::OperatorPool => {
            "SELECT EXISTS(SELECT 1 FROM channel_accounts WHERE operator_id=$1 AND platform=$2 AND platform_account_id=$3)"
        }
    };
    let duplicate: bool = sqlx::query_scalar(cross_table)
        .bind(storage.secret_scope.operator_id.as_uuid())
        .bind(&storage.platform)
        .bind(&storage.platform_account_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    if duplicate {
        return Err(AppError::conflict("platform identity is already connected"));
    }
    let patch = serde_json::json!({
        "platform_account_id": storage.platform_account_id,
        "display_name": storage.display_name,
        "avatar_url": storage.avatar_url,
        "status": "ready",
        "updated_at": at,
    });
    let sql = match storage.owner_kind {
        ChannelOwnerKind::Customer => {
            "UPDATE channel_accounts SET platform_account_id=$6,encrypted_session=$7,metadata=metadata || $8::jsonb \
            WHERE operator_id=$1 AND account_id=$2 AND platform=$3 AND platform_account_id IS NOT DISTINCT FROM $4 \
            AND encrypted_session IS NOT DISTINCT FROM $5 AND metadata->>'enabled'='true' AND metadata->>'owner_kind'='customer' \
            AND tenant_id=$9 AND project_id=$10"
        }
        ChannelOwnerKind::OperatorPool => {
            "UPDATE operator_channel_accounts SET platform_account_id=$6,encrypted_session=$7,metadata=metadata || $8::jsonb \
            WHERE operator_id=$1 AND account_id=$2 AND platform=$3 AND platform_account_id IS NOT DISTINCT FROM $4 \
            AND encrypted_session IS NOT DISTINCT FROM $5 AND metadata->>'enabled'='true'"
        }
    };
    let query = sqlx::query(sql)
        .bind(storage.secret_scope.operator_id.as_uuid())
        .bind(storage.account_id)
        .bind(&storage.platform)
        .bind(&storage.expected_platform_account_id)
        .bind(
            storage
                .expected_session
                .as_ref()
                .map(ChannelSecret::encrypted_bytes),
        )
        .bind(&storage.platform_account_id)
        .bind(storage.session.encrypted_bytes())
        .bind(patch);
    let result = match storage.owner_kind {
        ChannelOwnerKind::Customer => {
            crate::set_local_scope(tx, &storage.secret_scope)
                .await
                .map_err(db_error)?;
            query
                .bind(storage.secret_scope.tenant_id.as_uuid())
                .bind(project(&storage.secret_scope)?)
                .execute(&mut **tx)
                .await
        }
        ChannelOwnerKind::OperatorPool => query.execute(&mut **tx).await,
    }
    .map_err(db_error)?;
    if result.rows_affected() != 1 {
        return Err(AppError::conflict(
            "login account changed during verification",
        ));
    }
    if let Some(reconnect) = &storage.reconnect {
        sqlx::query(
            "UPDATE channel_accounts SET metadata=metadata || $5::jsonb \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4",
        )
        .bind(storage.secret_scope.operator_id.as_uuid())
        .bind(storage.secret_scope.tenant_id.as_uuid())
        .bind(project(&storage.secret_scope)?)
        .bind(reconnect.source_account_id)
        .bind(serde_json::json!({"enabled":false,"status":"disabled","updated_at":at}))
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

fn decode_account(
    json: serde_json::Value,
    group_id: Option<Uuid>,
) -> Result<ChannelAccount, AppError> {
    let mut account: ChannelAccount = serde_json::from_value(json)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored channel account invalid"))?;
    account.group_id = group_id;
    Ok(account)
}

#[async_trait]
impl ChannelRepository for PgChannelRepository {
    async fn save_network_metadata(
        &self,
        scope: &geo_domain::AccountNetworkScope,
        update: &geo_domain::AccountNetworkMetadataUpdate,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        save_network_metadata_tx(&mut tx, scope, update).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    async fn compare_set_network_settings(
        &self,
        scope: &TenantScope,
        expected: Option<&ChannelSettingsRecord>,
        record: &ChannelSettingsRecord,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        compare_set_network_settings_tx(&mut tx, scope, expected, record).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    async fn validate_account_network_assignment(
        &self,
        scope: &geo_domain::AccountNetworkScope,
        source: &geo_domain::AccountNetworkSource,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        super::account_networks::validate_assignment(&mut tx, scope, source).await
    }
    async fn save_verified_login_storage(
        &self,
        storage: &geo_domain::VerifiedLoginStorage,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        save_verified_login_storage_tx(&mut tx, storage, at).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    async fn account_login_sessions(
        &self,
        operator: OperatorId,
        account_id: Uuid,
    ) -> Result<Vec<Uuid>, AppError> {
        sqlx::query_scalar("SELECT session_id FROM channel_login_sessions WHERE operator_id=$1 AND account_id=$2 \
            UNION SELECT session_id FROM operator_channel_login_sessions WHERE operator_id=$1 AND account_id=$2")
            .bind(operator.as_uuid()).bind(account_id).fetch_all(&self.pool).await.map_err(db_error)
    }
    async fn list_import_rows(
        &self,
        operator: OperatorId,
        import_key: &str,
    ) -> Result<Vec<geo_domain::ChannelImportRecord>, AppError> {
        let rows: Vec<(Uuid, Vec<u8>, serde_json::Value)> = sqlx::query_as(
            "SELECT secret_id,encrypted_input,outcome FROM operator_channel_import_rows WHERE operator_id=$1 AND import_key=$2 ORDER BY (outcome->>'ordinal')::integer,row_key",
        ).bind(operator.as_uuid()).bind(import_key).fetch_all(&self.pool).await.map_err(db_error)?;
        rows.into_iter()
            .map(|(secret_id, encrypted, outcome)| {
                Ok(geo_domain::ChannelImportRecord {
                    secret_id,
                    encrypted_input: ChannelSecret::new(encrypted),
                    row: serde_json::from_value(outcome).map_err(|_| {
                        AppError::new(ErrorCode::Internal, "stored import outcome invalid")
                    })?,
                })
            })
            .collect()
    }

    async fn save_import_row(
        &self,
        operator: OperatorId,
        import_key: &str,
        expected: Option<ChannelSecret>,
        mut record: geo_domain::ChannelImportRecord,
        account: Option<PoolAccount>,
    ) -> Result<geo_domain::ChannelImportRecord, AppError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        // Serialize each batch's insert/count/CAS. The lock input contains only
        // opaque request keys, never credential material.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "channel-import:{}:{import_key}",
                operator.as_uuid()
            ))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let current: Option<(Uuid, Vec<u8>, serde_json::Value)> = sqlx::query_as(
            "SELECT secret_id,encrypted_input,outcome FROM operator_channel_import_rows WHERE operator_id=$1 AND import_key=$2 AND row_key=$3 FOR UPDATE",
        ).bind(operator.as_uuid()).bind(import_key).bind(&record.row.row_key)
            .fetch_optional(&mut *tx).await.map_err(db_error)?;
        if let Some((secret_id, encrypted, outcome)) = current {
            let row: geo_domain::ChannelImportRow = serde_json::from_value(outcome)
                .map_err(|_| AppError::new(ErrorCode::Internal, "stored import outcome invalid"))?;
            if row.status != geo_domain::ChannelImportStatus::Failed
                || expected.as_ref().map(ChannelSecret::encrypted_bytes)
                    != Some(encrypted.as_slice())
            {
                return Ok(geo_domain::ChannelImportRecord {
                    row,
                    secret_id,
                    encrypted_input: ChannelSecret::new(encrypted),
                });
            }
            record.row.ordinal = row.ordinal;
        } else {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM operator_channel_import_rows WHERE operator_id=$1 AND import_key=$2",
            ).bind(operator.as_uuid()).bind(import_key).fetch_one(&mut *tx).await.map_err(db_error)?;
            if count >= 100 {
                return Err(AppError::invalid_request("import batch exceeds 100 rows"));
            }
        }
        if let Some(account) = account {
            let group_exists = if let Some(group_id) = account.group_id {
                // Prevent group deletion between validation and account insertion.
                sqlx::query_scalar::<_, Uuid>(
                    "SELECT group_id FROM operator_channel_groups WHERE operator_id=$1 AND group_id=$2 FOR KEY SHARE",
                ).bind(operator.as_uuid()).bind(group_id).fetch_optional(&mut *tx).await.map_err(db_error)?.is_some()
            } else {
                true
            };
            if group_exists {
                let metadata = serde_json::to_value(&account).map_err(|_| {
                    AppError::new(ErrorCode::Internal, "cannot encode imported account")
                })?;
                sqlx::query(
                    "INSERT INTO operator_channel_accounts(operator_id,account_id,platform,group_id,metadata) VALUES($1,$2,$3,$4,$5)",
                ).bind(operator.as_uuid()).bind(account.account_id).bind(account.platform)
                    .bind(account.group_id).bind(metadata).execute(&mut *tx).await.map_err(db_error)?;
            } else {
                record.row.status = geo_domain::ChannelImportStatus::Failed;
                record.row.account_id = None;
                record.row.error_code = Some("group_not_found".into());
            }
        }
        let outcome = serde_json::to_value(&record.row)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode import outcome"))?;
        sqlx::query(
            "INSERT INTO operator_channel_import_rows(operator_id,import_key,row_key,secret_id,encrypted_input,outcome) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(operator_id,import_key,row_key) DO UPDATE SET outcome=EXCLUDED.outcome",
        ).bind(operator.as_uuid()).bind(import_key).bind(&record.row.row_key).bind(record.secret_id)
            .bind(record.encrypted_input.encrypted_bytes()).bind(outcome).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(record)
    }
    async fn save_account_metadata(
        &self,
        scope: &TenantScope,
        record: ChannelAccountRecord,
    ) -> Result<ChannelAccount, AppError> {
        if Some(record.account.project_id) != scope.project_id {
            return Err(AppError::forbidden("account outside project"));
        }
        let metadata = serde_json::to_value(&record.account)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode channel account"))?;
        let result:Option<serde_json::Value> = sqlx::query_scalar(
            "UPDATE channel_accounts SET metadata=$5 || jsonb_build_object('proxy_configured',metadata->'proxy_configured','proxy_server',metadata->'proxy_server'),group_id=$6 \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4 \
            AND encrypted_session IS NOT DISTINCT FROM $7 AND platform=$8 \
            AND platform_account_id IS NOT DISTINCT FROM $9 AND metadata->>'owner_kind'=$10 RETURNING metadata",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(record.account.account_id)
        .bind(metadata)
        .bind(record.account.group_id)
        .bind(record.session.as_ref().map(ChannelSecret::encrypted_bytes))
        .bind(&record.account.platform)
        .bind(&record.account.platform_account_id)
        .bind(match record.account.owner_kind {
            ChannelOwnerKind::Customer => "customer",
            ChannelOwnerKind::OperatorPool => "operator_pool",
        })
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        decode_account(
            result.ok_or_else(|| AppError::conflict("channel account changed; retry the edit"))?,
            record.account.group_id,
        )
    }

    async fn save_pool_account_metadata(
        &self,
        operator: OperatorId,
        record: PoolAccountRecord,
    ) -> Result<PoolAccount, AppError> {
        let metadata = serde_json::to_value(&record.account)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode pool account"))?;
        let result:Option<serde_json::Value> = sqlx::query_scalar(
            "UPDATE operator_channel_accounts SET metadata=$3 || jsonb_build_object('proxy_configured',metadata->'proxy_configured','proxy_server',metadata->'proxy_server'),group_id=$4 \
            WHERE operator_id=$1 AND account_id=$2 AND encrypted_session IS NOT DISTINCT FROM $5 \
            AND platform=$6 AND platform_account_id IS NOT DISTINCT FROM $7 RETURNING metadata",
        )
        .bind(operator.as_uuid())
        .bind(record.account.account_id)
        .bind(metadata)
        .bind(record.account.group_id)
        .bind(record.session.as_ref().map(ChannelSecret::encrypted_bytes))
        .bind(&record.account.platform)
        .bind(&record.account.platform_account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        serde_json::from_value(
            result.ok_or_else(|| AppError::conflict("channel account changed; retry the edit"))?,
        )
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored pool account invalid"))
    }

    async fn renew_session(
        &self,
        scope: &TenantScope,
        expected: &ChannelSessionVersion,
        renewed: ChannelSecret,
    ) -> Result<bool, AppError> {
        let project_id = project(scope)?;
        if expected.platform_account_id.trim().is_empty() {
            return Ok(false);
        }
        // A single conditional UPDATE serializes with reconnect/disable writes.
        // Deliberately do not replace metadata, timestamps, groups or proxies.
        let statement = match expected.owner_kind {
            ChannelOwnerKind::Customer => {
                "UPDATE channel_accounts SET encrypted_session=$8 \
                WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4 \
                AND platform=$5 AND platform_account_id=$6 AND encrypted_session=$7 \
                AND metadata->>'enabled'='true' AND metadata->>'status'='ready' \
                AND metadata->>'owner_kind'='customer'"
            }
            ChannelOwnerKind::OperatorPool => {
                "UPDATE operator_channel_accounts AS a SET encrypted_session=$8 \
                WHERE a.operator_id=$1 AND a.account_id=$4 \
                AND a.platform=$5 AND a.platform_account_id=$6 AND a.encrypted_session=$7 \
                AND a.metadata->>'enabled'='true' AND a.metadata->>'status'='ready' \
                AND EXISTS (SELECT 1 FROM operator_channel_assignments AS assignment \
                    WHERE assignment.operator_id=a.operator_id AND assignment.account_id=a.account_id \
                    AND assignment.tenant_id=$2 AND assignment.project_id=$3)"
            }
        };
        let result = sqlx::query(statement)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(expected.account_id)
            .bind(&expected.platform)
            .bind(&expected.platform_account_id)
            .bind(expected.session.encrypted_bytes())
            .bind(renewed.encrypted_bytes())
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(result.rows_affected() == 1)
    }

    async fn list_pool_groups(&self, operator: OperatorId) -> Result<Vec<PoolGroup>, AppError> {
        let rows: Vec<(Uuid, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT group_id,name,created_at FROM operator_channel_groups WHERE operator_id=$1 ORDER BY created_at,group_id",
        ).bind(operator.as_uuid()).fetch_all(&self.pool).await.map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|(group_id, name, created_at)| PoolGroup {
                group_id,
                name,
                created_at,
            })
            .collect())
    }

    async fn save_pool_group(
        &self,
        operator: OperatorId,
        group: PoolGroup,
    ) -> Result<PoolGroup, AppError> {
        sqlx::query(
            "INSERT INTO operator_channel_groups(operator_id,group_id,name,created_at) VALUES($1,$2,$3,$4) ON CONFLICT(operator_id,group_id) DO UPDATE SET name=EXCLUDED.name",
        ).bind(operator.as_uuid()).bind(group.group_id).bind(&group.name).bind(group.created_at)
            .execute(&self.pool).await.map_err(db_error)?;
        Ok(group)
    }

    async fn delete_pool_group(&self, operator: OperatorId, id: Uuid) -> Result<(), AppError> {
        let result =
            sqlx::query("DELETE FROM operator_channel_groups WHERE operator_id=$1 AND group_id=$2")
                .bind(operator.as_uuid())
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(db_error)?;
        if result.rows_affected() == 0 {
            return Err(AppError::not_found("pool group not found"));
        }
        Ok(())
    }

    async fn list_pool_accounts(&self, operator: OperatorId) -> Result<Vec<PoolAccount>, AppError> {
        let rows: Vec<(serde_json::Value,Option<Uuid>)> = sqlx::query_as(
            "SELECT metadata,group_id FROM operator_channel_accounts WHERE operator_id=$1 ORDER BY account_id",
        ).bind(operator.as_uuid()).fetch_all(&self.pool).await.map_err(db_error)?;
        rows.into_iter()
            .map(|(metadata, group)| {
                let mut account: PoolAccount = serde_json::from_value(metadata).map_err(|_| {
                    AppError::new(ErrorCode::Internal, "stored pool account invalid")
                })?;
                account.group_id = group;
                Ok(account)
            })
            .collect()
    }

    async fn get_pool_account(
        &self,
        operator: OperatorId,
        id: Uuid,
    ) -> Result<PoolAccountRecord, AppError> {
        let row: Option<(serde_json::Value,Option<Uuid>,Option<Vec<u8>>,Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT metadata,group_id,encrypted_session,encrypted_proxy FROM operator_channel_accounts WHERE operator_id=$1 AND account_id=$2",
        ).bind(operator.as_uuid()).bind(id).fetch_optional(&self.pool).await.map_err(db_error)?;
        let (metadata, group, session, proxy) =
            row.ok_or_else(|| AppError::not_found("pool account not found"))?;
        let mut account: PoolAccount = serde_json::from_value(metadata)
            .map_err(|_| AppError::new(ErrorCode::Internal, "stored pool account invalid"))?;
        account.group_id = group;
        Ok(PoolAccountRecord {
            account,
            session: session.map(ChannelSecret::new),
            proxy: proxy.map(ChannelSecret::new),
        })
    }

    async fn save_pool_account(
        &self,
        operator: OperatorId,
        record: PoolAccountRecord,
    ) -> Result<PoolAccount, AppError> {
        let account = &record.account;
        let metadata = serde_json::to_value(account)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode pool account"))?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        if let Some(identity) = &account.platform_account_id {
            lock_platform_identity(&mut tx, operator, &account.platform, identity).await?;
            let exists: Option<i32> = sqlx::query_scalar(
                "SELECT 1 FROM channel_accounts WHERE operator_id=$1 AND platform=$2 AND platform_account_id=$3 LIMIT 1",
            )
            .bind(operator.as_uuid()).bind(&account.platform).bind(identity)
            .fetch_optional(&mut *tx).await.map_err(db_error)?;
            if exists.is_some() {
                return Err(AppError::conflict("platform identity is already connected"));
            }
        }
        sqlx::query(
            "INSERT INTO operator_channel_accounts(operator_id,account_id,platform,platform_account_id,group_id,metadata,encrypted_session,encrypted_proxy) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(operator_id,account_id) DO UPDATE SET platform_account_id=EXCLUDED.platform_account_id,group_id=EXCLUDED.group_id,metadata=EXCLUDED.metadata || jsonb_build_object('proxy_configured',operator_channel_accounts.metadata->'proxy_configured','proxy_server',operator_channel_accounts.metadata->'proxy_server'),encrypted_session=EXCLUDED.encrypted_session",
        ).bind(operator.as_uuid()).bind(account.account_id).bind(&account.platform)
            .bind(&account.platform_account_id).bind(account.group_id).bind(metadata)
            .bind(record.session.as_ref().map(ChannelSecret::encrypted_bytes))
            .bind(record.proxy.as_ref().map(ChannelSecret::encrypted_bytes))
            .execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(self
            .get_pool_account(operator, account.account_id)
            .await?
            .account)
    }

    async fn delete_pool_account(&self, operator: OperatorId, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "DELETE FROM operator_channel_accounts WHERE operator_id=$1 AND account_id=$2",
        )
        .bind(operator.as_uuid())
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if result.rows_affected() == 0 {
            return Err(AppError::not_found("pool account not found"));
        }
        Ok(())
    }

    async fn assign_pool_account(
        &self,
        scope: &TenantScope,
        id: Uuid,
        assigned: bool,
    ) -> Result<(), AppError> {
        let p = project(scope)?;
        if assigned {
            sqlx::query(
                "INSERT INTO operator_channel_assignments(operator_id,tenant_id,project_id,account_id) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(p).bind(id)
                .execute(&self.pool).await.map_err(db_error)?;
        } else {
            sqlx::query(
                "DELETE FROM operator_channel_assignments WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4",
            ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(p).bind(id)
                .execute(&self.pool).await.map_err(db_error)?;
        }
        Ok(())
    }

    async fn list_assigned_pool_accounts(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<PoolAccount>, AppError> {
        let rows: Vec<(serde_json::Value,Option<Uuid>)>=sqlx::query_as(
            "SELECT a.metadata,a.group_id FROM operator_channel_assignments x JOIN operator_channel_accounts a ON (a.operator_id=x.operator_id AND a.account_id=x.account_id) WHERE x.operator_id=$1 AND x.tenant_id=$2 AND x.project_id=$3 ORDER BY a.account_id",
        ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project(scope)?)
            .fetch_all(&self.pool).await.map_err(db_error)?;
        rows.into_iter()
            .map(|(metadata, group)| {
                let mut account: PoolAccount = serde_json::from_value(metadata).map_err(|_| {
                    AppError::new(ErrorCode::Internal, "stored pool account invalid")
                })?;
                account.group_id = group;
                Ok(account)
            })
            .collect()
    }

    async fn list_pool_assignments(
        &self,
        operator: OperatorId,
        account_id: Uuid,
    ) -> Result<Vec<PoolAssignment>, AppError> {
        self.get_pool_account(operator, account_id).await?;
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT tenant_id,project_id FROM operator_channel_assignments WHERE operator_id=$1 AND account_id=$2 ORDER BY tenant_id,project_id",
        )
        .bind(operator.as_uuid()).bind(account_id).fetch_all(&self.pool).await.map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|(tenant_id, project_id)| PoolAssignment {
                tenant_id: tenant_id.into(),
                project_id: project_id.into(),
            })
            .collect())
    }

    async fn save_pool_login(
        &self,
        operator: OperatorId,
        session: PoolLoginSession,
    ) -> Result<(), AppError> {
        let result = sqlx::query("INSERT INTO operator_channel_login_sessions(operator_id,session_id,account_id,created_at) VALUES($1,$2,$3,$4) \
            ON CONFLICT(operator_id,session_id) DO UPDATE SET account_id=operator_channel_login_sessions.account_id \
            WHERE operator_channel_login_sessions.account_id=EXCLUDED.account_id")
            .bind(operator.as_uuid()).bind(session.session_id).bind(session.account_id).bind(session.created_at)
            .execute(&self.pool).await.map_err(db_error)?;
        if result.rows_affected() != 1 {
            return Err(AppError::conflict("pool login reference differs"));
        }
        Ok(())
    }

    async fn get_pool_login(
        &self,
        operator: OperatorId,
        id: Uuid,
    ) -> Result<PoolLoginSession, AppError> {
        let row:Option<(Uuid,chrono::DateTime<chrono::Utc>)>=sqlx::query_as(
            "SELECT account_id,created_at FROM operator_channel_login_sessions WHERE operator_id=$1 AND session_id=$2",
        ).bind(operator.as_uuid()).bind(id).fetch_optional(&self.pool).await.map_err(db_error)?;
        let (account_id, created_at) =
            row.ok_or_else(|| AppError::not_found("pool login not found"))?;
        Ok(PoolLoginSession {
            session_id: id,
            account_id,
            created_at,
        })
    }

    async fn delete_pool_login(&self, operator: OperatorId, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "DELETE FROM operator_channel_login_sessions WHERE operator_id=$1 AND session_id=$2",
        )
        .bind(operator.as_uuid())
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if result.rows_affected() == 0 {
            return Err(AppError::not_found("pool login not found"));
        }
        Ok(())
    }

    async fn get_settings(
        &self,
        scope: &TenantScope,
    ) -> Result<Option<ChannelSettingsRecord>, AppError> {
        let row: Option<(serde_json::Value, Option<Uuid>, Option<Vec<u8>>)> = sqlx::query_as(
            "SELECT metadata,default_group_id,encrypted_proxy FROM channel_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).fetch_optional(&self.pool).await.map_err(db_error)?;
        row.map(|(metadata, group, secret)| {
            let mut settings: ChannelSettings = serde_json::from_value(metadata).map_err(|_| {
                AppError::new(ErrorCode::Internal, "stored channel settings invalid")
            })?;
            settings.default_group_id = group;
            Ok(ChannelSettingsRecord {
                settings,
                proxy: secret.map(ChannelSecret::new),
            })
        })
        .transpose()
    }

    async fn save_settings(
        &self,
        scope: &TenantScope,
        record: ChannelSettingsRecord,
    ) -> Result<ChannelSettings, AppError> {
        if scope.project_id != Some(record.settings.project_id) {
            return Err(AppError::forbidden("channel settings outside project"));
        }
        let metadata = serde_json::to_value(&record.settings)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode channel settings"))?;
        sqlx::query(
            "INSERT INTO channel_settings (operator_id,tenant_id,project_id,default_group_id,metadata,encrypted_proxy) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (operator_id,tenant_id,project_id) DO UPDATE SET default_group_id=EXCLUDED.default_group_id,metadata=EXCLUDED.metadata,encrypted_proxy=EXCLUDED.encrypted_proxy",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(record.settings.default_group_id).bind(metadata)
        .bind(record.proxy.as_ref().map(ChannelSecret::encrypted_bytes))
        .execute(&self.pool).await.map_err(db_error)?;
        Ok(record.settings)
    }

    async fn list_groups(&self, scope: &TenantScope) -> Result<Vec<ChannelGroup>, AppError> {
        let p = project(scope)?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object('group_id', group_id, 'project_id', project_id, 'name', name, 'created_at', created_at) FROM channel_groups WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY created_at, group_id",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(p)
        .fetch_all(&self.pool).await.map_err(db_error)?;
        rows.into_iter().map(decode_group).collect()
    }

    async fn save_group(
        &self,
        scope: &TenantScope,
        group: ChannelGroup,
    ) -> Result<ChannelGroup, AppError> {
        if scope.project_id != Some(group.project_id) {
            return Err(AppError::forbidden("group outside project"));
        }
        sqlx::query(
            "INSERT INTO channel_groups (operator_id, tenant_id, project_id, group_id, name, created_at) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (operator_id, tenant_id, project_id, group_id) DO UPDATE SET name=EXCLUDED.name",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(group.group_id).bind(&group.name).bind(group.created_at)
        .execute(&self.pool).await.map_err(db_error)?;
        Ok(group)
    }

    async fn delete_group(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "DELETE FROM channel_groups WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND group_id=$4",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(id).execute(&self.pool).await.map_err(db_error)?;
        if result.rows_affected() == 0 {
            return Err(AppError::not_found("group not found"));
        }
        Ok(())
    }

    async fn list_accounts(&self, scope: &TenantScope) -> Result<Vec<ChannelAccount>, AppError> {
        let rows: Vec<(serde_json::Value, Option<Uuid>)> = sqlx::query_as(
            "SELECT metadata, group_id FROM channel_accounts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY account_id",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).fetch_all(&self.pool).await.map_err(db_error)?;
        rows.into_iter()
            .map(|(metadata, group)| decode_account(metadata, group))
            .collect()
    }

    async fn get_account(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<ChannelAccountRecord, AppError> {
        let row: Option<(serde_json::Value, Option<Uuid>, Option<Vec<u8>>, Option<Vec<u8>>)> =
            sqlx::query_as(
                "SELECT metadata, group_id, encrypted_session, encrypted_proxy FROM channel_accounts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4",
            )
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?).bind(id).fetch_optional(&self.pool).await.map_err(db_error)?;
        let (metadata, group_id, session, proxy) =
            row.ok_or_else(|| AppError::not_found("account not found"))?;
        Ok(ChannelAccountRecord {
            account: decode_account(metadata, group_id)?,
            session: session.map(ChannelSecret::new),
            proxy: proxy.map(ChannelSecret::new),
        })
    }

    async fn save_account(
        &self,
        scope: &TenantScope,
        record: ChannelAccountRecord,
    ) -> Result<ChannelAccount, AppError> {
        let account = &record.account;
        if scope.project_id != Some(account.project_id) {
            return Err(AppError::forbidden("account outside project"));
        }
        let metadata = serde_json::to_value(account)
            .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode account"))?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(db_error)?;
        if let Some(identity) = &account.platform_account_id {
            lock_platform_identity(&mut tx, scope.operator_id, &account.platform, identity).await?;
            let exists: Option<i32> = sqlx::query_scalar(
                "SELECT 1 FROM operator_channel_accounts WHERE operator_id=$1 AND platform=$2 AND platform_account_id=$3 LIMIT 1",
            )
            .bind(scope.operator_id.as_uuid()).bind(&account.platform).bind(identity)
            .fetch_optional(&mut *tx).await.map_err(db_error)?;
            if exists.is_some() {
                return Err(AppError::conflict("platform identity is already connected"));
            }
        }
        sqlx::query(
            "INSERT INTO channel_accounts (operator_id,tenant_id,project_id,account_id,platform,platform_account_id,group_id,metadata,encrypted_session,encrypted_proxy) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (operator_id,tenant_id,project_id,account_id) DO UPDATE SET platform_account_id=EXCLUDED.platform_account_id,group_id=EXCLUDED.group_id,metadata=EXCLUDED.metadata || jsonb_build_object('proxy_configured',channel_accounts.metadata->'proxy_configured','proxy_server',channel_accounts.metadata->'proxy_server'),encrypted_session=EXCLUDED.encrypted_session",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(account.account_id).bind(&account.platform)
        .bind(&account.platform_account_id).bind(account.group_id).bind(metadata)
        .bind(record.session.as_ref().map(ChannelSecret::encrypted_bytes))
        .bind(record.proxy.as_ref().map(ChannelSecret::encrypted_bytes))
        .execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(self.get_account(scope, account.account_id).await?.account)
    }

    async fn delete_account(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "DELETE FROM channel_accounts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND account_id=$4",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(id).execute(&self.pool).await.map_err(db_error)?;
        if result.rows_affected() == 0 {
            return Err(AppError::not_found("account not found"));
        }
        Ok(())
    }

    async fn save_login(&self, scope: &TenantScope, session: LoginSession) -> Result<(), AppError> {
        if scope.project_id != Some(session.project_id) {
            return Err(AppError::forbidden("login outside project"));
        }
        let result = sqlx::query(
            "INSERT INTO channel_login_sessions (operator_id,tenant_id,project_id,session_id,account_id,created_at) VALUES ($1,$2,$3,$4,$5,$6) \
             ON CONFLICT(operator_id,tenant_id,project_id,session_id) DO UPDATE SET account_id=channel_login_sessions.account_id \
             WHERE channel_login_sessions.account_id=EXCLUDED.account_id",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(session.session_id).bind(session.account_id)
        .bind(session.created_at).execute(&self.pool).await.map_err(db_error)?;
        if result.rows_affected() != 1 {
            return Err(AppError::conflict("login reference differs"));
        }
        Ok(())
    }

    async fn get_login(&self, scope: &TenantScope, id: Uuid) -> Result<LoginSession, AppError> {
        let row: Option<(Uuid, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT account_id,created_at FROM channel_login_sessions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND session_id=$4",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(id).fetch_optional(&self.pool).await.map_err(db_error)?;
        let (account_id, created_at) = row.ok_or_else(|| AppError::not_found("login not found"))?;
        Ok(LoginSession {
            session_id: id,
            account_id,
            project_id: scope.project_id.expect("project checked"),
            created_at,
        })
    }

    async fn delete_login(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "DELETE FROM channel_login_sessions WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND session_id=$4",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(id).execute(&self.pool).await.map_err(db_error)?;
        if result.rows_affected() == 0 {
            return Err(AppError::not_found("login not found"));
        }
        Ok(())
    }
}
