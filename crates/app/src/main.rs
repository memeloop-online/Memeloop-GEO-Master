mod config;
mod runtime;

use axum::Router;
use config::AppConfig;
use geo_api::{AppState, EmbeddedAgentRuntime, router};
use geo_persistence::Database;
use std::error::Error;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let config = AppConfig::from_env()?;
    let durable_storage = AppConfig::database_url_configured();
    config.validate_ai_mode(durable_storage)?;
    let (state, durable_storage) = if durable_storage {
        // A configured database is authoritative.  Connection or migration
        // failures terminate startup; the process never falls back to memory
        // authentication or idempotency state.
        let database = Database::connect_and_migrate_from_env().await?;
        let state =
            AppState::from_database(&database).with_allowed_origins(config.allowed_origins.clone());
        let state = state.with_agent_runtime(Arc::new(EmbeddedAgentRuntime::unconfigured()));
        if config.single_process_executor {
            let reconciled = state.reconcile_running_runs().await?;
            info!(
                reconciled,
                "reconciled abandoned agent runs during single-process startup"
            );
        }
        state.set_ready(true);
        (state, true)
    } else {
        let password = config.validate_for_memory_mode()?;
        let state = AppState::development_with_password(password)
            .with_allowed_origins(config.allowed_origins.clone());
        let runtime = runtime::assemble(&state, config.development_ai.as_ref())?;
        let state = state.with_agent_runtime(runtime);
        if config.single_process_executor {
            let reconciled = state.reconcile_running_runs().await?;
            info!(
                reconciled,
                "reconciled abandoned agent runs during single-process startup"
            );
        }
        if config.ready_on_start {
            state.set_ready(true);
        }
        (state, false)
    };
    let app: Router = router(state);
    let listener = TcpListener::bind(config.bind_addr).await?;
    info!(
        address = %config.bind_addr,
        durable_storage,
        "starting GEO API"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
