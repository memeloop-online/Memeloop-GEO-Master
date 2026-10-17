//! Durable, interpretation-independent browser observation checkpoints.
//! These are not verified measurements or permission to delete remote data.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

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
pub enum ObservationCompletionOutcome {
    Complete,
    ClearedFailure,
}

/// Transport lifecycle proof, independent of answer interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationCaptureCompletion {
    pub outcome: ObservationCompletionOutcome,
    pub message_inventory: Vec<(String, String)>,
}

impl ObservationCaptureCompletion {
    /// The authenticated adapter owns provider protocol verification. Rust only
    /// accepts a canonical inventory, never a claim of deletion authority.
    pub fn validate(&self) -> Result<(), AppError> {
        let valid = self.message_inventory.len() <= 32_768
            && self.message_inventory.iter().all(|(id, role)| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                    && matches!(role.as_str(), "user" | "assistant" | "system")
            })
            && self
                .message_inventory
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0)
            && match self.outcome {
                ObservationCompletionOutcome::Complete => {
                    self.message_inventory
                        .iter()
                        .any(|(_, role)| role == "user")
                        && self
                            .message_inventory
                            .iter()
                            .any(|(_, role)| role == "assistant")
                }
                ObservationCompletionOutcome::ClearedFailure => self.message_inventory.is_empty(),
            };
        if !valid {
            return Err(AppError::invalid_request(
                "invalid observation completion inventory",
            ));
        }
        Ok(())
    }

    pub fn message_inventory_sha256(&self) -> Result<String, AppError> {
        self.validate()?;
        Ok(hex::encode(Sha256::digest(
            serde_json::to_vec(&self.message_inventory).map_err(|_| {
                AppError::invalid_request("invalid observation completion inventory")
            })?,
        )))
    }
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
        self.input
            .completion
            .as_ref()
            .is_some_and(|completion| completion.outcome == ObservationCompletionOutcome::Complete)
            && self.valid_receipt(scope)
    }

    fn valid_receipt(&self, scope: &TenantScope) -> bool {
        self.receipt.capture_id == self.input.capture_id
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
        if !self.valid_receipt(scope) {
            return None;
        }
        self.input
            .completion
            .as_ref()?
            .message_inventory_sha256()
            .ok()
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

pub(crate) fn checked_json(
    raw: &str,
    digest: &str,
    limit: usize,
) -> Result<serde_json::Value, AppError> {
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
                if let Some(completion) = &self.completion {
                    let owned = self.owned_conversation.as_ref().filter(|owned| {
                        self.original_identity
                            .as_ref()
                            .is_some_and(|identity| identity.provider == owned.provider)
                    });
                    if owned.is_none() {
                        return Err(AppError::invalid_request(
                            "invalid observation completion evidence",
                        ));
                    }
                    completion.validate()?;
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

    #[test]
    fn generic_completion_inventory_is_canonical_and_distinguishes_empty_failure() {
        let completion = ObservationCaptureCompletion {
            outcome: ObservationCompletionOutcome::Complete,
            message_inventory: vec![
                ("1".into(), "user".into()),
                ("2".into(), "assistant".into()),
            ],
        };
        assert!(completion.validate().is_ok());
        assert_eq!(
            completion.message_inventory_sha256().unwrap(),
            hex::encode(Sha256::digest(br#"[["1","user"],["2","assistant"]]"#))
        );
        let failed = ObservationCaptureCompletion {
            outcome: ObservationCompletionOutcome::ClearedFailure,
            message_inventory: vec![],
        };
        assert!(failed.validate().is_ok());
        assert_eq!(
            failed.message_inventory_sha256().unwrap(),
            hex::encode(Sha256::digest(b"[]"))
        );
        for inventory in [
            vec![],
            vec![("1".into(), "assistant".into())],
            vec![("1".into(), "user".into())],
            vec![
                ("2".into(), "assistant".into()),
                ("1".into(), "user".into()),
            ],
            vec![
                ("1".into(), "user".into()),
                ("1".into(), "assistant".into()),
            ],
            vec![
                ("../1".into(), "user".into()),
                ("2".into(), "assistant".into()),
            ],
            vec![
                ("1".into(), "unknown".into()),
                ("2".into(), "assistant".into()),
            ],
        ] {
            let mut invalid = completion.clone();
            invalid.message_inventory = inventory;
            assert!(invalid.validate().is_err());
        }
        let mut invalid = failed;
        invalid.message_inventory = completion.message_inventory;
        assert!(invalid.validate().is_err());
        for legacy in [
            serde_json::json!({"protocol":"connect_json","terminal":true,"assistant_message_ids":["2"]}),
            serde_json::json!({"outcome":"unknown","message_inventory":[]}),
        ] {
            assert!(serde_json::from_value::<ObservationCaptureCompletion>(legacy).is_err());
        }
    }

    #[test]
    fn normalized_completion_binds_generic_owner_identity_phase_and_receipt() {
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
        input.owned_conversation = Some(CapturedConversation {
            provider: input.original_identity.as_ref().unwrap().provider.clone(),
            external_conversation_id: "synthetic-chat".into(),
            purpose: CapturedConversationPurpose::Measurement,
            correlation: ConversationCorrelation::CreateResponse,
        });
        input.completion = Some(ObservationCaptureCompletion {
            outcome: ObservationCompletionOutcome::Complete,
            message_inventory: vec![
                ("1".into(), "user".into()),
                ("2".into(), "assistant".into()),
            ],
        });
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
        assert!(capture.retained_message_inventory_sha256(&scope).is_some());
        let mut changed = capture.clone();
        changed
            .input
            .owned_conversation
            .as_mut()
            .unwrap()
            .external_conversation_id = "other-chat".into();
        assert!(changed.retained_message_inventory_sha256(&scope).is_none());
        for field in 0..3 {
            let mut invalid = input.clone();
            match field {
                0 => invalid.original_identity = None,
                1 => invalid.owned_conversation = None,
                _ => invalid
                    .completion
                    .as_mut()
                    .unwrap()
                    .message_inventory
                    .reverse(),
            }
            assert!(invalid.validate(&scope).is_err());
        }
        input.completion.as_mut().unwrap().outcome = ObservationCompletionOutcome::ClearedFailure;
        input.completion.as_mut().unwrap().message_inventory.clear();
        assert!(input.validate(&scope).is_ok());
        let cleared = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                digest_sha256: input.validate(&scope).unwrap(),
                ..capture.receipt
            },
            input,
        };
        assert!(!cleared.has_complete_conversation_evidence(&scope));
        assert_eq!(
            cleared.retained_message_inventory_sha256(&scope),
            Some(crate::sha256_hex(b"[]"))
        );
    }

    #[test]
    fn source_envelope_retains_opaque_evidence_and_closed_generic_metadata() {
        let document = serde_json::json!({
            "messages":[{"text":"synthetic answer"}],
            "ownership":{"opaque":"adapter audit"},
            "lifecycle":{"opaque":"adapter audit"},
            "capture_status":{"schema_version":"geo.observation.capture-status.v1","transport":"partial","termination":"eof"},
            "source_redaction":{"schema_version":"geo.observation.source-redaction.v1","policy":"reasoning-and-private-fields","omitted_paths":["/messages/0/private"]}
        });
        assert!(valid_observation_source_document(&document));
        for field in 0..5 {
            let mut invalid = document.clone();
            match field {
                0 => invalid["extra"] = serde_json::json!(true),
                1 => invalid["capture_status"]["extra"] = serde_json::json!(true),
                2 => invalid["capture_status"]["transport"] = serde_json::json!("complete"),
                3 => {
                    invalid["source_redaction"]["omitted_paths"] =
                        serde_json::json!(["/messages/0/text"])
                }
                _ => {
                    invalid["source_redaction"]["omitted_paths"] =
                        serde_json::json!(["/messages/2/private"])
                }
            }
            assert!(!valid_observation_source_document(&invalid), "{field}");
        }
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
