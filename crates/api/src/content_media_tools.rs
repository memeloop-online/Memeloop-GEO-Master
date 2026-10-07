//! AI media operations over the same scoped attachment, image and content services as HTTP.

use geo_domain::{
    AppError, AttachmentReference, ContentBlock, ContentBlockKind, ContentItem,
    ContentMediaBinding, ContentMediaBindingState, ContentRevision, ErrorCode, MediaReference,
    RichContent, RichNode, TenantScope,
};
use geo_worker::{
    ContentDocumentReadRequest, ContentDocumentSnapshot, ContentMediaBindRequest,
    ContentMediaInsertReceipt, ContentMediaInsertRequest, ContentMediaListRequest,
    ContentMediaPage, ContentMediaRef,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;

const MAX_DOCUMENT_READ_BYTES: usize = 256 * 1024;
const DEFAULT_LIST_LIMIT: usize = 10;
const MAX_LIST_LIMIT: usize = 25;

fn media_ref(binding: ContentMediaBinding) -> ContentMediaRef {
    ContentMediaRef {
        binding_id: binding.binding_id,
        key: binding.image.key,
        media_type: binding.image.media_type,
        byte_len: binding.image.byte_len,
        width: binding.image.width,
        height: binding.image.height,
    }
}

pub(crate) async fn list(
    state: &AppState,
    scope: &TenantScope,
    request: ContentMediaListRequest,
) -> Result<ContentMediaPage, AppError> {
    request
        .validate()
        .map_err(|reason| AppError::invalid_request(&reason))?;
    let limit = request.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(AppError::invalid_request("limit must be between 1 and 25"));
    }
    let page = crate::content_media::binding_page(state, scope, request.after, Some(limit)).await?;
    Ok(ContentMediaPage {
        items: page.items.into_iter().map(media_ref).collect(),
        next_cursor: page.next_cursor,
    })
}

pub(crate) async fn bind(
    state: &AppState,
    scope: &TenantScope,
    request: ContentMediaBindRequest,
    attachments: &[AttachmentReference],
) -> Result<ContentMediaRef, AppError> {
    if request.attachment_id.is_nil() {
        return Err(AppError::invalid_request(
            "attachment reference must be non-zero",
        ));
    }
    let bound = attachments
        .iter()
        .find(|attachment| attachment.attachment_id.as_uuid() == request.attachment_id)
        .ok_or_else(|| AppError::forbidden("attachment is not bound to this turn"))?;
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let scope = crate::content::scoped(state, scope, project_id).await?;
    let (object, filename) = state
        .knowledge_repository()
        .get_attachment_object(&scope, request.attachment_id)
        .await?
        .ok_or_else(|| AppError::not_found("attachment not found"))?;
    if bound.object_id != object.object_id.to_string()
        || bound.filename != filename
        || bound.object_version.as_deref() != Some(object.object_version.to_string().as_str())
        || bound.sha256.as_deref() != Some(object.sha256.as_str())
        || bound.size_bytes != Some(object.actual_size)
        || bound.media_type.as_deref() != Some(object.detected_media_type.as_str())
    {
        return Err(AppError::conflict(
            "attachment binding no longer matches the committed object",
        ));
    }
    let key = geo_domain::MediaObjectKey {
        object_id: object.object_id,
        object_version: object.object_version,
        sha256: object.sha256,
    };
    crate::content_media::bind_image(state, &scope, key)
        .await
        .map(media_ref)
}

struct LocatedItem {
    item: ContentItem,
    current_revision_id: Uuid,
    asset_id: Uuid,
    is_reused: bool,
}

async fn locate(
    state: &AppState,
    scope: &TenantScope,
    execution_id: Uuid,
    item_id: Uuid,
) -> Result<LocatedItem, AppError> {
    let repository = state.content_service().repository();
    let execution = repository
        .get_execution(scope, execution_id)
        .await?
        .ok_or_else(|| AppError::not_found("content execution not found"))?;
    if Some(execution.project_id) != scope.project_id {
        return Err(AppError::not_found("content execution not found"));
    }
    let item = repository
        .get_item(scope, execution_id, item_id)
        .await?
        .ok_or_else(|| AppError::not_found("content item not found"))?;
    let reuse = item.reuse_binding.as_ref();
    let is_reused = reuse.is_some();
    let asset_id = item
        .asset_id
        .or_else(|| reuse.map(|binding| binding.asset_id))
        .ok_or_else(|| AppError::conflict("content asset missing"))?;
    let current_revision_id = item
        .current_revision_id
        .or_else(|| reuse.map(|binding| binding.revision_id))
        .ok_or_else(|| AppError::conflict("content revision missing"))?;
    // A reused item owns no destination asset until fork. The origin must be
    // precisely the pinned binding, not an asset selected by the model.
    if let Some(binding) = reuse {
        if binding.asset_id != asset_id || binding.revision_id != current_revision_id {
            return Err(AppError::conflict("reused binding no longer matches item"));
        }
    } else {
        let asset = repository
            .get_asset(scope, asset_id)
            .await?
            .ok_or_else(|| AppError::not_found("content asset not found"))?;
        if asset.execution_id != execution_id
            || asset.item_id != item_id
            || asset.current_revision_id != current_revision_id
        {
            return Err(AppError::conflict("content asset no longer matches item"));
        }
    }
    Ok(LocatedItem {
        item,
        current_revision_id,
        asset_id,
        is_reused,
    })
}

pub(crate) async fn read(
    state: &AppState,
    scope: &TenantScope,
    request: ContentDocumentReadRequest,
) -> Result<ContentDocumentSnapshot, AppError> {
    request
        .validate()
        .map_err(|reason| AppError::invalid_request(&reason))?;
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let scope = crate::content::scoped(state, scope, project_id).await?;
    let located = locate(state, &scope, request.execution_id, request.item_id).await?;
    let revision_id = request.revision_id.unwrap_or(located.current_revision_id);
    let revision = state
        .content_service()
        .repository()
        .get_revision(&scope, located.asset_id, revision_id)
        .await?
        .filter(|revision| {
            revision.asset_id == located.asset_id && revision.revision_id == revision_id
        })
        .ok_or_else(|| AppError::not_found("content revision not found"))?;
    if serde_json::to_vec(&revision.document)
        .map_err(|_| AppError::new(ErrorCode::Internal, "document serialization failed"))?
        .len()
        > MAX_DOCUMENT_READ_BYTES
    {
        return Err(AppError::invalid_request(
            "content document is too large to read in conversation",
        ));
    }
    Ok(ContentDocumentSnapshot {
        execution_id: request.execution_id,
        item_id: request.item_id,
        asset_id: located.asset_id,
        revision_id,
        current_revision_id: located.current_revision_id,
        revision: revision.revision,
        is_reused: located.is_reused,
        document: revision.document,
    })
}

fn stable_block_id(scope: &TenantScope, request: &ContentMediaInsertRequest) -> Uuid {
    // Serialization of these fixed-order Rust fields is canonical for the
    // complete, strictly typed request; never hash a truncated model message.
    let request = serde_json::to_vec(&(
        "geo.content.media.insert.v1",
        scope.operator_id,
        scope.tenant_id,
        scope.project_id,
        request,
    ))
    .expect("typed media insert request is serializable");
    let digest = Sha256::digest(request);
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("sha256 length");
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn inserted_document(
    base: &ContentRevision,
    request: &ContentMediaInsertRequest,
    binding: &ContentMediaBinding,
    block_id: Uuid,
) -> Result<geo_domain::StructuredDocument, AppError> {
    let mut document = base.document.clone();
    let position = match request.after_block_id {
        Some(after) => document
            .blocks
            .iter()
            .position(|block| block.block_id == after)
            .map(|index| index + 1)
            .ok_or_else(|| AppError::invalid_request("insertion block not found"))?,
        None => document.blocks.len(),
    };
    document.blocks.insert(
        position,
        ContentBlock {
            block_id,
            kind: ContentBlockKind::Rich,
            text: String::new(),
            citation_ids: Vec::new(),
            items: Vec::new(),
            rich: Some(RichContent {
                version: 1,
                node: RichNode::Media {
                    attrs: MediaReference {
                        object_id: binding.image.key.object_id,
                        object_version: binding.image.key.object_version,
                        sha256: binding.image.key.sha256.clone(),
                        alt: request.alt.clone(),
                        caption: request.caption.clone(),
                    },
                },
            }),
        },
    );
    document.schema_version = Some(2);
    document.validate(&base.evidence)?;
    Ok(document)
}

fn receipt(
    request: &ContentMediaInsertRequest,
    revision: ContentRevision,
    block_id: Uuid,
) -> ContentMediaInsertReceipt {
    ContentMediaInsertReceipt {
        execution_id: request.execution_id,
        item_id: request.item_id,
        asset_id: revision.asset_id,
        revision_id: revision.revision_id,
        base_revision_id: request.base_revision_id,
        block_id,
        revision: revision.revision,
    }
}

fn is_exact_child(
    revision: &ContentRevision,
    asset_id: Uuid,
    base_revision_id: Uuid,
    document: &geo_domain::StructuredDocument,
) -> bool {
    revision.asset_id == asset_id
        && (revision.base_revision_id == Some(base_revision_id)
            || revision.derived_from_revision_id == Some(base_revision_id))
        && revision.document == *document
}

async fn replay(
    state: &AppState,
    scope: &TenantScope,
    located: &LocatedItem,
    request: &ContentMediaInsertRequest,
    document: &geo_domain::StructuredDocument,
) -> Result<Option<ContentRevision>, AppError> {
    // After a fork the original asset is different. Search only the owned
    // destination and only a *direct* child of this exact requested base.
    let asset_id = if located.is_reused {
        return Ok(None);
    } else {
        located.asset_id
    };
    let revision = state
        .content_service()
        .repository()
        .find_exact_child_revision(scope, asset_id, request.base_revision_id, document)
        .await?;
    Ok(revision
        .filter(|revision| is_exact_child(revision, asset_id, request.base_revision_id, document)))
}

pub(crate) async fn insert(
    state: &AppState,
    scope: &TenantScope,
    request: ContentMediaInsertRequest,
) -> Result<ContentMediaInsertReceipt, AppError> {
    request
        .validate()
        .map_err(|reason| AppError::invalid_request(&reason))?;
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let scope = crate::content::scoped(state, scope, project_id).await?;
    let mut located = locate(state, &scope, request.execution_id, request.item_id).await?;
    let repository = state.content_service().repository();
    // A previous fork has changed the item's asset. Its origin revision can
    // only be used to verify an exact semantic replay, never for a fresh edit.
    let prior_origin = if located.is_reused {
        Some(located.asset_id)
    } else {
        located
            .item
            .reuse_history
            .iter()
            .find(|binding| binding.revision_id == request.base_revision_id)
            .map(|binding| binding.asset_id)
    };
    let base_asset = prior_origin.unwrap_or(located.asset_id);
    let base = repository
        .get_revision(&scope, base_asset, request.base_revision_id)
        .await?
        .filter(|revision| {
            revision.asset_id == base_asset && revision.revision_id == request.base_revision_id
        })
        .ok_or_else(|| AppError::not_found("content base revision not found"))?;
    let binding = state
        .content_media_repository()
        .get_binding(&scope, request.binding_id)
        .await?
        .ok_or_else(|| AppError::not_found("image binding not found"))?;
    let block_id = stable_block_id(&scope, &request);
    let document = inserted_document(&base, &request, &binding, block_id)?;
    if let Some(existing) = replay(state, &scope, &located, &request, &document).await? {
        return Ok(receipt(&request, existing, block_id));
    }
    if binding.state != ContentMediaBindingState::Active {
        return Err(AppError::conflict("image binding has been withdrawn"));
    }
    if located.current_revision_id != request.base_revision_id {
        return Err(AppError::conflict("base revision changed"));
    }
    let service = state.content_service();
    let result = if located.is_reused {
        service
            .fork_reused_item(
                &scope,
                request.execution_id,
                request.item_id,
                request.base_revision_id,
                document.clone(),
            )
            .await
    } else {
        service
            .edit(
                &scope,
                located.asset_id,
                request.base_revision_id,
                document.clone(),
            )
            .await
    };
    match result {
        Ok(revision) => Ok(receipt(&request, revision, block_id)),
        Err(error) if error.code == ErrorCode::Conflict => {
            located = locate(state, &scope, request.execution_id, request.item_id).await?;
            if let Some(existing) = replay(state, &scope, &located, &request, &document).await? {
                return Ok(receipt(&request, existing, block_id));
            }
            Err(error)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use geo_domain::{
        ContentMediaBinding, ContentMediaBindingState, DEVELOPMENT_OPERATOR_ID,
        DEVELOPMENT_TENANT_ID, MediaObjectKey, ProjectId, StructuredDocument, VerifiedImage,
    };

    fn fixture() -> (
        TenantScope,
        ContentMediaInsertRequest,
        ContentRevision,
        ContentMediaBinding,
    ) {
        let scope = TenantScope::new(
            DEVELOPMENT_OPERATOR_ID,
            DEVELOPMENT_TENANT_ID,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let asset_id = Uuid::new_v4();
        let base_id = Uuid::new_v4();
        let binding_id = Uuid::new_v4();
        let base = ContentRevision {
            revision_id: base_id,
            asset_id,
            revision: 1,
            base_revision_id: None,
            derived_from_revision_id: None,
            document: StructuredDocument {
                title: "A sourced draft".into(),
                schema_version: None,
                blocks: vec![ContentBlock {
                    block_id: Uuid::new_v4(),
                    kind: ContentBlockKind::Paragraph,
                    text: "Preserve this paragraph and its identity".into(),
                    citation_ids: Vec::new(),
                    items: Vec::new(),
                    rich: None,
                }],
            },
            markdown: "Original immutable markdown".into(),
            evidence: Vec::new(),
            quotes: Vec::new(),
            findings: Vec::new(),
            created_at: Utc::now(),
        };
        let binding = ContentMediaBinding {
            binding_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: scope.project_id.unwrap(),
            image: VerifiedImage {
                key: MediaObjectKey {
                    object_id: Uuid::new_v4(),
                    object_version: 1,
                    sha256: "a".repeat(64),
                },
                media_type: "image/png".into(),
                byte_len: 67,
                width: 1,
                height: 1,
            },
            state: ContentMediaBindingState::Active,
            created_at: Utc::now(),
            withdrawn_at: None,
        };
        let request = ContentMediaInsertRequest {
            execution_id: Uuid::new_v4(),
            item_id: Uuid::new_v4(),
            base_revision_id: base_id,
            binding_id,
            after_block_id: Some(base.document.blocks[0].block_id),
            alt: "Diagram".into(),
            caption: "Illustration".into(),
        };
        (scope, request, base, binding)
    }

    #[test]
    fn media_insertion_retains_original_block_and_adds_uncited_rich_node() {
        let (scope, request, base, binding) = fixture();
        let block_id = stable_block_id(&scope, &request);
        let document = inserted_document(&base, &request, &binding, block_id).unwrap();
        assert_eq!(document.schema_version, Some(2));
        assert_eq!(document.blocks.len(), 2);
        assert_eq!(document.blocks[0], base.document.blocks[0]);
        assert_eq!(document.blocks[1].block_id, block_id);
        assert!(document.blocks[1].citation_ids.is_empty());
        assert!(matches!(
            document.blocks[1].rich,
            Some(RichContent {
                node: RichNode::Media { .. },
                ..
            })
        ));
        assert_eq!(stable_block_id(&scope, &request), block_id);
        let mut moved = request.clone();
        moved.after_block_id = None;
        assert_ne!(stable_block_id(&scope, &moved), block_id);
        let mut changed = request.clone();
        changed.caption.push('!');
        assert_ne!(stable_block_id(&scope, &changed), block_id);
        let mut missing = request;
        missing.after_block_id = Some(Uuid::new_v4());
        assert_eq!(
            inserted_document(&base, &missing, &binding, block_id)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn exact_replay_requires_entire_document_and_direct_parent() {
        let (scope, request, base, binding) = fixture();
        let document =
            inserted_document(&base, &request, &binding, stable_block_id(&scope, &request))
                .unwrap();
        let mut child = base.clone();
        child.revision_id = Uuid::new_v4();
        child.revision = 2;
        child.base_revision_id = Some(base.revision_id);
        child.document = document.clone();
        assert!(is_exact_child(
            &child,
            base.asset_id,
            base.revision_id,
            &document
        ));
        child.document.title.push_str(" changed");
        assert!(!is_exact_child(
            &child,
            base.asset_id,
            base.revision_id,
            &document
        ));
        child.document = document.clone();
        child.base_revision_id = Some(Uuid::new_v4());
        assert!(!is_exact_child(
            &child,
            base.asset_id,
            base.revision_id,
            &document
        ));
        child.base_revision_id = None;
        child.derived_from_revision_id = Some(base.revision_id);
        assert!(is_exact_child(
            &child,
            base.asset_id,
            base.revision_id,
            &document
        ));
        assert!(!is_exact_child(
            &child,
            Uuid::new_v4(),
            base.revision_id,
            &document
        ));
    }

    #[test]
    fn inserting_into_legacy_document_over_rich_title_limit_fails_without_truncation() {
        let (scope, request, mut base, binding) = fixture();
        base.document.title = "A".repeat(4097);
        assert_eq!(base.document.schema_version, None);
        base.document.validate(&base.evidence).unwrap();
        let original = base.document.clone();
        assert_eq!(
            inserted_document(&base, &request, &binding, stable_block_id(&scope, &request))
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(base.document, original);
        assert_eq!(base.document.title.len(), 4097);
    }
}
