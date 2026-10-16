//! Operator inventory only; inventory does not establish account routes or egress health.
use crate::{AppError, ChannelSecret, MemoryChannelRepository, OperatorId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyPool {
    pub pool_id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProxyPoolMember {
    pub member_id: Uuid,
    pub pool_id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub version: i64,
    pub revision_id: Uuid,
    pub proxy_server: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait ProxyPoolRepository: Send + Sync {
    async fn list_proxy_pools(
        &self,
        operator: OperatorId,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ProxyPool>, AppError>;
    async fn get_proxy_pool(&self, operator: OperatorId, id: Uuid) -> Result<ProxyPool, AppError>;
    async fn save_proxy_pool(
        &self,
        operator: OperatorId,
        expected: Option<i64>,
        pool: ProxyPool,
    ) -> Result<ProxyPool, AppError>;
    async fn list_proxy_members(
        &self,
        operator: OperatorId,
        pool: Uuid,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ProxyPoolMember>, AppError>;
    async fn get_proxy_member(
        &self,
        operator: OperatorId,
        pool: Uuid,
        id: Uuid,
    ) -> Result<ProxyPoolMember, AppError>;
    async fn save_proxy_member(
        &self,
        operator: OperatorId,
        expected: Option<i64>,
        member: ProxyPoolMember,
        revision: Option<ChannelSecret>,
    ) -> Result<ProxyPoolMember, AppError>;
    /// Internal encrypted lookup for future pinned bindings; never a public HTTP projection.
    async fn get_proxy_revision(
        &self,
        operator: OperatorId,
        pool: Uuid,
        member: Uuid,
        revision: Uuid,
    ) -> Result<ChannelSecret, AppError>;
}

pub fn validate_proxy_version(
    expected: Option<i64>,
    actual: Option<i64>,
    proposed: i64,
) -> Result<(), AppError> {
    if expected != actual
        || proposed
            != actual
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| AppError::conflict("inventory version exhausted"))?
    {
        return Err(AppError::conflict("inventory version changed"));
    }
    Ok(())
}

#[async_trait]
impl ProxyPoolRepository for MemoryChannelRepository {
    async fn list_proxy_pools(
        &self,
        operator: OperatorId,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ProxyPool>, AppError> {
        let state = self.0.read().await;
        let mut rows: Vec<_> = state
            .proxy_pools
            .iter()
            .filter(|((op, id), _)| {
                *op == operator.as_uuid() && after.is_none_or(|after| *id > after)
            })
            .map(|(_, v)| v.clone())
            .collect();
        rows.sort_by_key(|v| v.pool_id);
        rows.truncate(limit.min(101) as usize);
        Ok(rows)
    }
    async fn get_proxy_pool(&self, operator: OperatorId, id: Uuid) -> Result<ProxyPool, AppError> {
        self.0
            .read()
            .await
            .proxy_pools
            .get(&(operator.as_uuid(), id))
            .cloned()
            .ok_or_else(|| AppError::not_found("proxy pool not found"))
    }
    async fn save_proxy_pool(
        &self,
        operator: OperatorId,
        expected: Option<i64>,
        pool: ProxyPool,
    ) -> Result<ProxyPool, AppError> {
        let mut state = self.0.write().await;
        let key = (operator.as_uuid(), pool.pool_id);
        validate_proxy_version(
            expected,
            state.proxy_pools.get(&key).map(|v| v.version),
            pool.version,
        )?;
        state.proxy_pools.insert(key, pool.clone());
        Ok(pool)
    }
    async fn list_proxy_members(
        &self,
        operator: OperatorId,
        pool: Uuid,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ProxyPoolMember>, AppError> {
        self.get_proxy_pool(operator, pool).await?;
        let state = self.0.read().await;
        let mut rows: Vec<_> = state
            .proxy_members
            .iter()
            .filter(|((op, p, id), _)| {
                *op == operator.as_uuid() && *p == pool && after.is_none_or(|after| *id > after)
            })
            .map(|(_, v)| v.clone())
            .collect();
        rows.sort_by_key(|v| v.member_id);
        rows.truncate(limit.min(101) as usize);
        Ok(rows)
    }
    async fn get_proxy_member(
        &self,
        operator: OperatorId,
        pool: Uuid,
        id: Uuid,
    ) -> Result<ProxyPoolMember, AppError> {
        self.0
            .read()
            .await
            .proxy_members
            .get(&(operator.as_uuid(), pool, id))
            .cloned()
            .ok_or_else(|| AppError::not_found("proxy member not found"))
    }
    async fn save_proxy_member(
        &self,
        operator: OperatorId,
        expected: Option<i64>,
        member: ProxyPoolMember,
        revision: Option<ChannelSecret>,
    ) -> Result<ProxyPoolMember, AppError> {
        let mut state = self.0.write().await;
        let op = operator.as_uuid();
        if !state.proxy_pools.contains_key(&(op, member.pool_id)) {
            return Err(AppError::not_found("proxy pool not found"));
        }
        let key = (op, member.pool_id, member.member_id);
        let current = state.proxy_members.get(&key);
        validate_proxy_version(expected, current.map(|v| v.version), member.version)?;
        if revision.is_none()
            && current.is_none_or(|v| {
                v.revision_id != member.revision_id || v.proxy_server != member.proxy_server
            })
        {
            return Err(AppError::conflict("proxy revision required"));
        }
        if let Some(secret) = revision {
            let revision_key = (op, member.pool_id, member.member_id, member.revision_id);
            if state.proxy_revisions.contains_key(&revision_key) {
                return Err(AppError::conflict("proxy revision already exists"));
            }
            state.proxy_revisions.insert(revision_key, secret);
        }
        state.proxy_members.insert(key, member.clone());
        Ok(member)
    }
    async fn get_proxy_revision(
        &self,
        operator: OperatorId,
        pool: Uuid,
        member: Uuid,
        revision: Uuid,
    ) -> Result<ChannelSecret, AppError> {
        self.0
            .read()
            .await
            .proxy_revisions
            .get(&(operator.as_uuid(), pool, member, revision))
            .cloned()
            .ok_or_else(|| AppError::not_found("proxy revision not found"))
    }
}
