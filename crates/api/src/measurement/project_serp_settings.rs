//! Redacted project source settings and exact-version credential resolution.
use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    middleware,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ProjectId, ProjectSerpDispatchSource, ProjectSerpProvider, ProjectSerpSettingsCursor,
    ProjectSerpSettingsRecord, ProjectSerpSettingsRepository, ProjectSerpSettingsWrite, Role,
    SerpProtocol, TenantScope, project_serp_credential_aad,
};
use geo_provider::{
    SecretEnvelope,
    dataforseo::{DataForSeoClient, DataForSeoConnectionStatus},
};
use serde::{Deserialize, Serialize};

use crate::{
    ApiError, AppState, AuthContext, DataForSeoSerpConfig, DataForSeoSerpSource, RequestContext,
    SerpCapability, SerpSource, api_error,
    serp::{ResolvedSerpSource, SerpSourceResolver},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSerpCredentials {
    pub login: String,
    pub password: String,
}
impl std::fmt::Debug for ProjectSerpCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProjectSerpCredentials([redacted])")
    }
}

#[async_trait]
pub trait ProjectSerpSourceFactory: Send + Sync {
    fn source(
        &self,
        credentials: &ProjectSerpCredentials,
        protocol: &SerpProtocol,
    ) -> Result<Arc<dyn SerpSource>, AppError>;
    async fn test(&self, credentials: &ProjectSerpCredentials) -> DataForSeoConnectionStatus;
}

struct DataForSeoSourceFactory;

fn provider_client(credentials: &ProjectSerpCredentials) -> Result<DataForSeoClient, AppError> {
    DataForSeoClient::new(credentials.login.clone(), credentials.password.clone())
        .map_err(|_| AppError::invalid_request("invalid search credentials"))
}

fn source_config(protocol: &SerpProtocol) -> Result<DataForSeoSerpConfig, AppError> {
    let location_code: u32 = protocol
        .source_location_code
        .parse()
        .map_err(|_| AppError::invalid_request("invalid search location code"))?;
    if location_code == 0
        || protocol.language.is_empty()
        || protocol.language.len() > 16
        || !protocol
            .language
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
    {
        return Err(AppError::invalid_request("invalid search source locale"));
    }
    Ok(DataForSeoSerpConfig {
        location_code,
        country: protocol.country.clone(),
        city: protocol.city.clone(),
        language_code: protocol.language.clone(),
    })
}

#[async_trait]
impl ProjectSerpSourceFactory for DataForSeoSourceFactory {
    fn source(
        &self,
        credentials: &ProjectSerpCredentials,
        protocol: &SerpProtocol,
    ) -> Result<Arc<dyn SerpSource>, AppError> {
        let source = DataForSeoSerpSource::new(
            Arc::new(provider_client(credentials)?),
            source_config(protocol)?,
        )?;
        if source.protocol(&protocol.query) != *protocol {
            return Err(AppError::invalid_request(
                "unsupported search source protocol",
            ));
        }
        Ok(Arc::new(source))
    }
    async fn test(&self, credentials: &ProjectSerpCredentials) -> DataForSeoConnectionStatus {
        match provider_client(credentials) {
            Ok(client) => client.test_connection().await,
            Err(_) => DataForSeoConnectionStatus::InvalidResponse,
        }
    }
}

#[derive(Clone)]
pub struct ProjectSerpSettingsService {
    repository: Arc<dyn ProjectSerpSettingsRepository>,
    cipher: Option<Arc<SecretEnvelope>>,
    factory: Arc<dyn ProjectSerpSourceFactory>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectSerpSettings {
    pub expected_revision: i64,
    pub enabled: bool,
    pub protocol_defaults: SerpProtocol,
    pub login: Option<String>,
    pub password: Option<String>,
}
impl std::fmt::Debug for UpdateProjectSerpSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UpdateProjectSerpSettings([redacted])")
    }
}

#[derive(Serialize)]
pub struct ProjectSerpSettingsView {
    #[serde(flatten)]
    pub settings: ProjectSerpSettingsRecord,
    pub credentials_present: bool,
}
#[derive(Serialize)]
pub struct ProjectSerpSettingsPage {
    pub items: Vec<ProjectSerpSettingsView>,
    pub encryption_available: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestProjectSerpSettings {
    pub expected_revision: i64,
}
#[derive(Serialize)]
pub struct ProjectSerpTestResult {
    pub source_key: String,
    pub revision: i64,
    pub status: DataForSeoConnectionStatus,
    pub checked_at: DateTime<Utc>,
}

impl ProjectSerpSettingsService {
    pub fn unconfigured(repository: Arc<dyn ProjectSerpSettingsRepository>) -> Self {
        Self {
            repository,
            cipher: None,
            factory: Arc::new(DataForSeoSourceFactory),
        }
    }
    pub fn persistent(
        repository: Arc<dyn ProjectSerpSettingsRepository>,
        key_hex: &str,
    ) -> Result<Self, AppError> {
        let cipher = SecretEnvelope::from_hex_key(key_hex)
            .map_err(|_| AppError::invalid_request("search settings encryption key invalid"))?;
        Ok(Self {
            repository,
            cipher: Some(Arc::new(cipher)),
            factory: Arc::new(DataForSeoSourceFactory),
        })
    }
    pub fn development(repository: Arc<dyn ProjectSerpSettingsRepository>) -> Self {
        Self {
            repository,
            cipher: Some(Arc::new(SecretEnvelope::ephemeral())),
            factory: Arc::new(DataForSeoSourceFactory),
        }
    }
    pub fn with_factory(mut self, factory: Arc<dyn ProjectSerpSourceFactory>) -> Self {
        self.factory = factory;
        self
    }
    fn cipher(&self) -> Result<&SecretEnvelope, AppError> {
        self.cipher
            .as_deref()
            .ok_or_else(|| AppError::capability_missing("search credential encryption unavailable"))
    }
    async fn rows(&self, scope: &TenantScope) -> Result<Vec<ProjectSerpSettingsRecord>, AppError> {
        let mut rows = Vec::new();
        let mut after = None;
        loop {
            let page = self.repository.list(scope, after, 100).await?;
            let done = page.len() < 100;
            after = page.last().map(|row| row.source_key.clone());
            rows.extend(page);
            if done {
                break;
            }
        }
        Ok(rows)
    }
    pub async fn list(&self, scope: &TenantScope) -> Result<ProjectSerpSettingsPage, AppError> {
        Ok(ProjectSerpSettingsPage {
            items: self.rows(scope).await?.into_iter().map(view).collect(),
            encryption_available: self.cipher.is_some(),
        })
    }
    pub async fn save(
        &self,
        scope: &TenantScope,
        key: &str,
        input: UpdateProjectSerpSettings,
    ) -> Result<ProjectSerpSettingsView, AppError> {
        if input.expected_revision < 0
            || input.expected_revision == i64::MAX
            || !input.protocol_defaults.query.is_empty()
        {
            return Err(AppError::invalid_request(
                "invalid search settings revision or defaults",
            ));
        }
        // Validate the supported adapter's exact non-secret protocol even when
        // credentials are retained. No transport request occurs here.
        let protocol = &input.protocol_defaults;
        let config = source_config(protocol)?;
        let expected = DataForSeoSerpSource::protocol_for(&config, "");
        if expected != *protocol {
            return Err(AppError::invalid_request(
                "unsupported search source protocol",
            ));
        }
        let mut check = protocol.clone();
        check.query = "settings validation".into();
        check.validate()?;
        let login = input.login.filter(|value| !value.is_empty());
        let password = input.password.filter(|value| !value.is_empty());
        let encrypted_credentials = match (login, password) {
            (None, None) => None,
            (Some(login), Some(password)) => {
                let credentials = ProjectSerpCredentials { login, password };
                provider_client(&credentials)?;
                let bytes = serde_json::to_vec(&credentials)
                    .map_err(|_| AppError::invalid_request("invalid search credentials"))?;
                Some(
                    self.cipher()?
                        .seal(
                            &project_serp_credential_aad(scope, key, input.expected_revision + 1)?,
                            &bytes,
                        )
                        .map_err(|_| {
                            AppError::capability_missing("search credential encryption unavailable")
                        })?,
                )
            }
            _ => {
                return Err(AppError::invalid_request(
                    "search login and password must be provided together",
                ));
            }
        };
        let saved = self
            .repository
            .save(
                scope,
                input.expected_revision,
                ProjectSerpSettingsWrite {
                    source_key: key.into(),
                    provider: ProjectSerpProvider::Dataforseo,
                    enabled: input.enabled,
                    protocol_defaults: input.protocol_defaults,
                    encrypted_credentials,
                },
            )
            .await?;
        Ok(view(saved))
    }
    async fn credentials(
        &self,
        scope: &TenantScope,
        key: &str,
        revision: i64,
    ) -> Result<ProjectSerpCredentials, AppError> {
        let record = self
            .repository
            .get_credential(scope, key, revision)
            .await?
            .ok_or_else(|| AppError::not_ready("bound search credentials unavailable"))?;
        let bytes = self
            .cipher()?
            .open(
                &project_serp_credential_aad(scope, key, revision)?,
                &record.encrypted_credentials,
            )
            .map_err(|_| AppError::not_ready("bound search credentials unavailable"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| AppError::not_ready("bound search credentials unavailable"))
    }
    pub async fn test(
        &self,
        scope: &TenantScope,
        key: &str,
        expected_revision: i64,
    ) -> Result<ProjectSerpTestResult, AppError> {
        let row = self
            .repository
            .get(scope, key)
            .await?
            .ok_or_else(|| AppError::not_found("search source not configured"))?;
        if row.revision != expected_revision {
            return Err(AppError::conflict("search settings revision changed"));
        }
        let revision = row
            .active_credential_revision
            .ok_or_else(|| AppError::not_ready("search credentials unavailable"))?;
        let credentials = self.credentials(scope, key, revision).await?;
        let status = self.factory.test(&credentials).await;
        // Do not label a rotated configuration with an older diagnostic.
        if self
            .repository
            .get(scope, key)
            .await?
            .is_none_or(|current| current.revision != expected_revision)
        {
            return Err(AppError::conflict("search settings revision changed"));
        }
        Ok(ProjectSerpTestResult {
            source_key: key.into(),
            revision: expected_revision,
            status,
            checked_at: Utc::now(),
        })
    }
}

fn view(settings: ProjectSerpSettingsRecord) -> ProjectSerpSettingsView {
    ProjectSerpSettingsView {
        credentials_present: settings.active_credential_revision.is_some(),
        settings,
    }
}

#[async_trait]
impl SerpSourceResolver for ProjectSerpSettingsService {
    async fn capabilities(&self, scope: &TenantScope) -> Result<Vec<SerpCapability>, AppError> {
        if self.cipher.is_none() {
            return Ok(Vec::new());
        }
        Ok(self
            .rows(scope)
            .await?
            .into_iter()
            .filter(|row| row.enabled && row.active_credential_revision.is_some())
            .map(|row| SerpCapability {
                source_key: row.source_key,
                protocol_defaults: row.protocol_defaults,
            })
            .collect())
    }
    async fn current(
        &self,
        scope: &TenantScope,
        key: &str,
        protocol: Option<&SerpProtocol>,
    ) -> Result<ResolvedSerpSource, AppError> {
        let row = self
            .repository
            .get(scope, key)
            .await?
            .filter(|row| row.enabled)
            .ok_or_else(|| AppError::capability_missing("search source unavailable"))?;
        let revision = row
            .active_credential_revision
            .ok_or_else(|| AppError::not_ready("search credentials unavailable"))?;
        let credentials = self.credentials(scope, key, revision).await?;
        Ok(ResolvedSerpSource {
            source: self
                .factory
                .source(&credentials, protocol.unwrap_or(&row.protocol_defaults))?,
            credential_revision: Some(revision),
        })
    }
    async fn bound(
        &self,
        scope: &TenantScope,
        key: &str,
        protocol: &SerpProtocol,
        credential_revision: i64,
    ) -> Result<Arc<dyn SerpSource>, AppError> {
        let credentials = self.credentials(scope, key, credential_revision).await?;
        self.factory.source(&credentials, protocol)
    }
    async fn dispatch_sources(
        &self,
        after: Option<ProjectSerpSettingsCursor>,
        limit: usize,
    ) -> Result<Vec<ProjectSerpDispatchSource>, AppError> {
        self.repository.list_dispatch_sources(after, limit).await
    }
}

async fn scope(
    state: &AppState,
    auth: &AuthContext,
    id: ProjectId,
    admin: bool,
) -> Result<TenantScope, AppError> {
    if admin
        && !matches!(
            crate::selected_membership(auth).map(|membership| membership.role),
            Some(Role::CustomerAdmin)
        )
    {
        return Err(AppError::forbidden("customer administrator required"));
    }
    state
        .project_repository()
        .get(&auth.scope, id)
        .await?
        .ok_or_else(|| AppError::not_found("project not found"))?;
    Ok(TenantScope::new(
        auth.scope.operator_id,
        auth.scope.tenant_id,
        Some(id),
    ))
}
fn service(state: &AppState) -> Result<&ProjectSerpSettingsService, AppError> {
    state
        .project_serp_settings()
        .ok_or_else(|| AppError::capability_missing("search settings unavailable"))
}
async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path(id): Path<ProjectId>,
) -> Result<Json<ProjectSerpSettingsPage>, ApiError> {
    let map = |error| api_error(error, ctx.request_id);
    let scope = scope(&state, &auth, id, false).await.map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .list(&scope)
            .await
            .map_err(map)?,
    ))
}
async fn save(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((id, key)): Path<(ProjectId, String)>,
    Json(input): Json<UpdateProjectSerpSettings>,
) -> Result<Json<ProjectSerpSettingsView>, ApiError> {
    let map = |error| api_error(error, ctx.request_id);
    let scope = scope(&state, &auth, id, true).await.map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .save(&scope, &key, input)
            .await
            .map_err(map)?,
    ))
}
async fn test(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((id, key)): Path<(ProjectId, String)>,
    Json(input): Json<TestProjectSerpSettings>,
) -> Result<Json<ProjectSerpTestResult>, ApiError> {
    let map = |error| api_error(error, ctx.request_id);
    let scope = scope(&state, &auth, id, true).await.map_err(map)?;
    Ok(Json(
        service(&state)
            .map_err(map)?
            .test(&scope, &key, input.expected_revision)
            .await
            .map_err(map)?,
    ))
}
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{project_id}/serp-settings", get(list))
        .route(
            "/projects/{project_id}/serp-settings/{source_key}",
            axum::routing::put(save),
        )
        .route(
            "/projects/{project_id}/serp-settings/{source_key}/test",
            post(test),
        )
        .layer(middleware::from_fn(crate::csrf_origin_from_request))
        .layer(middleware::from_fn(crate::auth_scope_from_request))
        .layer(middleware::from_fn(crate::no_store_middleware))
}
