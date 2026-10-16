//! Durable acceptance followed by one atomic knowledge item per transaction.
mod admin;

pub use admin::PgPartnerAdminRepository;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ErrorCode, KnowledgePurpose, PartnerAdvance, PartnerDisposition, PartnerIssuedKey,
    PartnerKnowledgeRefs, PartnerPrincipal, PartnerRepository, PartnerSyncAction, PartnerSyncItem,
    PartnerSyncPage, PartnerSyncReceipt, PartnerSyncRequest, PartnerSyncResult, PartnerSyncStatus,
    PartnerSyncWork, SourceState, TenantScope, issue_partner_key, partner_sync_item_hash,
    partner_sync_request_hash, validate_partner_sync_item, validate_partner_sync_request,
    verify_partner_key,
};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    postgres::{PgArguments, PgRow},
    query::Query,
};
use uuid::Uuid;

use crate::{PgKnowledgeRepository, knowledge::PartnerTextSnapshot, set_local_scope};

#[derive(Clone)]
pub struct PgPartnerRepository {
    pool: PgPool,
}

fn db(_: sqlx::Error) -> AppError {
    AppError::new(
        ErrorCode::DependencyUnavailable,
        "partner synchronization storage unavailable",
    )
}

fn encoding(_: serde_json::Error) -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "partner synchronization record unavailable",
    )
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::forbidden("project scope required"))
}

fn scoped<'q>(
    sql: &'q str,
    scope: &TenantScope,
) -> Result<Query<'q, Postgres, PgArguments>, AppError> {
    Ok(sqlx::query(sql)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project(scope)?))
}

fn row_scope(row: &PgRow) -> TenantScope {
    TenantScope::new(
        row.get::<Uuid, _>("operator_id").into(),
        row.get::<Uuid, _>("tenant_id").into(),
        Some(row.get::<Uuid, _>("project_id").into()),
    )
}

fn receipt(row: &PgRow) -> Result<PartnerSyncReceipt, AppError> {
    let status = match row.get::<String, _>("status").as_str() {
        "pending" => PartnerSyncStatus::Pending,
        "running" => PartnerSyncStatus::Running,
        "completed" => PartnerSyncStatus::Completed,
        _ => {
            return Err(AppError::new(
                ErrorCode::Internal,
                "invalid synchronization state",
            ));
        }
    };
    Ok(PartnerSyncReceipt {
        operation_id: row.get("operation_id"),
        status,
        item_count: row.get::<i32, _>("item_count") as u32,
        completed_count: row.get::<i32, _>("completed_count") as u32,
    })
}

fn page_limit(limit: u32) -> Result<i64, AppError> {
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid_request(
            "page size must be between 1 and 100",
        ));
    }
    Ok(i64::from(limit))
}

async fn advisory(tx: &mut Transaction<'_, Postgres>, identity: &str) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(identity)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    Ok(())
}

/// SHARE (not KEY SHARE) also serializes non-key permission/revocation updates.
async fn connection_allowed(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    connection: Uuid,
) -> Result<bool, AppError> {
    Ok(scoped(
        "SELECT knowledge_write AND revoked_at IS NULL AS allowed FROM partner_connections \
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 FOR SHARE",
        scope,
    )?
    .bind(connection)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .is_some_and(|row| row.get::<bool, _>("allowed")))
}

async fn principal_allowed(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PartnerPrincipal,
) -> Result<bool, AppError> {
    let connection = connection_allowed(tx, &principal.scope, principal.connection_id).await?;
    let key = scoped(
        "SELECT revoked_at IS NULL AND (expires_at IS NULL OR expires_at>clock_timestamp()) AS allowed \
         FROM partner_keys WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
         AND connection_id=$4 AND key_id=$5 FOR SHARE", &principal.scope)?
        .bind(principal.connection_id).bind(principal.key_id)
        .fetch_optional(&mut **tx).await.map_err(db)?
        .is_some_and(|row| row.get::<bool, _>("allowed"));
    Ok(connection && key)
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    connection: Uuid,
    key: Option<Uuid>,
    event: &str,
) -> Result<(), AppError> {
    scoped(
        "INSERT INTO partner_audit_events \
        (operator_id,tenant_id,project_id,connection_id,key_id,event,audit_id) \
        VALUES ($1,$2,$3,$4,$5,$6,$7)",
        scope,
    )?
    .bind(connection)
    .bind(key)
    .bind(event)
    .bind(Uuid::new_v4())
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

impl PgPartnerRepository {
    pub fn from_database(database: &crate::Database) -> Self {
        Self {
            pool: database.pool().clone(),
        }
    }

    /// Internal service operation; its caller must authorize administration.
    pub async fn create_connection(&self, scope: &TenantScope) -> Result<Uuid, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        let connection = Uuid::new_v4();
        scoped(
            "INSERT INTO partner_connections(operator_id,tenant_id,project_id,connection_id) \
            VALUES ($1,$2,$3,$4)",
            scope,
        )?
        .bind(connection)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        audit(&mut tx, scope, connection, None, "connection_created").await?;
        tx.commit().await.map_err(db)?;
        Ok(connection)
    }

    pub async fn issue_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<PartnerIssuedKey, AppError> {
        self.issue_or_rotate(scope, connection, None, expires_at)
            .await
    }

    pub async fn rotate_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        old_key: Uuid,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<PartnerIssuedKey, AppError> {
        self.issue_or_rotate(scope, connection, Some(old_key), expires_at)
            .await
    }

    async fn issue_or_rotate(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        old_key: Option<Uuid>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<PartnerIssuedKey, AppError> {
        if expires_at.is_some_and(|expiry| expiry <= Utc::now()) {
            return Err(AppError::invalid_request(
                "service key expiry must be in the future",
            ));
        }
        let (issued, hash) = issue_partner_key()?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        if !connection_allowed(&mut tx, scope, connection).await? {
            return Err(AppError::forbidden("connection unavailable"));
        }
        if let Some(old_key) = old_key {
            revoke_key_tx(&mut tx, scope, connection, old_key).await?;
        }
        scoped(
            "INSERT INTO partner_keys \
            (operator_id,tenant_id,project_id,connection_id,key_id,verification_hash,expires_at) \
            VALUES ($1,$2,$3,$4,$5,$6,$7)",
            scope,
        )?
        .bind(connection)
        .bind(issued.key_id)
        .bind(hash)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        audit(
            &mut tx,
            scope,
            connection,
            Some(issued.key_id),
            "key_issued",
        )
        .await?;
        tx.commit().await.map_err(db)?;
        Ok(issued)
    }

    pub async fn revoke_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        key: Uuid,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        // Keep the same connection->key lock order as intake and the worker.
        connection_allowed(&mut tx, scope, connection).await?;
        revoke_key_tx(&mut tx, scope, connection, key).await?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    pub async fn revoke_connection(
        &self,
        scope: &TenantScope,
        connection: Uuid,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        let changed = scoped(
            "UPDATE partner_connections SET revoked_at=COALESCE(revoked_at,now()) \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4",
            scope,
        )?
        .bind(connection)
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected();
        if changed == 0 {
            return Err(AppError::not_found("connection not found"));
        }
        audit(&mut tx, scope, connection, None, "connection_revoked").await?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn advance_transaction(
        &self,
        work: &PartnerSyncWork,
    ) -> Result<PartnerAdvance, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, &work.scope).await.map_err(db)?;
        let key = scoped(
            "SELECT accepted_key_id FROM partner_sync_operations \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 \
            AND connection_id=$4 AND operation_id=$5",
            &work.scope,
        )?
        .bind(work.connection_id)
        .bind(work.operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("synchronization not found"))?
        .get::<Uuid, _>("accepted_key_id");
        let authorized = principal_allowed(
            &mut tx,
            &PartnerPrincipal {
                scope: work.scope.clone(),
                connection_id: work.connection_id,
                key_id: key,
            },
        )
        .await?;
        let Some(row) = scoped(
            "SELECT *,next_attempt_at<=clock_timestamp() AS due FROM partner_sync_operations \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
            AND operation_id=$5 FOR UPDATE SKIP LOCKED",
            &work.scope,
        )?
        .bind(work.connection_id)
        .bind(work.operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        else {
            return Ok(PartnerAdvance::Deferred);
        };
        let current = receipt(&row)?;
        if current.status == PartnerSyncStatus::Completed {
            return Ok(PartnerAdvance::Completed);
        }
        if !row.get::<bool, _>("due") {
            return Ok(PartnerAdvance::Deferred);
        }
        let row = scoped(
            "SELECT item_index,input,intake_reason_code FROM partner_sync_items \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
            AND operation_id=$5 AND result IS NULL ORDER BY item_index LIMIT 1 FOR UPDATE",
            &work.scope,
        )?
        .bind(work.connection_id)
        .bind(work.operation_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let index = row.get::<i32, _>("item_index") as u32;
        let item: PartnerSyncItem = serde_json::from_value(row.get("input")).map_err(encoding)?;
        let intake_reason: Option<String> = row.get("intake_reason_code");
        let result = if !authorized {
            rejected(index, "authorization_revoked")
        } else if let Some(reason) = intake_reason {
            rejected(index, &reason)
        } else if let Err(reason) = validate_partner_sync_item(&item) {
            rejected(index, reason)
        } else {
            let Some(result) = apply_item(&mut tx, work, index, &item).await? else {
                // Rotate dependency-blocked operations out of the due page;
                // otherwise they could starve their older, backed-off owner.
                scoped("UPDATE partner_sync_operations SET next_attempt_at=clock_timestamp()+interval '5 seconds' \
                    WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 AND operation_id=$5",
                    &work.scope)?
                    .bind(work.connection_id).bind(work.operation_id)
                    .execute(&mut *tx).await.map_err(db)?;
                tx.commit().await.map_err(db)?;
                return Ok(PartnerAdvance::Deferred);
            };
            result
        };
        // Only the original owner can finalize a claim. Rejected credentials
        // still unblock descendants without discarding the conservative fence.
        scoped(
            "UPDATE partner_source_intake_claims SET result=$7 \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
            AND operation_id=$5 AND item_index=$6 AND result IS NULL",
            &work.scope,
        )?
        .bind(work.connection_id)
        .bind(work.operation_id)
        .bind(index as i32)
        .bind(serde_json::to_value(&result).map_err(encoding)?)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        scoped(
            "UPDATE partner_sync_items SET result=$7,completed_at=now() \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
            AND operation_id=$5 AND item_index=$6 AND result IS NULL",
            &work.scope,
        )?
        .bind(work.connection_id)
        .bind(work.operation_id)
        .bind(index as i32)
        .bind(serde_json::to_value(&result).map_err(encoding)?)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let completed = current.completed_count + 1 == current.item_count;
        scoped("UPDATE partner_sync_operations SET completed_count=completed_count+1, \
            status=$6,updated_at=now(),next_attempt_at=now(),failure_count=0,reason_code=NULL \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 AND operation_id=$5",
            &work.scope)?
            .bind(work.connection_id).bind(work.operation_id)
            .bind(if completed { "completed" } else { "running" })
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(PartnerAdvance::ItemCommitted {
            index,
            operation_completed: completed,
        })
    }

    async fn defer_failure(&self, work: &PartnerSyncWork) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, &work.scope).await.map_err(db)?;
        scoped("UPDATE partner_sync_operations SET failure_count=LEAST(failure_count+1,30), \
            next_attempt_at=clock_timestamp()+make_interval(secs=>LEAST(300,5*power(2,LEAST(failure_count,6)))::double precision), \
            reason_code='storage_unavailable',updated_at=now() \
            WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 AND operation_id=$5 \
            AND status<>'completed'", &work.scope)?
            .bind(work.connection_id).bind(work.operation_id)
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }
}

async fn revoke_key_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    connection: Uuid,
    key: Uuid,
) -> Result<(), AppError> {
    let changed = scoped(
        "UPDATE partner_keys SET revoked_at=COALESCE(revoked_at,now()) \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 AND key_id=$5",
        scope,
    )?
    .bind(connection)
    .bind(key)
    .execute(&mut **tx)
    .await
    .map_err(db)?
    .rows_affected();
    if changed == 0 {
        return Err(AppError::not_found("service key not found"));
    }
    audit(tx, scope, connection, Some(key), "key_revoked").await
}

fn rejected(index: u32, reason: &str) -> PartnerSyncResult {
    PartnerSyncResult {
        index,
        disposition: PartnerDisposition::Rejected,
        reason_code: Some(reason.to_owned()),
        resources: PartnerKnowledgeRefs::default(),
    }
}

/// Reserve restrictive authority without parsing. All source locks precede
/// any helper's release lock, including multi-source batches.
async fn reserve_intake(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PartnerPrincipal,
    operation: Uuid,
    request: &PartnerSyncRequest,
) -> Result<(), AppError> {
    let scope = &principal.scope;
    let source_names = request
        .items
        .iter()
        .filter(|item| validate_partner_sync_item(item).is_ok())
        .map(|item| item.external_source_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    for name in &source_names {
        let identity =
            serde_json::to_string(&("partner-source", scope, principal.connection_id, name))
                .map_err(encoding)?;
        advisory(tx, &identity).await?;
    }
    let rows = scoped(
        "SELECT source_id FROM partner_source_mappings \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
        AND external_source_id=ANY($5) AND source_id IS NOT NULL",
        scope,
    )?
    .bind(principal.connection_id)
    .bind(&source_names)
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;
    let source_ids = rows
        .iter()
        .map(|row| row.get::<Uuid, _>("source_id"))
        .collect::<Vec<_>>();
    scoped(
        "SELECT source_id FROM knowledge_sources WHERE operator_id=$1 AND tenant_id=$2 \
        AND project_id=$3 AND source_id=ANY($4) ORDER BY source_id FOR UPDATE",
        scope,
    )?
    .bind(&source_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;
    for (index, item) in request.items.iter().enumerate() {
        let reason = if let Err(reason) = validate_partner_sync_item(item) {
            Some(reason)
        } else {
            reserve_item(tx, principal, operation, index as u32, item).await?
        };
        if let Some(reason) = reason {
            scoped(
                "UPDATE partner_sync_items SET intake_reason_code=$7 \
                WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
                AND operation_id=$5 AND item_index=$6",
                scope,
            )?
            .bind(principal.connection_id)
            .bind(operation)
            .bind(index as i32)
            .bind(reason)
            .execute(&mut **tx)
            .await
            .map_err(db)?;
        }
    }
    Ok(())
}

async fn reserve_item(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PartnerPrincipal,
    operation: Uuid,
    index: u32,
    item: &PartnerSyncItem,
) -> Result<Option<&'static str>, AppError> {
    let scope = &principal.scope;
    let hash = partner_sync_item_hash(item)?;
    let existing = scoped(
        "SELECT semantic_hash FROM partner_source_intake_claims \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
        AND external_source_id=$5 AND (external_version=$6 OR source_sequence=$7)",
        scope,
    )?
    .bind(principal.connection_id)
    .bind(&item.external_source_id)
    .bind(&item.external_version)
    .bind(item.source_sequence)
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;
    if !existing.is_empty() {
        return Ok(
            if existing.len() == 1 && existing[0].get::<String, _>("semantic_hash") == hash {
                None
            } else {
                Some("source_identity_conflict")
            },
        );
    }
    let mapping = scoped(
        "SELECT source_id,reserved_sequence FROM partner_source_mappings \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
        AND external_source_id=$5 FOR UPDATE",
        scope,
    )?
    .bind(principal.connection_id)
    .bind(&item.external_source_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    if mapping
        .as_ref()
        .is_some_and(|row| row.get::<i64, _>("reserved_sequence") >= item.source_sequence)
    {
        return Ok(Some("stale_source_sequence"));
    }
    let source = mapping
        .as_ref()
        .and_then(|row| row.get::<Option<Uuid>, _>("source_id"));
    let internal = matches!(
        &item.action,
        PartnerSyncAction::Upsert {
            purpose: KnowledgePurpose::Internal,
            ..
        }
    );
    let revoke = matches!(&item.action, PartnerSyncAction::Revoke {});
    scoped("INSERT INTO partner_source_mappings \
        (operator_id,tenant_id,project_id,connection_id,external_source_id,accepted_sequence,\
        reserved_sequence,internal_fence_sequence,revoke_fence_sequence,revoked) \
        VALUES($1,$2,$3,$4,$5,0,$6,$7,$8,$9) \
        ON CONFLICT(operator_id,tenant_id,project_id,connection_id,external_source_id) DO UPDATE SET \
        reserved_sequence=EXCLUDED.reserved_sequence,\
        internal_fence_sequence=GREATEST(partner_source_mappings.internal_fence_sequence,EXCLUDED.internal_fence_sequence),\
        revoke_fence_sequence=GREATEST(partner_source_mappings.revoke_fence_sequence,EXCLUDED.revoke_fence_sequence),\
        revoked=partner_source_mappings.revoked OR EXCLUDED.revoked", scope)?
        .bind(principal.connection_id).bind(&item.external_source_id).bind(item.source_sequence)
        .bind(internal.then_some(item.source_sequence)).bind(revoke.then_some(item.source_sequence)).bind(revoke)
        .execute(&mut **tx).await.map_err(db)?;
    scoped("INSERT INTO partner_source_intake_claims \
        (operator_id,tenant_id,project_id,connection_id,external_source_id,external_version,\
        source_sequence,semantic_hash,operation_id,item_index) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)", scope)?
        .bind(principal.connection_id).bind(&item.external_source_id).bind(&item.external_version)
        .bind(item.source_sequence).bind(hash).bind(operation).bind(index as i32)
        .execute(&mut **tx).await.map_err(db)?;
    if let Some(source) = source.filter(|_| internal || revoke) {
        PgKnowledgeRepository::restrict_partner_source_in_transaction(
            tx,
            scope,
            source,
            internal.then_some(KnowledgePurpose::Internal),
            revoke,
        )
        .await
        .map_err(|_| {
            AppError::new(
                ErrorCode::DependencyUnavailable,
                "partner synchronization storage unavailable",
            )
        })?;
    }
    Ok(None)
}

async fn apply_item(
    tx: &mut Transaction<'_, Postgres>,
    work: &PartnerSyncWork,
    index: u32,
    item: &PartnerSyncItem,
) -> Result<Option<PartnerSyncResult>, AppError> {
    let scope = &work.scope;
    // Hash the typed tuple to avoid ambiguous separator concatenations.
    let identity = serde_json::to_string(&(
        "partner-source",
        scope,
        work.connection_id,
        &item.external_source_id,
    ))
    .map_err(encoding)?;
    advisory(tx, &identity).await?;
    let semantic_hash = partner_sync_item_hash(item)?;
    let claim = scoped(
        "SELECT * FROM partner_source_intake_claims \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
        AND external_source_id=$5 AND external_version=$6 AND source_sequence=$7",
        scope,
    )?
    .bind(work.connection_id)
    .bind(&item.external_source_id)
    .bind(&item.external_version)
    .bind(item.source_sequence)
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    if claim.get::<String, _>("semantic_hash") != semantic_hash {
        return Ok(Some(rejected(index, "source_identity_conflict")));
    }
    if let Some(result) = claim.get::<Option<serde_json::Value>, _>("result") {
        let mut original: PartnerSyncResult = serde_json::from_value(result).map_err(encoding)?;
        original.index = index;
        if original.disposition != PartnerDisposition::Rejected {
            original.disposition = PartnerDisposition::Unchanged;
        }
        return Ok(Some(original));
    }
    if claim.get::<Uuid, _>("operation_id") != work.operation_id
        || claim.get::<i32, _>("item_index") != index as i32
    {
        return Ok(None);
    }
    let lower_pending = scoped(
        "SELECT 1 FROM partner_source_intake_claims \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
        AND external_source_id=$5 AND source_sequence<$6 AND result IS NULL LIMIT 1",
        scope,
    )?
    .bind(work.connection_id)
    .bind(&item.external_source_id)
    .bind(item.source_sequence)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .is_some();
    if lower_pending {
        return Ok(None);
    }
    let mapping = scoped(
        "SELECT source_id,accepted_sequence,internal_fence_sequence,revoke_fence_sequence FROM partner_source_mappings \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 \
        AND external_source_id=$5 FOR UPDATE",
        scope,
    )?
    .bind(work.connection_id)
    .bind(&item.external_source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    if mapping.get::<i64, _>("accepted_sequence") >= item.source_sequence {
        return Ok(Some(rejected(index, "stale_source_sequence")));
    }
    let source_id = mapping.get::<Option<Uuid>, _>("source_id");
    let pending_internal = mapping
        .get::<Option<i64>, _>("internal_fence_sequence")
        .is_some_and(|sequence| sequence > item.source_sequence);
    let pending_revoke = mapping
        .get::<Option<i64>, _>("revoke_fence_sequence")
        .is_some_and(|sequence| sequence > item.source_sequence);
    if let Some(source_id) = source_id {
        scoped(
            "SELECT source_id FROM knowledge_sources WHERE operator_id=$1 AND tenant_id=$2 \
            AND project_id=$3 AND source_id=$4 FOR UPDATE",
            scope,
        )?
        .bind(source_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?;
    }
    sqlx::query("SAVEPOINT partner_knowledge_item")
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    let applied = match &item.action {
        PartnerSyncAction::Upsert {
            name,
            purpose,
            media_type,
            text,
            ..
        } => {
            PgKnowledgeRepository::apply_partner_text_in_transaction(
                tx,
                scope,
                source_id,
                PartnerTextSnapshot {
                    name,
                    purpose: *purpose,
                    effective_purpose: if pending_internal {
                        KnowledgePurpose::Internal
                    } else {
                        *purpose
                    },
                    effective_state: if pending_revoke {
                        SourceState::Removed
                    } else {
                        SourceState::Active
                    },
                    media_type,
                    text,
                    effective_at: item.effective_at,
                },
            )
            .await
        }
        PartnerSyncAction::Revoke {} => match source_id {
            Some(source) => {
                PgKnowledgeRepository::revoke_partner_source_in_transaction(tx, scope, source).await
            }
            None => Ok(PartnerKnowledgeRefs::default()),
        },
    };
    let resources = match applied {
        Ok(resources) => {
            sqlx::query("RELEASE SAVEPOINT partner_knowledge_item")
                .execute(&mut **tx)
                .await
                .map_err(db)?;
            resources
        }
        Err(error)
            if matches!(
                error.code,
                ErrorCode::InvalidRequest | ErrorCode::Conflict | ErrorCode::NotFound
            ) =>
        {
            sqlx::query("ROLLBACK TO SAVEPOINT partner_knowledge_item")
                .execute(&mut **tx)
                .await
                .map_err(db)?;
            sqlx::query("RELEASE SAVEPOINT partner_knowledge_item")
                .execute(&mut **tx)
                .await
                .map_err(db)?;
            return Ok(Some(rejected(index, "knowledge_update_rejected")));
        }
        // Older knowledge paths include database details in their errors.
        // Never pass those across the partner/runner boundary.
        Err(_) => {
            return Err(AppError::new(
                ErrorCode::DependencyUnavailable,
                "partner synchronization storage unavailable",
            ));
        }
    };
    scoped("UPDATE partner_source_mappings SET source_id=$6,accepted_sequence=$7,revoked=$8 \
        WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND connection_id=$4 AND external_source_id=$5", scope)?
        .bind(work.connection_id).bind(&item.external_source_id)
        .bind(resources.source_id.or(source_id)).bind(item.source_sequence)
        .bind(matches!(item.action, PartnerSyncAction::Revoke {}) || pending_revoke)
        .execute(&mut **tx).await.map_err(db)?;
    scoped("INSERT INTO partner_source_events \
        (operator_id,tenant_id,project_id,connection_id,external_source_id,external_version,source_sequence,\
        semantic_hash,operation_id,item_index,source_id,source_version_id,import_job_id,knowledge_release_id) \
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)", scope)?
        .bind(work.connection_id).bind(&item.external_source_id).bind(&item.external_version)
        .bind(item.source_sequence).bind(semantic_hash).bind(work.operation_id).bind(index as i32)
        .bind(resources.source_id).bind(resources.source_version_id).bind(resources.import_job_id)
        .bind(resources.knowledge_release_id).execute(&mut **tx).await.map_err(db)?;
    Ok(Some(PartnerSyncResult {
        index,
        disposition: PartnerDisposition::Accepted,
        reason_code: None,
        resources,
    }))
}

#[async_trait]
impl PartnerRepository for PgPartnerRepository {
    async fn authenticate(
        &self,
        connection_id: Uuid,
        key_id: Uuid,
        secret: &str,
    ) -> Result<PartnerPrincipal, AppError> {
        // Discovery is constrained by unguessable connection and key IDs. The
        // verified scope is reapplied transaction-locally for every operation.
        let row = sqlx::query(
            "SELECT k.operator_id,k.tenant_id,k.project_id,k.verification_hash \
            FROM partner_keys k JOIN partner_connections c ON \
            (c.operator_id,c.tenant_id,c.project_id,c.connection_id)=\
            (k.operator_id,k.tenant_id,k.project_id,k.connection_id) \
            WHERE k.connection_id=$1 AND k.key_id=$2 AND k.revoked_at IS NULL \
            AND (k.expires_at IS NULL OR k.expires_at>clock_timestamp()) \
            AND c.revoked_at IS NULL AND c.knowledge_write",
        )
        .bind(connection_id)
        .bind(key_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::unauthorized("invalid service credential"))?;
        if !verify_partner_key(secret, &row.get::<String, _>("verification_hash")) {
            return Err(AppError::unauthorized("invalid service credential"));
        }
        Ok(PartnerPrincipal {
            scope: row_scope(&row),
            connection_id,
            key_id,
        })
    }

    async fn accept(
        &self,
        principal: &PartnerPrincipal,
        idempotency_key: &str,
        request: PartnerSyncRequest,
    ) -> Result<PartnerSyncReceipt, AppError> {
        validate_partner_sync_request(&request)?;
        if idempotency_key.trim().is_empty()
            || idempotency_key.len() > 256
            || idempotency_key.chars().any(char::is_control)
        {
            return Err(AppError::invalid_request("invalid idempotency key"));
        }
        let hash = partner_sync_request_hash(&request)?;
        let key_hash = hex::encode(Sha256::digest(idempotency_key.as_bytes()));
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, &principal.scope)
            .await
            .map_err(db)?;
        if !principal_allowed(&mut tx, principal).await? {
            return Err(AppError::unauthorized("invalid service credential"));
        }
        let identity = serde_json::to_string(&(
            "partner-operation",
            &principal.scope,
            principal.connection_id,
            &key_hash,
        ))
        .map_err(encoding)?;
        advisory(&mut tx, &identity).await?;
        let existing = scoped(
            "SELECT * FROM partner_sync_operations WHERE operator_id=$1 AND tenant_id=$2 \
            AND project_id=$3 AND connection_id=$4 AND idempotency_hash=$5",
            &principal.scope,
        )?
        .bind(principal.connection_id)
        .bind(&key_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if let Some(existing) = existing {
            if existing.get::<String, _>("request_hash") != hash {
                return Err(AppError::conflict("idempotency key has different input"));
            }
            let receipt = receipt(&existing)?;
            tx.commit().await.map_err(db)?;
            return Ok(receipt);
        }
        let operation = Uuid::new_v4();
        scoped("INSERT INTO partner_sync_operations \
            (operator_id,tenant_id,project_id,connection_id,operation_id,accepted_key_id,idempotency_hash,request_hash,item_count) \
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)", &principal.scope)?
            .bind(principal.connection_id).bind(operation).bind(principal.key_id)
            .bind(key_hash).bind(hash).bind(request.items.len() as i32)
            .execute(&mut *tx).await.map_err(db)?;
        for (index, item) in request.items.iter().enumerate() {
            scoped(
                "INSERT INTO partner_sync_items \
                (operator_id,tenant_id,project_id,connection_id,operation_id,item_index,input) \
                VALUES ($1,$2,$3,$4,$5,$6,$7)",
                &principal.scope,
            )?
            .bind(principal.connection_id)
            .bind(operation)
            .bind(index as i32)
            .bind(serde_json::to_value(item).map_err(encoding)?)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        reserve_intake(&mut tx, principal, operation, &request).await?;
        scoped("INSERT INTO outbox_events \
            (operator_id,tenant_id,project_id,event_id,event_type,schema_version,aggregate_id,aggregate_version,occurred_at,correlation_id,payload) \
            VALUES ($1,$2,$3,$4,'partner.knowledge_sync.accepted',1,$5,1,now(),$5,'{}'::jsonb)", &principal.scope)?
            .bind(Uuid::new_v4()).bind(operation).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(PartnerSyncReceipt {
            operation_id: operation,
            status: PartnerSyncStatus::Pending,
            item_count: request.items.len() as u32,
            completed_count: 0,
        })
    }

    async fn results(
        &self,
        principal: &PartnerPrincipal,
        operation_id: Uuid,
        after_index: Option<u32>,
        limit: u32,
    ) -> Result<PartnerSyncPage, AppError> {
        let limit = page_limit(limit)?;
        if after_index.is_some_and(|index| index >= 100) {
            return Err(AppError::invalid_request("invalid result cursor"));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, &principal.scope)
            .await
            .map_err(db)?;
        if !principal_allowed(&mut tx, principal).await? {
            return Err(AppError::unauthorized("invalid service credential"));
        }
        // SHARE prevents the worker advancing between receipt and result reads.
        let row = scoped(
            "SELECT * FROM partner_sync_operations WHERE operator_id=$1 AND tenant_id=$2 \
            AND project_id=$3 AND connection_id=$4 AND operation_id=$5 FOR SHARE",
            &principal.scope,
        )?
        .bind(principal.connection_id)
        .bind(operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("synchronization not found"))?;
        let operation = receipt(&row)?;
        let rows = scoped(
            "SELECT result FROM partner_sync_items WHERE operator_id=$1 AND tenant_id=$2 \
            AND project_id=$3 AND connection_id=$4 AND operation_id=$5 AND item_index>$6 \
            AND result IS NOT NULL ORDER BY item_index LIMIT $7",
            &principal.scope,
        )?
        .bind(principal.connection_id)
        .bind(operation_id)
        .bind(after_index.map(|n| n as i32).unwrap_or(-1))
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let mut results = rows
            .iter()
            .map(|row| serde_json::from_value::<PartnerSyncResult>(row.get("result")))
            .collect::<Result<Vec<_>, _>>()
            .map_err(encoding)?;
        let has_more = results.len() > limit as usize;
        results.truncate(limit as usize);
        let next_index = if has_more {
            results.last().map(|item| item.index)
        } else {
            None
        };
        tx.commit().await.map_err(db)?;
        Ok(PartnerSyncPage {
            operation,
            results,
            next_index,
        })
    }

    async fn candidates(&self, limit: u32) -> Result<Vec<PartnerSyncWork>, AppError> {
        let limit = page_limit(limit)?;
        let rows = sqlx::query("SELECT operator_id,tenant_id,project_id,connection_id,operation_id \
            FROM partner_sync_operations WHERE status<>'completed' AND next_attempt_at<=clock_timestamp() \
            ORDER BY next_attempt_at,operation_id LIMIT $1").bind(limit)
            .fetch_all(&self.pool).await.map_err(db)?;
        Ok(rows
            .iter()
            .map(|row| PartnerSyncWork {
                scope: row_scope(row),
                connection_id: row.get("connection_id"),
                operation_id: row.get("operation_id"),
            })
            .collect())
    }

    async fn advance_one(&self, work: &PartnerSyncWork) -> Result<PartnerAdvance, AppError> {
        match self.advance_transaction(work).await {
            Ok(result) => Ok(result),
            Err(error) => {
                if matches!(
                    error.code,
                    ErrorCode::DependencyUnavailable | ErrorCode::Internal
                ) {
                    // The item transaction is already dropped/rolled back.
                    // A durable retry time survives runner/process restarts.
                    self.defer_failure(work).await?;
                }
                Err(error)
            }
        }
    }
}
