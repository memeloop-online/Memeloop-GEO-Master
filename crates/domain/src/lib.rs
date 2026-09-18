//! Domain contracts shared by the HTTP API and background roles.
//!
//! The types in this crate deliberately do not depend on a database or an HTTP
//! framework.  They are the stable boundary used by the modular monolith.

mod auth;
mod error;
mod event;
mod idempotency;
mod operation;
mod project;
mod tenancy;

pub use auth::{
    AuthRepository, DEFAULT_SESSION_TTL_SECS, DEVELOPMENT_USER_EMAIL, LoginIdentity, Membership,
    MemoryAuthRepository, Role, Session, SessionCredentials, SessionId, User, UserId, hash_token,
    normalize_email, normalize_host,
};
pub use error::{AppError, ErrorCode};
pub use event::EventEnvelope;
pub use idempotency::{IdempotencyDecision, IdempotencyStore, IdempotencyToken, StoredResponse};
pub use operation::{Operation, OperationStatus};
pub use project::{
    CreateProject, DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_PROJECT_ID, DEVELOPMENT_TENANT_ID,
    InitialSource, InitialSourceKind, InitialSourceVisibility, MemoryProjectRepository, Operator,
    Project, ProjectCreate, ProjectOverview, ProjectPage, ProjectPatch, ProjectRepository,
    ProjectSettings, ProjectStatus, ResourceMode, Tenant, UpdateProject,
};
pub use tenancy::{OperatorId, ProjectId, TenantId, TenantScope, TenantScopeId};
