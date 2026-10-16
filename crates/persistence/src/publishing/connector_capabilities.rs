use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ChannelAttempt, ChannelOutcome, ChannelTarget, ChannelTargetInput,
    ConnectorCapabilityRepository, ConnectorKey, ConnectorResolution, ConnectorSettings,
    ConnectorVerification, OperatorId, PLAIN_TEXT_ARTICLE_FORMAT, ProjectId, TenantId, TenantScope,
    publication_storage_timestamp, resolve_connector, saved_publication_verification,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

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

    /// Never accepts a caller-supplied receipt or claimed connector version.
    /// All authority is reloaded from the immutable frozen target and attempt.
    pub async fn project_saved_publication_verification(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<ConnectorVerification>, AppError> {
        let project_id = scope
            .project_id
            .ok_or_else(|| AppError::forbidden("project scope required"))?
            .as_uuid();
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT t.frozen_input,t.kind,t.publication_intent_id,a.account_id AS saved_account_id,\
                    a.target_kind,a.claimed_at,a.outcome,a.received_at,\
                    own.platform AS own_platform,pool.platform AS pool_platform \
             FROM channel_execution_attempts a \
             JOIN channel_execution_targets t ON (t.operator_id,t.tenant_id,t.project_id,t.target_id)=\
                (a.operator_id,a.tenant_id,a.project_id,a.target_id) \
             LEFT JOIN channel_accounts own ON (own.operator_id,own.tenant_id,own.project_id,own.account_id)=\
                (a.operator_id,a.tenant_id,a.project_id,a.account_id) \
             LEFT JOIN operator_channel_accounts pool ON (pool.operator_id,pool.account_id)=\
                (a.operator_id,a.account_id) \
             WHERE a.operator_id=$1 AND a.tenant_id=$2 AND a.project_id=$3 \
                AND a.target_id=$4 AND a.attempt_id=$5 FOR UPDATE OF a,t",
        )
        .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id)
        .bind(target_id).bind(attempt_id)
        .fetch_optional(&mut *tx).await.map_err(db)?
        .ok_or_else(|| AppError::not_found("saved publication attempt not found"))?;
        let target: ChannelTarget = decode(row.get("frozen_input"))?;
        let outcome: Option<ChannelOutcome> = row
            .get::<Option<serde_json::Value>, _>("outcome")
            .map(decode)
            .transpose()?;
        let attempt = ChannelAttempt {
            attempt_id,
            target_id,
            claimed_at: row.get("claimed_at"),
            outcome,
            received_at: row.get("received_at"),
        };
        if target.target_id != target_id
            || row.get::<&str, _>("kind") != "publish"
            || row.get::<&str, _>("target_kind") != "publish"
            || target.input.account_id() != row.get::<Uuid, _>("saved_account_id")
        {
            return Err(AppError::conflict(
                "saved publication target differs from attempt",
            ));
        }
        let (
            platform,
            account_id,
            source_id,
            source_version_id,
            revision_id,
            variant_id,
            intent_id,
            body_sha256,
            payload_hash,
        ) = match &target.input {
            ChannelTargetInput::Publish {
                platform,
                account_id,
                source_id,
                source_version_id,
                title,
                body,
                body_sha256,
            } => {
                if row
                    .get::<Option<Uuid>, _>("publication_intent_id")
                    .is_some()
                {
                    return Err(AppError::conflict("source target has generated intent"));
                }
                let source: Option<Uuid> = sqlx::query_scalar(
                        "SELECT versions.source_id FROM knowledge_source_versions versions \
                         JOIN knowledge_sources sources ON (sources.operator_id,sources.tenant_id,sources.project_id,sources.source_id)=\
                            (versions.operator_id,versions.tenant_id,versions.project_id,versions.source_id) \
                         WHERE versions.operator_id=$1 AND versions.tenant_id=$2 AND versions.project_id=$3 \
                            AND versions.source_version_id=$4 AND versions.source_id=$5"
                    ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id)
                    .bind(source_version_id).bind(source_id)
                    .fetch_optional(&mut *tx).await.map_err(db)?;
                if source != Some(*source_id) {
                    return Err(AppError::conflict("publication source version missing"));
                }
                (
                    platform,
                    *account_id,
                    Some(*source_id),
                    Some(*source_version_id),
                    None,
                    None,
                    None,
                    body_sha256.clone(),
                    geo_domain::plain_text_article_readback_hash(title, body),
                )
            }
            ChannelTargetInput::GeneratedPublish {
                platform,
                account_id,
                content_revision_id,
                variant_id,
                publication_intent_id,
                distribution_target_id,
                evidence,
                title,
                body,
                body_sha256,
                payload_hash,
                ..
            } => {
                if row.get::<Option<Uuid>, _>("publication_intent_id")
                    != Some(*publication_intent_id)
                {
                    return Err(AppError::conflict("generated target intent differs"));
                }
                let generated = sqlx::query(
                        "SELECT c.payload_hash AS command_hash,c.fixture,v.body AS variant_body,\
                                i.body AS intent_body,r.body AS revision_body \
                         FROM distribution_publication_commands c \
                         JOIN distribution_publication_intents i ON (i.operator_id,i.tenant_id,i.project_id,i.intent_id)=\
                            (c.operator_id,c.tenant_id,c.project_id,c.intent_id) \
                         JOIN distribution_channel_variants v ON (v.operator_id,v.tenant_id,v.project_id,v.variant_id)=\
                            (i.operator_id,i.tenant_id,i.project_id,i.variant_id) \
                         JOIN content_revisions r ON (r.operator_id,r.tenant_id,r.project_id,r.revision_id)=\
                            (v.operator_id,v.tenant_id,v.project_id,v.content_revision_id) \
                         WHERE c.operator_id=$1 AND c.tenant_id=$2 AND c.project_id=$3 \
                            AND c.materialized_target_id=$4 AND c.intent_id=$5 AND c.origin_target_id=$6"
                    ).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id)
                    .bind(target_id).bind(publication_intent_id).bind(distribution_target_id)
                    .fetch_optional(&mut *tx).await.map_err(db)?
                    .ok_or_else(|| AppError::conflict("generated publication dependencies missing"))?;
                let variant: geo_domain::ChannelVariant = decode(generated.get("variant_body"))?;
                let intent: geo_domain::PublicationIntent = decode(generated.get("intent_body"))?;
                let revision: geo_domain::ContentRevision = decode(generated.get("revision_body"))?;
                if generated.get::<bool, _>("fixture")
                    || generated.get::<String, _>("command_hash") != *payload_hash
                    || intent.intent_id != *publication_intent_id
                    || intent.variant_id != *variant_id
                    || intent.content_revision_id != *content_revision_id
                    || intent.account_id != *account_id
                    || intent.channel_target_id != *distribution_target_id
                    || intent.platform_id != *platform
                    || intent.placement_slot != variant.placement_slot
                    || intent.payload_hash != *payload_hash
                    || variant.variant_id != *variant_id
                    || variant.content_revision_id != *content_revision_id
                    || variant.platform_id != *platform
                    || variant.placement_slot != "primary"
                    || variant.title != *title
                    || variant.markdown != *body
                    || variant.evidence != *evidence
                    || variant.payload_hash != *payload_hash
                    || revision.revision_id != *content_revision_id
                {
                    return Err(AppError::conflict("generated publication payload differs"));
                }
                (
                    platform,
                    *account_id,
                    None,
                    None,
                    Some(*content_revision_id),
                    Some(*variant_id),
                    Some(*publication_intent_id),
                    body_sha256.clone(),
                    payload_hash.clone(),
                )
            }
            ChannelTargetInput::Measure { .. } => return Ok(None),
        };
        let own_platform: Option<String> = row.get("own_platform");
        let pool_platform: Option<String> = row.get("pool_platform");
        // Assignment can be revoked after a real send. The scoped saved
        // attempt proves project membership at claim time; operator ownership
        // and account platform remain checked here without mutable assignment.
        if (own_platform.as_deref() == Some(platform.as_str())) as u8
            + (pool_platform.as_deref() == Some(platform.as_str())) as u8
            != 1
        {
            return Err(AppError::conflict(
                "publication account ownership or platform differs",
            ));
        }
        let Some(proof) = saved_publication_verification(&target, &attempt)? else {
            return Ok(None);
        };
        lock_key(&mut tx, scope.operator_id, &proof.key).await?;
        let receipt = serde_json::to_value(&proof.publication_receipt)
            .map_err(|_| AppError::invalid_request("invalid saved receipt"))?;
        let readback = serde_json::to_value(&proof.public_readback)
            .map_err(|_| AppError::invalid_request("invalid saved readback"))?;
        let inserted = sqlx::query(
            "INSERT INTO connector_capability_verifications \
             (verification_id,operator_id,tenant_id,project_id,target_id,attempt_id,account_id,\
              source_id,source_version_id,content_revision_id,variant_id,publication_intent_id,\
              platform_id,placement_slot,connector_version,content_type,body_sha256,payload_hash,\
              publication_receipt,public_readback,verified_at,observed_at,received_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23) \
             ON CONFLICT (verification_id) DO NOTHING",
        ).bind(proof.verification_id).bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid()).bind(project_id).bind(target_id).bind(attempt_id).bind(account_id)
        .bind(source_id).bind(source_version_id).bind(revision_id).bind(variant_id).bind(intent_id)
        .bind(&proof.key.platform_id).bind(&proof.key.placement_slot)
        .bind(&proof.connector_version).bind(&proof.content_type).bind(&body_sha256).bind(&payload_hash)
        .bind(&receipt).bind(&readback).bind(proof.verified_at).bind(proof.verified_at)
        .bind(attempt.received_at.expect("validated saved receipt"))
        .execute(&mut *tx).await.map_err(db)?;
        if inserted.rows_affected() == 0 {
            let existing = sqlx::query(
                "SELECT tenant_id,project_id,target_id,attempt_id,account_id,source_id,source_version_id,\
                        content_revision_id,variant_id,publication_intent_id,platform_id,placement_slot,\
                        connector_version,content_type,body_sha256,payload_hash,publication_receipt,\
                        public_readback,verified_at,observed_at,received_at \
                 FROM connector_capability_verifications WHERE verification_id=$1 AND operator_id=$2",
            ).bind(proof.verification_id).bind(scope.operator_id.as_uuid())
            .fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| AppError::conflict("saved verification identity already used"))?;
            if existing.get::<Option<Uuid>, _>("tenant_id") != Some(scope.tenant_id.as_uuid())
                || existing.get::<Option<Uuid>, _>("project_id") != Some(project_id)
                || existing.get::<Option<Uuid>, _>("target_id") != Some(target_id)
                || existing.get::<Option<Uuid>, _>("attempt_id") != Some(attempt_id)
                || existing.get::<Option<Uuid>, _>("account_id") != Some(account_id)
                || existing.get::<Option<Uuid>, _>("source_id") != source_id
                || existing.get::<Option<Uuid>, _>("source_version_id") != source_version_id
                || existing.get::<Option<Uuid>, _>("content_revision_id") != revision_id
                || existing.get::<Option<Uuid>, _>("variant_id") != variant_id
                || existing.get::<Option<Uuid>, _>("publication_intent_id") != intent_id
                || existing.get::<String, _>("platform_id") != proof.key.platform_id
                || existing.get::<String, _>("placement_slot") != proof.key.placement_slot
                || existing.get::<String, _>("connector_version") != proof.connector_version
                || existing.get::<String, _>("content_type") != proof.content_type
                || existing.get::<Option<String>, _>("body_sha256") != Some(body_sha256)
                || existing.get::<Option<String>, _>("payload_hash") != Some(payload_hash)
                || existing.get::<serde_json::Value, _>("publication_receipt") != receipt
                || existing.get::<serde_json::Value, _>("public_readback") != readback
                || existing.get::<DateTime<Utc>, _>("verified_at")
                    != publication_storage_timestamp(proof.verified_at)
                || existing.get::<Option<DateTime<Utc>>, _>("observed_at")
                    != Some(publication_storage_timestamp(proof.verified_at))
                || existing.get::<Option<DateTime<Utc>>, _>("received_at") != attempt.received_at
            {
                return Err(AppError::conflict(
                    "saved verification differs from existing proof",
                ));
            }
        }
        sqlx::query(
            "INSERT INTO connector_capability_settings \
             (operator_id,platform_id,placement_slot,revision,enabled,content_types) \
             VALUES ($1,$2,$3,1,true,$4) ON CONFLICT (operator_id,platform_id,placement_slot) DO NOTHING",
        ).bind(scope.operator_id.as_uuid()).bind(&proof.key.platform_id).bind(&proof.key.placement_slot)
        .bind(serde_json::json!([PLAIN_TEXT_ARTICLE_FORMAT]))
        .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(Some(proof))
    }

    /// Recovery observes only saved outcomes; it never claims or sends a job.
    /// The cursor advances even past rejected/legacy outcomes.
    pub async fn scan_unprojected_publications(
        &self,
        after_attempt_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<(TenantScope, Uuid, Uuid)>, AppError> {
        if limit == 0 || limit > 1000 {
            return Err(AppError::invalid_request("invalid verification page size"));
        }
        let rows = sqlx::query(
            "SELECT a.operator_id,a.tenant_id,a.project_id,a.target_id,a.attempt_id \
             FROM channel_execution_attempts a \
             WHERE a.attempt_id > COALESCE($1::uuid,'00000000-0000-0000-0000-000000000000'::uuid) \
               AND a.target_kind='publish' AND a.outcome IS NOT NULL \
               AND a.outcome->>'status'='verified' AND a.outcome->>'fixture'='false' \
               AND EXISTS (SELECT 1 FROM jsonb_array_elements(COALESCE(a.outcome->'runner_evidence','[]'::jsonb)) proof \
                   WHERE proof->>'kind'='runner_receipt' AND proof->>'schema_version'='geo.runner.receipt.v1' \
                     AND proof->>'provenance'='live') \
               AND NOT EXISTS (SELECT 1 FROM connector_capability_verifications v \
                 WHERE v.operator_id=a.operator_id AND v.tenant_id=a.tenant_id AND v.project_id=a.project_id \
                   AND v.attempt_id=a.attempt_id AND v.content_type=$2) \
             ORDER BY a.attempt_id LIMIT $3",
        ).bind(after_attempt_id).bind(PLAIN_TEXT_ARTICLE_FORMAT).bind(limit as i64)
        .fetch_all(&self.pool).await.map_err(db)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    TenantScope::new(
                        OperatorId::new(row.get("operator_id")),
                        TenantId::new(row.get("tenant_id")),
                        Some(ProjectId::new(row.get("project_id"))),
                    ),
                    row.get("target_id"),
                    row.get("attempt_id"),
                )
            })
            .collect())
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
