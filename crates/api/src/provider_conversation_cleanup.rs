//! Cleanup is independent of measurement results and never repeats a question.
use std::{sync::Arc, time::Duration};

use chrono::Utc;
use geo_domain::{
    AppError, ProviderCleanupAction, ProviderCleanupClaim, ProviderCleanupOutcome,
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
        let outcome = cleanup_once(&state, repository.as_ref(), &scope, &claim).await;
        if let Err(error) = repository
            .finish(&scope, claim.cleanup_id, claim.lease_id, outcome)
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
) -> ProviderCleanupOutcome {
    let Some(bridge) = state.channel_service().browser.as_ref() else {
        return ProviderCleanupOutcome::Failed;
    };
    let Some(callback) = state.provider_cleanup_callback() else {
        return ProviderCleanupOutcome::Failed;
    };
    let now = Utc::now();
    // The entire restore/identity/RPC deadline plus close must fit inside both
    // fences. No renewal, detached retry, or side effect after lease expiry.
    if now + chrono::Duration::seconds(100) >= claim.lease_until
        || claim.provider != claim.original_identity.provider
    {
        return ProviderCleanupOutcome::Failed;
    }
    let reservation_id = Uuid::new_v4();
    if state
        .channel_job_repository()
        .reserve_account(
            scope,
            claim.account_id,
            reservation_id,
            now,
            claim.lease_until,
        )
        .await
        .is_err()
    {
        return ProviderCleanupOutcome::Failed;
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
        return ProviderCleanupOutcome::Failed;
    }
    let session_id = Uuid::new_v4();
    let mut start_confirmed = false;
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
        let verified = bridge.complete(session_id).await?;
        if verified.identity.platform_account_id != claim.original_identity.platform_account_id {
            return Ok::<_, AppError>(ProviderCleanupOutcome::NeedsLogin);
        }
        // The runner's independent 60-second deadline must also end inside
        // the leases if this client's timeout or close response is lost.
        if Utc::now() + chrono::Duration::seconds(75) >= claim.lease_until {
            return Ok(ProviderCleanupOutcome::Failed);
        }
        let ticket = if claim.action == ProviderCleanupAction::Delete {
            Some(callback.issue_ticket(scope, claim, reservation_id, session_id)?)
        } else {
            None
        };
        let expected = CleanupExpectedIdentity {
            provider: claim.original_identity.provider.clone(),
            platform_account_id: claim.original_identity.platform_account_id.clone(),
        };
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
            return Ok(ProviderCleanupOutcome::Unknown);
        }
        Ok(map_outcome(claim.action, result.status))
    };
    let outcome = match tokio::time::timeout(Duration::from_secs(85), operation).await {
        Ok(Ok(outcome)) => outcome,
        _ => ProviderCleanupOutcome::Unknown,
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

fn map_outcome(action: ProviderCleanupAction, status: CleanupStatus) -> ProviderCleanupOutcome {
    match (action, status) {
        (ProviderCleanupAction::Delete, CleanupStatus::Deleted) => ProviderCleanupOutcome::Deleted,
        (ProviderCleanupAction::Reconcile, CleanupStatus::Present) => {
            ProviderCleanupOutcome::Present
        }
        (_, CleanupStatus::NeedsLogin) => ProviderCleanupOutcome::NeedsLogin,
        (ProviderCleanupAction::Delete, CleanupStatus::Retained) => ProviderCleanupOutcome::Failed,
        // Absence/404 does not prove deletion, including during reconciliation.
        _ => ProviderCleanupOutcome::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_requires_positive_presence_and_never_certifies_deletion() {
        for status in [
            CleanupStatus::Deleted,
            CleanupStatus::Unknown,
            CleanupStatus::Retained,
        ] {
            assert_eq!(
                map_outcome(ProviderCleanupAction::Reconcile, status),
                ProviderCleanupOutcome::Unknown
            );
        }
        assert_eq!(
            map_outcome(ProviderCleanupAction::Reconcile, CleanupStatus::Present),
            ProviderCleanupOutcome::Present
        );
        assert_eq!(
            map_outcome(ProviderCleanupAction::Delete, CleanupStatus::Deleted),
            ProviderCleanupOutcome::Deleted
        );
        assert_eq!(
            map_outcome(ProviderCleanupAction::Delete, CleanupStatus::Present),
            ProviderCleanupOutcome::Unknown
        );
    }
}
