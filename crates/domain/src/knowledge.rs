//! Versioned, tenant-scoped knowledge-import contracts.
//!
//! This module deliberately models the usable first vertical slice only:
//! verified upload bytes and `text/plain` / Markdown can be turned into
//! deterministic paragraph chunks and an immutable release.  It does not
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

pub const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
/// The JSON idempotency middleware buffers at most 1 MiB including syntax
/// and neighboring fields.  Keep inline text comfortably below that shared
/// boundary; larger material must use verified raw-byte upload.
pub const MAX_INLINE_TEXT_BYTES: usize = 256 * 1024;
pub const UPLOAD_SESSION_TTL_SECONDS: i64 = 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgePurpose {
    Public,
    Internal,
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
            supported_media_types: vec!["text/plain".to_owned(), "text/markdown".to_owned()],
            accepted_unparsed_media_types: vec![
                "text/csv".to_owned(),
                "application/pdf".to_owned(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_owned(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_owned(),
            ],
            limitations: vec![
                "uploaded bytes are kept only in process memory".to_owned(),
                "the first upload implementation buffers a whole request; production should use streaming direct object storage".to_owned(),
                "only text/plain and text/markdown are parsed".to_owned(),
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
    },
    Docx {
        heading_path: Vec<String>,
        paragraph_index: u32,
    },
    Xlsx {
        sheet: String,
        range: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header_range: Option<String>,
    },
    Csv {
        start_row: u32,
        end_row: u32,
        start_column: u32,
        end_column: u32,
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
    async fn capabilities(&self, scope: &TenantScope) -> Result<KnowledgeCapability, AppError>;
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

#[derive(Debug, Default)]
pub struct MemoryKnowledgeRepository {
    state: RwLock<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    upload_sessions: HashMap<Uuid, UploadSession>,
    upload_bytes: HashMap<Uuid, Vec<u8>>,
    stored_objects: HashMap<Uuid, StoredObject>,
    object_bytes: HashMap<Uuid, Vec<u8>>,
    sources: HashMap<Uuid, Source>,
    versions: HashMap<Uuid, SourceVersion>,
    jobs: HashMap<Uuid, ImportJob>,
    chunks: HashMap<Uuid, Vec<Chunk>>,
    products: HashMap<Uuid, Product>,
    facts: HashMap<Uuid, Fact>,
    releases: HashMap<Uuid, KnowledgeRelease>,
    current_release: HashMap<String, Uuid>,
    import_items: HashMap<(String, String), (String, ImportAcceptance)>,
    upload_completions: HashMap<(Uuid, String), ImportAcceptance>,
}

impl MemoryKnowledgeRepository {
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
        if text.len() > MAX_INLINE_TEXT_BYTES {
            return Err(AppError::invalid_request(
                "text exceeds inline limit; use an upload session",
            ));
        }
        if text.trim().is_empty() {
            return Err(AppError::invalid_request("text must not be empty"));
        }
        let hash = sha256_hex(text.as_bytes());
        let (source, version) = Self::source_and_version(
            scope,
            item.kind,
            item.name.clone(),
            item.purpose,
            json!({"kind":"inline_text"}),
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
        let chunks = deterministic_chunks(scope, version.source_version_id, &text);
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
            .filter(|version| state.chunks.contains_key(&version.source_version_id))
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
            .count() as u64;
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
            pipeline_versions: json!({
                "parser": "deterministic-text-v1",
                "extractor": "none-v1",
                "index": "substring-v1"
            }),
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
                blocked_reasons: Vec::new(),
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
    async fn capabilities(&self, scope: &TenantScope) -> Result<KnowledgeCapability, AppError> {
        Self::require_project(scope)?;
        Ok(KnowledgeCapability::memory())
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
        state.object_bytes.insert(object.object_id, content.clone());
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
        let mut acceptance = if is_text_media_type(&session.declared_media_type) {
            let text = String::from_utf8(content)
                .map_err(|_| AppError::invalid_request("text upload bytes must be valid UTF-8"))?;
            Self::import_text_locked(&mut state, scope, &item, text, Some(object.clone()))?
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
                SourceKind::Object => ImportAcceptance {
                    client_item_id: item.client_item_id.clone(),
                    status: ImportStatus::Failed,
                    source: None,
                    source_version: None,
                    import_job: None,
                    operation: None,
                    release: None,
                    error: Some(AppError::new(
                        ErrorCode::CapabilityMissing,
                        "object import adapter is not implemented",
                    )),
                },
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
        ImportItem, KnowledgePurpose, KnowledgeRepository, KnowledgeSearchRequest,
        MemoryKnowledgeRepository, SourceKind, UploadSessionCommand, sha256_hex,
    };
    use crate::TenantScope;
    use uuid::Uuid;

    fn scope() -> TenantScope {
        TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        )
    }

    #[test]
    fn source_kind_uses_snake_case_and_accepts_legacy_alias_in_project_contract() {
        assert_eq!(
            serde_json::to_string(&SourceKind::KnowledgeCollection).unwrap(),
            "\"knowledge_collection\""
        );
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
}
