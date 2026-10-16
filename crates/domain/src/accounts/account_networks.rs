//! Immutable account routes. Inventory membership is not an egress-health proof.
use crate::{AppError, ChannelRepository, ChannelSecret, OperatorId, ProjectId, TenantId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountNetworkScope {
    pub operator_id: OperatorId,
    pub account_id: Uuid,
    pub tenant_id: Option<TenantId>,
    pub project_id: Option<ProjectId>,
}

impl AccountNetworkScope {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.account_id.is_nil() || self.tenant_id.is_some() != self.project_id.is_some() {
            return Err(AppError::invalid_request("invalid account network scope"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountNetworkSource {
    Direct,
    Standalone,
    LegacySnapshot,
    ProjectDefault {
        settings_updated_at: Option<DateTime<Utc>>,
    },
    PoolRevision {
        pool_id: Uuid,
        member_id: Uuid,
        revision_id: Uuid,
    },
}

/// Server-only envelope authenticated to this immutable binding identity.
#[derive(Clone)]
pub struct AccountNetworkBinding {
    pub binding_id: Uuid,
    pub scope: AccountNetworkScope,
    pub source: AccountNetworkSource,
    pub effective_network: ChannelSecret,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AccountNetworkBindings {
    pub version: i64,
    pub active: AccountNetworkBinding,
    pub pending: Option<AccountNetworkBinding>,
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserNetworkSelection {
    pub scope: AccountNetworkScope,
    /// Only trusted historical execution may name an old binding.
    pub historical_binding_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct LegacyNetworkBootstrap {
    pub binding: AccountNetworkBinding,
    pub operation_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct ProjectNetworkBindingUpdate {
    pub expected_version: Option<i64>,
    pub binding: AccountNetworkBinding,
    pub legacy: Option<LegacyNetworkBootstrap>,
}

#[derive(Clone)]
pub enum AccountNetworkMetadataUpdate {
    Customer {
        expected: crate::ChannelAccountRecord,
        updated: crate::ChannelAccountRecord,
    },
    OperatorPool {
        expected: crate::PoolAccountRecord,
        updated: crate::PoolAccountRecord,
    },
}

#[async_trait]
pub trait AccountNetworkBindingRepository: Send + Sync {
    async fn set_project_network_defaults(
        &self,
        scope: &crate::TenantScope,
        expected: Option<&crate::ChannelSettingsRecord>,
        record: &crate::ChannelSettingsRecord,
        updates: &[ProjectNetworkBindingUpdate],
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<crate::ChannelSettings, AppError>;
    async fn get_account_network(
        &self,
        scope: &AccountNetworkScope,
    ) -> Result<Option<AccountNetworkBindings>, AppError>;
    async fn get_network_binding(
        &self,
        scope: &AccountNetworkScope,
        id: Uuid,
    ) -> Result<AccountNetworkBinding, AppError>;
    #[allow(clippy::too_many_arguments)]
    async fn set_account_network_with_metadata(
        &self,
        scope: &AccountNetworkScope,
        expected_version: Option<i64>,
        binding: &AccountNetworkBinding,
        legacy: Option<&LegacyNetworkBootstrap>,
        metadata: Option<&AccountNetworkMetadataUpdate>,
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<AccountNetworkBindings, AppError>;
    async fn set_account_network_with_legacy(
        &self,
        scope: &AccountNetworkScope,
        expected_version: Option<i64>,
        binding: &AccountNetworkBinding,
        legacy: Option<&LegacyNetworkBootstrap>,
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<AccountNetworkBindings, AppError> {
        self.set_account_network_with_metadata(
            scope,
            expected_version,
            binding,
            legacy,
            None,
            channels,
            at,
        )
        .await
    }
    async fn set_account_network(
        &self,
        scope: &AccountNetworkScope,
        expected_version: Option<i64>,
        binding: &AccountNetworkBinding,
        channels: &dyn ChannelRepository,
        at: DateTime<Utc>,
    ) -> Result<AccountNetworkBindings, AppError> {
        self.set_account_network_with_legacy(scope, expected_version, binding, None, channels, at)
            .await
    }
}

pub fn validate_network_binding(
    scope: &AccountNetworkScope,
    binding: &AccountNetworkBinding,
) -> Result<(), AppError> {
    scope.validate()?;
    if &binding.scope != scope
        || binding.binding_id.is_nil()
        || binding.effective_network.encrypted_bytes().is_empty()
    {
        return Err(AppError::invalid_request("invalid account network binding"));
    }
    Ok(())
}

#[derive(Default, Clone)]
pub(crate) struct MemoryAccountNetworks {
    pub heads: std::collections::HashMap<(Uuid, Uuid), AccountNetworkBindings>,
    pub bindings: std::collections::HashMap<Uuid, AccountNetworkBinding>,
}

impl MemoryAccountNetworks {
    pub fn set(
        &mut self,
        scope: &AccountNetworkScope,
        expected: Option<i64>,
        binding: &AccountNetworkBinding,
        legacy: Option<&LegacyNetworkBootstrap>,
        active_op: Option<Uuid>,
    ) -> Result<AccountNetworkBindings, AppError> {
        validate_network_binding(scope, binding)?;
        let key = (scope.operator_id.as_uuid(), scope.account_id);
        let current = self.heads.get(&key);
        if current.map(|h| h.version) != expected
            || current.is_some_and(|h| &h.active.scope != scope)
        {
            return Err(AppError::conflict("account network version changed"));
        }
        if self.bindings.contains_key(&binding.binding_id) {
            return Err(AppError::conflict("network binding already exists"));
        }
        let busy = active_op.is_some();
        if let Some(legacy) = legacy {
            validate_network_binding(scope, &legacy.binding)?;
            if legacy.binding.source != AccountNetworkSource::LegacySnapshot
                || legacy.operation_id != active_op
            {
                return Err(AppError::conflict("legacy network owner changed"));
            }
        }
        let initialize_legacy = current.is_none() && busy;
        let result = match current {
            Some(head) => AccountNetworkBindings {
                version: head
                    .version
                    .checked_add(1)
                    .ok_or_else(|| AppError::conflict("network version exhausted"))?,
                active: if busy {
                    head.active.clone()
                } else {
                    binding.clone()
                },
                pending: busy.then(|| binding.clone()),
                busy,
            },
            None if busy => {
                let legacy = legacy.ok_or_else(|| {
                    AppError::conflict("busy legacy network requires initialization")
                })?;
                if self.bindings.contains_key(&legacy.binding.binding_id)
                    || legacy.binding.binding_id == binding.binding_id
                {
                    return Err(AppError::conflict("legacy binding already exists"));
                }
                AccountNetworkBindings {
                    version: 1,
                    active: legacy.binding.clone(),
                    pending: Some(binding.clone()),
                    busy,
                }
            }
            None => AccountNetworkBindings {
                version: 1,
                active: binding.clone(),
                pending: None,
                busy,
            },
        };
        self.bindings.insert(binding.binding_id, binding.clone());
        if let Some(legacy) = legacy.filter(|_| initialize_legacy) {
            self.bindings
                .insert(legacy.binding.binding_id, legacy.binding.clone());
        }
        self.heads.insert(key, result.clone());
        Ok(result)
    }
}

pub fn settings_snapshot_matches(
    actual: Option<&crate::ChannelSettingsRecord>,
    expected: Option<&crate::ChannelSettingsRecord>,
) -> bool {
    match (actual, expected) {
        (None, None) => true,
        (Some(a), Some(e)) => {
            serde_json::to_value(&a.settings).ok() == serde_json::to_value(&e.settings).ok()
                && a.proxy.as_ref().map(ChannelSecret::encrypted_bytes)
                    == e.proxy.as_ref().map(ChannelSecret::encrypted_bytes)
        }
        _ => false,
    }
}
