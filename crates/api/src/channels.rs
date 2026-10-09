//! Project-scoped account management and interactive remote browser login.
//! Account identity is accepted only from a server-side connector completion.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Extension, OriginalUri, Path, Query, State, ws::WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
};
use chrono::{Duration, Utc};
use geo_domain::{
    AppError, ChannelAccount, ChannelAccountRecord, ChannelGroup, ChannelOwnerKind,
    ChannelRepository, ChannelSecret, ChannelSessionVersion, ChannelSettings,
    ChannelSettingsRecord, ChannelStatus, ErrorCode, LoginSession, PoolAccount, PoolAccountRecord,
    PoolAssignment, PoolGroup, PoolLoginSession, ProjectId, Role, TenantId, TenantScope,
    supported_channel,
};
use geo_provider::SecretEnvelope;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    ApiError, AppState, AuthContext, RequestContext, api_error,
    browser_bridge::{BrowserBridge, BrowserDesktopStatus, BrowserProxy},
    require_project_writer,
};

#[derive(Clone)]
pub struct ChannelService {
    pub repository: Arc<dyn ChannelRepository>,
    cipher: Option<Arc<SecretEnvelope>>,
    pub browser: Option<BrowserBridge>,
    operator_pool_tenant_id: Option<TenantId>,
}

impl ChannelService {
    pub fn development() -> Self {
        Self {
            repository: Arc::new(geo_domain::MemoryChannelRepository::default()),
            cipher: Some(Arc::new(SecretEnvelope::ephemeral())),
            browser: None,
            operator_pool_tenant_id: None,
        }
    }

    /// Safe temporary state during durable application assembly. It permits
    /// metadata reads but refuses to encrypt/decrypt until a stable key is
    /// explicitly supplied; the application should fail startup instead.
    pub fn unconfigured(repository: Arc<dyn ChannelRepository>) -> Self {
        Self {
            repository,
            cipher: None,
            browser: None,
            operator_pool_tenant_id: None,
        }
    }

    pub fn persistent(
        repository: Arc<dyn ChannelRepository>,
        key_hex: &str,
        browser: Option<BrowserBridge>,
    ) -> Result<Self, AppError> {
        let cipher = SecretEnvelope::from_hex_key(key_hex)
            .map_err(|_| AppError::invalid_request("GEO_CHANNEL_SECRET_KEY is invalid"))?;
        Ok(Self {
            repository,
            cipher: Some(Arc::new(cipher)),
            browser,
            operator_pool_tenant_id: None,
        })
    }

    pub fn with_browser(mut self, browser: BrowserBridge) -> Self {
        self.browser = Some(browser);
        self
    }

    pub fn with_operator_pool_tenant_id(mut self, tenant_id: TenantId) -> Self {
        self.operator_pool_tenant_id = Some(tenant_id);
        self
    }

    fn browser(&self) -> Result<&BrowserBridge, AppError> {
        self.browser
            .as_ref()
            .ok_or_else(|| AppError::capability_missing("browser login runner is not configured"))
    }

    fn aad(scope: &TenantScope, account_id: Uuid, purpose: &str) -> Vec<u8> {
        format!(
            "geo-channel-v1:{}:{}:{}:{}:{}",
            scope.operator_id,
            scope.tenant_id,
            scope
                .project_id
                .map(|id| id.as_uuid())
                .unwrap_or(Uuid::nil()),
            account_id,
            purpose
        )
        .into_bytes()
    }

    fn encrypt(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        purpose: &str,
        plaintext: &[u8],
    ) -> Result<ChannelSecret, AppError> {
        self.cipher
            .as_ref()
            .ok_or_else(|| AppError::capability_missing("channel secret key is not configured"))?
            .seal(&Self::aad(scope, account_id, purpose), plaintext)
            .map(ChannelSecret::new)
            .map_err(|_| AppError::new(ErrorCode::Internal, "channel secret encryption failed"))
    }

    fn decrypt(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        purpose: &str,
        secret: &ChannelSecret,
    ) -> Result<Vec<u8>, AppError> {
        self.cipher
            .as_ref()
            .ok_or_else(|| AppError::capability_missing("channel secret key is not configured"))?
            .open(
                &Self::aad(scope, account_id, purpose),
                secret.encrypted_bytes(),
            )
            .map_err(|_| AppError::new(ErrorCode::Internal, "channel secret authentication failed"))
    }

    async fn resolved_proxy(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        account_proxy: Option<&ChannelSecret>,
    ) -> Result<Option<BrowserProxy>, AppError> {
        let default_proxy = if account_proxy.is_none() {
            self.repository
                .get_settings(scope)
                .await?
                .and_then(|settings| settings.proxy)
        } else {
            None
        };
        account_proxy
            .map(|secret| (secret, account_id, "proxy"))
            .or(default_proxy
                .as_ref()
                .map(|secret| (secret, Uuid::nil(), "project_proxy")))
            .map(|(secret, owner, purpose)| {
                let bytes = self.decrypt(scope, owner, purpose, secret)?;
                let input: ProxyInput = serde_json::from_slice(&bytes)
                    .map_err(|_| AppError::new(ErrorCode::Internal, "stored proxy is invalid"))?;
                Ok(input.runner())
            })
            .transpose()
    }

    /// Opens a fresh server-only execution session from persisted encrypted
    /// browser state. Consumers must verify the identity again before any
    /// external action; a process restart never relies on old browser memory.
    pub async fn resume_account_browser(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
    ) -> Result<Uuid, AppError> {
        let browser = self.browser()?;
        let record = self.repository.get_account(scope, account_id).await?;
        if !record.account.enabled || record.account.status != ChannelStatus::Ready {
            return Err(AppError::conflict("channel account is not ready"));
        }
        let session = record
            .session
            .as_ref()
            .ok_or_else(|| AppError::conflict("channel account needs login"))?;
        let bytes = self.decrypt(scope, account_id, "session", session)?;
        let storage_state: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::new(ErrorCode::Internal, "stored browser session is invalid"))?;
        let proxy = self
            .resolved_proxy(scope, account_id, record.proxy.as_ref())
            .await?;
        let id = Uuid::new_v4();
        browser
            .start(id, &record.account.platform, proxy, Some(&storage_state))
            .await?;
        Ok(id)
    }

    /// Customer projects may use an operator resource only after the operator
    /// explicitly assigned that account to this exact tenant/project.
    pub async fn resolve_available_account(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
    ) -> Result<ChannelAccount, AppError> {
        match self.repository.get_account(scope, account_id).await {
            Ok(record) => Ok(record.account),
            Err(error) if error.code == ErrorCode::NotFound => {
                let project_id = scope
                    .project_id
                    .ok_or_else(|| AppError::forbidden("project scope is required"))?;
                self.repository
                    .list_assigned_pool_accounts(scope)
                    .await?
                    .into_iter()
                    .find(|account| account.account_id == account_id)
                    .map(|account| account.assigned_view(project_id))
                    .ok_or_else(|| AppError::not_found("channel account not assigned to project"))
            }
            Err(error) => Err(error),
        }
    }

    /// Customer projects may use an operator resource only after the operator
    /// explicitly assigned that account to this exact tenant/project.
    pub async fn resume_available_browser(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
    ) -> Result<Uuid, AppError> {
        self.resume_available_browser_with_renewal(scope, account_id)
            .await
            .map(|(session, _)| session)
    }

    pub(crate) async fn resume_available_browser_with_renewal(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
    ) -> Result<(Uuid, ChannelSessionVersion), AppError> {
        let prepared = self.prepare_available_browser(scope, account_id).await?;
        let version = prepared.session_version(account_id);
        let id = Uuid::new_v4();
        self.browser()?
            .start(
                id,
                &prepared.platform,
                prepared.proxy,
                Some(&prepared.storage_state),
            )
            .await?;
        Ok((id, version))
    }

    /// Accept only website-owned storage from a freshly verified same identity.
    /// A stale context loses the CAS instead of overwriting a reconnect.
    pub(crate) async fn persist_browser_renewal(
        &self,
        scope: &TenantScope,
        version: &mut ChannelSessionVersion,
        verified: &crate::browser_bridge::VerifiedBrowserSession,
    ) -> Result<bool, AppError> {
        if verified.identity.platform_account_id != version.platform_account_id {
            return Err(AppError::conflict("account identity changed"));
        }
        let bytes = serde_json::to_vec(&verified.storage_state)
            .map_err(|_| AppError::new(ErrorCode::Internal, "browser session invalid"))?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(AppError::invalid_request(
                "browser session exceeds maximum size",
            ));
        }
        let (secret_scope, purpose) = match version.owner_kind {
            ChannelOwnerKind::Customer => (scope.clone(), "session"),
            ChannelOwnerKind::OperatorPool => (
                TenantScope::new(
                    scope.operator_id,
                    self.operator_pool_tenant_id.ok_or_else(|| {
                        AppError::capability_missing("operator pool is not configured")
                    })?,
                    None,
                ),
                "pool_session",
            ),
        };
        let renewed = self.encrypt(&secret_scope, version.account_id, purpose, &bytes)?;
        let updated = self
            .repository
            .renew_session(scope, version, renewed.clone())
            .await?;
        if updated {
            version.session = renewed;
        }
        Ok(updated)
    }

    /// Freeze the actual browser network and account used by this send attempt.
    /// The returned opaque envelope is exclusively server-side; never include
    /// it (or its decrypted proxy credentials) in a response or receipt.
    pub(crate) async fn resume_available_browser_bound(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<(Uuid, ChannelSecret, ChannelSessionVersion), AppError> {
        let prepared = self.prepare_available_browser(scope, account_id).await?;
        let session_version = prepared.session_version(account_id);
        let version = self
            .publication_connector_version(&prepared.platform, "publish")
            .await?;
        let binding = PublicationBrowserBinding {
            version: 1,
            account_id,
            owner_kind: prepared.owner_kind,
            platform: prepared.platform.clone(),
            platform_account_id: prepared.platform_account_id.clone(),
            connector_version: version,
            effective_proxy: prepared.effective_proxy.clone(),
        };
        let bytes = serde_json::to_vec(&binding)
            .map_err(|_| AppError::new(ErrorCode::Internal, "publication binding invalid"))?;
        let sealed = self.encrypt(scope, attempt_id, "publication_execution", &bytes)?;
        let session_id = Uuid::new_v4();
        self.browser()?
            .start(
                session_id,
                &prepared.platform,
                prepared.proxy,
                Some(&prepared.storage_state),
            )
            .await?;
        Ok((session_id, sealed, session_version))
    }

    /// Restore only a currently authorized, usable session under the original
    /// publication's account identity, owner, network and connector version.
    /// The original platform account ID and connector version must be checked
    /// against the runner's completion/lookup receipt by the caller.
    pub(crate) async fn resume_publication_lookup_browser(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        attempt_id: Uuid,
        binding: &ChannelSecret,
    ) -> Result<(Uuid, String, String), AppError> {
        let bytes = self.decrypt(scope, attempt_id, "publication_execution", binding)?;
        let original: PublicationBrowserBinding = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::conflict("publication browser binding is invalid"))?;
        if original.version != 1
            || original.account_id != account_id
            || original.platform_account_id.trim().is_empty()
            || original.connector_version.trim().is_empty()
        {
            return Err(AppError::conflict(
                "publication browser binding has changed",
            ));
        }
        let current = self.prepare_available_browser(scope, account_id).await?;
        if original.owner_kind != current.owner_kind
            || original.platform != current.platform
            || original.platform_account_id != current.platform_account_id
            || original.effective_proxy != current.effective_proxy
        {
            return Err(AppError::conflict(
                "publication browser binding has changed",
            ));
        }
        if self
            .publication_connector_version(&current.platform, "lookup")
            .await?
            != original.connector_version
        {
            return Err(AppError::conflict(
                "publication browser binding has changed",
            ));
        }
        let session_id = Uuid::new_v4();
        self.browser()?
            .start(
                session_id,
                &current.platform,
                current.proxy,
                Some(&current.storage_state),
            )
            .await?;
        Ok((
            session_id,
            original.platform_account_id,
            original.connector_version,
        ))
    }

    /// Read only the immutable, encrypted pre-send version. In particular a
    /// lookup without a send receipt must never adopt the runner's current
    /// advertised version as the original send version.
    pub(crate) fn original_publication_connector_version(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        attempt_id: Uuid,
        binding: &ChannelSecret,
        platform: &str,
    ) -> Option<String> {
        let bytes = self
            .decrypt(scope, attempt_id, "publication_execution", binding)
            .ok()?;
        let original: PublicationBrowserBinding = serde_json::from_slice(&bytes).ok()?;
        (original.version == 1
            && original.account_id == account_id
            && original.platform == platform
            && !original.platform_account_id.trim().is_empty()
            && !original.connector_version.trim().is_empty()
            && original.connector_version.len() <= 100
            && !original.connector_version.starts_with("fixture"))
        .then_some(original.connector_version)
    }

    /// Restore current authorization, but never adopt a replacement identity.
    /// The caller owns the supplied session ID and must close even if start fails.
    pub(crate) async fn resume_provider_cleanup_browser(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
        identity: &geo_domain::ObservationProviderIdentity,
        session_id: Uuid,
    ) -> Result<(), AppError> {
        identity.validate()?;
        let current = self.prepare_available_browser(scope, account_id).await?;
        if current.platform != identity.provider
            || current.platform_account_id != identity.platform_account_id
        {
            return Err(AppError::conflict("cleanup browser identity has changed"));
        }
        self.browser()?
            .start(
                session_id,
                &current.platform,
                current.proxy,
                Some(&current.storage_state),
            )
            .await?;
        Ok(())
    }

    async fn publication_connector_version(
        &self,
        platform: &str,
        operation: &str,
    ) -> Result<String, AppError> {
        let capabilities = self.browser()?.capabilities().await?;
        let connector = capabilities.connectors.into_iter().find(|connector| {
            connector.platform == platform
                && connector.placement_slot == "primary"
                && connector.operations.iter().any(|op| op == operation)
                && !connector.connector_version.trim().is_empty()
                && connector.connector_version.len() <= 100
                && !connector.connector_version.starts_with("fixture")
        });
        connector
            .map(|connector| connector.connector_version)
            .ok_or_else(|| AppError::conflict("publication connector is unavailable"))
    }

    async fn prepare_available_browser(
        &self,
        scope: &TenantScope,
        account_id: Uuid,
    ) -> Result<PreparedPublicationBrowser, AppError> {
        self.browser()?;
        match self.repository.get_account(scope, account_id).await {
            Ok(record) => {
                if !record.account.enabled || record.account.status != ChannelStatus::Ready {
                    return Err(AppError::conflict("channel account is not ready"));
                }
                let platform_account_id = record
                    .account
                    .platform_account_id
                    .filter(|identity| !identity.trim().is_empty())
                    .ok_or_else(|| AppError::conflict("channel account needs login"))?;
                let session = record
                    .session
                    .as_ref()
                    .ok_or_else(|| AppError::conflict("channel account needs login"))?;
                let bytes = self.decrypt(scope, account_id, "session", session)?;
                let storage_state = serde_json::from_slice(&bytes).map_err(|_| {
                    AppError::new(ErrorCode::Internal, "stored browser session is invalid")
                })?;
                let proxy = self
                    .resolved_proxy(scope, account_id, record.proxy.as_ref())
                    .await?;
                let effective_proxy = serde_json::to_value(&proxy)
                    .map_err(|_| AppError::new(ErrorCode::Internal, "stored proxy is invalid"))?;
                Ok(PreparedPublicationBrowser {
                    encrypted_session: session.clone(),
                    owner_kind: ChannelOwnerKind::Customer,
                    platform: record.account.platform,
                    platform_account_id,
                    storage_state,
                    effective_proxy,
                    proxy,
                })
            }
            Err(error) if error.code == ErrorCode::NotFound => {
                let assigned = self
                    .repository
                    .list_assigned_pool_accounts(scope)
                    .await?
                    .iter()
                    .any(|account| account.account_id == account_id);
                if !assigned {
                    return Err(AppError::not_found(
                        "channel account not assigned to project",
                    ));
                }
                let pool_tenant = self.operator_pool_tenant_id.ok_or_else(|| {
                    AppError::capability_missing("operator pool is not configured")
                })?;
                let pool_scope = TenantScope::new(scope.operator_id, pool_tenant, None);
                let record = self
                    .repository
                    .get_pool_account(scope.operator_id, account_id)
                    .await?;
                if !record.account.enabled || record.account.status != ChannelStatus::Ready {
                    return Err(AppError::conflict("channel account is not ready"));
                }
                let platform_account_id = record
                    .account
                    .platform_account_id
                    .filter(|identity| !identity.trim().is_empty())
                    .ok_or_else(|| AppError::conflict("channel account needs login"))?;
                let session = record
                    .session
                    .as_ref()
                    .ok_or_else(|| AppError::conflict("channel account needs login"))?;
                let bytes = self.decrypt(&pool_scope, account_id, "pool_session", session)?;
                let storage_state = serde_json::from_slice(&bytes).map_err(|_| {
                    AppError::new(ErrorCode::Internal, "stored browser session is invalid")
                })?;
                let proxy = record
                    .proxy
                    .as_ref()
                    .map(|secret| {
                        let bytes = self.decrypt(&pool_scope, account_id, "pool_proxy", secret)?;
                        let proxy: ProxyInput = serde_json::from_slice(&bytes).map_err(|_| {
                            AppError::new(ErrorCode::Internal, "stored proxy is invalid")
                        })?;
                        Ok::<_, AppError>(proxy.runner())
                    })
                    .transpose()?;
                let effective_proxy = serde_json::to_value(&proxy)
                    .map_err(|_| AppError::new(ErrorCode::Internal, "stored proxy is invalid"))?;
                Ok(PreparedPublicationBrowser {
                    encrypted_session: session.clone(),
                    owner_kind: ChannelOwnerKind::OperatorPool,
                    platform: record.account.platform,
                    platform_account_id,
                    storage_state,
                    effective_proxy,
                    proxy,
                })
            }
            Err(error) => Err(error),
        }
    }
}

struct PreparedPublicationBrowser {
    encrypted_session: ChannelSecret,
    owner_kind: ChannelOwnerKind,
    platform: String,
    platform_account_id: String,
    storage_state: serde_json::Value,
    effective_proxy: serde_json::Value,
    proxy: Option<BrowserProxy>,
}

impl PreparedPublicationBrowser {
    fn session_version(&self, account_id: Uuid) -> ChannelSessionVersion {
        ChannelSessionVersion {
            account_id,
            owner_kind: self.owner_kind,
            platform: self.platform.clone(),
            platform_account_id: self.platform_account_id.clone(),
            session: self.encrypted_session.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationBrowserBinding {
    version: u8,
    account_id: Uuid,
    owner_kind: ChannelOwnerKind,
    platform: String,
    platform_account_id: String,
    connector_version: String,
    // Null means direct access. Any selected proxy includes credentials, all
    // protected by the attempt-scoped authenticated envelope.
    effective_proxy: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelQuery {
    pub project_id: ProjectId,
    #[serde(default)]
    #[allow(dead_code)]
    pub tenant_id: Option<String>,
}

#[derive(Serialize)]
pub struct ChannelList<T> {
    pub items: Vec<T>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlatformDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub purpose: &'static str,
    pub login_supported: bool,
}

pub async fn platforms() -> Json<ChannelList<PlatformDescriptor>> {
    Json(ChannelList {
        items: vec![
            PlatformDescriptor {
                id: "zhihu",
                label: "知乎创作中心",
                purpose: "publishing",
                login_supported: true,
            },
            PlatformDescriptor {
                id: "baidu_creator",
                label: "百度创作平台",
                purpose: "publishing",
                login_supported: true,
            },
            PlatformDescriptor {
                id: "xiaohongshu",
                label: "小红书创作中心",
                purpose: "publishing",
                login_supported: true,
            },
            PlatformDescriptor {
                id: "kimi",
                label: "Kimi 网页",
                purpose: "measurement",
                login_supported: true,
            },
        ],
    })
}

fn default_settings(project_id: ProjectId) -> ChannelSettings {
    ChannelSettings {
        project_id,
        default_group_id: None,
        proxy_configured: false,
        proxy_server: None,
        updated_at: Utc::now(),
    }
}

async fn scope(
    state: &AppState,
    tenant: &TenantScope,
    project_id: ProjectId,
) -> Result<TenantScope, AppError> {
    state
        .project_repository()
        .get(tenant, project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(TenantScope::new(
        tenant.operator_id,
        tenant.tenant_id,
        Some(project_id),
    ))
}

fn err(error: AppError, context: RequestContext) -> ApiError {
    api_error(error, context.request_id)
}

fn writer(auth: &AuthContext, context: RequestContext) -> Result<(), ApiError> {
    require_project_writer(auth).map_err(|error| err(error, context))
}

pub(crate) fn pool_tenant(
    service: &ChannelService,
    auth: &AuthContext,
) -> Result<TenantId, AppError> {
    let tenant_id = service
        .operator_pool_tenant_id
        .ok_or_else(|| AppError::forbidden("operator resource administration unavailable"))?;
    let authorized = auth.memberships.iter().any(|membership| {
        membership.active
            && membership.operator_id == auth.operator.id
            && membership.tenant_id == tenant_id
            && matches!(membership.role, Role::ResourceAdmin | Role::OemAdmin)
    });
    if !authorized {
        return Err(AppError::forbidden(
            "operator resource administrator required",
        ));
    }
    Ok(tenant_id)
}

fn trusted_pool_scope(
    service: &ChannelService,
    auth: &AuthContext,
) -> Result<TenantScope, AppError> {
    Ok(TenantScope::new(
        auth.operator.id,
        pool_tenant(service, auth)?,
        None,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewGroup {
    pub project_id: ProjectId,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupPatch {
    pub name: String,
}

fn checked_name(name: String) -> Result<String, AppError> {
    let name = name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(AppError::invalid_request(
            "group name must be 1–120 characters",
        ));
    }
    Ok(name)
}

pub async fn list_groups(
    State(state): State<AppState>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelList<ChannelGroup>>, ApiError> {
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let items = state
        .channel_service()
        .repository
        .list_groups(&scope)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(ChannelList { items }))
}

pub async fn create_group(
    State(state): State<AppState>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<NewGroup>,
) -> Result<(StatusCode, Json<ChannelGroup>), ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, input.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let group = ChannelGroup {
        group_id: Uuid::new_v4(),
        project_id: input.project_id,
        name: checked_name(input.name).map_err(|e| err(e, context))?,
        created_at: Utc::now(),
    };
    let group = state
        .channel_service()
        .repository
        .save_group(&scope, group)
        .await
        .map_err(|e| err(e, context))?;
    Ok((StatusCode::CREATED, Json(group)))
}

pub async fn patch_group(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<GroupPatch>,
) -> Result<Json<ChannelGroup>, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let mut group = state
        .channel_service()
        .repository
        .list_groups(&scope)
        .await
        .map_err(|e| err(e, context))?
        .into_iter()
        .find(|g| g.group_id == id)
        .ok_or_else(|| err(AppError::not_found("group not found"), context))?;
    group.name = checked_name(input.name).map_err(|e| err(e, context))?;
    let group = state
        .channel_service()
        .repository
        .save_group(&scope, group)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(group))
}

pub async fn delete_group(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<StatusCode, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .channel_service()
        .repository
        .delete_group(&scope, id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyInput {
    pub server: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProxyInput {
    fn validate(&self) -> Result<(), AppError> {
        let url = reqwest::Url::parse(&self.server)
            .map_err(|_| AppError::invalid_request("proxy URL is invalid"))?;
        if !matches!(url.scheme(), "http" | "https" | "socks5")
            || url.host_str().is_none()
            || url.port().is_none()
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || self.server.len() > 512
        {
            return Err(AppError::invalid_request(
                "proxy URL must contain scheme, host and port without credentials",
            ));
        }
        Ok(())
    }

    fn runner(self) -> BrowserProxy {
        BrowserProxy {
            server: self.server,
            username: self.username,
            password: self.password,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewAccount {
    pub project_id: ProjectId,
    pub platform: String,
    pub group_id: Option<Uuid>,
    pub proxy: Option<ProxyInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub group_id: Option<Option<Uuid>>,
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub proxy: Option<Option<ProxyInput>>,
}

fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    pub project_id: ProjectId,
    #[serde(default, deserialize_with = "nullable")]
    pub default_group_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "nullable")]
    pub proxy: Option<Option<ProxyInput>>,
}

pub async fn get_settings(
    State(state): State<AppState>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelSettings>, ApiError> {
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let settings = state
        .channel_service()
        .repository
        .get_settings(&scope)
        .await
        .map_err(|e| err(e, context))?
        .map(|record| record.settings)
        .unwrap_or_else(|| default_settings(query.project_id));
    Ok(Json(settings))
}

pub async fn patch_settings(
    State(state): State<AppState>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<SettingsPatch>,
) -> Result<Json<ChannelSettings>, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, input.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let service = state.channel_service();
    let mut record = service
        .repository
        .get_settings(&scope)
        .await
        .map_err(|e| err(e, context))?
        .unwrap_or(ChannelSettingsRecord {
            settings: default_settings(input.project_id),
            proxy: None,
        });
    if let Some(group) = input.default_group_id {
        record.settings.default_group_id = group;
    }
    if let Some(proxy) = input.proxy {
        if let Some(proxy) = proxy {
            proxy.validate().map_err(|e| err(e, context))?;
            record.settings.proxy_server = Some(proxy.server.clone());
            let bytes = serde_json::to_vec(&proxy).map_err(|_| {
                err(
                    AppError::new(ErrorCode::Internal, "proxy encoding failed"),
                    context,
                )
            })?;
            record.proxy = Some(
                service
                    .encrypt(&scope, Uuid::nil(), "project_proxy", &bytes)
                    .map_err(|e| err(e, context))?,
            );
            record.settings.proxy_configured = true;
        } else {
            record.proxy = None;
            record.settings.proxy_server = None;
            record.settings.proxy_configured = false;
        }
    }
    record.settings.updated_at = Utc::now();
    let settings = service
        .repository
        .save_settings(&scope, record)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(settings))
}

pub async fn list_accounts(
    State(state): State<AppState>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelList<ChannelAccount>>, ApiError> {
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let mut items = state
        .channel_service()
        .repository
        .list_accounts(&scope)
        .await
        .map_err(|e| err(e, context))?;
    items.extend(
        state
            .channel_service()
            .repository
            .list_assigned_pool_accounts(&scope)
            .await
            .map_err(|e| err(e, context))?
            .iter()
            .map(|account| account.assigned_view(query.project_id)),
    );
    Ok(Json(ChannelList { items }))
}

pub async fn create_account(
    State(state): State<AppState>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<NewAccount>,
) -> Result<(StatusCode, Json<ChannelAccount>), ApiError> {
    writer(&auth, context)?;
    if !supported_channel(&input.platform) {
        return Err(err(
            AppError::invalid_request("unsupported channel platform"),
            context,
        ));
    }
    let scope = scope(&state, &tenant, input.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let service = state.channel_service();
    let default_group = service
        .repository
        .get_settings(&scope)
        .await
        .map_err(|e| err(e, context))?
        .and_then(|record| record.settings.default_group_id);
    let id = Uuid::new_v4();
    let (proxy, proxy_server) = if let Some(proxy) = input.proxy {
        proxy.validate().map_err(|e| err(e, context))?;
        let server = proxy.server.clone();
        let bytes = serde_json::to_vec(&proxy).map_err(|_| {
            err(
                AppError::new(ErrorCode::Internal, "proxy encoding failed"),
                context,
            )
        })?;
        (
            Some(
                service
                    .encrypt(&scope, id, "proxy", &bytes)
                    .map_err(|e| err(e, context))?,
            ),
            Some(server),
        )
    } else {
        (None, None)
    };
    let now = Utc::now();
    let account = ChannelAccount {
        account_id: id,
        project_id: input.project_id,
        owner_kind: ChannelOwnerKind::Customer,
        platform: input.platform,
        group_id: input.group_id.or(default_group),
        status: ChannelStatus::NeedsLogin,
        display_name: None,
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: proxy.is_some(),
        proxy_server,
        created_at: now,
        updated_at: now,
    };
    let account = service
        .repository
        .save_account(
            &scope,
            ChannelAccountRecord {
                account,
                proxy,
                session: None,
            },
        )
        .await
        .map_err(|e| err(e, context))?;
    Ok((StatusCode::CREATED, Json(account)))
}

pub async fn patch_account(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<AccountPatch>,
) -> Result<Json<ChannelAccount>, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let service = state.channel_service();
    let mut record = service
        .repository
        .get_account(&scope, id)
        .await
        .map_err(|e| err(e, context))?;
    if let Some(group) = input.group_id {
        record.account.group_id = group;
    }
    if let Some(enabled) = input.enabled {
        record.account.enabled = enabled;
        record.account.status = if !enabled {
            ChannelStatus::Disabled
        } else if record.session.is_some() && record.account.platform_account_id.is_some() {
            ChannelStatus::Unverified
        } else {
            ChannelStatus::NeedsLogin
        };
    }
    if let Some(proxy) = input.proxy {
        match proxy {
            Some(proxy) => {
                proxy.validate().map_err(|e| err(e, context))?;
                record.account.proxy_server = Some(proxy.server.clone());
                let bytes = serde_json::to_vec(&proxy).map_err(|_| {
                    err(
                        AppError::new(ErrorCode::Internal, "proxy encoding failed"),
                        context,
                    )
                })?;
                record.proxy = Some(
                    service
                        .encrypt(&scope, id, "proxy", &bytes)
                        .map_err(|e| err(e, context))?,
                );
                record.account.proxy_configured = true;
            }
            None => {
                record.proxy = None;
                record.account.proxy_server = None;
                record.account.proxy_configured = false;
            }
        }
    }
    record.account.updated_at = Utc::now();
    let result = service
        .repository
        .save_account_metadata(&scope, record)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(result))
}

pub async fn delete_account(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<StatusCode, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .channel_service()
        .repository
        .delete_account(&scope, id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartLogin {
    pub project_id: ProjectId,
    pub account_id: Uuid,
}

#[derive(Serialize)]
pub struct LoginStarted {
    pub session_id: Uuid,
    pub account_id: Uuid,
    pub phase: &'static str,
}

#[derive(Serialize)]
pub struct LoginCompleted {
    pub account: ChannelAccount,
}

pub async fn start_login(
    State(state): State<AppState>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<StartLogin>,
) -> Result<(StatusCode, Json<LoginStarted>), ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, input.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let service = state.channel_service();
    let browser = service.browser().map_err(|e| err(e, context))?;
    let record = service
        .repository
        .get_account(&scope, input.account_id)
        .await
        .map_err(|e| err(e, context))?;
    if !record.account.enabled {
        return Err(err(AppError::conflict("account is disabled"), context));
    }
    let proxy = service
        .resolved_proxy(&scope, input.account_id, record.proxy.as_ref())
        .await
        .map_err(|e| err(e, context))?;
    // The browser runner must fail closed if the selected proxy cannot start.
    let session_id = Uuid::new_v4();
    browser
        .start(session_id, &record.account.platform, proxy, None)
        .await
        .map_err(|e| err(e, context))?;
    let session = LoginSession {
        session_id,
        account_id: input.account_id,
        project_id: input.project_id,
        created_at: Utc::now(),
    };
    if let Err(error) = service.repository.save_login(&scope, session).await {
        let _ = browser.close(session_id).await;
        return Err(err(error, context));
    }
    state
        .desktop_grants
        .bind(session_id, &auth, &tenant.tenant_id.to_string())
        .await;
    Ok((
        StatusCode::CREATED,
        Json(LoginStarted {
            session_id,
            account_id: input.account_id,
            phase: "awaiting_login",
        }),
    ))
}

async fn valid_login(
    state: &AppState,
    scope: &TenantScope,
    id: Uuid,
) -> Result<LoginSession, AppError> {
    let session = state
        .channel_service()
        .repository
        .get_login(scope, id)
        .await?;
    if Utc::now() - session.created_at > Duration::minutes(20) {
        state.desktop_grants.revoke(id).await;
        let _ = state.channel_service().browser()?.close(id).await;
        let _ = state
            .channel_service()
            .repository
            .delete_login(scope, id)
            .await;
        return Err(AppError::not_found("login session expired"));
    }
    Ok(session)
}

#[derive(Serialize)]
pub struct DesktopAuthorization {
    pub websocket_path: String,
    pub protocol: String,
}

pub async fn desktop_authorization(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    headers: HeaderMap,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<DesktopAuthorization>, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    valid_login(&state, &scope, id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .channel_service()
        .browser()
        .map_err(|e| err(e, context))?
        .desktop_status(id)
        .await
        .map_err(|e| err(e, context))?;
    crate::desktop_gateway::validate_desktop_origin(&headers, state.origin_config())
        .map_err(|e| err(e, context))?;
    let origin = crate::desktop_gateway::request_origin(&headers).map_err(|e| err(e, context))?;
    let protocol = state
        .desktop_grants
        .issue(id, &auth, &tenant.tenant_id.to_string(), origin)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(DesktopAuthorization {
        websocket_path: format!(
            "/api/v1/channel-login-sessions/{id}/desktop?project_id={}&tenant_id={}",
            query.project_id, tenant.tenant_id
        ),
        protocol,
    }))
}

// Axum extracts each independently authenticated request component here.
#[allow(clippy::too_many_arguments)]
pub async fn desktop_socket(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    upgrade: WebSocketUpgrade,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    writer(&auth, context)?;
    crate::desktop_gateway::validate_desktop_origin(&headers, state.origin_config())
        .map_err(|e| err(e, context))?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let session = valid_login(&state, &scope, id)
        .await
        .map_err(|e| err(e, context))?;
    let browser = state
        .channel_service()
        .browser()
        .map_err(|e| err(e, context))?
        .clone();
    let watch = crate::desktop_gateway::DesktopWatch {
        repository: state.auth_repository(),
        headers: headers.clone(),
        uri,
        customer: true,
        tenant: tenant.tenant_id.to_string(),
        expires_at: session.created_at + Duration::minutes(20),
    };
    crate::desktop_gateway::upgrade(
        upgrade,
        headers,
        state.desktop_grants.clone(),
        browser,
        id,
        &auth,
        &tenant.tenant_id.to_string(),
        context,
        watch,
    )
    .await
}

pub async fn desktop_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<BrowserDesktopStatus>, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    valid_login(&state, &scope, id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .desktop_grants
        .verify(id, &auth, &tenant.tenant_id.to_string())
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(
        state
            .channel_service()
            .browser()
            .map_err(|e| err(e, context))?
            .desktop_status(id)
            .await
            .map_err(|e| err(e, context))?,
    ))
}

pub async fn complete_login(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<LoginCompleted>, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    let session = valid_login(&state, &scope, id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .desktop_grants
        .verify(id, &auth, &tenant.tenant_id.to_string())
        .await
        .map_err(|e| err(e, context))?;
    let service = state.channel_service();
    let browser = service.browser().map_err(|e| err(e, context))?;
    let verified = browser.complete(id).await.map_err(|e| err(e, context))?;
    state.desktop_grants.revoke(id).await;
    if verified.identity.platform_account_id.trim().is_empty()
        || verified.identity.display_name.trim().is_empty()
    {
        return Err(err(
            AppError::conflict("connector did not verify account identity"),
            context,
        ));
    }
    let mut record = service
        .repository
        .get_account(&scope, session.account_id)
        .await
        .map_err(|e| err(e, context))?;
    if !record.account.enabled {
        return Err(err(AppError::conflict("account is disabled"), context));
    }
    if record
        .account
        .platform_account_id
        .as_ref()
        .is_some_and(|prior| prior != &verified.identity.platform_account_id)
    {
        return Err(err(
            AppError::conflict("login resolved to a different platform account"),
            context,
        ));
    }
    let bytes = serde_json::to_vec(&verified.storage_state).map_err(|_| {
        err(
            AppError::new(ErrorCode::Internal, "browser session invalid"),
            context,
        )
    })?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(err(
            AppError::invalid_request("browser session exceeds maximum size"),
            context,
        ));
    }
    record.session = Some(
        service
            .encrypt(&scope, session.account_id, "session", &bytes)
            .map_err(|e| err(e, context))?,
    );
    record.account.display_name = Some(verified.identity.display_name);
    record.account.avatar_url = verified.identity.avatar_url;
    record.account.platform_account_id = Some(verified.identity.platform_account_id);
    record.account.status = ChannelStatus::Ready;
    record.account.updated_at = Utc::now();
    let account = service
        .repository
        .save_account(&scope, record)
        .await
        .map_err(|e| err(e, context))?;
    let _ = browser.close(id).await;
    let _ = service.repository.delete_login(&scope, id).await;
    Ok(Json(LoginCompleted { account }))
}

pub async fn cancel_login(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ChannelQuery>,
    Extension(tenant): Extension<TenantScope>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<StatusCode, ApiError> {
    writer(&auth, context)?;
    let scope = scope(&state, &tenant, query.project_id)
        .await
        .map_err(|e| err(e, context))?;
    valid_login(&state, &scope, id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .desktop_grants
        .verify(id, &auth, &tenant.tenant_id.to_string())
        .await
        .map_err(|e| err(e, context))?;
    state.desktop_grants.revoke(id).await;
    state
        .channel_service()
        .browser()
        .map_err(|e| err(e, context))?
        .close(id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .channel_service()
        .repository
        .delete_login(&scope, id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPoolGroup {
    pub name: String,
}

pub async fn list_pool_groups(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelList<PoolGroup>>, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    let items = state
        .channel_service()
        .repository
        .list_pool_groups(auth.operator.id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(ChannelList { items }))
}

pub async fn create_pool_group(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<NewPoolGroup>,
) -> Result<(StatusCode, Json<PoolGroup>), ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    let group = PoolGroup {
        group_id: Uuid::new_v4(),
        name: checked_name(input.name).map_err(|e| err(e, context))?,
        created_at: Utc::now(),
    };
    let group = state
        .channel_service()
        .repository
        .save_pool_group(auth.operator.id, group)
        .await
        .map_err(|e| err(e, context))?;
    Ok((StatusCode::CREATED, Json(group)))
}

pub async fn patch_pool_group(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<GroupPatch>,
) -> Result<Json<PoolGroup>, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    let mut group = state
        .channel_service()
        .repository
        .list_pool_groups(auth.operator.id)
        .await
        .map_err(|e| err(e, context))?
        .into_iter()
        .find(|g| g.group_id == id)
        .ok_or_else(|| err(AppError::not_found("pool group not found"), context))?;
    group.name = checked_name(input.name).map_err(|e| err(e, context))?;
    Ok(Json(
        state
            .channel_service()
            .repository
            .save_pool_group(auth.operator.id, group)
            .await
            .map_err(|e| err(e, context))?,
    ))
}

pub async fn delete_pool_group(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<StatusCode, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    state
        .channel_service()
        .repository
        .delete_pool_group(auth.operator.id, id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPoolAccount {
    pub platform: String,
    pub group_id: Option<Uuid>,
    pub proxy: Option<ProxyInput>,
}

pub async fn list_pool_accounts(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelList<PoolAccount>>, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    let items = state
        .channel_service()
        .repository
        .list_pool_accounts(auth.operator.id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(ChannelList { items }))
}

pub async fn create_pool_account(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<NewPoolAccount>,
) -> Result<(StatusCode, Json<PoolAccount>), ApiError> {
    let service = state.channel_service();
    let pool_scope = trusted_pool_scope(service, &auth).map_err(|e| err(e, context))?;
    if !supported_channel(&input.platform) {
        return Err(err(
            AppError::invalid_request("unsupported channel platform"),
            context,
        ));
    }
    let id = Uuid::new_v4();
    let (proxy, proxy_server) = if let Some(proxy) = input.proxy {
        proxy.validate().map_err(|e| err(e, context))?;
        let server = proxy.server.clone();
        let bytes = serde_json::to_vec(&proxy).map_err(|_| {
            err(
                AppError::new(ErrorCode::Internal, "proxy encoding failed"),
                context,
            )
        })?;
        (
            Some(
                service
                    .encrypt(&pool_scope, id, "pool_proxy", &bytes)
                    .map_err(|e| err(e, context))?,
            ),
            Some(server),
        )
    } else {
        (None, None)
    };
    let now = Utc::now();
    let account = PoolAccount {
        account_id: id,
        platform: input.platform,
        group_id: input.group_id,
        status: ChannelStatus::NeedsLogin,
        display_name: None,
        platform_account_id: None,
        avatar_url: None,
        enabled: true,
        proxy_configured: proxy.is_some(),
        proxy_server,
        created_at: now,
        updated_at: now,
    };
    let account = service
        .repository
        .save_pool_account(
            auth.operator.id,
            PoolAccountRecord {
                account,
                session: None,
                proxy,
            },
        )
        .await
        .map_err(|e| err(e, context))?;
    Ok((StatusCode::CREATED, Json(account)))
}

pub async fn patch_pool_account(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<AccountPatch>,
) -> Result<Json<PoolAccount>, ApiError> {
    let service = state.channel_service();
    let pool_scope = trusted_pool_scope(service, &auth).map_err(|e| err(e, context))?;
    let mut record = service
        .repository
        .get_pool_account(auth.operator.id, id)
        .await
        .map_err(|e| err(e, context))?;
    if let Some(group) = input.group_id {
        record.account.group_id = group;
    }
    if let Some(enabled) = input.enabled {
        record.account.enabled = enabled;
        record.account.status = if !enabled {
            ChannelStatus::Disabled
        } else if record.session.is_some() && record.account.platform_account_id.is_some() {
            ChannelStatus::Unverified
        } else {
            ChannelStatus::NeedsLogin
        };
    }
    if let Some(proxy) = input.proxy {
        match proxy {
            Some(proxy) => {
                proxy.validate().map_err(|e| err(e, context))?;
                record.account.proxy_server = Some(proxy.server.clone());
                let bytes = serde_json::to_vec(&proxy).map_err(|_| {
                    err(
                        AppError::new(ErrorCode::Internal, "proxy encoding failed"),
                        context,
                    )
                })?;
                record.proxy = Some(
                    service
                        .encrypt(&pool_scope, id, "pool_proxy", &bytes)
                        .map_err(|e| err(e, context))?,
                );
                record.account.proxy_configured = true;
            }
            None => {
                record.proxy = None;
                record.account.proxy_server = None;
                record.account.proxy_configured = false;
            }
        }
    }
    record.account.updated_at = Utc::now();
    Ok(Json(
        service
            .repository
            .save_pool_account_metadata(auth.operator.id, record)
            .await
            .map_err(|e| err(e, context))?,
    ))
}

pub async fn delete_pool_account(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<StatusCode, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    state
        .channel_service()
        .repository
        .delete_pool_account(auth.operator.id, id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentTarget {
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
}

pub async fn list_pool_assignments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<ChannelList<PoolAssignment>>, ApiError> {
    pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    let items = state
        .channel_service()
        .repository
        .list_pool_assignments(auth.operator.id, id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(ChannelList { items }))
}

async fn set_assignment(
    state: &AppState,
    auth: &AuthContext,
    account_id: Uuid,
    target: AssignmentTarget,
    assigned: bool,
) -> Result<(), AppError> {
    pool_tenant(state.channel_service(), auth)?;
    let tenant = TenantScope::new(auth.operator.id, target.tenant_id, None);
    state
        .project_repository()
        .get(&tenant, target.project_id)
        .await?
        .ok_or_else(|| AppError::not_found("target project not found"))?;
    let project = TenantScope::new(auth.operator.id, target.tenant_id, Some(target.project_id));
    state
        .channel_service()
        .repository
        .assign_pool_account(&project, account_id, assigned)
        .await
}

pub async fn assign_pool_account(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(target): Json<AssignmentTarget>,
) -> Result<StatusCode, ApiError> {
    set_assignment(&state, &auth, id, target, true)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unassign_pool_account(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(target): Json<AssignmentTarget>,
) -> Result<StatusCode, ApiError> {
    set_assignment(&state, &auth, id, target, false)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartPoolLogin {
    pub account_id: Uuid,
}

pub async fn start_pool_login(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
    Json(input): Json<StartPoolLogin>,
) -> Result<(StatusCode, Json<LoginStarted>), ApiError> {
    let service = state.channel_service();
    let scope = trusted_pool_scope(service, &auth).map_err(|e| err(e, context))?;
    let record = service
        .repository
        .get_pool_account(auth.operator.id, input.account_id)
        .await
        .map_err(|e| err(e, context))?;
    if !record.account.enabled {
        return Err(err(AppError::conflict("account is disabled"), context));
    }
    let proxy = record
        .proxy
        .as_ref()
        .map(|secret| {
            let bytes = service.decrypt(&scope, input.account_id, "pool_proxy", secret)?;
            let input: ProxyInput = serde_json::from_slice(&bytes)
                .map_err(|_| AppError::new(ErrorCode::Internal, "stored proxy is invalid"))?;
            Ok::<_, AppError>(input.runner())
        })
        .transpose()
        .map_err(|e| err(e, context))?;
    let session_id = Uuid::new_v4();
    let browser = service.browser().map_err(|e| err(e, context))?;
    browser
        .start(session_id, &record.account.platform, proxy, None)
        .await
        .map_err(|e| err(e, context))?;
    if let Err(error) = service
        .repository
        .save_pool_login(
            auth.operator.id,
            PoolLoginSession {
                session_id,
                account_id: input.account_id,
                created_at: Utc::now(),
            },
        )
        .await
    {
        let _ = browser.close(session_id).await;
        return Err(err(error, context));
    }
    let pool_tenant = pool_tenant(service, &auth).map_err(|e| err(e, context))?;
    state
        .desktop_grants
        .bind(session_id, &auth, &pool_tenant.to_string())
        .await;
    Ok((
        StatusCode::CREATED,
        Json(LoginStarted {
            session_id,
            account_id: input.account_id,
            phase: "awaiting_login",
        }),
    ))
}

async fn valid_pool_login(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
) -> Result<PoolLoginSession, AppError> {
    pool_tenant(state.channel_service(), auth)?;
    let session = state
        .channel_service()
        .repository
        .get_pool_login(auth.operator.id, id)
        .await?;
    if Utc::now() - session.created_at > Duration::minutes(20) {
        state.desktop_grants.revoke(id).await;
        if let Ok(browser) = state.channel_service().browser() {
            let _ = browser.close(id).await;
        }
        let _ = state
            .channel_service()
            .repository
            .delete_pool_login(auth.operator.id, id)
            .await;
        return Err(AppError::not_found("login session expired"));
    }
    Ok(session)
}

pub async fn pool_desktop_authorization(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<DesktopAuthorization>, ApiError> {
    valid_pool_login(&state, &auth, id)
        .await
        .map_err(|e| err(e, context))?;
    let browser = state
        .channel_service()
        .browser()
        .map_err(|e| err(e, context))?;
    browser
        .desktop_status(id)
        .await
        .map_err(|e| err(e, context))?;
    crate::desktop_gateway::validate_desktop_origin(&headers, state.origin_config())
        .map_err(|e| err(e, context))?;
    let origin = crate::desktop_gateway::request_origin(&headers).map_err(|e| err(e, context))?;
    let tenant = crate::channels::pool_tenant(state.channel_service(), &auth)
        .map_err(|e| err(e, context))?;
    let protocol = state
        .desktop_grants
        .issue(id, &auth, &tenant.to_string(), origin)
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(DesktopAuthorization {
        websocket_path: format!("/api/v1/operator/channel-login-sessions/{id}/desktop"),
        protocol,
    }))
}

pub async fn pool_desktop_socket(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    upgrade: WebSocketUpgrade,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Response, ApiError> {
    let session = valid_pool_login(&state, &auth, id)
        .await
        .map_err(|e| err(e, context))?;
    crate::desktop_gateway::validate_desktop_origin(&headers, state.origin_config())
        .map_err(|e| err(e, context))?;
    let tenant = pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    let browser = state
        .channel_service()
        .browser()
        .map_err(|e| err(e, context))?
        .clone();
    let watch = crate::desktop_gateway::DesktopWatch {
        repository: state.auth_repository(),
        headers: headers.clone(),
        uri,
        customer: false,
        tenant: tenant.to_string(),
        expires_at: session.created_at + Duration::minutes(20),
    };
    crate::desktop_gateway::upgrade(
        upgrade,
        headers,
        state.desktop_grants.clone(),
        browser,
        id,
        &auth,
        &tenant.to_string(),
        context,
        watch,
    )
    .await
}

pub async fn pool_desktop_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<BrowserDesktopStatus>, ApiError> {
    valid_pool_login(&state, &auth, id)
        .await
        .map_err(|e| err(e, context))?;
    let tenant = pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    state
        .desktop_grants
        .verify(id, &auth, &tenant.to_string())
        .await
        .map_err(|e| err(e, context))?;
    Ok(Json(
        state
            .channel_service()
            .browser()
            .map_err(|e| err(e, context))?
            .desktop_status(id)
            .await
            .map_err(|e| err(e, context))?,
    ))
}

#[derive(Serialize)]
pub struct PoolLoginCompleted {
    pub account: PoolAccount,
}

pub async fn complete_pool_login(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<Json<PoolLoginCompleted>, ApiError> {
    let session = valid_pool_login(&state, &auth, id)
        .await
        .map_err(|e| err(e, context))?;
    let owner_tenant = pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    state
        .desktop_grants
        .verify(id, &auth, &owner_tenant.to_string())
        .await
        .map_err(|e| err(e, context))?;
    let service = state.channel_service();
    let scope = trusted_pool_scope(service, &auth).map_err(|e| err(e, context))?;
    let browser = service.browser().map_err(|e| err(e, context))?;
    let verified = browser.complete(id).await.map_err(|e| err(e, context))?;
    state.desktop_grants.revoke(id).await;
    if verified.identity.platform_account_id.trim().is_empty()
        || verified.identity.display_name.trim().is_empty()
    {
        return Err(err(
            AppError::conflict("connector did not verify account identity"),
            context,
        ));
    }
    let mut record = service
        .repository
        .get_pool_account(auth.operator.id, session.account_id)
        .await
        .map_err(|e| err(e, context))?;
    if !record.account.enabled {
        return Err(err(AppError::conflict("account is disabled"), context));
    }
    if record
        .account
        .platform_account_id
        .as_ref()
        .is_some_and(|prior| prior != &verified.identity.platform_account_id)
    {
        return Err(err(
            AppError::conflict("login resolved to a different platform account"),
            context,
        ));
    }
    let bytes = serde_json::to_vec(&verified.storage_state).map_err(|_| {
        err(
            AppError::new(ErrorCode::Internal, "browser session invalid"),
            context,
        )
    })?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(err(
            AppError::invalid_request("browser session exceeds maximum size"),
            context,
        ));
    }
    record.session = Some(
        service
            .encrypt(&scope, session.account_id, "pool_session", &bytes)
            .map_err(|e| err(e, context))?,
    );
    record.account.display_name = Some(verified.identity.display_name);
    record.account.avatar_url = verified.identity.avatar_url;
    record.account.platform_account_id = Some(verified.identity.platform_account_id);
    record.account.status = ChannelStatus::Ready;
    record.account.updated_at = Utc::now();
    let account = service
        .repository
        .save_pool_account(auth.operator.id, record)
        .await
        .map_err(|e| err(e, context))?;
    let _ = browser.close(id).await;
    let _ = service
        .repository
        .delete_pool_login(auth.operator.id, id)
        .await;
    Ok(Json(PoolLoginCompleted { account }))
}

pub async fn cancel_pool_login(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(auth): Extension<AuthContext>,
    Extension(context): Extension<RequestContext>,
) -> Result<StatusCode, ApiError> {
    valid_pool_login(&state, &auth, id)
        .await
        .map_err(|e| err(e, context))?;
    let owner_tenant = pool_tenant(state.channel_service(), &auth).map_err(|e| err(e, context))?;
    state
        .desktop_grants
        .verify(id, &auth, &owner_tenant.to_string())
        .await
        .map_err(|e| err(e, context))?;
    state.desktop_grants.revoke(id).await;
    state
        .channel_service()
        .browser()
        .map_err(|e| err(e, context))?
        .close(id)
        .await
        .map_err(|e| err(e, context))?;
    state
        .channel_service()
        .repository
        .delete_pool_login(auth.operator.id, id)
        .await
        .map_err(|e| err(e, context))?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn customer_routes() -> Router<AppState> {
    Router::new()
        .route("/channel-settings", get(get_settings).patch(patch_settings))
        .route("/channel-groups", get(list_groups).post(create_group))
        .route(
            "/channel-groups/{id}",
            axum::routing::patch(patch_group).delete(delete_group),
        )
        .route("/channel-accounts", get(list_accounts).post(create_account))
        .route(
            "/channel-accounts/{id}",
            axum::routing::patch(patch_account).delete(delete_account),
        )
        .route("/channel-login-sessions", axum::routing::post(start_login))
        .route("/channel-login-sessions/{id}/status", get(desktop_status))
        .route(
            "/channel-login-sessions/{id}/desktop-authorization",
            axum::routing::post(desktop_authorization),
        )
        .route("/channel-login-sessions/{id}/desktop", get(desktop_socket))
        .route(
            "/channel-login-sessions/{id}/complete",
            axum::routing::post(complete_login),
        )
        .route(
            "/channel-login-sessions/{id}",
            axum::routing::delete(cancel_login),
        )
}

pub fn operator_routes() -> Router<AppState> {
    Router::new()
        .route("/channel-platforms", get(platforms))
        .route(
            "/operator/channel-groups",
            get(list_pool_groups).post(create_pool_group),
        )
        .route(
            "/operator/channel-groups/{id}",
            axum::routing::patch(patch_pool_group).delete(delete_pool_group),
        )
        .route(
            "/operator/channel-accounts",
            get(list_pool_accounts).post(create_pool_account),
        )
        .route(
            "/operator/channel-accounts/{id}",
            axum::routing::patch(patch_pool_account).delete(delete_pool_account),
        )
        .route(
            "/operator/channel-accounts/{id}/assignments",
            get(list_pool_assignments)
                .post(assign_pool_account)
                .delete(unassign_pool_account),
        )
        .route(
            "/operator/channel-login-sessions",
            axum::routing::post(start_pool_login),
        )
        .route(
            "/operator/channel-login-sessions/{id}/status",
            get(pool_desktop_status),
        )
        .route(
            "/operator/channel-login-sessions/{id}/desktop-authorization",
            axum::routing::post(pool_desktop_authorization),
        )
        .route(
            "/operator/channel-login-sessions/{id}/desktop",
            get(pool_desktop_socket),
        )
        .route(
            "/operator/channel-login-sessions/{id}/complete",
            axum::routing::post(complete_pool_login),
        )
        .route(
            "/operator/channel-login-sessions/{id}",
            axum::routing::delete(cancel_pool_login),
        )
}

#[cfg(test)]
mod publication_binding_tests {
    use super::*;
    use axum::{Router, http::StatusCode, routing::any};
    use geo_domain::{DEVELOPMENT_OPERATOR_ID, DEVELOPMENT_TENANT_ID};
    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    #[derive(Clone, Default)]
    struct RunnerStub {
        starts: Arc<Mutex<Vec<Value>>>,
        version: Arc<Mutex<String>>,
        cleanup_events: Arc<Mutex<Vec<&'static str>>>,
        cleanup_failure: Arc<Mutex<bool>>,
    }

    async fn runner(
        State(stub): State<RunnerStub>,
        method: axum::http::Method,
        uri: axum::http::Uri,
        body: axum::body::Bytes,
    ) -> (StatusCode, Json<Value>) {
        if uri.path() == "/v1/capabilities" {
            return (
                StatusCode::OK,
                Json(json!({"connectors":[{
                    "platform":"zhihu","placement_slot":"primary",
                    "connector_version":stub.version.lock().await.clone(),
                    "operations":["publish","lookup"],"verified":false
                }]})),
            );
        }
        if method == axum::http::Method::POST && uri.path() == "/v1/sessions" {
            stub.cleanup_events.lock().await.push("start");
            let payload: Value = serde_json::from_slice(&body).unwrap();
            stub.starts.lock().await.push(payload.clone());
            return (
                StatusCode::OK,
                Json(json!({"session_id":payload["session_id"]})),
            );
        }
        if uri.path().ends_with("/complete") {
            stub.cleanup_events.lock().await.push("complete");
            return (
                StatusCode::OK,
                Json(json!({
                    "identity":{"platform_account_id":"original-identity","display_name":"Account","avatar_url":null},
                    "storage_state":{}
                })),
            );
        }
        if uri.path().ends_with("/cleanup-conversation") {
            let payload: Value = serde_json::from_slice(&body).unwrap();
            assert!(
                payload["authorization_ticket"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
            stub.cleanup_events.lock().await.push("rpc");
            let failure = *stub.cleanup_failure.lock().await;
            return (
                StatusCode::OK,
                Json(json!({
                    "execution_id":payload["execution_id"],
                    "external_conversation_id":payload["external_conversation_id"],
                    "status": if failure { "retained" } else { "deleted" },
                    "diagnostic": if failure {
                        json!({"stage":"delete","code":"http_error"})
                    } else { Value::Null }
                })),
            );
        }
        if method == axum::http::Method::DELETE {
            stub.cleanup_events.lock().await.push("close");
        }
        (StatusCode::OK, Json(json!({"closed":true})))
    }

    async fn fixture() -> (
        ChannelService,
        TenantScope,
        Uuid,
        RunnerStub,
        tokio::task::JoinHandle<()>,
    ) {
        let stub = RunnerStub {
            version: Arc::new(Mutex::new("connector.v1".into())),
            ..RunnerStub::default()
        };
        let app = Router::new().fallback(any(runner)).with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let browser =
            BrowserBridge::new(format!("http://{address}"), "runner-token".into()).unwrap();
        let service = ChannelService::development().with_browser(browser);
        let scope = TenantScope::new(
            DEVELOPMENT_OPERATOR_ID,
            DEVELOPMENT_TENANT_ID,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        let account_id = Uuid::new_v4();
        service
            .repository
            .save_account(
                &scope,
                ChannelAccountRecord {
                    account: ChannelAccount {
                        account_id,
                        project_id: scope.project_id.unwrap(),
                        owner_kind: ChannelOwnerKind::Customer,
                        platform: "zhihu".into(),
                        group_id: None,
                        status: ChannelStatus::Ready,
                        display_name: None,
                        platform_account_id: Some("original-identity".into()),
                        avatar_url: None,
                        enabled: true,
                        proxy_configured: false,
                        proxy_server: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    session: Some(
                        service
                            .encrypt(
                                &scope,
                                account_id,
                                "session",
                                br#"{"cookies":[{"name":"old"}]}"#,
                            )
                            .unwrap(),
                    ),
                    proxy: None,
                },
            )
            .await
            .unwrap();
        (service, scope, account_id, stub, server)
    }

    #[tokio::test]
    async fn renewed_storage_is_encrypted_and_verified_without_metadata_changes() {
        let (service, scope, account_id, _, server) = fixture().await;
        let before = service
            .repository
            .get_account(&scope, account_id)
            .await
            .unwrap();
        let (_, mut version) = service
            .resume_available_browser_with_renewal(&scope, account_id)
            .await
            .unwrap();
        let mut verified = crate::browser_bridge::VerifiedBrowserSession {
            identity: crate::browser_bridge::BrowserIdentity {
                platform_account_id: "different-identity".into(),
                display_name: "Not adopted".into(),
                avatar_url: None,
            },
            storage_state: json!({"cookies":[{"name":"renewed"}],"origins":[]}),
        };
        assert!(
            service
                .persist_browser_renewal(&scope, &mut version, &verified)
                .await
                .is_err()
        );
        assert_eq!(
            service
                .repository
                .get_account(&scope, account_id)
                .await
                .unwrap()
                .session
                .unwrap()
                .encrypted_bytes(),
            before.session.as_ref().unwrap().encrypted_bytes()
        );
        verified.identity.platform_account_id = "original-identity".into();
        assert!(
            service
                .persist_browser_renewal(&scope, &mut version, &verified)
                .await
                .unwrap()
        );
        let after = service
            .repository
            .get_account(&scope, account_id)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(before.account).unwrap(),
            serde_json::to_value(after.account).unwrap()
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &service
                    .decrypt(
                        &scope,
                        account_id,
                        "session",
                        after.session.as_ref().unwrap()
                    )
                    .unwrap()
            )
            .unwrap(),
            verified.storage_state
        );
        server.abort();
    }

    #[tokio::test]
    async fn cleanup_restore_checks_original_identity_before_starting() {
        let (service, scope, account_id, stub, server) = fixture().await;
        let mut identity = geo_domain::ObservationProviderIdentity {
            provider: "zhihu".into(),
            platform_account_id: "different-identity".into(),
        };
        assert!(
            service
                .resume_provider_cleanup_browser(&scope, account_id, &identity, Uuid::new_v4())
                .await
                .is_err()
        );
        assert!(stub.starts.lock().await.is_empty());
        identity.platform_account_id = "original-identity".into();
        identity.provider = "other".into();
        assert!(
            service
                .resume_provider_cleanup_browser(&scope, account_id, &identity, Uuid::new_v4())
                .await
                .is_err()
        );
        assert!(stub.starts.lock().await.is_empty());
        identity.provider = "zhihu".into();
        let session_id = Uuid::new_v4();
        service
            .resume_provider_cleanup_browser(&scope, account_id, &identity, session_id)
            .await
            .unwrap();
        assert_eq!(stub.starts.lock().await.len(), 1);
        assert_eq!(
            stub.starts.lock().await[0]["session_id"],
            session_id.to_string()
        );
        server.abort();
    }

    struct CleanupStore {
        claim: geo_domain::ProviderCleanupClaim,
        jobs: Arc<dyn geo_domain::ChannelJobRepository>,
        events: Arc<Mutex<Vec<&'static str>>>,
        complete_evidence: bool,
        finished: tokio::sync::Notify,
        outcome: Mutex<Option<geo_domain::ProviderCleanupOutcome>>,
        diagnostic: Mutex<Option<geo_domain::ProviderCleanupDiagnostic>>,
    }

    #[async_trait::async_trait]
    impl geo_domain::ProviderConversationCleanupRepository for CleanupStore {
        async fn scan_unqueued(
            &self,
            _: chrono::DateTime<Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<geo_domain::ProviderCleanupBackfillItem>, AppError> {
            unreachable!()
        }
        async fn scan_due(
            &self,
            _: chrono::DateTime<Utc>,
            _: Option<Uuid>,
            _: usize,
        ) -> Result<Vec<geo_domain::ProviderCleanupDueItem>, AppError> {
            unreachable!()
        }
        async fn enqueue(&self, _: &TenantScope, _: Uuid) -> Result<Uuid, AppError> {
            unreachable!()
        }
        async fn claim(
            &self,
            _: &TenantScope,
            cleanup_id: Uuid,
        ) -> Result<Option<geo_domain::ProviderCleanupClaim>, AppError> {
            assert_eq!(cleanup_id, self.claim.cleanup_id);
            self.events.lock().await.push("claim");
            Ok(Some(self.claim.clone()))
        }
        async fn claim_due(
            &self,
            _: &TenantScope,
        ) -> Result<Option<geo_domain::ProviderCleanupClaim>, AppError> {
            unreachable!()
        }
        async fn authorize_delete(
            &self,
            scope: &TenantScope,
            cleanup_id: Uuid,
            lease_id: Uuid,
            reservation_id: Uuid,
        ) -> Result<geo_domain::ProviderCleanupClaim, AppError> {
            assert_eq!(cleanup_id, self.claim.cleanup_id);
            assert_eq!(lease_id, self.claim.lease_id);
            assert!(!reservation_id.is_nil());
            let now = Utc::now();
            // A competing execution really is excluded before authorization.
            let conflict = self
                .jobs
                .reserve_account(
                    scope,
                    self.claim.account_id,
                    Uuid::new_v4(),
                    now,
                    now + Duration::minutes(2),
                )
                .await
                .unwrap_err();
            assert_eq!(conflict.code, ErrorCode::Conflict);
            self.events.lock().await.extend(["reserve", "authorize"]);
            if self.complete_evidence {
                Ok(self.claim.clone())
            } else {
                Err(AppError::conflict("retained evidence incomplete"))
            }
        }
        async fn finish(
            &self,
            scope: &TenantScope,
            cleanup_id: Uuid,
            lease_id: Uuid,
            outcome: geo_domain::ProviderCleanupOutcome,
        ) -> Result<(), AppError> {
            assert_eq!(cleanup_id, self.claim.cleanup_id);
            assert_eq!(lease_id, self.claim.lease_id);
            let reservation = Uuid::new_v4();
            let now = Utc::now();
            // Finish occurs after close/release, or after preflight rejects
            // incomplete evidence without ever starting a browser.
            self.jobs
                .reserve_account(
                    scope,
                    self.claim.account_id,
                    reservation,
                    now,
                    now + Duration::minutes(2),
                )
                .await
                .unwrap();
            self.jobs
                .release_account(scope, self.claim.account_id, reservation)
                .await
                .unwrap();
            self.events.lock().await.extend(["release", "finish"]);
            *self.outcome.lock().await = Some(outcome);
            self.finished.notify_one();
            Ok(())
        }
        async fn finish_with_diagnostic(
            &self,
            scope: &TenantScope,
            cleanup_id: Uuid,
            lease_id: Uuid,
            outcome: geo_domain::ProviderCleanupOutcome,
            diagnostic: Option<geo_domain::ProviderCleanupDiagnostic>,
        ) -> Result<(), AppError> {
            *self.diagnostic.lock().await = diagnostic;
            self.finish(scope, cleanup_id, lease_id, outcome).await
        }
    }

    async fn cleanup_dispatch_fixture(complete_evidence: bool, failure: bool) {
        use geo_domain::{
            ObservationProviderIdentity, ProviderCleanupAction, ProviderCleanupClaim,
            ProviderCleanupOutcome,
        };
        let (service, scope, account_id, stub, server) = fixture().await;
        *stub.cleanup_failure.lock().await = failure;
        let mut account = service
            .repository
            .get_account(&scope, account_id)
            .await
            .unwrap();
        account.account.platform = "kimi".into();
        service
            .repository
            .save_account(&scope, account)
            .await
            .unwrap();
        let state = AppState::development().with_channel_service(service);
        let repository = Arc::new(CleanupStore {
            claim: ProviderCleanupClaim {
                cleanup_id: Uuid::new_v4(),
                capture_id: Uuid::new_v4(),
                account_id,
                provider: "kimi".into(),
                external_conversation_id: "system-conversation".into(),
                retained_message_inventory_sha256: Some(geo_domain::sha256_hex(
                    br#"[["synthetic-message","assistant"]]"#,
                )),
                original_identity: ObservationProviderIdentity {
                    provider: "kimi".into(),
                    platform_account_id: "original-identity".into(),
                },
                lease_id: Uuid::new_v4(),
                lease_until: Utc::now() + Duration::minutes(2),
                action: ProviderCleanupAction::Delete,
            },
            jobs: state.channel_job_repository(),
            events: stub.cleanup_events.clone(),
            complete_evidence,
            finished: tokio::sync::Notify::new(),
            outcome: Mutex::new(None),
            diagnostic: Mutex::new(None),
        });
        let callbacks = crate::ProviderCleanupCallbackService::new(
            repository.clone(),
            Arc::new(SecretEnvelope::from_hex_key(&"12".repeat(32)).unwrap()),
            &"service-test-token".repeat(3),
        )
        .unwrap();
        let state = state.with_provider_cleanup_callback(callbacks);
        assert!(
            crate::dispatch_provider_conversation_cleanup(
                state,
                repository.clone(),
                scope,
                repository.claim.cleanup_id
            )
            .await
            .unwrap()
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            repository.finished.notified(),
        )
        .await
        .unwrap();
        let expected = if complete_evidence {
            vec![
                "claim",
                "reserve",
                "authorize",
                "start",
                "complete",
                "rpc",
                "close",
                "release",
                "finish",
            ]
        } else {
            vec!["claim", "reserve", "authorize", "release", "finish"]
        };
        assert_eq!(*stub.cleanup_events.lock().await, expected);
        assert_eq!(
            *repository.outcome.lock().await,
            Some(if complete_evidence && !failure {
                ProviderCleanupOutcome::Deleted
            } else {
                ProviderCleanupOutcome::Failed
            })
        );
        assert_eq!(
            *repository.diagnostic.lock().await,
            if !complete_evidence {
                Some(geo_domain::ProviderCleanupDiagnostic {
                    stage: geo_domain::ProviderCleanupStage::Authorization,
                    code: geo_domain::ProviderCleanupCode::AuthorizationRequired,
                })
            } else if failure {
                Some(geo_domain::ProviderCleanupDiagnostic {
                    stage: geo_domain::ProviderCleanupStage::Delete,
                    code: geo_domain::ProviderCleanupCode::HttpError,
                })
            } else {
                None
            }
        );
        server.abort();
    }

    #[tokio::test]
    async fn cleanup_dispatch_orders_reservation_authorization_identity_rpc_close_and_finish() {
        cleanup_dispatch_fixture(true, false).await;
    }

    #[tokio::test]
    async fn cleanup_dispatch_incomplete_evidence_releases_without_start_or_model_call() {
        cleanup_dispatch_fixture(false, false).await;
    }

    #[tokio::test]
    async fn cleanup_dispatch_retained_receipt_preserves_diagnostic_through_finish() {
        cleanup_dispatch_fixture(true, true).await;
    }

    async fn set_default_proxy(
        service: &ChannelService,
        scope: &TenantScope,
        password: Option<&str>,
    ) {
        let proxy = password.map(|password| ProxyInput {
            server: "http://127.0.0.1:9191".into(),
            username: Some("proxy-user".into()),
            password: Some(password.into()),
        });
        service
            .repository
            .save_settings(
                scope,
                ChannelSettingsRecord {
                    settings: ChannelSettings {
                        project_id: scope.project_id.unwrap(),
                        default_group_id: None,
                        proxy_configured: proxy.is_some(),
                        proxy_server: proxy.as_ref().map(|proxy| proxy.server.clone()),
                        updated_at: Utc::now(),
                    },
                    proxy: proxy.map(|proxy| {
                        service
                            .encrypt(
                                scope,
                                Uuid::nil(),
                                "project_proxy",
                                &serde_json::to_vec(&proxy).unwrap(),
                            )
                            .unwrap()
                    }),
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn original_proxy_and_identity_survive_session_rotation_but_not_network_or_scope_changes()
    {
        let (service, scope, account_id, stub, server) = fixture().await;
        set_default_proxy(&service, &scope, Some("proxy-secret")).await;
        let attempt = Uuid::new_v4();
        let (_, bound, _) = service
            .resume_available_browser_bound(&scope, account_id, attempt)
            .await
            .unwrap();
        assert_eq!(
            stub.starts.lock().await[0]["proxy"]["password"],
            "proxy-secret"
        );
        assert!(!String::from_utf8_lossy(bound.encrypted_bytes()).contains("proxy-secret"));
        let mut record = service
            .repository
            .get_account(&scope, account_id)
            .await
            .unwrap();
        record.session = Some(
            service
                .encrypt(
                    &scope,
                    account_id,
                    "session",
                    br#"{"cookies":[{"name":"renewed"}]}"#,
                )
                .unwrap(),
        );
        service
            .repository
            .save_account(&scope, record.clone())
            .await
            .unwrap();
        let (_, identity, version) = service
            .resume_publication_lookup_browser(&scope, account_id, attempt, &bound)
            .await
            .unwrap();
        assert_eq!(
            (identity.as_str(), version.as_str()),
            ("original-identity", "connector.v1")
        );
        assert_eq!(
            stub.starts.lock().await[1]["storage_state"]["cookies"][0]["name"],
            "renewed"
        );
        assert!(
            service
                .resume_publication_lookup_browser(&scope, account_id, Uuid::new_v4(), &bound)
                .await
                .is_err()
        );
        let other_scope = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(ProjectId::new(Uuid::new_v4())),
        );
        assert!(
            service
                .resume_publication_lookup_browser(&other_scope, account_id, attempt, &bound)
                .await
                .is_err()
        );
        set_default_proxy(&service, &scope, None).await;
        assert_eq!(
            service
                .resume_publication_lookup_browser(&scope, account_id, attempt, &bound)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        set_default_proxy(&service, &scope, Some("changed-secret")).await;
        assert!(
            service
                .resume_publication_lookup_browser(&scope, account_id, attempt, &bound)
                .await
                .is_err()
        );
        set_default_proxy(&service, &scope, Some("proxy-secret")).await;
        record.account.platform_account_id = Some("changed-identity".into());
        service
            .repository
            .save_account(&scope, record.clone())
            .await
            .unwrap();
        assert!(
            service
                .resume_publication_lookup_browser(&scope, account_id, attempt, &bound)
                .await
                .is_err()
        );
        record.account.platform_account_id = Some("original-identity".into());
        service
            .repository
            .save_account(&scope, record)
            .await
            .unwrap();
        *stub.version.lock().await = "connector.v2".into();
        assert!(
            service
                .resume_publication_lookup_browser(&scope, account_id, attempt, &bound)
                .await
                .is_err()
        );
        assert_eq!(stub.starts.lock().await.len(), 2);
        server.abort();
    }

    #[tokio::test]
    async fn revoked_pool_assignment_and_owner_change_cannot_restore_browser() {
        let (service, scope, account_id, stub, server) = fixture().await;
        let pool_tenant = TenantId::new(Uuid::new_v4());
        let service = service.with_operator_pool_tenant_id(pool_tenant);
        let pool_scope = TenantScope::new(scope.operator_id, pool_tenant, None);
        let pool_id = Uuid::new_v4();
        service
            .repository
            .save_pool_account(
                scope.operator_id,
                PoolAccountRecord {
                    account: PoolAccount {
                        account_id: pool_id,
                        platform: "zhihu".into(),
                        group_id: None,
                        status: ChannelStatus::Ready,
                        display_name: None,
                        platform_account_id: Some("pool-identity".into()),
                        avatar_url: None,
                        enabled: true,
                        proxy_configured: false,
                        proxy_server: None,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    session: Some(
                        service
                            .encrypt(&pool_scope, pool_id, "pool_session", br#"{"cookies":[]}"#)
                            .unwrap(),
                    ),
                    proxy: None,
                },
            )
            .await
            .unwrap();
        service
            .repository
            .assign_pool_account(&scope, pool_id, true)
            .await
            .unwrap();
        let attempt = Uuid::new_v4();
        let (_, bound, mut version) = service
            .resume_available_browser_bound(&scope, pool_id, attempt)
            .await
            .unwrap();
        let verified = crate::browser_bridge::VerifiedBrowserSession {
            identity: crate::browser_bridge::BrowserIdentity {
                platform_account_id: "pool-identity".into(),
                display_name: "Not adopted".into(),
                avatar_url: None,
            },
            storage_state: json!({"cookies":[{"name":"renewed"}]}),
        };
        assert!(
            service
                .persist_browser_renewal(&scope, &mut version, &verified)
                .await
                .unwrap()
        );
        let retained = service
            .repository
            .get_pool_account(scope.operator_id, pool_id)
            .await
            .unwrap();
        assert!(retained.account.display_name.is_none());
        assert_eq!(
            serde_json::from_slice::<Value>(
                &service
                    .decrypt(
                        &pool_scope,
                        pool_id,
                        "pool_session",
                        retained.session.as_ref().unwrap()
                    )
                    .unwrap()
            )
            .unwrap(),
            verified.storage_state
        );
        assert!(
            service
                .repository
                .get_account(&scope, pool_id)
                .await
                .is_err()
        );
        service
            .repository
            .assign_pool_account(&scope, pool_id, false)
            .await
            .unwrap();
        assert!(
            !service
                .persist_browser_renewal(&scope, &mut version, &verified)
                .await
                .unwrap()
        );
        assert!(
            service
                .resume_publication_lookup_browser(&scope, pool_id, attempt, &bound)
                .await
                .is_err()
        );
        let mut customer = service
            .repository
            .get_account(&scope, account_id)
            .await
            .unwrap();
        customer.account.account_id = pool_id;
        customer.account.platform_account_id = Some("pool-identity".into());
        customer.session = Some(
            service
                .encrypt(&scope, pool_id, "session", br#"{"cookies":[]}"#)
                .unwrap(),
        );
        assert_eq!(
            service
                .repository
                .save_account(&scope, customer)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        assert!(
            service
                .resume_publication_lookup_browser(&scope, pool_id, attempt, &bound)
                .await
                .is_err()
        );
        assert_eq!(stub.starts.lock().await.len(), 1);
        server.abort();
    }
}
