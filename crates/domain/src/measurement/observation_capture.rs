//! Durable, interpretation-independent browser observation checkpoints.
//! These are not verified measurements or permission to delete remote data.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::provider_conversation_lifecycle::{completion_inventory, deepseek_evidence_chat_id};
use crate::{AppError, ChannelJobRepository, ChannelTargetInput, TenantScope};

pub const MAX_OBSERVATION_SOURCE_BYTES: usize = 750_000;
pub const MAX_OBSERVATION_CANDIDATE_BYTES: usize = 150_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationCaptureSnapshot {
    Source {
        source_json: String,
        source_sha256: String,
    },
    Extraction {
        source_capture_id: Uuid,
        source_json: String,
        source_sha256: String,
    },
    Candidate {
        source_capture_id: Uuid,
        route: ExtractionRoute,
        candidate_json: String,
        candidate_sha256: String,
        grounding_reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionRoute {
    SignedInBrowser,
    ConfiguredModelApi,
}

/// Only a fresh-chat response or independently verified page may establish
/// ownership. A model-extracted ID, title or account history never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationCorrelation {
    CreateResponse,
    VerifiedPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturedConversationPurpose {
    Measurement,
    Extraction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedConversation {
    pub provider: String,
    pub external_conversation_id: String,
    pub purpose: CapturedConversationPurpose,
    pub correlation: ConversationCorrelation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationProviderIdentity {
    pub provider: String,
    pub platform_account_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCompletionProtocol {
    ConnectJson,
    DeepseekHistoryJson,
}

/// Transport lifecycle proof, independent of answer interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationCaptureCompletion {
    pub protocol: ObservationCompletionProtocol,
    pub terminal: bool,
    pub assistant_message_ids: Vec<String>,
}

impl ObservationProviderIdentity {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.provider.is_empty()
            || self.provider.len() > 64
            || !self
                .provider
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            || self.platform_account_id.trim().is_empty()
            || self.platform_account_id.len() > 512
            || self.platform_account_id.chars().any(char::is_control)
        {
            return Err(AppError::invalid_request(
                "invalid original provider identity",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationCaptureInput {
    pub capture_id: Uuid,
    pub target_id: Uuid,
    pub attempt_id: Uuid,
    pub account_id: Uuid,
    pub runner_session_id: Uuid,
    /// Frozen by the trusted execution path, never inferred from current account
    /// state. Legacy evidence without this binding cannot authorize cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_identity: Option<ObservationProviderIdentity>,
    /// Source is zero; raw extractions and candidates share positive indices.
    pub ordinal: u32,
    pub observed_at: DateTime<Utc>,
    pub snapshot: ObservationCaptureSnapshot,
    pub owned_conversation: Option<CapturedConversation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<ObservationCaptureCompletion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCaptureReceipt {
    pub capture_id: Uuid,
    pub schema_version: u32,
    pub digest_sha256: String,
    pub stored_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCapture {
    pub input: ObservationCaptureInput,
    pub receipt: ObservationCaptureReceipt,
}

impl ObservationCapture {
    /// This is necessary, not sufficient, for cleanup: callers must still
    /// authorize the exact persisted owner, attempt and original account.
    pub fn has_complete_conversation_evidence(&self, scope: &TenantScope) -> bool {
        self.input.completion.is_some()
            && self.receipt.capture_id == self.input.capture_id
            && self.receipt.schema_version == 1
            && self
                .input
                .validate(scope)
                .is_ok_and(|digest| digest == self.receipt.digest_sha256)
    }

    /// Canonical inventory of all retained messages, including user turns.
    /// The remote adapter must compare its fully paginated inventory before
    /// deletion; assistant completion alone does not cover later added turns.
    pub fn retained_message_inventory_sha256(&self, scope: &TenantScope) -> Option<String> {
        if !self.has_complete_conversation_evidence(scope) {
            return None;
        }
        let source_json = match &self.input.snapshot {
            ObservationCaptureSnapshot::Source { source_json, .. }
            | ObservationCaptureSnapshot::Extraction { source_json, .. } => source_json,
            ObservationCaptureSnapshot::Candidate { .. } => return None,
        };
        let document = serde_json::from_str(source_json).ok()?;
        let inventory = completion_inventory(
            &self.input.owned_conversation.as_ref()?.provider,
            &document,
            &self
                .input
                .owned_conversation
                .as_ref()?
                .external_conversation_id,
            self.input.completion.as_ref()?,
        )?;
        Some(hex::encode(Sha256::digest(
            serde_json::to_vec(&inventory).ok()?,
        )))
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn safe_json(value: &serde_json::Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match value {
        serde_json::Value::Array(values) => values.iter().all(|value| safe_json(value, depth + 1)),
        serde_json::Value::Object(fields) => fields.iter().all(|(key, value)| {
            let key = key.to_ascii_lowercase().replace(['-', '_'], "");
            !matches!(
                key.as_str(),
                "authorization"
                    | "cookie"
                    | "cookies"
                    | "password"
                    | "accesstoken"
                    | "refreshtoken"
                    | "sessiontoken"
                    | "sessionkey"
                    | "apikey"
                    | "credential"
                    | "credentials"
                    | "proxy"
                    | "encryptedproxy"
                    | "encryptedsession"
                    | "email"
                    | "phone"
                    | "userid"
                    | "accountid"
                    | "thinking"
                    | "reasoning"
                    | "reasoningcontent"
            ) && safe_json(value, depth + 1)
        }),
        _ => true,
    }
}

fn checked_json(raw: &str, digest: &str, limit: usize) -> Result<serde_json::Value, AppError> {
    if raw.is_empty()
        || raw.len() > limit
        || !valid_digest(digest)
        || hex::encode(Sha256::digest(raw.as_bytes())) != digest
    {
        return Err(AppError::invalid_request(
            "invalid observation capture digest or size",
        ));
    }
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| AppError::invalid_request("invalid observation capture JSON"))?;
    if !safe_json(&value, 0) {
        return Err(AppError::invalid_request(
            "unsafe observation capture fields",
        ));
    }
    Ok(value)
}

/// Capture transport metadata does not establish conversation ownership,
/// provider completion, or permission to delete remote data.
fn valid_capture_status(value: &serde_json::Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    fields.len() == 3
        && fields
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            == Some("geo.observation.capture-status.v1")
        && matches!(
            (
                fields.get("transport").and_then(serde_json::Value::as_str),
                fields
                    .get("termination")
                    .and_then(serde_json::Value::as_str),
            ),
            (Some("complete"), Some("complete"))
                | (
                    Some("partial"),
                    Some("eof" | "timeout" | "aborted" | "malformed" | "limit_exceeded")
                )
        )
}

/// Closed source envelope shared by capture and replay. An omission declaration
/// proves neither complete sanitization nor ownership of a remote conversation.
pub(crate) fn valid_observation_source_document(document: &serde_json::Value) -> bool {
    let Some(fields) = document.as_object() else {
        return false;
    };
    let Some(messages) = fields.get("messages").and_then(serde_json::Value::as_array) else {
        return false;
    };
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "messages"
                | "rendered_text"
                | "source_redaction"
                | "lifecycle"
                | "ownership"
                | "capture_status"
        )
    }) || fields
        .get("rendered_text")
        .is_some_and(|value| !value.is_string())
        || fields
            .get("capture_status")
            .is_some_and(|value| !valid_capture_status(value))
        || ((fields.contains_key("lifecycle") || fields.contains_key("ownership"))
            && deepseek_evidence_chat_id(document).is_none())
    {
        return false;
    }
    let Some(manifest) = fields.get("source_redaction") else {
        return true;
    };
    let Some(manifest) = manifest.as_object() else {
        return false;
    };
    if manifest.len() != 3
        || manifest
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("geo.observation.source-redaction.v1")
        || manifest.get("policy").and_then(serde_json::Value::as_str)
            != Some("reasoning-and-private-fields")
    {
        return false;
    }
    let Some(paths) = manifest
        .get("omitted_paths")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    paths.len() <= 32_768
        && paths.iter().all(|value| {
            let Some(path) = value.as_str() else {
                return false;
            };
            if path.len() > 2_048 || path.chars().any(char::is_control) {
                return false;
            }
            let Some(rest) = path.strip_prefix("/messages/") else {
                return false;
            };
            let Some((index, _)) = rest.split_once('/') else {
                return false;
            };
            if index.is_empty()
                || (index.len() > 1 && index.starts_with('0'))
                || !index.bytes().all(|byte| byte.is_ascii_digit())
                || !index
                    .parse::<usize>()
                    .is_ok_and(|index| index < messages.len())
            {
                return false;
            }
            let mut bytes = path.bytes();
            while let Some(byte) = bytes.next() {
                if byte == b'~' && !matches!(bytes.next(), Some(b'0' | b'1')) {
                    return false;
                }
            }
            document.pointer(path).is_none()
        })
}

impl ObservationCaptureInput {
    pub fn validate(&self, scope: &TenantScope) -> Result<String, AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        if [
            self.capture_id,
            self.target_id,
            self.attempt_id,
            self.account_id,
            self.runner_session_id,
        ]
        .iter()
        .any(Uuid::is_nil)
        {
            return Err(AppError::invalid_request("observation identity required"));
        }
        if let Some(identity) = &self.original_identity {
            identity.validate()?;
            if self
                .owned_conversation
                .as_ref()
                .is_some_and(|conversation| conversation.provider != identity.provider)
            {
                return Err(AppError::invalid_request(
                    "observation provider identity mismatch",
                ));
            }
        }
        if let Some(conversation) = &self.owned_conversation {
            let id = &conversation.external_conversation_id;
            if conversation.provider.is_empty()
                || conversation.provider.len() > 64
                || !conversation
                    .provider
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
                || id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            {
                return Err(AppError::invalid_request("invalid correlated conversation"));
            }
            if !matches!(
                (&self.snapshot, conversation.purpose),
                (
                    ObservationCaptureSnapshot::Source { .. },
                    CapturedConversationPurpose::Measurement
                ) | (
                    ObservationCaptureSnapshot::Extraction { .. },
                    CapturedConversationPurpose::Extraction
                ) | (
                    ObservationCaptureSnapshot::Candidate { .. },
                    CapturedConversationPurpose::Extraction
                )
            ) {
                return Err(AppError::invalid_request(
                    "conversation purpose does not match capture phase",
                ));
            }
        }
        match &self.snapshot {
            ObservationCaptureSnapshot::Source {
                source_json,
                source_sha256,
            }
            | ObservationCaptureSnapshot::Extraction {
                source_json,
                source_sha256,
                ..
            } => {
                if matches!(&self.snapshot, ObservationCaptureSnapshot::Source { .. })
                    && self.ordinal != 0
                {
                    return Err(AppError::invalid_request(
                        "source capture ordinal must be zero",
                    ));
                }
                if let ObservationCaptureSnapshot::Extraction {
                    source_capture_id, ..
                } = &self.snapshot
                    && (self.ordinal == 0 || source_capture_id.is_nil())
                {
                    return Err(AppError::invalid_request("invalid raw extraction identity"));
                }
                let document =
                    checked_json(source_json, source_sha256, MAX_OBSERVATION_SOURCE_BYTES)?;
                if !valid_observation_source_document(&document) {
                    return Err(AppError::invalid_request("invalid source document"));
                }
                if (document.get("lifecycle").is_some() || document.get("ownership").is_some())
                    && !self.owned_conversation.as_ref().is_some_and(|owned| {
                        owned.provider == "deepseek"
                            && deepseek_evidence_chat_id(&document)
                                == Some(owned.external_conversation_id.as_str())
                    })
                {
                    return Err(AppError::invalid_request(
                        "observation lifecycle conversation mismatch",
                    ));
                }
                if let Some(completion) = &self.completion {
                    let owned = self.owned_conversation.as_ref().filter(|owned| {
                        self.original_identity
                            .as_ref()
                            .is_some_and(|identity| identity.provider == owned.provider)
                    });
                    if !owned.is_some_and(|owned| {
                        completion_inventory(
                            &owned.provider,
                            &document,
                            &owned.external_conversation_id,
                            completion,
                        )
                        .is_some()
                    }) {
                        return Err(AppError::invalid_request(
                            "invalid observation completion evidence",
                        ));
                    }
                }
            }
            ObservationCaptureSnapshot::Candidate {
                source_capture_id,
                candidate_json,
                candidate_sha256,
                grounding_reason,
                ..
            } => {
                if self.completion.is_some() {
                    return Err(AppError::invalid_request(
                        "candidate cannot prove conversation completion",
                    ));
                }
                if self.ordinal == 0
                    || source_capture_id.is_nil()
                    || grounding_reason.as_ref().is_some_and(|reason| {
                        reason.len() > 64
                            || !reason
                                .bytes()
                                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                    })
                {
                    return Err(AppError::invalid_request(
                        "invalid extraction candidate identity",
                    ));
                }
                // Capture is evidence, not a second interpretation validator.
                // Retain sanitized rejected candidates as well as valid ones;
                // schema/grounding checks belong to the observation interpreter.
                checked_json(
                    candidate_json,
                    candidate_sha256,
                    MAX_OBSERVATION_CANDIDATE_BYTES,
                )?;
            }
        }
        let input = serde_json::to_vec(self)
            .map_err(|_| AppError::invalid_request("invalid observation capture"))?;
        Ok(hex::encode(Sha256::digest(input)))
    }
}

/// Service-only source discovery metadata; never includes captured content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCaptureSourceItem {
    pub scope: TenantScope,
    pub capture_id: Uuid,
}

#[async_trait]
pub trait ObservationCaptureRepository: Send + Sync {
    /// Source phase, ordinal zero, ordered by UUID with an inclusive stored_at
    /// cutoff fixed across pages. Does not decide interpretation eligibility.
    async fn scan_sources(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationCaptureSourceItem>, AppError>;
    /// Bounded discovery of immutable source checkpoints, including attempts
    /// whose runner response was lost after the checkpoint was saved.
    async fn list_sources_for_attempt(
        &self,
        _scope: &TenantScope,
        _target_id: Uuid,
        _attempt_id: Uuid,
    ) -> Result<Vec<ObservationCapture>, AppError> {
        Ok(Vec::new())
    }
    async fn save(
        &self,
        scope: &TenantScope,
        input: ObservationCaptureInput,
    ) -> Result<ObservationCaptureReceipt, AppError>;
    async fn get(
        &self,
        scope: &TenantScope,
        capture_id: Uuid,
    ) -> Result<Option<ObservationCapture>, AppError>;
}

pub type SharedObservationCaptureRepository = Arc<dyn ObservationCaptureRepository>;

pub struct MemoryObservationCaptureRepository {
    jobs: Arc<dyn ChannelJobRepository>,
    captures: Mutex<HashMap<Uuid, (TenantScope, ObservationCapture)>>,
}

impl MemoryObservationCaptureRepository {
    pub fn new(jobs: Arc<dyn ChannelJobRepository>) -> Self {
        Self {
            jobs,
            captures: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ObservationCaptureRepository for MemoryObservationCaptureRepository {
    async fn scan_sources(
        &self,
        as_of: DateTime<Utc>,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ObservationCaptureSourceItem>, AppError> {
        crate::validate_observation_extraction_page_size(limit)?;
        let mut items: Vec<_> = self
            .captures
            .lock()
            .await
            .values()
            .filter(|(_, capture)| {
                capture.receipt.stored_at <= as_of
                    && after.is_none_or(|id| capture.input.capture_id > id)
                    && capture.input.ordinal == 0
                    && matches!(
                        capture.input.snapshot,
                        ObservationCaptureSnapshot::Source { .. }
                    )
            })
            .map(|(scope, capture)| ObservationCaptureSourceItem {
                scope: scope.clone(),
                capture_id: capture.input.capture_id,
            })
            .collect();
        items.sort_by_key(|item| item.capture_id);
        items.truncate(limit);
        Ok(items)
    }
    async fn list_sources_for_attempt(
        &self,
        scope: &TenantScope,
        target_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Vec<ObservationCapture>, AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        let mut items: Vec<_> = self
            .captures
            .lock()
            .await
            .values()
            .filter(|(stored_scope, capture)| {
                stored_scope == scope
                    && capture.input.target_id == target_id
                    && capture.input.attempt_id == attempt_id
                    && matches!(
                        capture.input.snapshot,
                        ObservationCaptureSnapshot::Source { .. }
                    )
            })
            .map(|(_, capture)| capture.clone())
            .collect();
        items.sort_by_key(|capture| {
            (
                std::cmp::Reverse(capture.input.observed_at),
                capture.input.capture_id,
            )
        });
        items.truncate(100);
        Ok(items)
    }
    async fn save(
        &self,
        scope: &TenantScope,
        input: ObservationCaptureInput,
    ) -> Result<ObservationCaptureReceipt, AppError> {
        let digest = input.validate(scope)?;
        let target = self.jobs.get_target(scope, input.target_id).await?;
        if !matches!(target.target.input, ChannelTargetInput::Measure { .. })
            || target.target.input.account_id() != input.account_id
            || !target
                .attempts
                .iter()
                .any(|attempt| attempt.attempt_id == input.attempt_id)
        {
            return Err(AppError::forbidden(
                "observation attempt or account mismatch",
            ));
        }
        if input
            .owned_conversation
            .as_ref()
            .is_some_and(|conversation| {
                !matches!(&target.target.input, ChannelTargetInput::Measure { provider, .. }
                if provider == &conversation.provider)
            })
        {
            return Err(AppError::forbidden("observation provider mismatch"));
        }
        let mut captures = self.captures.lock().await;
        if let Some((prior_scope, existing)) = captures.get(&input.capture_id) {
            return if prior_scope == scope
                && existing.input == input
                && existing.receipt.digest_sha256 == digest
            {
                Ok(existing.receipt.clone())
            } else {
                Err(AppError::conflict("observation capture identity differs"))
            };
        }
        if captures.values().any(|(prior_scope, existing)| {
            prior_scope == scope
                && existing.input.attempt_id == input.attempt_id
                && existing.input.runner_session_id == input.runner_session_id
                && existing.input.ordinal == input.ordinal
        }) {
            return Err(AppError::conflict("observation capture ordinal differs"));
        }
        if let ObservationCaptureSnapshot::Candidate {
            source_capture_id, ..
        }
        | ObservationCaptureSnapshot::Extraction {
            source_capture_id, ..
        } = &input.snapshot
        {
            let Some((prior_scope, source)) = captures.get(source_capture_id) else {
                return Err(AppError::conflict("source capture not saved"));
            };
            if prior_scope != scope
                || source.input.attempt_id != input.attempt_id
                || source.input.runner_session_id != input.runner_session_id
                || !matches!(
                    source.input.snapshot,
                    ObservationCaptureSnapshot::Source { .. }
                )
            {
                return Err(AppError::conflict("candidate source capture mismatch"));
            }
        }
        let receipt = ObservationCaptureReceipt {
            capture_id: input.capture_id,
            schema_version: 1,
            digest_sha256: digest,
            stored_at: Utc::now(),
        };
        captures.insert(
            input.capture_id,
            (
                scope.clone(),
                ObservationCapture {
                    input,
                    receipt: receipt.clone(),
                },
            ),
        );
        Ok(receipt)
    }

    async fn get(
        &self,
        scope: &TenantScope,
        capture_id: Uuid,
    ) -> Result<Option<ObservationCapture>, AppError> {
        if scope.project_id.is_none() {
            return Err(AppError::forbidden("project scope required"));
        }
        Ok(self
            .captures
            .lock()
            .await
            .get(&capture_id)
            .filter(|(stored_scope, _)| stored_scope == scope)
            .map(|(_, capture)| capture.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChannelJobRepository, ChannelTarget, MemoryChannelJobRepository, OperatorId, ProjectId,
        StandaloneMeasurementPlan, TenantId,
    };

    fn source(
        target_id: Uuid,
        attempt_id: Uuid,
        account_id: Uuid,
        session: Uuid,
    ) -> ObservationCaptureInput {
        let source_json = r#"{"messages":[{"type":"answer","text":"synthetic answer"}],"rendered_text":"synthetic answer"}"#.to_owned();
        ObservationCaptureInput {
            capture_id: Uuid::new_v4(),
            target_id,
            attempt_id,
            account_id,
            runner_session_id: session,
            original_identity: Some(ObservationProviderIdentity {
                provider: "synthetic".into(),
                platform_account_id: "synthetic-account".into(),
            }),
            ordinal: 0,
            observed_at: Utc::now(),
            snapshot: ObservationCaptureSnapshot::Source {
                source_sha256: hex::encode(Sha256::digest(source_json.as_bytes())),
                source_json,
            },
            owned_conversation: None,
            completion: None,
        }
    }

    #[tokio::test]
    async fn source_scan_uses_storage_cutoff_phase_ordinal_and_uuid_pages() {
        let store = MemoryObservationCaptureRepository::new(Arc::new(
            MemoryChannelJobRepository::default(),
        ));
        let now = Utc::now();
        for id in 1..=6 {
            let scope = TenantScope::new(
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Some(Uuid::new_v4().into()),
            );
            let mut input = source(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
            );
            input.capture_id = Uuid::from_u128(id);
            // Old observation time is not the discovery cutoff.
            input.observed_at = now - chrono::Duration::days(1);
            if id == 3 {
                input.ordinal = 1;
            }
            if id == 4 {
                input.snapshot = ObservationCaptureSnapshot::Extraction {
                    source_capture_id: Uuid::new_v4(),
                    source_json: "{}".into(),
                    source_sha256: crate::sha256_hex(b"{}"),
                };
            }
            let receipt = ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: "synthetic".into(),
                stored_at: if id == 2 {
                    now + chrono::Duration::seconds(1)
                } else {
                    now
                },
            };
            store.captures.lock().await.insert(
                input.capture_id,
                (scope, ObservationCapture { input, receipt }),
            );
        }
        let first = store.scan_sources(now, None, 2).await.unwrap();
        assert_eq!(
            first.iter().map(|item| item.capture_id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(1), Uuid::from_u128(5)]
        );
        assert_ne!(first[0].scope, first[1].scope);
        let last = store
            .scan_sources(now, Some(first[1].capture_id), 2)
            .await
            .unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].capture_id, Uuid::from_u128(6));
        assert!(
            store
                .scan_sources(now, Some(last[0].capture_id), 2)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .scan_sources(now - chrono::Duration::seconds(1), None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        for limit in [0, 101] {
            assert!(store.scan_sources(now, None, limit).await.is_err());
        }
    }

    #[tokio::test]
    async fn exact_attempt_idempotence_candidate_and_project_scope() {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let jobs = Arc::new(MemoryChannelJobRepository::default());
        let target_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let plan = StandaloneMeasurementPlan {
            plan_id: Uuid::new_v4(),
            project_id: scope.project_id.unwrap(),
            title: "Synthetic topic".into(),
            input_hash: "synthetic".into(),
            revision: 1,
            created_at: Utc::now(),
            targets: vec![ChannelTarget {
                target_id,
                input: ChannelTargetInput::Measure {
                    account_id,
                    provider: "synthetic".into(),
                    model: "fixed".into(),
                    surface: "consumer_web".into(),
                    search_mode: "web_search".into(),
                    protocol_version: "v1".into(),
                    question_set_version: "adhoc".into(),
                    question: "Synthetic question?".into(),
                    market: "global".into(),
                    language: "en".into(),
                    scheduled_at: Utc::now(),
                    sample_ordinal: 0,
                    question_binding: None,
                },
            }],
        };
        jobs.create_measurement_plan(&scope, "synthetic-key", "synthetic-request", plan)
            .await
            .unwrap();
        let attempt_id = Uuid::new_v4();
        jobs.claim(&scope, target_id, attempt_id, Utc::now())
            .await
            .unwrap();
        let store = MemoryObservationCaptureRepository::new(jobs);
        let input = source(target_id, attempt_id, account_id, Uuid::new_v4());
        let first = store.save(&scope, input.clone()).await.unwrap();
        assert_eq!(store.save(&scope, input.clone()).await.unwrap(), first);
        assert_eq!(
            store
                .get(&scope, input.capture_id)
                .await
                .unwrap()
                .unwrap()
                .input,
            input
        );
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        assert!(store.get(&other, input.capture_id).await.unwrap().is_none());

        let mut conflict = input.clone();
        conflict.observed_at = Utc::now();
        assert_eq!(
            store.save(&scope, conflict).await.unwrap_err().code,
            crate::ErrorCode::Conflict
        );
        let mut rebound = input.clone();
        rebound
            .original_identity
            .as_mut()
            .unwrap()
            .platform_account_id = "different-account".into();
        assert_eq!(
            store.save(&scope, rebound).await.unwrap_err().code,
            crate::ErrorCode::Conflict
        );
        let mut wrong_account = input.clone();
        wrong_account.capture_id = Uuid::new_v4();
        wrong_account.account_id = Uuid::new_v4();
        assert_eq!(
            store.save(&scope, wrong_account).await.unwrap_err().code,
            crate::ErrorCode::Forbidden
        );
        let mut wrong_attempt = input.clone();
        wrong_attempt.capture_id = Uuid::new_v4();
        wrong_attempt.attempt_id = Uuid::new_v4();
        assert_eq!(
            store.save(&scope, wrong_attempt).await.unwrap_err().code,
            crate::ErrorCode::Forbidden
        );
        let mut same_ordinal = input.clone();
        same_ordinal.capture_id = Uuid::new_v4();
        assert_eq!(
            store.save(&scope, same_ordinal).await.unwrap_err().code,
            crate::ErrorCode::Conflict
        );

        let candidate_json = r#"{"decision":"unverified"}"#.to_owned();
        let candidate = ObservationCaptureInput {
            capture_id: Uuid::new_v4(),
            ordinal: 1,
            snapshot: ObservationCaptureSnapshot::Candidate {
                source_capture_id: input.capture_id,
                route: ExtractionRoute::SignedInBrowser,
                candidate_sha256: hex::encode(Sha256::digest(candidate_json.as_bytes())),
                candidate_json,
                grounding_reason: Some("model_unverified".into()),
            },
            ..input.clone()
        };
        store.save(&scope, candidate.clone()).await.unwrap();
        assert_eq!(
            store.save(&scope, candidate).await.unwrap().schema_version,
            1
        );
        let raw_json = r#"{"messages":[{"text":"not valid extraction JSON"}]}"#.to_owned();
        let mut raw = ObservationCaptureInput {
            capture_id: Uuid::new_v4(),
            ordinal: 2,
            snapshot: ObservationCaptureSnapshot::Extraction {
                source_capture_id: input.capture_id,
                source_sha256: hex::encode(Sha256::digest(raw_json.as_bytes())),
                source_json: raw_json,
            },
            owned_conversation: Some(CapturedConversation {
                provider: "synthetic".into(),
                external_conversation_id: "synthetic-extraction".into(),
                purpose: CapturedConversationPurpose::Extraction,
                correlation: ConversationCorrelation::CreateResponse,
            }),
            ..input.clone()
        };
        let raw_receipt = store.save(&scope, raw.clone()).await.unwrap();
        assert_eq!(store.save(&scope, raw.clone()).await.unwrap(), raw_receipt);
        raw.capture_id = Uuid::new_v4();
        raw.runner_session_id = Uuid::new_v4();
        assert!(store.save(&scope, raw.clone()).await.is_err());
        raw.ordinal = 0;
        assert!(raw.validate(&scope).is_err());
        raw.ordinal = 2;
        raw.owned_conversation.as_mut().unwrap().purpose = CapturedConversationPurpose::Measurement;
        assert!(raw.validate(&scope).is_err());
        let invalid_json = r#"{"decision":"unverified","config":{"api_key":"secret"}}"#;
        let mut unsafe_candidate = source(target_id, attempt_id, account_id, Uuid::new_v4());
        unsafe_candidate.ordinal = 1;
        unsafe_candidate.snapshot = ObservationCaptureSnapshot::Candidate {
            source_capture_id: input.capture_id,
            route: ExtractionRoute::ConfiguredModelApi,
            candidate_json: invalid_json.into(),
            candidate_sha256: hex::encode(Sha256::digest(invalid_json.as_bytes())),
            grounding_reason: None,
        };
        assert_eq!(
            unsafe_candidate.validate(&scope).unwrap_err().code,
            crate::ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn legacy_identity_is_absent_and_new_identity_is_validated() {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let mut input = source(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let mut legacy = serde_json::to_value(&input).unwrap();
        legacy.as_object_mut().unwrap().remove("original_identity");
        let decoded: ObservationCaptureInput = serde_json::from_value(legacy.clone()).unwrap();
        assert!(decoded.original_identity.is_none());
        assert!(decoded.completion.is_none());
        assert_eq!(serde_json::to_value(&decoded).unwrap(), legacy);
        // Exact pre-extension serialized bytes remain the digest input.
        let serialized = serde_json::to_string(&decoded).unwrap();
        assert!(!serialized.contains("\"completion\""));
        assert_eq!(
            decoded.validate(&scope).unwrap(),
            hex::encode(Sha256::digest(serialized.as_bytes()))
        );
        assert!(decoded.validate(&scope).is_ok());
        input.owned_conversation = Some(CapturedConversation {
            provider: "other".into(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        });
        assert!(input.validate(&scope).is_err());
        input.owned_conversation = None;
        for invalid in ["", "   ", "account\n"] {
            input
                .original_identity
                .as_mut()
                .unwrap()
                .platform_account_id = invalid.into();
            assert!(input.validate(&scope).is_err());
        }
        input
            .original_identity
            .as_mut()
            .unwrap()
            .platform_account_id = "synthetic-account".into();
        input.original_identity.as_mut().unwrap().provider = "invalid/provider".into();
        assert!(input.validate(&scope).is_err());
    }

    fn completed_capture() -> (TenantScope, ObservationCaptureInput, serde_json::Value) {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let mut input = source(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        input.original_identity.as_mut().unwrap().provider = "kimi".into();
        input.owned_conversation = Some(CapturedConversation {
            provider: "kimi".into(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        });
        input.completion = Some(ObservationCaptureCompletion {
            protocol: ObservationCompletionProtocol::ConnectJson,
            terminal: true,
            assistant_message_ids: vec!["synthetic-message".into()],
        });
        let document = serde_json::json!({"messages": [
            {"chat": {"id": "synthetic-chat"}},
            {"message": {"id": "synthetic-message", "chat_id": "synthetic-chat", "role": "assistant", "status": "COMPLETED"}}
        ]});
        replace_source(&mut input, &document);
        (scope, input, document)
    }

    fn replace_source(input: &mut ObservationCaptureInput, document: &serde_json::Value) {
        let raw = document.to_string();
        input.snapshot = ObservationCaptureSnapshot::Source {
            source_sha256: hex::encode(Sha256::digest(raw.as_bytes())),
            source_json: raw,
        };
    }

    #[test]
    fn deepseek_minimal_ownership_is_retained_without_completion_and_binds_receipt() {
        let (scope, mut input, _) = completed_capture();
        input.original_identity.as_mut().unwrap().provider = "deepseek".into();
        input.owned_conversation.as_mut().unwrap().provider = "deepseek".into();
        input.completion = None;
        let full = crate::measurement::provider_conversation_lifecycle::synthetic_deepseek_source();
        let document = serde_json::json!({
            "messages": [],
            "ownership": {
                "schema_version": "geo.deepseek.ownership.v1",
                "provider": "deepseek",
                "creation": full["lifecycle"]["creation"],
                "submission": full["lifecycle"]["submission"]
            }
        });
        replace_source(&mut input, &document);
        let capture = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: input.validate(&scope).unwrap(),
                stored_at: Utc::now(),
            },
            input: input.clone(),
        };
        assert!(!capture.has_complete_conversation_evidence(&scope));
        assert_eq!(capture.retained_message_inventory_sha256(&scope), None);
        let mut invalid = input.clone();
        invalid.completion = Some(ObservationCaptureCompletion {
            protocol: ObservationCompletionProtocol::DeepseekHistoryJson,
            terminal: true,
            assistant_message_ids: vec!["2".into()],
        });
        assert!(invalid.validate(&scope).is_err());
        for provider in ["kimi", "unknown"] {
            let mut invalid = input.clone();
            invalid.owned_conversation.as_mut().unwrap().provider = provider.into();
            invalid.original_identity.as_mut().unwrap().provider = provider.into();
            assert!(invalid.validate(&scope).is_err());
        }
        let mut invalid = input.clone();
        invalid.owned_conversation = None;
        assert!(invalid.validate(&scope).is_err());
        let mut invalid = input.clone();
        invalid
            .owned_conversation
            .as_mut()
            .unwrap()
            .external_conversation_id = "other-chat".into();
        assert!(invalid.validate(&scope).is_err());
        let mut changed = document.clone();
        changed["ownership"]["submission"]["prompt_sha256"] = serde_json::json!("b".repeat(64));
        replace_source(&mut invalid, &changed);
        invalid.owned_conversation = input.owned_conversation.clone();
        assert_ne!(
            invalid.validate(&scope).unwrap(),
            capture.receipt.digest_sha256
        );
    }

    #[test]
    fn deepseek_ownership_rejects_malformed_or_conflicting_proofs() {
        let (scope, mut input, _) = completed_capture();
        input.original_identity.as_mut().unwrap().provider = "deepseek".into();
        input.owned_conversation.as_mut().unwrap().provider = "deepseek".into();
        input.completion = None;
        let mut document =
            crate::measurement::provider_conversation_lifecycle::synthetic_deepseek_source();
        document["ownership"] = serde_json::json!({
            "schema_version": "geo.deepseek.ownership.v1",
            "provider": "deepseek",
            "creation": document["lifecycle"]["creation"],
            "submission": document["lifecycle"]["submission"]
        });
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_ok());
        for (path, value) in [
            ("/ownership/provider", serde_json::json!("kimi")),
            ("/ownership/schema_version", serde_json::json!("unknown")),
            (
                "/ownership/creation/source",
                serde_json::json!("model_output"),
            ),
            (
                "/ownership/creation/chat_session_id",
                serde_json::json!("bad/chat"),
            ),
            (
                "/ownership/submission/chat_session_id",
                serde_json::json!("other"),
            ),
            (
                "/ownership/submission/parent_message_id",
                serde_json::json!(0),
            ),
            (
                "/ownership/submission/model_type",
                serde_json::json!("bad model"),
            ),
            (
                "/ownership/submission/prompt_sha256",
                serde_json::json!("A".repeat(64)),
            ),
            (
                "/ownership/submission/search_enabled",
                serde_json::json!(false),
            ),
            (
                "/ownership/submission/thinking_enabled",
                serde_json::json!(true),
            ),
            (
                "/ownership/submission/model_type",
                serde_json::json!("other-model"),
            ),
            (
                "/ownership/submission/prompt_sha256",
                serde_json::json!("b".repeat(64)),
            ),
            ("/ownership", serde_json::Value::Null),
            ("/lifecycle", serde_json::Value::Null),
        ] {
            let mut invalid = document.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            replace_source(&mut input, &invalid);
            assert!(input.validate(&scope).is_err(), "{path}");
        }
        document.as_object_mut().unwrap().remove("lifecycle");
        for path in ["/ownership", "/ownership/creation", "/ownership/submission"] {
            let mut invalid = document.clone();
            invalid
                .pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("extra".into(), serde_json::json!(true));
            assert!(!valid_observation_source_document(&invalid), "{path}");
            for key in document.pointer(path).unwrap().as_object().unwrap().keys() {
                let mut invalid = document.clone();
                invalid
                    .pointer_mut(path)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(key);
                assert!(!valid_observation_source_document(&invalid), "{path}/{key}");
            }
        }
        document["messages"][0]["data"]["model_type"] = serde_json::json!("other-model");
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_err());
    }

    #[test]
    fn deepseek_completion_binds_owned_source_and_canonical_inventory() {
        let (scope, mut input, _) = completed_capture();
        let document =
            crate::measurement::provider_conversation_lifecycle::synthetic_deepseek_source();
        input.original_identity.as_mut().unwrap().provider = "deepseek".into();
        input.owned_conversation.as_mut().unwrap().provider = "deepseek".into();
        input.completion = Some(ObservationCaptureCompletion {
            protocol: ObservationCompletionProtocol::DeepseekHistoryJson,
            terminal: true,
            assistant_message_ids: vec!["2".into()],
        });
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_ok());
        let serialized = serde_json::to_value(&input).unwrap();
        assert_eq!(
            serialized["completion"]["protocol"],
            "deepseek_history_json"
        );
        assert_eq!(
            serde_json::from_value::<ObservationCaptureInput>(serialized).unwrap(),
            input
        );
        let capture = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: input.validate(&scope).unwrap(),
                stored_at: Utc::now(),
            },
            input: input.clone(),
        };
        assert!(capture.has_complete_conversation_evidence(&scope));
        assert_eq!(
            capture.retained_message_inventory_sha256(&scope),
            Some(hex::encode(Sha256::digest(
                br#"[["1","user"],["2","assistant"]]"#
            )))
        );
        for no_completion in [false, true] {
            let mut invalid = input.clone();
            if no_completion {
                invalid.completion = None;
            }
            invalid
                .owned_conversation
                .as_mut()
                .unwrap()
                .external_conversation_id = "other-chat".into();
            assert!(invalid.validate(&scope).is_err());
            invalid
                .owned_conversation
                .as_mut()
                .unwrap()
                .external_conversation_id = "synthetic-chat".into();
            invalid.owned_conversation.as_mut().unwrap().provider = "kimi".into();
            invalid.original_identity.as_mut().unwrap().provider = "kimi".into();
            assert!(invalid.validate(&scope).is_err());
            invalid.owned_conversation = None;
            assert!(invalid.validate(&scope).is_err());
        }
        let mut invalid = input.clone();
        invalid.original_identity = None;
        assert!(invalid.validate(&scope).is_err());
        let mut metadata_only = input.clone();
        metadata_only.completion = None;
        assert!(metadata_only.validate(&scope).is_ok());
        let mut capture = capture;
        capture.input = metadata_only;
        capture.receipt.digest_sha256 = capture.input.validate(&scope).unwrap();
        assert!(!capture.has_complete_conversation_evidence(&scope));
        assert_eq!(capture.retained_message_inventory_sha256(&scope), None);
    }

    #[test]
    fn deepseek_source_accepts_only_closed_metadata_without_granting_completion() {
        let (scope, mut input, _) = completed_capture();
        input.original_identity.as_mut().unwrap().provider = "deepseek".into();
        input.owned_conversation.as_mut().unwrap().provider = "deepseek".into();
        input.completion = None;
        let document =
            crate::measurement::provider_conversation_lifecycle::synthetic_deepseek_source();
        assert!(valid_observation_source_document(&document));
        for value in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!("geo.deepseek.lifecycle.v1"),
            serde_json::json!({"provider": "kimi"}),
        ] {
            let mut invalid = document.clone();
            invalid["lifecycle"] = value;
            assert!(!valid_observation_source_document(&invalid));
            replace_source(&mut input, &invalid);
            assert!(input.validate(&scope).is_err());
        }
        let mut missing_sse = document.clone();
        missing_sse["messages"] = serde_json::json!([]);
        replace_source(&mut input, &missing_sse);
        assert!(
            input.validate(&scope).is_ok(),
            "metadata may be retained without declaring completion"
        );
        input.completion = Some(ObservationCaptureCompletion {
            protocol: ObservationCompletionProtocol::DeepseekHistoryJson,
            terminal: true,
            assistant_message_ids: vec!["2".into()],
        });
        assert!(
            input.validate(&scope).is_err(),
            "metadata cannot substitute for actual stream evidence"
        );
        input.completion = None;
        missing_sse.as_object_mut().unwrap().remove("lifecycle");
        replace_source(&mut input, &missing_sse);
        assert!(
            input.validate(&scope).is_ok(),
            "partial evidence remains retainable with lifecycle omitted"
        );
    }

    #[test]
    fn capture_status_retains_ownerless_evidence_without_completion_authority() {
        let (scope, mut input, _) = completed_capture();
        input.owned_conversation = None;
        input.completion = None;
        for (transport, termination) in [
            ("complete", "complete"),
            ("partial", "eof"),
            ("partial", "timeout"),
            ("partial", "aborted"),
            ("partial", "malformed"),
            ("partial", "limit_exceeded"),
        ] {
            let document = serde_json::json!({
                "messages": [{"text": "synthetic prefix"}],
                "capture_status": {
                    "schema_version": "geo.observation.capture-status.v1",
                    "transport": transport,
                    "termination": termination
                }
            });
            replace_source(&mut input, &document);
            let before = input.clone();
            let capture = ObservationCapture {
                receipt: ObservationCaptureReceipt {
                    capture_id: input.capture_id,
                    schema_version: 1,
                    digest_sha256: input.validate(&scope).unwrap(),
                    stored_at: Utc::now(),
                },
                input: input.clone(),
            };
            assert_eq!(input, before);
            assert!(!capture.has_complete_conversation_evidence(&scope));
            assert_eq!(capture.retained_message_inventory_sha256(&scope), None);
            input.completion = Some(ObservationCaptureCompletion {
                protocol: ObservationCompletionProtocol::ConnectJson,
                terminal: true,
                assistant_message_ids: vec!["synthetic-message".into()],
            });
            assert!(input.validate(&scope).is_err());
            input.completion = None;
        }
    }

    #[test]
    fn capture_status_rejects_unknown_fields_values_and_inconsistent_transport() {
        let (scope, mut input, mut document) = completed_capture();
        document["capture_status"] = serde_json::json!({
            "schema_version": "geo.observation.capture-status.v1",
            "transport": "complete",
            "termination": "complete"
        });
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_ok());
        for (key, value) in [
            ("schema_version", serde_json::json!("unknown")),
            ("transport", serde_json::json!("unknown")),
            ("termination", serde_json::json!("creation_proof_missing")),
            ("transport", serde_json::json!("partial")),
            ("termination", serde_json::json!("timeout")),
            ("termination", serde_json::json!(null)),
            ("extra", serde_json::json!(true)),
        ] {
            let mut invalid = document.clone();
            invalid["capture_status"][key] = value;
            replace_source(&mut input, &invalid);
            assert!(input.validate(&scope).is_err(), "{key}");
        }
        for status in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"transport": "complete", "termination": "complete"}),
        ] {
            document["capture_status"] = status;
            replace_source(&mut input, &document);
            assert!(input.validate(&scope).is_err());
        }
    }

    #[test]
    fn source_redaction_retains_closed_manifest_without_changing_completion() {
        let (scope, mut input, mut document) = completed_capture();
        document["source_redaction"] = serde_json::json!({
            "schema_version": "geo.observation.source-redaction.v1",
            "policy": "reasoning-and-private-fields",
            "omitted_paths": ["/messages/1/message/reasoning", "/messages/0/escaped~1~0"]
        });
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_ok());
        let before = input.clone();
        assert!(input.validate(&scope).is_ok());
        assert_eq!(input, before);
        for paths in [
            serde_json::json!(["/messages/1/message/id"]),
            serde_json::json!(["/messages/2/reasoning"]),
            serde_json::json!(["/messages/01/reasoning"]),
            serde_json::json!(["/messages/-1/reasoning"]),
            serde_json::json!(["/messages/1"]),
            serde_json::json!(["/rendered_text"]),
            serde_json::json!(["/messages/1/invalid~2"]),
            serde_json::json!(["/messages/1/control\n"]),
            serde_json::json!([format!("/messages/1/{}", "x".repeat(2048))]),
            serde_json::json!([null]),
            serde_json::json!("not-an-array"),
            serde_json::json!(vec!["/messages/1/reasoning"; 32_769]),
        ] {
            let mut invalid = document.clone();
            invalid["source_redaction"]["omitted_paths"] = paths;
            replace_source(&mut input, &invalid);
            assert!(input.validate(&scope).is_err());
        }
        for (key, value) in [
            ("schema_version", serde_json::json!("unknown")),
            ("policy", serde_json::json!("unknown")),
            ("extra", serde_json::json!("synthetic")),
        ] {
            let mut invalid = document.clone();
            invalid["source_redaction"][key] = value;
            replace_source(&mut input, &invalid);
            assert!(input.validate(&scope).is_err());
        }
        document["source_redaction"] = serde_json::Value::Null;
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_err());
    }

    #[test]
    fn unregistered_provider_cannot_borrow_kimi_completion_or_inventory() {
        let (scope, input, _) = completed_capture();
        for provider in ["deepseek", "doubao", "glm", "unknown", "Kimi"] {
            let mut invalid = input.clone();
            invalid.original_identity.as_mut().unwrap().provider = provider.into();
            invalid.owned_conversation.as_mut().unwrap().provider = provider.into();
            assert!(invalid.validate(&scope).is_err());
            // Match the actual input bytes so a digest mismatch cannot mask
            // accidental acceptance of another provider's lifecycle evidence.
            let digest = hex::encode(Sha256::digest(serde_json::to_vec(&invalid).unwrap()));
            let capture = ObservationCapture {
                receipt: ObservationCaptureReceipt {
                    capture_id: invalid.capture_id,
                    schema_version: 1,
                    digest_sha256: digest,
                    stored_at: Utc::now(),
                },
                input: invalid.clone(),
            };
            assert!(!capture.has_complete_conversation_evidence(&scope));
            assert_eq!(capture.retained_message_inventory_sha256(&scope), None);
            // Unknown-provider raw evidence remains retainable, but it cannot
            // establish transport completion or authorize remote deletion.
            invalid.completion = None;
            assert!(invalid.validate(&scope).is_ok());
        }
    }

    #[test]
    fn completion_verifies_raw_source_and_extraction_and_receipt_integrity() {
        let (scope, input, _) = completed_capture();
        let digest = input.validate(&scope).unwrap();
        let mut capture = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: digest,
                stored_at: Utc::now(),
            },
            input,
        };
        assert!(capture.has_complete_conversation_evidence(&scope));
        capture.receipt.digest_sha256 = "0".repeat(64);
        assert!(!capture.has_complete_conversation_evidence(&scope));
        assert!(capture.retained_message_inventory_sha256(&scope).is_none());
        capture.input.ordinal = 1;
        capture.input.owned_conversation.as_mut().unwrap().purpose =
            CapturedConversationPurpose::Extraction;
        let ObservationCaptureSnapshot::Source {
            source_json,
            source_sha256,
        } = capture.input.snapshot
        else {
            unreachable!();
        };
        capture.input.snapshot = ObservationCaptureSnapshot::Extraction {
            source_capture_id: Uuid::new_v4(),
            source_json,
            source_sha256,
        };
        capture.receipt.digest_sha256 = capture.input.validate(&scope).unwrap();
        assert!(capture.has_complete_conversation_evidence(&scope));
        capture.input.completion = None;
        capture.receipt.digest_sha256 = capture.input.validate(&scope).unwrap();
        assert!(!capture.has_complete_conversation_evidence(&scope));
    }

    #[test]
    fn completion_latest_explicit_status_wins_without_interpreting_text() {
        let (scope, mut input, mut document) = completed_capture();
        document["messages"][1]["message"]["status"] = serde_json::json!("STREAMING");
        document["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "message": {"id": "synthetic-message", "status": 2}
            }));
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_ok());
        document["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "message": {"id": "synthetic-message", "status": "STREAMING", "text": "COMPLETED"}
            }));
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_err());
        input.completion = None;
        assert!(
            input.validate(&scope).is_ok(),
            "partial raw evidence remains retainable"
        );
    }

    #[test]
    fn retained_inventory_is_sorted_deduplicated_and_covers_user_turns() {
        let (scope, mut input, mut document) = completed_capture();
        document["messages"].as_array_mut().unwrap().extend([
            serde_json::json!({"message": {"id": "a-user", "chat_id": "synthetic-chat", "role": 2}}),
            serde_json::json!({"message": {"id": "synthetic-message", "status": 2}}),
            serde_json::json!({"message": {"id": "a-user", "text": "incremental"}}),
        ]);
        replace_source(&mut input, &document);
        let mut capture = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: input.validate(&scope).unwrap(),
                stored_at: Utc::now(),
            },
            input,
        };
        let expected = hex::encode(Sha256::digest(
            br#"[["a-user","user"],["synthetic-message","assistant"]]"#,
        ));
        assert_eq!(
            capture.retained_message_inventory_sha256(&scope),
            Some(expected.clone())
        );
        document["messages"].as_array_mut().unwrap().push(
            serde_json::json!({"message": {"id": "z-user", "chat_id": "synthetic-chat", "role": "user"}}),
        );
        replace_source(&mut capture.input, &document);
        capture.receipt.digest_sha256 = capture.input.validate(&scope).unwrap();
        assert_ne!(
            capture.retained_message_inventory_sha256(&scope),
            Some(expected)
        );
        document["messages"][2]["message"]["chat_id"] = serde_json::json!("different-chat");
        replace_source(&mut capture.input, &document);
        assert!(
            capture.input.validate(&scope).is_err(),
            "explicit user chat bindings must match"
        );
        assert_eq!(capture.retained_message_inventory_sha256(&scope), None);
        capture.input.completion = None;
        capture.receipt.digest_sha256 = capture.input.validate(&scope).unwrap();
        assert_eq!(capture.retained_message_inventory_sha256(&scope), None);
    }

    #[test]
    fn completion_rejects_missing_identity_status_and_unclaimed_assistants() {
        let (scope, valid, document) = completed_capture();
        for field in ["id", "role", "status"] {
            let mut document = document.clone();
            document["messages"][1]["message"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            let mut input = valid.clone();
            replace_source(&mut input, &document);
            assert!(input.validate(&scope).is_err(), "{field}");
        }
        for field in ["status", "role", "chat_id"] {
            let mut document = document.clone();
            document["messages"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "message": {"id": "synthetic-message", (field): null}
                }));
            let mut input = valid.clone();
            replace_source(&mut input, &document);
            assert!(input.validate(&scope).is_err(), "{field}");
        }
        let mut extra = document.clone();
        extra["messages"].as_array_mut().unwrap().push(serde_json::json!({
            "message": {"id": "other-message", "role": 3, "chat_id": "synthetic-chat", "status": 2}
        }));
        let mut input = valid.clone();
        replace_source(&mut input, &extra);
        assert!(input.validate(&scope).is_err());
        input
            .completion
            .as_mut()
            .unwrap()
            .assistant_message_ids
            .push("other-message".into());
        assert!(input.validate(&scope).is_ok());
        for ids in [
            vec![],
            vec!["forged".into()],
            vec!["synthetic-message".into(), "synthetic-message".into()],
            vec!["invalid/id".into()],
            vec!["x".repeat(129)],
            vec!["synthetic-message".into(); 257],
        ] {
            let mut input = valid.clone();
            input.completion.as_mut().unwrap().assistant_message_ids = ids;
            assert!(input.validate(&scope).is_err());
        }
    }

    #[test]
    fn completion_binds_observed_incremental_schema_to_preceding_chat() {
        let (scope, mut input, _) = completed_capture();
        let mut document = serde_json::json!({"messages": [
            {"chat": {"id": "synthetic-chat"}},
            {"message": {"id": "synthetic-system", "role": "system", "status": "MESSAGE_STATUS_COMPLETED", "scenario": "synthetic", "createTime": "2026-01-01T00:00:00Z"}},
            {"message": {"id": "synthetic-user", "role": "user", "blocks": [], "isGoal": false, "status": "MESSAGE_STATUS_COMPLETED", "parentId": "synthetic-system", "createTime": "2026-01-01T00:00:00Z"}},
            {"message": {"id": "synthetic-message", "role": "assistant", "status": "MESSAGE_STATUS_GENERATING"}},
            {"message": {"id": "synthetic-message", "refs": []}},
            {"message": {"id": "synthetic-message", "status": "MESSAGE_STATUS_COMPLETED"}}
        ]});
        replace_source(&mut input, &document);
        let capture = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: input.validate(&scope).unwrap(),
                stored_at: Utc::now(),
            },
            input: input.clone(),
        };
        assert_eq!(capture.retained_message_inventory_sha256(&scope), Some(hex::encode(Sha256::digest(
            br#"[["synthetic-message","assistant"],["synthetic-system","system"],["synthetic-user","user"]]"#
        ))));
        for status in [
            "MESSAGE_STATUS_GENERATING",
            "MESSAGE_STATUS_UNKNOWN",
            "completed",
        ] {
            document["messages"][5]["message"]["status"] = serde_json::json!(status);
            replace_source(&mut input, &document);
            assert!(input.validate(&scope).is_err());
        }
        document["messages"][5]["message"]["status"] =
            serde_json::json!("MESSAGE_STATUS_COMPLETED");
        document["messages"].as_array_mut().unwrap().swap(0, 1);
        for explicit_chat in [false, true] {
            if explicit_chat {
                document["messages"][0]["message"]["chat_id"] = serde_json::json!("synthetic-chat");
            }
            replace_source(&mut input, &document);
            assert!(
                input.validate(&scope).is_err(),
                "message before chat must not authorize completion"
            );
        }
        document["messages"].as_array_mut().unwrap().swap(0, 1);
        document["messages"][1]["message"]["role"] = serde_json::json!(1);
        replace_source(&mut input, &document);
        assert!(
            input.validate(&scope).is_err(),
            "unobserved numeric system role remains unknown"
        );
        document["messages"][1]["message"]["role"] = serde_json::json!("system");
        document["messages"][1]["message"]["chat_id"] = serde_json::json!("different-chat");
        replace_source(&mut input, &document);
        assert!(input.validate(&scope).is_err());
    }

    #[test]
    fn completion_requires_owned_provider_raw_phase_and_terminal_protocol() {
        let (scope, input, document) = completed_capture();
        let mut invalid = input.clone();
        invalid.original_identity = None;
        assert!(invalid.validate(&scope).is_err());
        invalid = input.clone();
        invalid.owned_conversation = None;
        assert!(invalid.validate(&scope).is_err());
        invalid = input.clone();
        invalid.original_identity.as_mut().unwrap().provider = "other".into();
        invalid.owned_conversation.as_mut().unwrap().provider = "other".into();
        assert!(invalid.validate(&scope).is_err());
        invalid = input.clone();
        invalid.completion.as_mut().unwrap().terminal = false;
        assert!(invalid.validate(&scope).is_err());
        invalid = input.clone();
        invalid
            .owned_conversation
            .as_mut()
            .unwrap()
            .external_conversation_id = "other-chat".into();
        assert!(invalid.validate(&scope).is_err());
        invalid = input.clone();
        let mut no_chat = document.clone();
        no_chat["messages"].as_array_mut().unwrap().remove(0);
        replace_source(&mut invalid, &no_chat);
        assert!(invalid.validate(&scope).is_err());
        invalid = input.clone();
        invalid.ordinal = 1;
        invalid.owned_conversation.as_mut().unwrap().purpose =
            CapturedConversationPurpose::Extraction;
        invalid.snapshot = ObservationCaptureSnapshot::Candidate {
            source_capture_id: Uuid::new_v4(),
            route: ExtractionRoute::SignedInBrowser,
            candidate_json: "{}".into(),
            candidate_sha256: hex::encode(Sha256::digest(b"{}")),
            grounding_reason: None,
        };
        assert!(invalid.validate(&scope).is_err());
        let mut serialized = serde_json::to_value(input).unwrap();
        serialized["completion"]["protocol"] = serde_json::json!("other");
        assert!(serde_json::from_value::<ObservationCaptureInput>(serialized).is_err());
    }

    #[test]
    fn candidate_retention_does_not_require_valid_interpretation() {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        for candidate in [
            serde_json::json!({
                "decision": "searched_answer",
                "answer_segments": [
                    {"path": "/rendered_text", "quote": "原文 excerpt ".repeat(400)},
                    {"path": "/rendered_text", "start": 0, "end": 20}
                ]
            }),
            serde_json::json!({"decision": "unexpected_shape", "answer_segments": []}),
            serde_json::json!(null),
        ] {
            let mut input = source(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
            );
            let candidate_json = candidate.to_string();
            input.ordinal = 1;
            input.snapshot = ObservationCaptureSnapshot::Candidate {
                source_capture_id: Uuid::new_v4(),
                route: ExtractionRoute::SignedInBrowser,
                candidate_sha256: hex::encode(Sha256::digest(candidate_json.as_bytes())),
                candidate_json,
                grounding_reason: Some("shape_rejected".into()),
            };
            assert!(input.validate(&scope).is_ok());
            if let ObservationCaptureSnapshot::Candidate {
                candidate_json,
                candidate_sha256,
                ..
            } = &mut input.snapshot
            {
                *candidate_json = r#"{"cookie":"synthetic-private-value"}"#.into();
                *candidate_sha256 = hex::encode(Sha256::digest(candidate_json.as_bytes()));
            }
            assert!(input.validate(&scope).is_err());
        }
    }

    #[test]
    fn rejects_bad_digest_oversize_and_credential_envelopes() {
        let scope = TenantScope::new(
            OperatorId::new(Uuid::new_v4()),
            TenantId::new(Uuid::new_v4()),
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let mut input = source(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        if let ObservationCaptureSnapshot::Source {
            source_json,
            source_sha256,
        } = &mut input.snapshot
        {
            *source_json = r#"{"messages":[{"api_key":"secret"}]}"#.into();
            *source_sha256 = hex::encode(Sha256::digest(source_json.as_bytes()));
        }
        assert_eq!(
            input.validate(&scope).unwrap_err().code,
            crate::ErrorCode::InvalidRequest
        );
        if let ObservationCaptureSnapshot::Source {
            source_json,
            source_sha256,
        } = &mut input.snapshot
        {
            *source_json = format!(
                r#"{{"messages":[],"rendered_text":"{}"}}"#,
                "x".repeat(MAX_OBSERVATION_SOURCE_BYTES)
            );
            *source_sha256 = hex::encode(Sha256::digest(source_json.as_bytes()));
        }
        assert_eq!(
            input.validate(&scope).unwrap_err().code,
            crate::ErrorCode::InvalidRequest
        );
    }
}
