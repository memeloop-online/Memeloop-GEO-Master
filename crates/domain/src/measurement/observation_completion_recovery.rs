//! Immutable, separately retained history evidence for an original source.
//! This contract neither changes a capture nor grants cleanup authorization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::observation_capture::checked_json;
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
    pub external_conversation_id: String,
    pub completion: ObservationCaptureCompletion,
    /// Bounded sanitized history audit; provider semantics belong to the adapter.
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
    /// Only incomplete ordinal-zero sources are recoverable here.
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
        let ObservationCaptureSnapshot::Source { source_sha256, .. } = &original.input.snapshot
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
            || self.recovery_identity.provider != owned.provider
            || self.external_conversation_id != owned.external_conversation_id
            || original.input.ordinal != 0
            || owned.purpose != crate::CapturedConversationPurpose::Measurement
            || owned.correlation != ConversationCorrelation::CreateResponse
        {
            return Err(invalid());
        }
        if original.input.completion.is_some() {
            return Err(AppError::invalid_request("completion recovery not needed"));
        }
        checked_json(
            &self.history_json,
            &self.history_sha256,
            MAX_OBSERVATION_SOURCE_BYTES,
        )?;
        let retained_message_inventory_sha256 = self.completion.message_inventory_sha256()?;
        Ok(ValidatedObservationCompletionRecovery {
            digest_sha256: hex::encode(Sha256::digest(
                serde_json::to_vec(self).map_err(|_| invalid())?,
            )),
            completion: self.completion.clone(),
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

impl ObservationCompletionRecovery {
    pub fn validate(
        &self,
        scope: &TenantScope,
        original: &ObservationCapture,
    ) -> Result<ValidatedObservationCompletionRecovery, AppError> {
        let evidence = self.input.validate(scope, original)?;
        if self.receipt.schema_version != 1
            || self.receipt.recovery_id != self.input.recovery_id
            || self.receipt.digest_sha256 != evidence.digest_sha256()
        {
            return Err(AppError::invalid_request(
                "invalid completion recovery receipt",
            ));
        }
        Ok(evidence)
    }
}

impl ObservationCapture {
    /// Resolve retained inventory without changing the immutable original or
    /// granting deletion authority. The caller must independently enforce
    /// ownership, account reservation, leases, and current remote inventory.
    pub fn resolved_message_inventory_sha256(
        &self,
        scope: &TenantScope,
        recovery: Option<&ObservationCompletionRecovery>,
    ) -> Result<Option<String>, AppError> {
        if let Some(inventory) = self.retained_message_inventory_sha256(scope) {
            return Ok(Some(inventory));
        }
        recovery
            .map(|recovery| {
                recovery
                    .validate(scope, self)
                    .map(|evidence| evidence.retained_message_inventory_sha256().to_owned())
            })
            .transpose()
    }
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

    /// Latest retained recovery for this scoped original source. Implementations
    /// must validate both immutable receipts before returning the evidence.
    async fn get_by_source(
        &self,
        scope: &TenantScope,
        source_capture_id: Uuid,
    ) -> Result<Option<ObservationCompletionRecovery>, AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CapturedConversation, CapturedConversationPurpose, ObservationCaptureInput,
        ObservationCaptureReceipt, ObservationCompletionOutcome,
    };

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
        let source_json = r#"{"messages":[{"text":"synthetic answer"}]}"#.to_owned();
        let identity = ObservationProviderIdentity {
            provider: "synthetic".into(),
            platform_account_id: "synthetic-account".into(),
        };
        let input = ObservationCaptureInput {
            capture_id: Uuid::new_v4(),
            target_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            runner_session_id: Uuid::new_v4(),
            ordinal: 0,
            observed_at: Utc::now(),
            original_identity: Some(identity.clone()),
            snapshot: ObservationCaptureSnapshot::Source {
                source_sha256: crate::sha256_hex(source_json.as_bytes()),
                source_json,
            },
            owned_conversation: Some(CapturedConversation {
                provider: "synthetic".into(),
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
                stored_at: Utc::now(),
            },
            input,
        };
        let ObservationCaptureSnapshot::Source { source_sha256, .. } = &original.input.snapshot
        else {
            unreachable!()
        };
        let recovery = ObservationCompletionRecoveryInput {
            recovery_id: Uuid::new_v4(),
            scope: scope.clone(),
            source_capture_id: original.input.capture_id,
            source_sha256: source_sha256.clone(),
            source_receipt_sha256: original.receipt.digest_sha256.clone(),
            recovery_execution_id: Uuid::new_v4(),
            runner_session_id: Uuid::new_v4(),
            observed_at: Utc::now(),
            recovery_identity: identity,
            external_conversation_id: "synthetic-chat".into(),
            completion: ObservationCaptureCompletion {
                outcome: ObservationCompletionOutcome::Complete,
                message_inventory: vec![
                    ("1".into(), "user".into()),
                    ("2".into(), "assistant".into()),
                ],
            },
            history_json: r#"{"messages":[]}"#.into(),
            history_sha256: crate::sha256_hex(br#"{"messages":[]}"#),
        };
        (scope, original, recovery)
    }

    #[test]
    fn generic_recovery_binds_source_owner_identity_and_immutable_audit() {
        let (scope, original, input) = fixture();
        let unchanged = original.clone();
        let evidence = input.validate(&scope, &original).unwrap();
        assert_eq!(
            evidence.completion().outcome,
            crate::ObservationCompletionOutcome::Complete
        );
        let binding = original.recovery_request_binding(&scope).unwrap();
        assert_eq!(binding.source_capture_id, input.source_capture_id);
        assert_eq!(
            binding.source_receipt_sha256,
            original.receipt.digest_sha256
        );
        let stored = ObservationCompletionRecovery {
            receipt: ObservationCompletionRecoveryReceipt {
                recovery_id: input.recovery_id,
                schema_version: 1,
                digest_sha256: evidence.digest_sha256().into(),
                stored_at: Utc::now(),
            },
            input: input.clone(),
        };
        assert!(stored.validate(&scope, &original).is_ok());
        assert_eq!(
            original
                .resolved_message_inventory_sha256(&scope, Some(&stored))
                .unwrap(),
            Some(evidence.retained_message_inventory_sha256().into())
        );
        assert_eq!(original, unchanged);
        assert!(!original.has_complete_conversation_evidence(&scope));
        let mut corrupt = stored;
        corrupt.receipt.digest_sha256 = "0".repeat(64);
        assert!(corrupt.validate(&scope, &original).is_err());
        for field in 0..12 {
            let mut invalid = input.clone();
            match field {
                0 => invalid.source_capture_id = Uuid::new_v4(),
                1 => invalid.source_sha256 = "0".repeat(64),
                2 => invalid.source_receipt_sha256 = "0".repeat(64),
                3 => invalid.recovery_identity.platform_account_id = "other-account".into(),
                4 => invalid.recovery_identity.provider = "other".into(),
                5 => invalid.external_conversation_id = "other-chat".into(),
                6 => invalid.runner_session_id = original.input.runner_session_id,
                7 => invalid.recovery_execution_id = original.input.attempt_id,
                8 => invalid.history_sha256 = "0".repeat(64),
                9 => invalid.completion.message_inventory.reverse(),
                10 => invalid.scope.project_id = Some(Uuid::new_v4().into()),
                _ => {
                    invalid.history_json = r#"{"cookie":"private"}"#.into();
                    invalid.history_sha256 = crate::sha256_hex(invalid.history_json.as_bytes());
                }
            }
            assert!(invalid.validate(&scope, &original).is_err(), "{field}");
        }
    }

    #[test]
    fn cleared_failure_is_an_explicit_empty_inventory_and_never_assistant_completion() {
        let (scope, original, mut input) = fixture();
        input.completion.outcome = crate::ObservationCompletionOutcome::ClearedFailure;
        input.completion.message_inventory.clear();
        let evidence = input.validate(&scope, &original).unwrap();
        assert_eq!(
            evidence.completion().outcome,
            crate::ObservationCompletionOutcome::ClearedFailure
        );
        assert_eq!(
            evidence.retained_message_inventory_sha256(),
            crate::sha256_hex(b"[]")
        );
        input.completion.outcome = crate::ObservationCompletionOutcome::Complete;
        assert!(input.validate(&scope, &original).is_err());
    }
}
