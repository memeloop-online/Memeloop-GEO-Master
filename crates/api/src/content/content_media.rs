//! Project-scoped binding of committed attachment bytes to verified static images.

use std::io::Cursor;

use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS},
    },
    response::Response,
};
use geo_domain::{
    AppError, ContentMediaBinding, ContentMediaBindingState, MAX_UPLOAD_BYTES, MediaObjectKey,
    ProjectId, TenantScope, VerifiedImage,
};
use image::{ImageDecoder, ImageFormat, Limits, codecs::webp::WebPDecoder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zune_jpeg::{
    JpegDecoder,
    zune_core::{bytestream::ZCursor, options::DecoderOptions},
};

use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};

// Keep both codec metadata and the decoded frame bounded. The original upload
// quota is separate: a small compressed file can decode into an enormous frame.
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_DECODED_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 100;
const THUMBNAIL_EDGE: u32 = 320;

/// Derivatives never replace committed attachments or version-export bytes.
/// Called on a blocking worker, after the original object's identity is checked.
fn thumbnail_png(bytes: Vec<u8>) -> Result<Vec<u8>, AppError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| decode_error())?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        return Err(decode_error());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| decode_error())?;
    let (width, height) = decoder.dimensions();
    check_dimensions(width, height)?;
    if decoder.total_bytes() > MAX_DECODED_BYTES {
        return Err(decode_error());
    }
    let orientation = decoder.orientation().map_err(|_| decode_error())?;
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|_| decode_error())?;
    image.apply_orientation(orientation);
    let image = if image.width() > THUMBNAIL_EDGE || image.height() > THUMBNAIL_EDGE {
        image.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE)
    } else {
        image
    };
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|_| AppError::new(geo_domain::ErrorCode::Internal, "thumbnail encoding failed"))?;
    Ok(output.into_inner())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingPageQuery {
    pub after: Option<Uuid>,
    pub limit: Option<usize>,
    // The shared auth middleware consumes the tenant selector.
    pub tenant_id: Option<String>,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Serialize)]
pub struct BindingPage {
    pub items: Vec<ContentMediaBinding>,
    pub next_cursor: Option<Uuid>,
}

fn decode_error() -> AppError {
    AppError::invalid_request("attachment is not a valid static PNG, JPEG, or WebP image")
}

fn check_dimensions(width: u32, height: u32) -> Result<(), AppError> {
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err(AppError::invalid_request(
            "image dimensions are out of range",
        ));
    }
    Ok(())
}

fn checked_frame(width: u32, height: u32, size: u64) -> Result<Vec<u8>, AppError> {
    check_dimensions(width, height)?;
    if size == 0 || size > MAX_DECODED_BYTES {
        return Err(AppError::invalid_request(
            "image decoded bytes are out of range",
        ));
    }
    let size = usize::try_from(size).map_err(|_| decode_error())?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(size)
        .map_err(|_| AppError::invalid_request("image exceeds available decode memory"))?;
    frame.resize(size, 0);
    Ok(frame)
}

fn check_and_decode<D: ImageDecoder>(decoder: D) -> Result<(u32, u32), AppError> {
    let (width, height) = decoder.dimensions();
    let mut frame = checked_frame(width, height, decoder.total_bytes())?;
    decoder.read_image(&mut frame).map_err(|_| decode_error())?;
    Ok((width, height))
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32), AppError> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: MAX_DECODED_BYTES as usize,
        },
    );
    let (width, height) = {
        let info = decoder.read_header_info().map_err(|_| decode_error())?;
        (info.width, info.height)
    };
    // Check dimensions before the PNG reader allocates its internal frame.
    check_dimensions(width, height)?;
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|_| decode_error())?;
    // `read_header_info` sees only IHDR; acTL is discovered while `read_info`
    // consumes metadata up to IDAT. Reject even a one-frame APNG.
    if reader.info().animation_control.is_some() {
        return Err(decode_error());
    }
    let size = reader.output_buffer_size().ok_or_else(decode_error)?;
    let mut frame = checked_frame(width, height, size as u64)?;
    reader.next_frame(&mut frame).map_err(|_| decode_error())?;
    // Pixel decode alone need not consume later PNG chunks or the IEND marker.
    reader.finish().map_err(|_| decode_error())?;
    Ok((width, height))
}

fn decode_jpeg(bytes: &[u8]) -> Result<(u32, u32), AppError> {
    // zune-jpeg strict mode catches more nonconforming streams than image's
    // JPEG adapter. Explicit EOI remains necessary: even strict MCU decoding
    // can successfully recover pixels from an absent terminal marker.
    if !bytes.ends_with(&[0xff, 0xd9]) {
        return Err(decode_error());
    }
    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .set_max_width(MAX_IMAGE_DIMENSION as usize)
        .set_max_height(MAX_IMAGE_DIMENSION as usize);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(|_| decode_error())?;
    let info = decoder.info().ok_or_else(decode_error)?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    let size = decoder.output_buffer_size().ok_or_else(decode_error)?;
    let mut frame = checked_frame(width, height, size as u64)?;
    decoder
        .decode_into(&mut frame)
        .map_err(|_| decode_error())?;
    Ok((width, height))
}

fn webp_container_complete(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && &bytes[..4] == b"RIFF"
        && &bytes[8..12] == b"WEBP"
        && u32::from_le_bytes(bytes[4..8].try_into().expect("checked length"))
            .checked_add(8)
            .is_some_and(|length| u64::from(length) == bytes.len() as u64)
}

/// This runs only on a blocking worker: decoding untrusted pixels can be CPU
/// intensive even when the compressed and decoded byte lengths are bounded.
fn verify_image_bytes(key: MediaObjectKey, bytes: Vec<u8>) -> Result<VerifiedImage, AppError> {
    key.validate()?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(AppError::invalid_request(
            "image byte length is out of range",
        ));
    }
    let byte_len = bytes.len() as u64;
    let format = image::guess_format(&bytes).map_err(|_| decode_error())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    let (media_type, (width, height)) = match format {
        ImageFormat::Png => ("image/png", decode_png(&bytes)?),
        ImageFormat::Jpeg => ("image/jpeg", decode_jpeg(&bytes)?),
        ImageFormat::WebP => {
            if !webp_container_complete(&bytes) {
                return Err(decode_error());
            }
            let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(|_| decode_error())?;
            if decoder.has_animation() {
                return Err(decode_error());
            }
            decoder.set_limits(limits).map_err(|_| decode_error())?;
            ("image/webp", check_and_decode(decoder)?)
        }
        _ => return Err(decode_error()),
    };
    let image = VerifiedImage {
        key,
        media_type: media_type.to_owned(),
        byte_len,
        width,
        height,
    };
    image.validate()?;
    Ok(image)
}

pub(crate) async fn create_binding(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(key): Json<MediaObjectKey>,
) -> Result<(StatusCode, Json<ContentMediaBinding>), ApiError> {
    require_project_writer(&auth).map_err(|e| api_error(e, context.request_id))?;
    key.validate()
        .map_err(|e| api_error(e, context.request_id))?;
    let scope = TenantScope::new(
        auth.scope.operator_id,
        auth.scope.tenant_id,
        Some(project_id),
    );
    let binding = bind_image(&state, &scope, key)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    Ok((StatusCode::CREATED, Json(binding)))
}

/// Bind committed attachment bytes for a project. Callers must separately
/// authorize the current operation (HTTP writer role or AI turn attachment).
pub(crate) async fn bind_image(
    state: &AppState,
    scope: &TenantScope,
    key: MediaObjectKey,
) -> Result<ContentMediaBinding, AppError> {
    key.validate()?;
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let scope = crate::content::scoped(state, scope, project_id).await?;
    let snapshot = state
        .knowledge_repository()
        .get_attachment_object_bytes(&scope, key.object_id, key.object_version, &key.sha256)
        .await?
        .ok_or_else(|| AppError::not_found("attachment not found"))?;
    // The storage seam verifies committed status, hash, version and byte
    // length. MIME in object metadata is uploader-declared and never trusted.
    let verified = tokio::task::spawn_blocking(move || verify_image_bytes(key, snapshot.bytes))
        .await
        .map_err(|_| {
            AppError::new(geo_domain::ErrorCode::Internal, "image decoder task failed")
        })??;
    state
        .content_media_repository()
        .create_binding(&scope, verified)
        .await
}

/// List current-project bindings using the same cursor and page limits as HTTP.
pub(crate) async fn binding_page(
    state: &AppState,
    scope: &TenantScope,
    after: Option<Uuid>,
    limit: Option<usize>,
) -> Result<BindingPage, AppError> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(AppError::invalid_request("limit must be between 1 and 100"));
    }
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::forbidden("project scope required"))?;
    let scope = crate::content::scoped(state, scope, project_id).await?;
    let mut items = state
        .content_media_repository()
        .list_bindings(&scope, after, limit + 1)
        .await?;
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].binding_id);
    items.truncate(limit);
    Ok(BindingPage { items, next_cursor })
}

pub(crate) async fn list_bindings(
    State(state): State<AppState>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<BindingPageQuery>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<BindingPage>, ApiError> {
    let _ = (query.tenant_id, query.project_id);
    let scope = TenantScope::new(scope.operator_id, scope.tenant_id, Some(project_id));
    binding_page(&state, &scope, query.after, query.limit)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

pub(crate) async fn get_binding_bytes(
    State(state): State<AppState>,
    Path((project_id, binding_id)): Path<(ProjectId, Uuid)>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    binding_response(state, project_id, binding_id, scope, context, false).await
}

pub(crate) async fn get_binding_thumbnail(
    State(state): State<AppState>,
    Path((project_id, binding_id)): Path<(ProjectId, Uuid)>,
    Extension(scope): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    binding_response(state, project_id, binding_id, scope, context, true).await
}

async fn binding_response(
    state: AppState,
    project_id: ProjectId,
    binding_id: Uuid,
    scope: TenantScope,
    context: RequestContext,
    thumbnail: bool,
) -> Result<Response, ApiError> {
    let scope = crate::content::scoped(&state, &scope, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    let media = state.content_media_repository();
    let binding = media
        .get_binding(&scope, binding_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?
        .filter(|binding| binding.state == ContentMediaBindingState::Active)
        .ok_or_else(|| {
            api_error(
                AppError::not_found("image binding not found"),
                context.request_id,
            )
        })?;
    let key = &binding.image.key;
    let snapshot = state
        .knowledge_repository()
        .get_attachment_object_bytes(&scope, key.object_id, key.object_version, &key.sha256)
        .await
        .map_err(|e| api_error(e, context.request_id))?
        .ok_or_else(|| {
            api_error(
                AppError::not_found("attachment not found"),
                context.request_id,
            )
        })?;
    if snapshot.bytes.len() as u64 != binding.image.byte_len {
        return Err(api_error(
            AppError::conflict("image bytes changed after binding"),
            context.request_id,
        ));
    }
    let bytes = if thumbnail {
        tokio::task::spawn_blocking(move || thumbnail_png(snapshot.bytes))
            .await
            .map_err(|_| {
                api_error(
                    AppError::new(geo_domain::ErrorCode::Internal, "thumbnail task failed"),
                    context.request_id,
                )
            })?
            .map_err(|e| api_error(e, context.request_id))?
    } else {
        snapshot.bytes
    };
    // Revalidate after storage I/O AND derivative work. Revocation before this
    // final authorization prevents either original or derived preview delivery.
    let current = media
        .get_binding(&scope, binding_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?
        .filter(|current| {
            current.state == ContentMediaBindingState::Active && current.image == binding.image
        })
        .ok_or_else(|| {
            api_error(
                AppError::not_found("image binding not found"),
                context.request_id,
            )
        })?;
    let media_type = if thumbnail {
        "image/png"
    } else {
        &current.image.media_type
    };
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(media_type).map_err(|_| {
            api_error(
                AppError::conflict("invalid bound image type"),
                context.request_id,
            )
        })?,
    );
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    Ok(response)
}

pub(crate) async fn withdraw_binding(
    State(state): State<AppState>,
    Path((project_id, binding_id)): Path<(ProjectId, Uuid)>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ContentMediaBinding>, ApiError> {
    require_project_writer(&auth).map_err(|e| api_error(e, context.request_id))?;
    let scope = crate::content::scoped(&state, &auth.scope, project_id)
        .await
        .map_err(|e| api_error(e, context.request_id))?;
    state
        .content_media_repository()
        .withdraw_binding(&scope, binding_id)
        .await
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_domain::{DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, ErrorCode};

    #[test]
    fn thumbnails_fit_without_upscaling_and_preserve_transparency() {
        for (width, height, expected) in [
            (800, 400, (320, 160)),
            (400, 800, (160, 320)),
            (12, 7, (12, 7)),
        ] {
            let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                width,
                height,
                image::Rgba([80, 120, 200, 96]),
            ));
            let mut source = Cursor::new(Vec::new());
            image.write_to(&mut source, ImageFormat::Png).unwrap();
            let result = thumbnail_png(source.into_inner()).unwrap();
            assert_eq!(image::guess_format(&result).unwrap(), ImageFormat::Png);
            let decoded = image::load_from_memory(&result).unwrap().into_rgba8();
            assert_eq!(decoded.dimensions(), expected);
            assert_eq!(decoded.get_pixel(0, 0).0[3], 96);
        }
        assert!(thumbnail_png(b"<svg/>".to_vec()).is_err());
        assert!(thumbnail_png(b"\x89PNG\r\n\x1a\n".to_vec()).is_err());
    }

    #[test]
    fn rejects_markup_and_missing_image_data() {
        let key = MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 1,
            sha256: "a".repeat(64),
        };
        assert!(verify_image_bytes(key.clone(), b"<svg/>".to_vec()).is_err());
        assert!(verify_image_bytes(key, b"\x89PNG\r\n\x1a\n".to_vec()).is_err());
    }

    #[tokio::test]
    async fn shared_services_require_project_scope_and_bounded_pages() {
        let state = AppState::development();
        let tenant = TenantScope::new(DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID, None);
        let key = MediaObjectKey {
            object_id: Uuid::new_v4(),
            object_version: 1,
            sha256: "a".repeat(64),
        };
        assert_eq!(
            bind_image(&state, &tenant, key).await.unwrap_err().code,
            ErrorCode::Forbidden
        );
        assert_eq!(
            binding_page(&state, &tenant, None, None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
        for limit in [Some(0), Some(101)] {
            assert_eq!(
                binding_page(&state, &tenant, None, limit)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidRequest
            );
        }
    }
}
