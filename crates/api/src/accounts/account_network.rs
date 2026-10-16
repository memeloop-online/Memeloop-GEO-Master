//! Account-owned immutable routes. Responses never contain proxy credentials.
use crate::browser_bridge::BrowserProxy;
use crate::channels::{ChannelQuery, ChannelService, ProxyInput, trusted_pool_scope};
use crate::{ApiError, AppState, AuthContext, RequestContext, api_error, require_project_writer};
use axum::{
    Json,
    extract::{Extension, Path, Query, State, rejection::JsonRejection},
};
use chrono::Utc;
use geo_domain::{
    AccountNetworkBinding, AccountNetworkBindings, AccountNetworkScope, AccountNetworkSource,
    AppError, BrowserOperation, ErrorCode, LegacyNetworkBootstrap, TenantScope,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkUpdate {
    expected_version: i64,
    source: NetworkSourceInput,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum NetworkSourceInput {
    Direct,
    ProjectDefault,
    Standalone {
        proxy: ProxyInput,
    },
    PoolRevision {
        pool_id: Uuid,
        member_id: Uuid,
        revision_id: Uuid,
    },
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NetworkSourceView {
    Direct,
    Standalone,
    LegacySnapshot,
    ProjectDefault,
    PoolRevision {
        pool_id: Uuid,
        member_id: Uuid,
        revision_id: Uuid,
    },
}
#[derive(Serialize)]
pub(crate) struct NetworkView {
    account_id: Uuid,
    version: i64,
    active: BindingView,
    pending: Option<BindingView>,
    busy: bool,
}
#[derive(Serialize)]
struct BindingView {
    binding_id: Option<Uuid>,
    source: NetworkSourceView,
    server: Option<String>,
    pool_name: Option<String>,
    member_name: Option<String>,
}

impl ChannelService {
    pub(crate) async fn project_network_metadata(
        &self,
        scope: &TenantScope,
        account: &mut geo_domain::ChannelAccount,
    ) -> Result<(), AppError> {
        if account.owner_kind != geo_domain::ChannelOwnerKind::Customer {
            return Ok(());
        }
        let key = AccountNetworkScope {
            operator_id: scope.operator_id,
            account_id: account.account_id,
            tenant_id: Some(scope.tenant_id),
            project_id: scope.project_id,
        };
        if let Some(head) = self.operations()?.get_account_network(&key).await? {
            let proxy = self.binding_proxy(&head.active)?;
            account.proxy_configured = proxy.is_some();
            account.proxy_server = proxy.map(|p| p.server);
        }
        Ok(())
    }
    pub(crate) async fn pool_network_metadata(
        &self,
        operator: geo_domain::OperatorId,
        account: &mut geo_domain::PoolAccount,
    ) -> Result<(), AppError> {
        let key = AccountNetworkScope {
            operator_id: operator,
            account_id: account.account_id,
            tenant_id: None,
            project_id: None,
        };
        if let Some(head) = self.operations()?.get_account_network(&key).await? {
            let proxy = self.binding_proxy(&head.active)?;
            account.proxy_configured = proxy.is_some();
            account.proxy_server = proxy.map(|p| p.server);
        }
        Ok(())
    }
    pub(crate) fn network_secret_scope(
        &self,
        scope: &AccountNetworkScope,
    ) -> Result<TenantScope, AppError> {
        Ok(TenantScope::new(
            scope.operator_id,
            match scope.tenant_id {
                Some(tenant) => tenant,
                None => self.operator_pool_tenant_id.ok_or_else(|| {
                    AppError::capability_missing("operator pool is not configured")
                })?,
            },
            scope.project_id,
        ))
    }

    pub(crate) async fn account_network_scope(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
    ) -> Result<AccountNetworkScope, AppError> {
        if scope.project_id.is_some() {
            match self.repository.get_account(scope, account_id).await {
                Ok(_) => {
                    return Ok(AccountNetworkScope {
                        operator_id: scope.operator_id,
                        account_id,
                        tenant_id: Some(scope.tenant_id),
                        project_id: scope.project_id,
                    });
                }
                Err(error) if error.code == ErrorCode::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.repository
            .get_pool_account(scope.operator_id, account_id)
            .await?;
        Ok(AccountNetworkScope {
            operator_id: scope.operator_id,
            account_id,
            tenant_id: None,
            project_id: None,
        })
    }

    pub(crate) fn binding_proxy(
        &self,
        binding: &AccountNetworkBinding,
    ) -> Result<Option<BrowserProxy>, AppError> {
        let bytes = self.decrypt(
            &self.network_secret_scope(&binding.scope)?,
            binding.binding_id,
            &format!("account_network_binding:{}", binding.scope.account_id),
            &binding.effective_network,
        )?;
        serde_json::from_slice(&bytes)
            .map_err(|_| AppError::conflict("stored account network is invalid"))
    }

    pub(crate) fn network_binding(
        &self,
        scope: &AccountNetworkScope,
        source: AccountNetworkSource,
        proxy: &Option<BrowserProxy>,
    ) -> Result<AccountNetworkBinding, AppError> {
        let binding_id = Uuid::new_v4();
        let bytes = serde_json::to_vec(proxy)
            .map_err(|_| AppError::invalid_request("invalid account network"))?;
        let effective_network = self.encrypt(
            &self.network_secret_scope(scope)?,
            binding_id,
            &format!("account_network_binding:{}", scope.account_id),
            &bytes,
        )?;
        Ok(AccountNetworkBinding {
            binding_id,
            scope: scope.clone(),
            source,
            effective_network,
            created_at: Utc::now(),
        })
    }

    pub(crate) fn operation_proxy(
        &self,
        operation: &BrowserOperation,
        legacy_scope: &TenantScope,
    ) -> Result<Option<BrowserProxy>, AppError> {
        match (&operation.network_scope, operation.network_binding_id) {
            (Some(scope), Some(binding_id)) => self.binding_proxy(&AccountNetworkBinding {
                binding_id,
                scope: scope.clone(),
                source: AccountNetworkSource::LegacySnapshot,
                effective_network: operation.effective_network.clone(),
                created_at: operation.created_at,
            }),
            (None, None) => {
                let bytes = self.decrypt(
                    legacy_scope,
                    operation.operation_id,
                    "browser_operation_network",
                    &operation.effective_network,
                )?;
                serde_json::from_slice(&bytes)
                    .map_err(|_| AppError::conflict("stored browser network is invalid"))
            }
            _ => Err(AppError::conflict(
                "browser network provenance is incomplete",
            )),
        }
    }

    pub(crate) async fn ensure_account_network(
        &self,
        scope: &AccountNetworkScope,
        legacy_proxy: &Option<BrowserProxy>,
    ) -> Result<AccountNetworkBindings, AppError> {
        if let Some(head) = self.operations()?.get_account_network(scope).await? {
            return Ok(head);
        }
        let inherited = if scope.tenant_id.is_some() {
            let secret_scope = self.network_secret_scope(scope)?;
            self.repository
                .get_account(&secret_scope, scope.account_id)
                .await?
                .proxy
                .is_none()
        } else {
            false
        };
        let binding = if inherited {
            let secret_scope = self.network_secret_scope(scope)?;
            let settings = self.repository.get_settings(&secret_scope).await?;
            let proxy = self.project_default_proxy(&secret_scope, settings.as_ref())?;
            self.network_binding(
                scope,
                AccountNetworkSource::ProjectDefault {
                    settings_updated_at: settings.as_ref().map(|record| record.settings.updated_at),
                },
                &proxy,
            )?
        } else {
            self.network_binding(scope, AccountNetworkSource::LegacySnapshot, legacy_proxy)?
        };
        match self
            .operations()?
            .set_account_network(scope, None, &binding, self.repository.as_ref(), Utc::now())
            .await
        {
            Ok(head) => Ok(head),
            Err(error) if error.code == ErrorCode::Conflict => self
                .operations()?
                .get_account_network(scope)
                .await?
                .ok_or(error),
            Err(error) => Err(error),
        }
    }

    async fn legacy_network(
        &self,
        scope: &AccountNetworkScope,
    ) -> Result<(Option<BrowserProxy>, Option<Uuid>), AppError> {
        for id in self
            .repository
            .account_login_sessions(scope.operator_id, scope.account_id)
            .await?
        {
            if !self
                .operations()?
                .browser_operation_exists(scope.operator_id, id)
                .await?
            {
                return Err(AppError::conflict(
                    "an earlier login has unknown browser ownership; recovery is required",
                ));
            }
        }
        if let Some(operation) = self
            .operations()?
            .get_account_browser_operation(scope.operator_id, scope.account_id)
            .await?
        {
            let legacy_scope = TenantScope::new(
                operation.scope.operator_id,
                operation
                    .scope
                    .tenant_id
                    .unwrap_or(self.network_secret_scope(scope)?.tenant_id),
                operation.scope.project_id,
            );
            return Ok((
                self.operation_proxy(&operation, &legacy_scope)?,
                Some(operation.operation_id),
            ));
        }
        let secret_scope = self.network_secret_scope(scope)?;
        let proxy = if scope.tenant_id.is_some() {
            let record = self
                .repository
                .get_account(&secret_scope, scope.account_id)
                .await?;
            self.resolved_proxy(&secret_scope, scope.account_id, record.proxy.as_ref())
                .await?
        } else {
            let record = self
                .repository
                .get_pool_account(scope.operator_id, scope.account_id)
                .await?;
            record
                .proxy
                .as_ref()
                .map(|secret| {
                    let bytes =
                        self.decrypt(&secret_scope, scope.account_id, "pool_proxy", secret)?;
                    serde_json::from_slice::<ProxyInput>(&bytes)
                        .map(ProxyInput::runner)
                        .map_err(|_| AppError::conflict("stored account proxy is invalid"))
                })
                .transpose()?
        };
        Ok((proxy, None))
    }

    async fn binding_view(&self, binding: &AccountNetworkBinding) -> Result<BindingView, AppError> {
        let (pool_name, member_name) = match binding.source {
            AccountNetworkSource::PoolRevision {
                pool_id, member_id, ..
            } => (
                self.repository
                    .get_proxy_pool(binding.scope.operator_id, pool_id)
                    .await
                    .ok()
                    .map(|pool| pool.name),
                self.repository
                    .get_proxy_member(binding.scope.operator_id, pool_id, member_id)
                    .await
                    .ok()
                    .map(|member| member.name),
            ),
            _ => (None, None),
        };
        Ok(BindingView {
            binding_id: Some(binding.binding_id),
            source: match &binding.source {
                AccountNetworkSource::Direct => NetworkSourceView::Direct,
                AccountNetworkSource::Standalone => NetworkSourceView::Standalone,
                AccountNetworkSource::LegacySnapshot => NetworkSourceView::LegacySnapshot,
                AccountNetworkSource::ProjectDefault { .. } => NetworkSourceView::ProjectDefault,
                AccountNetworkSource::PoolRevision {
                    pool_id,
                    member_id,
                    revision_id,
                } => NetworkSourceView::PoolRevision {
                    pool_id: *pool_id,
                    member_id: *member_id,
                    revision_id: *revision_id,
                },
            },
            server: self.binding_proxy(binding)?.map(|proxy| proxy.server),
            pool_name,
            member_name,
        })
    }

    async fn network_view(
        &self,
        scope: &AccountNetworkScope,
        head: AccountNetworkBindings,
    ) -> Result<NetworkView, AppError> {
        let pending = match head.pending.as_ref() {
            Some(binding) => Some(self.binding_view(binding).await?),
            None => None,
        };
        Ok(NetworkView {
            account_id: scope.account_id,
            version: head.version,
            active: self.binding_view(&head.active).await?,
            pending,
            busy: head.busy,
        })
    }

    async fn read_network(&self, scope: &AccountNetworkScope) -> Result<NetworkView, AppError> {
        if let Some(head) = self.operations()?.get_account_network(scope).await? {
            return self.network_view(scope, head).await;
        }
        let (proxy, owner) = self.legacy_network(scope).await?;
        Ok(NetworkView {
            account_id: scope.account_id,
            version: 0,
            active: BindingView {
                binding_id: None,
                source: NetworkSourceView::LegacySnapshot,
                server: proxy.map(|p| p.server),
                pool_name: None,
                member_name: None,
            },
            pending: None,
            busy: owner.is_some(),
        })
    }

    pub(crate) async fn update_network(
        &self,
        scope: &AccountNetworkScope,
        expected_version: i64,
        source: NetworkSourceInput,
    ) -> Result<AccountNetworkBindings, AppError> {
        self.update_network_with_metadata(scope, expected_version, source, None)
            .await
    }

    async fn update_network_with_metadata(
        &self,
        scope: &AccountNetworkScope,
        expected_version: i64,
        source: NetworkSourceInput,
        metadata: Option<&geo_domain::AccountNetworkMetadataUpdate>,
    ) -> Result<AccountNetworkBindings, AppError> {
        for id in self
            .repository
            .account_login_sessions(scope.operator_id, scope.account_id)
            .await?
        {
            if !self
                .operations()?
                .browser_operation_exists(scope.operator_id, id)
                .await?
            {
                return Err(AppError::conflict(
                    "an earlier login has unknown browser ownership; recovery is required",
                ));
            }
        }
        if expected_version < 0 {
            return Err(AppError::invalid_request(
                "expected_version must not be negative",
            ));
        }
        let (source, proxy) = match source {
            NetworkSourceInput::Direct => (AccountNetworkSource::Direct, None),
            NetworkSourceInput::ProjectDefault => {
                if scope.tenant_id.is_none() {
                    return Err(AppError::invalid_request(
                        "project default requires a project account",
                    ));
                }
                let secret_scope = self.network_secret_scope(scope)?;
                let settings = self.repository.get_settings(&secret_scope).await?;
                (
                    AccountNetworkSource::ProjectDefault {
                        settings_updated_at: settings
                            .as_ref()
                            .map(|record| record.settings.updated_at),
                    },
                    self.project_default_proxy(&secret_scope, settings.as_ref())?,
                )
            }
            NetworkSourceInput::Standalone { proxy } => {
                proxy.validate()?;
                (AccountNetworkSource::Standalone, Some(proxy.runner()))
            }
            NetworkSourceInput::PoolRevision {
                pool_id,
                member_id,
                revision_id,
            } => {
                if scope.tenant_id.is_some() {
                    return Err(AppError::forbidden(
                        "operator proxy inventory is not available to this account",
                    ));
                }
                let pool = self
                    .repository
                    .get_proxy_pool(scope.operator_id, pool_id)
                    .await?;
                let member = self
                    .repository
                    .get_proxy_member(scope.operator_id, pool_id, member_id)
                    .await?;
                if !pool.enabled || !member.enabled || member.revision_id != revision_id {
                    return Err(AppError::conflict(
                        "selected proxy revision is no longer available",
                    ));
                }
                let secret = self
                    .repository
                    .get_proxy_revision(scope.operator_id, pool_id, member_id, revision_id)
                    .await?;
                let bytes = self.decrypt(
                    &self.network_secret_scope(scope)?,
                    member_id,
                    &format!("proxy-pool-v1:{pool_id}:{revision_id}"),
                    &secret,
                )?;
                let proxy: ProxyInput = serde_json::from_slice(&bytes)
                    .map_err(|_| AppError::conflict("stored pool proxy is invalid"))?;
                proxy.validate()?;
                (
                    AccountNetworkSource::PoolRevision {
                        pool_id,
                        member_id,
                        revision_id,
                    },
                    Some(proxy.runner()),
                )
            }
        };
        let binding = self.network_binding(scope, source, &proxy)?;
        let legacy = if expected_version == 0 {
            let (proxy, operation_id) = self.legacy_network(scope).await?;
            Some(LegacyNetworkBootstrap {
                binding: self.network_binding(
                    scope,
                    AccountNetworkSource::LegacySnapshot,
                    &proxy,
                )?,
                operation_id,
            })
        } else {
            None
        };
        self.operations()?
            .set_account_network_with_metadata(
                scope,
                (expected_version != 0).then_some(expected_version),
                &binding,
                legacy.as_ref(),
                metadata,
                self.repository.as_ref(),
                Utc::now(),
            )
            .await
    }

    pub(crate) async fn update_legacy_account_proxy(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        proxy: Option<ProxyInput>,
        metadata: &geo_domain::AccountNetworkMetadataUpdate,
    ) -> Result<(), AppError> {
        let network_scope = self.account_network_scope(scope, account_id).await?;
        let version = self
            .operations()?
            .get_account_network(&network_scope)
            .await?
            .map_or(0, |head| head.version);
        self.update_network_with_metadata(
            &network_scope,
            version,
            match proxy {
                Some(proxy) => NetworkSourceInput::Standalone { proxy },
                None if network_scope.tenant_id.is_some() => NetworkSourceInput::ProjectDefault,
                None => NetworkSourceInput::Direct,
            },
            Some(metadata),
        )
        .await?;
        Ok(())
    }

    fn project_default_proxy(
        &self,
        scope: &TenantScope,
        settings: Option<&geo_domain::ChannelSettingsRecord>,
    ) -> Result<Option<BrowserProxy>, AppError> {
        settings
            .and_then(|record| record.proxy.as_ref())
            .map(|secret| {
                let bytes = self.decrypt(scope, Uuid::nil(), "project_proxy", secret)?;
                serde_json::from_slice::<ProxyInput>(&bytes)
                    .map(ProxyInput::runner)
                    .map_err(|_| AppError::conflict("stored project proxy is invalid"))
            })
            .transpose()
    }

    pub(crate) async fn save_project_network_defaults(
        &self,
        scope: &TenantScope,
        expected: Option<&geo_domain::ChannelSettingsRecord>,
        record: &geo_domain::ChannelSettingsRecord,
    ) -> Result<geo_domain::ChannelSettings, AppError> {
        if expected
            .and_then(|record| record.proxy.as_ref())
            .map(geo_domain::ChannelSecret::encrypted_bytes)
            == record
                .proxy
                .as_ref()
                .map(geo_domain::ChannelSecret::encrypted_bytes)
        {
            return self
                .operations()?
                .set_project_network_defaults(
                    scope,
                    expected,
                    record,
                    &[],
                    self.repository.as_ref(),
                    Utc::now(),
                )
                .await;
        }
        let proxy = self.project_default_proxy(scope, Some(record))?;
        let mut updates = Vec::new();
        for account in self.repository.list_accounts(scope).await? {
            let key = AccountNetworkScope {
                operator_id: scope.operator_id,
                account_id: account.account_id,
                tenant_id: Some(scope.tenant_id),
                project_id: scope.project_id,
            };
            let head = self.operations()?.get_account_network(&key).await?;
            let (expected_version, legacy) = match head {
                Some(head) => {
                    if !matches!(
                        head.pending.as_ref().unwrap_or(&head.active).source,
                        AccountNetworkSource::ProjectDefault { .. }
                    ) {
                        continue;
                    }
                    (Some(head.version), None)
                }
                None => {
                    if self
                        .repository
                        .get_account(scope, account.account_id)
                        .await?
                        .proxy
                        .is_some()
                    {
                        continue;
                    }
                    let (old_proxy, operation_id) = self.legacy_network(&key).await?;
                    if operation_id.is_none() {
                        continue;
                    }
                    let baseline = self.network_binding(
                        &key,
                        AccountNetworkSource::LegacySnapshot,
                        &old_proxy,
                    )?;
                    (
                        None,
                        Some(LegacyNetworkBootstrap {
                            binding: baseline,
                            operation_id,
                        }),
                    )
                }
            };
            let binding = self.network_binding(
                &key,
                AccountNetworkSource::ProjectDefault {
                    settings_updated_at: Some(record.settings.updated_at),
                },
                &proxy,
            )?;
            updates.push(geo_domain::ProjectNetworkBindingUpdate {
                expected_version,
                binding,
                legacy,
            });
        }
        self.operations()?
            .set_project_network_defaults(
                scope,
                expected,
                record,
                &updates,
                self.repository.as_ref(),
                Utc::now(),
            )
            .await
    }
}

async fn customer_scope(
    state: &AppState,
    tenant: &TenantScope,
    project: geo_domain::ProjectId,
    id: Uuid,
) -> Result<AccountNetworkScope, AppError> {
    let scope = crate::channels::scope(state, tenant, project).await?;
    state
        .channel_service()
        .repository
        .get_account(&scope, id)
        .await?;
    Ok(AccountNetworkScope {
        operator_id: scope.operator_id,
        account_id: id,
        tenant_id: Some(scope.tenant_id),
        project_id: scope.project_id,
    })
}
async fn operator_scope(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
) -> Result<AccountNetworkScope, AppError> {
    let scope = trusted_pool_scope(state.channel_service(), auth)?;
    state
        .channel_service()
        .repository
        .get_pool_account(scope.operator_id, id)
        .await?;
    Ok(AccountNetworkScope {
        operator_id: scope.operator_id,
        account_id: id,
        tenant_id: None,
        project_id: None,
    })
}
fn body(input: Result<Json<NetworkUpdate>, JsonRejection>) -> Result<NetworkUpdate, AppError> {
    input
        .map(|Json(input)| input)
        .map_err(|_| AppError::invalid_request("invalid account network request"))
}
pub(crate) async fn get_customer(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<NetworkView>, ApiError> {
    let result = async {
        let scope = customer_scope(&state, &tenant, query.project_id, id).await?;
        state.channel_service().read_network(&scope).await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn put_customer(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    input: Result<Json<NetworkUpdate>, JsonRejection>,
) -> Result<Json<NetworkView>, ApiError> {
    let result = async {
        require_project_writer(&auth)?;
        let scope = customer_scope(&state, &tenant, query.project_id, id).await?;
        let input = body(input)?;
        let head = state
            .channel_service()
            .update_network(&scope, input.expected_version, input.source)
            .await?;
        state.channel_service().network_view(&scope, head).await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn get_operator(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<NetworkView>, ApiError> {
    let result = async {
        let scope = operator_scope(&state, &auth, id).await?;
        state.channel_service().read_network(&scope).await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
pub(crate) async fn put_operator(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    input: Result<Json<NetworkUpdate>, JsonRejection>,
) -> Result<Json<NetworkView>, ApiError> {
    let result = async {
        let scope = operator_scope(&state, &auth, id).await?;
        let input = body(input)?;
        let head = state
            .channel_service()
            .update_network(&scope, input.expected_version, input.source)
            .await?;
        state.channel_service().network_view(&scope, head).await
    }
    .await;
    result
        .map(Json)
        .map_err(|e| api_error(e, context.request_id))
}
