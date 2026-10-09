use async_trait::async_trait;
use geo_domain::{
    AppError, ProjectAiMode, ProjectAiSettingsRecord, ProjectAiSettingsRepository, ProjectAiUsage,
    TenantScope,
};
use sqlx::{PgPool, Row};

#[derive(Clone)]
pub struct PgProjectAiSettingsRepository {
    pool: PgPool,
}
impl PgProjectAiSettingsRepository {
    pub fn from_database(db: &crate::Database) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }
}
fn unavailable(_: sqlx::Error) -> AppError {
    AppError::not_ready("AI settings storage unavailable")
}
#[async_trait]
impl ProjectAiSettingsRepository for PgProjectAiSettingsRepository {
    async fn get(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
    ) -> Result<ProjectAiSettingsRecord, AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?;
        let row = sqlx::query("SELECT revision,mode,model,base_url,encrypted_api_key,prefer_connected_account FROM project_ai_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND usage=$4")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(usage.as_str()).fetch_optional(&self.pool).await.map_err(unavailable)?;
        Ok(match row {
            None => ProjectAiSettingsRecord::inherited(usage),
            Some(row) => ProjectAiSettingsRecord {
                usage,
                revision: row.get("revision"),
                mode: if row.get::<String, _>("mode") == "custom" {
                    ProjectAiMode::Custom
                } else {
                    ProjectAiMode::Inherit
                },
                model: row.get("model"),
                base_url: row.get("base_url"),
                encrypted_api_key: row.get("encrypted_api_key"),
                prefer_connected_account: row.get("prefer_connected_account"),
            },
        })
    }
    async fn save(
        &self,
        scope: &TenantScope,
        expected: i64,
        mut r: ProjectAiSettingsRecord,
    ) -> Result<ProjectAiSettingsRecord, AppError> {
        let project = scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?;
        if expected < 0 || expected == i64::MAX {
            return Err(AppError::conflict("AI settings revision changed"));
        }
        let result = sqlx::query("INSERT INTO project_ai_settings (operator_id,tenant_id,project_id,usage,revision,mode,model,base_url,encrypted_api_key,prefer_connected_account) SELECT $1,$2,$3,$4,$5+1,$6,$7,$8,$9,$10 WHERE $5=0 OR EXISTS (SELECT 1 FROM project_ai_settings WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND usage=$4) ON CONFLICT (operator_id,tenant_id,project_id,usage) DO UPDATE SET revision=EXCLUDED.revision,mode=EXCLUDED.mode,model=EXCLUDED.model,base_url=EXCLUDED.base_url,encrypted_api_key=EXCLUDED.encrypted_api_key,prefer_connected_account=EXCLUDED.prefer_connected_account WHERE project_ai_settings.revision=$5")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(r.usage.as_str()).bind(expected).bind(if r.mode == ProjectAiMode::Custom {"custom"} else {"inherit"}).bind(&r.model).bind(&r.base_url).bind(&r.encrypted_api_key).bind(r.prefer_connected_account).execute(&self.pool).await.map_err(unavailable)?;
        if result.rows_affected() != 1 {
            return Err(AppError::conflict("AI settings revision changed"));
        }
        r.revision = expected + 1;
        Ok(r)
    }
}
