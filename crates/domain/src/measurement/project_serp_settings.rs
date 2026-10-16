//! Project search-source configuration and independently versioned ciphertext.
use crate::{AppError, SerpMeasurement, SerpProtocol, TenantScope};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSerpProvider {
    Dataforseo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSerpSettingsRecord {
    pub source_key: String,
    pub provider: ProjectSerpProvider,
    pub revision: i64,
    pub enabled: bool,
    pub protocol_defaults: SerpProtocol,
    pub active_credential_revision: Option<i64>,
}

#[derive(Clone)]
pub struct ProjectSerpSettingsWrite {
    pub source_key: String,
    pub provider: ProjectSerpProvider,
    pub enabled: bool,
    pub protocol_defaults: SerpProtocol,
    /// None preserves the current credential revision; Some creates revision
    /// expected_revision+1. Both login and password are encrypted together.
    pub encrypted_credentials: Option<Vec<u8>>,
}
impl std::fmt::Debug for ProjectSerpSettingsWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProjectSerpSettingsWrite([redacted])")
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct ProjectSerpCredentialRecord {
    pub source_key: String,
    pub credential_revision: i64,
    pub encrypted_credentials: Vec<u8>,
}
impl std::fmt::Debug for ProjectSerpCredentialRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProjectSerpCredentialRecord([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectSerpSettingsCursor {
    pub operator_id: Uuid,
    pub tenant_id: Uuid,
    pub project_id: Uuid,
    pub source_key: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSerpDispatchSource {
    pub scope: TenantScope,
    pub source_key: String,
}
impl ProjectSerpDispatchSource {
    pub fn cursor(&self) -> Result<ProjectSerpSettingsCursor, AppError> {
        project_serp_settings_key(&self.scope, &self.source_key)
    }
}

pub fn project_serp_settings_key(
    scope: &TenantScope,
    source_key: &str,
) -> Result<ProjectSerpSettingsCursor, AppError> {
    let project = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    if source_key.trim().is_empty()
        || source_key.len() > 128
        || source_key.chars().any(char::is_control)
    {
        return Err(AppError::invalid_request("invalid search source key"));
    }
    Ok(ProjectSerpSettingsCursor {
        operator_id: scope.operator_id.as_uuid(),
        tenant_id: scope.tenant_id.as_uuid(),
        project_id: project.as_uuid(),
        source_key: source_key.into(),
    })
}

/// Authenticated encryption context. JSON encoding prevents delimiter ambiguity.
pub fn project_serp_credential_aad(
    scope: &TenantScope,
    source_key: &str,
    credential_revision: i64,
) -> Result<Vec<u8>, AppError> {
    let key = project_serp_settings_key(scope, source_key)?;
    if credential_revision <= 0 {
        return Err(AppError::invalid_request(
            "invalid search credential revision",
        ));
    }
    serde_json::to_vec(&(
        "geo.project-serp.credentials.v1",
        key.operator_id,
        key.tenant_id,
        key.project_id,
        key.source_key,
        credential_revision,
    ))
    .map_err(|_| AppError::invalid_request("invalid search credential context"))
}

impl ProjectSerpSettingsWrite {
    pub fn validate(&self, scope: &TenantScope, expected: i64) -> Result<(), AppError> {
        project_serp_settings_key(scope, &self.source_key)?;
        if expected < 0 || expected == i64::MAX {
            return Err(AppError::conflict("search settings revision changed"));
        }
        if !self.protocol_defaults.query.is_empty()
            || self.protocol_defaults.source != "dataforseo"
            || self
                .encrypted_credentials
                .as_ref()
                .is_some_and(|bytes| bytes.is_empty() || bytes.len() > 16384)
        {
            return Err(AppError::invalid_request("invalid search settings"));
        }
        let mut protocol = self.protocol_defaults.clone();
        protocol.query = "protocol validation".into();
        protocol.validate()?;
        Ok(())
    }
    pub fn apply(
        &self,
        expected: i64,
        prior: Option<&ProjectSerpSettingsRecord>,
    ) -> Result<ProjectSerpSettingsRecord, AppError> {
        if expected < 0 || expected == i64::MAX || prior.map_or(0, |row| row.revision) != expected {
            return Err(AppError::conflict("search settings revision changed"));
        }
        let credential_revision = if self.encrypted_credentials.is_some() {
            Some(expected + 1)
        } else {
            prior.and_then(|row| row.active_credential_revision)
        };
        if self.enabled && credential_revision.is_none() {
            return Err(AppError::invalid_request("search credentials required"));
        }
        Ok(ProjectSerpSettingsRecord {
            source_key: self.source_key.clone(),
            provider: self.provider,
            revision: expected + 1,
            enabled: self.enabled,
            protocol_defaults: self.protocol_defaults.clone(),
            active_credential_revision: credential_revision,
        })
    }
}

#[async_trait]
pub trait ProjectSerpSettingsRepository: Send + Sync {
    async fn get(
        &self,
        scope: &TenantScope,
        source_key: &str,
    ) -> Result<Option<ProjectSerpSettingsRecord>, AppError>;
    async fn list(
        &self,
        scope: &TenantScope,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpSettingsRecord>, AppError>;
    async fn save(
        &self,
        scope: &TenantScope,
        expected_revision: i64,
        write: ProjectSerpSettingsWrite,
    ) -> Result<ProjectSerpSettingsRecord, AppError>;
    /// Exact immutable credential version, including disabled sources. Only
    /// trusted server adapters use this; no ciphertext in public settings DTOs.
    async fn get_credential(
        &self,
        scope: &TenantScope,
        source_key: &str,
        credential_revision: i64,
    ) -> Result<Option<ProjectSerpCredentialRecord>, AppError>;
    /// Bounded trusted dispatcher inventory includes disabled sources, whose
    /// already-submitted provider tasks may still require read-only recovery.
    async fn list_dispatch_sources(
        &self,
        after: Option<ProjectSerpSettingsCursor>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpDispatchSource>, AppError>;
}

#[derive(Default)]
pub struct MemoryProjectSerpSettingsRepository {
    data: Mutex<MemoryData>,
    send_consistency: Arc<tokio::sync::RwLock<()>>,
}
impl MemoryProjectSerpSettingsRepository {
    /// Internal shared consistency lock for the in-memory persistence adapter.
    /// Settings mutations take it before the settings mutex; first-send intents
    /// hold a read lock through their own atomic state transition.
    pub fn send_consistency_gate(&self) -> Arc<tokio::sync::RwLock<()>> {
        self.send_consistency.clone()
    }
}

pub fn validate_project_serp_send(
    record: Option<&ProjectSerpSettingsRecord>,
    measurement: &SerpMeasurement,
    credential_revision: i64,
) -> Result<(), AppError> {
    let record = record.ok_or_else(|| AppError::not_ready("search source unavailable"))?;
    if !record.enabled
        || record.source_key != measurement.source_key
        || record.active_credential_revision != Some(credential_revision)
        || measurement.protocol.source != "dataforseo"
    {
        return Err(AppError::conflict("search source configuration changed"));
    }
    Ok(())
}
#[derive(Default)]
struct MemoryData {
    rows: BTreeMap<ProjectSerpSettingsCursor, ProjectSerpSettingsRecord>,
    credentials: BTreeMap<(ProjectSerpSettingsCursor, i64), ProjectSerpCredentialRecord>,
}
fn limit_valid(limit: usize) -> Result<(), AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request(
            "invalid search settings page size",
        ));
    }
    Ok(())
}
#[async_trait]
impl ProjectSerpSettingsRepository for MemoryProjectSerpSettingsRepository {
    async fn get(
        &self,
        scope: &TenantScope,
        source_key: &str,
    ) -> Result<Option<ProjectSerpSettingsRecord>, AppError> {
        let key = project_serp_settings_key(scope, source_key)?;
        Ok(self
            .data
            .lock()
            .expect("search settings lock")
            .rows
            .get(&key)
            .cloned())
    }
    async fn list(
        &self,
        scope: &TenantScope,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpSettingsRecord>, AppError> {
        limit_valid(limit)?;
        let prefix = project_serp_settings_key(scope, "scope")?;
        let data = self.data.lock().expect("search settings lock");
        if let Some(after) = &after
            && !data
                .rows
                .contains_key(&project_serp_settings_key(scope, after)?)
        {
            return Err(AppError::invalid_request("invalid search settings cursor"));
        }
        Ok(data
            .rows
            .iter()
            .filter(|(key, _)| {
                key.operator_id == prefix.operator_id
                    && key.tenant_id == prefix.tenant_id
                    && key.project_id == prefix.project_id
                    && after.as_ref().is_none_or(|after| key.source_key > *after)
            })
            .take(limit)
            .map(|(_, row)| row.clone())
            .collect())
    }
    async fn save(
        &self,
        scope: &TenantScope,
        expected: i64,
        write: ProjectSerpSettingsWrite,
    ) -> Result<ProjectSerpSettingsRecord, AppError> {
        write.validate(scope, expected)?;
        let key = project_serp_settings_key(scope, &write.source_key)?;
        let _send_consistency = self.send_consistency.write().await;
        let mut data = self.data.lock().expect("search settings lock");
        let row = write.apply(expected, data.rows.get(&key))?;
        if let Some(bytes) = write.encrypted_credentials {
            data.credentials.insert(
                (key.clone(), expected + 1),
                ProjectSerpCredentialRecord {
                    source_key: row.source_key.clone(),
                    credential_revision: expected + 1,
                    encrypted_credentials: bytes,
                },
            );
        }
        data.rows.insert(key, row.clone());
        Ok(row)
    }
    async fn get_credential(
        &self,
        scope: &TenantScope,
        source_key: &str,
        revision: i64,
    ) -> Result<Option<ProjectSerpCredentialRecord>, AppError> {
        project_serp_credential_aad(scope, source_key, revision)?;
        let key = project_serp_settings_key(scope, source_key)?;
        Ok(self
            .data
            .lock()
            .expect("search settings lock")
            .credentials
            .get(&(key, revision))
            .cloned())
    }
    async fn list_dispatch_sources(
        &self,
        after: Option<ProjectSerpSettingsCursor>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpDispatchSource>, AppError> {
        limit_valid(limit)?;
        let data = self.data.lock().expect("search settings lock");
        if after
            .as_ref()
            .is_some_and(|after| !data.rows.contains_key(after))
        {
            return Err(AppError::invalid_request("invalid search settings cursor"));
        }
        Ok(data
            .rows
            .keys()
            .filter(|key| after.as_ref().is_none_or(|after| *key > after))
            .take(limit)
            .map(|key| ProjectSerpDispatchSource {
                scope: TenantScope::new(
                    key.operator_id.into(),
                    key.tenant_id.into(),
                    Some(key.project_id.into()),
                ),
                source_key: key.source_key.clone(),
            })
            .collect())
    }
}
