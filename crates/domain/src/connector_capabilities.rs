//! Operator-wide connector availability. Account login is not publication proof.
use crate::{
    AppError, ChannelAttempt, ChannelOutcome, ChannelOutcomeStatus, ChannelTarget,
    ChannelTargetInput, OperatorId, RICH_MARKDOWN_FORMAT,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectorKey {
    pub platform_id: String,
    pub placement_slot: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorSettings {
    pub key: ConnectorKey,
    pub revision: i32,
    pub enabled: bool,
    /// Operator-chosen subset of independently proven document formats.
    pub content_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorVerification {
    pub verification_id: Uuid,
    pub key: ConnectorKey,
    pub connector_version: String,
    pub content_type: String,
    pub publication_receipt: ChannelOutcome,
    pub public_readback: ChannelOutcome,
    pub verified_at: DateTime<Utc>,
}

/// The actual wire representation of both a frozen source article and a
/// markdown-based generated variant. Document semantics are not wire formats.
pub const PLAIN_TEXT_ARTICLE_FORMAT: &str = "plain_text_article.v1";

pub fn publication_format_for_semantic_type(content_type: &str) -> Option<&'static str> {
    match content_type {
        "faq" | "guide" | "article" | "comparison" | "case_study" | "landing_page"
        | "product_page" | "how_to" | "company_profile" => Some(PLAIN_TEXT_ARTICLE_FORMAT),
        _ => None,
    }
}

/// Hash of the exact normalized title/body submitted to the plain text runner.
pub fn plain_text_article_readback_hash(title: &str, body: &str) -> String {
    let normalized = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    hex::encode(Sha256::digest(
        format!("{}\n{}", normalized(title), normalized(body)).as_bytes(),
    ))
}

/// SQLx/chrono encodes PostgreSQL `timestamptz` with truncated microseconds;
/// keep exact Rust receipt-marker comparison, but use persisted precision
/// for claimed/observed/received ordering and idempotent database replay.
pub fn publication_storage_timestamp(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_micros(at.timestamp_micros())
        .expect("valid persisted event timestamp")
}

/// Pure portion of the trusted projection; the caller MUST first load and
/// cross-check the scoped saved target, attempt, account and source/intent.
pub fn saved_publication_verification(
    target: &ChannelTarget,
    attempt: &ChannelAttempt,
) -> Result<Option<ConnectorVerification>, AppError> {
    // A plain title/body readback does not prove the structure or images of a
    // rich publication. A separate versioned rich readback proof is required.
    if matches!(
        &target.input,
        ChannelTargetInput::GeneratedPublish {
            rich_payload: Some(_),
            ..
        }
    ) {
        return Ok(None);
    }
    let (platform, title, body, frozen_hash) = match &target.input {
        ChannelTargetInput::Publish {
            platform,
            title,
            body,
            body_sha256,
            ..
        }
        | ChannelTargetInput::GeneratedPublish {
            platform,
            title,
            body,
            body_sha256,
            ..
        } => (platform, title, body, body_sha256),
        ChannelTargetInput::Measure { .. } => return Ok(None),
    };
    let Some(outcome) = attempt.outcome.as_ref() else {
        return Ok(None);
    };
    let Some(received_at) = attempt.received_at else {
        return Ok(None);
    };
    if attempt.target_id != target.target_id
        || outcome.fixture
        || outcome.status != ChannelOutcomeStatus::Verified
        || publication_storage_timestamp(attempt.claimed_at)
            > publication_storage_timestamp(outcome.occurred_at)
        || publication_storage_timestamp(outcome.occurred_at)
            > publication_storage_timestamp(received_at)
        || !valid_label(platform)
        || title.trim().is_empty()
        || body.trim().is_empty()
        || hex::encode(Sha256::digest(body.as_bytes())) != *frozen_hash
    {
        return Ok(None);
    }
    let Some(version) = outcome.connector_version.as_deref() else {
        return Ok(None);
    };
    if !valid_label(version) || version.to_ascii_lowercase().contains("fixture") {
        return Ok(None);
    }
    // The marker is appended by Rust after the runner reply has been checked;
    // a legacy false fixture flag alone is not evidence of a live execution.
    let markers: Vec<_> = outcome
        .runner_evidence
        .iter()
        .filter(|proof| proof.get("kind").and_then(|v| v.as_str()) == Some("runner_receipt"))
        .collect();
    if markers.len() != 1
        || !markers.iter().any(|proof| {
            proof.get("schema_version").and_then(|v| v.as_str()) == Some("geo.runner.receipt.v1")
                && proof.get("provenance").and_then(|v| v.as_str()) == Some("live")
                && proof.get("execution_id").and_then(|v| v.as_str())
                    == Some(attempt.attempt_id.to_string().as_str())
                && proof.get("connector_version").and_then(|v| v.as_str()) == Some(version)
                && proof
                    .get("occurred_at")
                    .and_then(|v| v.as_str())
                    .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
                    .is_some_and(|time| time.with_timezone(&Utc) == outcome.occurred_at)
        })
    {
        return Ok(None);
    }
    let hash = plain_text_article_readback_hash(title, body);
    let readback: Vec<_> = outcome
        .runner_evidence
        .iter()
        .filter(|proof| {
            proof.get("kind").and_then(|v| v.as_str()) == Some("public_readback")
                && proof.get("url").and_then(|v| v.as_str()) == outcome.public_url.as_deref()
                && proof.get("content_matched").and_then(|v| v.as_bool()) == Some(true)
                && proof.get("owned_by_account").and_then(|v| v.as_bool()) == Some(true)
                && proof.get("expected_sha256").and_then(|v| v.as_str()) == Some(hash.as_str())
                && proof.get("readback_sha256").and_then(|v| v.as_str()) == Some(hash.as_str())
        })
        .collect();
    if readback.len() != 1
        || !outcome.public_url.as_deref().is_some_and(|raw| {
            Url::parse(raw).is_ok_and(|url| {
                let zhihu_post = url
                    .path()
                    .strip_prefix("/p/")
                    .unwrap_or_default()
                    .trim_end_matches('/');
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
                    && (platform != "zhihu"
                        || (matches!(url.host_str(), Some("www.zhihu.com" | "zhuanlan.zhihu.com"))
                            && !zhihu_post.is_empty()
                            && zhihu_post.bytes().all(|byte| byte.is_ascii_digit())
                            && url.query().is_none()))
            })
        })
    {
        return Ok(None);
    }
    let id_hash = Sha256::digest(
        format!(
            "connector-saved-attempt:{}:{PLAIN_TEXT_ARTICLE_FORMAT}",
            attempt.attempt_id
        )
        .as_bytes(),
    );
    let mut id_bytes = [0_u8; 16];
    id_bytes.copy_from_slice(&id_hash[..16]);
    id_bytes[6] = (id_bytes[6] & 0x0f) | 0x50;
    id_bytes[8] = (id_bytes[8] & 0x3f) | 0x80;
    let verification = ConnectorVerification {
        verification_id: Uuid::from_bytes(id_bytes),
        key: ConnectorKey {
            platform_id: platform.clone(),
            placement_slot: "primary".into(),
        },
        connector_version: version.into(),
        content_type: PLAIN_TEXT_ARTICLE_FORMAT.into(),
        publication_receipt: outcome.clone(),
        public_readback: outcome.clone(),
        verified_at: outcome.occurred_at,
    };
    verification.validate_saved()?;
    Ok(Some(verification))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAvailability {
    Unavailable,
    Disabled,
    VersionMismatch,
    UnsupportedContentType,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorResolution {
    pub availability: ConnectorAvailability,
    pub settings: Option<ConnectorSettings>,
    pub verification_id: Option<Uuid>,
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 128
        && label.trim() == label
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/+".contains(&byte))
}

impl ConnectorKey {
    pub fn validate(&self) -> Result<(), AppError> {
        if !valid_label(&self.platform_id) || !valid_label(&self.placement_slot) {
            return Err(AppError::invalid_request("invalid connector key"));
        }
        Ok(())
    }
}

fn trusted_version(version: &str) -> bool {
    valid_label(version)
        && !version.to_ascii_lowercase().contains("fixture")
        && !version.to_ascii_lowercase().contains("unverified")
        && !version.to_ascii_lowercase().contains("source_derived")
}

impl ConnectorVerification {
    /// Only call from a trusted Rust publication adapter after binding the saved
    /// attempt's account, platform and placement to this key and confirming the
    /// live external receipt. Never synthesize this from login, checkbox, or
    /// client-provided input. This validates provenance and content readback,
    /// but cannot itself establish that the caller contacted the platform.
    pub fn validate(&self) -> Result<(), AppError> {
        self.validate_inner(true)
    }

    /// Only saved attempts with a separately verified Rust-owned live marker.
    pub fn validate_saved(&self) -> Result<(), AppError> {
        self.validate_inner(false)
    }

    fn validate_inner(&self, legacy_version_policy: bool) -> Result<(), AppError> {
        self.key.validate()?;
        // Current proofs attest to title/body only. Until a typed anonymous
        // structure+image readback contract exists, even a trusted caller
        // cannot turn that text-only marker into a rich-format capability.
        if self.content_type == RICH_MARKDOWN_FORMAT {
            return Err(AppError::invalid_request(
                "rich publication requires structured and media readback proof",
            ));
        }
        if self.verification_id.is_nil()
            || (legacy_version_policy && !trusted_version(&self.connector_version))
            || (!legacy_version_policy
                && (!valid_label(&self.connector_version)
                    || self
                        .connector_version
                        .to_ascii_lowercase()
                        .contains("fixture")))
            || !valid_label(&self.content_type)
            || self.publication_receipt.fixture
            || self.public_readback.fixture
            || !matches!(
                self.publication_receipt.status,
                ChannelOutcomeStatus::Published | ChannelOutcomeStatus::Verified
            )
            || self.public_readback.status != ChannelOutcomeStatus::Verified
            || self.public_readback.occurred_at < self.publication_receipt.occurred_at
            || self.verified_at < self.public_readback.occurred_at
            || self.publication_receipt.connector_version.as_deref()
                != Some(&self.connector_version)
            || self.public_readback.connector_version.as_deref() != Some(&self.connector_version)
        {
            return Err(AppError::invalid_request(
                "trusted live publication and readback required",
            ));
        }
        let receipt = self
            .publication_receipt
            .public_url
            .as_deref()
            .filter(|url| {
                Url::parse(url).is_ok_and(|parsed| {
                    parsed.scheme() == "https"
                        && parsed.host_str().is_some()
                        && parsed.username().is_empty()
                        && parsed.password().is_none()
                        && parsed.fragment().is_none()
                })
            });
        let readback = self.public_readback.public_url.as_deref();
        if receipt.is_none()
            || receipt != readback
            || !self.public_readback.runner_evidence.iter().any(|proof| {
                let expected = proof.get("expected_sha256").and_then(|v| v.as_str());
                proof.get("kind").and_then(|v| v.as_str()) == Some("public_readback")
                    && proof.get("url").and_then(|v| v.as_str()) == readback
                    && proof.get("content_matched").and_then(|v| v.as_bool()) == Some(true)
                    && proof.get("owned_by_account").and_then(|v| v.as_bool()) == Some(true)
                    && expected.is_some_and(|hash| {
                        hash.len() == 64
                            && hash.bytes().all(|b| b.is_ascii_hexdigit())
                            && proof.get("readback_sha256").and_then(|v| v.as_str()) == Some(hash)
                    })
            })
        {
            return Err(AppError::invalid_request(
                "matching owned public readback required",
            ));
        }
        Ok(())
    }
}

pub fn resolve_connector(
    settings: Option<ConnectorSettings>,
    verifications: &[ConnectorVerification],
    deployed_version: &str,
    content_type: &str,
) -> ConnectorResolution {
    let mut result = ConnectorResolution {
        availability: ConnectorAvailability::Unavailable,
        settings,
        verification_id: None,
    };
    let Some(ref current) = result.settings else {
        return result;
    };
    if content_type == RICH_MARKDOWN_FORMAT {
        result.availability = ConnectorAvailability::UnsupportedContentType;
        return result;
    }
    if !current.enabled {
        result.availability = ConnectorAvailability::Disabled;
    } else if !valid_label(deployed_version)
        || deployed_version.to_ascii_lowercase().contains("fixture")
        || !verifications
            .iter()
            .any(|v| v.connector_version == deployed_version)
    {
        result.availability = ConnectorAvailability::VersionMismatch;
    } else if !current
        .content_types
        .iter()
        .any(|kind| kind == content_type)
    {
        result.availability = ConnectorAvailability::UnsupportedContentType;
    } else if let Some(proof) = verifications
        .iter()
        .filter(|v| v.connector_version == deployed_version && v.content_type == content_type)
        .max_by_key(|v| (v.verified_at, v.verification_id))
    {
        result.availability = ConnectorAvailability::Available;
        result.verification_id = Some(proof.verification_id);
    } else {
        result.availability = ConnectorAvailability::UnsupportedContentType;
    }
    result
}

pub fn validate_connector_settings(
    enabled: bool,
    content_types: &[String],
    verifications: &[ConnectorVerification],
    deployed_version: &str,
) -> Result<(), AppError> {
    let unique: HashSet<_> = content_types.iter().collect();
    if unique.len() != content_types.len() || content_types.iter().any(|kind| !valid_label(kind)) {
        return Err(AppError::invalid_request("invalid connector content types"));
    }
    if enabled
        && (content_types
            .iter()
            .any(|kind| kind == RICH_MARKDOWN_FORMAT)
            || content_types.is_empty()
            || !valid_label(deployed_version)
            || deployed_version.to_ascii_lowercase().contains("fixture")
            || content_types.iter().any(|kind| {
                !verifications.iter().any(|v| {
                    v.connector_version == deployed_version
                        && &v.content_type == kind
                        && v.validate().is_ok()
                })
            }))
    {
        return Err(AppError::invalid_request(
            "each enabled content type requires live verification for the deployed version",
        ));
    }
    Ok(())
}

#[async_trait]
pub trait ConnectorCapabilityRepository: Send + Sync {
    async fn get(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
    ) -> Result<Option<ConnectorSettings>, AppError>;
    async fn list(&self, operator: OperatorId) -> Result<Vec<ConnectorSettings>, AppError>;
    async fn history(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
    ) -> Result<Vec<ConnectorVerification>, AppError>;
    /// expected_revision=0 creates; otherwise atomically compare and increment.
    /// Disabling remains possible after a connector has been undeployed.
    async fn configure(
        &self,
        operator: OperatorId,
        key: ConnectorKey,
        expected_revision: i32,
        enabled: bool,
        content_types: Vec<String>,
        deployed_version: &str,
    ) -> Result<ConnectorSettings, AppError>;
    /// Trusted Rust adapter only; must never be reachable from operator checkbox or login.
    async fn insert_verification(
        &self,
        operator: OperatorId,
        proof: ConnectorVerification,
    ) -> Result<(), AppError>;
    async fn resolve(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
        deployed_version: &str,
        content_type: &str,
    ) -> Result<ConnectorResolution, AppError> {
        let settings = self.get(operator, key).await?;
        let history = self.history(operator, key).await?;
        Ok(resolve_connector(
            settings,
            &history,
            deployed_version,
            content_type,
        ))
    }
}

#[derive(Default, Clone)]
pub struct MemoryConnectorCapabilityRepository {
    state: Arc<Mutex<MemoryConnectorState>>,
}

#[derive(Default)]
struct MemoryConnectorState {
    settings: HashMap<(OperatorId, ConnectorKey), ConnectorSettings>,
    records: HashMap<(OperatorId, ConnectorKey), Vec<ConnectorVerification>>,
    ids: HashSet<Uuid>,
}

#[async_trait]
impl ConnectorCapabilityRepository for MemoryConnectorCapabilityRepository {
    async fn get(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
    ) -> Result<Option<ConnectorSettings>, AppError> {
        key.validate()?;
        Ok(self
            .state
            .lock()
            .await
            .settings
            .get(&(operator, key.clone()))
            .cloned())
    }
    async fn list(&self, operator: OperatorId) -> Result<Vec<ConnectorSettings>, AppError> {
        let state = self.state.lock().await;
        let mut rows: Vec<_> = state
            .settings
            .iter()
            .filter(|((owner, _), _)| *owner == operator)
            .map(|(_, settings)| settings.clone())
            .collect();
        rows.sort_by(|a, b| {
            (&a.key.platform_id, &a.key.placement_slot)
                .cmp(&(&b.key.platform_id, &b.key.placement_slot))
        });
        Ok(rows)
    }
    async fn history(
        &self,
        operator: OperatorId,
        key: &ConnectorKey,
    ) -> Result<Vec<ConnectorVerification>, AppError> {
        key.validate()?;
        Ok(self
            .state
            .lock()
            .await
            .records
            .get(&(operator, key.clone()))
            .cloned()
            .unwrap_or_default())
    }
    async fn configure(
        &self,
        operator: OperatorId,
        key: ConnectorKey,
        expected_revision: i32,
        enabled: bool,
        content_types: Vec<String>,
        deployed_version: &str,
    ) -> Result<ConnectorSettings, AppError> {
        key.validate()?;
        let mut state = self.state.lock().await;
        let previous = state
            .settings
            .get(&(operator, key.clone()))
            .map(|s| s.revision)
            .unwrap_or(0);
        if expected_revision < 0 || expected_revision == i32::MAX || previous != expected_revision {
            return Err(AppError::conflict("connector settings revision changed"));
        }
        validate_connector_settings(
            enabled,
            &content_types,
            state
                .records
                .get(&(operator, key.clone()))
                .map(Vec::as_slice)
                .unwrap_or_default(),
            deployed_version,
        )?;
        let settings = ConnectorSettings {
            key: key.clone(),
            revision: previous + 1,
            enabled,
            content_types,
        };
        state.settings.insert((operator, key), settings.clone());
        Ok(settings)
    }
    async fn insert_verification(
        &self,
        operator: OperatorId,
        proof: ConnectorVerification,
    ) -> Result<(), AppError> {
        proof.validate()?;
        let mut state = self.state.lock().await;
        if !state.ids.insert(proof.verification_id) {
            return Err(AppError::conflict("verification identity already exists"));
        }
        state
            .records
            .entry((operator, proof.key.clone()))
            .or_default()
            .push(proof);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn evidence() -> ConnectorVerification {
        let now = Utc::now();
        let url = "https://example.com/posts/100".to_owned();
        let hash = "b".repeat(64);
        let publication_receipt = ChannelOutcome {
            status: ChannelOutcomeStatus::Published,
            detail: None,
            occurred_at: now,
            raw_answer: None,
            citations: vec![],
            public_url: Some(url.clone()),
            screenshot_ref: None,
            connector_version: Some("live.v1".into()),
            runner_evidence: vec![],
            fixture: false,
        };
        let public_readback = ChannelOutcome {
            status: ChannelOutcomeStatus::Verified,
            detail: None,
            runner_evidence: vec![serde_json::json!({
                "kind":"public_readback","url":url,"content_matched":true,
                "owned_by_account":true,"expected_sha256":hash,"readback_sha256":hash
            })],
            ..publication_receipt.clone()
        };
        ConnectorVerification {
            verification_id: Uuid::new_v4(),
            key: ConnectorKey {
                platform_id: "creator".into(),
                placement_slot: "primary".into(),
            },
            connector_version: "live.v1".into(),
            content_type: "article".into(),
            publication_receipt,
            public_readback,
            verified_at: now,
        }
    }

    #[test]
    fn trusted_verification_rejects_fixture_missing_readback_and_unverified_connector() {
        let mut record = evidence();
        record.validate().unwrap();
        record.publication_receipt = record.public_readback.clone();
        record.validate().unwrap(); // A persisted send/readback can be one combined outcome.
        record = evidence();
        record.public_readback.runner_evidence.clear();
        assert!(record.validate().is_err());
        record = evidence();
        record.publication_receipt.fixture = true;
        assert!(record.validate().is_err());
        record = evidence();
        record.connector_version = "live_unverified.source_derived.v1".into();
        record.publication_receipt.connector_version = Some(record.connector_version.clone());
        record.public_readback.connector_version = Some(record.connector_version.clone());
        assert!(record.validate().is_err());
        record = evidence();
        record.public_readback.public_url = Some("https://example.com/posts/other".into());
        assert!(record.validate().is_err());
        record = evidence();
        record.public_readback.runner_evidence[0]["owned_by_account"] = serde_json::json!(false);
        assert!(record.validate().is_err());
        record = evidence();
        record.public_readback.runner_evidence[0]
            .as_object_mut()
            .unwrap()
            .remove("expected_sha256");
        record.public_readback.runner_evidence[0]
            .as_object_mut()
            .unwrap()
            .remove("readback_sha256");
        assert!(record.validate().is_err());
        record = evidence();
        record.public_readback.public_url =
            Some("https://user:password@example.com/posts/100".into());
        record.publication_receipt.public_url = record.public_readback.public_url.clone();
        assert!(record.validate().is_err());
        record = evidence();
        record.public_readback.occurred_at =
            record.publication_receipt.occurred_at - chrono::Duration::seconds(1);
        assert!(record.validate().is_err());
    }

    #[test]
    fn title_body_only_readback_cannot_create_rich_publication_capability() {
        let mut record = evidence();
        record.content_type = RICH_MARKDOWN_FORMAT.into();
        assert!(record.validate().is_err());
        assert!(record.validate_saved().is_err());
        let configured = ConnectorSettings {
            key: record.key.clone(),
            revision: 1,
            enabled: true,
            content_types: vec![RICH_MARKDOWN_FORMAT.into()],
        };
        assert_eq!(
            resolve_connector(Some(configured), &[record], "live.v1", RICH_MARKDOWN_FORMAT)
                .availability,
            ConnectorAvailability::UnsupportedContentType
        );
    }

    #[tokio::test]
    async fn memory_registry_is_operator_scoped_revisioned_and_fail_closed() {
        let repo = MemoryConnectorCapabilityRepository::default();
        let operator: OperatorId = Uuid::new_v4().into();
        let other: OperatorId = Uuid::new_v4().into();
        let proof = evidence();
        let key = proof.key.clone();
        assert_eq!(
            repo.resolve(operator, &key, "live.v1", "article")
                .await
                .unwrap()
                .availability,
            ConnectorAvailability::Unavailable
        );
        assert!(
            repo.configure(
                operator,
                key.clone(),
                0,
                true,
                vec!["article".into()],
                "live.v1"
            )
            .await
            .is_err()
        );
        repo.insert_verification(operator, proof.clone())
            .await
            .unwrap();
        assert!(repo.insert_verification(operator, proof).await.is_err());
        assert!(
            repo.configure(
                other,
                key.clone(),
                0,
                true,
                vec!["article".into()],
                "live.v1"
            )
            .await
            .is_err()
        );
        assert!(
            repo.configure(
                operator,
                key.clone(),
                0,
                true,
                vec!["image".into()],
                "live.v1"
            )
            .await
            .is_err()
        );
        repo.configure(
            operator,
            key.clone(),
            0,
            true,
            vec!["article".into()],
            "live.v1",
        )
        .await
        .unwrap();
        assert_eq!(
            repo.resolve(operator, &key, "live.v1", "article")
                .await
                .unwrap()
                .availability,
            ConnectorAvailability::Available
        );
        assert_eq!(
            repo.resolve(operator, &key, "live.v2", "article")
                .await
                .unwrap()
                .availability,
            ConnectorAvailability::VersionMismatch
        );
        assert_eq!(
            repo.resolve(operator, &key, "live.v1", "image")
                .await
                .unwrap()
                .availability,
            ConnectorAvailability::UnsupportedContentType
        );
        assert_eq!(
            repo.resolve(other, &key, "live.v1", "article")
                .await
                .unwrap()
                .availability,
            ConnectorAvailability::Unavailable
        );
        assert_eq!(
            repo.configure(operator, key.clone(), 0, false, vec![], "live.v2")
                .await
                .unwrap_err()
                .code,
            crate::ErrorCode::Conflict
        );
        let disabled = repo
            .configure(operator, key.clone(), 1, false, vec![], "live.v2")
            .await
            .unwrap();
        assert_eq!(disabled.revision, 2);
        assert_eq!(
            repo.resolve(operator, &key, "live.v1", "article")
                .await
                .unwrap()
                .availability,
            ConnectorAvailability::Disabled
        );
        assert_eq!(repo.history(operator, &key).await.unwrap().len(), 1);
    }

    #[test]
    fn saved_attempt_requires_rust_live_receipt_exact_payload_and_event_times() {
        let claimed_at = Utc::now();
        let target = ChannelTarget {
            target_id: Uuid::new_v4(),
            input: ChannelTargetInput::Publish {
                source_id: Uuid::new_v4(),
                source_version_id: Uuid::new_v4(),
                platform: "creator".into(),
                account_id: Uuid::new_v4(),
                title: "Title".into(),
                body: "Body content".into(),
                body_sha256: hex::encode(Sha256::digest(b"Body content")),
            },
        };
        let attempt_id = Uuid::new_v4();
        let version = "source_derived.unverified.v1";
        let url = "https://example.com/posts/123";
        let hash = plain_text_article_readback_hash("Title", "Body content");
        let outcome = ChannelOutcome {
            status: ChannelOutcomeStatus::Verified,
            detail: None,
            occurred_at: claimed_at,
            raw_answer: None,
            citations: vec![],
            public_url: Some(url.into()),
            screenshot_ref: None,
            connector_version: Some(version.into()),
            fixture: false,
            runner_evidence: vec![serde_json::json!({
                "kind":"public_readback","url":url,"content_matched":true,
                "owned_by_account":true,"expected_sha256":hash,"readback_sha256":hash
            })],
        };
        let mut attempt = ChannelAttempt {
            attempt_id,
            target_id: target.target_id,
            claimed_at,
            outcome: Some(outcome),
            received_at: Some(claimed_at),
        };
        assert!(
            saved_publication_verification(&target, &attempt)
                .unwrap()
                .is_none(),
            "a legacy fixture:false flag alone cannot promote a connector"
        );
        attempt
            .outcome
            .as_mut()
            .unwrap()
            .runner_evidence
            .push(serde_json::json!({
                "kind":"runner_receipt","schema_version":"geo.runner.receipt.v1",
                "provenance":"live","execution_id":attempt_id,
                "connector_version":version,"occurred_at":claimed_at
            }));
        let proof = saved_publication_verification(&target, &attempt)
            .unwrap()
            .unwrap();
        assert_eq!(proof.content_type, PLAIN_TEXT_ARTICLE_FORMAT);
        assert_eq!(proof.connector_version, version);
        assert_eq!(
            saved_publication_verification(&target, &attempt)
                .unwrap()
                .unwrap(),
            proof
        );
        let mut changed = target.clone();
        if let ChannelTargetInput::Publish { body_sha256, .. } = &mut changed.input {
            *body_sha256 = "0".repeat(64);
        }
        assert!(
            saved_publication_verification(&changed, &attempt)
                .unwrap()
                .is_none()
        );
        let mut changed = attempt.clone();
        changed.received_at = Some(claimed_at - chrono::Duration::seconds(1));
        assert!(
            saved_publication_verification(&target, &changed)
                .unwrap()
                .is_none()
        );
        let mut changed = attempt.clone();
        changed.outcome.as_mut().unwrap().runner_evidence[0]["readback_sha256"] =
            "0".repeat(64).into();
        assert!(
            saved_publication_verification(&target, &changed)
                .unwrap()
                .is_none()
        );
        let mut changed = attempt.clone();
        changed.outcome.as_mut().unwrap().runner_evidence[1]["execution_id"] =
            Uuid::new_v4().to_string().into();
        assert!(
            saved_publication_verification(&target, &changed)
                .unwrap()
                .is_none()
        );
        let mut changed = attempt.clone();
        changed.outcome.as_mut().unwrap().runner_evidence[1]["connector_version"] =
            "other.v1".into();
        assert!(
            saved_publication_verification(&target, &changed)
                .unwrap()
                .is_none()
        );
        let mut changed = attempt.clone();
        changed.outcome.as_mut().unwrap().fixture = true;
        assert!(
            saved_publication_verification(&target, &changed)
                .unwrap()
                .is_none()
        );
        let mut changed = attempt.clone();
        changed.outcome.as_mut().unwrap().public_url = Some("https://example.com/p/other".into());
        assert!(
            saved_publication_verification(&target, &changed)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn publication_format_does_not_relabel_unknown_or_rich_formats() {
        assert_eq!(
            publication_format_for_semantic_type("faq"),
            Some(PLAIN_TEXT_ARTICLE_FORMAT)
        );
        assert_eq!(
            publication_format_for_semantic_type("article"),
            Some(PLAIN_TEXT_ARTICLE_FORMAT)
        );
        assert_eq!(publication_format_for_semantic_type("image"), None);
        assert_eq!(publication_format_for_semantic_type("rich_text"), None);
    }

    #[test]
    fn persisted_event_timestamp_truncates_submicrosecond_precision() {
        let event = DateTime::<Utc>::from_timestamp(1_700_000_000, 123_456_789).unwrap();
        assert_eq!(
            publication_storage_timestamp(event),
            DateTime::<Utc>::from_timestamp(1_700_000_000, 123_456_000).unwrap()
        );
    }
}
