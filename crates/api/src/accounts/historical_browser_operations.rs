//! Historical work restores only a proven original route, never today's route.
use chrono::{Duration, Utc};
use geo_domain::{
    AccountNetworkScope, AppError, BeginBrowserOperation, BrowserNetworkSelection,
    BrowserOperationPurpose, ChannelOwnerKind, TenantScope,
};
use uuid::Uuid;

use crate::{browser_bridge::BrowserProxy, channels::ChannelService};

pub(crate) enum HistoricalBrowserNetwork {
    Binding {
        scope: AccountNetworkScope,
        id: Uuid,
    },
    /// An authenticated pre-binding execution snapshot, including explicit direct.
    Legacy(Option<BrowserProxy>),
}

pub(crate) fn account_network_scope(
    scope: &TenantScope,
    account_id: Uuid,
    owner: ChannelOwnerKind,
) -> AccountNetworkScope {
    AccountNetworkScope {
        operator_id: scope.operator_id,
        account_id,
        tenant_id: (owner == ChannelOwnerKind::Customer).then_some(scope.tenant_id),
        project_id: if owner == ChannelOwnerKind::Customer {
            scope.project_id
        } else {
            None
        },
    }
}

impl ChannelService {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_historical_browser(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        session_id: Uuid,
        purpose: BrowserOperationPurpose,
        reservation_id: Option<Uuid>,
        platform: &str,
        storage_state: &serde_json::Value,
        network: HistoricalBrowserNetwork,
    ) -> Result<(), AppError> {
        // Preserve the same legacy-login recovery fence as ordinary startup.
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
        let (network_selection, effective_network) = match network {
            HistoricalBrowserNetwork::Binding {
                scope: network_scope,
                id,
            } => {
                if network_scope.operator_id != scope.operator_id
                    || network_scope.account_id != account_id
                    || network_scope
                        .tenant_id
                        .is_some_and(|tenant| tenant != scope.tenant_id)
                    || network_scope
                        .project_id
                        .is_some_and(|project| Some(project) != scope.project_id)
                {
                    return Err(AppError::conflict(
                        "historical account network scope differs",
                    ));
                }
                let original = self
                    .operations()?
                    .get_network_binding(&network_scope, id)
                    .await?;
                (
                    Some(BrowserNetworkSelection {
                        scope: network_scope,
                        historical_binding_id: Some(id),
                    }),
                    original.effective_network,
                )
            }
            HistoricalBrowserNetwork::Legacy(proxy) => {
                let bytes = serde_json::to_vec(&proxy)
                    .map_err(|_| AppError::conflict("historical account network is invalid"))?;
                (
                    None,
                    self.encrypt(scope, session_id, "browser_operation_network", &bytes)?,
                )
            }
        };
        let generation = self.browser()?.lifecycle_generation().await?;
        let operation = self
            .operations()?
            .begin_browser_operation(
                &crate::browser_operations::execution_scope(scope, session_id),
                &BeginBrowserOperation {
                    operation_id: session_id,
                    session_id,
                    account_id,
                    purpose,
                    runner_generation: generation,
                    effective_network,
                    network_selection,
                    reservation_id,
                    expires_at: Utc::now() + Duration::minutes(20),
                },
                Utc::now(),
            )
            .await?;
        self.start_owned_browser(&operation, scope, platform, Some(storage_state))
            .await
    }
}
