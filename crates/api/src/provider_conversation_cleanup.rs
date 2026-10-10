//! Cleanup is independent of measurement results and never repeats a question.
use std::{sync::Arc, time::Duration};

use chrono::Utc;
use geo_domain::{
    AppError, ProviderCleanupAction, ProviderCleanupClaim, ProviderCleanupCode as Code,
    ProviderCleanupDiagnostic, ProviderCleanupOutcome, ProviderCleanupStage as Stage,
    ProviderConversationCleanupRepository, TenantScope,
};
use uuid::Uuid;

use crate::{
    AppState,
    browser_bridge::{CleanupAction, CleanupExpectedIdentity, CleanupStatus},
};

/// Claim before spawning: competing scans cannot create duplicate remote work.
pub async fn dispatch_provider_conversation_cleanup(
    state: AppState,
    repository: Arc<dyn ProviderConversationCleanupRepository>,
    scope: TenantScope,
    cleanup_id: Uuid,
) -> Result<bool, AppError> {
    if state.channel_service().browser.is_none() || state.provider_cleanup_callback().is_none() {
        return Ok(false);
    }
    let Some(claim) = repository.claim(&scope, cleanup_id).await? else {
        return Ok(false);
    };
    tokio::spawn(async move {
        let (outcome, diagnostic) = cleanup_once(&state, repository.as_ref(), &scope, &claim).await;
        if let Err(error) = repository
            .finish_with_diagnostic(
                &scope,
                claim.cleanup_id,
                claim.lease_id,
                outcome,
                diagnostic,
            )
            .await
        {
            // No account, external conversation, ticket, or provider response logs.
            tracing::warn!(code = ?error.code, "conversation cleanup finish failed");
        }
    });
    Ok(true)
}

async fn cleanup_once(
    state: &AppState,
    repository: &dyn ProviderConversationCleanupRepository,
    scope: &TenantScope,
    claim: &ProviderCleanupClaim,
) -> (ProviderCleanupOutcome, Option<ProviderCleanupDiagnostic>) {
    let Some(bridge) = state.channel_service().browser.as_ref() else {
        return diagnosed(
            ProviderCleanupOutcome::Failed,
            Stage::Preflight,
            Code::DependencyUnavailable,
        );
    };
    let Some(callback) = state.provider_cleanup_callback() else {
        return diagnosed(
            ProviderCleanupOutcome::Failed,
            Stage::Preflight,
            Code::DependencyUnavailable,
        );
    };
    let now = Utc::now();
    // The entire restore/identity/RPC deadline plus close must fit inside both
    // fences. No renewal, detached retry, or side effect after lease expiry.
    if now + chrono::Duration::seconds(100) >= claim.lease_until {
        return diagnosed(
            ProviderCleanupOutcome::Failed,
            Stage::Deadline,
            Code::DeadlineExceeded,
        );
    }
    if claim.provider != claim.original_identity.provider {
        return diagnosed(
            ProviderCleanupOutcome::Failed,
            Stage::Identity,
            Code::AccountMismatch,
        );
    }
    let reservation_id = Uuid::new_v4();
    if let Err(error) = state
        .channel_job_repository()
        .reserve_account(
            scope,
            claim.account_id,
            reservation_id,
            now,
            claim.lease_until,
        )
        .await
    {
        return diagnosed(
            ProviderCleanupOutcome::Failed,
            Stage::Preflight,
            if error.code == geo_domain::ErrorCode::Conflict {
                Code::AccountBusy
            } else {
                Code::DependencyUnavailable
            },
        );
    }
    let authorized = if claim.action == ProviderCleanupAction::Delete {
        matches!(
            repository.authorize_delete(
                scope, claim.cleanup_id, claim.lease_id, reservation_id
            ).await,
            Ok(current) if current == *claim
        )
    } else {
        true
    };
    if !authorized || Utc::now() + chrono::Duration::seconds(100) >= claim.lease_until {
        // No browser was started; releasing this exact reservation is safe.
        let _ = state
            .channel_job_repository()
            .release_account(scope, claim.account_id, reservation_id)
            .await;
        return diagnosed(
            ProviderCleanupOutcome::Failed,
            Stage::Authorization,
            if authorized {
                Code::DeadlineExceeded
            } else {
                Code::AuthorizationRequired
            },
        );
    }
    let session_id = Uuid::new_v4();
    let mut start_confirmed = false;
    let mut operation_stage = Stage::Preflight;
    let operation = async {
        let restored = state
            .channel_service()
            .resume_provider_cleanup_browser(
                scope,
                claim.account_id,
                &claim.original_identity,
                session_id,
            )
            .await;
        // Repository/network errors do not identify a specific login failure.
        // Preserve uncertainty rather than depending on English error strings.
        restored?;
        start_confirmed = true;
        operation_stage = Stage::Identity;
        let verified = bridge.complete(session_id).await?;
        if verified.identity.platform_account_id != claim.original_identity.platform_account_id {
            return Ok::<_, AppError>(diagnosed(
                ProviderCleanupOutcome::NeedsLogin,
                Stage::Identity,
                Code::AccountMismatch,
            ));
        }
        // The runner's independent 60-second deadline must also end inside
        // the leases if this client's timeout or close response is lost.
        if Utc::now() + chrono::Duration::seconds(75) >= claim.lease_until {
            return Ok(diagnosed(
                ProviderCleanupOutcome::Failed,
                Stage::Deadline,
                Code::DeadlineExceeded,
            ));
        }
        operation_stage = Stage::Authorization;
        let ticket = if claim.action == ProviderCleanupAction::Delete {
            Some(callback.issue_ticket(scope, claim, reservation_id, session_id)?)
        } else {
            None
        };
        let expected = CleanupExpectedIdentity {
            provider: claim.original_identity.provider.clone(),
            platform_account_id: claim.original_identity.platform_account_id.clone(),
        };
        operation_stage = Stage::Runner;
        let result = bridge
            .cleanup_conversation(
                session_id,
                claim.lease_id,
                &expected,
                &claim.external_conversation_id,
                match claim.action {
                    ProviderCleanupAction::Delete => CleanupAction::Delete,
                    ProviderCleanupAction::Reconcile => CleanupAction::Reconcile,
                },
                ticket.as_deref(),
            )
            .await?;
        if result.execution_id != claim.lease_id
            || result.external_conversation_id != claim.external_conversation_id
        {
            return Ok(diagnosed(
                ProviderCleanupOutcome::Unknown,
                Stage::Runner,
                Code::InvalidResponse,
            ));
        }
        Ok((
            map_outcome(
                claim.action,
                result.status,
                claim.has_prior_delete_attempt && claim.retained_message_inventory_sha256.is_some(),
            ),
            result.diagnostic,
        ))
    };
    let outcome = match tokio::time::timeout(Duration::from_secs(85), operation).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => diagnosed(
            ProviderCleanupOutcome::Unknown,
            operation_stage,
            Code::TransportUnknown,
        ),
        Err(_) => diagnosed(
            ProviderCleanupOutcome::Unknown,
            operation_stage,
            Code::DeadlineExceeded,
        ),
    };
    let closed = matches!(
        tokio::time::timeout(Duration::from_secs(10), bridge.close(session_id)).await,
        Ok(Ok(()))
    );
    // A timed-out start may create a context after this close. Expiry, not a
    // speculative close receipt, releases the account on that path.
    if start_confirmed
        && closed
        && let Err(error) = state
            .channel_job_repository()
            .release_account(scope, claim.account_id, reservation_id)
            .await
    {
        tracing::warn!(code = ?error.code, "conversation cleanup reservation release failed");
    }
    outcome
}

fn diagnosed(
    outcome: ProviderCleanupOutcome,
    stage: Stage,
    code: Code,
) -> (ProviderCleanupOutcome, Option<ProviderCleanupDiagnostic>) {
    (outcome, Some(ProviderCleanupDiagnostic { stage, code }))
}

fn map_outcome(
    action: ProviderCleanupAction,
    status: CleanupStatus,
    has_retained_delete_attempt: bool,
) -> ProviderCleanupOutcome {
    match (action, status) {
        (ProviderCleanupAction::Delete, CleanupStatus::Deleted) => ProviderCleanupOutcome::Deleted,
        (ProviderCleanupAction::Reconcile, CleanupStatus::Present) => {
            ProviderCleanupOutcome::Present
        }
        (ProviderCleanupAction::Reconcile, CleanupStatus::Absent)
            if has_retained_delete_attempt =>
        {
            ProviderCleanupOutcome::Deleted
        }
        (_, CleanupStatus::NeedsLogin) => ProviderCleanupOutcome::NeedsLogin,
        (ProviderCleanupAction::Delete, CleanupStatus::Retained) => ProviderCleanupOutcome::Failed,
        // A generic deletion status or unproven absence cannot settle lookup.
        _ => ProviderCleanupOutcome::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_requires_strict_absence_and_retained_delete_attempt() {
        for status in [
            CleanupStatus::Deleted,
            CleanupStatus::Unknown,
            CleanupStatus::Retained,
        ] {
            assert_eq!(
                map_outcome(ProviderCleanupAction::Reconcile, status, true),
                ProviderCleanupOutcome::Unknown
            );
        }
        assert_eq!(
            map_outcome(
                ProviderCleanupAction::Reconcile,
                CleanupStatus::Present,
                false
            ),
            ProviderCleanupOutcome::Present
        );
        assert_eq!(
            map_outcome(ProviderCleanupAction::Delete, CleanupStatus::Deleted, false),
            ProviderCleanupOutcome::Deleted
        );
        assert_eq!(
            map_outcome(ProviderCleanupAction::Delete, CleanupStatus::Present, true),
            ProviderCleanupOutcome::Unknown
        );
        assert_eq!(
            map_outcome(
                ProviderCleanupAction::Reconcile,
                CleanupStatus::Absent,
                true
            ),
            ProviderCleanupOutcome::Deleted
        );
        for (action, proof) in [
            (ProviderCleanupAction::Delete, false),
            (ProviderCleanupAction::Delete, true),
            (ProviderCleanupAction::Reconcile, false),
        ] {
            assert_eq!(
                map_outcome(action, CleanupStatus::Absent, proof),
                ProviderCleanupOutcome::Unknown
            );
        }
    }
}
