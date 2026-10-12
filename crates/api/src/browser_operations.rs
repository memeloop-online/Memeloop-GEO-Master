//! Application orchestration for durable browser ownership. No process-local
//! map is authoritative; a timeout is not evidence that a browser disappeared.
use chrono::{Duration, Utc};
use geo_domain::{
    AppError, BeginBrowserOperation, BrowserOperation, BrowserOperationPurpose,
    BrowserOperationScope, BrowserOperationState, ChannelJobRepository, TenantScope,
};
use uuid::Uuid;

use crate::{browser_bridge::BrowserProxy, channels::ChannelService};

pub(crate) fn execution_scope(scope: &TenantScope, session_id: Uuid) -> BrowserOperationScope {
    BrowserOperationScope {
        operator_id: scope.operator_id,
        tenant_id: Some(scope.tenant_id),
        project_id: scope.project_id,
        owner_id: session_id,
    }
}

impl ChannelService {
    pub(crate) fn operations(&self) -> Result<&dyn ChannelJobRepository, AppError> {
        self.operation_repository.as_deref().ok_or_else(|| {
            AppError::capability_missing("durable browser ownership is not configured")
        })
    }

    /// The operation-specific AAD prevents a pinned network being transplanted
    /// to another operation. The scope is genuine project or operator scope.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_owned_browser(
        &self,
        scope: &BrowserOperationScope,
        secret_scope: &TenantScope,
        account_id: Uuid,
        session_id: Uuid,
        purpose: BrowserOperationPurpose,
        reservation_id: Option<Uuid>,
        proxy: &Option<BrowserProxy>,
    ) -> Result<BrowserOperation, AppError> {
        for retained in self
            .repository
            .account_login_sessions(scope.operator_id, account_id)
            .await?
        {
            if !self
                .operations()?
                .browser_operation_exists(scope.operator_id, retained)
                .await?
            {
                return Err(AppError::conflict(
                    "an earlier login has unknown browser ownership; recovery is required",
                ));
            }
        }
        let generation = self.browser()?.lifecycle_generation().await?;
        let network_scope = self.account_network_scope(secret_scope, account_id).await?;
        let network = self.ensure_account_network(&network_scope, proxy).await?;
        let effective_network = network.active.effective_network;
        self.operations()?
            .begin_browser_operation(
                scope,
                &BeginBrowserOperation {
                    operation_id: session_id,
                    session_id,
                    account_id,
                    purpose,
                    runner_generation: generation,
                    effective_network,
                    network_selection: Some(geo_domain::BrowserNetworkSelection {
                        scope: network_scope,
                        historical_binding_id: None,
                    }),
                    reservation_id,
                    expires_at: Utc::now() + Duration::minutes(20),
                },
                Utc::now(),
            )
            .await
    }

    pub(crate) async fn start_owned_browser(
        &self,
        operation: &BrowserOperation,
        secret_scope: &TenantScope,
        platform: &str,
        storage_state: Option<&serde_json::Value>,
    ) -> Result<(), AppError> {
        let proxy = self.operation_proxy(operation, secret_scope)?;
        let operation = self
            .operations()?
            .mark_browser_start_requested(&operation.scope, operation.operation_id, Utc::now())
            .await?;
        let started = self
            .browser()?
            .start_in_generation(
                operation.session_id,
                operation.runner_generation,
                platform,
                proxy,
                storage_state,
            )
            .await;
        if started.is_err() {
            // Best effort only: keep the persisted reference and gate if the
            // exact-generation disposal acknowledgement is unavailable.
            let _ = self.dispose_owned_operation(&operation).await;
        }
        started
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_execution_browser(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        session_id: Uuid,
        purpose: BrowserOperationPurpose,
        reservation_id: Option<Uuid>,
        platform: &str,
        proxy: Option<BrowserProxy>,
        storage_state: &serde_json::Value,
    ) -> Result<(), AppError> {
        let operation = self
            .prepare_owned_browser(
                &execution_scope(scope, session_id),
                scope,
                account_id,
                session_id,
                purpose,
                reservation_id,
                &proxy,
            )
            .await?;
        self.start_owned_browser(&operation, scope, platform, Some(storage_state))
            .await
    }

    pub(crate) async fn close_owned_browser(
        &self,
        scope: &TenantScope,
        session_id: Uuid,
    ) -> Result<(), AppError> {
        let operation = self
            .operations()?
            .get_browser_operation(&execution_scope(scope, session_id), session_id)
            .await?
            .ok_or_else(|| AppError::conflict("browser ownership generation is unknown"))?;
        self.dispose_owned_operation(&operation).await
    }

    /// Used by both interactive close and the trusted reconciliation scanner.
    /// This never discovers the current generation to dispose an older owner.
    pub(crate) async fn dispose_owned_operation(
        &self,
        operation: &BrowserOperation,
    ) -> Result<(), AppError> {
        let operation = self
            .operations()?
            .get_browser_operation(&operation.scope, operation.operation_id)
            .await?
            .ok_or_else(|| AppError::conflict("browser ownership generation is unknown"))?;
        if operation.state == BrowserOperationState::Disposed {
            return Ok(());
        }
        if operation.state == BrowserOperationState::Prepared {
            self.operations()?
                .release_browser_unstarted(&operation.scope, operation.operation_id, Utc::now())
                .await?;
            return Ok(());
        }
        self.operations()?
            .request_browser_close(&operation.scope, operation.operation_id, Utc::now())
            .await?;
        let receipt = self
            .browser()?
            .dispose_in_generation(operation.session_id, operation.runner_generation)
            .await
            .map_err(|_| {
                AppError::new(
                    geo_domain::ErrorCode::DependencyUnavailable,
                    "browser release is not confirmed; retry closing the session",
                )
            })?;
        self.operations()?
            .confirm_browser_disposed(
                &operation.scope,
                operation.operation_id,
                receipt.session_id(),
                receipt.generation(),
                Utc::now(),
            )
            .await?;
        Ok(())
    }
}
