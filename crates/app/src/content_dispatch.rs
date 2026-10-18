//! Cross-replica content recovery. Claims are taken in the same database
//! path used by HTTP dispatch; scan pages never hold execution locks.
use geo_api::AppState;
use geo_domain::TenantScope;
use geo_persistence::{PgContentRepository, PgProjectRepository};
use tracing::warn;

pub fn spawn(
    state: AppState,
    repository: PgContentRepository,
    projects: PgProjectRepository,
    allowed_scope: Option<TenantScope>,
) {
    if !state.content_executor_available() {
        return;
    }
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let now = chrono::Utc::now();
            // Preparation can reuse an independently checked revision without
            // a model. Only a genuine generation/check/repair miss needs one;
            // those operations preserve retryable capability-missing state.
            {
                let mut after = None;
                loop {
                    match projects
                        .scan_pending_content_cycles_after(after, now, 100)
                        .await
                    {
                        Ok(cycles) => {
                            let count = cycles.len();
                            for cycle in cycles {
                                after = Some(cycle.cycle_id);
                                if !within_dispatch_scope(allowed_scope.as_ref(), &cycle.scope) {
                                    continue;
                                }
                                let lease = match projects
                                    .try_claim_content_bootstrap(
                                        &cycle.scope,
                                        cycle.cycle_id,
                                        chrono::Utc::now(),
                                        chrono::Duration::minutes(15),
                                    )
                                    .await
                                {
                                    Ok(lease) => lease,
                                    Err(error) => {
                                        warn!(code = ?error.code, "content bootstrap claim failed");
                                        continue;
                                    }
                                };
                                let Some(lease) = lease else { continue };
                                let started = state
                                    .content_service()
                                    .start(&cycle.scope, cycle.cycle_id)
                                    .await;
                                let outcome = started.as_ref().map(|_| ()).map_err(|e| e.code);
                                if let Err(error) = projects
                                    .finish_content_bootstrap(&lease, chrono::Utc::now(), outcome)
                                    .await
                                {
                                    warn!(code = ?error.code, "content bootstrap release failed");
                                }
                                match started {
                                    Ok(execution) => {
                                        if let Err(error) = state.dispatch_content_execution(
                                            cycle.scope,
                                            execution.execution_id,
                                        ) {
                                            warn!(code = ?error.code, "content bootstrap dispatch failed");
                                        }
                                    }
                                    Err(error) => {
                                        warn!(code = ?error.code, "content bootstrap deferred");
                                    }
                                }
                            }
                            if count < 100 {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                        Err(error) => {
                            warn!(code = ?error.code, "content bootstrap scan failed");
                            break;
                        }
                    }
                }
            }
            for stage in [false, true] {
                let mut after = None;
                loop {
                    let page = if stage {
                        repository.scan_closed_after(after, now, 100).await
                    } else {
                        repository.scan_running_after(after, now, 100).await
                    };
                    match page {
                        Ok(candidates) => {
                            let count = candidates.len();
                            for candidate in candidates {
                                after = Some(candidate.execution_id);
                                if !within_dispatch_scope(allowed_scope.as_ref(), &candidate.scope)
                                {
                                    continue;
                                }
                                // The same fence covers HTTP dispatch and both
                                // native workflow stages, including after close.
                                if let Err(error) = state.dispatch_content_execution(
                                    candidate.scope,
                                    candidate.execution_id,
                                ) {
                                    warn!(code = ?error.code, "content recovery dispatch failed");
                                }
                            }
                            if count < 100 {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                        Err(error) => {
                            warn!(code = ?error.code, "content recovery scan failed");
                            break;
                        }
                    }
                }
            }
        }
    });
}

fn within_dispatch_scope(allowed: Option<&TenantScope>, candidate: &TenantScope) -> bool {
    // Scoped tests always pin a project; persistent development may pin only
    // the tenant. Production retains its normal per-tenant model routing.
    allowed.is_none_or(|scope| scope.contains(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn scope(operator: u128, tenant: u128, project: Option<u128>) -> TenantScope {
        TenantScope::new(
            Uuid::from_u128(operator).into(),
            Uuid::from_u128(tenant).into(),
            project.map(|id| Uuid::from_u128(id).into()),
        )
    }

    fn candidate_scopes() -> Vec<TenantScope> {
        vec![
            scope(2, 10, Some(100)),
            scope(1, 20, Some(100)),
            scope(1, 10, Some(200)),
            scope(1, 10, None),
            scope(1, 10, Some(100)),
        ]
    }

    #[test]
    fn project_pin_requires_matching_operator_tenant_and_project() {
        let allowed = scope(1, 10, Some(100));
        let actual: Vec<_> = candidate_scopes()
            .into_iter()
            .filter(|candidate| within_dispatch_scope(Some(&allowed), candidate))
            .collect();
        assert_eq!(actual, vec![allowed]);
    }

    #[test]
    fn persistent_development_tenant_pin_preserves_contains_semantics() {
        let allowed = scope(1, 10, None);
        let actual: Vec<_> = candidate_scopes()
            .into_iter()
            .filter(|candidate| within_dispatch_scope(Some(&allowed), candidate))
            .collect();
        assert_eq!(
            actual,
            vec![
                scope(1, 10, Some(200)),
                scope(1, 10, None),
                scope(1, 10, Some(100)),
            ]
        );
    }

    #[test]
    fn production_dispatch_keeps_all_scopes_eligible() {
        for candidate in candidate_scopes() {
            assert!(within_dispatch_scope(None, &candidate));
        }
    }
}
