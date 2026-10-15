use async_trait::async_trait;
use geo_domain::*;
use sqlx::{PgPool, Row, postgres::PgRow};

#[derive(Clone)]
pub struct PgProjectSerpSettingsRepository {
    pool: PgPool,
}
impl PgProjectSerpSettingsRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}
fn unavailable(_: sqlx::Error) -> AppError {
    AppError::not_ready("search settings storage unavailable")
}
pub(crate) fn decode(row: &PgRow) -> Result<ProjectSerpSettingsRecord, AppError> {
    if row.get::<String, _>("provider") != "dataforseo" {
        return Err(AppError::new(
            ErrorCode::Internal,
            "stored search provider invalid",
        ));
    }
    Ok(ProjectSerpSettingsRecord {
        source_key: row.get("source_key"),
        provider: ProjectSerpProvider::Dataforseo,
        revision: row.get("revision"),
        enabled: row.get("enabled"),
        protocol_defaults: serde_json::from_value(row.get("protocol_defaults"))
            .map_err(|_| AppError::new(ErrorCode::Internal, "stored search settings invalid"))?,
        active_credential_revision: row.get("active_credential_revision"),
    })
}
fn limit_value(limit: usize) -> Result<i64, AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request(
            "invalid search settings page size",
        ));
    }
    Ok(limit as i64)
}
#[async_trait]
impl ProjectSerpSettingsRepository for PgProjectSerpSettingsRepository {
    async fn get(
        &self,
        scope: &TenantScope,
        source_key: &str,
    ) -> Result<Option<ProjectSerpSettingsRecord>, AppError> {
        let key = project_serp_settings_key(scope, source_key)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let row = sqlx::query("SELECT * FROM project_serp_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_key=$4")
            .bind(key.operator_id).bind(key.tenant_id).bind(key.project_id).bind(key.source_key).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let result = row.as_ref().map(decode).transpose()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }
    async fn list(
        &self,
        scope: &TenantScope,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpSettingsRecord>, AppError> {
        let key = project_serp_settings_key(scope, "scope")?;
        let limit = limit_value(limit)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        if let Some(after) = &after {
            project_serp_settings_key(scope, after)?;
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_serp_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_key=$4)")
                .bind(key.operator_id).bind(key.tenant_id).bind(key.project_id).bind(after).fetch_one(&mut *tx).await.map_err(unavailable)?;
            if !exists {
                return Err(AppError::invalid_request("invalid search settings cursor"));
            }
        }
        let rows = sqlx::query("SELECT * FROM project_serp_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND ($4::text IS NULL OR source_key>$4) ORDER BY source_key LIMIT $5")
            .bind(key.operator_id).bind(key.tenant_id).bind(key.project_id).bind(after).bind(limit).fetch_all(&mut *tx).await.map_err(unavailable)?;
        let result = rows.iter().map(decode).collect::<Result<_, _>>()?;
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }
    async fn save(
        &self,
        scope: &TenantScope,
        expected: i64,
        write: ProjectSerpSettingsWrite,
    ) -> Result<ProjectSerpSettingsRecord, AppError> {
        write.validate(scope, expected)?;
        let key = project_serp_settings_key(scope, &write.source_key)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        // Serialize both first creation and later CAS updates. A hash collision
        // only serializes unrelated sources; the scoped row predicate remains
        // authoritative and no secret enters the advisory key.
        let lock_key = String::from_utf8(project_serp_credential_aad(scope, &write.source_key, 1)?)
            .map_err(|_| AppError::invalid_request("invalid search settings context"))?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(lock_key)
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let prior = sqlx::query("SELECT * FROM project_serp_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_key=$4 FOR UPDATE")
            .bind(key.operator_id).bind(key.tenant_id).bind(key.project_id).bind(&key.source_key).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let prior = prior.as_ref().map(decode).transpose()?;
        let record = write.apply(expected, prior.as_ref())?;
        let protocol = serde_json::to_value(&record.protocol_defaults)
            .map_err(|_| AppError::invalid_request("invalid search protocol"))?;
        sqlx::query("INSERT INTO project_serp_settings (operator_id,tenant_id,project_id,source_key,revision,provider,enabled,protocol_defaults,active_credential_revision) VALUES($1,$2,$3,$4,$5,'dataforseo',$6,$7,$8) ON CONFLICT(operator_id,tenant_id,project_id,source_key) DO UPDATE SET revision=EXCLUDED.revision,enabled=EXCLUDED.enabled,protocol_defaults=EXCLUDED.protocol_defaults,active_credential_revision=EXCLUDED.active_credential_revision,updated_at=clock_timestamp()")
            .bind(key.operator_id).bind(key.tenant_id).bind(key.project_id).bind(&key.source_key).bind(record.revision)
            .bind(record.enabled).bind(protocol).bind(record.active_credential_revision).execute(&mut *tx).await.map_err(unavailable)?;
        if let Some(bytes) = write.encrypted_credentials {
            sqlx::query("INSERT INTO project_serp_credentials (operator_id,tenant_id,project_id,source_key,credential_revision,encrypted_credentials) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(key.operator_id).bind(key.tenant_id).bind(key.project_id).bind(&key.source_key).bind(record.revision).bind(bytes).execute(&mut *tx).await.map_err(unavailable)?;
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(record)
    }
    async fn get_credential(
        &self,
        scope: &TenantScope,
        source_key: &str,
        revision: i64,
    ) -> Result<Option<ProjectSerpCredentialRecord>, AppError> {
        project_serp_credential_aad(scope, source_key, revision)?;
        let key = project_serp_settings_key(scope, source_key)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        crate::set_local_scope(&mut tx, scope)
            .await
            .map_err(unavailable)?;
        let row = sqlx::query("SELECT encrypted_credentials FROM project_serp_credentials WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_key=$4 AND credential_revision=$5")
            .bind(key.operator_id).bind(key.tenant_id).bind(key.project_id).bind(source_key).bind(revision).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let result = row.map(|row| ProjectSerpCredentialRecord {
            source_key: source_key.into(),
            credential_revision: revision,
            encrypted_credentials: row.get("encrypted_credentials"),
        });
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }
    async fn list_dispatch_sources(
        &self,
        after: Option<ProjectSerpSettingsCursor>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpDispatchSource>, AppError> {
        let limit = limit_value(limit)?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        if let Some(after) = &after {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project_serp_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_key=$4)")
                .bind(after.operator_id).bind(after.tenant_id).bind(after.project_id).bind(&after.source_key).fetch_one(&mut *tx).await.map_err(unavailable)?;
            if !exists {
                return Err(AppError::invalid_request("invalid search settings cursor"));
            }
        }
        let rows = sqlx::query("SELECT operator_id,tenant_id,project_id,source_key FROM project_serp_settings WHERE ($1::uuid IS NULL OR (operator_id,tenant_id,project_id,source_key)>($1,$2,$3,$4)) ORDER BY operator_id,tenant_id,project_id,source_key LIMIT $5")
            .bind(after.as_ref().map(|key| key.operator_id)).bind(after.as_ref().map(|key| key.tenant_id)).bind(after.as_ref().map(|key| key.project_id))
            .bind(after.as_ref().map(|key| &key.source_key)).bind(limit).fetch_all(&mut *tx).await.map_err(unavailable)?;
        let result = rows
            .iter()
            .map(|row| ProjectSerpDispatchSource {
                scope: TenantScope::new(
                    row.get::<uuid::Uuid, _>("operator_id").into(),
                    row.get::<uuid::Uuid, _>("tenant_id").into(),
                    Some(row.get::<uuid::Uuid, _>("project_id").into()),
                ),
                source_key: row.get("source_key"),
            })
            .collect();
        tx.commit().await.map_err(unavailable)?;
        Ok(result)
    }
}
