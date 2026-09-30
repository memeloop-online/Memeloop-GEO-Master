//! Isolated Rust-hosted JavaScript worker.
//!
//! The crate answers two questions.  First, can the chosen embedded engine load
//! a MemeLoop-shaped bundle and satisfy the loop's runtime requirements (ESM,
//! Promise, host ops, timeout, cancellation, memory limits and checkpoint
//! serialisation)?  That is the W00 compatibility probe, [`ProbeRuntime`], with
//! one test per concern.  Second, how does a tenant script reach the product's
//! real capabilities?  That is the production host-op surface in [`host`],
//! registered by [`HostRuntime`].
//!
//! Boundary rules encoded here, per the product spec:
//!
//! - JavaScript reaches Rust only through the versioned ops declared in
//!   [`host::HostOp`] and registered by [`HostRuntime`].  There is no op that
//!   grants SQL, arbitrary network, filesystem, process or environment-variable
//!   access, and no op that synthesises a result when a capability is missing.
//! - The op bodies delegate to the [`host::HostOps`] trait, which the API
//!   process implements over its own repositories and provider bridge.  The
//!   dependency therefore runs `geo-api -> geo-worker`, never the reverse.
//! - Modules are only those seeded into [`InMemoryModuleLoader`]; there is no
//!   filesystem, network or package-registry resolution.
//! - Rust owns the state that survives a restart, so checkpoints are
//!   serialised host state rather than opaque engine snapshots.
//! - The probe's deterministic model stub is registered only on
//!   [`ProbeRuntime`].  A production run has no path to a fabricated answer.

pub mod bundle;
pub mod host;
mod host_ops;
mod host_runtime;
pub mod loader;
pub mod ops;
pub mod runtime;

pub use bundle::{
    GEO_LOOP_JS, HOST_BUNDLE, HOST_LOOP_JS, HOST_MAIN_MODULE, HOST_OPS_JS, LOOP_CORE_JS,
    MAIN_MODULE, PROBE_BUNDLE, TURN_COMPLETION_TOPIC,
};
pub use host::{
    HOST_OP_ERROR_BOOTSTRAP, HOST_OP_ERROR_NAME, HOST_OPS_VERSION, HostBridge, HostOp,
    HostOpBudgets, HostOpError, HostOpErrorCode, HostOpLimits, HostOpMeter, HostOps,
    KnowledgeSearchRequest, KnowledgeSearchResult, ManifestItem, ManifestKind, ManifestPage,
    ManifestReadRequest, MeasureRequest, MeasureSample, ModelCompletion, ModelCompletionRequest,
    PublishReceipt, PublishRequest, PublishState, TenantScope, redact_secrets,
};
pub use host_runtime::HostRuntime;
pub use loader::InMemoryModuleLoader;
pub use ops::{HostEvent, HostState};
pub use runtime::{EmbeddedIsolate, ProbeRuntime, WorkerError};
