//! Deployment-only account execution settings. No secrets are sent to clients.

use std::{env, error::Error};

use geo_api::{AppState, BrowserBridge, ChannelService};

/// Only durable deployments expose evidence checkpoints and publication
/// callbacks. The independently configured service credential stays server-side.
pub fn configure_callbacks(
    state: AppState,
    database: &geo_persistence::Database,
) -> Result<AppState, Box<dyn Error>> {
    // Sources are discovered from project-owned persistent settings. Old task
    // reads retain their exact encrypted credential revision after rotation.
    let serp_settings_repository = std::sync::Arc::new(
        geo_persistence::PgProjectSerpSettingsRepository::from_database(database),
    );
    let serp_settings = match env::var("GEO_CHANNEL_SECRET_KEY") {
        Ok(key) => geo_api::ProjectSerpSettingsService::persistent(serp_settings_repository, &key)?,
        Err(_) => geo_api::ProjectSerpSettingsService::unconfigured(serp_settings_repository),
    };
    let serp = geo_api::SerpService::new(
        std::sync::Arc::new(geo_persistence::PgSerpRepository::from_database(database)),
        state.project_repository(),
        state.question_repository(),
    )
    .with_source_resolver(std::sync::Arc::new(serp_settings.clone()));
    let _serp_dispatcher = geo_api::spawn_serp_dispatcher(serp.clone());
    let state = state
        .with_project_serp_settings(serp_settings)
        .with_serp_service(serp);
    let state =
        state.with_observation_evidence_resolver(geo_api::ObservationEvidenceResolver::new(
            std::sync::Arc::new(
                geo_persistence::PgObservationAnalysisRepository::from_database(database),
            ),
            std::sync::Arc::new(
                geo_persistence::PgObservationCaptureRepository::from_database(database),
            ),
        ));
    let state = match (
        env::var("GEO_BROWSER_RUNNER_URL").ok(),
        env::var("GEO_BROWSER_RUNNER_TOKEN").ok(),
    ) {
        (Some(url), Some(token)) => {
            let settings = state.project_ai_settings();
            let inherited =
                settings.inherited_provider(geo_domain::ProjectAiUsage::ObservationAnalysis);
            let analysis = geo_api::ObservationAnalysisService::new(
                std::sync::Arc::new(
                    geo_persistence::PgObservationAnalysisRepository::from_database(database),
                ),
                state.channel_job_repository(),
                std::sync::Arc::new(
                    geo_persistence::PgObservationCaptureRepository::from_database(database),
                ),
                geo_api::ProjectConfiguredModelBridge::new(settings, inherited)?,
                std::sync::Arc::new(geo_api::HttpSavedObservationGrounder::new(&url, &token)?),
            );
            state.with_observation_analysis(analysis)
        }
        (None, None) => state,
        _ => return Err("browser runner URL and token must be configured together".into()),
    };
    let token = env::var("GEO_BROWSER_RUNNER_CALLBACK_TOKEN").ok();
    let key = env::var("GEO_CHANNEL_SECRET_KEY").ok();
    callbacks_with_credentials(state, database, token.as_deref(), key.as_deref())
}

fn callbacks_with_credentials(
    state: AppState,
    database: &geo_persistence::Database,
    token: Option<&str>,
    key: Option<&str>,
) -> Result<AppState, Box<dyn Error>> {
    let Some(token) = token else {
        return Ok(state);
    };
    let key = key.ok_or("browser callback configuration requires a persistent channel key")?;
    let cipher = std::sync::Arc::new(geo_provider::SecretEnvelope::from_hex_key(key)?);
    let captures = geo_api::ObservationCaptureCallbackService::new(
        std::sync::Arc::new(
            geo_persistence::PgObservationCaptureRepository::from_database(database),
        ),
        cipher.clone(),
        token,
    )?;
    let settings = state.project_ai_settings();
    let inherited = settings.inherited_provider(geo_domain::ProjectAiUsage::ObservationAnalysis);
    let observation_ai = geo_api::ObservationAiCallbackService::new(
        captures.clone(),
        settings.clone(),
        geo_api::ProjectConfiguredModelBridge::new(settings, inherited)?,
    );
    let publication = geo_api::PublicationSendCallbackService::new(
        std::sync::Arc::new(
            geo_persistence::PgPublicationSendAuthorizationRepository::from_database(database),
        ),
        cipher.clone(),
        token,
    )?;
    let cleanup = geo_api::ProviderCleanupCallbackService::new(
        std::sync::Arc::new(
            geo_persistence::PgProviderConversationCleanupRepository::from_database(database),
        ),
        cipher,
        token,
    )?;
    Ok(state
        .with_observation_ai_callback(observation_ai)
        .with_observation_capture_callback(captures)
        .with_publication_send_callback(publication)
        .with_provider_cleanup_callback(cleanup))
}

pub fn configure(state: AppState) -> Result<AppState, Box<dyn Error>> {
    let runner_url = env::var("GEO_BROWSER_RUNNER_URL").ok();
    let runner_token = env::var("GEO_BROWSER_RUNNER_TOKEN").ok();
    let browser = match (runner_url, runner_token) {
        (None, None) => None,
        (Some(url), Some(token)) => Some(BrowserBridge::new(url, token)?),
        _ => return Err("browser runner URL and token must be configured together".into()),
    };
    let current = state.channel_service();
    let mut service = if let Ok(key) = env::var("GEO_CHANNEL_SECRET_KEY") {
        ChannelService::persistent(current.repository.clone(), &key, browser.clone())?
    } else {
        // An unconfigured persistent vault does not block unrelated app work.
        // Its credential operations remain unavailable, with no plaintext fallback.
        current.clone()
    };
    if let Some(browser) = browser {
        service = service.with_browser(browser);
    }
    if let Ok(tenant_id) = env::var("GEO_OPERATOR_POOL_TENANT_ID") {
        service = service.with_operator_pool_tenant_id(tenant_id.parse()?);
    }
    Ok(state.with_channel_service(service))
}
