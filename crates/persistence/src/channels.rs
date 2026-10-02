use async_trait::async_trait;
use geo_domain::{
    AppError, ChannelAccount, ChannelAccountRecord, ChannelGroup, ChannelRepository, ChannelSecret,
    ChannelSettings, ChannelSettingsRecord, ErrorCode, LoginSession, OperatorId, PoolAccount,
    PoolAccountRecord, PoolAssignment, PoolGroup, PoolLoginSession, TenantScope,
};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgChannelRepository {
    pool: PgPool,
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
            "INSERT INTO operator_channel_accounts(operator_id,account_id,platform,platform_account_id,group_id,metadata,encrypted_session,encrypted_proxy) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(operator_id,account_id) DO UPDATE SET platform_account_id=EXCLUDED.platform_account_id,group_id=EXCLUDED.group_id,metadata=EXCLUDED.metadata,encrypted_session=EXCLUDED.encrypted_session,encrypted_proxy=EXCLUDED.encrypted_proxy",
        ).bind(operator.as_uuid()).bind(account.account_id).bind(&account.platform)
            .bind(&account.platform_account_id).bind(account.group_id).bind(metadata)
            .bind(record.session.as_ref().map(ChannelSecret::encrypted_bytes))
            .bind(record.proxy.as_ref().map(ChannelSecret::encrypted_bytes))
            .execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(account.clone())
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
        sqlx::query("INSERT INTO operator_channel_login_sessions(operator_id,session_id,account_id,created_at) VALUES($1,$2,$3,$4)")
            .bind(operator.as_uuid()).bind(session.session_id).bind(session.account_id).bind(session.created_at)
            .execute(&self.pool).await.map_err(db_error)?;
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
            "INSERT INTO channel_accounts (operator_id,tenant_id,project_id,account_id,platform,platform_account_id,group_id,metadata,encrypted_session,encrypted_proxy) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (operator_id,tenant_id,project_id,account_id) DO UPDATE SET platform_account_id=EXCLUDED.platform_account_id,group_id=EXCLUDED.group_id,metadata=EXCLUDED.metadata,encrypted_session=EXCLUDED.encrypted_session,encrypted_proxy=EXCLUDED.encrypted_proxy",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(account.account_id).bind(&account.platform)
        .bind(&account.platform_account_id).bind(account.group_id).bind(metadata)
        .bind(record.session.as_ref().map(ChannelSecret::encrypted_bytes))
        .bind(record.proxy.as_ref().map(ChannelSecret::encrypted_bytes))
        .execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(account.clone())
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
        sqlx::query(
            "INSERT INTO channel_login_sessions (operator_id,tenant_id,project_id,session_id,account_id,created_at) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?).bind(session.session_id).bind(session.account_id)
        .bind(session.created_at).execute(&self.pool).await.map_err(db_error)?;
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
