//! Project-scoped administration metadata, separate from bearer authentication.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppError, TenantScope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerAdminConnection {
    pub connection_id: Uuid,
    pub knowledge_write: bool,
    pub content_export: bool,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerAdminKey {
    pub key_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerAdminConnectionPage {
    pub items: Vec<PartnerAdminConnection>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerAdminKeyPage {
    pub items: Vec<PartnerAdminKey>,
    pub next_cursor: Option<Uuid>,
}

/// One-time provisioning result. Never log or persist the bearer in a client.
/// Deliberately not Debug/Serialize; an authorized HTTP boundary must opt in.
pub struct PartnerAdminIssuedKey {
    pub key: PartnerAdminKey,
    pub bearer: String,
}

/// Callers must authorize project administration before invoking this interface.
/// Lists include revoked records, use ascending UUID order, and accept limits 1..=100.
#[async_trait]
pub trait PartnerAdminRepository: Send + Sync {
    async fn list_connections(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<PartnerAdminConnectionPage, AppError>;
    async fn create_connection(
        &self,
        scope: &TenantScope,
    ) -> Result<PartnerAdminConnection, AppError>;
    async fn set_content_export(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        enabled: bool,
    ) -> Result<PartnerAdminConnection, AppError>;
    async fn list_keys(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<PartnerAdminKeyPage, AppError>;
    async fn issue_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<PartnerAdminIssuedKey, AppError>;
    async fn rotate_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        old_key: Uuid,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<PartnerAdminIssuedKey, AppError>;
    async fn revoke_key(
        &self,
        scope: &TenantScope,
        connection: Uuid,
        key: Uuid,
    ) -> Result<(), AppError>;
    async fn revoke_connection(
        &self,
        scope: &TenantScope,
        connection: Uuid,
    ) -> Result<(), AppError>;
}
