use async_trait::async_trait;
use geo_domain::{
    AcceptContentDistributionRequest, AppError, ChannelVariant, ContentDistributionRequest,
    ContentDistributionRequestRepository, ErrorCode, PublicationIntent, TenantScope,
    prepare_content_distribution_request, validate_distribution_request_intent,
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

fn read_request(row: &sqlx::postgres::PgRow) -> ContentDistributionRequest {
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

const REQUEST_COLUMNS: &str = "request_id,operator_id,tenant_id,project_id,schema_version,\
content_revision_id,content_asset_id,platform_id,placement_slot,account_id,account_owner_kind,\
format,idempotency_key_hash,request_hash,publication_intent_id,created_at";

#[async_trait]
impl ContentDistributionRequestRepository for PgContentDistributionRequestRepository {
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
}
