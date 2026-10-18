//! The grant, source eligibility and exact media bytes share one transaction.
use geo_domain::{
    AppError, ContentItem, ContentRevision, MediaObjectKey, PartnerContentExport, PartnerPrincipal,
    ordered_media_snapshot_keys,
};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    PgContentRepository, content::snapshot_authorized_images_in_transaction, set_local_scope,
};

use super::{PartnerPermission, PgPartnerRepository, db, encoding, principal_allowed, scoped};

pub(super) async fn snapshot(
    repository: &PgPartnerRepository,
    principal: &PartnerPrincipal,
    revision_id: Uuid,
) -> Result<PartnerContentExport, AppError> {
    let scope = &principal.scope;
    let mut tx = repository.pool.begin().await.map_err(db)?;
    set_local_scope(&mut tx, scope).await.map_err(db)?;
    // Match content/media lock order; permission mutation locks only the connection.
    PgContentRepository::lock_project(&mut tx, scope).await?;
    if !principal_allowed(&mut tx, principal, PartnerPermission::ContentExport).await? {
        return Err(AppError::forbidden("content export is not permitted"));
    }
    let row = scoped(
        "SELECT r.asset_id,r.execution_id,r.body, \
           (SELECT item FROM jsonb_array_elements(e.state->'items') item \
            WHERE item->>'asset_id'=r.asset_id::text) AS item \
         FROM content_revisions r JOIN content_executions e \
           ON (e.operator_id,e.tenant_id,e.project_id,e.execution_id)= \
              (r.operator_id,r.tenant_id,r.project_id,r.execution_id) \
         WHERE r.operator_id=$1 AND r.tenant_id=$2 AND r.project_id=$3 AND r.revision_id=$4",
        scope,
    )?
    .bind(revision_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?
    .ok_or_else(|| AppError::not_found("content revision not found"))?;
    let revision: ContentRevision = serde_json::from_value(row.get("body")).map_err(encoding)?;
    let item: ContentItem = serde_json::from_value(
        row.get::<Option<serde_json::Value>, _>("item")
            .ok_or_else(|| AppError::conflict("content source binding is unavailable"))?,
    )
    .map_err(encoding)?;
    let execution_id: Uuid = row.get("execution_id");
    if revision.revision_id != revision_id || revision.asset_id != row.get::<Uuid, _>("asset_id") {
        return Err(AppError::conflict("content revision identity changed"));
    }
    PgContentRepository::validate_frozen_sources(
        &mut tx,
        scope,
        execution_id,
        &item.document_key,
        &item.source_version_refs,
        &revision.quotes,
    )
    .await?;
    let keys = ordered_media_snapshot_keys(
        &revision
            .document
            .media_references()
            .into_iter()
            .map(|media| MediaObjectKey {
                object_id: media.object_id,
                object_version: media.object_version,
                sha256: media.sha256.clone(),
            })
            .collect::<Vec<_>>(),
    )?;
    let media = snapshot_authorized_images_in_transaction(&mut tx, scope, &keys).await?;
    tx.commit().await.map_err(db)?;
    Ok(PartnerContentExport {
        revision,
        language: if item.language.is_empty() {
            None
        } else {
            Some(item.language)
        },
        media_keys: keys,
        media,
    })
}
