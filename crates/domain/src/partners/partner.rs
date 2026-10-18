//! Inbound, project-bound knowledge synchronization. No publication authority.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    AppError, AuthorizedMediaSnapshot, ContentRevision, ErrorCode, KnowledgePurpose,
    MediaObjectKey, TenantScope,
};

pub const PARTNER_SYNC_SCHEMA_VERSION: u16 = 1;
pub const PARTNER_SYNC_MAX_ITEMS: usize = 100;
pub const PARTNER_SYNC_MAX_TEXT_BYTES: usize = 256 * 1024;
pub const PARTNER_SYNC_MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartnerSyncRequest {
    pub schema_version: u16,
    pub items: Vec<PartnerSyncItem>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartnerSyncItem {
    pub external_source_id: String,
    pub external_version: String,
    pub source_sequence: i64,
    pub effective_at: DateTime<Utc>,
    pub action: PartnerSyncAction,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum PartnerSyncAction {
    Upsert {
        name: String,
        purpose: KnowledgePurpose,
        media_type: String,
        text: String,
        content_sha256: String,
    },
    Revoke {},
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerPrincipal {
    pub scope: TenantScope,
    pub connection_id: Uuid,
    pub key_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartnerSyncStatus {
    Pending,
    Running,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartnerDisposition {
    Accepted,
    Unchanged,
    Rejected,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerKnowledgeRefs {
    pub source_id: Option<Uuid>,
    pub source_version_id: Option<Uuid>,
    pub import_job_id: Option<Uuid>,
    pub knowledge_release_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerSyncResult {
    pub index: u32,
    pub disposition: PartnerDisposition,
    pub reason_code: Option<String>,
    pub resources: PartnerKnowledgeRefs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerSyncReceipt {
    pub operation_id: Uuid,
    pub status: PartnerSyncStatus,
    pub item_count: u32,
    pub completed_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerSyncPage {
    pub operation: PartnerSyncReceipt,
    pub results: Vec<PartnerSyncResult>,
    pub next_index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerSyncWork {
    pub scope: TenantScope,
    pub connection_id: Uuid,
    pub operation_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartnerAdvance {
    ItemCommitted {
        index: u32,
        operation_completed: bool,
    },
    Completed,
    Deferred,
}

/// Returned once to a trusted provisioning caller. Deliberately not Debug/Serialize.
pub struct PartnerIssuedKey {
    pub key_id: Uuid,
    pub secret: String,
}

/// Authorized immutable bytes captured before packaging; never serialize the
/// internal revision, which includes source quotes and checking diagnostics.
pub struct PartnerContentExport {
    pub revision: ContentRevision,
    pub language: Option<String>,
    pub media_keys: Vec<MediaObjectKey>,
    pub media: Vec<AuthorizedMediaSnapshot>,
}

impl PartnerIssuedKey {
    pub fn bearer(&self) -> String {
        format!("{}.{}", self.key_id, self.secret)
    }
}

#[async_trait]
pub trait PartnerRepository: Send + Sync {
    async fn authenticate(
        &self,
        connection_id: Uuid,
        key_id: Uuid,
        secret: &str,
    ) -> Result<PartnerPrincipal, AppError>;
    async fn content_export(
        &self,
        _principal: &PartnerPrincipal,
        _revision_id: Uuid,
    ) -> Result<PartnerContentExport, AppError> {
        Err(AppError::capability_missing("content export unavailable"))
    }
    async fn accept(
        &self,
        principal: &PartnerPrincipal,
        idempotency_key: &str,
        request: PartnerSyncRequest,
    ) -> Result<PartnerSyncReceipt, AppError>;
    async fn results(
        &self,
        principal: &PartnerPrincipal,
        operation_id: Uuid,
        after_index: Option<u32>,
        limit: u32,
    ) -> Result<PartnerSyncPage, AppError>;
    async fn candidates(&self, limit: u32) -> Result<Vec<PartnerSyncWork>, AppError>;
    async fn advance_one(&self, work: &PartnerSyncWork) -> Result<PartnerAdvance, AppError>;
}

/// Validate the envelope before durable acceptance; item errors are durable per-item results.
pub fn validate_partner_sync_request(request: &PartnerSyncRequest) -> Result<(), AppError> {
    if request.schema_version != PARTNER_SYNC_SCHEMA_VERSION
        || request.items.is_empty()
        || request.items.len() > PARTNER_SYNC_MAX_ITEMS
    {
        return Err(AppError::invalid_request("invalid knowledge sync envelope"));
    }
    let bytes = serde_json::to_vec(request)
        .map_err(|_| AppError::invalid_request("invalid knowledge sync envelope"))?;
    if bytes.len() > PARTNER_SYNC_MAX_REQUEST_BYTES {
        return Err(AppError::invalid_request(
            "knowledge sync request is too large",
        ));
    }
    // PostgreSQL JSONB cannot represent NUL. Reject the envelope before any
    // write instead of surfacing an infrastructure error for an invalid value.
    if request.items.iter().any(|item| {
        item.external_source_id.contains('\0')
            || item.external_version.contains('\0')
            || match &item.action {
                PartnerSyncAction::Upsert {
                    name,
                    media_type,
                    text,
                    content_sha256,
                    ..
                } => [name, media_type, text, content_sha256]
                    .iter()
                    .any(|value| value.contains('\0')),
                PartnerSyncAction::Revoke {} => false,
            }
    }) {
        return Err(AppError::invalid_request(
            "knowledge sync request contains unsupported characters",
        ));
    }
    Ok(())
}

/// Fixed codes only: identifiers and content must never appear in errors or logs.
pub fn validate_partner_sync_item(item: &PartnerSyncItem) -> Result<(), &'static str> {
    let identifier_valid = |value: &str| {
        !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
    };
    if !identifier_valid(&item.external_source_id)
        || !identifier_valid(&item.external_version)
        || item.source_sequence <= 0
    {
        return Err("invalid_source_identity");
    }
    if let PartnerSyncAction::Upsert {
        name,
        media_type,
        text,
        content_sha256,
        ..
    } = &item.action
    {
        if name.trim().is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
            return Err("invalid_source_name");
        }
        if !matches!(media_type.as_str(), "text/plain" | "text/markdown") {
            return Err("unsupported_media_type");
        }
        if text.trim().is_empty() || text.len() > PARTNER_SYNC_MAX_TEXT_BYTES || text.contains('\0')
        {
            return Err("invalid_source_text");
        }
        if *content_sha256 != hex::encode(Sha256::digest(text.as_bytes())) {
            return Err("content_digest_mismatch");
        }
    }
    Ok(())
}

/// Typed serialization fixes field order, UTC timestamps and defaults; text is byte-exact.
pub fn partner_sync_request_hash(request: &PartnerSyncRequest) -> Result<String, AppError> {
    canonical_hash(request)
}

pub fn partner_sync_item_hash(item: &PartnerSyncItem) -> Result<String, AppError> {
    canonical_hash(item)
}

fn canonical_hash(value: &impl Serialize) -> Result<String, AppError> {
    serde_json::to_vec(value)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .map_err(|_| AppError::new(ErrorCode::Internal, "knowledge sync encoding unavailable"))
}

/// High-entropy service tokens need a digest verifier, not a password KDF.
pub fn issue_partner_key() -> Result<(PartnerIssuedKey, String), AppError> {
    let mut random = [0_u8; 32];
    OsRng.fill_bytes(&mut random);
    let issued = PartnerIssuedKey {
        key_id: Uuid::new_v4(),
        secret: hex::encode(random),
    };
    let hash = hex::encode(Sha256::digest(issued.secret.as_bytes()));
    Ok((issued, hash))
}

pub fn verify_partner_key(secret: &str, verification_hash: &str) -> bool {
    if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let mut expected = [0_u8; 32];
    if hex::decode_to_slice(verification_hash, &mut expected).is_err() {
        return false;
    }
    let actual = Sha256::digest(secret.as_bytes());
    bool::from(actual[..].ct_eq(&expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> PartnerSyncItem {
        PartnerSyncItem {
            external_source_id: "synthetic-source".into(),
            external_version: "opaque-version".into(),
            source_sequence: 1,
            effective_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            action: PartnerSyncAction::Upsert {
                name: "Synthetic".into(),
                purpose: KnowledgePurpose::Public,
                media_type: "text/plain".into(),
                text: "Evidence".into(),
                content_sha256: hex::encode(Sha256::digest(b"Evidence")),
            },
        }
    }

    #[test]
    fn byte_exact_digest_and_semantics() {
        let original = item();
        assert!(validate_partner_sync_item(&original).is_ok());
        let mut changed = original.clone();
        if let PartnerSyncAction::Upsert { text, .. } = &mut changed.action {
            text.push(' ');
        }
        assert_eq!(
            validate_partner_sync_item(&changed),
            Err("content_digest_mismatch")
        );
        changed = original.clone();
        changed.source_sequence += 1;
        assert_ne!(
            partner_sync_item_hash(&original).unwrap(),
            partner_sync_item_hash(&changed).unwrap()
        );
    }

    #[test]
    fn canonical_deserialization_rejects_scope_and_unknown_action_fields() {
        let revoke = serde_json::json!({"operation": "revoke"});
        let decoded: PartnerSyncAction = serde_json::from_value(revoke.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), revoke);
        let original = item();
        let mut value = serde_json::to_value(&original).unwrap();
        value["effective_at"] = serde_json::json!("2026-01-01T08:00:00+08:00");
        let equivalent: PartnerSyncItem = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            partner_sync_item_hash(&original).unwrap(),
            partner_sync_item_hash(&equivalent).unwrap()
        );
        value["operator_id"] = serde_json::json!(Uuid::new_v4());
        assert!(serde_json::from_value::<PartnerSyncItem>(value).is_err());
        assert!(
            serde_json::from_value::<PartnerSyncAction>(serde_json::json!({
                "operation": "revoke", "text": "not permitted"
            }))
            .is_err()
        );
    }

    #[test]
    fn envelope_and_item_bounds() {
        assert!(
            validate_partner_sync_request(&PartnerSyncRequest {
                schema_version: 1,
                items: vec![]
            })
            .is_err()
        );
        let mut invalid = item();
        invalid.source_sequence = 0;
        assert_eq!(
            validate_partner_sync_item(&invalid),
            Err("invalid_source_identity")
        );
    }

    #[test]
    fn key_verification_and_randomness() {
        let (first, hash) = issue_partner_key().unwrap();
        let (second, _) = issue_partner_key().unwrap();
        assert_ne!(first.secret, second.secret);
        assert!(verify_partner_key(&first.secret, &hash));
        assert!(!verify_partner_key(&second.secret, &hash));
        assert!(!verify_partner_key("", &hash));
    }
}
