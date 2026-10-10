//! Transport evidence and domain-independent SERP adapter values.
//! Persist raw responses before invoking a decoder. A correlation tag is never
//! an upstream idempotency guarantee, and no response authorizes a paid retry.
use std::fmt;

use serde::{Deserialize, Serialize};

pub const MAX_SERP_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerpOperation {
    PostTask,
    GetTask,
    TasksReady,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerpSentCertainty {
    NotSent,
    PossiblySent,
    ResponseReceived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerpTransportError {
    InvalidRequest,
    Timeout,
    TransportFailed,
    BodyReadFailed,
    BodyTooLarge,
}

/// Controlled evidence, deliberately not printable through derived Debug.
#[derive(Clone, PartialEq, Eq)]
pub struct SerpRawResponse {
    pub operation: SerpOperation,
    pub http_status: Option<u16>,
    pub body: Vec<u8>,
    pub body_complete: bool,
    pub sent: SerpSentCertainty,
    pub error: Option<SerpTransportError>,
}

impl fmt::Debug for SerpRawResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SerpRawResponse")
            .field("operation", &self.operation)
            .field("http_status", &self.http_status)
            .field("body_bytes", &self.body.len())
            .field("body_complete", &self.body_complete)
            .field("sent", &self.sent)
            .field("error", &self.error)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SerpAdapterError {
    #[error("invalid SERP adapter configuration")]
    InvalidConfiguration,
    #[error("SERP response is incomplete")]
    IncompleteResponse,
    #[error("SERP HTTP response was not successful")]
    HttpRejected,
    #[error("SERP response JSON is invalid")]
    InvalidJson,
    #[error("SERP response envelope is invalid")]
    InvalidEnvelope,
    #[error("SERP response operation differs")]
    OperationMismatch,
    #[error("SERP task identity differs")]
    TaskIdentityMismatch,
    #[error("SERP task status is not accepted")]
    TaskStatusRejected,
    #[error("SERP result is invalid")]
    InvalidResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerpItemKind {
    Organic,
    Paid,
    FeaturedSnippet,
    LocalPack,
    AiOverview,
    Other,
}

/// Only organic rows carry organic_rank. Duplicate URLs and unknown item kinds
/// retain their original rows and JSON-pointer evidence locations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpProviderItem {
    pub kind: SerpItemKind,
    pub raw_kind: String,
    pub raw_url: Option<String>,
    pub title: Option<String>,
    pub page: Option<u32>,
    pub organic_rank: Option<u32>,
    pub absolute_position: Option<u32>,
    pub evidence_pointer: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerpProviderResult {
    /// Metadata returned by the provider, not a locally verified capture time.
    pub provider_reported_datetime: Option<String>,
    /// Request echoes do not prove actual location, language, device or login.
    pub request_echo: serde_json::Value,
    pub provider_reported_pages: Option<u32>,
    pub provider_reported_items: Option<u32>,
    pub requested_depth: u32,
    pub returned_organic_count: usize,
    pub max_returned_organic_rank: Option<u32>,
    /// This adapter never infers complete requested-depth coverage.
    pub coverage_verified: bool,
    pub items: Vec<SerpProviderItem>,
}
