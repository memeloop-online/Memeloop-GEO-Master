//! Versioned, tenant-scoped knowledge-import contracts.
//!
//! This module deliberately models the usable first vertical slice only:
//! verified upload bytes and text / Markdown / UTF-8 CSV can be turned into
//! deterministic paragraph/table chunks and an immutable release. It does not
//! pretend that crawling, office/PDF parsing, OCR, embeddings, or an LLM are
//! available.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::sync::RwLock;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppError, ErrorCode, Operation, OperationStatus, OperatorId, ProjectId, TenantId, TenantScope,
};
use crate::{DocumentScope, QuestionClusterState};
use crate::{
    OFFICE_MAX_DOCUMENT_TEXT_BYTES, OfficeDocumentManifest, OfficeFormat, OfficeParseCursor,
    OfficeParseInput, OfficeParseJobRef, OfficeParseLease, OfficeUnitResult,
    office_document_error_code, office_unit_chunks,
};
use crate::{
    PDF_MAX_DOCUMENT_TEXT_BYTES, PdfDocumentManifest, PdfPageResult, PdfPageText, PdfParseCursor,
    PdfParseInput, PdfParseJobRef, PdfParseLease, pdf_page_chunks,
};

pub const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
/// The JSON idempotency middleware buffers at most 1 MiB including syntax
/// and neighboring fields.  Keep inline text comfortably below that shared
/// boundary; larger material must use verified raw-byte upload.
pub const MAX_INLINE_TEXT_BYTES: usize = 256 * 1024;
pub const UPLOAD_SESSION_TTL_SECONDS: i64 = 60 * 60;
pub const CONTENT_EVIDENCE_MAX_QUOTE_CHARS: usize = 1600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgePurpose {
    Public,
    Internal,
}

#[cfg(test)]
mod text_revision_tests {
    use super::*;

    #[tokio::test]
    async fn authored_revisions_preserve_original_and_replay_immutable_receipts() {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let repository = MemoryKnowledgeRepository::default();
        let original = repository
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "original".to_owned(),
                    kind: SourceKind::Text,
                    name: "Synthetic source".to_owned(),
                    purpose: KnowledgePurpose::Public,
                    text: Some("Original evidence".to_owned()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap()
            .items
            .remove(0);
        let source = original.source.unwrap();
        let base = original.source_version.unwrap();
        let first = ReviseSourceTextCommand {
            base_version_id: base.source_version_id,
            media_type: "text/markdown".to_owned(),
            text: "  中文标题\n\n- original evidence\n".to_owned(),
        };
        let receipt = repository
            .revise_source_text(
                &scope,
                source.source_id,
                source.revision,
                "first",
                first.clone(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.source_version.version, base.version + 1);
        assert_eq!(
            receipt.source_version.parent_version_id,
            Some(base.source_version_id)
        );
        assert_eq!(receipt.source_version.object_id, None);
        {
            let mut state = repository.state.write().await;
            let mut unpublished = receipt.source_version.clone();
            unpublished.source_version_id = Uuid::new_v4();
            unpublished.version += 1;
            unpublished.representation = SourceVersionRepresentation::Original;
            state
                .versions
                .insert(unpublished.source_version_id, unpublished);
        }
        let next = repository
            .revise_source_text(
                &scope,
                source.source_id,
                receipt.source.revision,
                "second",
                ReviseSourceTextCommand {
                    base_version_id: receipt.source_version.source_version_id,
                    media_type: "text/plain".to_owned(),
                    text: "Another revision".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            next.source_version.version,
            receipt.source_version.version + 2
        );
        assert_eq!(
            repository
                .revise_source_text(
                    &scope,
                    source.source_id,
                    source.revision,
                    "first",
                    first.clone(),
                )
                .await
                .unwrap(),
            receipt
        );
        assert_ne!(
            receipt.knowledge_release.knowledge_release_id,
            next.knowledge_release.knowledge_release_id
        );
        assert_eq!(
            repository
                .get_source_version_content(
                    &scope,
                    source.source_id,
                    receipt.source_version.source_version_id
                )
                .await
                .unwrap()
                .unwrap()
                .text,
            first.text
        );
        assert_eq!(
            repository
                .get_source_version_content(&scope, source.source_id, base.source_version_id)
                .await
                .unwrap()
                .unwrap()
                .text_basis,
            SourceTextBasis::Extracted
        );
        assert!(
            repository
                .revise_source_text(
                    &scope,
                    source.source_id,
                    source.revision,
                    "first",
                    ReviseSourceTextCommand {
                        text: "different".to_owned(),
                        ..first.clone()
                    },
                )
                .await
                .unwrap_err()
                .details
                .unwrap()
                .to_string()
                .contains("idempotency_conflict")
        );
        assert!(
            repository
                .revise_source_text(&scope, source.source_id, source.revision, "stale", first)
                .await
                .unwrap_err()
                .details
                .unwrap()
                .to_string()
                .contains("source_revision_conflict")
        );
        assert!(
            repository
                .get_source_version_content(&scope, Uuid::new_v4(), base.source_version_id)
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UploadSessionState {
    Created,
    Uploading,
    Uploaded,
    Committed,
    Failed,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StoredObjectState {
    Staged,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    File,
    Url,
    Text,
    Object,
    KnowledgeCollection,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Active,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStage {
    Acquire,
    Parse,
    Extract,
    Index,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    Queued,
    Running,
    Partial,
    Succeeded,
    Failed,
    Cancelled,
}

/// Bounded, non-sensitive import state for polling and receipt reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeImportProgress {
    pub import_job_id: Option<Uuid>,
    pub status: ImportStatus,
    pub stage: Option<ImportStage>,
    pub source_id: Option<Uuid>,
    pub source_version_id: Option<Uuid>,
    pub knowledge_release_id: Option<Uuid>,
    pub completed_units: i32,
    pub failed_units: i32,
    /// Total errors, including entries omitted from the bounded `errors` list.
    pub error_count: u32,
    pub errors: Vec<KnowledgeImportProgressError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeImportProgressError {
    pub code: String,
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<OfficeFormat>,
}

/// Never reflect arbitrary parser or operation messages into progress reads.
pub fn knowledge_import_progress_error(value: &Value) -> KnowledgeImportProgressError {
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| {
            matches!(
                *code,
                "capability_missing"
                    | "invalid_request"
                    | "not_found"
                    | "conflict"
                    | "dependency_unavailable"
                    | "invalid_pdf"
                    | "encrypted_pdf"
                    | "parse_failed"
                    | "page_limit"
                    | "ocr_required"
                    | "empty_text"
                    | "invalid_docx"
                    | "invalid_xlsx"
                    | "encrypted_office"
                    | "unit_limit"
                    | "unsupported_content"
            )
        })
        .unwrap_or("import_failed");
    KnowledgeImportProgressError {
        code: code.to_owned(),
        page: value
            .get("page")
            .and_then(Value::as_u64)
            .and_then(|page| u32::try_from(page).ok())
            .filter(|page| *page > 0),
        unit_id: value
            .get("unit_id")
            .and_then(Value::as_u64)
            .and_then(|unit_id| u32::try_from(unit_id).ok()),
        format: value
            .get("format")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
    }
}

pub fn knowledge_import_progress_errors(
    values: &[Value],
) -> (u32, Vec<KnowledgeImportProgressError>) {
    (
        u32::try_from(values.len()).unwrap_or(u32::MAX),
        values
            .iter()
            .take(100)
            .map(knowledge_import_progress_error)
            .collect(),
    )
}

pub fn knowledge_import_progress_app_error(error: &AppError) -> KnowledgeImportProgressError {
    let code = match error.code {
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::Forbidden => "forbidden",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Conflict => "conflict",
        ErrorCode::NotReady => "not_ready",
        ErrorCode::CapabilityMissing => "capability_missing",
        ErrorCode::DependencyUnavailable => "dependency_unavailable",
        ErrorCode::Internal => "import_failed",
    };
    KnowledgeImportProgressError {
        code: code.to_owned(),
        page: None,
        unit_id: None,
        format: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    Paragraph,
    Table,
    ImageDescription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProductState {
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Candidate,
    Confirmed,
    Conflicted,
    Superseded,
}

/// The subset of adapters which is actually wired in this release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeCapability {
    pub upload_sessions: bool,
    pub memory_blob_adapter: bool,
    pub durable_blob_storage: bool,
    pub deterministic_text_parser: bool,
    pub url_fetch: bool,
    pub pdf_parser: bool,
    pub docx_parser: bool,
    pub xlsx_parser: bool,
    pub ocr: bool,
    pub vector_search: bool,
    pub llm_answering: bool,
    pub evidence_only_answering: bool,
    pub max_inline_text_bytes: usize,
    pub max_upload_bytes: u64,
    pub max_batch_files: u32,
    /// Formats which the deterministic parser can actually turn into chunks.
    pub supported_media_types: Vec<String>,
    /// Other accepted upload bytes are preserved/verified but fail completion
    /// until a parser adapter is configured.
    pub accepted_unparsed_media_types: Vec<String>,
    pub limitations: Vec<String>,
}

impl KnowledgeCapability {
    pub fn memory() -> Self {
        Self {
            upload_sessions: true,
            memory_blob_adapter: true,
            durable_blob_storage: false,
            deterministic_text_parser: true,
            url_fetch: false,
            pdf_parser: false,
            docx_parser: false,
            xlsx_parser: false,
            ocr: false,
            vector_search: false,
            llm_answering: false,
            evidence_only_answering: true,
            max_inline_text_bytes: MAX_INLINE_TEXT_BYTES,
            max_upload_bytes: MAX_UPLOAD_BYTES,
            max_batch_files: 100,
            supported_media_types: vec![
                "text/plain".to_owned(),
                "text/markdown".to_owned(),
                "text/csv".to_owned(),
            ],
            accepted_unparsed_media_types: vec![
                "application/pdf".to_owned(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_owned(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_owned(),
            ],
            limitations: vec![
                "uploaded bytes are kept only in process memory".to_owned(),
                "the first upload implementation buffers a whole request; production should use streaming direct object storage".to_owned(),
                "only text/plain, text/markdown and UTF-8 comma-separated text/csv with a header are parsed".to_owned(),
                "CSV limits: 256 columns, 10000 data records, 256 KiB per encoded cell, 32 MiB total chunk text".to_owned(),
                "a text declaration is parsed only after UTF-8 validation; the declared media type is not content sniffing".to_owned(),
                "URL acquisition, office/PDF parsing, OCR, vector search, and LLM answers require adapters".to_owned(),
            ],
        }
    }

    pub fn durable_text_only() -> Self {
        let mut value = Self::memory();
        value.memory_blob_adapter = false;
        value.durable_blob_storage = true;
        value
            .limitations
            .retain(|message| !message.contains("only in process memory"));
        value
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadSession {
    pub upload_session_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub revision: i64,
    pub filename: String,
    pub declared_media_type: String,
    pub expected_size: u64,
    pub expected_sha256: String,
    pub purpose: KnowledgePurpose,
    pub state: UploadSessionState,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging_object_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_object_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<Uuid>,
}

impl UploadSession {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, Some(self.project_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StoredObject {
    pub object_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub object_version: i64,
    pub backend: String,
    pub opaque_key: String,
    pub actual_size: u64,
    pub detected_media_type: String,
    pub sha256: String,
    pub state: StoredObjectState,
    pub created_at: DateTime<Utc>,
}

/// An internal snapshot of one committed agent attachment's original bytes.
/// `detected_media_type` in the object is currently uploader-declared; callers
/// must inspect and decode the bytes before treating them as media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentObjectBytes {
    pub object: StoredObject,
    pub bytes: Vec<u8>,
}

impl AttachmentObjectBytes {
    pub fn verified(object: StoredObject, bytes: Vec<u8>) -> Result<Self, AppError> {
        if object.state != StoredObjectState::Committed
            || object.actual_size == 0
            || object.actual_size > MAX_UPLOAD_BYTES
            || bytes.len() as u64 != object.actual_size
            || sha256_hex(&bytes) != object.sha256
        {
            return Err(AppError::conflict(
                "committed attachment bytes do not match object metadata",
            ));
        }
        Ok(Self { object, bytes })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Source {
    pub source_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub revision: i64,
    pub kind: SourceKind,
    pub name: String,
    pub purpose: KnowledgePurpose,
    pub state: SourceState,
    pub locator: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version_id: Option<Uuid>,
    pub sync_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_sync_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SourceVersion {
    pub source_version_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub source_id: Uuid,
    pub version: i64,
    #[serde(default)]
    pub representation: SourceVersionRepresentation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_version: Option<i64>,
    pub content_sha256: String,
    pub captured_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_version_id: Option<Uuid>,
    pub parser_version: String,
    pub extraction_version: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceVersionRepresentation {
    #[default]
    Original,
    AuthoredText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceTextBasis {
    Exact,
    Extracted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SourceVersionContent {
    pub source_version_id: Uuid,
    pub representation: SourceVersionRepresentation,
    pub media_type: String,
    pub text: String,
    pub text_basis: SourceTextBasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviseSourceTextCommand {
    pub base_version_id: Uuid,
    pub media_type: String,
    pub text: String,
}

impl ReviseSourceTextCommand {
    pub fn validate(&self) -> Result<(), AppError> {
        if !matches!(self.media_type.as_str(), "text/plain" | "text/markdown") {
            return Err(AppError::invalid_request(
                "media_type must be text/plain or text/markdown",
            ));
        }
        if self.text.trim().is_empty() || self.text.len() > MAX_INLINE_TEXT_BYTES {
            return Err(AppError::invalid_request(
                "text must be nonblank and no more than 256 KiB",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SourceTextRevisionReceipt {
    pub source: Source,
    pub source_version: SourceVersion,
    pub knowledge_release: KnowledgeRelease,
}

/// P04 source-detail read model.  It contains only scoped, traceable rows;
/// source versions remain immutable and chunk locators are typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SourceDetail {
    pub source: Source,
    pub versions: Vec<SourceVersion>,
    pub chunks: Vec<Chunk>,
    pub facts: Vec<Fact>,
    pub import_jobs: Vec<ImportJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportJob {
    pub import_job_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub operation_id: Uuid,
    pub source_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version_id: Option<Uuid>,
    pub stage: ImportStage,
    pub status: ImportStatus,
    pub attempt: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<DateTime<Utc>>,
    pub input_hash: String,
    pub stage_output_refs: Vec<String>,
    pub completed_units: i32,
    pub failed_units: i32,
    pub errors: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChunkLocator {
    Text {
        start_line: u32,
        end_line: u32,
        start_char: u32,
        end_char: u32,
    },
    Web {
        snapshot_object_id: Uuid,
        original_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_char: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_char: Option<u32>,
    },
    Pdf {
        page: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bbox: Option<Vec<i32>>,
        #[serde(default)]
        ocr: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_char: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_char: Option<u32>,
    },
    Docx {
        heading_path: Vec<String>,
        paragraph_index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_element_index: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table_index: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table_row: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table_column: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table_row_span: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table_col_span: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table_merged: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_char: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_char: Option<u32>,
    },
    Xlsx {
        sheet: String,
        range: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header_range: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cell_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        formula: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cached_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cached_value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merged_range: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_char: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_char: Option<u32>,
    },
    Csv {
        start_row: u32,
        end_row: u32,
        start_column: u32,
        end_column: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header_row: Option<u32>,
        /// Zero-based, half-open character range within a single CSV cell.
        /// Absent on full records and multi-column evidence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_char: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_char: Option<u32>,
    },
    Manual {},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Chunk {
    pub chunk_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub source_version_id: Uuid,
    pub ordinal: i32,
    pub kind: ChunkKind,
    pub text: String,
    pub text_hash: String,
    pub locator: ChunkLocator,
    pub product_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub extraction_method: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EvidenceRef {
    pub source_version_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<Uuid>,
    pub locator: ChunkLocator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Product {
    pub product_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub revision: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub aliases: Vec<String>,
    pub state: ProductState,
    pub evidence_refs: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Fact {
    pub fact_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub revision: i64,
    pub subject_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub attribute: String,
    pub typed_value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<DateTime<Utc>>,
    pub status: FactStatus,
    pub pinned: bool,
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_fact_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeCoverage {
    pub source_version_count: u64,
    pub chunk_count: u64,
    pub failed_source_count: u64,
    pub blocked_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeRelease {
    pub knowledge_release_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub sequence: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_release_id: Option<Uuid>,
    pub source_version_refs: Vec<Uuid>,
    pub fact_revision_refs: Vec<(Uuid, i64)>,
    pub index_build_id: String,
    pub pipeline_versions: Value,
    pub content_hash: String,
    pub coverage: KnowledgeCoverage,
    pub created_at: DateTime<Utc>,
}

/// The finite first-stage document fan-out.  This is a planning artifact, not
/// generated content.  A manifest is immutable after it is sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocumentManifest {
    pub manifest_id: Uuid,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub revision: i32,
    pub knowledge_release_id: Uuid,
    pub planner_version: String,
    pub state: DocumentManifestState,
    pub sealed: bool,
    pub expected_count: Option<i64>,
    pub scope_hash: String,
    pub items: Vec<DocumentManifestItem>,
    pub coverage: DocumentManifestCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentManifestState {
    AwaitingKnowledge,
    Planning,
    Ready,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentManifestItemState {
    Planned,
    Blocked,
    Deferred,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocumentManifestItem {
    pub document_manifest_item_id: Uuid,
    pub manifest_id: Uuid,
    pub knowledge_release_id: Uuid,
    pub document_key: String,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<Uuid>,
    pub market: String,
    pub language: String,
    pub state: DocumentManifestItemState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    pub dependency_hash: String,
    pub source_version_refs: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct DocumentManifestCoverage {
    pub total: u64,
    pub planned: u64,
    pub blocked: u64,
    pub deferred: u64,
    pub not_applicable: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentManifestPlanRequest {
    pub manifest_id: Uuid,
    pub knowledge_release_id: Uuid,
}

pub const DOCUMENT_PLANNER_VERSION: &str = "deterministic-document-v1";

/// Build a bounded, stable set of document branches from a frozen release.
/// The order and keys are deterministic, so callers can safely page or resume
/// fan-out without re-creating completed branches.
pub fn plan_document_manifest(
    scope: &TenantScope,
    release: &KnowledgeRelease,
    manifest_id: Uuid,
    document_scope: &DocumentScope,
    public_source_version_refs: &[Uuid],
) -> Result<DocumentManifest, AppError> {
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::invalid_request("project_id is required for document planning"))?;
    if release.operator_id != scope.operator_id
        || release.tenant_id != scope.tenant_id
        || release.project_id != project_id
    {
        return Err(AppError::not_found("knowledge release not found"));
    }
    let markets = normalized_dimension(&document_scope.markets, "default");
    let languages = normalized_dimension(&document_scope.languages, "default");
    let content_types = normalized_dimension(&document_scope.content_types, "company_profile");
    let question_clusters = {
        let values = document_scope
            .question_clusters
            .iter()
            .filter(|cluster| cluster.state == QuestionClusterState::Resolved)
            .map(|cluster| cluster.key.trim().to_owned())
            .filter(|key| !key.is_empty())
            .collect::<Vec<_>>();
        if values.is_empty() {
            vec!["general".to_owned()]
        } else {
            normalized_dimension(&values, "general")
        }
    };
    let expected_count = markets
        .len()
        .checked_mul(languages.len())
        .and_then(|count| count.checked_mul(content_types.len()))
        .and_then(|count| count.checked_mul(question_clusters.len()))
        .ok_or_else(|| AppError::invalid_request("document coverage exceeds maximum"))?;
    if expected_count > 10_000 {
        return Err(AppError::invalid_request(
            "document coverage exceeds 10,000 planned items",
        ));
    }
    let mut items = Vec::new();
    // Product extraction is intentionally not part of W03's deterministic
    // parser yet.  A product-less project branch keeps the denominator
    // explicit and can later be replaced by product-specific branches without
    // changing the document key contract.
    for market in markets {
        for language in &languages {
            for content_type in &content_types {
                for question_cluster in &question_clusters {
                    let dimensions = serde_json::to_vec(&(
                        market.as_str(),
                        language.as_str(),
                        content_type.as_str(),
                        question_cluster.as_str(),
                    ))
                    .map_err(|_| {
                        AppError::new(
                            ErrorCode::Internal,
                            "document dimensions could not be encoded",
                        )
                    })?;
                    let document_key = format!("project:{}", sha256_hex(&dimensions));
                    let dependency_hash = sha256_hex(
                        format!(
                            "{}\n{}\n{}",
                            document_key,
                            DOCUMENT_PLANNER_VERSION,
                            public_source_version_refs
                                .iter()
                                .map(Uuid::to_string)
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                        .as_bytes(),
                    );
                    let (state, block_reason) = if public_source_version_refs.is_empty() {
                        (
                            DocumentManifestItemState::Blocked,
                            Some("knowledge_release_has_no_public_sources".to_owned()),
                        )
                    } else {
                        (DocumentManifestItemState::Planned, None)
                    };
                    items.push(DocumentManifestItem {
                        document_manifest_item_id: deterministic_uuid(&format!(
                            "{}:{document_key}",
                            manifest_id
                        )),
                        manifest_id,
                        knowledge_release_id: release.knowledge_release_id,
                        document_key,
                        content_type: content_type.clone(),
                        product_id: None,
                        market: market.clone(),
                        language: language.clone(),
                        state,
                        block_reason,
                        dependency_hash,
                        source_version_refs: public_source_version_refs.to_vec(),
                    });
                }
            }
        }
    }
    items.sort_by(|left, right| left.document_key.cmp(&right.document_key));
    let scope_hash = sha256_hex(
        serde_json::to_vec(document_scope)
            .map_err(|_| AppError::new(ErrorCode::Internal, "document scope could not be encoded"))?
            .as_slice(),
    );
    let mut coverage = DocumentManifestCoverage {
        total: items.len() as u64,
        ..DocumentManifestCoverage::default()
    };
    for item in &items {
        match item.state {
            DocumentManifestItemState::Planned => coverage.planned += 1,
            DocumentManifestItemState::Blocked => coverage.blocked += 1,
            DocumentManifestItemState::Deferred => coverage.deferred += 1,
            DocumentManifestItemState::NotApplicable => coverage.not_applicable += 1,
        }
    }
    Ok(DocumentManifest {
        manifest_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id,
        revision: 1,
        knowledge_release_id: release.knowledge_release_id,
        planner_version: DOCUMENT_PLANNER_VERSION.to_owned(),
        state: DocumentManifestState::Ready,
        sealed: true,
        expected_count: Some(expected_count as i64),
        scope_hash,
        items,
        coverage,
    })
}

fn normalized_dimension(values: &[String], fallback: &str) -> Vec<String> {
    let mut values = values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    if values.is_empty() {
        vec![fallback.to_owned()]
    } else {
        values
    }
}

fn deterministic_uuid(value: &str) -> Uuid {
    let digest = Sha256::digest(value.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CurrentKnowledgeRelease {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_release_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSearchRequest {
    pub query: String,
    #[serde(default = "default_search_purpose")]
    pub purpose: KnowledgePurpose,
    #[serde(default = "default_search_limit")]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_release_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeSearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_release_id: Option<Uuid>,
    pub evidence: Vec<KnowledgeEvidence>,
    pub capability_missing: Option<String>,
}

/// Navigation-ready evidence for the source detail view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeEvidence {
    pub source_id: Uuid,
    pub source_version_id: Uuid,
    pub chunk_id: Uuid,
    pub source_name: String,
    pub purpose: KnowledgePurpose,
    pub locator: ChunkLocator,
    pub text: String,
    pub quote: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeAnswerStatus {
    Answered,
    InsufficientEvidence,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeAskResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_release_id: Option<Uuid>,
    pub mode: String,
    pub answer_status: KnowledgeAnswerStatus,
    pub answer: String,
    pub evidence: Vec<KnowledgeEvidence>,
    pub capability_missing: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadSessionCommand {
    pub filename: String,
    pub declared_media_type: String,
    pub expected_size: u64,
    pub expected_sha256: String,
    pub purpose: KnowledgePurpose,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportItem {
    pub client_item_id: String,
    pub kind: SourceKind,
    pub name: String,
    pub purpose: KnowledgePurpose,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_release_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportAcceptance {
    pub client_item_id: String,
    pub status: ImportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version: Option<SourceVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_job: Option<ImportJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<KnowledgeRelease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportBatchAcceptance {
    pub items: Vec<ImportAcceptance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeOverview {
    pub source_count: u64,
    pub fact_count: u64,
    pub importing_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_release_id: Option<Uuid>,
}

#[async_trait]
pub trait KnowledgeRepository: Send + Sync {
    async fn get_source_version_content(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<SourceVersionContent>, AppError>;
    async fn revise_source_text(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        command: ReviseSourceTextCommand,
    ) -> Result<SourceTextRevisionReceipt, AppError>;
    /// Validate the precise frozen content branches while holding the memory
    /// source read lock until the caller's content transaction has committed.
    /// A PostgreSQL implementation explicitly opts into the transactional
    /// marker; third-party wrappers fail closed unless they implement this.
    async fn hold_content_evidence<'a>(
        &'a self,
        _scope: &TenantScope,
        _inputs: &[ContentPublicEligibility],
    ) -> Result<ContentKnowledgeGuard<'a>, AppError> {
        Err(AppError::capability_missing(
            "atomic content evidence guard is unavailable",
        ))
    }
    async fn capabilities(&self, scope: &TenantScope) -> Result<KnowledgeCapability, AppError>;
    /// Read a single original job; PDF retry successors are never followed implicitly.
    async fn get_import_progress(
        &self,
        _scope: &TenantScope,
        _job_id: Uuid,
        _purpose: KnowledgePurpose,
    ) -> Result<Option<KnowledgeImportProgress>, AppError> {
        Err(AppError::capability_missing(
            "import progress is not configured",
        ))
    }
    /// Resolve only the original scoped receipt for this exact import request.
    async fn resolve_import_receipt(
        &self,
        _scope: &TenantScope,
        _expected: &ImportItem,
    ) -> Result<Option<KnowledgeImportProgress>, AppError> {
        Err(AppError::capability_missing(
            "import receipt lookup is not configured",
        ))
    }
    /// A strict `(created_at, job_id)` cursor, not an offset into an unstable queue.
    async fn pdf_parse_candidates(
        &self,
        _after: Option<PdfParseCursor>,
        _limit: usize,
    ) -> Result<Vec<PdfParseJobRef>, AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn claim_pdf_parse(
        &self,
        _scope: &TenantScope,
        _job_id: Uuid,
        _lease_id: Uuid,
        _lease_seconds: i64,
    ) -> Result<Option<PdfParseLease>, AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn renew_pdf_parse(
        &self,
        _scope: &TenantScope,
        _lease: &PdfParseLease,
        _lease_seconds: i64,
    ) -> Result<Option<PdfParseLease>, AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn pdf_parse_input(
        &self,
        _scope: &TenantScope,
        _lease: &PdfParseLease,
    ) -> Result<PdfParseInput, AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn record_pdf_manifest(
        &self,
        _scope: &TenantScope,
        _lease: &PdfParseLease,
        _manifest: PdfDocumentManifest,
    ) -> Result<(), AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn record_pdf_page(
        &self,
        _scope: &TenantScope,
        _lease: &PdfParseLease,
        _result: PdfPageResult,
    ) -> Result<(), AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn finish_pdf_parse(
        &self,
        _scope: &TenantScope,
        _lease: &PdfParseLease,
    ) -> Result<ImportAcceptance, AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn fail_pdf_parse(
        &self,
        _scope: &TenantScope,
        _lease: &PdfParseLease,
        _code: &str,
    ) -> Result<ImportAcceptance, AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn retry_pdf_parse(
        &self,
        _scope: &TenantScope,
        _job_id: Uuid,
    ) -> Result<ImportJob, AppError> {
        Err(AppError::capability_missing("PDF parser is not configured"))
    }
    async fn pdf_parse_operation(
        &self,
        _scope: &TenantScope,
        _job_id: Uuid,
    ) -> Result<Option<Operation>, AppError> {
        Ok(None)
    }
    /// Office queue is independent from PDF pages and unavailable unless a
    /// format-specific adapter profile is explicitly configured.
    async fn office_parse_candidates(
        &self,
        _after: Option<OfficeParseCursor>,
        _limit: usize,
    ) -> Result<Vec<OfficeParseJobRef>, AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn claim_office_parse(
        &self,
        _scope: &TenantScope,
        _job_id: Uuid,
        _lease_id: Uuid,
        _lease_seconds: i64,
    ) -> Result<Option<OfficeParseLease>, AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn renew_office_parse(
        &self,
        _scope: &TenantScope,
        _lease: &OfficeParseLease,
        _lease_seconds: i64,
    ) -> Result<Option<OfficeParseLease>, AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn office_parse_input(
        &self,
        _scope: &TenantScope,
        _lease: &OfficeParseLease,
    ) -> Result<OfficeParseInput, AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn record_office_manifest(
        &self,
        _scope: &TenantScope,
        _lease: &OfficeParseLease,
        _manifest: OfficeDocumentManifest,
    ) -> Result<(), AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn record_office_unit(
        &self,
        _scope: &TenantScope,
        _lease: &OfficeParseLease,
        _result: OfficeUnitResult,
    ) -> Result<(), AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn finish_office_parse(
        &self,
        _scope: &TenantScope,
        _lease: &OfficeParseLease,
    ) -> Result<ImportAcceptance, AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn fail_office_parse(
        &self,
        _scope: &TenantScope,
        _lease: &OfficeParseLease,
        _code: &str,
    ) -> Result<ImportAcceptance, AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn retry_office_parse(
        &self,
        _scope: &TenantScope,
        _job_id: Uuid,
    ) -> Result<ImportJob, AppError> {
        Err(AppError::capability_missing(
            "Office parser is not configured",
        ))
    }
    async fn office_parse_operation(
        &self,
        _scope: &TenantScope,
        _job_id: Uuid,
    ) -> Result<Option<Operation>, AppError> {
        Ok(None)
    }
    async fn create_upload_session(
        &self,
        scope: &TenantScope,
        command: UploadSessionCommand,
    ) -> Result<UploadSession, AppError>;
    async fn put_upload_content(
        &self,
        scope: &TenantScope,
        id: Uuid,
        content: Vec<u8>,
    ) -> Result<UploadSession, AppError>;
    async fn complete_upload(
        &self,
        scope: &TenantScope,
        id: Uuid,
        idempotency_key: &str,
    ) -> Result<ImportAcceptance, AppError>;
    /// Commit verified upload bytes as an object without creating a knowledge source.
    async fn complete_attachment_upload(
        &self,
        scope: &TenantScope,
        id: Uuid,
        idempotency_key: &str,
    ) -> Result<(StoredObject, String), AppError>;
    /// Read committed object metadata and the original upload filename within scope.
    async fn get_attachment_object(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<(StoredObject, String)>, AppError>;
    /// Read only an agent-attachment upload committed in this project, matching
    /// its immutable object identity, version, and digest. This is not a public
    /// media authorization or MIME validation boundary.
    async fn get_attachment_object_bytes(
        &self,
        _scope: &TenantScope,
        _object_id: Uuid,
        _object_version: i64,
        _sha256: &str,
    ) -> Result<Option<AttachmentObjectBytes>, AppError> {
        Err(AppError::capability_missing(
            "committed attachment byte reading is unavailable",
        ))
    }
    async fn import_batch(
        &self,
        scope: &TenantScope,
        items: Vec<ImportItem>,
    ) -> Result<ImportBatchAcceptance, AppError>;
    async fn list_sources(&self, scope: &TenantScope) -> Result<Vec<Source>, AppError>;
    async fn get_source(&self, scope: &TenantScope, id: Uuid) -> Result<Option<Source>, AppError>;
    async fn get_source_detail(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SourceDetail>, AppError>;
    async fn get_source_version(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<SourceVersion>, AppError>;
    async fn list_products(&self, scope: &TenantScope) -> Result<Vec<Product>, AppError>;
    async fn list_facts(&self, scope: &TenantScope) -> Result<Vec<Fact>, AppError>;
    async fn current_release(
        &self,
        scope: &TenantScope,
    ) -> Result<CurrentKnowledgeRelease, AppError>;
    async fn get_release(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<KnowledgeRelease>, AppError>;
    async fn get_document_manifest(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<DocumentManifest>, AppError>;
    async fn plan_document_manifest(
        &self,
        scope: &TenantScope,
        request: DocumentManifestPlanRequest,
        document_scope: DocumentScope,
    ) -> Result<DocumentManifest, AppError>;
    async fn search(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, AppError>;
    async fn ask(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeAskResult, AppError>;
    async fn overview(&self, scope: &TenantScope) -> Result<KnowledgeOverview, AppError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPublicEligibility {
    pub document_manifest_id: Uuid,
    pub document_manifest_item_id: Uuid,
    pub source_version_ids: Vec<Uuid>,
    pub evidence: Vec<crate::ContentEvidence>,
}

pub struct ContentKnowledgeGuard<'a> {
    mode: crate::project::ContentGuardMode,
    _hold: Option<Box<dyn Send + 'a>>,
}

impl<'a> ContentKnowledgeGuard<'a> {
    pub fn transactional() -> Self {
        Self {
            mode: crate::project::ContentGuardMode::Transactional,
            _hold: None,
        }
    }

    fn held(guard: tokio::sync::RwLockReadGuard<'a, MemoryState>) -> Self {
        Self {
            mode: crate::project::ContentGuardMode::Held,
            _hold: Some(Box::new(guard)),
        }
    }

    pub fn mode(&self) -> crate::project::ContentGuardMode {
        self.mode
    }
}

#[derive(Debug, Default)]
pub struct MemoryKnowledgeRepository {
    state: RwLock<MemoryState>,
    pdf_parser_profile: Option<String>,
    docx_parser_profile: Option<String>,
    xlsx_parser_profile: Option<String>,
}

#[derive(Debug, Clone)]
struct MemoryPdfState {
    object_id: Uuid,
    parser_profile: String,
    created_at: DateTime<Utc>,
    manifest: Option<PdfDocumentManifest>,
    pages: HashMap<u32, PdfPageResult>,
    lease: Option<PdfParseLease>,
    fencing_token: i64,
    acceptance: Option<ImportAcceptance>,
    parent_job_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
struct MemoryOfficeState {
    object_id: Uuid,
    format: OfficeFormat,
    parser_profile: String,
    created_at: DateTime<Utc>,
    manifest: Option<OfficeDocumentManifest>,
    units: HashMap<u32, OfficeUnitResult>,
    lease: Option<OfficeParseLease>,
    fencing_token: i64,
    acceptance: Option<ImportAcceptance>,
}

#[derive(Debug, Default)]
struct MemoryState {
    upload_sessions: HashMap<Uuid, UploadSession>,
    upload_bytes: HashMap<Uuid, Vec<u8>>,
    stored_objects: HashMap<Uuid, StoredObject>,
    object_bytes: HashMap<Uuid, Vec<u8>>,
    sources: HashMap<Uuid, Source>,
    versions: HashMap<Uuid, SourceVersion>,
    authored_text: HashMap<Uuid, (String, String)>,
    revision_receipts: HashMap<(String, Uuid, String), (String, SourceTextRevisionReceipt)>,
    jobs: HashMap<Uuid, ImportJob>,
    pdf_jobs: HashMap<Uuid, MemoryPdfState>,
    office_jobs: HashMap<Uuid, MemoryOfficeState>,
    operations: HashMap<Uuid, Operation>,
    chunks: HashMap<Uuid, Vec<Chunk>>,
    products: HashMap<Uuid, Product>,
    facts: HashMap<Uuid, Fact>,
    releases: HashMap<Uuid, KnowledgeRelease>,
    document_manifests: HashMap<Uuid, DocumentManifest>,
    current_release: HashMap<String, Uuid>,
    import_items: HashMap<(String, String), (String, ImportAcceptance)>,
    upload_completions: HashMap<(Uuid, String), ImportAcceptance>,
    attachment_completions: HashMap<Uuid, String>,
}

impl MemoryKnowledgeRepository {
    fn progress_for_job_locked(
        state: &MemoryState,
        scope: &TenantScope,
        job: &ImportJob,
        purpose: KnowledgePurpose,
    ) -> Result<Option<KnowledgeImportProgress>, AppError> {
        let Some(source) = state
            .sources
            .get(&job.source_id)
            .filter(|source| Self::in_scope(scope, *source))
            .filter(|source| {
                source.state == SourceState::Active
                    && (purpose == KnowledgePurpose::Internal
                        || source.purpose == KnowledgePurpose::Public)
            })
        else {
            return Ok(None);
        };
        let (error_count, errors) = knowledge_import_progress_errors(&job.errors);
        let mut progress = KnowledgeImportProgress {
            import_job_id: Some(job.import_job_id),
            status: job.status,
            stage: Some(job.stage),
            source_id: Some(source.source_id),
            source_version_id: None,
            knowledge_release_id: None,
            completed_units: job.completed_units,
            failed_units: job.failed_units,
            error_count,
            errors,
        };
        if matches!(job.status, ImportStatus::Succeeded | ImportStatus::Partial) {
            if job.stage != ImportStage::Release {
                return Err(AppError::conflict("import completion is inconsistent"));
            }
            let version_id = job
                .source_version_id
                .ok_or_else(|| AppError::conflict("import version is missing"))?;
            let version = state
                .versions
                .get(&version_id)
                .filter(|version| Self::in_scope(scope, *version))
                .filter(|version| version.source_id == job.source_id)
                .filter(|version| version.content_sha256 == job.input_hash)
                .ok_or_else(|| AppError::conflict("import version is inconsistent"))?;
            let operation = state
                .operations
                .get(&job.operation_id)
                .filter(|operation| operation.scope == *scope)
                .filter(|operation| operation.status == OperationStatus::Succeeded)
                .ok_or_else(|| AppError::conflict("import operation is inconsistent"))?;
            let result = operation
                .result
                .as_ref()
                .ok_or_else(|| AppError::conflict("import result is missing"))?;
            if result.get("source_id").and_then(Value::as_str)
                != Some(source.source_id.to_string().as_str())
                || result.get("source_version_id").and_then(Value::as_str)
                    != Some(version.source_version_id.to_string().as_str())
            {
                return Err(AppError::conflict("import result is inconsistent"));
            }
            let release_id = result
                .get("knowledge_release_id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(|| AppError::conflict("import release is missing"))?;
            let release = state
                .releases
                .get(&release_id)
                .filter(|release| Self::in_scope(scope, *release))
                .filter(|release| release.source_version_refs.contains(&version_id))
                .ok_or_else(|| AppError::conflict("import release is inconsistent"))?;
            if let Some(pdf) = state.pdf_jobs.get(&job.import_job_id) {
                state
                    .stored_objects
                    .get(&pdf.object_id)
                    .filter(|object| Self::in_scope(scope, *object))
                    .filter(|object| object.sha256 == job.input_hash)
                    .filter(|object| {
                        version.object_id == Some(object.object_id)
                            && version.object_version == Some(object.object_version)
                    })
                    .ok_or_else(|| AppError::conflict("import object is inconsistent"))?;
                let final_acceptance = pdf
                    .acceptance
                    .as_ref()
                    .ok_or_else(|| AppError::conflict("PDF final receipt is missing"))?;
                if final_acceptance.status != job.status
                    || final_acceptance
                        .import_job
                        .as_ref()
                        .map(|accepted| accepted.import_job_id)
                        != Some(job.import_job_id)
                    || final_acceptance
                        .source
                        .as_ref()
                        .map(|accepted| accepted.source_id)
                        != Some(source.source_id)
                    || final_acceptance
                        .source_version
                        .as_ref()
                        .map(|accepted| accepted.source_version_id)
                        != Some(version_id)
                    || final_acceptance
                        .release
                        .as_ref()
                        .map(|accepted| accepted.knowledge_release_id)
                        != Some(release_id)
                {
                    return Err(AppError::conflict("PDF final receipt is inconsistent"));
                }
            } else if let Some(office) = state.office_jobs.get(&job.import_job_id) {
                state
                    .stored_objects
                    .get(&office.object_id)
                    .filter(|object| Self::in_scope(scope, *object))
                    .filter(|object| {
                        object.sha256 == job.input_hash
                            && version.object_id == Some(object.object_id)
                            && version.object_version == Some(object.object_version)
                    })
                    .ok_or_else(|| AppError::conflict("Office import object is inconsistent"))?;
                let acceptance = office
                    .acceptance
                    .as_ref()
                    .ok_or_else(|| AppError::conflict("Office final receipt is missing"))?;
                if acceptance.status != job.status
                    || acceptance
                        .import_job
                        .as_ref()
                        .map(|accepted| accepted.import_job_id)
                        != Some(job.import_job_id)
                    || acceptance
                        .source_version
                        .as_ref()
                        .map(|accepted| accepted.source_version_id)
                        != Some(version_id)
                    || acceptance
                        .release
                        .as_ref()
                        .map(|accepted| accepted.knowledge_release_id)
                        != Some(release_id)
                {
                    return Err(AppError::conflict("Office final receipt is inconsistent"));
                }
            } else if let Some(object_id) = version.object_id {
                state
                    .stored_objects
                    .get(&object_id)
                    .filter(|object| Self::in_scope(scope, *object))
                    .filter(|object| {
                        object.sha256 == job.input_hash
                            && version.object_version == Some(object.object_version)
                    })
                    .ok_or_else(|| AppError::conflict("import object is inconsistent"))?;
            }
            progress.source_version_id = Some(version_id);
            progress.knowledge_release_id = Some(release.knowledge_release_id);
        }
        Ok(Some(progress))
    }

    pub fn with_pdf_parser_profile(profile: String) -> Self {
        Self {
            state: RwLock::new(MemoryState::default()),
            pdf_parser_profile: (!profile.trim().is_empty()).then_some(profile),
            docx_parser_profile: None,
            xlsx_parser_profile: None,
        }
    }

    pub fn with_office_parser_profiles(
        docx_profile: Option<String>,
        xlsx_profile: Option<String>,
    ) -> Self {
        Self::with_parser_profiles(None, docx_profile, xlsx_profile)
    }

    pub fn with_parser_profiles(
        pdf_profile: Option<String>,
        docx_profile: Option<String>,
        xlsx_profile: Option<String>,
    ) -> Self {
        Self {
            state: RwLock::new(MemoryState::default()),
            pdf_parser_profile: pdf_profile.filter(|profile| !profile.trim().is_empty()),
            docx_parser_profile: docx_profile.filter(|profile| !profile.trim().is_empty()),
            xlsx_parser_profile: xlsx_profile.filter(|profile| !profile.trim().is_empty()),
        }
    }

    fn office_profile(&self, format: OfficeFormat) -> Option<&str> {
        match format {
            OfficeFormat::Docx => self.docx_parser_profile.as_deref(),
            OfficeFormat::Xlsx => self.xlsx_parser_profile.as_deref(),
        }
    }

    fn queued_office_format(&self, media_type: &str) -> Option<OfficeFormat> {
        [OfficeFormat::Docx, OfficeFormat::Xlsx]
            .into_iter()
            .find(|format| {
                format.media_type() == media_type && self.office_profile(*format).is_some()
            })
    }

    fn queue_office_locked(
        &self,
        state: &mut MemoryState,
        scope: &TenantScope,
        item: &ImportItem,
        object: &StoredObject,
        format: OfficeFormat,
    ) -> ImportAcceptance {
        let (mut source, _) = Self::source_and_version(
            scope,
            item.kind,
            item.name.clone(),
            item.purpose,
            json!({"kind":"object","object_id":object.object_id,"object_version":object.object_version}),
            Some(object),
            object.sha256.clone(),
        );
        source.current_version_id = None;
        let operation = Operation::queued("knowledge.import", scope.clone());
        let job = ImportJob {
            import_job_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: source.project_id,
            operation_id: operation.id,
            source_id: source.source_id,
            source_version_id: None,
            stage: ImportStage::Parse,
            status: ImportStatus::Queued,
            attempt: 1,
            lease_until: None,
            input_hash: object.sha256.clone(),
            stage_output_refs: Vec::new(),
            completed_units: 0,
            failed_units: 0,
            errors: Vec::new(),
            resumed_from: None,
        };
        state.office_jobs.insert(
            job.import_job_id,
            MemoryOfficeState {
                object_id: object.object_id,
                format,
                parser_profile: self
                    .office_profile(format)
                    .expect("configured parser")
                    .to_owned(),
                created_at: operation.created_at,
                manifest: None,
                units: HashMap::new(),
                lease: None,
                fencing_token: 0,
                acceptance: None,
            },
        );
        state.operations.insert(operation.id, operation.clone());
        state.sources.insert(source.source_id, source.clone());
        state.jobs.insert(job.import_job_id, job.clone());
        ImportAcceptance {
            client_item_id: item.client_item_id.clone(),
            status: ImportStatus::Queued,
            source: Some(source),
            source_version: None,
            import_job: Some(job),
            operation: Some(operation),
            release: None,
            error: None,
        }
    }

    fn checked_office_lease<'a>(
        state: &'a MemoryState,
        scope: &TenantScope,
        lease: &OfficeParseLease,
    ) -> Result<(&'a ImportJob, &'a MemoryOfficeState), AppError> {
        let job = state
            .jobs
            .get(&lease.job_id)
            .filter(|job| Self::in_scope(scope, *job))
            .ok_or_else(|| AppError::not_found("Office parse job not found"))?;
        let office = state
            .office_jobs
            .get(&lease.job_id)
            .ok_or_else(|| AppError::not_found("Office parse job not found"))?;
        if job.status != ImportStatus::Running
            || !office.lease.as_ref().is_some_and(|current| {
                current.job_id == lease.job_id
                    && current.lease_id == lease.lease_id
                    && current.fencing_token == lease.fencing_token
                    && current.expires_at > Utc::now()
            })
        {
            return Err(AppError::conflict(
                "Office parse lease is expired or fenced",
            ));
        }
        Ok((job, office))
    }
    fn queue_pdf_locked(
        &self,
        state: &mut MemoryState,
        scope: &TenantScope,
        item: &ImportItem,
        object: &StoredObject,
    ) -> ImportAcceptance {
        let (mut source, _) = Self::source_and_version(
            scope,
            item.kind,
            item.name.clone(),
            item.purpose,
            json!({"kind":"object","object_id":object.object_id,"object_version":object.object_version}),
            Some(object),
            object.sha256.clone(),
        );
        source.current_version_id = None;
        let operation = Operation::queued("knowledge.import", scope.clone());
        let job = ImportJob {
            import_job_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: source.project_id,
            operation_id: operation.id,
            source_id: source.source_id,
            source_version_id: None,
            stage: ImportStage::Parse,
            status: ImportStatus::Queued,
            attempt: 1,
            lease_until: None,
            input_hash: object.sha256.clone(),
            stage_output_refs: Vec::new(),
            completed_units: 0,
            failed_units: 0,
            errors: Vec::new(),
            resumed_from: None,
        };
        state.pdf_jobs.insert(
            job.import_job_id,
            MemoryPdfState {
                object_id: object.object_id,
                parser_profile: self
                    .pdf_parser_profile
                    .clone()
                    .expect("configured PDF parser"),
                created_at: operation.created_at,
                manifest: None,
                pages: HashMap::new(),
                lease: None,
                fencing_token: 0,
                acceptance: None,
                parent_job_id: None,
            },
        );
        state.operations.insert(operation.id, operation.clone());
        state.sources.insert(source.source_id, source.clone());
        state.jobs.insert(job.import_job_id, job.clone());
        ImportAcceptance {
            client_item_id: item.client_item_id.clone(),
            status: ImportStatus::Queued,
            source: Some(source),
            source_version: None,
            import_job: Some(job),
            operation: Some(operation),
            release: None,
            error: None,
        }
    }

    fn checked_pdf_lease<'a>(
        state: &'a MemoryState,
        scope: &TenantScope,
        lease: &PdfParseLease,
    ) -> Result<(&'a ImportJob, &'a MemoryPdfState), AppError> {
        let job = state
            .jobs
            .get(&lease.job_id)
            .filter(|job| Self::in_scope(scope, *job))
            .ok_or_else(|| AppError::not_found("PDF parse job not found"))?;
        let pdf = state
            .pdf_jobs
            .get(&lease.job_id)
            .ok_or_else(|| AppError::not_found("PDF parse job not found"))?;
        if job.status != ImportStatus::Running
            || !pdf.lease.as_ref().is_some_and(|current| {
                current.job_id == lease.job_id
                    && current.lease_id == lease.lease_id
                    && current.fencing_token == lease.fencing_token
                    && current.expires_at > Utc::now()
            })
        {
            return Err(AppError::conflict("PDF parse lease is expired or fenced"));
        }
        Ok((job, pdf))
    }

    fn require_project(scope: &TenantScope) -> Result<ProjectId, AppError> {
        scope.project_id.ok_or_else(|| {
            AppError::invalid_request("project_id is required for knowledge resources")
        })
    }

    fn in_scope<T: ScopedKnowledge>(scope: &TenantScope, item: &T) -> bool {
        item.operator_id() == scope.operator_id
            && item.tenant_id() == scope.tenant_id
            && scope.project_id == Some(item.project_id())
    }

    fn validate_upload(command: &UploadSessionCommand) -> Result<(), AppError> {
        if command.filename.trim().is_empty() || command.filename.len() > 255 {
            return Err(AppError::invalid_request(
                "filename must be between 1 and 255 characters",
            ));
        }
        if command.declared_media_type.trim().is_empty() || command.declared_media_type.len() > 255
        {
            return Err(AppError::invalid_request("declared_media_type is required"));
        }
        if command.expected_size == 0 || command.expected_size > MAX_UPLOAD_BYTES {
            return Err(AppError::invalid_request(format!(
                "expected_size must be between 1 and {MAX_UPLOAD_BYTES}"
            )));
        }
        validate_sha256(&command.expected_sha256)
    }

    fn source_and_version(
        scope: &TenantScope,
        kind: SourceKind,
        name: String,
        purpose: KnowledgePurpose,
        locator: Value,
        object: Option<&StoredObject>,
        content_hash: String,
    ) -> (Source, SourceVersion) {
        let now = Utc::now();
        let source_id = Uuid::new_v4();
        let version_id = Uuid::new_v4();
        let project_id = scope.project_id.expect("validated project scope");
        let source = Source {
            source_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            revision: 1,
            kind,
            name,
            purpose,
            state: SourceState::Active,
            locator,
            current_version_id: Some(version_id),
            sync_enabled: false,
            next_sync_at: None,
            last_sync_at: None,
        };
        let version = SourceVersion {
            source_version_id: version_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            source_id,
            version: 1,
            representation: SourceVersionRepresentation::Original,
            object_id: object.map(|value| value.object_id),
            object_version: object.map(|value| value.object_version),
            content_sha256: content_hash,
            captured_at: now,
            original_url: None,
            parent_version_id: None,
            parser_version: "deterministic-text-v1".to_owned(),
            extraction_version: "none-v1".to_owned(),
            created_at: now,
        };
        (source, version)
    }

    fn import_text_locked(
        state: &mut MemoryState,
        scope: &TenantScope,
        item: &ImportItem,
        text: String,
        object: Option<StoredObject>,
    ) -> Result<ImportAcceptance, AppError> {
        if object.is_none() && text.len() > MAX_INLINE_TEXT_BYTES {
            return Err(AppError::invalid_request(
                "text exceeds inline limit; use an upload session",
            ));
        }
        if text.trim().is_empty() {
            return Err(AppError::invalid_request("text must not be empty"));
        }
        let hash = sha256_hex(text.as_bytes());
        let media_type = object
            .as_ref()
            .map(|object| object.detected_media_type.as_str())
            .unwrap_or("text/plain");
        let (source, mut version) = Self::source_and_version(
            scope,
            item.kind,
            item.name.clone(),
            item.purpose,
            object
                .as_ref()
                .map(|object| json!({"kind":"object","object_id":object.object_id,"object_version":object.object_version}))
                .unwrap_or_else(|| json!({"kind":"inline_text"})),
            object.as_ref(),
            hash.clone(),
        );
        let mut operation = Operation::queued(
            "knowledge.import",
            TenantScope::new(
                source.operator_id,
                source.tenant_id,
                Some(source.project_id),
            ),
        );
        operation.status = OperationStatus::Succeeded;
        version.parser_version = knowledge_parser_version(media_type).to_owned();
        let chunks = parsed_knowledge_chunks(scope, version.source_version_id, &text, media_type)?;
        let job = ImportJob {
            import_job_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: source.project_id,
            operation_id: operation.id,
            source_id: source.source_id,
            source_version_id: Some(version.source_version_id),
            stage: ImportStage::Release,
            status: ImportStatus::Succeeded,
            attempt: 1,
            lease_until: None,
            input_hash: hash,
            stage_output_refs: vec![format!("chunks:{}", chunks.len())],
            completed_units: chunks.len() as i32,
            failed_units: 0,
            errors: Vec::new(),
            resumed_from: None,
        };
        state.sources.insert(source.source_id, source.clone());
        state
            .versions
            .insert(version.source_version_id, version.clone());
        state.chunks.insert(version.source_version_id, chunks);
        state.jobs.insert(job.import_job_id, job.clone());
        if let Some(object) = object {
            state.stored_objects.insert(object.object_id, object);
        }
        let release = Self::make_release_locked(state, scope)?;
        operation.result = Some(json!({
            "source_id": source.source_id,
            "source_version_id": version.source_version_id,
            "knowledge_release_id": release.knowledge_release_id
        }));
        state.operations.insert(operation.id, operation.clone());
        Ok(ImportAcceptance {
            client_item_id: item.client_item_id.clone(),
            status: ImportStatus::Succeeded,
            source: Some(source),
            source_version: Some(version),
            import_job: Some(job),
            operation: Some(operation),
            release: Some(release),
            error: None,
        })
    }

    fn make_release_locked(
        state: &mut MemoryState,
        scope: &TenantScope,
    ) -> Result<KnowledgeRelease, AppError> {
        let project_id = Self::require_project(scope)?;
        let scope_key = scope.storage_key();
        let previous_release_id = state.current_release.get(&scope_key).copied();
        let sequence = previous_release_id
            .and_then(|id| state.releases.get(&id).map(|release| release.sequence + 1))
            .unwrap_or(1);
        let mut source_versions = state
            .versions
            .values()
            .filter(|version| Self::in_scope(scope, *version))
            .filter(|version| {
                state
                    .sources
                    .get(&version.source_id)
                    .is_some_and(|source| source.state == SourceState::Active)
            })
            .filter(|version| {
                state.chunks.contains_key(&version.source_version_id)
                    && state.sources.get(&version.source_id).is_some_and(|source| {
                        source.current_version_id == Some(version.source_version_id)
                    })
            })
            .map(|version| version.source_version_id)
            .collect::<Vec<_>>();
        source_versions.sort_unstable();
        let chunk_count = source_versions
            .iter()
            .filter_map(|version_id| state.chunks.get(version_id))
            .map(|chunks| chunks.len() as u64)
            .sum();
        let fact_refs = state
            .facts
            .values()
            .filter(|fact| Self::in_scope(scope, *fact))
            .map(|fact| (fact.fact_id, fact.revision))
            .collect::<Vec<_>>();
        let failed_source_count = state
            .jobs
            .values()
            .filter(|job| Self::in_scope(scope, *job))
            .filter(|job| matches!(job.status, ImportStatus::Failed | ImportStatus::Partial))
            .filter(|job| {
                state
                    .sources
                    .get(&job.source_id)
                    .is_some_and(|source| source.current_version_id == job.source_version_id)
            })
            .count() as u64;
        let mut pipeline_versions = json!({
            "parser": "deterministic-text-v1",
            "extractor": "none-v1",
            "index": "substring-v1"
        });
        let parsers = source_versions
            .iter()
            .filter_map(|id| state.versions.get(id))
            .map(|version| version.parser_version.clone())
            .collect::<std::collections::BTreeSet<_>>();
        if parsers.contains("deterministic-csv-v1")
            || parsers.contains("deterministic-csv-v2")
            || parsers.iter().any(|parser| parser.starts_with("tika-"))
        {
            pipeline_versions["parser"] = json!("deterministic-knowledge-v1");
            pipeline_versions["parsers"] = json!(parsers);
        }
        let release = KnowledgeRelease {
            knowledge_release_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            sequence,
            previous_release_id,
            source_version_refs: source_versions.clone(),
            fact_revision_refs: fact_refs,
            index_build_id: "deterministic-text-index-v1".to_owned(),
            pipeline_versions,
            content_hash: sha256_hex(
                source_versions
                    .iter()
                    .map(Uuid::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .as_bytes(),
            ),
            coverage: KnowledgeCoverage {
                source_version_count: source_versions.len() as u64,
                chunk_count,
                failed_source_count,
                blocked_reasons: state
                    .jobs
                    .values()
                    .filter(|job| {
                        Self::in_scope(scope, *job) && job.status == ImportStatus::Partial
                    })
                    .filter(|job| {
                        state.sources.get(&job.source_id).is_some_and(|source| {
                            source.current_version_id == job.source_version_id
                        })
                    })
                    .map(|job| {
                        format!(
                            "source {} has {} failed PDF pages",
                            job.source_id, job.failed_units
                        )
                    })
                    .collect(),
            },
            created_at: Utc::now(),
        };
        state
            .current_release
            .insert(scope_key, release.knowledge_release_id);
        state
            .releases
            .insert(release.knowledge_release_id, release.clone());
        Ok(release)
    }

    fn failed_missing_adapter(
        scope: &TenantScope,
        item: &ImportItem,
        capability: &str,
    ) -> ImportAcceptance {
        let project_id = scope.project_id.expect("validated project scope");
        let source_id = Uuid::new_v4();
        let mut operation = Operation::queued(
            "knowledge.import",
            TenantScope::new(scope.operator_id, scope.tenant_id, Some(project_id)),
        );
        operation.status = OperationStatus::Failed;
        let capability_error =
            AppError::capability_missing(format!("{capability} is not configured"));
        operation.error = Some(capability_error.clone());
        let source = Source {
            source_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            revision: 1,
            kind: item.kind,
            name: item.name.clone(),
            purpose: item.purpose,
            state: SourceState::Active,
            locator: json!({"kind": "pending", "capability": capability}),
            current_version_id: None,
            sync_enabled: false,
            next_sync_at: None,
            last_sync_at: None,
        };
        let job = ImportJob {
            import_job_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            operation_id: operation.id,
            source_id,
            source_version_id: None,
            stage: ImportStage::Acquire,
            status: ImportStatus::Failed,
            attempt: 0,
            lease_until: None,
            input_hash: sha256_hex(item.client_item_id.as_bytes()),
            stage_output_refs: Vec::new(),
            completed_units: 0,
            failed_units: 0,
            errors: vec![json!({"code": "capability_missing", "capability": capability})],
            resumed_from: None,
        };
        ImportAcceptance {
            client_item_id: item.client_item_id.clone(),
            status: ImportStatus::Failed,
            source: Some(source),
            source_version: None,
            import_job: Some(job),
            operation: Some(operation),
            release: None,
            error: Some(capability_error),
        }
    }
}

trait ScopedKnowledge {
    fn operator_id(&self) -> OperatorId;
    fn tenant_id(&self) -> TenantId;
    fn project_id(&self) -> ProjectId;
}

macro_rules! scoped_knowledge {
    ($($type:ty),+ $(,)?) => {$(
        impl ScopedKnowledge for $type {
            fn operator_id(&self) -> OperatorId { self.operator_id }
            fn tenant_id(&self) -> TenantId { self.tenant_id }
            fn project_id(&self) -> ProjectId { self.project_id }
        }
    )+};
}
scoped_knowledge!(
    UploadSession,
    StoredObject,
    Source,
    SourceVersion,
    ImportJob,
    Chunk,
    Product,
    Fact,
    KnowledgeRelease
);

#[async_trait]
impl KnowledgeRepository for MemoryKnowledgeRepository {
    async fn get_source_version_content(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<SourceVersionContent>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        let Some(version) = state
            .versions
            .get(&version_id)
            .filter(|version| version.source_id == source_id && Self::in_scope(scope, *version))
        else {
            return Ok(None);
        };
        if let Some((media_type, text)) = state.authored_text.get(&version_id) {
            return Ok(Some(SourceVersionContent {
                source_version_id: version_id,
                representation: SourceVersionRepresentation::AuthoredText,
                media_type: media_type.clone(),
                text: text.clone(),
                text_basis: SourceTextBasis::Exact,
            }));
        }
        if let Some(object_id) = version.object_id
            && let (Some(object), Some(bytes)) = (
                state.stored_objects.get(&object_id),
                state.object_bytes.get(&object_id),
            )
            && matches!(
                object.detected_media_type.as_str(),
                "text/plain" | "text/markdown"
            )
            && object.object_version == version.object_version.unwrap_or_default()
            && bytes.len() as u64 == object.actual_size
            && sha256_hex(bytes) == object.sha256
            && object.sha256 == version.content_sha256
            && let Ok(text) = String::from_utf8(bytes.clone())
        {
            return Ok(Some(SourceVersionContent {
                source_version_id: version_id,
                representation: SourceVersionRepresentation::Original,
                media_type: object.detected_media_type.clone(),
                text,
                text_basis: SourceTextBasis::Exact,
            }));
        }
        let mut chunks = state.chunks.get(&version_id).cloned().unwrap_or_default();
        chunks.sort_by_key(|chunk| chunk.ordinal);
        Ok(Some(SourceVersionContent {
            source_version_id: version_id,
            representation: SourceVersionRepresentation::Original,
            media_type: "text/plain".to_owned(),
            text: chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            text_basis: SourceTextBasis::Extracted,
        }))
    }

    async fn revise_source_text(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        command: ReviseSourceTextCommand,
    ) -> Result<SourceTextRevisionReceipt, AppError> {
        let project_id = Self::require_project(scope)?;
        command.validate()?;
        if idempotency_key.is_empty() {
            return Err(AppError::invalid_request(
                "Idempotency-Key must not be empty",
            ));
        }
        let fingerprint = sha256_hex(
            &serde_json::to_vec(&(expected_revision, &command))
                .map_err(|_| AppError::invalid_request("invalid revision request"))?,
        );
        let receipt_key = (
            scope.storage_key(),
            source_id,
            sha256_hex(idempotency_key.as_bytes()),
        );
        let mut state = self.state.write().await;
        if let Some((stored_hash, receipt)) = state.revision_receipts.get(&receipt_key) {
            return if stored_hash == &fingerprint {
                Ok(receipt.clone())
            } else {
                Err(
                    AppError::conflict("idempotency key reused for a different request")
                        .with_details(json!({"reason":"idempotency_conflict"})),
                )
            };
        }
        let source = state
            .sources
            .get(&source_id)
            .filter(|source| Self::in_scope(scope, *source))
            .cloned()
            .ok_or_else(|| AppError::not_found("knowledge source not found"))?;
        let base = state
            .versions
            .get(&command.base_version_id)
            .filter(|version| version.source_id == source_id && Self::in_scope(scope, *version))
            .cloned()
            .ok_or_else(|| AppError::not_found("source version not found"))?;
        if source.state != SourceState::Active
            || source.revision != expected_revision
            || source.current_version_id != Some(base.source_version_id)
        {
            return Err(AppError::conflict("knowledge source revision changed")
                .with_details(json!({"reason":"source_revision_conflict"})));
        }
        if state.jobs.values().any(|job| {
            job.source_id == source_id
                && Self::in_scope(scope, job)
                && matches!(job.status, ImportStatus::Queued | ImportStatus::Running)
                && (state.pdf_jobs.contains_key(&job.import_job_id)
                    || state.office_jobs.contains_key(&job.import_job_id))
        }) {
            return Err(AppError::conflict("source parse in progress")
                .with_details(json!({"reason":"source_parse_in_progress"})));
        }
        let next_version = state
            .versions
            .values()
            .filter(|version| version.source_id == source_id && Self::in_scope(scope, *version))
            .map(|version| version.version)
            .max()
            .unwrap_or(base.version)
            + 1;
        let now = Utc::now();
        let version_id = Uuid::new_v4();
        let chunks =
            parsed_knowledge_chunks(scope, version_id, &command.text, &command.media_type)?;
        let version = SourceVersion {
            source_version_id: version_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            source_id,
            version: next_version,
            representation: SourceVersionRepresentation::AuthoredText,
            object_id: None,
            object_version: None,
            content_sha256: sha256_hex(command.text.as_bytes()),
            captured_at: now,
            original_url: None,
            parent_version_id: Some(base.source_version_id),
            parser_version: knowledge_parser_version(&command.media_type).to_owned(),
            extraction_version: "authored-text-v1".to_owned(),
            created_at: now,
        };
        let mut source = source;
        source.revision += 1;
        source.current_version_id = Some(version_id);
        state.versions.insert(version_id, version.clone());
        state.chunks.insert(version_id, chunks);
        state
            .authored_text
            .insert(version_id, (command.media_type, command.text));
        state.sources.insert(source_id, source.clone());
        let knowledge_release = Self::make_release_locked(&mut state, scope)?;
        let receipt = SourceTextRevisionReceipt {
            source,
            source_version: version,
            knowledge_release,
        };
        state
            .revision_receipts
            .insert(receipt_key, (fingerprint, receipt.clone()));
        Ok(receipt)
    }

    async fn hold_content_evidence<'a>(
        &'a self,
        scope: &TenantScope,
        inputs: &[ContentPublicEligibility],
    ) -> Result<ContentKnowledgeGuard<'a>, AppError> {
        let project_id = Self::require_project(scope)?;
        let guard = self.state.read().await;
        for input in inputs {
            let manifest = guard
                .document_manifests
                .get(&input.document_manifest_id)
                .filter(|manifest| {
                    manifest.operator_id == scope.operator_id
                        && manifest.tenant_id == scope.tenant_id
                        && manifest.project_id == project_id
                        && manifest.sealed
                        && manifest.expected_count == Some(manifest.items.len() as i64)
                })
                .ok_or_else(|| AppError::conflict("frozen public document manifest missing"))?;
            let planned = manifest
                .items
                .iter()
                .find(|item| item.document_manifest_item_id == input.document_manifest_item_id)
                .filter(|item| {
                    item.manifest_id == manifest.manifest_id
                        && item.knowledge_release_id == manifest.knowledge_release_id
                        && item.state == DocumentManifestItemState::Planned
                })
                .ok_or_else(|| AppError::conflict("frozen public document branch missing"))?;
            let mut expected = planned.source_version_refs.clone();
            let mut actual = input.source_version_ids.clone();
            expected.sort_unstable();
            actual.sort_unstable();
            if expected.is_empty() || expected != actual {
                return Err(AppError::conflict(
                    "public source dependencies differ from frozen branch",
                ));
            }
            let release = guard
                .releases
                .get(&manifest.knowledge_release_id)
                .filter(|release| {
                    release.operator_id == scope.operator_id
                        && release.tenant_id == scope.tenant_id
                        && release.project_id == project_id
                })
                .ok_or_else(|| AppError::conflict("frozen public source release missing"))?;
            for id in &expected {
                if !release.source_version_refs.contains(id) {
                    return Err(AppError::conflict(
                        "public source is outside frozen release",
                    ));
                }
                let version = guard
                    .versions
                    .get(id)
                    .filter(|version| Self::in_scope(scope, *version))
                    .ok_or_else(|| AppError::conflict("frozen public source version missing"))?;
                let source = guard
                    .sources
                    .get(&version.source_id)
                    .filter(|source| {
                        Self::in_scope(scope, *source)
                            && source.purpose == KnowledgePurpose::Public
                            && source.state == SourceState::Active
                            && source.current_version_id == Some(*id)
                    })
                    .ok_or_else(|| {
                        AppError::conflict("frozen source no longer public or current")
                    })?;
                if source.source_id != version.source_id {
                    return Err(AppError::conflict("source/version ownership differs"));
                }
            }
            for quote in &input.evidence {
                let reference = &quote.reference;
                if !expected.contains(&reference.source_version_id)
                    || quote.exact_quote.trim().is_empty()
                {
                    return Err(AppError::conflict(
                        "public quote is outside frozen dependencies",
                    ));
                }
                let chunk_id = reference
                    .chunk_id
                    .ok_or_else(|| AppError::conflict("public quote must identify a chunk"))?;
                if !guard
                    .chunks
                    .get(&reference.source_version_id)
                    .is_some_and(|chunks| {
                        chunks.iter().any(|chunk| {
                            Self::in_scope(scope, chunk)
                                && chunk.chunk_id == chunk_id
                                && chunk.source_version_id == reference.source_version_id
                                && chunk.locator == reference.locator
                                && match &chunk.locator {
                                    ChunkLocator::Csv { .. } => {
                                        chunk.text.chars().count()
                                            <= CONTENT_EVIDENCE_MAX_QUOTE_CHARS
                                            && chunk.text == quote.exact_quote
                                    }
                                    _ => {
                                        chunk
                                            .text
                                            .chars()
                                            .take(CONTENT_EVIDENCE_MAX_QUOTE_CHARS)
                                            .collect::<String>()
                                            == quote.exact_quote
                                    }
                                }
                        })
                    })
                {
                    return Err(AppError::conflict(
                        "public evidence quote or locator changed",
                    ));
                }
            }
        }
        Ok(ContentKnowledgeGuard::held(guard))
    }
    async fn capabilities(&self, scope: &TenantScope) -> Result<KnowledgeCapability, AppError> {
        Self::require_project(scope)?;
        let mut capability = KnowledgeCapability::memory();
        if self.pdf_parser_profile.is_some() {
            capability.pdf_parser = true;
            capability
                .supported_media_types
                .push("application/pdf".to_owned());
            capability
                .accepted_unparsed_media_types
                .retain(|media| media != "application/pdf");
            capability
                .limitations
                .retain(|line| !line.contains("office/PDF parsing"));
            capability.limitations.push(
                "URL acquisition, office parsing, vector search, and LLM answers require adapters"
                    .to_owned(),
            );
            capability
                .limitations
                .push("OCR for scanned PDFs is not configured".to_owned());
        }
        for format in [OfficeFormat::Docx, OfficeFormat::Xlsx] {
            if self.office_profile(format).is_some() {
                match format {
                    OfficeFormat::Docx => capability.docx_parser = true,
                    OfficeFormat::Xlsx => capability.xlsx_parser = true,
                }
                capability
                    .supported_media_types
                    .push(format.media_type().to_owned());
                capability
                    .accepted_unparsed_media_types
                    .retain(|mime| mime != format.media_type());
            }
        }
        if capability.docx_parser || capability.xlsx_parser {
            capability.limitations.retain(|line| {
                !line.contains("office/PDF parsing") && !line.contains("office parsing,")
            });
            capability.limitations.push(
                "Office structural import does not execute formulas, fetch external resources, or infer headers; OCR, vector search and LLM answers are not configured"
                    .to_owned(),
            );
        }
        Ok(capability)
    }

    async fn get_import_progress(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
        purpose: KnowledgePurpose,
    ) -> Result<Option<KnowledgeImportProgress>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        let Some(job) = state
            .jobs
            .get(&job_id)
            .filter(|job| Self::in_scope(scope, *job))
        else {
            return Ok(None);
        };
        Self::progress_for_job_locked(&state, scope, job, purpose)
    }

    async fn resolve_import_receipt(
        &self,
        scope: &TenantScope,
        expected: &ImportItem,
    ) -> Result<Option<KnowledgeImportProgress>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        let key = (
            scope.storage_key(),
            expected.client_item_id.trim().to_owned(),
        );
        let Some((hash, receipt)) = state.import_items.get(&key) else {
            return Ok(None);
        };
        if *hash != import_item_hash(expected)? {
            return Err(AppError::conflict(
                "client_item_id was already used with different input",
            ));
        }
        if let Some(job) = &receipt.import_job {
            let current = state
                .jobs
                .get(&job.import_job_id)
                .filter(|current| Self::in_scope(scope, *current))
                .ok_or_else(|| AppError::conflict("import receipt job is inconsistent"))?;
            if job.source_id != current.source_id
                || job.operation_id != current.operation_id
                || receipt.source.as_ref().map(|source| source.source_id) != Some(current.source_id)
            {
                return Err(AppError::conflict("import receipt job is inconsistent"));
            }
            if state.sources.get(&current.source_id).is_none_or(|source| {
                !Self::in_scope(scope, source)
                    || source.state != SourceState::Active
                    || source.purpose != expected.purpose
            }) {
                return Ok(None);
            }
            return Self::progress_for_job_locked(&state, scope, current, expected.purpose);
        }
        if receipt.status != ImportStatus::Failed
            || receipt.source_version.is_some()
            || receipt.release.is_some()
        {
            return Err(AppError::conflict("import receipt is inconsistent"));
        }
        if let Some(source) = receipt.source.as_ref() {
            let Some(current) = state
                .sources
                .get(&source.source_id)
                .filter(|current| Self::in_scope(scope, *current))
                .filter(|current| {
                    current.state == SourceState::Active && current.purpose == expected.purpose
                })
            else {
                return Ok(None);
            };
            if source.source_id != current.source_id {
                return Err(AppError::conflict("import receipt source is inconsistent"));
            }
        }
        let error = receipt
            .error
            .as_ref()
            .map(knowledge_import_progress_app_error);
        Ok(Some(KnowledgeImportProgress {
            import_job_id: None,
            status: ImportStatus::Failed,
            stage: None,
            source_id: receipt.source.as_ref().map(|source| source.source_id),
            source_version_id: None,
            knowledge_release_id: None,
            completed_units: 0,
            failed_units: 0,
            error_count: u32::from(error.is_some()),
            errors: error.into_iter().collect(),
        }))
    }

    async fn pdf_parse_candidates(
        &self,
        after: Option<PdfParseCursor>,
        limit: usize,
    ) -> Result<Vec<PdfParseJobRef>, AppError> {
        if self.pdf_parser_profile.is_none() {
            return Err(AppError::capability_missing("PDF parser is not configured"));
        }
        let state = self.state.read().await;
        let mut candidates = state
            .pdf_jobs
            .iter()
            .filter_map(|(job_id, pdf)| {
                let job = state.jobs.get(job_id)?;
                let eligible = job.status == ImportStatus::Queued
                    || (job.status == ImportStatus::Running
                        && pdf
                            .lease
                            .as_ref()
                            .is_none_or(|lease| lease.expires_at <= Utc::now()));
                let after_cursor = after.is_none_or(|after| {
                    (pdf.created_at, *job_id) > (after.created_at, after.job_id)
                });
                (eligible && after_cursor).then_some(PdfParseJobRef {
                    scope: TenantScope::new(job.operator_id, job.tenant_id, Some(job.project_id)),
                    job_id: *job_id,
                    created_at: pdf.created_at,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| (candidate.created_at, candidate.job_id));
        candidates.truncate(limit.min(1000));
        Ok(candidates)
    }

    async fn claim_pdf_parse(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
        lease_id: Uuid,
        lease_seconds: i64,
    ) -> Result<Option<PdfParseLease>, AppError> {
        Self::require_project(scope)?;
        if !(1..=3600).contains(&lease_seconds) || lease_id.is_nil() {
            return Err(AppError::invalid_request(
                "invalid PDF lease duration or ID",
            ));
        }
        let mut state = self.state.write().await;
        let Some(job) = state
            .jobs
            .get(&job_id)
            .filter(|job| Self::in_scope(scope, *job))
        else {
            return Ok(None);
        };
        if !matches!(job.status, ImportStatus::Queued | ImportStatus::Running) {
            return Ok(None);
        }
        let Some(pdf) = state.pdf_jobs.get_mut(&job_id) else {
            return Ok(None);
        };
        if pdf
            .lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at > Utc::now())
        {
            return Ok(None);
        }
        pdf.fencing_token += 1;
        let lease = PdfParseLease {
            job_id,
            lease_id,
            fencing_token: pdf.fencing_token,
            expires_at: Utc::now() + Duration::seconds(lease_seconds),
        };
        pdf.lease = Some(lease.clone());
        let job = state.jobs.get_mut(&job_id).expect("scoped job");
        job.status = ImportStatus::Running;
        job.lease_until = Some(lease.expires_at);
        let operation_id = job.operation_id;
        if let Some(operation) = state.operations.get_mut(&operation_id) {
            operation.status = OperationStatus::Running;
            operation.updated_at = Utc::now();
        }
        Ok(Some(lease))
    }

    async fn renew_pdf_parse(
        &self,
        scope: &TenantScope,
        lease: &PdfParseLease,
        lease_seconds: i64,
    ) -> Result<Option<PdfParseLease>, AppError> {
        if !(1..=3600).contains(&lease_seconds) {
            return Err(AppError::invalid_request("invalid PDF lease duration"));
        }
        let mut state = self.state.write().await;
        if Self::checked_pdf_lease(&state, scope, lease).is_err() {
            return Ok(None);
        }
        let renewed = PdfParseLease {
            expires_at: Utc::now() + Duration::seconds(lease_seconds),
            ..lease.clone()
        };
        state
            .pdf_jobs
            .get_mut(&lease.job_id)
            .expect("checked")
            .lease = Some(renewed.clone());
        state
            .jobs
            .get_mut(&lease.job_id)
            .expect("checked")
            .lease_until = Some(renewed.expires_at);
        Ok(Some(renewed))
    }

    async fn pdf_parse_input(
        &self,
        scope: &TenantScope,
        lease: &PdfParseLease,
    ) -> Result<PdfParseInput, AppError> {
        let state = self.state.read().await;
        let (job, pdf) = Self::checked_pdf_lease(&state, scope, lease)?;
        let object = state
            .stored_objects
            .get(&pdf.object_id)
            .filter(|object| Self::in_scope(scope, *object))
            .ok_or_else(|| AppError::not_found("PDF object not found"))?;
        let bytes = state
            .object_bytes
            .get(&pdf.object_id)
            .ok_or_else(|| AppError::not_found("PDF bytes not found"))?;
        if object.actual_size != bytes.len() as u64
            || sha256_hex(bytes) != object.sha256
            || job.input_hash != object.sha256
            || object.detected_media_type != "application/pdf"
        {
            return Err(AppError::conflict(
                "PDF bytes or media type no longer match verified object",
            ));
        }
        let mut successful_pages = pdf
            .pages
            .iter()
            .filter_map(|(page, result)| {
                matches!(result, PdfPageResult::Success { .. }).then_some(*page)
            })
            .collect::<Vec<_>>();
        successful_pages.sort_unstable();
        Ok(PdfParseInput {
            bytes: bytes.clone(),
            input_sha256: object.sha256.clone(),
            media_type: object.detected_media_type.clone(),
            parser_profile: pdf.parser_profile.clone(),
            successful_pages,
            manifest: pdf.manifest.clone(),
        })
    }

    async fn record_pdf_manifest(
        &self,
        scope: &TenantScope,
        lease: &PdfParseLease,
        manifest: PdfDocumentManifest,
    ) -> Result<(), AppError> {
        let mut state = self.state.write().await;
        let (job, pdf) = Self::checked_pdf_lease(&state, scope, lease)?;
        manifest.validate(&job.input_hash, &pdf.parser_profile)?;
        if pdf
            .manifest
            .as_ref()
            .is_some_and(|prior| prior != &manifest)
        {
            return Err(AppError::conflict(
                "PDF manifest cannot change across retries",
            ));
        }
        state
            .pdf_jobs
            .get_mut(&lease.job_id)
            .expect("checked")
            .manifest = Some(manifest);
        Ok(())
    }

    async fn record_pdf_page(
        &self,
        scope: &TenantScope,
        lease: &PdfParseLease,
        result: PdfPageResult,
    ) -> Result<(), AppError> {
        let mut state = self.state.write().await;
        let (_, pdf) = Self::checked_pdf_lease(&state, scope, lease)?;
        let manifest = pdf
            .manifest
            .as_ref()
            .ok_or_else(|| AppError::conflict("PDF manifest not recorded"))?;
        result.validate(manifest.page_count)?;
        if let Some(prior) = pdf.pages.get(&result.page()) {
            if prior == &result {
                return Ok(());
            }
            if matches!(prior, PdfPageResult::Success { .. }) {
                return Err(AppError::conflict(
                    "successful PDF page cannot be overwritten",
                ));
            }
        }
        let result = match result {
            PdfPageResult::Success { page, text }
                if pdf
                    .pages
                    .iter()
                    .filter(|(existing_page, _)| **existing_page != page)
                    .filter_map(|(_, existing)| match existing {
                        PdfPageResult::Success { text, .. } => Some(text.len()),
                        PdfPageResult::Failure { .. } => None,
                    })
                    .sum::<usize>()
                    .saturating_add(text.len())
                    > PDF_MAX_DOCUMENT_TEXT_BYTES =>
            {
                PdfPageResult::Failure {
                    page,
                    code: "page_limit".to_owned(),
                }
            }
            result => result,
        };
        let pdf = state.pdf_jobs.get_mut(&lease.job_id).expect("checked");
        pdf.pages.insert(result.page(), result);
        let completed_units = pdf
            .pages
            .values()
            .filter(|page| matches!(page, PdfPageResult::Success { .. }))
            .count() as i32;
        let mut errors = pdf
            .pages
            .iter()
            .filter_map(|(page, result)| match result {
                PdfPageResult::Failure { code, .. } => Some(json!({"page":page,"code":code})),
                PdfPageResult::Success { .. } => None,
            })
            .collect::<Vec<_>>();
        errors.sort_by_key(|error| error["page"].as_u64());
        let job = state.jobs.get_mut(&lease.job_id).expect("checked");
        job.completed_units = completed_units;
        job.failed_units = errors.len() as i32;
        job.errors = errors;
        Ok(())
    }

    async fn finish_office_parse(
        &self,
        scope: &TenantScope,
        lease: &OfficeParseLease,
    ) -> Result<ImportAcceptance, AppError> {
        let mut state = self.state.write().await;
        let (job, office) = Self::checked_office_lease(&state, scope, lease)?;
        let manifest = office
            .manifest
            .clone()
            .ok_or_else(|| AppError::conflict("Office manifest not recorded"))?;
        if office.units.len() != manifest.unit_count() {
            return Err(AppError::conflict("Office units are incomplete"));
        }
        let job = job.clone();
        let office = office.clone();
        let mut source = state
            .sources
            .get(&job.source_id)
            .filter(|source| Self::in_scope(scope, *source))
            .cloned()
            .ok_or_else(|| AppError::not_found("Office source not found"))?;
        if source.state != SourceState::Active {
            return Err(AppError::conflict("Office source was removed"));
        }
        if source.current_version_id.is_some_and(|id| {
            state.versions.get(&id).is_some_and(|version| {
                version.representation == SourceVersionRepresentation::AuthoredText
            })
        }) {
            return Err(AppError::conflict(
                "Office source has a newer authored version",
            ));
        }
        let object = state
            .stored_objects
            .get(&office.object_id)
            .filter(|object| Self::in_scope(scope, *object))
            .cloned()
            .ok_or_else(|| AppError::not_found("Office object not found"))?;
        let bytes = state
            .object_bytes
            .get(&office.object_id)
            .ok_or_else(|| AppError::not_found("Office bytes not found"))?;
        if sha256_hex(bytes) != object.sha256
            || bytes.len() as u64 != object.actual_size
            || job.input_hash != object.sha256
            || object.detected_media_type != office.format.media_type()
        {
            return Err(AppError::conflict("Office object changed during parsing"));
        }
        let success_count = office
            .units
            .values()
            .filter(|result| result.is_success())
            .count();
        let failed_count = manifest.unit_count() - success_count;
        let status = if success_count == 0 {
            ImportStatus::Failed
        } else if failed_count == 0 {
            ImportStatus::Succeeded
        } else {
            ImportStatus::Partial
        };
        // Build all chunks and their bounded locators before modifying state;
        // an invalid unit may not leave a half-published release behind.
        let (version, chunks) = if success_count > 0 {
            let parent_id = source.current_version_id;
            let version = SourceVersion {
                source_version_id: Uuid::new_v4(),
                operator_id: scope.operator_id,
                tenant_id: scope.tenant_id,
                project_id: source.project_id,
                source_id: source.source_id,
                version: parent_id
                    .and_then(|id| state.versions.get(&id))
                    .map_or(1, |parent| parent.version + 1),
                representation: SourceVersionRepresentation::Original,
                object_id: Some(object.object_id),
                object_version: Some(object.object_version),
                content_sha256: object.sha256.clone(),
                captured_at: Utc::now(),
                original_url: None,
                parent_version_id: parent_id,
                parser_version: office.parser_profile.clone(),
                extraction_version: office.format.extraction_method().to_owned(),
                created_at: Utc::now(),
            };
            let mut chunks = Vec::new();
            for ordinal in 0..manifest.unit_count() {
                let unit_id = manifest.unit_id(ordinal).expect("validated manifest");
                let result = office
                    .units
                    .get(&unit_id)
                    .expect("all Office units recorded");
                if result.is_success() {
                    chunks.extend(office_unit_chunks(
                        scope,
                        version.source_version_id,
                        &manifest,
                        result,
                        chunks.len() as i32,
                    )?);
                }
            }
            if chunks.is_empty() {
                return Err(AppError::conflict(
                    "Office success has no usable source evidence",
                ));
            }
            (Some(version), chunks)
        } else {
            (None, Vec::new())
        };
        if let Some(version) = &version {
            source.current_version_id = Some(version.source_version_id);
            state.sources.insert(source.source_id, source.clone());
            state.chunks.insert(version.source_version_id, chunks);
            state
                .versions
                .insert(version.source_version_id, version.clone());
        }
        let mut finished_job = job;
        finished_job.status = status;
        finished_job.stage = if version.is_some() {
            ImportStage::Release
        } else {
            ImportStage::Parse
        };
        finished_job.source_version_id = version.as_ref().map(|value| value.source_version_id);
        finished_job.completed_units = success_count as i32;
        finished_job.failed_units = failed_count as i32;
        finished_job.lease_until = None;
        finished_job.errors = office
            .units
            .iter()
            .filter_map(|(id, result)| match result {
                OfficeUnitResult::Failure { code, .. } => {
                    Some(json!({"unit_id":id,"format":office.format,"code":code}))
                }
                _ => None,
            })
            .collect();
        finished_job
            .errors
            .sort_by_key(|entry| entry["unit_id"].as_u64());
        finished_job.stage_output_refs = vec![
            format!(
                "office:{:?}:units:{}/{}",
                office.format,
                success_count,
                manifest.unit_count()
            )
            .to_ascii_lowercase(),
        ];
        state
            .jobs
            .insert(finished_job.import_job_id, finished_job.clone());
        let release = if version.is_some() {
            Some(Self::make_release_locked(&mut state, scope)?)
        } else {
            None
        };
        let operation = state
            .operations
            .get_mut(&finished_job.operation_id)
            .expect("queued operation");
        operation.status = if version.is_some() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        };
        operation.updated_at = Utc::now();
        operation.result = Some(json!({
            "source_id":source.source_id,
            "source_version_id":version.as_ref().map(|v|v.source_version_id),
            "knowledge_release_id":release.as_ref().map(|r|r.knowledge_release_id),
            "completed_units":success_count,"failed_units":failed_count,
            "total_units":manifest.unit_count(),"format":office.format
        }));
        if version.is_none() {
            operation.error = Some(AppError::new(
                ErrorCode::CapabilityMissing,
                "no Office units could be parsed",
            ));
        }
        let acceptance = ImportAcceptance {
            client_item_id: format!("office:{}", finished_job.import_job_id),
            status,
            source: Some(source),
            source_version: version,
            import_job: Some(finished_job),
            operation: Some(operation.clone()),
            release,
            error: operation.error.clone(),
        };
        let office = state.office_jobs.get_mut(&lease.job_id).expect("checked");
        office.lease = None;
        office.acceptance = Some(acceptance.clone());
        Ok(acceptance)
    }

    async fn fail_office_parse(
        &self,
        scope: &TenantScope,
        lease: &OfficeParseLease,
        code: &str,
    ) -> Result<ImportAcceptance, AppError> {
        if !office_document_error_code(code) {
            return Err(AppError::invalid_request(
                "unknown Office document failure code",
            ));
        }
        let mut state = self.state.write().await;
        let (job, office) = Self::checked_office_lease(&state, scope, lease)?;
        if office.manifest.is_some() || !office.units.is_empty() {
            return Err(AppError::conflict("Office unit results already recorded"));
        }
        let mut job = job.clone();
        let format = office.format;
        job.status = ImportStatus::Failed;
        job.lease_until = None;
        job.failed_units = 1;
        job.errors = vec![json!({"code":code,"format":format})];
        let source = state.sources.get(&job.source_id).cloned();
        let operation = state
            .operations
            .get_mut(&job.operation_id)
            .expect("queued operation");
        operation.status = OperationStatus::Failed;
        operation.updated_at = Utc::now();
        operation.error = Some(
            AppError::invalid_request("Office document could not be parsed")
                .with_details(json!({"reason":code})),
        );
        let acceptance = ImportAcceptance {
            client_item_id: format!("office:{}", job.import_job_id),
            status: ImportStatus::Failed,
            source,
            source_version: None,
            import_job: Some(job.clone()),
            operation: Some(operation.clone()),
            release: None,
            error: operation.error.clone(),
        };
        state.jobs.insert(job.import_job_id, job);
        let office = state.office_jobs.get_mut(&lease.job_id).expect("checked");
        office.lease = None;
        office.acceptance = Some(acceptance.clone());
        Ok(acceptance)
    }

    async fn retry_office_parse(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
    ) -> Result<ImportJob, AppError> {
        let mut state = self.state.write().await;
        let prior = state
            .jobs
            .get(&job_id)
            .filter(|job| Self::in_scope(scope, *job))
            .cloned()
            .ok_or_else(|| AppError::not_found("Office parse job not found"))?;
        if !matches!(prior.status, ImportStatus::Failed | ImportStatus::Partial) {
            return Err(AppError::conflict("Office parse job is not retryable"));
        }
        let old_office = state
            .office_jobs
            .get(&job_id)
            .ok_or_else(|| AppError::not_found("Office parse job not found"))?
            .clone();
        if let Some(existing) = state
            .jobs
            .values()
            .find(|job| job.resumed_from == Some(job_id))
        {
            return Ok(existing.clone());
        }
        let source = state
            .sources
            .get(&prior.source_id)
            .ok_or_else(|| AppError::not_found("Office source not found"))?;
        if source.state != SourceState::Active {
            return Err(AppError::conflict("Office source was removed"));
        }
        if source.current_version_id != prior.source_version_id {
            return Err(AppError::conflict(
                "Office source has a newer current version",
            ));
        }
        let operation = Operation::queued("knowledge.import", scope.clone());
        let next = ImportJob {
            import_job_id: Uuid::new_v4(),
            operation_id: operation.id,
            status: ImportStatus::Queued,
            attempt: prior.attempt + 1,
            stage: ImportStage::Parse,
            source_version_id: None,
            lease_until: None,
            stage_output_refs: Vec::new(),
            completed_units: old_office
                .units
                .values()
                .filter(|unit| unit.is_success())
                .count() as i32,
            failed_units: 0,
            errors: Vec::new(),
            resumed_from: Some(job_id),
            ..prior
        };
        let mut office = old_office;
        office.created_at = operation.created_at;
        office.lease = None;
        office.acceptance = None;
        office.units.retain(|_, result| result.is_success());
        state.operations.insert(operation.id, operation);
        state.office_jobs.insert(next.import_job_id, office);
        state.jobs.insert(next.import_job_id, next.clone());
        Ok(next)
    }

    async fn office_parse_operation(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
    ) -> Result<Option<Operation>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        Ok(state
            .jobs
            .get(&job_id)
            .filter(|job| Self::in_scope(scope, *job) && state.office_jobs.contains_key(&job_id))
            .and_then(|job| state.operations.get(&job.operation_id))
            .cloned())
    }

    async fn finish_pdf_parse(
        &self,
        scope: &TenantScope,
        lease: &PdfParseLease,
    ) -> Result<ImportAcceptance, AppError> {
        let mut state = self.state.write().await;
        let (job, pdf) = Self::checked_pdf_lease(&state, scope, lease)?;
        let manifest = pdf
            .manifest
            .clone()
            .ok_or_else(|| AppError::conflict("PDF manifest not recorded"))?;
        if pdf.pages.len() != manifest.page_count as usize {
            return Err(AppError::conflict("PDF pages are incomplete"));
        }
        let job = job.clone();
        let pdf = pdf.clone();
        let mut source = state
            .sources
            .get(&job.source_id)
            .filter(|source| Self::in_scope(scope, *source))
            .cloned()
            .ok_or_else(|| AppError::not_found("PDF source not found"))?;
        if source.state != SourceState::Active {
            return Err(AppError::conflict("PDF source was removed"));
        }
        if source.current_version_id.is_some_and(|id| {
            state.versions.get(&id).is_some_and(|version| {
                version.representation == SourceVersionRepresentation::AuthoredText
            })
        }) {
            return Err(AppError::conflict(
                "PDF source has a newer authored version",
            ));
        }
        let object = state
            .stored_objects
            .get(&pdf.object_id)
            .ok_or_else(|| AppError::not_found("PDF object not found"))?
            .clone();
        let bytes = state
            .object_bytes
            .get(&pdf.object_id)
            .ok_or_else(|| AppError::not_found("PDF bytes not found"))?;
        if sha256_hex(bytes) != object.sha256
            || bytes.len() as u64 != object.actual_size
            || job.input_hash != object.sha256
        {
            return Err(AppError::conflict("PDF object changed during parsing"));
        }
        let mut successes = pdf
            .pages
            .values()
            .filter_map(|result| match result {
                PdfPageResult::Success { page, text } => Some(PdfPageText {
                    page: *page,
                    text: text.clone(),
                }),
                PdfPageResult::Failure { .. } => None,
            })
            .collect::<Vec<_>>();
        successes.sort_by_key(|page| page.page);
        let failed_units = manifest.page_count as usize - successes.len();
        let status = if successes.is_empty() {
            ImportStatus::Failed
        } else if failed_units != 0 {
            ImportStatus::Partial
        } else {
            ImportStatus::Succeeded
        };
        let mut version = None;
        let mut release = None;
        if !successes.is_empty() {
            let parent_id = source.current_version_id;
            let new_version = SourceVersion {
                source_version_id: Uuid::new_v4(),
                operator_id: scope.operator_id,
                tenant_id: scope.tenant_id,
                project_id: source.project_id,
                source_id: source.source_id,
                version: parent_id
                    .and_then(|id| state.versions.get(&id))
                    .map_or(1, |parent| parent.version + 1),
                representation: SourceVersionRepresentation::Original,
                object_id: Some(object.object_id),
                object_version: Some(object.object_version),
                content_sha256: object.sha256.clone(),
                captured_at: Utc::now(),
                original_url: None,
                parent_version_id: parent_id,
                parser_version: pdf.parser_profile.clone(),
                extraction_version: "pdf-text-v1".to_owned(),
                created_at: Utc::now(),
            };
            let mut chunks = Vec::new();
            for page in &successes {
                chunks.extend(pdf_page_chunks(
                    scope,
                    new_version.source_version_id,
                    page,
                    chunks.len() as i32,
                )?);
            }
            source.current_version_id = Some(new_version.source_version_id);
            state.sources.insert(source.source_id, source.clone());
            state.chunks.insert(new_version.source_version_id, chunks);
            state
                .versions
                .insert(new_version.source_version_id, new_version.clone());
            version = Some(new_version);
        }
        let mut finished_job = job;
        finished_job.status = status;
        finished_job.stage = if version.is_some() {
            ImportStage::Release
        } else {
            ImportStage::Parse
        };
        finished_job.source_version_id = version.as_ref().map(|value| value.source_version_id);
        finished_job.completed_units = successes.len() as i32;
        finished_job.failed_units = failed_units as i32;
        finished_job.lease_until = None;
        finished_job.errors = pdf
            .pages
            .iter()
            .filter_map(|(page, result)| match result {
                PdfPageResult::Failure { code, .. } => Some(json!({"page":page,"code":code})),
                PdfPageResult::Success { .. } => None,
            })
            .collect();
        finished_job
            .errors
            .sort_by_key(|error| error["page"].as_u64());
        finished_job.stage_output_refs = vec![format!(
            "pdf:pages:{}/{}",
            successes.len(),
            manifest.page_count
        )];
        state
            .jobs
            .insert(finished_job.import_job_id, finished_job.clone());
        if version.is_some() {
            release = Some(Self::make_release_locked(&mut state, scope)?);
        }
        let operation = state
            .operations
            .get_mut(&finished_job.operation_id)
            .expect("queued operation");
        operation.status = if version.is_some() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        };
        operation.updated_at = Utc::now();
        operation.result = Some(
            json!({"source_id": source.source_id, "source_version_id": version.as_ref().map(|v| v.source_version_id), "knowledge_release_id": release.as_ref().map(|r|r.knowledge_release_id), "completed_pages":successes.len(), "failed_pages":failed_units, "total_pages":manifest.page_count}),
        );
        if version.is_none() {
            operation.error = Some(AppError::new(
                ErrorCode::CapabilityMissing,
                "no PDF pages could be parsed",
            ));
        }
        let acceptance = ImportAcceptance {
            client_item_id: format!("pdf:{}", finished_job.import_job_id),
            status,
            source: Some(source),
            source_version: version,
            import_job: Some(finished_job.clone()),
            operation: Some(operation.clone()),
            release,
            error: operation.error.clone(),
        };
        let pdf = state.pdf_jobs.get_mut(&lease.job_id).expect("checked");
        pdf.lease = None;
        pdf.acceptance = Some(acceptance.clone());
        Ok(acceptance)
    }

    async fn fail_pdf_parse(
        &self,
        scope: &TenantScope,
        lease: &PdfParseLease,
        code: &str,
    ) -> Result<ImportAcceptance, AppError> {
        if !matches!(
            code,
            "invalid_pdf" | "encrypted_pdf" | "parse_failed" | "page_limit"
        ) {
            return Err(AppError::invalid_request(
                "unknown PDF document failure code",
            ));
        }
        let mut state = self.state.write().await;
        let (job, pdf) = Self::checked_pdf_lease(&state, scope, lease)?;
        if pdf.manifest.is_some() || !pdf.pages.is_empty() {
            return Err(AppError::conflict("PDF page results already recorded"));
        }
        let mut job = job.clone();
        job.status = ImportStatus::Failed;
        job.lease_until = None;
        job.failed_units = 1;
        job.errors = vec![json!({"code":code})];
        let source = state.sources.get(&job.source_id).cloned();
        let operation = state
            .operations
            .get_mut(&job.operation_id)
            .expect("queued operation");
        operation.status = OperationStatus::Failed;
        operation.updated_at = Utc::now();
        operation.error = Some(
            AppError::invalid_request("PDF document could not be parsed")
                .with_details(json!({"reason":code})),
        );
        let acceptance = ImportAcceptance {
            client_item_id: format!("pdf:{}", job.import_job_id),
            status: ImportStatus::Failed,
            source,
            source_version: None,
            import_job: Some(job.clone()),
            operation: Some(operation.clone()),
            release: None,
            error: operation.error.clone(),
        };
        state.jobs.insert(job.import_job_id, job);
        let pdf = state.pdf_jobs.get_mut(&lease.job_id).expect("checked");
        pdf.lease = None;
        pdf.acceptance = Some(acceptance.clone());
        Ok(acceptance)
    }

    async fn retry_pdf_parse(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
    ) -> Result<ImportJob, AppError> {
        let mut state = self.state.write().await;
        let prior = state
            .jobs
            .get(&job_id)
            .filter(|job| Self::in_scope(scope, *job))
            .cloned()
            .ok_or_else(|| AppError::not_found("PDF parse job not found"))?;
        if !matches!(prior.status, ImportStatus::Failed | ImportStatus::Partial) {
            return Err(AppError::conflict("PDF parse job is not retryable"));
        }
        let old_pdf = state
            .pdf_jobs
            .get(&job_id)
            .ok_or_else(|| AppError::not_found("PDF parse job not found"))?
            .clone();
        if let Some(existing) = state
            .jobs
            .values()
            .find(|job| job.resumed_from == Some(job_id))
        {
            return Ok(existing.clone());
        }
        let source = state
            .sources
            .get(&prior.source_id)
            .ok_or_else(|| AppError::not_found("PDF source not found"))?;
        if source.state != SourceState::Active {
            return Err(AppError::conflict("PDF source was removed"));
        }
        if source.current_version_id != prior.source_version_id {
            return Err(AppError::conflict("PDF source has a newer current version"));
        }
        let operation = Operation::queued("knowledge.import", scope.clone());
        let next = ImportJob {
            import_job_id: Uuid::new_v4(),
            operation_id: operation.id,
            status: ImportStatus::Queued,
            attempt: prior.attempt + 1,
            stage: ImportStage::Parse,
            source_version_id: None,
            lease_until: None,
            stage_output_refs: Vec::new(),
            completed_units: old_pdf
                .pages
                .values()
                .filter(|p| matches!(p, PdfPageResult::Success { .. }))
                .count() as i32,
            failed_units: 0,
            errors: Vec::new(),
            resumed_from: Some(job_id),
            ..prior
        };
        let mut pdf = old_pdf;
        pdf.created_at = operation.created_at;
        pdf.lease = None;
        pdf.acceptance = None;
        pdf.parent_job_id = Some(job_id);
        pdf.pages
            .retain(|_, result| matches!(result, PdfPageResult::Success { .. }));
        state.operations.insert(operation.id, operation);
        state.pdf_jobs.insert(next.import_job_id, pdf);
        state.jobs.insert(next.import_job_id, next.clone());
        Ok(next)
    }

    async fn pdf_parse_operation(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
    ) -> Result<Option<Operation>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        Ok(state
            .jobs
            .get(&job_id)
            .filter(|job| Self::in_scope(scope, *job) && state.pdf_jobs.contains_key(&job_id))
            .and_then(|job| state.operations.get(&job.operation_id))
            .cloned())
    }

    async fn office_parse_candidates(
        &self,
        after: Option<OfficeParseCursor>,
        limit: usize,
    ) -> Result<Vec<OfficeParseJobRef>, AppError> {
        if self.docx_parser_profile.is_none() && self.xlsx_parser_profile.is_none() {
            return Err(AppError::capability_missing(
                "Office parser is not configured",
            ));
        }
        let state = self.state.read().await;
        let mut candidates = state
            .office_jobs
            .iter()
            .filter_map(|(job_id, office)| {
                let job = state.jobs.get(job_id)?;
                let eligible = self.office_profile(office.format).is_some()
                    && (job.status == ImportStatus::Queued
                        || (job.status == ImportStatus::Running
                            && office
                                .lease
                                .as_ref()
                                .is_none_or(|lease| lease.expires_at <= Utc::now())));
                let after_cursor = after.is_none_or(|cursor| {
                    (office.created_at, *job_id) > (cursor.created_at, cursor.job_id)
                });
                (eligible && after_cursor).then_some(OfficeParseJobRef {
                    scope: TenantScope::new(job.operator_id, job.tenant_id, Some(job.project_id)),
                    job_id: *job_id,
                    created_at: office.created_at,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| (candidate.created_at, candidate.job_id));
        candidates.truncate(limit.min(1000));
        Ok(candidates)
    }

    async fn claim_office_parse(
        &self,
        scope: &TenantScope,
        job_id: Uuid,
        lease_id: Uuid,
        lease_seconds: i64,
    ) -> Result<Option<OfficeParseLease>, AppError> {
        Self::require_project(scope)?;
        if !(1..=3600).contains(&lease_seconds) || lease_id.is_nil() {
            return Err(AppError::invalid_request(
                "invalid Office lease duration or ID",
            ));
        }
        let mut state = self.state.write().await;
        let Some(job) = state
            .jobs
            .get(&job_id)
            .filter(|job| Self::in_scope(scope, *job))
        else {
            return Ok(None);
        };
        if !matches!(job.status, ImportStatus::Queued | ImportStatus::Running) {
            return Ok(None);
        }
        let Some(office) = state.office_jobs.get_mut(&job_id) else {
            return Ok(None);
        };
        if office
            .lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at > Utc::now())
        {
            return Ok(None);
        }
        office.fencing_token += 1;
        let lease = OfficeParseLease {
            job_id,
            lease_id,
            fencing_token: office.fencing_token,
            expires_at: Utc::now() + Duration::seconds(lease_seconds),
        };
        office.lease = Some(lease.clone());
        let job = state.jobs.get_mut(&job_id).expect("scoped job");
        job.status = ImportStatus::Running;
        job.lease_until = Some(lease.expires_at);
        let operation_id = job.operation_id;
        if let Some(operation) = state.operations.get_mut(&operation_id) {
            operation.status = OperationStatus::Running;
            operation.updated_at = Utc::now();
        }
        Ok(Some(lease))
    }

    async fn renew_office_parse(
        &self,
        scope: &TenantScope,
        lease: &OfficeParseLease,
        lease_seconds: i64,
    ) -> Result<Option<OfficeParseLease>, AppError> {
        if !(1..=3600).contains(&lease_seconds) {
            return Err(AppError::invalid_request("invalid Office lease duration"));
        }
        let mut state = self.state.write().await;
        if Self::checked_office_lease(&state, scope, lease).is_err() {
            return Ok(None);
        }
        let renewed = OfficeParseLease {
            expires_at: Utc::now() + Duration::seconds(lease_seconds),
            ..lease.clone()
        };
        state
            .office_jobs
            .get_mut(&lease.job_id)
            .expect("checked")
            .lease = Some(renewed.clone());
        state
            .jobs
            .get_mut(&lease.job_id)
            .expect("checked")
            .lease_until = Some(renewed.expires_at);
        Ok(Some(renewed))
    }

    async fn office_parse_input(
        &self,
        scope: &TenantScope,
        lease: &OfficeParseLease,
    ) -> Result<OfficeParseInput, AppError> {
        let state = self.state.read().await;
        let (job, office) = Self::checked_office_lease(&state, scope, lease)?;
        let object = state
            .stored_objects
            .get(&office.object_id)
            .filter(|object| Self::in_scope(scope, *object))
            .ok_or_else(|| AppError::not_found("Office object not found"))?;
        let bytes = state
            .object_bytes
            .get(&office.object_id)
            .ok_or_else(|| AppError::not_found("Office bytes not found"))?;
        if object.actual_size != bytes.len() as u64
            || sha256_hex(bytes) != object.sha256
            || job.input_hash != object.sha256
            || object.detected_media_type != office.format.media_type()
        {
            return Err(AppError::conflict(
                "Office original bytes or media type changed",
            ));
        }
        let mut successful_units = office
            .units
            .iter()
            .filter_map(|(id, result)| result.is_success().then_some(*id))
            .collect::<Vec<_>>();
        successful_units.sort_unstable();
        Ok(OfficeParseInput {
            bytes: bytes.clone(),
            input_sha256: object.sha256.clone(),
            media_type: object.detected_media_type.clone(),
            parser_profile: office.parser_profile.clone(),
            successful_units,
            manifest: office.manifest.clone(),
        })
    }

    async fn record_office_manifest(
        &self,
        scope: &TenantScope,
        lease: &OfficeParseLease,
        manifest: OfficeDocumentManifest,
    ) -> Result<(), AppError> {
        let mut state = self.state.write().await;
        let (job, office) = Self::checked_office_lease(&state, scope, lease)?;
        manifest.validate(&job.input_hash, &office.parser_profile)?;
        if manifest.format() != office.format
            || office.manifest.as_ref().is_some_and(|old| old != &manifest)
        {
            return Err(AppError::conflict(
                "Office format or manifest changed across retries",
            ));
        }
        state
            .office_jobs
            .get_mut(&lease.job_id)
            .expect("checked")
            .manifest = Some(manifest);
        Ok(())
    }

    async fn record_office_unit(
        &self,
        scope: &TenantScope,
        lease: &OfficeParseLease,
        result: OfficeUnitResult,
    ) -> Result<(), AppError> {
        let mut state = self.state.write().await;
        let (_, office) = Self::checked_office_lease(&state, scope, lease)?;
        let manifest = office
            .manifest
            .as_ref()
            .ok_or_else(|| AppError::conflict("Office manifest not recorded"))?;
        result.validate(manifest)?;
        if let Some(old) = office.units.get(&result.unit_id()) {
            if old == &result {
                return Ok(());
            }
            if old.is_success() {
                return Err(AppError::conflict(
                    "successful Office unit cannot be overwritten",
                ));
            }
        }
        let total = office
            .units
            .iter()
            .filter(|(id, unit)| **id != result.unit_id() && unit.is_success())
            .filter_map(|(_, unit)| serde_json::to_vec(unit).ok().map(|value| value.len()))
            .sum::<usize>()
            .saturating_add(if result.is_success() {
                serde_json::to_vec(&result)
                    .map_err(|_| AppError::invalid_request("invalid Office unit"))?
                    .len()
            } else {
                0
            });
        let empty = result.is_success()
            && office_unit_chunks(scope, Uuid::nil(), manifest, &result, 0)?.is_empty();
        let result = if empty {
            OfficeUnitResult::Failure {
                unit_id: result.unit_id(),
                code: "empty_text".to_owned(),
            }
        } else if result.is_success() && total > OFFICE_MAX_DOCUMENT_TEXT_BYTES {
            OfficeUnitResult::Failure {
                unit_id: result.unit_id(),
                code: "unit_limit".to_owned(),
            }
        } else {
            result
        };
        let office = state.office_jobs.get_mut(&lease.job_id).expect("checked");
        office.units.insert(result.unit_id(), result);
        let completed = office
            .units
            .values()
            .filter(|unit| unit.is_success())
            .count() as i32;
        let mut errors = office
            .units
            .iter()
            .filter_map(|(id, result)| match result {
                OfficeUnitResult::Failure { code, .. } => {
                    Some(json!({"unit_id":id,"format":office.format,"code":code}))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        errors.sort_by_key(|entry| entry["unit_id"].as_u64());
        let job = state.jobs.get_mut(&lease.job_id).expect("checked");
        job.completed_units = completed;
        job.failed_units = errors.len() as i32;
        job.errors = errors;
        Ok(())
    }

    async fn create_upload_session(
        &self,
        scope: &TenantScope,
        command: UploadSessionCommand,
    ) -> Result<UploadSession, AppError> {
        let project_id = Self::require_project(scope)?;
        Self::validate_upload(&command)?;
        let id = Uuid::new_v4();
        let session = UploadSession {
            upload_session_id: id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            revision: 1,
            filename: command.filename.trim().to_owned(),
            declared_media_type: normalize_media_type(&command.declared_media_type),
            expected_size: command.expected_size,
            expected_sha256: command.expected_sha256.to_ascii_lowercase(),
            purpose: command.purpose,
            state: UploadSessionState::Created,
            expires_at: Utc::now() + Duration::seconds(UPLOAD_SESSION_TTL_SECONDS),
            staging_object_ref: Some(format!("memory-staging/{id}")),
            committed_object_id: None,
            operation_id: None,
        };
        self.state
            .write()
            .await
            .upload_sessions
            .insert(id, session.clone());
        Ok(session)
    }

    async fn put_upload_content(
        &self,
        scope: &TenantScope,
        id: Uuid,
        content: Vec<u8>,
    ) -> Result<UploadSession, AppError> {
        Self::require_project(scope)?;
        let mut state = self.state.write().await;
        let updated = {
            let session = state
                .upload_sessions
                .get_mut(&id)
                .filter(|session| Self::in_scope(scope, *session))
                .ok_or_else(|| AppError::not_found("upload session not found"))?;
            if session.expires_at <= Utc::now() {
                session.state = UploadSessionState::Expired;
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "upload session has expired",
                ));
            }
            if !matches!(
                session.state,
                UploadSessionState::Created
                    | UploadSessionState::Uploading
                    | UploadSessionState::Uploaded
            ) {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "upload session cannot accept content",
                ));
            }
            if content.len() as u64 > session.expected_size
                || content.len() as u64 > MAX_UPLOAD_BYTES
            {
                return Err(AppError::invalid_request(
                    "uploaded content exceeds declared size",
                ));
            }
            session.state = UploadSessionState::Uploaded;
            session.revision += 1;
            session.clone()
        };
        state.upload_bytes.insert(id, content);
        Ok(updated)
    }

    async fn complete_upload(
        &self,
        scope: &TenantScope,
        id: Uuid,
        idempotency_key: &str,
    ) -> Result<ImportAcceptance, AppError> {
        Self::require_project(scope)?;
        if idempotency_key.trim().is_empty() {
            return Err(AppError::invalid_request(
                "Idempotency-Key must not be empty",
            ));
        }
        let mut state = self.state.write().await;
        let completion_key = (id, sha256_hex(idempotency_key.trim().as_bytes()));
        if let Some(value) = state.upload_completions.get(&completion_key) {
            return Ok(value.clone());
        }
        let session = state
            .upload_sessions
            .get(&id)
            .filter(|session| Self::in_scope(scope, *session))
            .cloned()
            .ok_or_else(|| AppError::not_found("upload session not found"))?;
        if session.expires_at <= Utc::now() {
            if let Some(session) = state.upload_sessions.get_mut(&id) {
                session.state = UploadSessionState::Expired;
            }
            return Err(AppError::new(
                ErrorCode::Conflict,
                "upload session has expired",
            ));
        }
        if session.state == UploadSessionState::Committed {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "upload session was completed with a different idempotency key",
            ));
        }
        let content = state.upload_bytes.get(&id).cloned().ok_or_else(|| {
            AppError::new(ErrorCode::Conflict, "upload content has not been received")
        })?;
        let actual_hash = sha256_hex(&content);
        if content.len() as u64 != session.expected_size || actual_hash != session.expected_sha256 {
            if let Some(session) = state.upload_sessions.get_mut(&id) {
                session.state = UploadSessionState::Failed;
                session.revision += 1;
            }
            return Err(AppError::invalid_request(
                "uploaded content size or sha256 does not match upload session",
            ));
        }
        let object = StoredObject {
            object_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: scope.project_id.expect("validated scope"),
            object_version: 1,
            backend: "memory".to_owned(),
            opaque_key: format!("memory-object/{}", Uuid::new_v4()),
            actual_size: content.len() as u64,
            detected_media_type: session.declared_media_type.clone(),
            sha256: actual_hash,
            state: StoredObjectState::Committed,
            created_at: Utc::now(),
        };
        let item = ImportItem {
            client_item_id: format!("upload:{id}"),
            kind: SourceKind::File,
            name: session.filename.clone(),
            purpose: session.purpose,
            text: None,
            url: None,
            object_id: Some(object.object_id),
            knowledge_release_id: None,
        };
        let mut acceptance = if is_supported_knowledge_media_type(&session.declared_media_type) {
            let text = String::from_utf8(content.clone())
                .map_err(|_| AppError::invalid_request("text upload bytes must be valid UTF-8"))?;
            Self::import_text_locked(&mut state, scope, &item, text, Some(object.clone()))?
        } else if session.declared_media_type == "application/pdf"
            && self.pdf_parser_profile.is_some()
        {
            state
                .stored_objects
                .insert(object.object_id, object.clone());
            self.queue_pdf_locked(&mut state, scope, &item, &object)
        } else if let Some(format) = self.queued_office_format(&session.declared_media_type) {
            state
                .stored_objects
                .insert(object.object_id, object.clone());
            self.queue_office_locked(&mut state, scope, &item, &object, format)
        } else {
            let result = Self::failed_missing_adapter(scope, &item, "document_parser");
            if let Some(source) = &result.source {
                state.sources.insert(source.source_id, source.clone());
            }
            if let Some(job) = &result.import_job {
                state.jobs.insert(job.import_job_id, job.clone());
            }
            state
                .stored_objects
                .insert(object.object_id, object.clone());
            result
        };
        state.object_bytes.insert(object.object_id, content);
        if let Some(operation) = acceptance.operation.as_mut() {
            operation.result = Some(json!({
                "upload_session_id": id,
                "object_id": object.object_id,
                "source_id": acceptance.source.as_ref().map(|source| source.source_id),
                "knowledge_release_id": acceptance.release.as_ref().map(|release| release.knowledge_release_id)
            }));
        }
        let operation_id = acceptance.operation.as_ref().map(|operation| operation.id);
        let session = state
            .upload_sessions
            .get_mut(&id)
            .expect("session remains present");
        session.state = UploadSessionState::Committed;
        session.revision += 1;
        session.committed_object_id = Some(object.object_id);
        session.operation_id = operation_id;
        state
            .upload_completions
            .insert(completion_key, acceptance.clone());
        Ok(acceptance)
    }

    async fn complete_attachment_upload(
        &self,
        scope: &TenantScope,
        id: Uuid,
        idempotency_key: &str,
    ) -> Result<(StoredObject, String), AppError> {
        Self::require_project(scope)?;
        if idempotency_key.trim().is_empty() {
            return Err(AppError::invalid_request(
                "Idempotency-Key must not be empty",
            ));
        }
        let mut state = self.state.write().await;
        let session = state
            .upload_sessions
            .get(&id)
            .filter(|session| Self::in_scope(scope, *session))
            .cloned()
            .ok_or_else(|| AppError::not_found("upload session not found"))?;
        let key_hash = sha256_hex(idempotency_key.trim().as_bytes());
        if session.state == UploadSessionState::Committed {
            if state
                .upload_completions
                .keys()
                .any(|(session_id, _)| *session_id == id)
                || session.operation_id.is_some()
            {
                return Err(AppError::conflict("upload session was already imported"));
            }
            if state.attachment_completions.get(&id) != Some(&key_hash) {
                return Err(AppError::conflict(
                    "upload session was completed with a different idempotency key",
                ));
            }
            let object = state
                .stored_objects
                .get(&session.committed_object_id.expect("committed object"))
                .cloned()
                .ok_or_else(|| AppError::not_found("attachment object not found"))?;
            return Ok((object, session.filename));
        }
        if session.expires_at <= Utc::now() {
            state
                .upload_sessions
                .get_mut(&id)
                .expect("session exists")
                .state = UploadSessionState::Expired;
            return Err(AppError::conflict("upload session has expired"));
        }
        if session.state != UploadSessionState::Uploaded {
            return Err(AppError::conflict(
                "upload session content is not ready to complete",
            ));
        }
        let content = state
            .upload_bytes
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::conflict("upload content has not been received"))?;
        let hash = sha256_hex(&content);
        if content.len() as u64 != session.expected_size || hash != session.expected_sha256 {
            let session = state.upload_sessions.get_mut(&id).expect("session exists");
            session.state = UploadSessionState::Failed;
            session.revision += 1;
            return Err(AppError::invalid_request(
                "uploaded content size or sha256 does not match upload session",
            ));
        }
        let object = StoredObject {
            object_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: scope.project_id.expect("validated project"),
            object_version: 1,
            backend: "memory".to_owned(),
            opaque_key: format!("memory-object/{}", Uuid::new_v4()),
            actual_size: session.expected_size,
            detected_media_type: session.declared_media_type.clone(),
            sha256: hash,
            state: StoredObjectState::Committed,
            created_at: Utc::now(),
        };
        state.object_bytes.insert(object.object_id, content);
        state
            .stored_objects
            .insert(object.object_id, object.clone());
        let updated = state.upload_sessions.get_mut(&id).expect("session exists");
        updated.state = UploadSessionState::Committed;
        updated.revision += 1;
        updated.committed_object_id = Some(object.object_id);
        state.attachment_completions.insert(id, key_hash);
        Ok((object, session.filename))
    }

    async fn get_attachment_object(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<(StoredObject, String)>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        let Some(object) = state
            .stored_objects
            .get(&id)
            .filter(|object| {
                object.state == StoredObjectState::Committed && Self::in_scope(scope, *object)
            })
            .cloned()
        else {
            return Ok(None);
        };
        let Some(session) = state.upload_sessions.values().find(|session| {
            session.committed_object_id == Some(id)
                && state
                    .attachment_completions
                    .contains_key(&session.upload_session_id)
                && Self::in_scope(scope, *session)
        }) else {
            return Ok(None);
        };
        Ok(Some((object, session.filename.clone())))
    }

    async fn get_attachment_object_bytes(
        &self,
        scope: &TenantScope,
        object_id: Uuid,
        object_version: i64,
        sha256: &str,
    ) -> Result<Option<AttachmentObjectBytes>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        let Some(object) = state.stored_objects.get(&object_id).filter(|object| {
            Self::in_scope(scope, *object)
                && object.state == StoredObjectState::Committed
                && object.object_version == object_version
                && object.sha256 == sha256
        }) else {
            return Ok(None);
        };
        if !state.upload_sessions.values().any(|session| {
            session.committed_object_id == Some(object_id)
                && session.state == UploadSessionState::Committed
                && state
                    .attachment_completions
                    .contains_key(&session.upload_session_id)
                && Self::in_scope(scope, session)
        }) {
            return Ok(None);
        }
        let bytes = state.object_bytes.get(&object_id).cloned().ok_or_else(|| {
            AppError::conflict("committed attachment bytes do not match object metadata")
        })?;
        AttachmentObjectBytes::verified(object.clone(), bytes).map(Some)
    }

    async fn import_batch(
        &self,
        scope: &TenantScope,
        items: Vec<ImportItem>,
    ) -> Result<ImportBatchAcceptance, AppError> {
        Self::require_project(scope)?;
        if items.is_empty() || items.len() > 100 {
            return Err(AppError::invalid_request(
                "imports must contain between 1 and 100 items",
            ));
        }
        let mut state = self.state.write().await;
        let mut output = Vec::with_capacity(items.len());
        for item in items {
            let item_key = item.client_item_id.trim().to_owned();
            if item_key.is_empty() || item_key.len() > 200 {
                output.push(ImportAcceptance {
                    client_item_id: item.client_item_id,
                    status: ImportStatus::Failed,
                    source: None,
                    source_version: None,
                    import_job: None,
                    operation: None,
                    release: None,
                    error: Some(AppError::invalid_request(
                        "client_item_id is required and at most 200 characters",
                    )),
                });
                continue;
            }
            let request_hash = import_item_hash(&item)?;
            let key = (scope.storage_key(), item_key.clone());
            if let Some((prior_hash, prior)) = state.import_items.get(&key) {
                if prior_hash != &request_hash {
                    output.push(ImportAcceptance {
                        client_item_id: item.client_item_id,
                        status: ImportStatus::Failed,
                        source: None,
                        source_version: None,
                        import_job: None,
                        operation: None,
                        release: None,
                        error: Some(AppError::conflict(
                            "client_item_id was already used with different input",
                        )),
                    });
                } else {
                    output.push(prior.clone());
                }
                continue;
            }
            let acceptance = match item.kind {
                SourceKind::Text => {
                    let text = item.text.clone().ok_or_else(|| {
                        AppError::invalid_request("text imports require the text field")
                    });
                    match text.and_then(|text| {
                        Self::import_text_locked(&mut state, scope, &item, text, None)
                    }) {
                        Ok(value) => value,
                        Err(error) => ImportAcceptance {
                            client_item_id: item.client_item_id.clone(),
                            status: ImportStatus::Failed,
                            source: None,
                            source_version: None,
                            import_job: None,
                            operation: None,
                            release: None,
                            error: Some(error),
                        },
                    }
                }
                SourceKind::Url => {
                    if !item.url.as_deref().is_some_and(|url| {
                        url.starts_with("https://") || url.starts_with("http://")
                    }) {
                        ImportAcceptance {
                            client_item_id: item.client_item_id.clone(),
                            status: ImportStatus::Failed,
                            source: None,
                            source_version: None,
                            import_job: None,
                            operation: None,
                            release: None,
                            error: Some(AppError::invalid_request(
                                "url imports require an http or https URL",
                            )),
                        }
                    } else {
                        Self::failed_missing_adapter(scope, &item, "url_fetch")
                    }
                }
                SourceKind::Object => {
                    let result = (|| {
                        let id = item.object_id.ok_or_else(|| {
                            AppError::invalid_request("object imports require object_id")
                        })?;
                        let object = state
                            .stored_objects
                            .get(&id)
                            .filter(|object| {
                                object.state == StoredObjectState::Committed
                                    && Self::in_scope(scope, *object)
                            })
                            .cloned()
                            .ok_or_else(|| {
                                AppError::not_found("committed attachment object not found")
                            })?;
                        let session = state
                            .upload_sessions
                            .values()
                            .find(|session| {
                                session.committed_object_id == Some(id)
                                    && state
                                        .attachment_completions
                                        .contains_key(&session.upload_session_id)
                                    && Self::in_scope(scope, *session)
                            })
                            .ok_or_else(|| {
                                AppError::not_found("committed attachment object not found")
                            })?;
                        if item.name != session.filename {
                            return Err(AppError::invalid_request(
                                "object name does not match uploaded filename",
                            ));
                        }
                        let bytes = state.object_bytes.get(&id).ok_or_else(|| {
                            AppError::not_found("committed attachment bytes not found")
                        })?;
                        if bytes.len() as u64 != object.actual_size
                            || sha256_hex(bytes) != object.sha256
                        {
                            return Err(AppError::conflict(
                                "committed attachment bytes do not match object metadata",
                            ));
                        }
                        if object.detected_media_type == "application/pdf"
                            && self.pdf_parser_profile.is_some()
                        {
                            return Ok(self.queue_pdf_locked(&mut state, scope, &item, &object));
                        }
                        if let Some(format) = self.queued_office_format(&object.detected_media_type)
                        {
                            return Ok(
                                self.queue_office_locked(&mut state, scope, &item, &object, format)
                            );
                        }
                        if !is_supported_knowledge_media_type(&object.detected_media_type) {
                            return Err(AppError::capability_missing(
                                "document_parser is not configured",
                            ));
                        }
                        let text = String::from_utf8(bytes.clone()).map_err(|_| {
                            AppError::invalid_request("text upload bytes must be valid UTF-8")
                        })?;
                        Self::import_text_locked(&mut state, scope, &item, text, Some(object))
                    })();
                    result.unwrap_or_else(|error| ImportAcceptance {
                        client_item_id: item.client_item_id.clone(),
                        status: ImportStatus::Failed,
                        source: None,
                        source_version: None,
                        import_job: None,
                        operation: None,
                        release: None,
                        error: Some(error),
                    })
                }
                SourceKind::KnowledgeCollection => ImportAcceptance {
                    client_item_id: item.client_item_id.clone(),
                    status: ImportStatus::Failed,
                    source: None,
                    source_version: None,
                    import_job: None,
                    operation: None,
                    release: None,
                    error: Some(AppError::new(
                        ErrorCode::CapabilityMissing,
                        "knowledge collection materialization is not implemented",
                    )),
                },
                SourceKind::File | SourceKind::Manual => ImportAcceptance {
                    client_item_id: item.client_item_id.clone(),
                    status: ImportStatus::Failed,
                    source: None,
                    source_version: None,
                    import_job: None,
                    operation: None,
                    release: None,
                    error: Some(AppError::invalid_request(
                        "file imports require an upload session; manual imports require text",
                    )),
                },
            };
            if let Some(source) = &acceptance.source
                && acceptance.source_version.is_none()
            {
                state.sources.insert(source.source_id, source.clone());
            }
            if let Some(job) = &acceptance.import_job
                && acceptance.source_version.is_none()
            {
                state.jobs.insert(job.import_job_id, job.clone());
            }
            state
                .import_items
                .insert(key, (request_hash, acceptance.clone()));
            output.push(acceptance);
        }
        Ok(ImportBatchAcceptance { items: output })
    }

    async fn list_sources(&self, scope: &TenantScope) -> Result<Vec<Source>, AppError> {
        Self::require_project(scope)?;
        let mut sources = self
            .state
            .read()
            .await
            .sources
            .values()
            .filter(|source| Self::in_scope(scope, *source))
            .cloned()
            .collect::<Vec<_>>();
        sources.sort_by_key(|source| source.source_id);
        Ok(sources)
    }

    async fn get_source(&self, scope: &TenantScope, id: Uuid) -> Result<Option<Source>, AppError> {
        Self::require_project(scope)?;
        Ok(self
            .state
            .read()
            .await
            .sources
            .get(&id)
            .filter(|source| Self::in_scope(scope, *source))
            .cloned())
    }

    async fn get_source_detail(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SourceDetail>, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        let Some(source) = state
            .sources
            .get(&id)
            .filter(|source| Self::in_scope(scope, *source))
            .cloned()
        else {
            return Ok(None);
        };
        let versions = state
            .versions
            .values()
            .filter(|version| version.source_id == id && Self::in_scope(scope, *version))
            .cloned()
            .collect::<Vec<_>>();
        let version_ids = versions
            .iter()
            .map(|version| version.source_version_id)
            .collect::<std::collections::HashSet<_>>();
        let mut chunks = state
            .chunks
            .values()
            .flatten()
            .filter(|chunk| version_ids.contains(&chunk.source_version_id))
            .cloned()
            .collect::<Vec<_>>();
        chunks.sort_by_key(|chunk| (chunk.source_version_id, chunk.ordinal));
        let chunk_ids = chunks
            .iter()
            .map(|chunk| chunk.chunk_id)
            .collect::<std::collections::HashSet<_>>();
        let facts = state
            .facts
            .values()
            .filter(|fact| {
                Self::in_scope(scope, *fact)
                    && fact
                        .evidence_refs
                        .iter()
                        .any(|evidence| chunk_ids.contains(&evidence.chunk_id.unwrap_or_default()))
            })
            .cloned()
            .collect();
        let import_jobs = state
            .jobs
            .values()
            .filter(|job| job.source_id == id && Self::in_scope(scope, *job))
            .cloned()
            .collect();
        Ok(Some(SourceDetail {
            source,
            versions,
            chunks,
            facts,
            import_jobs,
        }))
    }

    async fn get_source_version(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<SourceVersion>, AppError> {
        Self::require_project(scope)?;
        Ok(self
            .state
            .read()
            .await
            .versions
            .get(&version_id)
            .filter(|version| version.source_id == source_id && Self::in_scope(scope, *version))
            .cloned())
    }

    async fn list_products(&self, scope: &TenantScope) -> Result<Vec<Product>, AppError> {
        Self::require_project(scope)?;
        Ok(self
            .state
            .read()
            .await
            .products
            .values()
            .filter(|product| Self::in_scope(scope, *product))
            .cloned()
            .collect())
    }

    async fn list_facts(&self, scope: &TenantScope) -> Result<Vec<Fact>, AppError> {
        Self::require_project(scope)?;
        Ok(self
            .state
            .read()
            .await
            .facts
            .values()
            .filter(|fact| Self::in_scope(scope, *fact))
            .cloned()
            .collect())
    }

    async fn current_release(
        &self,
        scope: &TenantScope,
    ) -> Result<CurrentKnowledgeRelease, AppError> {
        let project_id = Self::require_project(scope)?;
        let state = self.state.read().await;
        let release_id = state.current_release.get(&scope.storage_key()).copied();
        Ok(CurrentKnowledgeRelease {
            project_id,
            knowledge_release_id: release_id,
            sequence: release_id
                .and_then(|id| state.releases.get(&id).map(|release| release.sequence)),
        })
    }

    async fn get_release(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<KnowledgeRelease>, AppError> {
        Self::require_project(scope)?;
        Ok(self
            .state
            .read()
            .await
            .releases
            .get(&id)
            .filter(|release| Self::in_scope(scope, *release))
            .cloned())
    }

    async fn get_document_manifest(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<DocumentManifest>, AppError> {
        Self::require_project(scope)?;
        Ok(self
            .state
            .read()
            .await
            .document_manifests
            .get(&id)
            .filter(|manifest| {
                manifest.operator_id == scope.operator_id
                    && manifest.tenant_id == scope.tenant_id
                    && Some(manifest.project_id) == scope.project_id
            })
            .cloned())
    }

    async fn plan_document_manifest(
        &self,
        scope: &TenantScope,
        request: DocumentManifestPlanRequest,
        document_scope: DocumentScope,
    ) -> Result<DocumentManifest, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        let release = state
            .releases
            .get(&request.knowledge_release_id)
            .filter(|release| Self::in_scope(scope, *release))
            .cloned()
            .ok_or_else(|| AppError::not_found("knowledge release not found"))?;
        let mut public_refs = release
            .source_version_refs
            .iter()
            .filter_map(|id| {
                state.versions.get(id).and_then(|version| {
                    state
                        .sources
                        .get(&version.source_id)
                        .filter(|source| source.purpose == KnowledgePurpose::Public)
                        .map(|_| *id)
                })
            })
            .collect::<Vec<_>>();
        public_refs.sort_unstable();
        drop(state);
        let manifest = plan_document_manifest(
            scope,
            &release,
            request.manifest_id,
            &document_scope,
            &public_refs,
        )?;
        let mut state = self.state.write().await;
        if let Some(existing) = state.document_manifests.get(&request.manifest_id) {
            if existing.knowledge_release_id != request.knowledge_release_id
                || existing.scope_hash != manifest.scope_hash
                || existing.planner_version != manifest.planner_version
            {
                return Err(AppError::conflict(
                    "document manifest ID was already used with different planning input",
                ));
            }
            return Ok(existing.clone());
        }
        state
            .document_manifests
            .insert(manifest.manifest_id, manifest.clone());
        Ok(manifest)
    }

    async fn search(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, AppError> {
        Self::require_project(scope)?;
        if request.query.trim().is_empty() {
            return Err(AppError::invalid_request("query must not be empty"));
        }
        let limit = request.limit.clamp(1, 50) as usize;
        let state = self.state.read().await;
        let release_id = request
            .knowledge_release_id
            .or_else(|| state.current_release.get(&scope.storage_key()).copied());
        let Some(release_id) = release_id else {
            return Ok(KnowledgeSearchResult {
                knowledge_release_id: None,
                evidence: Vec::new(),
                capability_missing: None,
            });
        };
        let release = state
            .releases
            .get(&release_id)
            .filter(|release| Self::in_scope(scope, *release))
            .ok_or_else(|| AppError::not_found("knowledge release not found"))?;
        let needle = request.query.to_lowercase();
        let mut evidence = release
            .source_version_refs
            .iter()
            .filter_map(|version_id| state.versions.get(version_id))
            .filter_map(|version| {
                state.sources.get(&version.source_id).and_then(|source| {
                    (source.state == SourceState::Active
                        && (request.purpose == KnowledgePurpose::Internal
                            || source.purpose == KnowledgePurpose::Public))
                        .then_some((version, source))
                })
            })
            .flat_map(|(version, source)| {
                let needle = needle.clone();
                state
                    .chunks
                    .get(&version.source_version_id)
                    .into_iter()
                    .flatten()
                    .filter(|chunk| chunk.extraction_method != "deterministic_csv_evidence_v1")
                    .filter(move |chunk| chunk.text.to_lowercase().contains(&needle))
                    .map(move |chunk| KnowledgeEvidence {
                        source_id: source.source_id,
                        source_version_id: version.source_version_id,
                        chunk_id: chunk.chunk_id,
                        source_name: source.name.clone(),
                        purpose: source.purpose,
                        locator: chunk.locator.clone(),
                        text: chunk.text.clone(),
                        quote: chunk.text.clone(),
                    })
            })
            .collect::<Vec<_>>();
        evidence.sort_by_key(|chunk| (chunk.source_version_id, chunk.chunk_id));
        evidence.truncate(limit);
        Ok(KnowledgeSearchResult {
            knowledge_release_id: Some(release_id),
            evidence,
            capability_missing: None,
        })
    }

    async fn ask(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeAskResult, AppError> {
        let search = self.search(scope, request).await?;
        let answer = if search.evidence.is_empty() {
            "当前资料没有这项信息".to_owned()
        } else {
            search
                .evidence
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        Ok(KnowledgeAskResult {
            knowledge_release_id: search.knowledge_release_id,
            mode: "evidence_only".to_owned(),
            answer_status: if search.evidence.is_empty() {
                KnowledgeAnswerStatus::InsufficientEvidence
            } else {
                KnowledgeAnswerStatus::Answered
            },
            answer,
            evidence: search.evidence,
            capability_missing: Some("llm_answering".to_owned()),
        })
    }

    async fn overview(&self, scope: &TenantScope) -> Result<KnowledgeOverview, AppError> {
        Self::require_project(scope)?;
        let state = self.state.read().await;
        Ok(KnowledgeOverview {
            source_count: state
                .sources
                .values()
                .filter(|source| Self::in_scope(scope, *source))
                .count() as u64,
            fact_count: state
                .facts
                .values()
                .filter(|fact| Self::in_scope(scope, *fact))
                .count() as u64,
            importing_count: state
                .jobs
                .values()
                .filter(|job| Self::in_scope(scope, *job))
                .filter(|job| matches!(job.status, ImportStatus::Queued | ImportStatus::Running))
                .count() as u64,
            current_release_id: state.current_release.get(&scope.storage_key()).copied(),
        })
    }
}

/// Parse without changing source bytes, interpreting formulas, or inferring cell types.
pub fn parsed_knowledge_chunks(
    scope: &TenantScope,
    source_version_id: Uuid,
    text: &str,
    media_type: &str,
) -> Result<Vec<Chunk>, AppError> {
    if normalize_media_type(media_type) == "text/csv" {
        crate::knowledge_csv::csv_chunks(scope, source_version_id, text)
    } else if is_text_media_type(media_type) {
        Ok(deterministic_chunks(scope, source_version_id, text))
    } else {
        Err(AppError::capability_missing(
            "document_parser is not configured",
        ))
    }
}

pub fn knowledge_parser_version(media_type: &str) -> &'static str {
    if normalize_media_type(media_type) == "text/csv" {
        "deterministic-csv-v2"
    } else if is_text_media_type(media_type) {
        "deterministic-text-v1"
    } else {
        "unsupported"
    }
}

pub fn is_supported_knowledge_media_type(media_type: &str) -> bool {
    is_text_media_type(media_type) || normalize_media_type(media_type) == "text/csv"
}

pub fn deterministic_chunks(
    scope: &TenantScope,
    source_version_id: Uuid,
    text: &str,
) -> Vec<Chunk> {
    let project_id = scope.project_id.expect("validated project scope");
    let mut chunks = Vec::new();
    let mut paragraph_start_line = 1_u32;
    let mut paragraph_start_char = 0_u32;
    let mut current = String::new();
    let mut current_end_line = 0_u32;
    let mut char_offset = 0_u32;
    let flush = |chunks: &mut Vec<Chunk>,
                 current: &mut String,
                 start_line: u32,
                 end_line: u32,
                 start_char: u32,
                 end_char: u32| {
        let text = current.trim().to_owned();
        current.clear();
        if text.is_empty() {
            return;
        }
        let ordinal = chunks.len() as i32;
        chunks.push(Chunk {
            chunk_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            source_version_id,
            ordinal,
            kind: ChunkKind::Paragraph,
            text_hash: sha256_hex(text.as_bytes()),
            text,
            locator: ChunkLocator::Text {
                start_line,
                end_line,
                start_char,
                end_char,
            },
            product_ids: Vec::new(),
            market: None,
            language: None,
            extraction_method: "deterministic_paragraph_v1".to_owned(),
            confidence: 1.0,
        });
    };
    for (index, line) in text.lines().enumerate() {
        let line_number = (index + 1) as u32;
        if line.trim().is_empty() {
            flush(
                &mut chunks,
                &mut current,
                paragraph_start_line,
                current_end_line,
                paragraph_start_char,
                char_offset,
            );
            paragraph_start_line = line_number + 1;
            paragraph_start_char = char_offset + line.chars().count() as u32 + 1;
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
            current_end_line = line_number;
        }
        char_offset += line.chars().count() as u32 + 1;
    }
    flush(
        &mut chunks,
        &mut current,
        paragraph_start_line,
        current_end_line.max(paragraph_start_line),
        paragraph_start_char,
        char_offset,
    );
    chunks
}

fn default_search_purpose() -> KnowledgePurpose {
    KnowledgePurpose::Public
}

fn default_search_limit() -> u32 {
    10
}

fn normalize_media_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase()
}

fn is_text_media_type(value: &str) -> bool {
    matches!(
        normalize_media_type(value).as_str(),
        "text/plain" | "text/markdown" | "text/x-markdown"
    )
}

fn validate_sha256(value: &str) -> Result<(), AppError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::invalid_request(
            "expected_sha256 must be a 64-character hexadecimal SHA-256",
        ));
    }
    Ok(())
}

pub fn sha256_hex(value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(value);
    hex::encode(digest.finalize())
}

fn import_item_hash(item: &ImportItem) -> Result<String, AppError> {
    serde_json::to_vec(item)
        .map(|value| sha256_hex(&value))
        .map_err(|error| {
            AppError::new(
                ErrorCode::Internal,
                format!("cannot hash import item: {error}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        DocumentManifestItemState, DocumentManifestPlanRequest, ImportItem, KnowledgePurpose,
        KnowledgeRepository, KnowledgeSearchRequest, MemoryKnowledgeRepository, SourceKind,
        UploadSessionCommand, sha256_hex,
    };
    use crate::{
        DocumentScope, ImportStatus, PDF_PARSE_SCHEMA_VERSION, PdfDocumentManifest, PdfPageResult,
        PdfPageText, TenantScope,
    };
    use uuid::Uuid;

    #[tokio::test]
    async fn frozen_public_evidence_guard_blocks_source_writer_until_content_commit() {
        use std::sync::Arc;
        use tokio::time::{Duration, sleep, timeout};

        let repository = Arc::new(MemoryKnowledgeRepository::default());
        let scope = scope();
        let imported = repository
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "public-evidence".into(),
                    kind: SourceKind::Text,
                    name: "Public source".into(),
                    purpose: KnowledgePurpose::Public,
                    text: Some("Grounded public sentence".into()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap();
        let accepted = &imported.items[0];
        let release_id = accepted.release.as_ref().unwrap().knowledge_release_id;
        let version_id = accepted.source_version.as_ref().unwrap().source_version_id;
        let source_id = accepted.source.as_ref().unwrap().source_id;
        let manifest = repository
            .plan_document_manifest(
                &scope,
                DocumentManifestPlanRequest {
                    manifest_id: Uuid::new_v4(),
                    knowledge_release_id: release_id,
                },
                DocumentScope {
                    content_types: vec!["faq".into()],
                    markets: vec!["global".into()],
                    languages: vec!["en".into()],
                    ..DocumentScope::default()
                },
            )
            .await
            .unwrap();
        let item = manifest
            .items
            .iter()
            .find(|item| item.state == DocumentManifestItemState::Planned)
            .unwrap();
        let detail = repository
            .get_source_detail(&scope, source_id)
            .await
            .unwrap()
            .unwrap();
        let chunk = detail
            .chunks
            .iter()
            .find(|chunk| chunk.source_version_id == version_id)
            .unwrap();
        let input = super::ContentPublicEligibility {
            document_manifest_id: manifest.manifest_id,
            document_manifest_item_id: item.document_manifest_item_id,
            source_version_ids: item.source_version_refs.clone(),
            evidence: vec![crate::ContentEvidence {
                reference: super::EvidenceRef {
                    source_version_id: version_id,
                    chunk_id: Some(chunk.chunk_id),
                    locator: chunk.locator.clone(),
                },
                exact_quote: chunk.text.clone(),
            }],
        };
        let held = repository
            .hold_content_evidence(&scope, std::slice::from_ref(&input))
            .await
            .unwrap();
        assert_eq!(held.mode(), crate::ContentGuardMode::Held);
        let writer_repo = repository.clone();
        let writer = tokio::spawn(async move {
            let mut state = writer_repo.state.write().await;
            state.sources.get_mut(&source_id).unwrap().purpose = KnowledgePurpose::Internal;
        });
        sleep(Duration::from_millis(30)).await;
        assert!(
            !writer.is_finished(),
            "source writer must wait for content commit"
        );
        drop(held);
        timeout(Duration::from_secs(2), writer)
            .await
            .unwrap()
            .unwrap();
        assert!(
            repository
                .hold_content_evidence(&scope, std::slice::from_ref(&input))
                .await
                .is_err()
        );
        let mut invalid = input;
        invalid.source_version_ids = vec![Uuid::new_v4()];
        assert!(
            repository
                .hold_content_evidence(&scope, &[invalid])
                .await
                .is_err()
        );
    }

    fn scope() -> TenantScope {
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        )
    }

    async fn queued_pdf(
        repository: &MemoryKnowledgeRepository,
        scope: &TenantScope,
    ) -> super::ImportAcceptance {
        let bytes = b"%PDF-1.7\nverified original bytes".to_vec();
        let session = repository
            .create_upload_session(
                scope,
                UploadSessionCommand {
                    filename: "example.pdf".to_owned(),
                    declared_media_type: "application/pdf".to_owned(),
                    expected_size: bytes.len() as u64,
                    expected_sha256: sha256_hex(&bytes),
                    purpose: KnowledgePurpose::Public,
                },
            )
            .await
            .unwrap();
        repository
            .put_upload_content(scope, session.upload_session_id, bytes.clone())
            .await
            .unwrap();
        let accepted = repository
            .complete_upload(scope, session.upload_session_id, "once")
            .await
            .unwrap();
        assert_eq!(accepted.status, ImportStatus::Queued);
        assert!(accepted.source_version.is_none());
        assert!(accepted.release.is_none());
        assert_eq!(
            accepted.import_job.as_ref().unwrap().input_hash,
            sha256_hex(&bytes)
        );
        assert_eq!(
            repository
                .complete_upload(scope, session.upload_session_id, "once")
                .await
                .unwrap(),
            accepted
        );
        accepted
    }

    #[tokio::test]
    async fn pdf_queue_partial_release_retry_keeps_old_evidence_and_scope_fence() {
        let repository = MemoryKnowledgeRepository::with_pdf_parser_profile(
            "tika-3.2.3_pdfbox-3.0.5_text-v1".to_owned(),
        );
        let scope = scope();
        assert!(repository.capabilities(&scope).await.unwrap().pdf_parser);
        let accepted = queued_pdf(&repository, &scope).await;
        let job_id = accepted.import_job.unwrap().import_job_id;
        let candidates = repository.pdf_parse_candidates(None, 10).await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].job_id, job_id);
        assert!(
            repository
                .pdf_parse_candidates(
                    Some(crate::PdfParseCursor {
                        created_at: candidates[0].created_at,
                        job_id
                    }),
                    10
                )
                .await
                .unwrap()
                .is_empty()
        );
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert!(
            repository
                .claim_pdf_parse(&other, job_id, Uuid::new_v4(), 30)
                .await
                .unwrap()
                .is_none()
        );
        let lease = repository
            .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert!(
            repository
                .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 30)
                .await
                .unwrap()
                .is_none()
        );
        let input = repository.pdf_parse_input(&scope, &lease).await.unwrap();
        assert_eq!(sha256_hex(&input.bytes), input.input_sha256);
        assert_eq!(input.parser_profile, "tika-3.2.3_pdfbox-3.0.5_text-v1");
        let manifest = PdfDocumentManifest {
            schema_version: PDF_PARSE_SCHEMA_VERSION.to_owned(),
            input_sha256: input.input_sha256,
            parser_version: input.parser_profile,
            page_count: 2,
        };
        repository
            .record_pdf_manifest(&scope, &lease, manifest.clone())
            .await
            .unwrap();
        repository
            .record_pdf_page(
                &scope,
                &lease,
                PdfPageResult::Success {
                    page: 1,
                    text: "Alpha product is available.".to_owned(),
                },
            )
            .await
            .unwrap();
        repository
            .record_pdf_page(
                &scope,
                &lease,
                PdfPageResult::Failure {
                    page: 2,
                    code: "ocr_required".to_owned(),
                },
            )
            .await
            .unwrap();
        let progress = repository
            .get_source_detail(&scope, accepted.source.as_ref().unwrap().source_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(progress.import_jobs[0].status, ImportStatus::Running);
        assert_eq!(progress.import_jobs[0].completed_units, 1);
        assert_eq!(progress.import_jobs[0].failed_units, 1);
        let first = repository.finish_pdf_parse(&scope, &lease).await.unwrap();
        assert_eq!(first.status, ImportStatus::Partial);
        assert_eq!(first.import_job.as_ref().unwrap().completed_units, 1);
        assert_eq!(first.import_job.as_ref().unwrap().failed_units, 1);
        let old_version = first.source_version.unwrap();
        assert_eq!(old_version.content_sha256, manifest.input_sha256);
        let old_release = first.release.unwrap();
        assert_eq!(old_release.coverage.chunk_count, 1);
        assert!(!old_release.coverage.blocked_reasons.is_empty());
        assert!(repository.pdf_parse_input(&scope, &lease).await.is_err());
        let retry = repository.retry_pdf_parse(&scope, job_id).await.unwrap();
        assert_eq!(
            retry.import_job_id,
            repository
                .retry_pdf_parse(&scope, job_id)
                .await
                .unwrap()
                .import_job_id
        );
        let new_lease = repository
            .claim_pdf_parse(&scope, retry.import_job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        let retry_input = repository
            .pdf_parse_input(&scope, &new_lease)
            .await
            .unwrap();
        assert_eq!(retry_input.successful_pages, vec![1]);
        assert_eq!(retry_input.manifest, Some(manifest.clone()));
        let stale = crate::PdfParseLease {
            fencing_token: new_lease.fencing_token - 1,
            ..new_lease.clone()
        };
        assert!(
            repository
                .record_pdf_page(
                    &scope,
                    &stale,
                    PdfPageResult::Success {
                        page: 2,
                        text: "stale".to_owned()
                    }
                )
                .await
                .is_err()
        );
        repository
            .record_pdf_manifest(&scope, &new_lease, manifest)
            .await
            .unwrap();
        repository
            .record_pdf_page(
                &scope,
                &new_lease,
                PdfPageResult::Success {
                    page: 2,
                    text: "Beta product is available.".to_owned(),
                },
            )
            .await
            .unwrap();
        let second = repository
            .finish_pdf_parse(&scope, &new_lease)
            .await
            .unwrap();
        assert_eq!(second.status, ImportStatus::Succeeded);
        assert_eq!(
            second
                .release
                .as_ref()
                .unwrap()
                .coverage
                .failed_source_count,
            0
        );
        assert!(
            second
                .release
                .as_ref()
                .unwrap()
                .coverage
                .blocked_reasons
                .is_empty()
        );
        let new_version = second.source_version.unwrap();
        assert_eq!(
            new_version.parent_version_id,
            Some(old_version.source_version_id)
        );
        assert_ne!(new_version.source_version_id, old_version.source_version_id);
        let prior = repository
            .search(
                &scope,
                super::KnowledgeSearchRequest {
                    query: "Alpha".to_owned(),
                    knowledge_release_id: Some(old_release.knowledge_release_id),
                    purpose: KnowledgePurpose::Public,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(prior.evidence.len(), 1);
        assert_eq!(
            prior.evidence[0].source_version_id,
            old_version.source_version_id
        );
        let old_search = repository
            .search(
                &scope,
                super::KnowledgeSearchRequest {
                    query: "Beta".to_owned(),
                    knowledge_release_id: Some(old_release.knowledge_release_id),
                    purpose: KnowledgePurpose::Public,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert!(old_search.evidence.is_empty());
        let new_search = repository
            .search(
                &scope,
                super::KnowledgeSearchRequest {
                    query: "Beta".to_owned(),
                    knowledge_release_id: None,
                    purpose: KnowledgePurpose::Public,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(new_search.evidence.len(), 1);
        assert!(matches!(
            new_search.evidence[0].locator,
            super::ChunkLocator::Pdf {
                page: 2,
                start_char: Some(0),
                ..
            }
        ));
        assert_eq!(
            repository
                .get_source_detail(&scope, first.source.unwrap().source_id)
                .await
                .unwrap()
                .unwrap()
                .versions
                .len(),
            2
        );
        assert!(
            repository
                .get_source_detail(&other, second.source.unwrap().source_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn pdf_chunk_ranges_preserve_utf8_and_bound_large_pages() {
        let scope = scope();
        let text = "海".repeat(2001);
        let chunks = crate::pdf_page_chunks(
            &scope,
            Uuid::new_v4(),
            &PdfPageText {
                page: 4,
                text: text.clone(),
            },
            5,
        )
        .unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].ordinal, 5);
        assert_eq!(chunks[1].ordinal, 6);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>(),
            text
        );
        assert!(matches!(
            chunks[1].locator,
            super::ChunkLocator::Pdf {
                page: 4,
                start_char: Some(2000),
                end_char: Some(2001),
                ..
            }
        ));
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.text_hash == sha256_hex(chunk.text.as_bytes()))
        );
    }

    #[tokio::test]
    async fn attachment_pdf_is_queued_only_on_explicit_import_and_zero_success_never_releases() {
        let repo = MemoryKnowledgeRepository::with_pdf_parser_profile("test-parser-v1".to_owned());
        let scope = scope();
        let bytes = b"%PDF-1.4\nraw".to_vec();
        let upload = repo
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "attachment.pdf".to_owned(),
                    declared_media_type: "application/pdf".to_owned(),
                    expected_size: bytes.len() as u64,
                    expected_sha256: sha256_hex(&bytes),
                    purpose: KnowledgePurpose::Internal,
                },
            )
            .await
            .unwrap();
        repo.put_upload_content(&scope, upload.upload_session_id, bytes)
            .await
            .unwrap();
        let (object, filename) = repo
            .complete_attachment_upload(&scope, upload.upload_session_id, "attachment")
            .await
            .unwrap();
        assert!(
            repo.pdf_parse_candidates(None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let item = ImportItem {
            client_item_id: "explicit-object-import".to_owned(),
            kind: SourceKind::Object,
            name: filename,
            purpose: KnowledgePurpose::Internal,
            text: None,
            url: None,
            object_id: Some(object.object_id),
            knowledge_release_id: None,
        };
        let accepted = repo
            .import_batch(&scope, vec![item])
            .await
            .unwrap()
            .items
            .remove(0);
        assert_eq!(accepted.status, ImportStatus::Queued);
        assert!(accepted.release.is_none());
        let job_id = accepted.import_job.unwrap().import_job_id;
        let lease = repo
            .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        repo.record_pdf_manifest(
            &scope,
            &lease,
            PdfDocumentManifest {
                schema_version: PDF_PARSE_SCHEMA_VERSION.to_owned(),
                input_sha256: object.sha256.clone(),
                parser_version: "test-parser-v1".to_owned(),
                page_count: 1,
            },
        )
        .await
        .unwrap();
        repo.record_pdf_page(
            &scope,
            &lease,
            PdfPageResult::Failure {
                page: 1,
                code: "ocr_required".to_owned(),
            },
        )
        .await
        .unwrap();
        let finished = repo.finish_pdf_parse(&scope, &lease).await.unwrap();
        assert_eq!(finished.status, ImportStatus::Failed);
        assert!(finished.source_version.is_none());
        assert!(finished.release.is_none());
        assert!(
            repo.current_release(&scope)
                .await
                .unwrap()
                .knowledge_release_id
                .is_none()
        );
        assert_eq!(
            repo.retry_pdf_parse(&scope, job_id).await.unwrap().status,
            ImportStatus::Queued
        );
    }

    #[tokio::test]
    async fn pdf_inspect_failure_is_terminal_without_manifest() {
        let repo = MemoryKnowledgeRepository::with_pdf_parser_profile("test-parser-v1".to_owned());
        let scope = scope();
        let job_id = queued_pdf(&repo, &scope)
            .await
            .import_job
            .unwrap()
            .import_job_id;
        let lease = repo
            .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert!(
            repo.fail_pdf_parse(&scope, &lease, "unknown_code")
                .await
                .is_err()
        );
        let result = repo
            .fail_pdf_parse(&scope, &lease, "invalid_pdf")
            .await
            .unwrap();
        assert_eq!(result.status, ImportStatus::Failed);
        assert!(
            repo.pdf_parse_candidates(None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.current_release(&scope)
                .await
                .unwrap()
                .knowledge_release_id
                .is_none()
        );
        assert!(repo.retry_pdf_parse(&scope, job_id).await.is_ok());
    }

    #[tokio::test]
    async fn expired_pdf_worker_cannot_write_after_new_fenced_claim() {
        let repo = MemoryKnowledgeRepository::with_pdf_parser_profile("test-parser-v1".to_owned());
        let scope = scope();
        let job_id = queued_pdf(&repo, &scope)
            .await
            .import_job
            .unwrap()
            .import_job_id;
        let original = repo
            .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        {
            let mut state = repo.state.write().await;
            state
                .pdf_jobs
                .get_mut(&job_id)
                .unwrap()
                .lease
                .as_mut()
                .unwrap()
                .expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        }
        let replacement = repo
            .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        assert!(replacement.fencing_token > original.fencing_token);
        assert!(repo.pdf_parse_input(&scope, &original).await.is_err());
        assert!(
            repo.record_pdf_page(
                &scope,
                &original,
                PdfPageResult::Failure {
                    page: 1,
                    code: "parse_failed".to_owned()
                }
            )
            .await
            .is_err()
        );
        let input = repo.pdf_parse_input(&scope, &replacement).await.unwrap();
        assert!(input.manifest.is_none());
    }

    #[test]
    fn knowledge_capabilities_agree_with_shared_media_dispatch() {
        let capabilities = super::KnowledgeCapability::memory();
        assert!(
            capabilities
                .supported_media_types
                .iter()
                .any(|media| media == "text/csv")
        );
        for media in capabilities.supported_media_types {
            assert!(super::is_supported_knowledge_media_type(&media));
            assert!(!capabilities.accepted_unparsed_media_types.contains(&media));
        }
        for media in capabilities.accepted_unparsed_media_types {
            assert!(!super::is_supported_knowledge_media_type(&media));
        }
        assert!(super::is_supported_knowledge_media_type(
            " TEXT/CSV; charset=UTF-8 "
        ));
        assert_eq!(
            super::knowledge_parser_version("TEXT/CSV"),
            "deterministic-csv-v2"
        );
        assert_eq!(
            super::knowledge_parser_version("text/x-markdown; charset=utf-8"),
            "deterministic-text-v1"
        );
    }

    #[test]
    fn source_kind_uses_snake_case_and_accepts_legacy_alias_in_project_contract() {
        assert_eq!(
            serde_json::to_string(&SourceKind::KnowledgeCollection).unwrap(),
            "\"knowledge_collection\""
        );
    }

    #[tokio::test]
    async fn csv_upload_and_attachment_import_preserve_raw_hash_and_table_evidence() {
        let bytes = "\u{feff}name,amount,note\r\n\"样品,蓝\",001,\"first\r\nsecond\"\r\nother, 2 kg ,=SUM(A1)\r\n".as_bytes().to_vec();
        for attachment in [false, true] {
            let repository = MemoryKnowledgeRepository::default();
            let scope = scope();
            let session = repository
                .create_upload_session(
                    &scope,
                    UploadSessionCommand {
                        filename: "table.csv".to_owned(),
                        declared_media_type: "Text/CSV; charset=utf-8".to_owned(),
                        expected_size: bytes.len() as u64,
                        expected_sha256: sha256_hex(&bytes),
                        purpose: KnowledgePurpose::Public,
                    },
                )
                .await
                .unwrap();
            repository
                .put_upload_content(&scope, session.upload_session_id, bytes.clone())
                .await
                .unwrap();
            let accepted = if attachment {
                let (object, name) = repository
                    .complete_attachment_upload(&scope, session.upload_session_id, "commit")
                    .await
                    .unwrap();
                assert_eq!(object.sha256, sha256_hex(&bytes));
                assert!(repository.list_sources(&scope).await.unwrap().is_empty());
                repository
                    .import_batch(
                        &scope,
                        vec![ImportItem {
                            client_item_id: "csv-object".to_owned(),
                            kind: SourceKind::Object,
                            name,
                            purpose: KnowledgePurpose::Public,
                            text: None,
                            url: None,
                            object_id: Some(object.object_id),
                            knowledge_release_id: None,
                        }],
                    )
                    .await
                    .unwrap()
                    .items
                    .remove(0)
            } else {
                repository
                    .complete_upload(&scope, session.upload_session_id, "commit")
                    .await
                    .unwrap()
            };
            let source = accepted.source.unwrap();
            let version = accepted.source_version.unwrap();
            assert_eq!(version.content_sha256, sha256_hex(&bytes));
            assert_eq!(version.parser_version, "deterministic-csv-v2");
            let detail = repository
                .get_source_detail(&scope, source.source_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(detail.chunks.len(), 2);
            assert_eq!(detail.versions[0], version);
            for (index, chunk) in detail.chunks.iter().enumerate() {
                assert_eq!(chunk.kind, super::ChunkKind::Table);
                assert_eq!(chunk.source_version_id, version.source_version_id);
                assert_eq!(
                    chunk.locator,
                    super::ChunkLocator::Csv {
                        start_row: index as u32 + 2,
                        end_row: index as u32 + 2,
                        start_column: 1,
                        end_column: 3,
                        header_row: Some(1),
                        start_char: None,
                        end_char: None,
                    }
                );
                let table: serde_json::Value = serde_json::from_str(&chunk.text).unwrap();
                assert_eq!(
                    table["headers"],
                    serde_json::json!(["name", "amount", "note"])
                );
                let expected = if index == 0 {
                    serde_json::json!(["样品,蓝", "001", "first\r\nsecond"])
                } else {
                    serde_json::json!(["other", " 2 kg ", "=SUM(A1)"])
                };
                assert_eq!(table["values"], expected);
            }
            assert_eq!(accepted.release.unwrap().coverage.chunk_count, 2);
            let result = repository
                .search(
                    &scope,
                    KnowledgeSearchRequest {
                        query: "样品".to_owned(),
                        purpose: KnowledgePurpose::Public,
                        limit: 10,
                        knowledge_release_id: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(result.evidence.len(), 1);
        }
    }

    #[tokio::test]
    async fn malformed_csv_upload_and_object_import_never_publish_partial_sources() {
        for bytes in [
            b"name,value\nvalid,1\nwrong\n".to_vec(),
            b"name,value\nvalid,1\nwrong,\"unclosed".to_vec(),
            b"name,value\nvalid,1\nwrong,\xff\n".to_vec(),
        ] {
            for attachment in [false, true] {
                let repository = MemoryKnowledgeRepository::default();
                let scope = scope();
                let session = repository
                    .create_upload_session(
                        &scope,
                        UploadSessionCommand {
                            filename: "invalid.csv".to_owned(),
                            declared_media_type: "text/csv".to_owned(),
                            expected_size: bytes.len() as u64,
                            expected_sha256: sha256_hex(&bytes),
                            purpose: KnowledgePurpose::Internal,
                        },
                    )
                    .await
                    .unwrap();
                repository
                    .put_upload_content(&scope, session.upload_session_id, bytes.clone())
                    .await
                    .unwrap();
                if attachment {
                    let (object, name) = repository
                        .complete_attachment_upload(&scope, session.upload_session_id, "commit")
                        .await
                        .unwrap();
                    let result = repository
                        .import_batch(
                            &scope,
                            vec![ImportItem {
                                client_item_id: "invalid-csv".to_owned(),
                                kind: SourceKind::Object,
                                name,
                                purpose: KnowledgePurpose::Internal,
                                text: None,
                                url: None,
                                object_id: Some(object.object_id),
                                knowledge_release_id: None,
                            }],
                        )
                        .await
                        .unwrap();
                    let item = &result.items[0];
                    assert_eq!(item.status, super::ImportStatus::Failed);
                    assert!(item.source.is_none() && item.source_version.is_none());
                    assert!(item.release.is_none());
                    assert_eq!(
                        item.error.as_ref().unwrap().code,
                        crate::ErrorCode::InvalidRequest
                    );
                } else {
                    assert!(
                        repository
                            .complete_upload(&scope, session.upload_session_id, "commit")
                            .await
                            .is_err()
                    );
                }
                assert!(repository.list_sources(&scope).await.unwrap().is_empty());
                let state = repository.state.read().await;
                assert!(state.versions.is_empty());
                assert!(state.chunks.is_empty());
                assert!(state.releases.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn text_import_creates_chunks_release_and_evidence_only_answer() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        let accepted = repository
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "intro".to_owned(),
                    kind: SourceKind::Text,
                    name: "intro".to_owned(),
                    purpose: KnowledgePurpose::Public,
                    text: Some(
                        "Acme Widget has a two year warranty.\n\nIt is available in blue."
                            .to_owned(),
                    ),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap();
        assert!(accepted.items[0].release.is_some());
        let answer = repository
            .ask(
                &scope,
                KnowledgeSearchRequest {
                    query: "warranty".to_owned(),
                    purpose: KnowledgePurpose::Public,
                    limit: 10,
                    knowledge_release_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(answer.mode, "evidence_only");
        assert_eq!(answer.evidence.len(), 1);
    }

    #[tokio::test]
    async fn internal_source_is_not_in_public_search() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        repository
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "secret".to_owned(),
                    kind: SourceKind::Text,
                    name: "secret".to_owned(),
                    purpose: KnowledgePurpose::Internal,
                    text: Some("Internal launch date is next Tuesday.".to_owned()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap();
        assert!(
            repository
                .search(
                    &scope,
                    KnowledgeSearchRequest {
                        query: "launch".to_owned(),
                        purpose: KnowledgePurpose::Public,
                        limit: 10,
                        knowledge_release_id: None,
                    },
                )
                .await
                .unwrap()
                .evidence
                .is_empty()
        );
    }

    #[tokio::test]
    async fn upload_checks_size_checksum_and_idempotent_completion() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        let bytes = b"Verified upload text".to_vec();
        let session = repository
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "verified.txt".to_owned(),
                    declared_media_type: "text/plain".to_owned(),
                    expected_size: bytes.len() as u64,
                    expected_sha256: sha256_hex(&bytes),
                    purpose: KnowledgePurpose::Public,
                },
            )
            .await
            .unwrap();
        repository
            .put_upload_content(&scope, session.upload_session_id, bytes)
            .await
            .unwrap();
        let first = repository
            .complete_upload(&scope, session.upload_session_id, "same")
            .await
            .unwrap();
        let replay = repository
            .complete_upload(&scope, session.upload_session_id, "same")
            .await
            .unwrap();
        assert_eq!(
            first.release.unwrap().knowledge_release_id,
            replay.release.unwrap().knowledge_release_id
        );
    }

    #[tokio::test]
    async fn attachment_import_reuses_verified_object_and_receipt() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        let text = "x".repeat(super::MAX_INLINE_TEXT_BYTES + 1);
        assert!(text.len() > super::MAX_INLINE_TEXT_BYTES);
        let bytes = text.into_bytes();
        let session = repository
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "notes.md".to_owned(),
                    declared_media_type: "text/markdown".to_owned(),
                    expected_size: bytes.len() as u64,
                    expected_sha256: sha256_hex(&bytes),
                    purpose: KnowledgePurpose::Internal,
                },
            )
            .await
            .unwrap();
        repository
            .put_upload_content(&scope, session.upload_session_id, bytes.clone())
            .await
            .unwrap();
        let (object, filename) = repository
            .complete_attachment_upload(&scope, session.upload_session_id, "attachment")
            .await
            .unwrap();
        assert!(repository.list_sources(&scope).await.unwrap().is_empty());
        let item = ImportItem {
            client_item_id: format!("agent-attachment:{}", object.object_id),
            kind: SourceKind::Object,
            name: filename,
            purpose: KnowledgePurpose::Internal,
            text: None,
            url: None,
            object_id: Some(object.object_id),
            knowledge_release_id: None,
        };
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        let denied = repository
            .import_batch(&other, vec![item.clone()])
            .await
            .unwrap();
        assert_eq!(
            denied.items[0].error.as_ref().unwrap().code,
            crate::ErrorCode::NotFound
        );
        let (first, replay) = tokio::join!(
            repository.import_batch(&scope, vec![item.clone()]),
            repository.import_batch(&scope, vec![item.clone()])
        );
        let first = first.unwrap().items.remove(0);
        assert_eq!(first, replay.unwrap().items.remove(0));
        assert_eq!(repository.list_sources(&scope).await.unwrap().len(), 1);
        let version = first.source_version.unwrap();
        assert_eq!(version.object_id, Some(object.object_id));
        assert_eq!(version.object_version, Some(object.object_version));
        assert_eq!(version.content_sha256, object.sha256);
        assert_eq!(
            first.source.unwrap().locator["object_id"],
            object.object_id.to_string()
        );
        let mut changed = item;
        changed.purpose = KnowledgePurpose::Public;
        let conflict = repository
            .import_batch(&scope, vec![changed])
            .await
            .unwrap();
        assert_eq!(
            conflict.items[0].error.as_ref().unwrap().code,
            crate::ErrorCode::Conflict
        );
        assert_eq!(repository.list_sources(&scope).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn attachment_import_preserves_partial_result_for_unparsed_file() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        let mut items = Vec::new();
        for (filename, media, bytes) in [
            ("valid.txt", "text/plain", b"Verified evidence".as_slice()),
            (
                "unsupported.pdf",
                "application/pdf",
                b"opaque bytes".as_slice(),
            ),
        ] {
            let session = repository
                .create_upload_session(
                    &scope,
                    UploadSessionCommand {
                        filename: filename.to_owned(),
                        declared_media_type: media.to_owned(),
                        expected_size: bytes.len() as u64,
                        expected_sha256: sha256_hex(bytes),
                        purpose: KnowledgePurpose::Internal,
                    },
                )
                .await
                .unwrap();
            repository
                .put_upload_content(&scope, session.upload_session_id, bytes.to_vec())
                .await
                .unwrap();
            let (object, name) = repository
                .complete_attachment_upload(&scope, session.upload_session_id, filename)
                .await
                .unwrap();
            items.push(ImportItem {
                client_item_id: format!("agent-attachment:{}", object.object_id),
                kind: SourceKind::Object,
                name,
                purpose: KnowledgePurpose::Internal,
                text: None,
                url: None,
                object_id: Some(object.object_id),
                knowledge_release_id: None,
            });
        }
        let result = repository.import_batch(&scope, items).await.unwrap();
        assert!(result.items[0].release.is_some());
        assert_eq!(
            result.items[1].error.as_ref().unwrap().code,
            crate::ErrorCode::CapabilityMissing
        );
        assert_eq!(repository.list_sources(&scope).await.unwrap().len(), 1);
    }

    #[test]
    fn import_progress_errors_are_bounded_and_never_reflect_raw_input() {
        let input = (0..104)
            .map(|page| {
                serde_json::json!({
                    "code": format!("filename-private-{page}"),
                    "page":page + 1,
                    "text":"confidential extracted bytes",
                    "locator":"private resource"
                })
            })
            .collect::<Vec<_>>();
        let (count, errors) = super::knowledge_import_progress_errors(&input);
        assert_eq!(count, 104);
        assert_eq!(errors.len(), 100);
        assert_eq!(errors[0].code, "import_failed");
        assert_eq!(errors[0].page, Some(1));
        let encoded = serde_json::to_string(&errors).unwrap();
        assert!(!encoded.contains("private"));
        assert!(!encoded.contains("confidential"));
        assert_eq!(
            super::knowledge_import_progress_error(
                &serde_json::json!({"code":"ocr_required","page":4294967296_u64})
            )
            .page,
            None
        );
    }

    #[tokio::test]
    async fn receipt_progress_reads_original_live_job_and_rechecks_source_and_release() {
        let repo = MemoryKnowledgeRepository::with_pdf_parser_profile("test-parser-v1".to_owned());
        let scope = scope();
        let bytes = b"%PDF-1.7\nverified original bytes".to_vec();
        let session = repo
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "input.pdf".to_owned(),
                    declared_media_type: "application/pdf".to_owned(),
                    expected_size: bytes.len() as u64,
                    expected_sha256: sha256_hex(&bytes),
                    purpose: KnowledgePurpose::Public,
                },
            )
            .await
            .unwrap();
        repo.put_upload_content(&scope, session.upload_session_id, bytes)
            .await
            .unwrap();
        let (object, name) = repo
            .complete_attachment_upload(&scope, session.upload_session_id, "attachment")
            .await
            .unwrap();
        let item = ImportItem {
            client_item_id: "safe-receipt".to_owned(),
            kind: SourceKind::Object,
            name,
            purpose: KnowledgePurpose::Public,
            text: None,
            url: None,
            object_id: Some(object.object_id),
            knowledge_release_id: None,
        };
        assert!(
            repo.resolve_import_receipt(&scope, &item)
                .await
                .unwrap()
                .is_none()
        );
        let receipt = repo
            .import_batch(&scope, vec![item.clone()])
            .await
            .unwrap()
            .items
            .remove(0);
        let job_id = receipt.import_job.as_ref().unwrap().import_job_id;
        assert_eq!(receipt.status, ImportStatus::Queued);
        let queued = repo
            .resolve_import_receipt(&scope, &item)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queued.import_job_id, Some(job_id));
        assert_eq!(queued.status, ImportStatus::Queued);
        assert!(queued.source_version_id.is_none());
        let cross_project = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(Uuid::new_v4().into()),
        );
        assert!(
            repo.get_import_progress(&cross_project, job_id, item.purpose)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.resolve_import_receipt(&cross_project, &item)
                .await
                .unwrap()
                .is_none()
        );
        let mut changed = item.clone();
        changed.name = "different.pdf".to_owned();
        assert_eq!(
            repo.resolve_import_receipt(&scope, &changed)
                .await
                .unwrap_err()
                .code,
            crate::ErrorCode::Conflict
        );
        let lease = repo
            .claim_pdf_parse(&scope, job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        let input = repo.pdf_parse_input(&scope, &lease).await.unwrap();
        repo.record_pdf_manifest(
            &scope,
            &lease,
            PdfDocumentManifest {
                schema_version: PDF_PARSE_SCHEMA_VERSION.to_owned(),
                input_sha256: input.input_sha256,
                parser_version: input.parser_profile,
                page_count: 2,
            },
        )
        .await
        .unwrap();
        repo.record_pdf_page(
            &scope,
            &lease,
            PdfPageResult::Success {
                page: 1,
                text: "Good searchable text".to_owned(),
            },
        )
        .await
        .unwrap();
        repo.record_pdf_page(
            &scope,
            &lease,
            PdfPageResult::Failure {
                page: 2,
                code: "ocr_required".to_owned(),
            },
        )
        .await
        .unwrap();
        let running = repo
            .get_import_progress(&scope, job_id, item.purpose)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(running.status, ImportStatus::Running);
        assert_eq!((running.completed_units, running.failed_units), (1, 1));
        assert!(running.source_version_id.is_none());
        let completed = repo.finish_pdf_parse(&scope, &lease).await.unwrap();
        assert_eq!(completed.status, ImportStatus::Partial);
        assert_eq!(receipt.status, ImportStatus::Queued); // immutable receipt
        let partial = repo
            .resolve_import_receipt(&scope, &item)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(partial.status, ImportStatus::Partial);
        assert_eq!(
            repo.get_import_progress(&scope, job_id, KnowledgePurpose::Internal)
                .await
                .unwrap()
                .unwrap(),
            partial
        );
        assert_eq!(
            partial.source_version_id,
            completed
                .source_version
                .as_ref()
                .map(|v| v.source_version_id)
        );
        assert_eq!(
            partial.knowledge_release_id,
            completed.release.as_ref().map(|r| r.knowledge_release_id)
        );
        assert_eq!(partial.errors[0].code, "ocr_required");
        let retry = repo.retry_pdf_parse(&scope, job_id).await.unwrap();
        assert_ne!(retry.import_job_id, job_id);
        let retry_lease = repo
            .claim_pdf_parse(&scope, retry.import_job_id, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .unwrap();
        repo.record_pdf_page(
            &scope,
            &retry_lease,
            PdfPageResult::Success {
                page: 2,
                text: "Recovered text".to_owned(),
            },
        )
        .await
        .unwrap();
        let retried = repo.finish_pdf_parse(&scope, &retry_lease).await.unwrap();
        assert_eq!(retried.status, ImportStatus::Succeeded);
        assert_ne!(
            retried.release.as_ref().unwrap().knowledge_release_id,
            partial.knowledge_release_id.unwrap()
        );
        assert_eq!(
            repo.resolve_import_receipt(&scope, &item)
                .await
                .unwrap()
                .unwrap(),
            partial
        );
        {
            let mut state = repo.state.write().await;
            state
                .sources
                .get_mut(&partial.source_id.unwrap())
                .unwrap()
                .purpose = KnowledgePurpose::Internal;
        }
        assert!(
            repo.resolve_import_receipt(&scope, &item)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.get_import_progress(&scope, job_id, KnowledgePurpose::Public)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.get_import_progress(&scope, job_id, KnowledgePurpose::Internal)
                .await
                .unwrap()
                .is_some()
        );
        {
            let mut state = repo.state.write().await;
            state
                .sources
                .get_mut(&partial.source_id.unwrap())
                .unwrap()
                .purpose = KnowledgePurpose::Public;
            state
                .releases
                .get_mut(&partial.knowledge_release_id.unwrap())
                .unwrap()
                .source_version_refs
                .clear();
        }
        assert_eq!(
            repo.get_import_progress(&scope, job_id, item.purpose)
                .await
                .unwrap_err()
                .code,
            crate::ErrorCode::Conflict
        );
        {
            let mut state = repo.state.write().await;
            state
                .releases
                .get_mut(&partial.knowledge_release_id.unwrap())
                .unwrap()
                .source_version_refs
                .push(partial.source_version_id.unwrap());
            state
                .stored_objects
                .get_mut(&object.object_id)
                .unwrap()
                .sha256 = "mismatched".to_owned();
        }
        assert_eq!(
            repo.get_import_progress(&scope, job_id, item.purpose)
                .await
                .unwrap_err()
                .code,
            crate::ErrorCode::Conflict
        );
    }

    #[tokio::test]
    async fn immediate_failure_and_synchronous_success_have_live_safe_progress() {
        let repo = MemoryKnowledgeRepository::default();
        let scope = scope();
        let failed = ImportItem {
            client_item_id: "failed".to_owned(),
            kind: SourceKind::Object,
            name: "unavailable".to_owned(),
            purpose: KnowledgePurpose::Internal,
            text: None,
            url: None,
            object_id: Some(Uuid::new_v4()),
            knowledge_release_id: None,
        };
        let acceptance = repo
            .import_batch(&scope, vec![failed.clone()])
            .await
            .unwrap()
            .items
            .remove(0);
        assert!(acceptance.import_job.is_none());
        let progress = repo
            .resolve_import_receipt(&scope, &failed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(progress.status, ImportStatus::Failed);
        assert_eq!(progress.import_job_id, None);
        assert_eq!(progress.error_count, 1);
        assert_eq!(progress.errors[0].code, "not_found");
        let success = ImportItem {
            client_item_id: "success".to_owned(),
            kind: SourceKind::Text,
            name: "generic".to_owned(),
            purpose: KnowledgePurpose::Public,
            text: Some("evidence text".to_owned()),
            url: None,
            object_id: None,
            knowledge_release_id: None,
        };
        let acceptance = repo
            .import_batch(&scope, vec![success.clone()])
            .await
            .unwrap()
            .items
            .remove(0);
        let progress = repo
            .resolve_import_receipt(&scope, &success)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(progress.status, ImportStatus::Succeeded);
        assert_eq!(
            progress.source_version_id,
            acceptance
                .source_version
                .as_ref()
                .map(|v| v.source_version_id)
        );
        assert_eq!(
            progress.knowledge_release_id,
            acceptance.release.as_ref().map(|r| r.knowledge_release_id)
        );
        assert_eq!(
            repo.get_import_progress(
                &scope,
                acceptance.import_job.unwrap().import_job_id,
                success.purpose
            )
            .await
            .unwrap()
            .unwrap(),
            progress
        );
        {
            let mut state = repo.state.write().await;
            state
                .sources
                .get_mut(&progress.source_id.unwrap())
                .unwrap()
                .state = super::SourceState::Removed;
        }
        assert!(
            repo.resolve_import_receipt(&scope, &success)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn cross_tenant_lookup_is_not_found() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        let other = TenantScope::new(scope.operator_id, Uuid::new_v4().into(), scope.project_id);
        let session = repository
            .create_upload_session(
                &scope,
                UploadSessionCommand {
                    filename: "one.txt".to_owned(),
                    declared_media_type: "text/plain".to_owned(),
                    expected_size: 1,
                    expected_sha256: sha256_hex(b"x"),
                    purpose: KnowledgePurpose::Public,
                },
            )
            .await
            .unwrap();
        assert!(
            repository
                .put_upload_content(&other, session.upload_session_id, b"x".to_vec())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn document_manifest_is_finite_deterministic_and_replayed_without_internal_sources() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        let imported = repository
            .import_batch(
                &scope,
                vec![
                    ImportItem {
                        client_item_id: "public".to_owned(),
                        kind: SourceKind::Text,
                        name: "public".to_owned(),
                        purpose: KnowledgePurpose::Public,
                        text: Some("Public description".to_owned()),
                        url: None,
                        object_id: None,
                        knowledge_release_id: None,
                    },
                    ImportItem {
                        client_item_id: "internal".to_owned(),
                        kind: SourceKind::Text,
                        name: "internal".to_owned(),
                        purpose: KnowledgePurpose::Internal,
                        text: Some("Private planning notes".to_owned()),
                        url: None,
                        object_id: None,
                        knowledge_release_id: None,
                    },
                ],
            )
            .await
            .unwrap();
        let release = imported.items[1].release.as_ref().unwrap();
        let scope_spec = DocumentScope {
            markets: vec!["CN".to_owned(), "US".to_owned()],
            languages: vec!["zh".to_owned()],
            content_types: vec!["company_profile".to_owned(), "faq".to_owned()],
            ..DocumentScope::default()
        };
        let request = DocumentManifestPlanRequest {
            manifest_id: Uuid::new_v4(),
            knowledge_release_id: release.knowledge_release_id,
        };
        let first = repository
            .plan_document_manifest(&scope, request.clone(), scope_spec.clone())
            .await
            .unwrap();
        assert!(first.sealed);
        assert_eq!(first.expected_count, Some(4));
        assert_eq!(first.coverage.planned, 4);
        assert_eq!(first.items.len(), 4);
        assert!(
            first
                .items
                .iter()
                .all(|item| item.state == DocumentManifestItemState::Planned)
        );
        assert!(
            first
                .items
                .iter()
                .all(|item| item.source_version_refs.len() == 1)
        );
        assert_eq!(
            first,
            repository
                .plan_document_manifest(&scope, request.clone(), scope_spec.clone())
                .await
                .unwrap()
        );
        let internal_change = repository
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "internal-2".to_owned(),
                    kind: SourceKind::Text,
                    name: "internal-2".to_owned(),
                    purpose: KnowledgePurpose::Internal,
                    text: Some("Another private note".to_owned()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap();
        let next = repository
            .plan_document_manifest(
                &scope,
                DocumentManifestPlanRequest {
                    manifest_id: Uuid::new_v4(),
                    knowledge_release_id: internal_change.items[0]
                        .release
                        .as_ref()
                        .unwrap()
                        .knowledge_release_id,
                },
                scope_spec.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| &item.dependency_hash)
                .collect::<Vec<_>>(),
            next.items
                .iter()
                .map(|item| &item.dependency_hash)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            first.items[0].source_version_refs,
            next.items[0].source_version_refs
        );
        assert!(
            repository
                .plan_document_manifest(
                    &scope,
                    request,
                    DocumentScope {
                        markets: vec!["GB".to_owned()],
                        ..scope_spec
                    }
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn internal_only_release_keeps_blocked_denominator() {
        let repository = MemoryKnowledgeRepository::default();
        let scope = scope();
        let imported = repository
            .import_batch(
                &scope,
                vec![ImportItem {
                    client_item_id: "internal".to_owned(),
                    kind: SourceKind::Text,
                    name: "internal".to_owned(),
                    purpose: KnowledgePurpose::Internal,
                    text: Some("Internal source".to_owned()),
                    url: None,
                    object_id: None,
                    knowledge_release_id: None,
                }],
            )
            .await
            .unwrap();
        let manifest = repository
            .plan_document_manifest(
                &scope,
                DocumentManifestPlanRequest {
                    manifest_id: Uuid::new_v4(),
                    knowledge_release_id: imported.items[0]
                        .release
                        .as_ref()
                        .unwrap()
                        .knowledge_release_id,
                },
                DocumentScope::default(),
            )
            .await
            .unwrap();
        assert_eq!(manifest.expected_count, Some(1));
        assert_eq!(manifest.coverage.blocked, 1);
        assert!(manifest.items[0].source_version_refs.is_empty());
        assert_eq!(
            manifest.items[0].block_reason.as_deref(),
            Some("knowledge_release_has_no_public_sources")
        );
    }

    #[test]
    fn document_planner_rejects_unbounded_cross_product() {
        let scope = scope();
        let release = super::KnowledgeRelease {
            knowledge_release_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: scope.project_id.unwrap(),
            sequence: 1,
            previous_release_id: None,
            source_version_refs: vec![],
            fact_revision_refs: vec![],
            index_build_id: "test".to_owned(),
            pipeline_versions: serde_json::json!({}),
            content_hash: sha256_hex(b"test"),
            coverage: super::KnowledgeCoverage {
                source_version_count: 0,
                chunk_count: 0,
                failed_source_count: 0,
                blocked_reasons: vec![],
            },
            created_at: chrono::Utc::now(),
        };
        let spec = DocumentScope {
            markets: (0..101).map(|i| format!("market-{i}")).collect(),
            languages: (0..101).map(|i| format!("lang-{i}")).collect(),
            ..DocumentScope::default()
        };
        assert!(
            super::plan_document_manifest(&scope, &release, Uuid::new_v4(), &spec, &[],).is_err()
        );
    }
}
