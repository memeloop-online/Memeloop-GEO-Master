//! Project-owned AI configuration. Credential ciphertext never enters public DTOs.
use crate::{AppError, TenantScope};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAiUsage {
    WorkbenchContent,
    ObservationAnalysis,
}
impl ProjectAiUsage {
    pub const ALL: [Self; 2] = [Self::WorkbenchContent, Self::ObservationAnalysis];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkbenchContent => "workbench_content",
            Self::ObservationAnalysis => "observation_analysis",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAiMode {
    Inherit,
    Custom,
}
#[derive(Clone)]
pub struct ProjectAiSettingsRecord {
    pub usage: ProjectAiUsage,
    pub revision: i64,
    pub mode: ProjectAiMode,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub encrypted_api_key: Option<Vec<u8>>,
    pub prefer_connected_account: bool,
}
impl std::fmt::Debug for ProjectAiSettingsRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProjectAiSettingsRecord([redacted])")
    }
}
impl ProjectAiSettingsRecord {
    pub fn inherited(usage: ProjectAiUsage) -> Self {
        Self {
            usage,
            revision: 0,
            mode: ProjectAiMode::Inherit,
            model: None,
            base_url: None,
            encrypted_api_key: None,
            prefer_connected_account: true,
        }
    }
}
#[async_trait]
pub trait ProjectAiSettingsRepository: Send + Sync {
    async fn get(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
    ) -> Result<ProjectAiSettingsRecord, AppError>;
    async fn save(
        &self,
        scope: &TenantScope,
        expected_revision: i64,
        record: ProjectAiSettingsRecord,
    ) -> Result<ProjectAiSettingsRecord, AppError>;
}
#[derive(Default)]
pub struct MemoryProjectAiSettingsRepository {
    rows: Mutex<HashMap<String, ProjectAiSettingsRecord>>,
}
pub fn project_ai_scope_key(
    scope: &TenantScope,
    usage: ProjectAiUsage,
) -> Result<String, AppError> {
    let project = scope
        .project_id
        .ok_or_else(|| AppError::invalid_request("project scope required"))?;
    Ok(format!(
        "geo-project-ai-v1:{}:{}:{}:{}",
        scope.operator_id.as_uuid(),
        scope.tenant_id.as_uuid(),
        project.as_uuid(),
        usage.as_str()
    ))
}
#[async_trait]
impl ProjectAiSettingsRepository for MemoryProjectAiSettingsRepository {
    async fn get(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
    ) -> Result<ProjectAiSettingsRecord, AppError> {
        let key = project_ai_scope_key(scope, usage)?;
        Ok(self
            .rows
            .lock()
            .expect("settings lock")
            .get(&key)
            .cloned()
            .unwrap_or_else(|| ProjectAiSettingsRecord::inherited(usage)))
    }
    async fn save(
        &self,
        scope: &TenantScope,
        expected: i64,
        mut record: ProjectAiSettingsRecord,
    ) -> Result<ProjectAiSettingsRecord, AppError> {
        let key = project_ai_scope_key(scope, record.usage)?;
        let mut rows = self.rows.lock().expect("settings lock");
        if expected < 0
            || expected == i64::MAX
            || rows.get(&key).map_or(0, |r| r.revision) != expected
        {
            return Err(AppError::conflict("AI settings revision changed"));
        }
        record.revision = expected + 1;
        rows.insert(key, record.clone());
        Ok(record)
    }
}
