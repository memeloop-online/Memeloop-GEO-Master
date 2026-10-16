//! Durable operator-only onboarding results. Secrets are never public projections.
use crate::ChannelSecret;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelImportStatus {
    NeedsLogin,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelImportRow {
    pub row_key: String,
    pub ordinal: u32,
    pub account_id: Option<Uuid>,
    pub status: ChannelImportStatus,
    pub error_code: Option<String>,
    pub masked_username: Option<String>,
    pub display_alias: Option<String>,
}

/// The envelope contains canonical input, not a verified browser session.
/// `secret_id` binds the envelope to its prospective account through AEAD AAD.
#[derive(Clone)]
pub struct ChannelImportRecord {
    pub row: ChannelImportRow,
    pub secret_id: Uuid,
    pub encrypted_input: ChannelSecret,
}
