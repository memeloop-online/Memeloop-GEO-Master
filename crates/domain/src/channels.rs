//! Project-scoped channel accounts. Secret material is deliberately excluded
//! from every serializable/public type.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, ProjectId, TenantScope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStatus {
    NeedsLogin,
    Unverified,
    Ready,
    Disabled,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelOwnerKind {
    Customer,
    OperatorPool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelGroup {
    pub group_id: Uuid,
    pub project_id: ProjectId,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelAccount {
    pub account_id: Uuid,
    pub project_id: ProjectId,
    pub owner_kind: ChannelOwnerKind,
    pub platform: String,
    pub group_id: Option<Uuid>,
    pub status: ChannelStatus,
    pub display_name: Option<String>,
    pub platform_account_id: Option<String>,
    pub avatar_url: Option<String>,
    pub enabled: bool,
    pub proxy_configured: bool,
    pub proxy_server: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PoolGroup {
    pub group_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PoolAccount {
    pub account_id: Uuid,
    pub platform: String,
    pub group_id: Option<Uuid>,
    pub status: ChannelStatus,
    pub display_name: Option<String>,
    pub platform_account_id: Option<String>,
    pub avatar_url: Option<String>,
    pub enabled: bool,
    pub proxy_configured: bool,
    pub proxy_server: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct PoolAccountRecord {
    pub account: PoolAccount,
    pub session: Option<ChannelSecret>,
    pub proxy: Option<ChannelSecret>,
}

#[derive(Clone)]
pub struct PoolLoginSession {
    pub session_id: Uuid,
    pub account_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PoolAssignment {
    pub tenant_id: crate::TenantId,
    pub project_id: ProjectId,
}

impl PoolAccount {
    pub fn assigned_view(&self, project_id: ProjectId) -> ChannelAccount {
        ChannelAccount {
            account_id: self.account_id,
            project_id,
            owner_kind: ChannelOwnerKind::OperatorPool,
            platform: self.platform.clone(),
            group_id: self.group_id,
            status: self.status,
            display_name: self.display_name.clone(),
            platform_account_id: self.platform_account_id.clone(),
            avatar_url: self.avatar_url.clone(),
            enabled: self.enabled,
            proxy_configured: self.proxy_configured,
            proxy_server: None, // operator infrastructure stays private
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelSettings {
    pub project_id: ProjectId,
    pub default_group_id: Option<Uuid>,
    pub proxy_configured: bool,
    pub proxy_server: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Opaque AEAD envelope. Neither Debug nor Serialize is implemented to avoid
/// accidental tracing or inclusion in public JSON. Only the repository and
/// trusted server-side browser bridge handle its bytes.
#[derive(Clone)]
pub struct ChannelSecret(Vec<u8>);

impl ChannelSecret {
    pub fn new(encrypted: Vec<u8>) -> Self {
        Self(encrypted)
    }

    pub fn encrypted_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone)]
pub struct ChannelAccountRecord {
    pub account: ChannelAccount,
    pub session: Option<ChannelSecret>,
    pub proxy: Option<ChannelSecret>,
}

/// Server-only version of the exact encrypted state restored into a browser.
/// Renewals change only the session, never account metadata or ownership.
#[derive(Clone)]
pub struct ChannelSessionVersion {
    pub account_id: Uuid,
    pub owner_kind: ChannelOwnerKind,
    pub platform: String,
    pub platform_account_id: String,
    pub session: ChannelSecret,
}

#[derive(Clone)]
pub struct ChannelSettingsRecord {
    pub settings: ChannelSettings,
    pub proxy: Option<ChannelSecret>,
}

#[derive(Clone)]
pub struct LoginSession {
    pub session_id: Uuid,
    pub account_id: Uuid,
    pub project_id: ProjectId,
    pub created_at: DateTime<Utc>,
}

pub fn supported_channel(platform: &str) -> bool {
    matches!(platform, "zhihu" | "baidu_creator" | "xiaohongshu") || consumer_web_provider(platform)
}

/// Registered account namespaces, not a claim of installed login or measurement
/// support. Those capabilities belong to the authenticated running adapter.
pub fn consumer_web_provider(platform: &str) -> bool {
    matches!(platform, "kimi" | "doubao" | "deepseek" | "glm")
}

fn project(scope: &TenantScope) -> Result<ProjectId, AppError> {
    scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope is required"))
}

#[async_trait]
pub trait ChannelRepository: Send + Sync {
    /// Metadata edits must not restore a session read before a renewal.
    async fn save_account_metadata(
        &self,
        scope: &TenantScope,
        record: ChannelAccountRecord,
    ) -> Result<ChannelAccount, AppError>;
    async fn save_pool_account_metadata(
        &self,
        operator: crate::OperatorId,
        record: PoolAccountRecord,
    ) -> Result<PoolAccount, AppError>;
    async fn renew_session(
        &self,
        scope: &TenantScope,
        expected: &ChannelSessionVersion,
        renewed: ChannelSecret,
    ) -> Result<bool, AppError>;
    async fn list_pool_groups(
        &self,
        operator: crate::OperatorId,
    ) -> Result<Vec<PoolGroup>, AppError>;
    async fn save_pool_group(
        &self,
        operator: crate::OperatorId,
        group: PoolGroup,
    ) -> Result<PoolGroup, AppError>;
    async fn delete_pool_group(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<(), AppError>;
    async fn list_pool_accounts(
        &self,
        operator: crate::OperatorId,
    ) -> Result<Vec<PoolAccount>, AppError>;
    async fn get_pool_account(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<PoolAccountRecord, AppError>;
    async fn save_pool_account(
        &self,
        operator: crate::OperatorId,
        record: PoolAccountRecord,
    ) -> Result<PoolAccount, AppError>;
    async fn delete_pool_account(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<(), AppError>;
    async fn assign_pool_account(
        &self,
        scope: &TenantScope,
        id: Uuid,
        assigned: bool,
    ) -> Result<(), AppError>;
    async fn list_assigned_pool_accounts(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<PoolAccount>, AppError>;
    async fn list_pool_assignments(
        &self,
        operator: crate::OperatorId,
        account_id: Uuid,
    ) -> Result<Vec<PoolAssignment>, AppError>;
    async fn save_pool_login(
        &self,
        operator: crate::OperatorId,
        session: PoolLoginSession,
    ) -> Result<(), AppError>;
    async fn get_pool_login(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<PoolLoginSession, AppError>;
    async fn delete_pool_login(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<(), AppError>;
    async fn get_settings(
        &self,
        scope: &TenantScope,
    ) -> Result<Option<ChannelSettingsRecord>, AppError>;
    async fn save_settings(
        &self,
        scope: &TenantScope,
        settings: ChannelSettingsRecord,
    ) -> Result<ChannelSettings, AppError>;
    async fn list_groups(&self, scope: &TenantScope) -> Result<Vec<ChannelGroup>, AppError>;
    async fn save_group(
        &self,
        scope: &TenantScope,
        group: ChannelGroup,
    ) -> Result<ChannelGroup, AppError>;
    async fn delete_group(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError>;
    async fn list_accounts(&self, scope: &TenantScope) -> Result<Vec<ChannelAccount>, AppError>;
    async fn get_account(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<ChannelAccountRecord, AppError>;
    async fn save_account(
        &self,
        scope: &TenantScope,
        record: ChannelAccountRecord,
    ) -> Result<ChannelAccount, AppError>;
    async fn delete_account(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError>;
    async fn save_login(&self, scope: &TenantScope, session: LoginSession) -> Result<(), AppError>;
    async fn get_login(&self, scope: &TenantScope, id: Uuid) -> Result<LoginSession, AppError>;
    async fn delete_login(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError>;
}

#[derive(Default)]
struct ChannelMemory {
    pool_groups: HashMap<(Uuid, Uuid), PoolGroup>,
    pool_accounts: HashMap<(Uuid, Uuid), PoolAccountRecord>,
    pool_logins: HashMap<(Uuid, Uuid), PoolLoginSession>,
    pool_assignments: std::collections::HashSet<(Uuid, Uuid, Uuid, Uuid)>,
    settings: HashMap<(Uuid, Uuid, Uuid), ChannelSettingsRecord>,
    groups: HashMap<(Uuid, Uuid, Uuid, Uuid), ChannelGroup>,
    accounts: HashMap<(Uuid, Uuid, Uuid, Uuid), ChannelAccountRecord>,
    logins: HashMap<(Uuid, Uuid, Uuid, Uuid), LoginSession>,
}

#[derive(Clone, Default)]
pub struct MemoryChannelRepository(Arc<RwLock<ChannelMemory>>);

fn key(scope: &TenantScope, id: Uuid) -> Result<(Uuid, Uuid, Uuid, Uuid), AppError> {
    Ok((
        scope.operator_id.as_uuid(),
        scope.tenant_id.as_uuid(),
        project(scope)?.as_uuid(),
        id,
    ))
}

#[async_trait]
impl ChannelRepository for MemoryChannelRepository {
    async fn save_account_metadata(
        &self,
        scope: &TenantScope,
        record: ChannelAccountRecord,
    ) -> Result<ChannelAccount, AppError> {
        let account_key = key(scope, record.account.account_id)?;
        let mut data = self.0.write().await;
        if Some(record.account.project_id) != scope.project_id {
            return Err(AppError::forbidden("account outside project"));
        }
        if let Some(group) = record.account.group_id
            && !data.groups.contains_key(&key(scope, group)?)
        {
            return Err(AppError::not_found("group not found"));
        }
        let current = data
            .accounts
            .get_mut(&account_key)
            .ok_or_else(|| AppError::not_found("account not found"))?;
        if current.session.as_ref().map(ChannelSecret::encrypted_bytes)
            != record.session.as_ref().map(ChannelSecret::encrypted_bytes)
            || current.account.platform != record.account.platform
            || current.account.platform_account_id != record.account.platform_account_id
            || current.account.owner_kind != record.account.owner_kind
        {
            return Err(AppError::conflict(
                "channel account changed; retry the edit",
            ));
        }
        current.account = record.account;
        current.proxy = record.proxy;
        Ok(current.account.clone())
    }

    async fn save_pool_account_metadata(
        &self,
        operator: crate::OperatorId,
        record: PoolAccountRecord,
    ) -> Result<PoolAccount, AppError> {
        let mut data = self.0.write().await;
        if let Some(group) = record.account.group_id
            && !data.pool_groups.contains_key(&(operator.as_uuid(), group))
        {
            return Err(AppError::not_found("pool group not found"));
        }
        let current = data
            .pool_accounts
            .get_mut(&(operator.as_uuid(), record.account.account_id))
            .ok_or_else(|| AppError::not_found("pool account not found"))?;
        if current.session.as_ref().map(ChannelSecret::encrypted_bytes)
            != record.session.as_ref().map(ChannelSecret::encrypted_bytes)
            || current.account.platform != record.account.platform
            || current.account.platform_account_id != record.account.platform_account_id
        {
            return Err(AppError::conflict(
                "channel account changed; retry the edit",
            ));
        }
        current.account = record.account;
        current.proxy = record.proxy;
        Ok(current.account.clone())
    }

    async fn renew_session(
        &self,
        scope: &TenantScope,
        expected: &ChannelSessionVersion,
        renewed: ChannelSecret,
    ) -> Result<bool, AppError> {
        let account_key = key(scope, expected.account_id)?;
        let mut data = self.0.write().await;
        if expected.platform_account_id.trim().is_empty() {
            return Ok(false);
        }
        let (platform, identity, enabled, status, session) = match expected.owner_kind {
            ChannelOwnerKind::Customer => {
                let Some(record) = data.accounts.get_mut(&account_key) else {
                    return Ok(false);
                };
                if record.account.owner_kind != ChannelOwnerKind::Customer {
                    return Ok(false);
                }
                (
                    &record.account.platform,
                    &record.account.platform_account_id,
                    record.account.enabled,
                    record.account.status,
                    &mut record.session,
                )
            }
            ChannelOwnerKind::OperatorPool => {
                if !data.pool_assignments.contains(&account_key) {
                    return Ok(false);
                }
                let Some(record) = data.pool_accounts.get_mut(&(account_key.0, account_key.3))
                else {
                    return Ok(false);
                };
                (
                    &record.account.platform,
                    &record.account.platform_account_id,
                    record.account.enabled,
                    record.account.status,
                    &mut record.session,
                )
            }
        };
        if !enabled
            || status != ChannelStatus::Ready
            || platform != &expected.platform
            || identity.as_deref() != Some(expected.platform_account_id.as_str())
            || session.as_ref().map(ChannelSecret::encrypted_bytes)
                != Some(expected.session.encrypted_bytes())
        {
            return Ok(false);
        }
        *session = Some(renewed);
        Ok(true)
    }

    async fn list_pool_groups(
        &self,
        operator: crate::OperatorId,
    ) -> Result<Vec<PoolGroup>, AppError> {
        Ok(self
            .0
            .read()
            .await
            .pool_groups
            .iter()
            .filter(|((o, _), _)| *o == operator.as_uuid())
            .map(|(_, group)| group.clone())
            .collect())
    }

    async fn save_pool_group(
        &self,
        operator: crate::OperatorId,
        group: PoolGroup,
    ) -> Result<PoolGroup, AppError> {
        self.0
            .write()
            .await
            .pool_groups
            .insert((operator.as_uuid(), group.group_id), group.clone());
        Ok(group)
    }

    async fn delete_pool_group(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<(), AppError> {
        let mut data = self.0.write().await;
        if data.pool_groups.remove(&(operator.as_uuid(), id)).is_none() {
            return Err(AppError::not_found("pool group not found"));
        }
        for ((o, _), record) in data.pool_accounts.iter_mut() {
            if *o == operator.as_uuid() && record.account.group_id == Some(id) {
                record.account.group_id = None;
            }
        }
        Ok(())
    }

    async fn list_pool_accounts(
        &self,
        operator: crate::OperatorId,
    ) -> Result<Vec<PoolAccount>, AppError> {
        Ok(self
            .0
            .read()
            .await
            .pool_accounts
            .iter()
            .filter(|((o, _), _)| *o == operator.as_uuid())
            .map(|(_, record)| record.account.clone())
            .collect())
    }

    async fn get_pool_account(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<PoolAccountRecord, AppError> {
        self.0
            .read()
            .await
            .pool_accounts
            .get(&(operator.as_uuid(), id))
            .cloned()
            .ok_or_else(|| AppError::not_found("pool account not found"))
    }

    async fn save_pool_account(
        &self,
        operator: crate::OperatorId,
        record: PoolAccountRecord,
    ) -> Result<PoolAccount, AppError> {
        let mut data = self.0.write().await;
        if let Some(group) = record.account.group_id
            && !data.pool_groups.contains_key(&(operator.as_uuid(), group))
        {
            return Err(AppError::not_found("pool group not found"));
        }
        if record.account.platform_account_id.is_some()
            && (data.pool_accounts.iter().any(|((o, id), prior)| {
                *o == operator.as_uuid()
                    && *id != record.account.account_id
                    && prior.account.platform == record.account.platform
                    && prior.account.platform_account_id == record.account.platform_account_id
            }) || data.accounts.iter().any(|((o, _, _, _), prior)| {
                *o == operator.as_uuid()
                    && prior.account.platform == record.account.platform
                    && prior.account.platform_account_id == record.account.platform_account_id
            }))
        {
            return Err(AppError::conflict("platform identity is already connected"));
        }
        let account = record.account.clone();
        data.pool_accounts
            .insert((operator.as_uuid(), account.account_id), record);
        Ok(account)
    }

    async fn delete_pool_account(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<(), AppError> {
        let mut data = self.0.write().await;
        if data
            .pool_accounts
            .remove(&(operator.as_uuid(), id))
            .is_none()
        {
            return Err(AppError::not_found("pool account not found"));
        }
        data.pool_assignments
            .retain(|(o, _, _, account)| *o != operator.as_uuid() || *account != id);
        data.pool_logins
            .retain(|(o, _), login| *o != operator.as_uuid() || login.account_id != id);
        Ok(())
    }

    async fn assign_pool_account(
        &self,
        scope: &TenantScope,
        id: Uuid,
        assigned: bool,
    ) -> Result<(), AppError> {
        let project_id = project(scope)?;
        let mut data = self.0.write().await;
        if !data
            .pool_accounts
            .contains_key(&(scope.operator_id.as_uuid(), id))
        {
            return Err(AppError::not_found("pool account not found"));
        }
        let key = (
            scope.operator_id.as_uuid(),
            scope.tenant_id.as_uuid(),
            project_id.as_uuid(),
            id,
        );
        if assigned {
            data.pool_assignments.insert(key);
        } else {
            data.pool_assignments.remove(&key);
        }
        Ok(())
    }

    async fn list_assigned_pool_accounts(
        &self,
        scope: &TenantScope,
    ) -> Result<Vec<PoolAccount>, AppError> {
        let project_id = project(scope)?;
        let data = self.0.read().await;
        Ok(data
            .pool_assignments
            .iter()
            .filter(|(o, t, p, _)| {
                (*o, *t, *p)
                    == (
                        scope.operator_id.as_uuid(),
                        scope.tenant_id.as_uuid(),
                        project_id.as_uuid(),
                    )
            })
            .filter_map(|(o, _, _, id)| data.pool_accounts.get(&(*o, *id)))
            .map(|record| record.account.clone())
            .collect())
    }

    async fn list_pool_assignments(
        &self,
        operator: crate::OperatorId,
        account_id: Uuid,
    ) -> Result<Vec<PoolAssignment>, AppError> {
        let data = self.0.read().await;
        if !data
            .pool_accounts
            .contains_key(&(operator.as_uuid(), account_id))
        {
            return Err(AppError::not_found("pool account not found"));
        }
        Ok(data
            .pool_assignments
            .iter()
            .filter(|(o, _, _, a)| *o == operator.as_uuid() && *a == account_id)
            .map(|(_, tenant, project, _)| PoolAssignment {
                tenant_id: (*tenant).into(),
                project_id: (*project).into(),
            })
            .collect())
    }

    async fn save_pool_login(
        &self,
        operator: crate::OperatorId,
        session: PoolLoginSession,
    ) -> Result<(), AppError> {
        let mut data = self.0.write().await;
        if !data
            .pool_accounts
            .contains_key(&(operator.as_uuid(), session.account_id))
        {
            return Err(AppError::not_found("pool account not found"));
        }
        data.pool_logins
            .insert((operator.as_uuid(), session.session_id), session);
        Ok(())
    }

    async fn get_pool_login(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<PoolLoginSession, AppError> {
        self.0
            .read()
            .await
            .pool_logins
            .get(&(operator.as_uuid(), id))
            .cloned()
            .ok_or_else(|| AppError::not_found("pool login not found"))
    }

    async fn delete_pool_login(
        &self,
        operator: crate::OperatorId,
        id: Uuid,
    ) -> Result<(), AppError> {
        if self
            .0
            .write()
            .await
            .pool_logins
            .remove(&(operator.as_uuid(), id))
            .is_none()
        {
            return Err(AppError::not_found("pool login not found"));
        }
        Ok(())
    }

    async fn get_settings(
        &self,
        scope: &TenantScope,
    ) -> Result<Option<ChannelSettingsRecord>, AppError> {
        let (o, t, p, _) = key(scope, Uuid::nil())?;
        Ok(self.0.read().await.settings.get(&(o, t, p)).cloned())
    }

    async fn save_settings(
        &self,
        scope: &TenantScope,
        settings: ChannelSettingsRecord,
    ) -> Result<ChannelSettings, AppError> {
        if scope.project_id != Some(settings.settings.project_id) {
            return Err(AppError::forbidden("channel settings outside project"));
        }
        let (o, t, p, _) = key(scope, Uuid::nil())?;
        let mut data = self.0.write().await;
        if let Some(group) = settings.settings.default_group_id
            && !data.groups.contains_key(&(o, t, p, group))
        {
            return Err(AppError::not_found("default group not found"));
        }
        let public = settings.settings.clone();
        data.settings.insert((o, t, p), settings);
        Ok(public)
    }

    async fn list_groups(&self, scope: &TenantScope) -> Result<Vec<ChannelGroup>, AppError> {
        let (o, t, p, _) = key(scope, Uuid::nil())?;
        let mut groups: Vec<_> = self
            .0
            .read()
            .await
            .groups
            .iter()
            .filter(|((operator, tenant, project, _), _)| {
                (*operator, *tenant, *project) == (o, t, p)
            })
            .map(|(_, group)| group.clone())
            .collect();
        groups.sort_by_key(|group| (group.created_at, group.group_id));
        Ok(groups)
    }

    async fn save_group(
        &self,
        scope: &TenantScope,
        group: ChannelGroup,
    ) -> Result<ChannelGroup, AppError> {
        if Some(group.project_id) != scope.project_id {
            return Err(AppError::forbidden("group outside project"));
        }
        let mut data = self.0.write().await;
        let group_key = key(scope, group.group_id)?;
        if !data.groups.contains_key(&group_key)
            && data
                .groups
                .keys()
                .any(|(_, _, _, id)| *id == group.group_id)
        {
            return Err(AppError::conflict(
                "group identifier exists in another scope",
            ));
        }
        data.groups.insert(group_key, group.clone());
        Ok(group)
    }

    async fn delete_group(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let mut data = self.0.write().await;
        let group_key = key(scope, id)?;
        if data.groups.remove(&group_key).is_none() {
            return Err(AppError::not_found("group not found"));
        }
        for ((o, t, p, _), record) in data.accounts.iter_mut() {
            if (*o, *t, *p) == (group_key.0, group_key.1, group_key.2)
                && record.account.group_id == Some(id)
            {
                record.account.group_id = None;
            }
        }
        if let Some(settings) = data
            .settings
            .get_mut(&(group_key.0, group_key.1, group_key.2))
            && settings.settings.default_group_id == Some(id)
        {
            settings.settings.default_group_id = None;
        }
        Ok(())
    }

    async fn list_accounts(&self, scope: &TenantScope) -> Result<Vec<ChannelAccount>, AppError> {
        let (o, t, p, _) = key(scope, Uuid::nil())?;
        let mut accounts: Vec<_> = self
            .0
            .read()
            .await
            .accounts
            .iter()
            .filter(|((operator, tenant, project, _), _)| {
                (*operator, *tenant, *project) == (o, t, p)
            })
            .map(|(_, record)| record.account.clone())
            .collect();
        accounts.sort_by_key(|account| (account.created_at, account.account_id));
        Ok(accounts)
    }

    async fn get_account(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<ChannelAccountRecord, AppError> {
        self.0
            .read()
            .await
            .accounts
            .get(&key(scope, id)?)
            .cloned()
            .ok_or_else(|| AppError::not_found("account not found"))
    }

    async fn save_account(
        &self,
        scope: &TenantScope,
        record: ChannelAccountRecord,
    ) -> Result<ChannelAccount, AppError> {
        if Some(record.account.project_id) != scope.project_id {
            return Err(AppError::forbidden("account outside project"));
        }
        let mut data = self.0.write().await;
        if let Some(group_id) = record.account.group_id
            && !data.groups.contains_key(&key(scope, group_id)?)
        {
            return Err(AppError::not_found("group not found"));
        }
        let account_key = key(scope, record.account.account_id)?;
        if data.accounts.iter().any(|((o, t, p, id), existing)| {
            (*o, *t) == (account_key.0, account_key.1)
                && (*p, *id) != (account_key.2, account_key.3)
                && existing.account.platform == record.account.platform
                && existing.account.platform_account_id.is_some()
                && existing.account.platform_account_id == record.account.platform_account_id
        }) {
            return Err(AppError::conflict("platform identity is already connected"));
        }
        if record.account.platform_account_id.is_some()
            && data.pool_accounts.iter().any(|((o, _), prior)| {
                *o == account_key.0
                    && prior.account.platform == record.account.platform
                    && prior.account.platform_account_id == record.account.platform_account_id
            })
        {
            return Err(AppError::conflict("platform identity is already connected"));
        }
        let account = record.account.clone();
        data.accounts.insert(account_key, record);
        Ok(account)
    }

    async fn delete_account(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        let mut data = self.0.write().await;
        let account_key = key(scope, id)?;
        if data.accounts.remove(&account_key).is_none() {
            return Err(AppError::not_found("account not found"));
        }
        data.logins.retain(|(o, t, p, _), login| {
            (*o, *t, *p) != (account_key.0, account_key.1, account_key.2) || login.account_id != id
        });
        Ok(())
    }

    async fn save_login(&self, scope: &TenantScope, session: LoginSession) -> Result<(), AppError> {
        if Some(session.project_id) != scope.project_id {
            return Err(AppError::forbidden("login outside project"));
        }
        let mut data = self.0.write().await;
        if !data.accounts.contains_key(&key(scope, session.account_id)?) {
            return Err(AppError::not_found("account not found"));
        }
        data.logins.insert(key(scope, session.session_id)?, session);
        Ok(())
    }

    async fn get_login(&self, scope: &TenantScope, id: Uuid) -> Result<LoginSession, AppError> {
        self.0
            .read()
            .await
            .logins
            .get(&key(scope, id)?)
            .cloned()
            .ok_or_else(|| AppError::not_found("login not found"))
    }

    async fn delete_login(&self, scope: &TenantScope, id: Uuid) -> Result<(), AppError> {
        if self
            .0
            .write()
            .await
            .logins
            .remove(&key(scope, id)?)
            .is_none()
        {
            return Err(AppError::not_found("login not found"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OperatorId, TenantId};

    fn scope(project_id: Uuid) -> TenantScope {
        TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(project_id)),
        )
    }

    #[tokio::test]
    async fn project_scope_and_duplicate_connector_identity() {
        let repo = MemoryChannelRepository::default();
        let project = Uuid::new_v4();
        let owner = scope(project);
        let other = TenantScope::new(
            owner.operator_id,
            owner.tenant_id,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let now = Utc::now();
        let group = ChannelGroup {
            group_id: Uuid::new_v4(),
            project_id: ProjectId::new(project),
            name: "team".into(),
            created_at: now,
        };
        repo.save_group(&owner, group.clone()).await.unwrap();
        repo.save_settings(
            &owner,
            ChannelSettingsRecord {
                settings: ChannelSettings {
                    project_id: ProjectId::new(project),
                    default_group_id: Some(group.group_id),
                    proxy_configured: true,
                    proxy_server: Some("http://127.0.0.1:3128".into()),
                    updated_at: now,
                },
                proxy: Some(ChannelSecret::new(vec![9, 8, 7])),
            },
        )
        .await
        .unwrap();
        assert!(repo.get_settings(&other).await.unwrap().is_none());
        let account = ChannelAccount {
            account_id: Uuid::new_v4(),
            project_id: ProjectId::new(project),
            owner_kind: ChannelOwnerKind::Customer,
            platform: "zhihu".into(),
            group_id: Some(group.group_id),
            status: ChannelStatus::Ready,
            display_name: Some("example".into()),
            platform_account_id: Some("connector-verified-id".into()),
            avatar_url: None,
            enabled: true,
            proxy_configured: false,
            proxy_server: None,
            created_at: now,
            updated_at: now,
        };
        repo.save_account(
            &owner,
            ChannelAccountRecord {
                account: account.clone(),
                session: Some(ChannelSecret::new(vec![1, 2, 3])),
                proxy: None,
            },
        )
        .await
        .unwrap();
        assert!(repo.list_accounts(&other).await.unwrap().is_empty());
        assert!(repo.get_account(&other, account.account_id).await.is_err());
        assert!(!serde_json::to_string(&account).unwrap().contains("session"));
        let mut duplicate = account.clone();
        duplicate.account_id = Uuid::new_v4();
        assert!(
            repo.save_account(
                &owner,
                ChannelAccountRecord {
                    account: duplicate,
                    session: None,
                    proxy: None,
                }
            )
            .await
            .is_err()
        );
        let mut cross_project = account;
        cross_project.account_id = Uuid::new_v4();
        cross_project.project_id = other.project_id.unwrap();
        cross_project.group_id = None;
        assert!(
            repo.save_account(
                &other,
                ChannelAccountRecord {
                    account: cross_project,
                    session: None,
                    proxy: None,
                }
            )
            .await
            .is_err()
        );
        repo.delete_group(&owner, group.group_id).await.unwrap();
        assert_eq!(repo.list_accounts(&owner).await.unwrap()[0].group_id, None);
        assert_eq!(
            repo.get_settings(&owner)
                .await
                .unwrap()
                .unwrap()
                .settings
                .default_group_id,
            None
        );
    }

    #[tokio::test]
    async fn operator_pool_requires_explicit_project_assignment() {
        let repo = MemoryChannelRepository::default();
        let owner = scope(Uuid::new_v4());
        let other_project = TenantScope::new(
            owner.operator_id,
            owner.tenant_id,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let now = Utc::now();
        let pool = PoolAccount {
            account_id: Uuid::new_v4(),
            platform: "zhihu".into(),
            group_id: None,
            status: ChannelStatus::Ready,
            display_name: Some("verified".into()),
            platform_account_id: Some("pool-verified-id".into()),
            avatar_url: None,
            enabled: true,
            proxy_configured: true,
            proxy_server: Some("http://internal.example:3128".into()),
            created_at: now,
            updated_at: now,
        };
        repo.save_pool_account(
            owner.operator_id,
            PoolAccountRecord {
                account: pool.clone(),
                session: Some(ChannelSecret::new(vec![42])),
                proxy: Some(ChannelSecret::new(vec![43])),
            },
        )
        .await
        .unwrap();
        assert!(
            repo.list_assigned_pool_accounts(&owner)
                .await
                .unwrap()
                .is_empty()
        );
        repo.assign_pool_account(&owner, pool.account_id, true)
            .await
            .unwrap();
        assert_eq!(
            repo.list_assigned_pool_accounts(&owner)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            repo.list_assigned_pool_accounts(&other_project)
                .await
                .unwrap()
                .is_empty()
        );
        let assigned = pool.assigned_view(owner.project_id.unwrap());
        assert_eq!(assigned.owner_kind, ChannelOwnerKind::OperatorPool);
        assert!(assigned.proxy_server.is_none());
        assert!(
            !serde_json::to_string(&assigned)
                .unwrap()
                .contains("session")
        );
        repo.assign_pool_account(&owner, pool.account_id, false)
            .await
            .unwrap();
        assert!(
            repo.list_assigned_pool_accounts(&owner)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
