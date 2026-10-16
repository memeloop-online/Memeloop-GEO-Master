//! Domain contracts shared by the HTTP API and background roles.
//!
//! The types in this crate deliberately do not depend on a database or an HTTP
//! framework.  They are the stable boundary used by the modular monolith.

mod accounts;
mod agent;
mod partners;
mod projects;
mod publishing;

use accounts::{
    account_networks, auth, browser_operations, channel_imports, channels, operator_login_queue,
    proxy_pools,
};
use content::{content_media, content_reuse, rich_content};
use knowledge::{knowledge_csv, office_parse, pdf_parse};
use measurement::questions;
use partners::{partner, partner_admin};
use projects::{appearance, model_routes, project, project_ai_settings, tenancy};
use publishing::{
    channel_jobs, connector_capabilities, content_distribution_request, distribution,
    publication_lookup, publication_send_authorization,
};

pub use account_networks::*;
pub use appearance::{
    DEFAULT_LOCALE, DEFAULT_PRIMARY_COLOR, OperatorAppearance, UpdateOperatorAppearance,
};
pub use browser_operations::*;
pub use channel_jobs::*;
pub use partner::*;
pub use partner_admin::*;
mod measurement;
pub use channel_imports::*;
pub use channels::*;
pub use connector_capabilities::*;
pub use measurement::*;
pub use operator_login_queue::*;
pub use proxy_pools::*;
mod content;
pub use content::*;
pub use content_distribution_request::*;
pub use content_media::*;
pub use content_reuse::*;
pub use distribution::*;
pub use rich_content::*;
mod error;
mod event;
mod idempotency;
mod knowledge;
pub use model_routes::ModelRouteGrant;
pub use office_parse::*;
pub use pdf_parse::{
    PDF_MAX_DOCUMENT_TEXT_BYTES, PDF_MAX_PAGE_TEXT_BYTES, PDF_MAX_PAGES, PDF_PARSE_SCHEMA_VERSION,
    PdfDocumentManifest, PdfPageResult, PdfPageText, PdfParseCursor, PdfParseInput, PdfParseJobRef,
    PdfParseLease, pdf_page_chunks,
};
mod operation;
pub use publication_lookup::*;
pub use publication_send_authorization::*;
pub use questions::*;

pub use agent::{
    AgentCheckpoint, AgentRepository, AgentRuntime, AppendMessage, AttachmentId,
    AttachmentReference, CheckpointId, Conversation, ConversationDetail, ConversationEvent,
    ConversationEventId, ConversationId, ConversationStatus, CreateConversation, MAX_HISTORY_BYTES,
    MAX_HISTORY_TURNS, MAX_MESSAGE_CHARS, MemoryAgentRepository, Message, MessageId, MessageRole,
    MissingAgentRuntime, ObjectRef, RUNTIME_NOT_CONFIGURED, RecordToolCall, Run, RunCompletion,
    RunId, RunStatus, RunTransition, RuntimeCapability, RuntimeCapabilityStatus,
    SharedAgentRepository, StoreCheckpoint, SubmitAcceptance, ToolCallDecision, ToolCallIdentity,
    ToolCallLedgerEntry, ToolCallLedgerId, ToolCallOutcome, Turn, TurnHistoryMessage, TurnId,
    TurnInput, TurnReport, TurnStatus, validate_append_message, validate_checkpoint_write,
    validate_message_content, validate_tool_call_authorized, validate_tool_call_begin,
    validate_tool_call_finish, validate_tool_call_identity, validate_tool_call_write,
};
pub use auth::{
    AuthRepository, DEFAULT_SESSION_TTL_SECS, DEVELOPMENT_USER_EMAIL, LoginIdentity, Membership,
    MemoryAuthRepository, Role, SESSION_COOKIE_MAX_AGE_SECS, Session, SessionCredentials,
    SessionId, User, UserId, hash_token, normalize_email, normalize_host,
};
pub use error::{AppError, ErrorCode};
pub use event::EventEnvelope;
pub use idempotency::{IdempotencyDecision, IdempotencyStore, IdempotencyToken, StoredResponse};
pub use knowledge::{
    AttachmentObjectBytes, CONTENT_EVIDENCE_MAX_QUOTE_CHARS, Chunk, ChunkKind, ChunkLocator,
    ContentKnowledgeGuard, ContentPublicEligibility, CurrentKnowledgeRelease,
    DOCUMENT_PLANNER_VERSION, DocumentManifest, DocumentManifestCoverage, DocumentManifestItem,
    DocumentManifestItemState, DocumentManifestPlanRequest, DocumentManifestState, EvidenceRef,
    Fact, FactStatus, ImportAcceptance, ImportBatchAcceptance, ImportItem, ImportJob, ImportStage,
    ImportStatus, KnowledgeCapability, KnowledgeCoverage, KnowledgeEvidence,
    KnowledgeImportProgress, KnowledgeImportProgressError, KnowledgeOverview, KnowledgePurpose,
    KnowledgeRelease, KnowledgeRepository, MAX_INLINE_TEXT_BYTES, MAX_UPLOAD_BYTES,
    MemoryKnowledgeRepository, Product, ProductState, ReviseSourceTextCommand, Source,
    SourceDetail, SourceKind, SourceState, SourceTextBasis, SourceTextRevisionReceipt,
    SourceVersion, SourceVersionContent, SourceVersionRepresentation, StoredObject,
    StoredObjectState, UPLOAD_SESSION_TTL_SECONDS, UploadSession, UploadSessionCommand,
    UploadSessionState, deterministic_chunks, is_supported_knowledge_media_type,
    knowledge_import_progress_app_error, knowledge_import_progress_error,
    knowledge_import_progress_errors, knowledge_parser_version, parsed_knowledge_chunks,
    plan_document_manifest, sha256_hex,
};
pub use operation::{Operation, OperationStatus};
pub use project::{
    ContentGuardMode, ContentProjectGuard, CreateProject, CycleReportView, DEVELOPMENT_OPERATOR_ID,
    DEVELOPMENT_PROJECT_ID, DEVELOPMENT_TENANT_ID, DistributionManifestAcceptance,
    DistributionScope, DistributionScopeMode, DocumentManifestAcceptance, DocumentScope,
    InitialSource, InitialSourceKind, InitialSourceVisibility, MemoryProjectRepository, Operator,
    OperatorProjectPage, OperatorProjectSummary, OverviewKnowledgeStatus, PendingSuccessorCycle,
    PeriodPolicy, Project, ProjectCreate, ProjectOverview, ProjectPage, ProjectPatch,
    ProjectRepository, ProjectSettings, ProjectStartAcceptance, ProjectStartCommand,
    ProjectStartView, ProjectStatus, QuestionClusterScope, QuestionClusterState, ReplicationPolicy,
    ReportSchedule, ReportWeekday, ResourceMode, StartAcceptanceStatus, Tenant, UpdateProject,
    hash_idempotency_key, next_calendar_week_window, previous_calendar_week_window, settings_hash,
    start_request_hash, validate_operator_project_page,
};
pub use project_ai_settings::*;
pub use tenancy::{OperatorId, ProjectId, TenantId, TenantScope, TenantScopeId};
