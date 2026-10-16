//! Persisted PDF parse contract. The parser is an isolated adapter; these
//! values are accepted only against verified object bytes and a current lease.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, Chunk, ChunkKind, ChunkLocator, TenantScope, sha256_hex};

pub const PDF_PARSE_SCHEMA_VERSION: &str = "geo.pdf.parse.v1";
pub const PDF_MAX_PAGES: u32 = 10_000;
pub const PDF_MAX_PAGE_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const PDF_MAX_DOCUMENT_TEXT_BYTES: usize = 32 * 1024 * 1024;
const PDF_CHUNK_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PdfParseCursor {
    pub created_at: DateTime<Utc>,
    pub job_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PdfParseJobRef {
    pub scope: TenantScope,
    pub job_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PdfParseLease {
    pub job_id: Uuid,
    pub lease_id: Uuid,
    pub fencing_token: i64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PdfDocumentManifest {
    pub schema_version: String,
    pub input_sha256: String,
    pub parser_version: String,
    pub page_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PdfPageText {
    pub page: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PdfPageResult {
    Success { page: u32, text: String },
    Failure { page: u32, code: String },
}

impl PdfPageResult {
    pub fn page(&self) -> u32 {
        match self {
            Self::Success { page, .. } | Self::Failure { page, .. } => *page,
        }
    }

    pub fn validate(&self, page_count: u32) -> Result<(), AppError> {
        if self.page() == 0 || self.page() > page_count {
            return Err(AppError::invalid_request(
                "PDF page is outside the manifest",
            ));
        }
        match self {
            Self::Success { text, .. }
                if text.trim().is_empty() || text.len() > PDF_MAX_PAGE_TEXT_BYTES =>
            {
                Err(AppError::invalid_request(
                    "PDF page text is empty or too large",
                ))
            }
            Self::Failure { code, .. }
                if !matches!(
                    code.as_str(),
                    "ocr_required" | "empty_text" | "parse_failed" | "page_limit"
                ) =>
            {
                Err(AppError::invalid_request("unknown PDF page failure code"))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfParseInput {
    /// Server-verified original bytes, never client-provided page text.
    pub bytes: Vec<u8>,
    pub input_sha256: String,
    pub media_type: String,
    pub parser_profile: String,
    pub successful_pages: Vec<u32>,
    pub manifest: Option<PdfDocumentManifest>,
}

impl std::fmt::Debug for PdfParseInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfParseInput")
            .field(
                "bytes",
                &format_args!("<redacted; {} bytes>", self.bytes.len()),
            )
            .field("input_sha256", &self.input_sha256)
            .field("media_type", &self.media_type)
            .field("parser_profile", &self.parser_profile)
            .field("successful_pages", &self.successful_pages)
            .field("manifest", &self.manifest)
            .finish()
    }
}

impl PdfDocumentManifest {
    pub fn validate(&self, input_sha256: &str, profile: &str) -> Result<(), AppError> {
        if self.schema_version != PDF_PARSE_SCHEMA_VERSION
            || self.input_sha256 != input_sha256
            || self.parser_version != profile
            || self.page_count == 0
            || self.page_count > PDF_MAX_PAGES
        {
            return Err(AppError::invalid_request(
                "PDF manifest does not match verified input",
            ));
        }
        Ok(())
    }
}

/// Keep the original extracted text intact in stored page results. Search
/// chunks use bounded Unicode character ranges, each pointing to a PDF page.
pub fn pdf_page_chunks(
    scope: &TenantScope,
    source_version_id: Uuid,
    page: &PdfPageText,
    first_ordinal: i32,
) -> Result<Vec<Chunk>, AppError> {
    if page.page == 0 || page.text.len() > PDF_MAX_PAGE_TEXT_BYTES {
        return Err(AppError::invalid_request("invalid PDF page text"));
    }
    let project_id = scope
        .project_id
        .ok_or_else(|| AppError::invalid_request("project_id is required"))?;
    let characters = page.text.chars().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    for (start, part) in characters.chunks(PDF_CHUNK_CHARS).enumerate() {
        let text = part.iter().collect::<String>();
        if text.trim().is_empty() {
            continue;
        }
        let start_char = start * PDF_CHUNK_CHARS;
        let end_char = start_char + part.len();
        let start_char = u32::try_from(start_char)
            .map_err(|_| AppError::invalid_request("PDF character offset exceeds limit"))?;
        let end_char = u32::try_from(end_char)
            .map_err(|_| AppError::invalid_request("PDF character offset exceeds limit"))?;
        chunks.push(Chunk {
            chunk_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            source_version_id,
            ordinal: first_ordinal + chunks.len() as i32,
            kind: ChunkKind::Paragraph,
            text_hash: sha256_hex(text.as_bytes()),
            text,
            locator: ChunkLocator::Pdf {
                page: page.page,
                bbox: None,
                ocr: false,
                start_char: Some(start_char),
                end_char: Some(end_char),
            },
            product_ids: Vec::new(),
            market: None,
            language: None,
            extraction_method: "pdf_text_v1".to_owned(),
            confidence: 1.0,
        });
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::{PdfPageResult, PdfParseInput};

    #[test]
    fn raw_pdf_input_is_redacted_and_empty_pages_cannot_be_successful() {
        let input = PdfParseInput {
            bytes: b"secret-raw-pdf-content".to_vec(),
            input_sha256: "verified".to_owned(),
            media_type: "application/pdf".to_owned(),
            parser_profile: "test-v1".to_owned(),
            successful_pages: Vec::new(),
            manifest: None,
        };
        let diagnostic = format!("{input:?}");
        assert!(!diagnostic.contains("secret-raw-pdf-content"));
        assert!(
            PdfPageResult::Success {
                page: 1,
                text: "  \n".to_owned()
            }
            .validate(1)
            .is_err()
        );
    }
}
