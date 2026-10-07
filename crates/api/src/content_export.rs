//! Revision-specific, scoped content archive. Authorization and byte snapshots
//! finish before any ZIP work; packaging never holds repository locks.

use std::{
    collections::HashMap,
    io::{Cursor, Write},
};

use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{
        Response,
        header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, HeaderValue},
    },
};
use geo_domain::{
    AppError, AuthorizedMediaSnapshot, ContentRevision, ErrorCode, MediaObjectKey, ProjectId,
    TenantScope, add_media_snapshot_bytes, ordered_media_snapshot_keys,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{ApiError, AppState, RequestContext, api_error};

const MAX_DOCUMENT_EXPORT_BYTES: usize = 8 * 1024 * 1024;
const X_CONTENT_TYPE_OPTIONS: &str = "x-content-type-options";

#[derive(Deserialize)]
pub(crate) struct ExportBundleQuery {
    format: String,
}

pub(crate) async fn export_bundle(
    State(state): State<AppState>,
    Path((project_id, asset_id, revision_id)): Path<(ProjectId, Uuid, Uuid)>,
    Query(query): Query<ExportBundleQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Response<Body>, ApiError> {
    let format = query.format;
    if !matches!(format.as_str(), "markdown" | "html") {
        return Err(api_error(
            AppError::invalid_request("export format must be markdown or html"),
            context.request_id,
        ));
    }
    let scope = crate::content::scoped(&state, &tenant, project_id)
        .await
        .map_err(|error| api_error(error, context.request_id))?;
    let repository = state.content_service().repository();
    repository
        .get_asset(&scope, asset_id)
        .await
        .and_then(|asset| asset.ok_or_else(|| AppError::not_found("content asset not found")))
        .map_err(|error| api_error(error, context.request_id))?;
    let revision = repository
        .list_revisions(&scope, asset_id)
        .await
        .and_then(|revisions| {
            revisions
                .into_iter()
                .find(|revision| {
                    revision.revision_id == revision_id && revision.asset_id == asset_id
                })
                .ok_or_else(|| AppError::not_found("content revision not found"))
        })
        .map_err(|error| api_error(error, context.request_id))?;

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
    )
    .map_err(|error| api_error(error, context.request_id))?;
    // The repository must authorize the whole set under one guard/transaction.
    // No archive or partial HTTP result exists if even one binding is withdrawn.
    let snapshots = if keys.is_empty() {
        Vec::new()
    } else {
        state
            .content_media_repository()
            .snapshot_authorized_images(&scope, &keys)
            .await
            .map_err(|error| api_error(error, context.request_id))?
    };
    let archive =
        tokio::task::spawn_blocking(move || bundle_bytes(revision, &format, keys, snapshots))
            .await
            .map_err(|_| {
                api_error(
                    AppError::new(ErrorCode::Internal, "content export could not be prepared"),
                    context.request_id,
                )
            })?
            .map_err(|error| api_error(error, context.request_id))?;

    let mut response = Response::new(Body::from(archive));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{revision_id}.zip\""))
            .expect("UUID filename has only safe ASCII"),
    );
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn bundle_bytes(
    revision: ContentRevision,
    format: &str,
    keys: Vec<MediaObjectKey>,
    snapshots: Vec<AuthorizedMediaSnapshot>,
) -> Result<Vec<u8>, AppError> {
    if snapshots.len() != keys.len() {
        return Err(AppError::conflict("authorized image set is incomplete"));
    }
    let mut paths = HashMap::with_capacity(keys.len());
    let mut entries = Vec::with_capacity(keys.len());
    let mut total = 0u64;
    for (expected, snapshot) in keys.into_iter().zip(snapshots) {
        snapshot.image.validate()?;
        let image = &snapshot.image;
        if image.key != expected || image.byte_len != snapshot.bytes.len() as u64 {
            return Err(AppError::conflict("authorized image snapshot changed"));
        }
        let actual = hex::encode(Sha256::digest(&snapshot.bytes));
        if actual != expected.sha256 {
            return Err(AppError::conflict("authorized image bytes are corrupt"));
        }
        total = add_media_snapshot_bytes(total, image.byte_len)?;
        let extension = match image.media_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => return Err(AppError::conflict("unsupported authorized image type")),
        };
        let path = format!(
            "media/{}-{}.{}",
            expected.object_id, expected.object_version, extension
        );
        paths.insert(expected, path.clone());
        entries.push((path, snapshot.bytes));
    }
    let (filename, document) = match format {
        "markdown" if entries.is_empty() => {
            (format!("{}.md", revision.revision_id), revision.markdown)
        }
        "markdown" => (
            format!("{}.md", revision.revision_id),
            revision
                .document
                .markdown_with_media_paths(&revision.evidence, &paths)?,
        ),
        "html" => (
            format!("{}.html", revision.revision_id),
            revision
                .document
                .html_with_media_paths(&revision.evidence, &paths)?,
        ),
        _ => {
            return Err(AppError::invalid_request(
                "export format must be markdown or html",
            ));
        }
    };
    if document.len() > MAX_DOCUMENT_EXPORT_BYTES {
        return Err(AppError::invalid_request("content export is too large"));
    }
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer
        .start_file(filename, options)
        .map_err(|_| AppError::new(ErrorCode::Internal, "content export could not be prepared"))?;
    writer
        .write_all(document.as_bytes())
        .map_err(|_| AppError::new(ErrorCode::Internal, "content export could not be prepared"))?;
    for (path, bytes) in entries {
        writer.start_file(path, options).map_err(|_| {
            AppError::new(ErrorCode::Internal, "content export could not be prepared")
        })?;
        writer.write_all(&bytes).map_err(|_| {
            AppError::new(ErrorCode::Internal, "content export could not be prepared")
        })?;
    }
    let cursor = writer
        .finish()
        .map_err(|_| AppError::new(ErrorCode::Internal, "content export could not be prepared"))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use geo_domain::{ContentBlock, ContentBlockKind, StructuredDocument, VerifiedImage};

    fn text_revision() -> ContentRevision {
        let document = StructuredDocument {
            title: "Synthetic".into(),
            schema_version: None,
            blocks: vec![ContentBlock {
                block_id: Uuid::new_v4(),
                kind: ContentBlockKind::Paragraph,
                text: "Exact text".into(),
                citation_ids: vec![],
                items: vec![],
                rich: None,
            }],
        };
        ContentRevision {
            revision_id: Uuid::new_v4(),
            asset_id: Uuid::new_v4(),
            revision: 1,
            base_revision_id: None,
            derived_from_revision_id: None,
            document,
            markdown: "Exact saved text.\r\n".into(),
            evidence: vec![],
            quotes: vec![],
            findings: vec![],
            created_at: Utc::now(),
        }
    }

    #[test]
    fn text_only_archive_preserves_saved_markdown_bytes() {
        use std::io::Read;

        let revision = text_revision();
        let archive = bundle_bytes(revision.clone(), "markdown", vec![], vec![]).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
        assert_eq!(zip.len(), 1);
        let mut saved = String::new();
        zip.by_name(&format!("{}.md", revision.revision_id))
            .unwrap()
            .read_to_string(&mut saved)
            .unwrap();
        assert_eq!(saved, revision.markdown);
    }

    #[test]
    fn incomplete_or_corrupt_snapshot_never_produces_archive() {
        let revision = text_revision();
        let key = MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 2,
            sha256: hex::encode(Sha256::digest(b"the original bytes")),
        };
        assert!(bundle_bytes(revision.clone(), "html", vec![key.clone()], vec![]).is_err());
        let snapshot = AuthorizedMediaSnapshot {
            image: VerifiedImage {
                key: key.clone(),
                media_type: "image/png".into(),
                byte_len: 7,
                width: 1,
                height: 1,
            },
            bytes: b"changed".to_vec(),
        };
        assert!(bundle_bytes(revision, "html", vec![key], vec![snapshot]).is_err());
    }

    #[test]
    fn same_object_version_cannot_map_to_conflicting_files() {
        let key = MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 1,
            sha256: "a".repeat(64),
        };
        let mut conflicting = key.clone();
        conflicting.sha256 = "b".repeat(64);
        assert!(ordered_media_snapshot_keys(&[key, conflicting]).is_err());
    }
}
