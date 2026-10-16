use crate::PgChannelRepository;
use async_trait::async_trait;
use geo_domain::{
    AppError, ChannelSecret, ErrorCode, OperatorId, ProxyPool, ProxyPoolMember,
    ProxyPoolRepository, validate_proxy_version,
};
use uuid::Uuid;

fn error(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "proxy inventory storage unavailable",
    )
}
fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "proxy inventory encoding failed"))
}
fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "stored proxy inventory invalid"))
}

#[async_trait]
impl ProxyPoolRepository for PgChannelRepository {
    async fn list_proxy_pools(
        &self,
        operator: OperatorId,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ProxyPool>, AppError> {
        let rows:Vec<(serde_json::Value,)> = sqlx::query_as("SELECT payload FROM operator_proxy_pools WHERE operator_id=$1 AND ($2::uuid IS NULL OR pool_id>$2) ORDER BY pool_id LIMIT $3")
            .bind(operator.as_uuid()).bind(after).bind(i64::from(limit.min(101))).fetch_all(&self.pool).await.map_err(error)?;
        rows.into_iter().map(|(v,)| decode(v)).collect()
    }
    async fn get_proxy_pool(&self, operator: OperatorId, id: Uuid) -> Result<ProxyPool, AppError> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT payload FROM operator_proxy_pools WHERE operator_id=$1 AND pool_id=$2",
        )
        .bind(operator.as_uuid())
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(error)?;
        decode(
            row.ok_or_else(|| AppError::not_found("proxy pool not found"))?
                .0,
        )
    }
    async fn save_proxy_pool(
        &self,
        operator: OperatorId,
        expected: Option<i64>,
        pool: ProxyPool,
    ) -> Result<ProxyPool, AppError> {
        validate_proxy_version(expected, expected, pool.version)?;
        let affected = if let Some(version) = expected {
            sqlx::query("UPDATE operator_proxy_pools SET version=$4,payload=$5 WHERE operator_id=$1 AND pool_id=$2 AND version=$3")
                .bind(operator.as_uuid()).bind(pool.pool_id).bind(version).bind(pool.version).bind(encode(&pool)?).execute(&self.pool).await.map_err(error)?.rows_affected()
        } else {
            sqlx::query("INSERT INTO operator_proxy_pools(operator_id,pool_id,version,payload) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
                .bind(operator.as_uuid()).bind(pool.pool_id).bind(pool.version).bind(encode(&pool)?).execute(&self.pool).await.map_err(error)?.rows_affected()
        };
        if affected != 1 {
            return Err(AppError::conflict("inventory version changed"));
        }
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
        let rows:Vec<(serde_json::Value,)> = sqlx::query_as("SELECT payload FROM operator_proxy_members WHERE operator_id=$1 AND pool_id=$2 AND ($3::uuid IS NULL OR member_id>$3) ORDER BY member_id LIMIT $4")
            .bind(operator.as_uuid()).bind(pool).bind(after).bind(i64::from(limit.min(101))).fetch_all(&self.pool).await.map_err(error)?;
        rows.into_iter().map(|(v,)| decode(v)).collect()
    }
    async fn get_proxy_member(
        &self,
        operator: OperatorId,
        pool: Uuid,
        id: Uuid,
    ) -> Result<ProxyPoolMember, AppError> {
        let row:Option<(serde_json::Value,)> = sqlx::query_as("SELECT payload FROM operator_proxy_members WHERE operator_id=$1 AND pool_id=$2 AND member_id=$3")
            .bind(operator.as_uuid()).bind(pool).bind(id).fetch_optional(&self.pool).await.map_err(error)?;
        decode(
            row.ok_or_else(|| AppError::not_found("proxy member not found"))?
                .0,
        )
    }
    async fn save_proxy_member(
        &self,
        operator: OperatorId,
        expected: Option<i64>,
        member: ProxyPoolMember,
        revision: Option<ChannelSecret>,
    ) -> Result<ProxyPoolMember, AppError> {
        let mut tx = self.pool.begin().await.map_err(error)?;
        // Serialize member creates and updates within the parent, including absent rows.
        let parent:Option<(Uuid,)> = sqlx::query_as("SELECT pool_id FROM operator_proxy_pools WHERE operator_id=$1 AND pool_id=$2 FOR UPDATE")
            .bind(operator.as_uuid()).bind(member.pool_id).fetch_optional(&mut *tx).await.map_err(error)?;
        if parent.is_none() {
            return Err(AppError::not_found("proxy pool not found"));
        }
        let current:Option<(serde_json::Value,)> = sqlx::query_as("SELECT payload FROM operator_proxy_members WHERE operator_id=$1 AND pool_id=$2 AND member_id=$3 FOR UPDATE")
            .bind(operator.as_uuid()).bind(member.pool_id).bind(member.member_id).fetch_optional(&mut *tx).await.map_err(error)?;
        let current: Option<ProxyPoolMember> = current.map(|(v,)| decode(v)).transpose()?;
        validate_proxy_version(
            expected,
            current.as_ref().map(|v| v.version),
            member.version,
        )?;
        if revision.is_none()
            && current.as_ref().is_none_or(|v| {
                v.revision_id != member.revision_id || v.proxy_server != member.proxy_server
            })
        {
            return Err(AppError::conflict("proxy revision required"));
        }
        if revision.is_some() {
            let exists:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operator_proxy_revisions WHERE operator_id=$1 AND pool_id=$2 AND member_id=$3 AND revision_id=$4)")
                .bind(operator.as_uuid()).bind(member.pool_id).bind(member.member_id).bind(member.revision_id).fetch_one(&mut *tx).await.map_err(error)?;
            if exists {
                return Err(AppError::conflict("proxy revision already exists"));
            }
        }
        sqlx::query("INSERT INTO operator_proxy_members(operator_id,pool_id,member_id,version,revision_id,payload) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(operator_id,pool_id,member_id) DO UPDATE SET version=excluded.version,revision_id=excluded.revision_id,payload=excluded.payload")
            .bind(operator.as_uuid()).bind(member.pool_id).bind(member.member_id).bind(member.version).bind(member.revision_id).bind(encode(&member)?).execute(&mut *tx).await.map_err(error)?;
        if let Some(secret) = revision {
            sqlx::query("INSERT INTO operator_proxy_revisions(operator_id,pool_id,member_id,revision_id,encrypted_proxy) VALUES($1,$2,$3,$4,$5)")
                .bind(operator.as_uuid()).bind(member.pool_id).bind(member.member_id).bind(member.revision_id).bind(secret.encrypted_bytes()).execute(&mut *tx).await.map_err(error)?;
        }
        tx.commit().await.map_err(error)?;
        Ok(member)
    }
    async fn get_proxy_revision(
        &self,
        operator: OperatorId,
        pool: Uuid,
        member: Uuid,
        revision: Uuid,
    ) -> Result<ChannelSecret, AppError> {
        let row:Option<(Vec<u8>,)> = sqlx::query_as("SELECT encrypted_proxy FROM operator_proxy_revisions WHERE operator_id=$1 AND pool_id=$2 AND member_id=$3 AND revision_id=$4")
            .bind(operator.as_uuid()).bind(pool).bind(member).bind(revision).fetch_optional(&self.pool).await.map_err(error)?;
        Ok(ChannelSecret::new(
            row.ok_or_else(|| AppError::not_found("proxy revision not found"))?
                .0,
        ))
    }
}
