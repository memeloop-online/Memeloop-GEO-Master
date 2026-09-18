//! Domain contracts shared by the HTTP API and background roles.
//!
//! The types in this crate deliberately do not depend on a database or an HTTP
//! framework.  They are the stable boundary used by the modular monolith.

mod error;
mod event;
mod operation;
mod tenancy;

pub use error::{AppError, ErrorCode};
pub use event::EventEnvelope;
pub use operation::{Operation, OperationStatus};
pub use tenancy::{OperatorId, ProjectId, TenantId, TenantScope, TenantScopeId};
