use async_trait::async_trait;
use geo_domain::{
    AppError, ConnectorCapabilityRepository, ConnectorKey, ConnectorResolution, ConnectorSettings,
    ConnectorVerification, OperatorId, resolve_connector,
};
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Clone)]
pub struct PgConnectorCapabilityRepository {
    pool: PgPool,
}

impl PgConnectorCapabilityRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }
}

fn db(_error: sqlx::Error) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        "connector capability storage unavailable",
    )
}

fn decode<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(json).map_err(|_| {
        AppError::new(
            geo_domain::ErrorCode::Internal,
            "stored connector capability invalid",
        )
    })
}

fn settings(row: sqlx::postgres::PgRow) -> Result<ConnectorSettings, AppError> {
    Ok(ConnectorSettings {
        key: ConnectorKey {
            platform_id: row.get("platform_id"),
            placement_slot: row.get("placement_slot"),
        },
        revision: row.get("revision"),
        enabled: row.get("enabled"),
        content_types: decode(row.get("content_types"))?,
    })
}

fn proof(row: sqlx::postgres::PgRow) -> Result<ConnectorVerification, AppError> {
    Ok(ConnectorVerification {
        verification_id: row.get("verification_id"),
        key: ConnectorKey {
            platform_id: row.get("platform_id"),
            placement_slot: row.get("placement_slot"),
        },
        connector_version: row.get("connector_version"),
        content_type: row.get("content_type"),
        publication_receipt: decode(row.get("publication_receipt"))?,
        public_readback: decode(row.get("public_readback"))?,
        verified_at: row.get("verified_at"),
    })
}

async fn lock_key(
    tx: &mut Transaction<'_, Postgres>,
    operator: OperatorId,
    key: &ConnectorKey,
) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "connector-capability:{}:{}:{}",
            operator.as_uuid(),
            key.platform_id,
            key.placement_slot
        ))
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    Ok(())
}

async fn history_in(
    tx: &mut Transaction<'_, Postgres>,
    operator: OperatorId,
    key: &ConnectorKey,
) -> Result<Vec<ConnectorVerification>, AppError> {
    sqlx::query(
        "SELECT verification_id,platform_id,placement_slot,connector_version,content_type,\
         publication_receipt,public_readback,verified_at FROM connector_capability_verifications \
         WHERE operator_id=$1 AND platform_id=$2 AND placement_slot=$3 ORDER BY verified_at,verification_id"
    ).bind(operator.as_uuid()).bind(&key.platform_id).bind(&key.placement_slot)
        .fetch_all(&mut **tx).await.map_err(db)?
        .into_iter().map(proof).collect()
}

#[async_trait]
impl ConnectorCapabilityRepository for PgConnectorCapabilityRepository {
    async fn get(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
    ) -> Result<Option<ConnectorSettings>, AppError> {
        key.validate()?;
        sqlx::query("SELECT platform_id,placement_slot,revision,enabled,content_types \
            FROM connector_capability_settings WHERE operator_id=$1 AND platform_id=$2 AND placement_slot=$3")
            .bind(operator.as_uuid()).bind(&key.platform_id).bind(&key.placement_slot)
            .fetch_optional(&self.pool).await.map_err(db)?.map(settings).transpose()
    }

    async fn list(&self, operator: OperatorId) -> Result<Vec<ConnectorSettings>, AppError> {
        sqlx::query("SELECT platform_id,placement_slot,revision,enabled,content_types \
            FROM connector_capability_settings WHERE operator_id=$1 ORDER BY platform_id,placement_slot")
            .bind(operator.as_uuid()).fetch_all(&self.pool).await.map_err(db)?
            .into_iter().map(settings).collect()
    }

    async fn history(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
    ) -> Result<Vec<ConnectorVerification>, AppError> {
        key.validate()?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let records = history_in(&mut tx, operator, key).await?;
        tx.commit().await.map_err(db)?;
        Ok(records)
    }

    async fn configure(
        &self,
        operator: OperatorId,
        key: ConnectorKey,
        expected_revision: i32,
        enabled: bool,
        content_types: Vec<String>,
        deployed_version: &str,
    ) -> Result<ConnectorSettings, AppError> {
        key.validate()?;
        if expected_revision < 0 || expected_revision == i32::MAX {
            return Err(AppError::invalid_request("invalid expected revision"));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_key(&mut tx, operator, &key).await?;
        let current: Option<i32> = sqlx::query_scalar(
            "SELECT revision FROM connector_capability_settings WHERE operator_id=$1 AND platform_id=$2 AND placement_slot=$3")
            .bind(operator.as_uuid()).bind(&key.platform_id).bind(&key.placement_slot)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        if current.unwrap_or(0) != expected_revision {
            return Err(AppError::conflict("connector settings revision changed"));
        }
        let verified = history_in(&mut tx, operator, &key).await?;
        // This check lives in the domain, identical in memory and PostgreSQL.
        geo_domain::validate_connector_settings(
            enabled,
            &content_types,
            &verified,
            deployed_version,
        )?;
        let new_revision = expected_revision + 1;
        let content_json = serde_json::to_value(&content_types)
            .map_err(|_| AppError::invalid_request("invalid connector content types"))?;
        sqlx::query("INSERT INTO connector_capability_settings \
            (operator_id,platform_id,placement_slot,revision,enabled,content_types) VALUES($1,$2,$3,$4,$5,$6) \
            ON CONFLICT(operator_id,platform_id,placement_slot) DO UPDATE SET \
                revision=EXCLUDED.revision,enabled=EXCLUDED.enabled,content_types=EXCLUDED.content_types")
            .bind(operator.as_uuid()).bind(&key.platform_id).bind(&key.placement_slot)
            .bind(new_revision).bind(enabled).bind(content_json)
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(ConnectorSettings {
            key,
            revision: new_revision,
            enabled,
            content_types,
        })
    }

    async fn insert_verification(
        &self,
        operator: OperatorId,
        proof: ConnectorVerification,
    ) -> Result<(), AppError> {
        proof.validate()?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock_key(&mut tx, operator, &proof.key).await?;
        let receipt = serde_json::to_value(&proof.publication_receipt)
            .map_err(|_| AppError::invalid_request("invalid publication receipt"))?;
        let readback = serde_json::to_value(&proof.public_readback)
            .map_err(|_| AppError::invalid_request("invalid public readback"))?;
        let inserted = sqlx::query(
            "INSERT INTO connector_capability_verifications \
            (verification_id,operator_id,platform_id,placement_slot,connector_version,content_type,\
             publication_receipt,public_readback,verified_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(proof.verification_id)
        .bind(operator.as_uuid())
        .bind(&proof.key.platform_id)
        .bind(&proof.key.placement_slot)
        .bind(&proof.connector_version)
        .bind(&proof.content_type)
        .bind(receipt)
        .bind(readback)
        .bind(proof.verified_at)
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => tx.commit().await.map_err(db),
            Err(sqlx::Error::Database(ref error)) if error.is_unique_violation() => {
                Err(AppError::conflict("verification identity already exists"))
            }
            Err(error) => Err(db(error)),
        }
    }

    async fn resolve(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
        deployed_version: &str,
        content_type: &str,
    ) -> Result<ConnectorResolution, AppError> {
        key.validate()?;
        // One repeatable-read snapshot prevents settings/revocation and evidence
        // from being mixed across transactions during new-manifest resolution.
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let row = sqlx::query("SELECT platform_id,placement_slot,revision,enabled,content_types \
            FROM connector_capability_settings WHERE operator_id=$1 AND platform_id=$2 AND placement_slot=$3")
            .bind(operator.as_uuid()).bind(&key.platform_id).bind(&key.placement_slot)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let settings = row.map(settings).transpose()?;
        let records = history_in(&mut tx, operator, key).await?;
        tx.commit().await.map_err(db)?;
        Ok(resolve_connector(
            settings,
            &records,
            deployed_version,
            content_type,
        ))
    }
}
