mod config;

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
    // The JavaScript runtime is assembled here, once, for the whole process.
    //
    // A configured runtime is `EmbeddedAgentRuntime::configured(capabilities)`
    // over an approved bundle — the API process holds the seam and each run
    // builds its own isolate on its own thread.  No such bundle or provider
    // bridge exists yet, so the process assembles the explicit absence of a
    // runtime instead: every accepted run then fails durably with
    // `capability_missing`.  There is deliberately no environment switch that
    // could turn on a partially configured runtime, because a runtime whose
    // capabilities are not all reachable must not accept the run.
    let runtime = Arc::new(EmbeddedAgentRuntime::unconfigured());
    let (state, durable_storage) = if AppConfig::database_url_configured() {
        // A configured database is authoritative.  Connection or migration
        // failures terminate startup; the process never falls back to memory
        // authentication or idempotency state.
        let database = Database::connect_and_migrate_from_env().await?;
        let state = AppState::from_database(&database)
            .with_allowed_origins(config.allowed_origins.clone())
            .with_agent_runtime(runtime);
        state.set_ready(true);
        (state, true)
    } else {
        let password = config.validate_for_memory_mode()?;
        let state = AppState::development_with_password(password)
            .with_allowed_origins(config.allowed_origins.clone())
            .with_agent_runtime(runtime);
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
