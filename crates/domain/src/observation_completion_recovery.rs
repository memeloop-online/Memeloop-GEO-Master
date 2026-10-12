//! Immutable, separately retained history evidence for an original source.
//! This contract neither changes a capture nor grants cleanup authorization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::provider_conversation_lifecycle::{completion_inventory, deepseek_recovery_document};
use crate::{
    AppError, ConversationCorrelation, MAX_OBSERVATION_SOURCE_BYTES, ObservationCapture,
    ObservationCaptureCompletion, ObservationCaptureSnapshot, ObservationProviderIdentity,
    TenantScope,
};

/// Constructed by the trusted history execution path, not a public authorization
/// request. Persist this input immutably and revalidate it on every later use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationCompletionRecoveryInput {
    pub recovery_id: Uuid,
    pub scope: TenantScope,
    pub source_capture_id: Uuid,
    pub source_sha256: String,
    /// The original receipt's digest_sha256 (the full capture input hash).
    pub source_receipt_sha256: String,
    pub recovery_execution_id: Uuid,
    pub runner_session_id: Uuid,
    pub observed_at: DateTime<Utc>,
    /// Independently observed on the recovery browser, never copied as proof
    /// from the original capture or inferred from history.
    pub recovery_identity: ObservationProviderIdentity,
    /// Closed browser_observed_history projection, not the raw private response.
    pub history_json: String,
    pub history_sha256: String,
}

/// Integrity-validated evidence only. Private fields and no Deserialize prevent
/// accidentally treating unchecked persisted projections as validated output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedObservationCompletionRecovery {
    digest_sha256: String,
    completion: ObservationCaptureCompletion,
    retained_message_inventory_sha256: String,
}

impl ValidatedObservationCompletionRecovery {
    pub fn digest_sha256(&self) -> &str {
        &self.digest_sha256
    }

    pub fn completion(&self) -> &ObservationCaptureCompletion {
        &self.completion
    }

    pub fn retained_message_inventory_sha256(&self) -> &str {
        &self.retained_message_inventory_sha256
    }
}

impl ObservationCompletionRecoveryInput {
    /// Caller MUST load `original` from authoritative storage under exactly
    /// `scope`: a capture receipt itself does not encode tenant ownership.
    /// This verifies evidence, not the current account reservation, execution
    /// lease, exclusive conversation ownership, or permission to delete.
    ///
    /// Only ownership-only ordinal-zero sources are recoverable here. A source
    /// already carrying lifecycle/completion returns an explicit not-needed
    /// error; replay the original immutable sidecar for idempotent recovery.
    pub fn validate(
        &self,
        scope: &TenantScope,
        original: &ObservationCapture,
    ) -> Result<ValidatedObservationCompletionRecovery, AppError> {
        let invalid = || AppError::invalid_request("invalid completion recovery evidence");
        if &self.scope != scope
            || scope.project_id.is_none()
            || scope.operator_id.as_uuid().is_nil()
            || scope.tenant_id.as_uuid().is_nil()
            || scope.project_id.is_some_and(|id| id.as_uuid().is_nil())
        {
            return Err(AppError::forbidden("completion recovery scope mismatch"));
        }
        if [
            self.recovery_id,
            self.recovery_execution_id,
            self.runner_session_id,
        ]
        .iter()
        .any(Uuid::is_nil)
            || self.recovery_id == original.input.capture_id
            || self.recovery_execution_id == original.input.attempt_id
            || self.runner_session_id == original.input.runner_session_id
            || self.source_capture_id != original.input.capture_id
            || original.receipt.capture_id != original.input.capture_id
            || original.receipt.schema_version != 1
            || self.source_receipt_sha256 != original.receipt.digest_sha256
            || original.input.validate(scope)? != original.receipt.digest_sha256
        {
            return Err(invalid());
        }
        let ObservationCaptureSnapshot::Source {
            source_json,
            source_sha256,
        } = &original.input.snapshot
        else {
            return Err(invalid());
        };
        self.recovery_identity.validate()?;
        let owned = original
            .input
            .owned_conversation
            .as_ref()
            .ok_or_else(invalid)?;
        if self.source_sha256 != *source_sha256
            || original.input.original_identity.as_ref() != Some(&self.recovery_identity)
            || self.recovery_identity.provider != "deepseek"
            || owned.provider != "deepseek"
            || owned.correlation != ConversationCorrelation::CreateResponse
            || self.history_json.is_empty()
            || self.history_json.len() > MAX_OBSERVATION_SOURCE_BYTES
            || hex::encode(Sha256::digest(self.history_json.as_bytes())) != self.history_sha256
        {
            return Err(invalid());
        }
        let source: serde_json::Value = serde_json::from_str(source_json).map_err(|_| invalid())?;
        if original.input.completion.is_some() || source.get("lifecycle").is_some() {
            return Err(AppError::invalid_request(
                "completion recovery not needed for lifecycle-bearing source",
            ));
        }
        let history: serde_json::Value =
            serde_json::from_str(&self.history_json).map_err(|_| invalid())?;
        if history["observed_at"]
            .as_str()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            != Some(self.observed_at)
        {
            return Err(invalid());
        }
        let (document, completion) =
            deepseek_recovery_document(&source, history).ok_or_else(invalid)?;
        let inventory = completion_inventory(
            &owned.provider,
            &document,
            &owned.external_conversation_id,
            &completion,
        )
        .ok_or_else(invalid)?;
        let retained_message_inventory_sha256 = hex::encode(Sha256::digest(
            serde_json::to_vec(&inventory).map_err(|_| invalid())?,
        ));
        Ok(ValidatedObservationCompletionRecovery {
            digest_sha256: hex::encode(Sha256::digest(
                serde_json::to_vec(self).map_err(|_| invalid())?,
            )),
            completion,
            retained_message_inventory_sha256,
        })
    }
}

/// Durable acknowledgement only; it does not authorize remote cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCompletionRecoveryReceipt {
    pub recovery_id: Uuid,
    pub schema_version: u32,
    pub digest_sha256: String,
    pub stored_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCompletionRecovery {
    pub input: ObservationCompletionRecoveryInput,
    pub receipt: ObservationCompletionRecoveryReceipt,
}

#[async_trait::async_trait]
pub trait ObservationCompletionRecoveryRepository: Send + Sync {
    async fn save(
        &self,
        scope: &TenantScope,
        input: ObservationCompletionRecoveryInput,
    ) -> Result<ObservationCompletionRecoveryReceipt, AppError>;

    async fn get(
        &self,
        scope: &TenantScope,
        recovery_id: Uuid,
    ) -> Result<Option<ObservationCompletionRecovery>, AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_conversation_lifecycle::synthetic_deepseek_source;
    use crate::{
        CapturedConversation, CapturedConversationPurpose, ObservationCaptureInput,
        ObservationCaptureReceipt,
    };
    use serde_json::{Value, json};

    fn digest(value: &str) -> String {
        hex::encode(Sha256::digest(value.as_bytes()))
    }

    fn fixture() -> (
        TenantScope,
        ObservationCapture,
        ObservationCompletionRecoveryInput,
    ) {
        let scope = TenantScope::new(
            Uuid::new_v4().into(),
            Uuid::new_v4().into(),
            Some(Uuid::new_v4().into()),
        );
        let mut source = synthetic_deepseek_source();
        let history_json = source["lifecycle"]["history"].to_string();
        source["ownership"] = json!({
            "schema_version": "geo.deepseek.ownership.v1",
            "provider": "deepseek",
            "creation": source["lifecycle"]["creation"],
            "submission": source["lifecycle"]["submission"],
        });
        source.as_object_mut().unwrap().remove("lifecycle");
        let source_json = source.to_string();
        let identity = ObservationProviderIdentity {
            provider: "deepseek".into(),
            platform_account_id: "synthetic-account".into(),
        };
        let observed_at = "2026-01-01T00:00:00Z".parse().unwrap();
        let input = ObservationCaptureInput {
            capture_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            runner_session_id: Uuid::new_v4(),
            original_identity: Some(identity.clone()),
            ordinal: 0,
            observed_at,
            snapshot: ObservationCaptureSnapshot::Source {
                source_sha256: digest(&source_json),
                source_json: source_json.clone(),
            },
            owned_conversation: Some(CapturedConversation {
                provider: "deepseek".into(),
                external_conversation_id: "synthetic-chat".into(),
                purpose: CapturedConversationPurpose::Measurement,
                correlation: ConversationCorrelation::CreateResponse,
            }),
            completion: None,
        };
        let original = ObservationCapture {
            receipt: ObservationCaptureReceipt {
                capture_id: input.capture_id,
                schema_version: 1,
                digest_sha256: input.validate(&scope).unwrap(),
                stored_at: observed_at,
            },
            input,
        };
        let recovery = ObservationCompletionRecoveryInput {
            recovery_id: Uuid::new_v4(),
            scope: scope.clone(),
            source_capture_id: original.input.capture_id,
            source_sha256: digest(&source_json),
            source_receipt_sha256: original.receipt.digest_sha256.clone(),
            recovery_execution_id: Uuid::new_v4(),
            runner_session_id: Uuid::new_v4(),
            observed_at,
            recovery_identity: identity,
            history_sha256: digest(&history_json),
            history_json,
        };
        (scope, original, recovery)
    }

    fn replace_source(
        scope: &TenantScope,
        original: &mut ObservationCapture,
        recovery: &mut ObservationCompletionRecoveryInput,
        source: Value,
    ) {
        let source_json = source.to_string();
        recovery.source_sha256 = digest(&source_json);
        original.input.snapshot = ObservationCaptureSnapshot::Source {
            source_sha256: recovery.source_sha256.clone(),
            source_json,
        };
        original.receipt.digest_sha256 = original.input.validate(scope).unwrap();
        recovery.source_receipt_sha256 = original.receipt.digest_sha256.clone();
    }

    #[test]
    fn completion_recovery_is_replayable_without_mutating_original_or_authorizing_delete() {
        let (scope, original, recovery) = fixture();
        let before = original.clone();
        let validated = recovery.validate(&scope, &original).unwrap();
        assert_eq!(validated, recovery.validate(&scope, &original).unwrap());
        assert_eq!(validated.completion().assistant_message_ids, ["2"]);
        assert_eq!(
            validated.retained_message_inventory_sha256(),
            digest(r#"[["1","user"],["2","assistant"]]"#)
        );
        assert_eq!(validated.digest_sha256().len(), 64);
        assert_eq!(original, before);
        assert!(!original.has_complete_conversation_evidence(&scope));
        assert!(original.retained_message_inventory_sha256(&scope).is_none());
    }

    #[test]
    fn completion_recovery_rejects_changed_bindings_and_provenance() {
        let (scope, original, recovery) = fixture();
        for change in 0..12 {
            let mut invalid = recovery.clone();
            match change {
                0 => invalid.scope.project_id = Some(Uuid::new_v4().into()),
                1 => invalid.source_capture_id = Uuid::new_v4(),
                2 => invalid.source_sha256 = "0".repeat(64),
                3 => invalid.source_receipt_sha256 = "0".repeat(64),
                4 => invalid.recovery_identity.platform_account_id = "other-account".into(),
                5 => invalid.runner_session_id = original.input.runner_session_id,
                6 => invalid.recovery_execution_id = original.input.attempt_id,
                7 => invalid.recovery_id = Uuid::nil(),
                8 => invalid.history_sha256 = "0".repeat(64),
                9 => invalid.observed_at += chrono::Duration::seconds(1),
                10 => invalid.scope.tenant_id = Uuid::new_v4().into(),
                _ => invalid.scope.operator_id = Uuid::new_v4().into(),
            }
            assert!(invalid.validate(&scope, &original).is_err(), "{change}");
        }
        for change in 0..6 {
            let mut invalid = original.clone();
            match change {
                0 => invalid.receipt.capture_id = Uuid::new_v4(),
                1 => invalid.receipt.schema_version = 2,
                2 => invalid.input.target_id = Uuid::new_v4(),
                3 => invalid.input.attempt_id = Uuid::new_v4(),
                4 => invalid.input.account_id = Uuid::new_v4(),
                _ => invalid.input.original_identity = None,
            }
            assert!(recovery.validate(&scope, &invalid).is_err(), "{change}");
        }
        let mut missing_identity = original.clone();
        missing_identity.input.original_identity = None;
        missing_identity.receipt.digest_sha256 = missing_identity.input.validate(&scope).unwrap();
        let mut rebound = recovery;
        rebound.source_receipt_sha256 = missing_identity.receipt.digest_sha256.clone();
        assert!(rebound.validate(&scope, &missing_identity).is_err());
    }

    #[test]
    fn completion_recovery_cannot_supply_missing_or_ambiguous_original_transport_evidence() {
        for change in 0..8 {
            let (scope, mut original, mut recovery) = fixture();
            let ObservationCaptureSnapshot::Source { source_json, .. } = &original.input.snapshot
            else {
                unreachable!()
            };
            let mut source: Value = serde_json::from_str(source_json).unwrap();
            match change {
                0 => {
                    source["messages"].as_array_mut().unwrap().remove(0);
                }
                1 => {
                    source["messages"].as_array_mut().unwrap().remove(2);
                }
                2 => {
                    let ready = source["messages"][0].clone();
                    source["messages"].as_array_mut().unwrap().push(ready);
                }
                3 => {
                    let finish = source["messages"][2].clone();
                    source["messages"].as_array_mut().unwrap().push(finish);
                }
                4 => source["messages"].as_array_mut().unwrap().swap(0, 2),
                5 => source["messages"][0]["data"]["request_message_id"] = json!(3),
                6 => source["messages"][2]["raw_data"] = Value::Null,
                _ => {
                    source.as_object_mut().unwrap().remove("ownership");
                }
            }
            replace_source(&scope, &mut original, &mut recovery, source);
            assert!(recovery.validate(&scope, &original).is_err(), "{change}");
        }
    }

    #[test]
    fn completion_recovery_reuses_closed_history_verifier() {
        for (path, value) in [
            ("/chat_session_id", json!("other-chat")),
            ("/cache_control", json!("APPEND")),
            ("/cache_request/cache_version", json!(1)),
            ("/messages/1/status", json!("STREAMING")),
            ("/messages/1/message_id", json!(3)),
            ("/messages/1/parent_id", json!(3)),
            ("/messages/1/has_pending_fragment", json!(true)),
            ("/current_message_id", json!(3)),
        ] {
            let (scope, original, mut recovery) = fixture();
            let mut history: Value = serde_json::from_str(&recovery.history_json).unwrap();
            *history.pointer_mut(path).unwrap() = value;
            recovery.history_json = history.to_string();
            recovery.history_sha256 = digest(&recovery.history_json);
            assert!(recovery.validate(&scope, &original).is_err(), "{path}");
        }
        let (scope, original, mut recovery) = fixture();
        let mut history: Value = serde_json::from_str(&recovery.history_json).unwrap();
        history["answer"] = json!("not a transport field");
        recovery.history_json = history.to_string();
        recovery.history_sha256 = digest(&recovery.history_json);
        assert!(recovery.validate(&scope, &original).is_err());
    }

    #[test]
    fn completion_recovery_accepts_original_redacted_finish_but_not_fabricated_terminal_flag() {
        let (scope, mut original, mut recovery) = fixture();
        let ObservationCaptureSnapshot::Source { source_json, .. } = &original.input.snapshot
        else {
            unreachable!()
        };
        let mut source: Value = serde_json::from_str(source_json).unwrap();
        source["messages"][2]
            .as_object_mut()
            .unwrap()
            .remove("raw_data");
        source["source_redaction"] = json!({
            "schema_version": "geo.observation.source-redaction.v1",
            "policy": "reasoning-and-private-fields",
            "omitted_paths": ["/messages/2/raw_data"],
        });
        replace_source(&scope, &mut original, &mut recovery, source.clone());
        assert!(recovery.validate(&scope, &original).is_ok());
        source["source_redaction"]["omitted_paths"] = json!([]);
        replace_source(&scope, &mut original, &mut recovery, source);
        assert!(recovery.validate(&scope, &original).is_err());
    }
}
