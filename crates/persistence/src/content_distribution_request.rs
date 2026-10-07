use async_trait::async_trait;
use geo_domain::{
    AcceptContentDistributionRequest, AppError, ChannelAccount, ChannelStatus, ChannelVariant,
    ContentCheck, ContentDistributionRequest, ContentDistributionRequestRepository,
    ContentRevision, ErrorCode, PlatformPlacement, PoolAccount, PublicationIntent, TenantScope,
    distribution_request_key_hash, prepare_content_distribution_request,
    prepare_request_publication_intent, prepare_variant, validate_distribution_request_intent,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgContentDistributionRequestRepository {
    pool: PgPool,
}

impl PgContentDistributionRequestRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }

    async fn transaction<'a>(
        &'a self,
        scope: &TenantScope,
    ) -> Result<Transaction<'a, Postgres>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        crate::set_local_scope(&mut tx, scope).await.map_err(db)?;
        Ok(tx)
    }
}

fn db(error: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        format!("distribution request database operation failed: {error}"),
    )
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("distribution request requires project scope"))
}

pub(crate) fn read_request(row: &sqlx::postgres::PgRow) -> ContentDistributionRequest {
    ContentDistributionRequest {
        request_id: row.get("request_id"),
        scope: TenantScope::new(
            row.get::<Uuid, _>("operator_id").into(),
            row.get::<Uuid, _>("tenant_id").into(),
            Some(row.get::<Uuid, _>("project_id").into()),
        ),
        schema_version: row.get("schema_version"),
        content_revision_id: row.get("content_revision_id"),
        content_asset_id: row.get("content_asset_id"),
        platform_id: row.get("platform_id"),
        placement_slot: row.get("placement_slot"),
        account_id: row.get("account_id"),
        account_owner_kind: row.get("account_owner_kind"),
        format: row.get("format"),
        idempotency_key_hash: row.get("idempotency_key_hash"),
        request_hash: row.get("request_hash"),
        publication_intent_id: row.get("publication_intent_id"),
        created_at: row.get("created_at"),
    }
}

pub(crate) const REQUEST_COLUMNS: &str = "request_id,operator_id,tenant_id,project_id,schema_version,\
content_revision_id,content_asset_id,platform_id,placement_slot,account_id,account_owner_kind,\
format,idempotency_key_hash,request_hash,publication_intent_id,created_at";

fn decode<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> Result<T, AppError> {
    serde_json::from_value(json)
        .map_err(|_| AppError::new(ErrorCode::Internal, "invalid stored publication dependency"))
}
fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value)
        .map_err(|_| AppError::new(ErrorCode::Internal, "cannot encode publication dependency"))
}

async fn live_request_account(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    request: &ContentDistributionRequest,
) -> Result<(), AppError> {
    let metadata: Option<serde_json::Value> = if request.account_owner_kind == "customer" {
        sqlx::query_scalar(
            "SELECT metadata FROM channel_accounts WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 AND account_id=$4 AND platform=$5 FOR SHARE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(request.account_id)
        .bind(&request.platform_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
    } else {
        sqlx::query_scalar(
            "SELECT p.metadata FROM operator_channel_assignments a \
             JOIN operator_channel_accounts p ON p.operator_id=a.operator_id AND p.account_id=a.account_id \
             WHERE a.operator_id=$1 AND a.tenant_id=$2 AND a.project_id=$3 \
                AND a.account_id=$4 AND p.platform=$5 FOR SHARE OF a,p",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(request.account_id)
        .bind(&request.platform_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
    };
    let Some(metadata) = metadata else {
        return Err(AppError::conflict("publication account is not assigned"));
    };
    let account: ChannelAccount = if request.account_owner_kind == "customer" {
        decode(metadata)?
    } else {
        let pool: PoolAccount = decode(metadata)?;
        pool.assigned_view(scope.project_id.expect("project scope"))
    };
    if account.account_id != request.account_id
        || account.platform != request.platform_id
        || !account.enabled
        || account.status != ChannelStatus::Ready
    {
        return Err(AppError::conflict("publication account is not ready"));
    }
    Ok(())
}

async fn live_request_evidence(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    revision: &ContentRevision,
) -> Result<(), AppError> {
    if revision.evidence.is_empty() || revision.quotes.is_empty() {
        return Err(AppError::conflict("publication requires public evidence"));
    }
    let mut versions = std::collections::BTreeSet::new();
    for reference in &revision.evidence {
        versions.insert(reference.source_version_id);
    }
    for version in versions {
        let row = sqlx::query(
            "SELECT s.state,s.purpose,s.current_version_id FROM knowledge_source_versions v \
             JOIN knowledge_sources s ON (s.operator_id,s.tenant_id,s.project_id,s.source_id)= \
                (v.operator_id,v.tenant_id,v.project_id,v.source_id) \
             WHERE v.operator_id=$1 AND v.tenant_id=$2 AND v.project_id=$3 \
                AND v.source_version_id=$4 FOR SHARE OF s",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(version)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("publication source is unavailable"))?;
        if row.get::<Option<Uuid>, _>("current_version_id") != Some(version)
            || row.get::<String, _>("state") != "active"
            || row.get::<String, _>("purpose") != "public"
        {
            return Err(AppError::conflict("publication source is no longer public"));
        }
    }
    for reference in &revision.evidence {
        let quote = revision
            .quotes
            .iter()
            .find(|quote| quote.reference == *reference)
            .ok_or_else(|| AppError::conflict("publication evidence quote missing"))?;
        let chunk_id = reference
            .chunk_id
            .ok_or_else(|| AppError::conflict("publication evidence chunk missing"))?;
        let row = sqlx::query(
            "SELECT text,locator FROM knowledge_chunks \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
                AND source_version_id=$4 AND chunk_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?)
        .bind(reference.source_version_id)
        .bind(chunk_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("publication evidence chunk unavailable"))?;
        let text: String = row.get("text");
        let expected = if matches!(reference.locator, geo_domain::ChunkLocator::Csv { .. }) {
            text
        } else {
            text.chars()
                .take(geo_domain::CONTENT_EVIDENCE_MAX_QUOTE_CHARS)
                .collect()
        };
        if row.get::<serde_json::Value, _>("locator") != encode(&reference.locator)?
            || expected != quote.exact_quote
            || quote.exact_quote.chars().count() > geo_domain::CONTENT_EVIDENCE_MAX_QUOTE_CHARS
        {
            return Err(AppError::conflict("publication evidence quote has changed"));
        }
    }
    Ok(())
}

#[async_trait]
impl ContentDistributionRequestRepository for PgContentDistributionRequestRepository {
    async fn get_by_idempotency_key(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> Result<Option<ContentDistributionRequest>, AppError> {
        let key_hash = distribution_request_key_hash(key)?;
        let mut tx = self.transaction(scope).await?;
        let query = format!(
            "SELECT {REQUEST_COLUMNS} FROM content_distribution_requests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND idempotency_key_hash=$4"
        );
        let row = sqlx::query(&query)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?)
            .bind(key_hash)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(row.as_ref().map(read_request))
    }

    async fn accept(
        &self,
        scope: &TenantScope,
        input: AcceptContentDistributionRequest,
    ) -> Result<ContentDistributionRequest, AppError> {
        let request = prepare_content_distribution_request(scope, &input)?;
        let project_id = project(scope)?;
        let mut tx = self.transaction(scope).await?;
        // The unique idempotency index is the cross-process serialization
        // point. The INSERT verifies both authoritative resources in the same
        // transaction and never writes a publication outbox record.
        let inserted = sqlx::query(
            "INSERT INTO content_distribution_requests \
             (request_id,operator_id,tenant_id,project_id,schema_version,content_revision_id,\
              content_asset_id,platform_id,placement_slot,account_id,account_owner_kind,format,\
              idempotency_key_hash,request_hash,created_at) \
             SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15 \
             WHERE EXISTS (SELECT 1 FROM content_revisions r WHERE \
                 r.operator_id=$2 AND r.tenant_id=$3 AND r.project_id=$4 \
                 AND r.revision_id=$6 AND r.asset_id=$7) \
             AND (($11='customer' AND EXISTS (SELECT 1 FROM channel_accounts a WHERE \
                 a.operator_id=$2 AND a.tenant_id=$3 AND a.project_id=$4 \
                 AND a.account_id=$10 AND a.platform=$8)) \
                OR ($11='operator_pool' AND EXISTS (SELECT 1 FROM \
                 operator_channel_assignments a JOIN operator_channel_accounts p \
                 ON p.operator_id=a.operator_id AND p.account_id=a.account_id \
                 WHERE a.operator_id=$2 AND a.tenant_id=$3 AND a.project_id=$4 \
                 AND a.account_id=$10 AND p.platform=$8))) \
             ON CONFLICT (operator_id,tenant_id,project_id,idempotency_key_hash) DO NOTHING \
             RETURNING *",
        )
        .bind(request.request_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(request.schema_version)
        .bind(request.content_revision_id)
        .bind(request.content_asset_id)
        .bind(&request.platform_id)
        .bind(&request.placement_slot)
        .bind(request.account_id)
        .bind(&request.account_owner_kind)
        .bind(&request.format)
        .bind(&request.idempotency_key_hash)
        .bind(&request.request_hash)
        .bind(request.created_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let result = if let Some(row) = inserted {
            read_request(&row)
        } else {
            let query = format!(
                "SELECT {REQUEST_COLUMNS} FROM content_distribution_requests \
                 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
                 AND idempotency_key_hash=$4"
            );
            let existing = sqlx::query(&query)
                .bind(scope.operator_id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(project_id)
                .bind(&request.idempotency_key_hash)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .ok_or_else(|| AppError::not_found("revision or account not found"))?;
            let existing = read_request(&existing);
            if existing.request_hash != request.request_hash {
                return Err(AppError::conflict(
                    "idempotency key reused for another request",
                ));
            }
            existing
        };
        tx.commit().await.map_err(db)?;
        Ok(result)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError> {
        let mut tx = self.transaction(scope).await?;
        let query = format!(
            "SELECT {REQUEST_COLUMNS} FROM content_distribution_requests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND request_id=$4"
        );
        let row = sqlx::query(&query)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project(scope)?)
            .bind(request_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::not_found("distribution request not found"))?;
        tx.commit().await.map_err(db)?;
        Ok(read_request(&row))
    }

    async fn link_intent(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
        intent_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError> {
        let mut tx = self.transaction(scope).await?;
        let project_id = project(scope)?;
        let request_query = format!(
            "SELECT {REQUEST_COLUMNS} FROM content_distribution_requests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
             AND request_id=$4 FOR UPDATE"
        );
        let request = sqlx::query(&request_query)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(request_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::not_found("distribution request not found"))?;
        let request = read_request(&request);
        let row = sqlx::query(
            "SELECT i.body AS intent, v.body AS variant FROM distribution_publication_intents i \
             JOIN distribution_channel_variants v ON (v.operator_id,v.tenant_id,v.project_id,v.variant_id)=\
               (i.operator_id,i.tenant_id,i.project_id,i.variant_id) \
             JOIN distribution_publication_commands c ON (c.operator_id,c.tenant_id,c.project_id,c.intent_id)=\
               (i.operator_id,i.tenant_id,i.project_id,i.intent_id) \
             WHERE i.operator_id=$1 AND i.tenant_id=$2 AND i.project_id=$3 AND i.intent_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(intent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("publication intent not found"))?;
        let intent: PublicationIntent = serde_json::from_value(row.get("intent"))
            .map_err(|_| AppError::new(ErrorCode::Internal, "invalid stored intent"))?;
        let variant: ChannelVariant = serde_json::from_value(row.get("variant"))
            .map_err(|_| AppError::new(ErrorCode::Internal, "invalid stored variant"))?;
        validate_distribution_request_intent(&request, &intent, &variant)?;
        if request
            .publication_intent_id
            .is_some_and(|old| old != intent_id)
        {
            return Err(AppError::conflict(
                "distribution request already linked to another intent",
            ));
        }
        let query = format!(
            "UPDATE content_distribution_requests SET publication_intent_id=$1 \
             WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 AND request_id=$5 \
             RETURNING {REQUEST_COLUMNS}"
        );
        let row = sqlx::query(&query)
            .bind(intent_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(request_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(read_request(&row))
    }

    async fn materialize(
        &self,
        scope: &TenantScope,
        request_id: Uuid,
    ) -> Result<ContentDistributionRequest, AppError> {
        let mut tx = self.transaction(scope).await?;
        let project_id = project(scope)?;
        // Same project lock/order as cycle materialize: separate requests
        // and coverage cells cannot race to create the same logical send.
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM projects WHERE operator_id=$1 AND tenant_id=$2 \
             AND project_id=$3 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let status = status.ok_or_else(|| AppError::not_found("project not found"))?;
        if matches!(status.as_str(), "paused" | "archived") {
            return Err(AppError::conflict("project is not active"));
        }
        let query = format!(
            "SELECT {REQUEST_COLUMNS} FROM content_distribution_requests \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND request_id=$4 FOR UPDATE"
        );
        let row = sqlx::query(&query)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(request_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
            .ok_or_else(|| AppError::not_found("distribution request not found"))?;
        let request = read_request(&row);
        if request.publication_intent_id.is_some() {
            tx.commit().await.map_err(db)?;
            return Ok(request);
        }
        if request.format != geo_domain::TEXT_DISTRIBUTION_FORMAT {
            return Err(AppError::conflict("publication format is not supported"));
        }
        live_request_account(&mut tx, scope, &request).await?;
        // An account login is not format proof. The independent persisted
        // connector evidence and current settings must both permit text.
        let semantic: Option<String> = sqlx::query_scalar(
            "SELECT entry->>'content_type' FROM content_revisions r \
             JOIN content_executions e ON (e.operator_id,e.tenant_id,e.project_id,e.execution_id)= \
                (r.operator_id,r.tenant_id,r.project_id,r.execution_id) \
             CROSS JOIN LATERAL jsonb_array_elements(COALESCE(e.state->'items','[]'::jsonb)) entry \
             WHERE r.operator_id=$1 AND r.tenant_id=$2 AND r.project_id=$3 \
                AND r.revision_id=$4 AND r.asset_id=$5 AND entry->>'asset_id'=$6 LIMIT 1",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(request.content_revision_id)
        .bind(request.content_asset_id)
        .bind(request.content_asset_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let configured = sqlx::query(
            "SELECT enabled,content_types FROM connector_capability_settings \
             WHERE operator_id=$1 AND platform_id=$2 AND placement_slot=$3 FOR SHARE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(&request.platform_id)
        .bind(&request.placement_slot)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(configured) = configured else {
            return Err(AppError::conflict("publication format is not available"));
        };
        if !configured.get::<bool, _>("enabled") {
            return Err(AppError::conflict("publication connector is disabled"));
        }
        let types: Vec<String> = decode(configured.get("content_types"))?;
        let proof_format = if types
            .iter()
            .any(|item| item == geo_domain::PLAIN_TEXT_ARTICLE_FORMAT)
        {
            geo_domain::PLAIN_TEXT_ARTICLE_FORMAT
        } else if semantic.as_deref().is_some_and(|semantic| {
            geo_domain::publication_format_for_semantic_type(semantic).is_some()
                && types.iter().any(|item| item == semantic)
        }) {
            semantic.as_deref().expect("recognized semantic type")
        } else {
            return Err(AppError::conflict("publication format is not available"));
        };
        let has_proof: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM connector_capability_verifications \
             WHERE operator_id=$1 AND platform_id=$2 AND placement_slot=$3 \
                AND content_type=$4)",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(&request.platform_id)
        .bind(&request.placement_slot)
        .bind(proof_format)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if !has_proof {
            return Err(AppError::conflict(
                "publication format has no verified connector",
            ));
        }
        let row = sqlx::query(
            "SELECT r.body AS revision_body,c.body AS check_body \
             FROM content_revisions r JOIN content_checks c ON \
                (c.operator_id,c.tenant_id,c.project_id,c.revision_id)= \
                (r.operator_id,r.tenant_id,r.project_id,r.revision_id) \
             WHERE r.operator_id=$1 AND r.tenant_id=$2 AND r.project_id=$3 \
                AND r.revision_id=$4 AND r.asset_id=$5",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(request.content_revision_id)
        .bind(request.content_asset_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("independent content check is missing"))?;
        let mut revision: ContentRevision = decode(row.get("revision_body"))?;
        let check: ContentCheck = decode(row.get("check_body"))?;
        if revision.revision_id != request.content_revision_id
            || revision.asset_id != request.content_asset_id
            || check.revision_id != revision.revision_id
            || check.findings.iter().any(|finding| finding.blocking)
        {
            return Err(AppError::conflict(
                "content revision has no passing independent check",
            ));
        }
        revision.findings = check.findings;
        revision.document.validate(&revision.evidence)?;
        if revision.markdown != revision.document.markdown() {
            return Err(AppError::conflict("content revision markdown differs"));
        }
        live_request_evidence(&mut tx, scope, &revision).await?;
        let placement = PlatformPlacement {
            platform_id: request.platform_id.clone(),
            placement_slot: request.placement_slot.clone(),
            capability_version: "request-text-v1".into(),
            supported_formats: vec![geo_domain::TEXT_DISTRIBUTION_FORMAT.into()],
            unavailable_reason: None,
            fixture: false,
        };
        let variant = prepare_variant(&revision, &placement)?;
        let (candidate, command) =
            prepare_request_publication_intent(scope, &request, &variant, chrono::Utc::now())?;
        let existing_variant: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT body FROM distribution_channel_variants WHERE operator_id=$1 \
             AND tenant_id=$2 AND project_id=$3 AND variant_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(variant.variant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if let Some(stored) = existing_variant {
            if decode::<ChannelVariant>(stored)? != variant {
                return Err(AppError::conflict("variant identity differs"));
            }
        } else {
            sqlx::query(
                "INSERT INTO distribution_channel_variants \
                 (variant_id,operator_id,tenant_id,project_id,content_revision_id,body) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(variant.variant_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(revision.revision_id)
            .bind(encode(&variant)?)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        let previous = sqlx::query(
            "SELECT body,verification,verification_evidence_id,origin_target_id,origin_request_id \
             FROM distribution_publication_intents \
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND logical_key=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(&candidate.logical_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let intent = if let Some(row) = previous {
            let mut prior: PublicationIntent = decode(row.get("body"))?;
            prior.verification = decode(serde_json::Value::String(row.get("verification")))?;
            prior.verification_evidence_id = row.get("verification_evidence_id");
            if prior.intent_id != candidate.intent_id
                || prior.project_id != candidate.project_id
                || prior.variant_id != candidate.variant_id
                || prior.content_revision_id != candidate.content_revision_id
                || prior.account_id != candidate.account_id
                || prior.platform_id != candidate.platform_id
                || prior.placement_slot != candidate.placement_slot
                || prior.payload_hash != candidate.payload_hash
                || (row.get::<Option<Uuid>, _>("origin_target_id").is_some()
                    == row.get::<Option<Uuid>, _>("origin_request_id").is_some())
                || prior.channel_target_id
                    != row
                        .get::<Option<Uuid>, _>("origin_target_id")
                        .unwrap_or(Uuid::nil())
            {
                return Err(AppError::conflict("existing publication identity differs"));
            }
            prior
        } else {
            sqlx::query(
                "INSERT INTO distribution_publication_intents \
                 (intent_id,operator_id,tenant_id,project_id,origin_request_id,variant_id,logical_key,body) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(candidate.intent_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(request_id)
            .bind(variant.variant_id)
            .bind(&candidate.logical_key)
            .bind(encode(&candidate)?)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            sqlx::query(
                "INSERT INTO distribution_publication_commands \
                 (command_id,operator_id,tenant_id,project_id,intent_id,origin_request_id,payload_hash,fixture) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,false)",
            )
            .bind(command.command_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(candidate.intent_id)
            .bind(request_id)
            .bind(&command.payload_hash)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            candidate
        };
        // Keep origin immutable: a second request merely links the original
        // intent/command, including when it is claimed, unknown or verified.
        validate_distribution_request_intent(&request, &intent, &variant)?;
        let query = format!(
            "UPDATE content_distribution_requests SET publication_intent_id=$1 \
             WHERE operator_id=$2 AND tenant_id=$3 AND project_id=$4 \
                AND request_id=$5 AND publication_intent_id IS NULL RETURNING {REQUEST_COLUMNS}"
        );
        let linked = sqlx::query(&query)
            .bind(intent.intent_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id)
            .bind(request_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(read_request(&linked))
    }

    async fn list_unlinked(
        &self,
        after_request_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ContentDistributionRequest>, AppError> {
        if !(1..=1000).contains(&limit) {
            return Err(AppError::invalid_request("invalid request scan page size"));
        }
        // Trusted internal scanner traverses every scope; the returned scope
        // comes from persisted rows, never caller-controlled selectors.
        let query = format!(
            "SELECT {REQUEST_COLUMNS} FROM content_distribution_requests \
             WHERE publication_intent_id IS NULL \
                AND ($1::uuid IS NULL OR request_id>$1) \
             ORDER BY request_id LIMIT $2"
        );
        let rows = sqlx::query(&query)
            .bind(after_request_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        Ok(rows.iter().map(read_request).collect())
    }
}
