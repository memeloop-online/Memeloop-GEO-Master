//! Operator-wide connector availability. Account login is not publication proof.
use crate::{AppError, ChannelOutcome, ChannelOutcomeStatus, OperatorId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
        self.key.validate()?;
        if self.verification_id.is_nil()
            || !trusted_version(&self.connector_version)
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
    if !current.enabled {
        result.availability = ConnectorAvailability::Disabled;
    } else if !trusted_version(deployed_version)
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
        && (content_types.is_empty()
            || !trusted_version(deployed_version)
            || content_types.iter().any(|kind| {
                !verifications
                    .iter()
                    .any(|v| v.connector_version == deployed_version && &v.content_type == kind)
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
}
