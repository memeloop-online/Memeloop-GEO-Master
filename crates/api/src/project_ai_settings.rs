//! Redacted project settings and bounded, saved-configuration diagnostics.
use crate::{ApiError, AppState, AuthContext, RequestContext, SharedModelProvider, api_error};
use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    middleware,
    routing::{get, post},
};
use geo_domain::{
    AppError, ProjectAiMode, ProjectAiSettingsRecord, ProjectAiSettingsRepository, ProjectAiUsage,
    ProjectId, Role, TenantScope, project_ai_scope_key,
};
use geo_provider::SecretEnvelope;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc, time::Duration};

type InheritedProviders = HashMap<ProjectAiUsage, (SharedModelProvider, Option<String>)>;
type InheritedMetadata = HashMap<ProjectAiUsage, Arc<dyn InheritedModelMetadata>>;

#[async_trait::async_trait]
pub trait InheritedModelMetadata: Send + Sync {
    async fn default_model(&self, scope: &TenantScope) -> Result<Option<String>, AppError>;
}

#[derive(Clone)]
pub struct ProjectAiSettingsService {
    repository: Arc<dyn ProjectAiSettingsRepository>,
    cipher: Option<Arc<SecretEnvelope>>,
    inherited: Arc<std::sync::RwLock<InheritedProviders>>,
    inherited_scopes: Arc<std::sync::RwLock<HashMap<ProjectAiUsage, TenantScope>>>,
    inherited_metadata: Arc<std::sync::RwLock<InheritedMetadata>>,
}
pub struct ResolvedProjectAiConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
}
impl std::fmt::Debug for ResolvedProjectAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolvedProjectAiConfig([redacted])")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectAiSettings {
    pub expected_revision: i64,
    pub mode: ProjectAiMode,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    pub prefer_connected_account: Option<bool>,
}
impl std::fmt::Debug for UpdateProjectAiSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UpdateProjectAiSettings([redacted])")
    }
}
#[derive(Serialize)]
pub struct ProjectAiSettingsView {
    pub usage: ProjectAiUsage,
    pub revision: i64,
    pub mode: ProjectAiMode,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub key_present: bool,
    pub prefer_connected_account: bool,
    pub effective: EffectiveAiSettings,
}
#[derive(Serialize)]
pub struct EffectiveAiSettings {
    pub source: &'static str,
    pub configured: bool,
    pub model: Option<String>,
}
impl ProjectAiSettingsService {
    pub fn development() -> Self {
        Self {
            repository: Arc::new(geo_domain::MemoryProjectAiSettingsRepository::default()),
            cipher: Some(Arc::new(SecretEnvelope::ephemeral())),
            inherited: Default::default(),
            inherited_scopes: Default::default(),
            inherited_metadata: Default::default(),
        }
    }
    pub fn unconfigured(repository: Arc<dyn ProjectAiSettingsRepository>) -> Self {
        Self {
            repository,
            cipher: None,
            inherited: Default::default(),
            inherited_scopes: Default::default(),
            inherited_metadata: Default::default(),
        }
    }
    pub fn persistent(
        repository: Arc<dyn ProjectAiSettingsRepository>,
        key_hex: &str,
    ) -> Result<Self, AppError> {
        Ok(Self {
            repository,
            cipher: Some(Arc::new(SecretEnvelope::from_hex_key(key_hex).map_err(
                |_| AppError::invalid_request("AI settings encryption key invalid"),
            )?)),
            inherited: Default::default(),
            inherited_scopes: Default::default(),
            inherited_metadata: Default::default(),
        })
    }
    pub fn with_inherited_provider(
        &self,
        usage: ProjectAiUsage,
        provider: Option<SharedModelProvider>,
        model: Option<String>,
    ) -> Self {
        let mut inherited = self.inherited.write().expect("inherited settings lock");
        if let Some(provider) = provider {
            inherited.insert(usage, (provider, model));
        } else {
            inherited.remove(&usage);
        }
        self.clone()
    }
    pub fn inherited_provider(&self, usage: ProjectAiUsage) -> Option<SharedModelProvider> {
        self.inherited
            .read()
            .expect("inherited settings lock")
            .get(&usage)
            .map(|(provider, _)| provider.clone())
    }
    pub fn with_inherited_metadata(
        &self,
        usage: ProjectAiUsage,
        metadata: Arc<dyn InheritedModelMetadata>,
    ) -> Self {
        self.inherited_metadata
            .write()
            .expect("inherited metadata lock")
            .insert(usage, metadata);
        self.clone()
    }
    pub fn with_inherited_model(&self, usage: ProjectAiUsage, model: String) -> Self {
        if let Some((_, current)) = self
            .inherited
            .write()
            .expect("inherited settings lock")
            .get_mut(&usage)
        {
            *current = Some(model);
        }
        self.clone()
    }
    pub fn with_inherited_scope(&self, usage: ProjectAiUsage, scope: TenantScope) -> Self {
        self.inherited_scopes
            .write()
            .expect("inherited scopes lock")
            .insert(usage, scope);
        self.clone()
    }
    pub async fn preference(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
    ) -> Result<bool, AppError> {
        Ok(self
            .repository
            .get(scope, usage)
            .await?
            .prefer_connected_account)
    }
    fn cipher(&self) -> Result<&SecretEnvelope, AppError> {
        self.cipher
            .as_deref()
            .ok_or_else(|| AppError::capability_missing("AI settings encryption is unavailable"))
    }
    pub async fn resolve(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
    ) -> Result<Option<ResolvedProjectAiConfig>, AppError> {
        Ok(self.resolve_with_revision(scope, usage).await?.0)
    }
    pub async fn resolve_with_revision(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
    ) -> Result<(Option<ResolvedProjectAiConfig>, i64), AppError> {
        let row = self.repository.get(scope, usage).await?;
        if row.mode == ProjectAiMode::Inherit {
            return Ok((None, row.revision));
        }
        let encrypted = row
            .encrypted_api_key
            .ok_or_else(|| AppError::not_ready("AI API key is not configured"))?;
        let secret = self
            .cipher()?
            .open(project_ai_scope_key(scope, usage)?.as_bytes(), &encrypted)
            .map_err(|_| AppError::not_ready("AI credential unavailable"))?;
        Ok((
            Some(ResolvedProjectAiConfig {
                model: row
                    .model
                    .ok_or_else(|| AppError::not_ready("AI model is not configured"))?,
                base_url: row
                    .base_url
                    .ok_or_else(|| AppError::not_ready("AI endpoint is not configured"))?,
                api_key: String::from_utf8(secret)
                    .map_err(|_| AppError::not_ready("AI credential unavailable"))?,
            }),
            row.revision,
        ))
    }
    fn view(&self, scope: &TenantScope, r: ProjectAiSettingsRecord) -> ProjectAiSettingsView {
        let custom = r.mode == ProjectAiMode::Custom;
        let inherited_guard = self.inherited.read().expect("inherited settings lock");
        let scopes = self.inherited_scopes.read().expect("inherited scopes lock");
        let permitted = scopes
            .get(&r.usage)
            .is_none_or(|bound| bound.contains(scope));
        let inherited = inherited_guard.get(&r.usage).filter(|_| permitted);
        let configured = if custom {
            self.cipher.is_some()
                && r.model.is_some()
                && r.base_url.is_some()
                && r.encrypted_api_key.is_some()
        } else {
            inherited.is_some()
        };
        let effective = EffectiveAiSettings {
            source: if custom {
                "custom"
            } else if configured {
                "inherit"
            } else {
                "unconfigured"
            },
            configured,
            model: if custom {
                r.model.clone()
            } else {
                inherited.and_then(|(_, m)| m.clone())
            },
        };
        ProjectAiSettingsView {
            usage: r.usage,
            revision: r.revision,
            mode: r.mode,
            model: r.model,
            base_url: r.base_url,
            key_present: r.encrypted_api_key.is_some(),
            prefer_connected_account: r.prefer_connected_account,
            effective,
        }
    }
    pub async fn get(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
    ) -> Result<ProjectAiSettingsView, AppError> {
        self.effective_view(scope, self.repository.get(scope, usage).await?)
            .await
    }
    async fn effective_view(
        &self,
        scope: &TenantScope,
        record: ProjectAiSettingsRecord,
    ) -> Result<ProjectAiSettingsView, AppError> {
        let mut view = self.view(scope, record);
        if view.mode == ProjectAiMode::Inherit && view.effective.configured {
            let metadata = self
                .inherited_metadata
                .read()
                .expect("inherited metadata lock")
                .get(&view.usage)
                .cloned();
            if let Some(metadata) = metadata {
                let model = metadata.default_model(scope).await?;
                view.effective = EffectiveAiSettings {
                    source: if model.is_some() {
                        "inherit"
                    } else {
                        "unconfigured"
                    },
                    configured: model.is_some(),
                    model,
                };
            }
        }
        Ok(view)
    }
    pub async fn save(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
        input: UpdateProjectAiSettings,
    ) -> Result<ProjectAiSettingsView, AppError> {
        if input.api_key.is_some() && input.clear_api_key {
            return Err(AppError::invalid_request(
                "replace and clear API key are mutually exclusive",
            ));
        }
        let mut row = self.repository.get(scope, usage).await?;
        if row.revision != input.expected_revision {
            return Err(AppError::conflict("AI settings revision changed"));
        }
        row.mode = input.mode;
        row.prefer_connected_account = input
            .prefer_connected_account
            .unwrap_or(row.prefer_connected_account);
        if input.mode == ProjectAiMode::Inherit {
            row.model = None;
            row.base_url = None;
            row.encrypted_api_key = None;
        } else {
            let model = input
                .model
                .filter(|v| {
                    !v.trim().is_empty()
                        && v.len() <= 256
                        && !v.contains("://")
                        && !v.chars().any(char::is_control)
                })
                .ok_or_else(|| AppError::invalid_request("a valid model is required"))?;
            let base_url = input
                .base_url
                .ok_or_else(|| AppError::invalid_request("an API base URL is required"))?;
            validate_base_url(&base_url)?;
            row.model = Some(model.trim().into());
            row.base_url = Some(base_url.trim_end_matches('/').into());
            if input.clear_api_key {
                row.encrypted_api_key = None;
            }
            if let Some(key) = input.api_key {
                if key.trim().is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
                    return Err(AppError::invalid_request("API key is invalid"));
                }
                row.encrypted_api_key = Some(
                    self.cipher()?
                        .seal(
                            project_ai_scope_key(scope, usage)?.as_bytes(),
                            key.as_bytes(),
                        )
                        .map_err(|_| AppError::not_ready("AI credential encryption unavailable"))?,
                );
            }
        }
        self.effective_view(
            scope,
            self.repository
                .save(scope, input.expected_revision, row)
                .await?,
        )
        .await
    }
    async fn check_revision(
        &self,
        scope: &TenantScope,
        usage: ProjectAiUsage,
        expected: i64,
    ) -> Result<(), AppError> {
        if self.repository.get(scope, usage).await?.revision != expected {
            return Err(AppError::conflict("AI settings revision changed"));
        }
        Ok(())
    }
}
fn validate_base_url(value: &str) -> Result<(), AppError> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| AppError::invalid_request("API base URL is invalid"))?;
    if value.len() > 2048
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::invalid_request(
            "API base URL must be HTTP(S) without credentials, query or fragment",
        ));
    }
    Ok(())
}
async fn scope(
    state: &AppState,
    auth: &AuthContext,
    id: ProjectId,
    admin: bool,
) -> Result<TenantScope, AppError> {
    if admin
        && !matches!(
            crate::selected_membership(auth).map(|m| m.role),
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
async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path(id): Path<ProjectId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let scope = scope(&state, &auth, id, false)
        .await
        .map_err(|e| api_error(e, ctx.request_id))?;
    let mut items = Vec::new();
    for usage in ProjectAiUsage::ALL {
        items.push(
            state
                .project_ai_settings()
                .get(&scope, usage)
                .await
                .map_err(|e| api_error(e, ctx.request_id))?,
        );
    }
    Ok(Json(serde_json::json!({"items":items})))
}
async fn update(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((id, usage)): Path<(ProjectId, ProjectAiUsage)>,
    Json(input): Json<UpdateProjectAiSettings>,
) -> Result<Json<ProjectAiSettingsView>, ApiError> {
    let scope = scope(&state, &auth, id, true)
        .await
        .map_err(|e| api_error(e, ctx.request_id))?;
    Ok(Json(
        state
            .project_ai_settings()
            .save(&scope, usage, input)
            .await
            .map_err(|e| api_error(e, ctx.request_id))?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRequest {
    expected_revision: i64,
}
async fn test(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((id, usage)): Path<(ProjectId, ProjectAiUsage)>,
    Json(input): Json<StoredRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let scope = scope(&state, &auth, id, true)
        .await
        .map_err(|e| api_error(e, ctx.request_id))?;
    let service = state.project_ai_settings();
    service
        .check_revision(&scope, usage, input.expected_revision)
        .await
        .map_err(|e| api_error(e, ctx.request_id))?;
    let inherited = service
        .inherited
        .read()
        .expect("inherited settings lock")
        .get(&usage)
        .map(|(p, _)| p.clone());
    let bridge = crate::ProjectConfiguredModelBridge::new(service.clone(), inherited)
        .map_err(|_| api_error(diagnostic_error(), ctx.request_id))?;
    let request = geo_worker::ModelCompletionRequest {
        prompt: "Reply with OK.".into(),
        system: None,
        model: None,
        max_output_tokens: Some(8),
        messages: Vec::new(),
        tools: Vec::new(),
    };
    tokio::time::timeout(
        Duration::from_secs(20),
        bridge.complete_for_usage_at_revision(&scope, usage, input.expected_revision, &request),
    )
    .await
    .map_err(|_| api_error(diagnostic_error(), ctx.request_id))?
    .map_err(|_| api_error(diagnostic_error(), ctx.request_id))?;
    service
        .check_revision(&scope, usage, input.expected_revision)
        .await
        .map_err(|e| api_error(e, ctx.request_id))?;
    Ok(Json(serde_json::json!({"success":true})))
}
fn diagnostic_error() -> AppError {
    AppError::not_ready(
        "AI provider request failed; check the saved endpoint, model and credential",
    )
}
async fn models(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ctx): Extension<RequestContext>,
    Path((id, usage)): Path<(ProjectId, ProjectAiUsage)>,
    Json(input): Json<StoredRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = async {
        let scope = scope(&state, &auth, id, true).await?;
        let service = state.project_ai_settings();
        service
            .check_revision(&scope, usage, input.expected_revision)
            .await?;
        let (config, revision) = service.resolve_with_revision(&scope, usage).await?;
        if revision != input.expected_revision {
            return Err(AppError::conflict("AI settings revision changed"));
        }
        let config = config.ok_or_else(|| {
            AppError::capability_missing("model discovery requires custom saved configuration")
        })?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| diagnostic_error())?;
        let mut response = client
            .get(format!("{}/models", config.base_url.trim_end_matches('/')))
            .bearer_auth(&config.api_key)
            .send()
            .await
            .map_err(|_| diagnostic_error())?;
        if !response.status().is_success() {
            return Err(AppError::capability_missing(
                "model discovery unavailable; enter a model manually",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| diagnostic_error())? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(diagnostic_error());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| diagnostic_error())?;
        let entries = body.get("data").and_then(|v| v.as_array()).ok_or_else(|| {
            AppError::capability_missing("model discovery unavailable; enter a model manually")
        })?;
        let items: Vec<_> = entries
            .iter()
            .take(500)
            .filter_map(|entry| entry.get("id").and_then(|v| v.as_str()))
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 256
                    && !s.chars().any(char::is_control)
                    && !s.contains(&config.api_key)
            })
            .map(|id| serde_json::json!({"id":id}))
            .collect();
        service
            .check_revision(&scope, usage, input.expected_revision)
            .await?;
        Ok(Json(serde_json::json!({"items":items})))
    }
    .await;
    result.map_err(|e| api_error(e, ctx.request_id))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/ai-settings", get(list))
        .route(
            "/projects/{id}/ai-settings/{usage}",
            axum::routing::put(update),
        )
        .route("/projects/{id}/ai-settings/{usage}/test", post(test))
        .route("/projects/{id}/ai-settings/{usage}/models", post(models))
        .layer(middleware::from_fn(crate::csrf_origin_from_request))
        .layer(middleware::from_fn(crate::auth_scope_from_request))
        .layer(middleware::from_fn(crate::no_store_middleware))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct UnusedProvider;
    #[async_trait::async_trait]
    impl crate::ModelProviderBridge for UnusedProvider {
        async fn complete(
            &self,
            _: &TenantScope,
            _: &geo_worker::ModelCompletionRequest,
        ) -> Result<geo_worker::ModelCompletion, geo_worker::HostOpError> {
            panic!("settings projection must not invoke inference")
        }
    }
    struct MutableMetadata(std::sync::Mutex<Option<String>>);
    #[async_trait::async_trait]
    impl InheritedModelMetadata for MutableMetadata {
        async fn default_model(&self, _: &TenantScope) -> Result<Option<String>, AppError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }
    fn scope() -> TenantScope {
        TenantScope::new(
            uuid::Uuid::new_v4().into(),
            uuid::Uuid::new_v4().into(),
            Some(uuid::Uuid::new_v4().into()),
        )
    }
    fn custom(revision: i64, key: Option<&str>) -> UpdateProjectAiSettings {
        UpdateProjectAiSettings {
            expected_revision: revision,
            mode: ProjectAiMode::Custom,
            model: Some("synthetic-model".into()),
            base_url: Some("http://127.0.0.1:8123/v1".into()),
            api_key: key.map(str::to_owned),
            clear_api_key: false,
            prefer_connected_account: None,
        }
    }
    #[tokio::test]
    async fn inherited_metadata_is_live_scoped_and_never_fabricates_configuration() {
        let service = ProjectAiSettingsService::development();
        let usage = ProjectAiUsage::WorkbenchContent;
        let scope = scope();
        let metadata = Arc::new(MutableMetadata(std::sync::Mutex::new(None)));
        service.with_inherited_provider(usage, Some(Arc::new(UnusedProvider)), None);
        service.with_inherited_scope(usage, scope.clone());
        service.with_inherited_metadata(usage, metadata.clone());
        assert!(
            !service
                .get(&scope, usage)
                .await
                .unwrap()
                .effective
                .configured
        );
        *metadata.0.lock().unwrap() = Some("synthetic-inherited-model".into());
        let view = service.get(&scope, usage).await.unwrap();
        assert!(view.effective.configured);
        assert_eq!(
            view.effective.model.as_deref(),
            Some("synthetic-inherited-model")
        );
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(uuid::Uuid::new_v4().into()),
        );
        assert!(
            !service
                .get(&other, usage)
                .await
                .unwrap()
                .effective
                .configured
        );
        *metadata.0.lock().unwrap() = None;
        assert!(
            !service
                .get(&scope, usage)
                .await
                .unwrap()
                .effective
                .configured
        );
        let view = service
            .save(&scope, usage, custom(0, Some("synthetic-key")))
            .await
            .unwrap();
        assert_eq!(view.effective.model.as_deref(), Some("synthetic-model"));
        assert!(view.effective.configured);
    }
    #[tokio::test]
    async fn encryption_rotation_clear_inherit_and_scope_isolation() {
        let repo = Arc::new(geo_domain::MemoryProjectAiSettingsRepository::default());
        let service = ProjectAiSettingsService::persistent(repo.clone(), &"12".repeat(32)).unwrap();
        let scope = scope();
        let usage = ProjectAiUsage::WorkbenchContent;
        let initial = service.get(&scope, usage).await.unwrap();
        assert_eq!(initial.revision, 0);
        assert!(!initial.effective.configured);
        let saved = service
            .save(&scope, usage, custom(0, Some("synthetic-secret-one")))
            .await
            .unwrap();
        assert!(saved.key_present);
        let json = serde_json::to_string(&saved).unwrap();
        assert!(!json.contains("synthetic-secret"));
        let row = repo.get(&scope, usage).await.unwrap();
        assert!(
            !String::from_utf8_lossy(row.encrypted_api_key.as_ref().unwrap())
                .contains("synthetic-secret")
        );
        assert!(!format!("{:?}", row).contains("synthetic"));
        let reboot = ProjectAiSettingsService::persistent(repo.clone(), &"12".repeat(32)).unwrap();
        assert_eq!(
            reboot
                .resolve(&scope, usage)
                .await
                .unwrap()
                .unwrap()
                .api_key,
            "synthetic-secret-one"
        );
        assert!(
            ProjectAiSettingsService::persistent(repo.clone(), &"34".repeat(32))
                .unwrap()
                .resolve(&scope, usage)
                .await
                .is_err()
        );
        service.save(&scope, usage, custom(1, None)).await.unwrap();
        assert_eq!(
            service
                .resolve(&scope, usage)
                .await
                .unwrap()
                .unwrap()
                .api_key,
            "synthetic-secret-one"
        );
        service
            .save(&scope, usage, custom(2, Some("synthetic-secret-two")))
            .await
            .unwrap();
        assert_eq!(
            service
                .resolve(&scope, usage)
                .await
                .unwrap()
                .unwrap()
                .api_key,
            "synthetic-secret-two"
        );
        assert!(service.save(&scope, usage, custom(2, None)).await.is_err());
        let other = TenantScope::new(
            scope.operator_id,
            scope.tenant_id,
            Some(uuid::Uuid::new_v4().into()),
        );
        assert!(service.resolve(&other, usage).await.unwrap().is_none());
        // Even transplanted ciphertext cannot cross usage or project boundaries.
        let mut transplanted = repo.get(&scope, usage).await.unwrap();
        transplanted.usage = ProjectAiUsage::ObservationAnalysis;
        repo.save(&scope, 0, transplanted).await.unwrap();
        assert!(
            service
                .resolve(&scope, ProjectAiUsage::ObservationAnalysis)
                .await
                .is_err()
        );
        let mut cleared = custom(3, None);
        cleared.clear_api_key = true;
        assert!(
            !service
                .save(&scope, usage, cleared)
                .await
                .unwrap()
                .key_present
        );
        assert!(service.resolve(&scope, usage).await.is_err());
        let mut inherit = custom(4, None);
        inherit.mode = ProjectAiMode::Inherit;
        let saved = service.save(&scope, usage, inherit).await.unwrap();
        assert!(!saved.key_present);
        assert!(saved.model.is_none());
        assert!(saved.base_url.is_none());
    }
    #[tokio::test]
    async fn validation_and_persistent_missing_key_fail_closed() {
        let service = ProjectAiSettingsService::unconfigured(Arc::new(
            geo_domain::MemoryProjectAiSettingsRepository::default(),
        ));
        let scope = scope();
        let usage = ProjectAiUsage::WorkbenchContent;
        assert!(
            service
                .save(&scope, usage, custom(0, Some("synthetic-secret")))
                .await
                .is_err()
        );
        let service = ProjectAiSettingsService::development();
        let mut input = custom(0, Some("synthetic-secret"));
        input.clear_api_key = true;
        assert!(service.save(&scope, usage, input).await.is_err());
        for url in [
            "ftp://localhost/v1",
            "https://user:pass@example.invalid/v1",
            "https://example.invalid/v1?key=x",
            "https://example.invalid/v1#fragment",
        ] {
            let mut input = custom(0, Some("synthetic-secret"));
            input.base_url = Some(url.into());
            assert!(service.save(&scope, usage, input).await.is_err());
        }
        assert!(validate_base_url("http://127.0.0.1:8000/v1").is_ok());
    }
}
